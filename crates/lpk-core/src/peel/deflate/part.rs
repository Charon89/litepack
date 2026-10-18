//! The Deflate peel's part encoder (design `docs/notes/e2-10-11-design.md`, section 3.5): the
//! model step M that encodes one stream's inflated text, the first step of the stream's block
//! `[M, deflate-reconstruct {id}]`.
//!
//! - Balanced: its own LZMA encoder (the tier's dictionary cap, `dict_for(len)`, the end marker
//!   always written, as every raw LZMA1 stream of liblzma's encoder) and its own zstd core (the
//!   tier's level and window, no dictionary), running the tier's four-stripe trial. No class
//!   rule and no "stored, no gain" fallback: the peel's own gate decides.
//! - Fast (an option): zstd at the tier's level and window, no gate, no dictionary.
//!
//! Its blocks are peel work: they are counted in the Deflate summary, not in the tier
//! summaries. It runs on the feeding thread, one stream at a time; after each stream the zstd
//! context's tables are dropped (rebuilt empty with the same parameters, which gives the same
//! output), so the encoder holds no full-size tables between streams (E2-20).

use lpk_format::{Encoded, FormatError};

use crate::balanced::{trial, BalancedOptions, Candidate, LzmaEncoder};
use crate::error::CoreError;
use crate::fast::{FastOptions, ZstdCore};
use crate::memory::{lzma_out_reserve, window_for, zstd_bound, zstd_cctx_memory};

/// Which model a part block uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartModel {
    /// `lzma`.
    Lzma,
    /// `zstd`.
    Zstd,
}

/// One stream's model encoding: M's `Encoded` (one step), the model and the trial's sample.
#[derive(Debug)]
pub struct PartEncoded {
    /// M's encoding of the inflated text.
    pub encoded: Encoded,
    /// The model that won.
    pub model: PartModel,
    /// Plain bytes the trial fed to each candidate (0 for Fast).
    pub sample_bytes: u64,
}

/// The part encoder of a tier.
pub struct PartEncoder(Inner);

enum Inner {
    /// The Balanced tier's trial between its own LZMA encoder and zstd core.
    Balanced {
        /// The LZMA encoder (the tier's dictionary cap).
        lzma: LzmaEncoder,
        /// The zstd core (the tier's level and window).
        zstd: ZstdCore,
        /// The trial's sample length.
        sample_len: usize,
        /// The zstd candidate's level and window log (for the memory figure).
        zstd_level: i32,
        window_log: u32,
        /// The largest text it encodes.
        plain_limit: u64,
    },
    /// The Fast tier's zstd.
    Fast {
        /// The zstd core (the tier's level and window).
        zstd: ZstdCore,
        /// The level and window log (for the memory figure).
        level: i32,
        window_log: u32,
        /// The largest text it encodes.
        plain_limit: u64,
    },
}

impl std::fmt::Debug for PartEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Inner::Balanced {
                lzma, sample_len, ..
            } => f
                .debug_struct("Inner::Balanced")
                .field("lzma", lzma)
                .field("sample_len", sample_len)
                .finish_non_exhaustive(),
            Inner::Fast { .. } => f.debug_struct("Inner::Fast").finish_non_exhaustive(),
        }
    }
}

impl PartEncoder {
    /// The Balanced part encoder of `o`; `plain_limit` is the largest inflated text it encodes
    /// (for the memory figure).
    pub fn balanced(o: &BalancedOptions, plain_limit: u64) -> Result<Self, CoreError> {
        let lzma = LzmaEncoder::with_dict(o.dict_size)?;
        let window_log = o.zstd_window_log.unwrap_or(o.dict_size.ilog2());
        let zstd = ZstdCore::standalone(o.zstd_level, window_log)?;
        let sample_len = o.sample_len.max(4);
        Ok(PartEncoder(Inner::Balanced {
            lzma,
            zstd,
            sample_len,
            zstd_level: o.zstd_level,
            window_log,
            plain_limit,
        }))
    }

    /// The Fast part encoder of `o` (see [`PartEncoder::balanced`] for `plain_limit`).
    pub fn fast(o: &FastOptions, plain_limit: u64) -> Result<Self, CoreError> {
        let zstd = ZstdCore::standalone(o.level, o.window_log)?;
        Ok(PartEncoder(Inner::Fast {
            zstd,
            level: o.level,
            window_log: o.window_log,
            plain_limit,
        }))
    }

    /// A new encoder with this one's settings (a pool worker's own; or a replacement after an
    /// encode failed, so no state of a failed encode is carried into the next).
    pub fn fresh(&self) -> Result<Self, CoreError> {
        match &self.0 {
            Inner::Balanced {
                lzma,
                sample_len,
                zstd_level,
                window_log,
                plain_limit,
                ..
            } => Ok(PartEncoder(Inner::Balanced {
                lzma: LzmaEncoder::with_dict(lzma.dict_size())?,
                zstd: ZstdCore::standalone(*zstd_level, *window_log)?,
                sample_len: *sample_len,
                zstd_level: *zstd_level,
                window_log: *window_log,
                plain_limit: *plain_limit,
            })),
            Inner::Fast {
                level,
                window_log,
                plain_limit,
                ..
            } => Ok(PartEncoder(Inner::Fast {
                zstd: ZstdCore::standalone(*level, *window_log)?,
                level: *level,
                window_log: *window_log,
                plain_limit: *plain_limit,
            })),
        }
    }

    /// The encoder's working set while it encodes a text of the `plain_limit` given at
    /// construction: the encoders and the output buffer (the text itself is not counted).
    pub fn memory(&self) -> u64 {
        match &self.0 {
            Inner::Balanced { plain_limit, .. } | Inner::Fast { plain_limit, .. } => {
                self.memory_for(*plain_limit)
            }
        }
    }

    /// The working set for texts of at most `plain` bytes (at most the construction's limit):
    /// the LZMA dictionary and the zstd window follow the text (`dict_for(len)`, the declared
    /// window), so a run whose files can only hold small texts plans less.
    pub fn memory_for(&self, plain: u64) -> u64 {
        match &self.0 {
            Inner::Balanced {
                lzma,
                sample_len,
                zstd_level,
                window_log,
                plain_limit,
                ..
            } => {
                let plain = plain.min(*plain_limit);
                let fallback = |m: u64| {
                    if m == 0 {
                        u64::from(lzma.dict_size()).saturating_mul(12)
                    } else {
                        m
                    }
                };
                let stripe = (*sample_len / 4).max(1);
                let lzma_full = fallback(
                    lzma.memory_usage_for_len(usize::try_from(plain).unwrap_or(usize::MAX)),
                );
                let lzma_stripe = fallback(lzma.memory_usage_for_len(stripe));
                let z_full = zstd_cctx_memory(*zstd_level, window_for(*window_log, plain), true);
                let z_stripe =
                    zstd_cctx_memory(*zstd_level, window_for(*window_log, stripe as u64), true);
                // As a Balanced block: the larger of the two ways the trial ends, plus the output.
                let encoder = lzma_full
                    .saturating_add(z_stripe)
                    .max(z_full.saturating_add(lzma_stripe));
                encoder
                    .saturating_add(lzma_out_reserve(plain).max(zstd_bound(plain)))
                    .saturating_add(*sample_len as u64)
            }
            Inner::Fast {
                level,
                window_log,
                plain_limit,
                ..
            } => {
                let plain = plain.min(*plain_limit);
                zstd_cctx_memory(*level, window_for(*window_log, plain), true)
                    .saturating_add(zstd_bound(plain))
            }
        }
    }

    /// Encode one inflated text.
    pub fn encode(&mut self, plain: &[u8]) -> Result<PartEncoded, FormatError> {
        let io = |e: CoreError| FormatError::Io(std::io::Error::other(e.to_string()));
        let r = match &mut self.0 {
            Inner::Balanced {
                lzma,
                zstd,
                sample_len,
                ..
            } => {
                // No BWT candidate: the part encoder stays `[M]` in revision 1.3 (D-69 (6)).
                let (encoded, chosen, sample_bytes) =
                    trial(lzma, zstd, *sample_len, None, plain, |z, st| {
                        z.encode_raw(st)
                    })?;
                zstd.release().map_err(io)?;
                PartEncoded {
                    encoded,
                    model: match chosen {
                        Candidate::Zstd => PartModel::Zstd,
                        Candidate::Lzma | Candidate::Bwt => PartModel::Lzma,
                    },
                    sample_bytes,
                }
            }
            Inner::Fast { zstd, .. } => {
                let encoded = zstd.encode_raw(plain)?;
                zstd.release().map_err(io)?;
                PartEncoded {
                    encoded,
                    model: PartModel::Zstd,
                    sample_bytes: 0,
                }
            }
        };
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lpk_format::PrimitiveId;

    fn text(len: usize) -> Vec<u8> {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let words = ["alpha ", "beta ", "gamma ", "delta\n"];
        let mut out = Vec::new();
        while out.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(words[(x % 4) as usize].as_bytes());
        }
        out.truncate(len);
        out
    }

    #[test]
    fn balanced_runs_the_trial_without_a_store_and_fast_is_zstd() {
        let o = BalancedOptions {
            dict_size: 1 << 20,
            sample_len: 1 << 16,
            ..BalancedOptions::default()
        };
        let mut b = PartEncoder::balanced(&o, 64 << 20).unwrap();
        assert!(b.memory() > 64 << 20);
        assert_eq!(b.memory_for(u64::MAX), b.memory());
        assert!(b.memory_for(1 << 20) < b.memory() / 4);
        let t = text(300_000);
        let e = b.encode(&t).unwrap();
        assert!(e.sample_bytes > 0 && e.encoded.bytes.len() < t.len());
        let p = e.encoded.graph.steps[0].primitive;
        assert!(matches!(p, PrimitiveId::Lzma | PrimitiveId::Zstd));
        assert_eq!(p == PrimitiveId::Zstd, e.model == PartModel::Zstd);
        // Incompressible bytes are still encoded by a model, never stored.
        let noise: Vec<u8> = (0..5000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let e = b.encode(&noise).unwrap();
        assert_ne!(e.encoded.graph.steps[0].primitive, PrimitiveId::Store);
        // Deterministic: the same text, the same bytes.
        assert_eq!(b.encode(&t).unwrap().encoded.bytes, {
            let mut b2 = PartEncoder::balanced(&o, 64 << 20).unwrap();
            b2.encode(&t).unwrap().encoded.bytes
        });
        // A fresh encoder with the same settings writes the same bytes and plans the same.
        let mut b3 = b.fresh().unwrap();
        assert_eq!(b3.memory(), b.memory());
        assert_eq!(
            b3.encode(&t).unwrap().encoded.bytes,
            b.encode(&t).unwrap().encoded.bytes
        );
        let mut f = PartEncoder::fast(&FastOptions::default(), 64 << 20).unwrap();
        let mut f2 = f.fresh().unwrap();
        assert_eq!(
            f2.encode(&t).unwrap().encoded.bytes,
            f.encode(&t).unwrap().encoded.bytes
        );
        let e = f.encode(&t).unwrap();
        assert_eq!(e.model, PartModel::Zstd);
        assert_eq!(e.encoded.graph.steps[0].primitive, PrimitiveId::Zstd);
        assert_eq!(e.sample_bytes, 0);
    }
}
