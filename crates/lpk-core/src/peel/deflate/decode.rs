//! Revision 1.2's `deflate-reconstruct` decoder (spec section 8, "Decoding
//! `deflate-reconstruct`"; design `docs/notes/e2-10-11-design.md`, section 2.2(c)).
//!
//! The step's input is the plain text of one Deflate stream, `plain_len` bytes; its output is the
//! original raw Deflate stream, `original_len` bytes, recreated by the patched `preflate-rs`
//! 0.7.6 (D-66) from the record's `corrections` under an output limit of `original_len`.
//!
//! Order of checks, exactly as performed:
//! 1. the record by id (the errors of the `Records` frame);
//! 2. its kind is `deflate`, otherwise `BadRecord`, `kind`;
//! 3. `original_len` against the step's output bound: the last step's must equal the block's
//!    `plain_len` (`BlockLengthMismatch`), another step's must not exceed its bound
//!    (`PayloadTooLarge`);
//! 4. `corrections` no longer than `original_len`, otherwise `BadRecord`, `corrections`;
//! 5. the memory term ([`memory_term`]) within the decode memory left after `max_block_plain`,
//!    otherwise `Refused`, field `decode_memory`;
//! 6. the input exactly `plain_len` bytes, otherwise `BadRecord`, `plain_len`;
//! 7. recreation: refused corrections or a panic are `BadRecord`, `corrections`; output past
//!    `original_len` is `BadRecord`, `original_len` (the recreation stops at the limit);
//! 8. output shorter than `original_len` is `BadRecord`, `original_len`; a BLAKE3 other than
//!    `original_hash` is `BadRecord`, `original_hash`.
//!
//! Checks 1 to 5 need only the record: [`PrimitiveDecoder::input_bound`] runs them before the
//! previous step and returns `plain_len` as that step's output bound; `decode_in` runs them again
//! (the records are cached) and then 6 to 8.
//!
//! A hostile record can make the library panic (check 7 contains it). The panic still goes
//! through the process's panic hook first, so with the default hook one "thread panicked" line is
//! printed on standard error per such record; this decoder does not change the global hook.

use std::panic::{catch_unwind, AssertUnwindSafe};

use lpk_format::envelope::Refusal;
use lpk_format::{
    DecodeContext, DeflateRecord, FormatError, PrimitiveDecoder, PrimitiveId, RecordBody, Resources,
};
use preflate_rs::{recreate_whole_deflate_stream_limited, ExitCode};

/// Decode memory per plain byte of a stream, from the layout of the patched 0.7.6 recreation:
/// - 1 byte: the step's input (the previous step's output, held by the reader);
/// - 1 byte: the library's own copy of it (`PlainText`, filled by one exact-size append);
/// - 6 bytes: the token list of the block being recreated. A `DeflateToken` is 6 bytes (a tag
///   and a 4-byte reference, alignment 2), and one block may hold a token per plain byte: a
///   block of literals only, which is a valid stream, or any `TokenCount` a crafted record
///   decodes (the reservation is `min(TokenCount, input remaining)` exactly, and the loop never
///   pushes past it). The limited recreation drops each block's tokens once written, so only
///   one block's list is live.
///
/// The output is not part of the term: it is the block's own buffer (the capped writer reserves
/// `original_len + 8` bytes once, and `original_len` is the block's `plain_len` or at most
/// `max_block_plain`). Measured (`tests/deflate_memory.rs`, a counting allocator): a
/// literals-only block needs exactly this per-byte part; the fixed allowance covers the rest.
pub const MEMORY_PER_PLAIN_BYTE: u64 = 8;
/// Decode memory per byte of the record's `corrections`: the decoder's own parsed record (the
/// reader parses a fresh `Record` per lookup, holding a copy of the corrections) and the
/// library's decoded `ReconstructionData`, whose bitcode decoding copies the CABAC bytes into a
/// new vector. Both are live during the recreation; the archive's `Records` frame is the
/// reader's, outside the step.
pub const MEMORY_PER_CORRECTION_BYTE: u64 = 2;
/// The fixed allowance for the library's tables (hash chains, Huffman tables, the CABAC
/// contexts) and the small buffers of a recreation; the test asserts at least half of it is left
/// unused in the worst cases it measures.
pub const MEMORY_ALLOWANCE: u64 = 16 * 1024 * 1024;

/// The decode memory a stream of `plain_len` plain bytes with `corrections_len` correction bytes
/// needs beyond the block buffer: `8 × plain_len + 2 × corrections_len + 16777216` (saturating).
/// Both lengths come from the record, so check 5 runs before anything is decoded.
pub fn memory_term(plain_len: u64, corrections_len: u64) -> u64 {
    plain_len
        .saturating_mul(MEMORY_PER_PLAIN_BYTE)
        .saturating_add(corrections_len.saturating_mul(MEMORY_PER_CORRECTION_BYTE))
        .saturating_add(MEMORY_ALLOWANCE)
}

/// Revision 1.2's `deflate-reconstruct` decoder; see the module text for its checks.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeflateDecoder;

fn bad(record: u64, reason: &'static str) -> FormatError {
    FormatError::BadRecord { record, reason }
}

#[cfg(test)]
thread_local! {
    /// Test hook: when set, the recreation panics (inside the containment).
    static PANIC_IN_RECREATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl DeflateDecoder {
    /// Checks 1 to 5: the record, checked against the step's bound and the decode memory.
    fn checked_record(
        params: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<(u64, DeflateRecord), FormatError> {
        let id =
            PrimitiveId::DeflateReconstruct
                .record_id(params)
                .ok_or(FormatError::BadParams {
                    id: PrimitiveId::DeflateReconstruct as u16,
                    reason: "record_id",
                })?;
        // 1, 2
        let RecordBody::Deflate(d) = ctx.record(id)?.body else {
            return Err(bad(id, "kind"));
        };
        // 3
        if last {
            if d.original_len != expected_len {
                return Err(FormatError::BlockLengthMismatch { block: ctx.block() });
            }
        } else if d.original_len > expected_len {
            return Err(FormatError::PayloadTooLarge {
                len: d.original_len,
                max: expected_len,
            });
        }
        // 4
        if d.corrections.len() as u64 > d.original_len {
            return Err(bad(id, "corrections"));
        }
        // 5
        let need = memory_term(d.plain_len, d.corrections.len() as u64);
        let allowance = limits.memory.saturating_sub(limits.max_block_plain);
        if need > allowance {
            return Err(FormatError::Refused(Refusal {
                field: "decode_memory",
                needed: limits.max_block_plain.saturating_add(need),
                allowed: limits.memory,
            }));
        }
        Ok((id, d))
    }
}

impl PrimitiveDecoder for DeflateDecoder {
    fn decode(
        &self,
        _params: &[u8],
        _input: &[u8],
        _expected_len: u64,
        _limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        // Without the archive behind it the record cannot be read.
        Err(FormatError::UnimplementedPrimitive {
            id: PrimitiveId::DeflateReconstruct as u16,
        })
    }

    fn decode_in(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        let (id, d) = Self::checked_record(params, expected_len, last, limits, ctx)?;
        // 6
        if input.len() as u64 != d.plain_len {
            return Err(bad(id, "plain_len"));
        }
        // 7
        let limit = usize::try_from(d.original_len).map_err(|_| bad(id, "original_len"))?;
        let recreated = catch_unwind(AssertUnwindSafe(|| {
            #[cfg(test)]
            if PANIC_IN_RECREATION.with(|p| p.get()) {
                panic!("test hook: panic in recreation");
            }
            recreate_whole_deflate_stream_limited(input, &d.corrections, limit)
        }));
        let out = match recreated {
            Err(_) => return Err(bad(id, "corrections")),
            Ok(Err(e)) if e.exit_code() == ExitCode::OutputLimit => {
                return Err(bad(id, "original_len"))
            }
            Ok(Err(_)) => return Err(bad(id, "corrections")),
            Ok(Ok(out)) => out,
        };
        // 8 (the limit makes a longer output impossible; it would be `original_len` too)
        if out.len() as u64 != d.original_len {
            return Err(bad(id, "original_len"));
        }
        if blake3::hash(&out).as_bytes() != &d.original_hash {
            return Err(bad(id, "original_hash"));
        }
        Ok(out)
    }

    fn input_bound(
        &self,
        params: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Option<u64>, FormatError> {
        // Checks 1 to 5; check 5 bounds `plain_len` by the archive's decode memory (never more
        // than the reader's) left after `max_block_plain`, as the hook's contract requires.
        let (_, d) = Self::checked_record(params, expected_len, last, limits, ctx)?;
        Ok(Some(d.plain_len))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lpk_format::{Record, RecordKind};

    /// A context holding one record (id 0).
    struct One(Record);
    impl DecodeContext for One {
        fn block(&self) -> usize {
            3
        }
        fn record(&mut self, id: u64) -> Result<Record, FormatError> {
            if id == 0 {
                Ok(self.0.clone())
            } else {
                Err(FormatError::RecordOutOfRange {
                    record: id,
                    count: 1,
                })
            }
        }
        fn chunk(&mut self, _: u64, index: u64) -> Result<Vec<u8>, FormatError> {
            Err(FormatError::ChunkIndexOutOfRange {
                chunk: index,
                len: 0,
            })
        }
    }

    /// A real stream: (plain text, record).
    fn stream() -> (Vec<u8>, DeflateRecord) {
        use std::io::Write;
        let plain: Vec<u8> = (0..20_000u32)
            .flat_map(|i| format!("line {} of the text\n", i % 97).into_bytes())
            .collect();
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        e.write_all(&plain).unwrap();
        let deflated = e.finish().unwrap();
        let (r, p) = preflate_rs::preflate_whole_deflate_stream(
            &deflated,
            &super::super::analyse::preflate_config(super::super::PLAIN_TEXT_LIMIT),
        )
        .unwrap();
        assert_eq!(r.compressed_size, deflated.len());
        assert_eq!(p.text(), plain.as_slice());
        let d = DeflateRecord {
            original_len: deflated.len() as u64,
            plain_len: plain.len() as u64,
            corrections: r.corrections,
            library: 0,
            original_hash: *blake3::hash(&deflated).as_bytes(),
        };
        (plain, d)
    }

    fn run(d: &DeflateRecord, input: &[u8]) -> Result<Vec<u8>, FormatError> {
        let mut ctx = One(Record::new(RecordBody::Deflate(d.clone())));
        DeflateDecoder.decode_in(
            &[0],
            input,
            d.original_len,
            true,
            &Resources::default(),
            &mut ctx,
        )
    }

    fn reason(r: Result<Vec<u8>, FormatError>) -> &'static str {
        match r {
            Err(FormatError::BadRecord { record: 0, reason }) => reason,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_real_stream_recreates() {
        let (plain, d) = stream();
        let out = run(&d, &plain).unwrap();
        assert_eq!(out.len() as u64, d.original_len);
        assert_eq!(blake3::hash(&out).as_bytes(), &d.original_hash);
    }

    #[test]
    fn every_bad_record_reason() {
        let (plain, d) = stream();
        // kind
        let mut ctx = One(Record {
            kind: RecordKind::Utf16,
            body: RecordBody::Utf16(lpk_format::Utf16Record {
                endian: 0,
                bom: 0,
                original_len: 4,
                original_hash: [0; 32],
            }),
        });
        let r = DeflateDecoder.decode_in(&[0], &plain, 4, true, &Resources::default(), &mut ctx);
        assert_eq!(reason(r), "kind");
        // corrections: longer than original_len
        let mut long = d.clone();
        long.corrections = vec![0; d.original_len as usize + 1];
        assert_eq!(reason(run(&long, &plain)), "corrections");
        // corrections: the library refuses them
        let mut garbage = d.clone();
        garbage.corrections = vec![0xA5; 16];
        assert_eq!(reason(run(&garbage, &plain)), "corrections");
        // plain_len: the input is not plain_len bytes
        assert_eq!(reason(run(&d, &plain[1..])), "plain_len");
        // original_len: recreation would exceed it (record and block both say one byte less)
        let mut short = d.clone();
        short.original_len -= 1;
        assert_eq!(reason(run(&short, &plain)), "original_len");
        // original_len: recreation ends short of it
        let mut longer = d.clone();
        longer.original_len += 1;
        assert_eq!(reason(run(&longer, &plain)), "original_len");
        // original_hash
        let mut hash = d.clone();
        hash.original_hash[0] ^= 1;
        assert_eq!(reason(run(&hash, &plain)), "original_hash");
    }

    #[test]
    fn check_3_block_length_and_payload_bound() {
        let (plain, d) = stream();
        let mut ctx = One(Record::new(RecordBody::Deflate(d.clone())));
        let lim = Resources::default();
        let r = DeflateDecoder.decode_in(&[0], &plain, d.original_len + 1, true, &lim, &mut ctx);
        assert!(matches!(
            r,
            Err(FormatError::BlockLengthMismatch { block: 3 })
        ));
        let r = DeflateDecoder.decode_in(&[0], &plain, d.original_len - 1, false, &lim, &mut ctx);
        assert!(matches!(r, Err(FormatError::PayloadTooLarge { len, max })
            if len == d.original_len && max == d.original_len - 1));
        // Not the last step and within its bound: decodes.
        let r = DeflateDecoder.decode_in(&[0], &plain, d.original_len, false, &lim, &mut ctx);
        assert_eq!(r.unwrap().len() as u64, d.original_len);
    }

    #[test]
    fn check_5_refuses_by_the_memory_term() {
        let (plain, d) = stream();
        let need = memory_term(d.plain_len, d.corrections.len() as u64);
        let mut ctx = One(Record::new(RecordBody::Deflate(d.clone())));
        let exact = Resources {
            max_block_plain: 1000,
            memory: 1000 + need,
            ..Resources::default()
        };
        assert!(DeflateDecoder
            .decode_in(&[0], &plain, d.original_len, true, &exact, &mut ctx)
            .is_ok());
        let less = Resources {
            memory: 999 + need,
            ..exact
        };
        let r = DeflateDecoder.decode_in(&[0], &plain, d.original_len, true, &less, &mut ctx);
        assert!(
            matches!(r, Err(FormatError::Refused(Refusal { field: "decode_memory", needed, allowed }))
            if needed == 1000 + need && allowed == 999 + need)
        );
        // The hook refuses the same way, before any step runs.
        let r = DeflateDecoder.input_bound(&[0], d.original_len, true, &less, &mut ctx);
        assert!(matches!(r, Err(FormatError::Refused(_))));
        let r = DeflateDecoder.input_bound(&[0], d.original_len, true, &exact, &mut ctx);
        assert_eq!(r.unwrap(), Some(d.plain_len));
    }

    #[test]
    fn the_hook_runs_checks_1_to_4() {
        let (_, d) = stream();
        let lim = Resources::default();
        let mut ctx = One(Record::new(RecordBody::Deflate(d.clone())));
        assert!(matches!(
            DeflateDecoder.input_bound(&[1], d.original_len, true, &lim, &mut ctx),
            Err(FormatError::RecordOutOfRange { record: 1, .. })
        ));
        assert!(matches!(
            DeflateDecoder.input_bound(&[0], d.original_len - 1, true, &lim, &mut ctx),
            Err(FormatError::BlockLengthMismatch { block: 3 })
        ));
        assert!(matches!(
            DeflateDecoder.input_bound(&[0], d.original_len - 1, false, &lim, &mut ctx),
            Err(FormatError::PayloadTooLarge { .. })
        ));
        let mut long = d.clone();
        long.corrections = vec![0; d.original_len as usize + 1];
        let mut ctx = One(Record::new(RecordBody::Deflate(long)));
        assert!(matches!(
            DeflateDecoder.input_bound(&[0], d.original_len, true, &lim, &mut ctx),
            Err(FormatError::BadRecord {
                reason: "corrections",
                ..
            })
        ));
    }

    #[test]
    fn a_panic_in_recreation_is_bad_record_corrections() {
        let (plain, d) = stream();
        PANIC_IN_RECREATION.with(|p| p.set(true));
        let r = run(&d, &plain);
        PANIC_IN_RECREATION.with(|p| p.set(false));
        assert_eq!(reason(r), "corrections");
        assert!(run(&d, &plain).is_ok());
    }

    #[test]
    fn without_the_archive_it_is_unimplemented() {
        assert!(matches!(
            DeflateDecoder.decode(&[0], b"", 0, &Resources::default()),
            Err(FormatError::UnimplementedPrimitive { id: 8 })
        ));
    }
}
