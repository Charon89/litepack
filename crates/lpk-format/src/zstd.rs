//! The `zstd` primitive (ID 1) decoded in pure Rust with `ruzstd` (spec
//! section 8). Window and prior rules are enforced here, before any input
//! is decoded.

use crate::decode::PrimitiveDecoder;
use crate::envelope::Resources;
use crate::error::FormatError;
use crate::priors::{prior_id, NoPriors, PriorStore};
use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, Dictionary, FrameDecoder};
use std::sync::Arc;

const ID: u16 = 1;
/// Bytes the decoder is asked to produce per `decode_blocks` call (it decodes whole blocks,
/// so it may produce up to one block more): with the window, what its ring buffer holds.
pub const DECODE_STEP: usize = 64 * 1024;
const STEP: usize = DECODE_STEP;

fn zerr(e: impl std::fmt::Display) -> FormatError {
    FormatError::ZstdError {
        reason: e.to_string(),
    }
}

/// Decodes a zstd frame, or a sequence of them, into at most `expected_len`
/// bytes, with the priors of its store.
#[derive(Clone)]
pub struct ZstdDecoder {
    priors: Arc<dyn PriorStore>,
}

impl std::fmt::Debug for ZstdDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ZstdDecoder")
    }
}

impl Default for ZstdDecoder {
    fn default() -> Self {
        ZstdDecoder::new(Arc::new(NoPriors))
    }
}

impl ZstdDecoder {
    /// A decoder that looks priors up in `priors`.
    pub fn new(priors: Arc<dyn PriorStore>) -> Self {
        ZstdDecoder { priors }
    }

    /// The dictionary a step names, checked against its ID, or `None`. Each
    /// call re-hashes the prior and parses its tables (ruzstd's `Dictionary`
    /// cannot be cloned, so it is not cached): a cost per block, small next to
    /// decoding a block.
    fn dictionary(&self, id: &[u8; 32]) -> Result<Option<Dictionary>, FormatError> {
        if id.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        let bytes = self
            .priors
            .get(id)
            .filter(|b| prior_id(b) == *id)
            .ok_or(FormatError::MissingPrior { id: *id })?;
        Dictionary::decode_dict(bytes).map(Some).map_err(zerr)
    }
}

impl ZstdDecoder {
    /// The output length of a frame's complete blocks before a damaged one: the frame cut at
    /// `done` (the end of the last complete block, counted from the frame's start) with that
    /// block's `Last_Block` flag set (and four placeholder checksum bytes when the frame has a
    /// checksum), decoded on its own. `None` when there is no complete block or the cut frame does
    /// not decode (then the original error stands).
    fn complete_blocks_len(
        &self,
        frame: &[u8],
        done: u64,
        last_header: Option<u64>,
        dict: &[u8; 32],
    ) -> Option<u64> {
        let header = usize::try_from(last_header?).ok()?;
        let end = usize::try_from(done).ok()?;
        let mut cut = frame.get(..end)?.to_vec();
        *cut.get_mut(header)? |= 1;
        // Frame_Header_Descriptor (after the 4-byte magic): bit 2 is Content_Checksum_flag.
        if frame.get(4).is_some_and(|d| d & 0x04 != 0) {
            cut.extend_from_slice(&[0; 4]);
        }
        let mut dec = FrameDecoder::new();
        dec.set_max_window_size(u64::MAX);
        let dictionary = self.dictionary(dict).ok()?;
        let key = dictionary.as_ref().map(|d| d.id);
        if let Some(d) = dictionary {
            dec.add_dict(d).ok()?;
        }
        let mut src = &cut[..];
        dec.init(&mut src).ok()?;
        if let Some(id) = key {
            dec.force_dict(id).ok()?;
        }
        dec.decode_blocks(&mut src, BlockDecodingStrategy::All)
            .ok()?;
        dec.is_finished().then(|| dec.can_collect() as u64)
    }
}

/// Map a frame decoder error: a window above the declared one is a parameter
/// error, everything else is the decoder's text.
fn map_frame_error(e: FrameDecoderError) -> FormatError {
    match e {
        FrameDecoderError::WindowSizeTooBig { .. } => FormatError::BadParams {
            id: ID,
            reason: "frame window exceeds declared",
        },
        other => zerr(other),
    }
}

/// Move what the decoder can hand out into `out`, refusing to pass `max`. The bytes go
/// from the decoder's ring buffer straight into `out` (`collect_to_writer`), with no
/// intermediate copy, so the decoder holds `out` plus its ring buffer (E2-20).
fn drain(dec: &mut FrameDecoder, out: &mut Vec<u8>, max: u64) -> Result<(), FormatError> {
    while dec.can_collect() > 0 {
        let n = dec.can_collect();
        let len = (out.len() as u64).saturating_add(n as u64);
        if len > max {
            return Err(FormatError::PayloadTooLarge { len, max });
        }
        let need = out.len() + n;
        if out.capacity() < need {
            // Grow by an eighth (at least 64 KiB), never past `max`:
            // capacity stays within `max`, copies stay amortised.
            let step = (out.len() / 8).max(STEP).max(n);
            let cap = (out.len() + step).min(max.try_into().unwrap_or(usize::MAX));
            out.reserve_exact(cap.max(need) - out.len());
        }
        if dec.collect_to_writer(&mut *out).map_err(zerr)? == 0 {
            break;
        }
    }
    Ok(())
}

impl PrimitiveDecoder for ZstdDecoder {
    fn decode(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        let bad = |reason| FormatError::BadParams { id: ID, reason };
        let (&window_log, rest) = params.split_first().ok_or_else(|| bad("length"))?;
        let dict: [u8; 32] = rest.try_into().map_err(|_| bad("length"))?;
        if !(10..=31).contains(&window_log) {
            return Err(bad("window_log"));
        }
        let declared = 1u64 << window_log;
        if declared > limits.max_window {
            return Err(FormatError::WindowTooLarge {
                needed: declared,
                allowed: limits.max_window,
            });
        }
        let dictionary = self.dictionary(&dict)?;
        // No frame at all cannot produce a non-empty output.
        if input.is_empty() && expected_len > 0 {
            return Err(zerr("truncated"));
        }
        let dict_key = dictionary.as_ref().map(|d| d.id);

        let mut dec = FrameDecoder::new();
        dec.set_max_window_size(declared);
        if let Some(d) = dictionary {
            dec.add_dict(d).map_err(map_frame_error)?;
        }
        let mut src = input;
        // Reserved once to the block (bounded by the reader's block limit): the output never
        // reallocates past it (E2-20).
        let reserve = usize::try_from(expected_len.min(limits.max_block_plain)).unwrap_or(0);
        let mut out: Vec<u8> = Vec::with_capacity(reserve);
        while !src.is_empty() {
            let frame_start = input.len() - src.len();
            let out_before_frame = out.len() as u64;
            match dec.init(&mut src) {
                Ok(()) => {}
                Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                    length,
                    ..
                })) => {
                    src = src
                        .get(length as usize..)
                        .ok_or_else(|| zerr("skippable frame is cut short"))?;
                    continue;
                }
                Err(e) => return Err(map_frame_error(e)),
            }
            if let Some(id) = dict_key {
                dec.force_dict(id).map_err(map_frame_error)?;
            }
            // One block at a time, so a damaged block's position is known: the end of the
            // last complete block (`done`), whose header is at `last_header`.
            let mut last_header: Option<u64> = None;
            loop {
                let done = dec.bytes_read_from_source();
                if let Err(e) = dec.decode_blocks(&mut src, BlockDecodingStrategy::UptoBlocks(1)) {
                    let e = map_frame_error(e);
                    if matches!(e, FormatError::ZstdError { .. }) {
                        // Spec section 8, zstd rule 7: the output of the complete blocks before
                        // the damaged one past the bound is `PayloadTooLarge` (the damaged
                        // block's partial output is never counted).
                        let frame = &input[frame_start..];
                        if let Some(held) =
                            self.complete_blocks_len(frame, done, last_header, &dict)
                        {
                            let len = out_before_frame.saturating_add(held);
                            if len > expected_len {
                                return Err(FormatError::PayloadTooLarge {
                                    len,
                                    max: expected_len,
                                });
                            }
                        }
                    }
                    return Err(e);
                }
                last_header = Some(done);
                drain(&mut dec, &mut out, expected_len)?;
                if dec.is_finished() {
                    break;
                }
            }
            if let (Some(stored), Some(computed)) =
                (dec.get_checksum_from_data(), dec.get_calculated_checksum())
            {
                if stored != computed {
                    return Err(zerr("content checksum mismatch"));
                }
            }
        }
        Ok(out)
    }
}
