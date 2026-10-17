//! `deflate-reconstruct` (revision 1.2, section 8 "Decoding `deflate-reconstruct`"): checks (2) to
//! (5), which need only the record, and checks (6) to (8), the recreation through the cited library
//! (`preflate-rs` 0.7.6, its output-limited recreation, D-49/D-66). Check (1), the record lookup,
//! needs the archive and is in [`crate::archive`].

use crate::block::too_large;
use crate::error::{Error, Result};
use crate::record::{bad_record, DeflateRecord};

/// The fixed allowance of a stream's term (section 8, "Resources").
pub const FIXED_TERM: u64 = 16_777_216;

/// A stream's term: 8 × `plain_len` + 2 × (corrections length) + 16777216, in 64-bit unsigned
/// arithmetic that saturates at 2^64 - 1.
pub fn term(plain_len: u64, corrections_len: u64) -> u64 {
    plain_len
        .saturating_mul(8)
        .saturating_add(corrections_len.saturating_mul(2))
        .saturating_add(FIXED_TERM)
}

/// What check (5) compares against: the archive's `decode_memory`, never above the reader's memory
/// resource, and `max_block_plain`.
#[derive(Debug, Clone, Copy)]
pub struct Memory {
    /// The envelope's `decode_memory`.
    pub decode_memory: u64,
    /// The reader's `memory` resource.
    pub memory: u64,
    /// The envelope's `max_block_plain`.
    pub max_block_plain: u64,
}

impl Memory {
    /// Check (5) of both reconstruction steps: `term` above the decode memory left after
    /// `max_block_plain` is `Refused` (field `decode_memory`). The message carries what the text
    /// states informatively: `needed` = `max_block_plain` + the term, `allowed` = the decode memory
    /// compared against (the archive's `decode_memory`, or the reader's memory if smaller).
    pub fn check(&self, term: u64) -> Result<()> {
        let allowed = self.decode_memory.min(self.memory);
        if term > allowed.saturating_sub(self.max_block_plain) {
            let needed = self.max_block_plain.saturating_add(term);
            return Err(Error::new(
                "Refused",
                format!(
                    "the archive needs decode_memory of {needed} bytes; this reader allows {allowed}"
                ),
            ));
        }
        Ok(())
    }
}

/// Checks (2) to (5) on a record already looked up (check (1)): `kind` is the record's kind,
/// `rec` its parsed `deflate` body when the kind is 8. `bound` is the step's output bound and
/// `last` whether the step is the last of its graph.
pub fn record_checks<'r>(
    id: u64,
    rec: Option<&'r DeflateRecord>,
    bound: u64,
    last: bool,
    mem: &Memory,
) -> Result<&'r DeflateRecord> {
    // (2) the kind.
    let r = rec.ok_or_else(|| bad_record(id, "kind"))?;
    // (3) original_len against the step's output bound.
    if last {
        if r.original_len != bound {
            return Err(Error::new(
                "BlockLengthMismatch",
                format!(
                    "deflate-reconstruct: record {id} original_len {} differs from plain_len {bound}",
                    r.original_len
                ),
            ));
        }
    } else if r.original_len > bound {
        return Err(too_large(false, r.original_len, bound));
    }
    // (4) corrections no longer than original_len.
    if r.corrections.len() as u64 > r.original_len {
        return Err(bad_record(id, "corrections"));
    }
    // (5) resources.
    mem.check(term(r.plain_len, r.corrections.len() as u64))?;
    Ok(r)
}

/// Checks (6) to (8): the input is exactly `plain_len` bytes, the stream is recreated from it and
/// the corrections (stopping as soon as the output would pass `original_len`), and the output is
/// exactly `original_len` bytes hashing to `original_hash`.
pub fn recreate(id: u64, r: &DeflateRecord, input: &[u8]) -> Result<Vec<u8>> {
    // (6).
    if input.len() as u64 != r.plain_len {
        return Err(bad_record(id, "plain_len"));
    }
    // (7). A reader that cannot address `original_len` bytes refuses with `original_len`.
    let limit = usize::try_from(r.original_len).map_err(|_| bad_record(id, "original_len"))?;
    let corrections = &r.corrections[..];
    let run = std::panic::catch_unwind(|| {
        preflate_rs::recreate_whole_deflate_stream_limited(input, corrections, limit)
    });
    let out = match run {
        Err(_) => return Err(bad_record(id, "corrections")),
        Ok(Err(e)) if e.exit_code() == preflate_rs::ExitCode::OutputLimit => {
            return Err(bad_record(id, "original_len"))
        }
        Ok(Err(_)) => return Err(bad_record(id, "corrections")),
        Ok(Ok(out)) => out,
    };
    // (8).
    if out.len() != limit {
        return Err(bad_record(id, "original_len"));
    }
    if blake3::hash(&out).as_bytes() != &r.original_hash {
        return Err(bad_record(id, "original_hash"));
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rec(original: &[u8], plain_len: u64, corrections: Vec<u8>) -> DeflateRecord {
        DeflateRecord {
            original_len: original.len() as u64,
            plain_len,
            corrections,
            original_hash: *blake3::hash(original).as_bytes(),
        }
    }

    const MEM: Memory = Memory {
        decode_memory: 1 << 30,
        memory: 1 << 31,
        max_block_plain: 1 << 20,
    };

    #[test]
    fn term_saturates() {
        assert_eq!(term(10, 3), 80 + 6 + FIXED_TERM);
        assert_eq!(term(u64::MAX / 4, 0), u64::MAX);
        assert_eq!(term(0, u64::MAX), u64::MAX);
    }

    #[test]
    fn check_5_reports_needed_and_allowed() {
        let m = Memory {
            decode_memory: 1000 + FIXED_TERM,
            memory: u64::MAX,
            max_block_plain: 900,
        };
        // 8 × 12 = 96 <= 100 left after max_block_plain; 8 × 13 = 104 > 100.
        assert!(m.check(term(12, 0)).is_ok());
        let e = m.check(term(13, 0)).unwrap_err();
        assert_eq!(e.class, "Refused");
        assert_eq!(
            e.detail,
            format!(
                "the archive needs decode_memory of {} bytes; this reader allows {}",
                900 + 104 + FIXED_TERM,
                1000 + FIXED_TERM
            )
        );
        // The reader's memory below the archive's: compared against the reader's.
        let m = Memory {
            memory: 950 + FIXED_TERM,
            ..m
        };
        let e = m.check(term(12, 0)).unwrap_err();
        assert!(e.detail.ends_with(&format!("allows {}", 950 + FIXED_TERM)));
        // A saturated term is refused.
        assert!(MEM.check(u64::MAX).is_err());
    }

    #[test]
    fn checks_2_to_5_in_order() {
        let r = rec(b"0123456789", 4, vec![1; 10]);
        // (2) a record of another kind.
        let e = record_checks(3, None, 10, true, &MEM).unwrap_err();
        assert_eq!(e.detail, "bad record 3: kind");
        // (3) last step: original_len must equal plain_len.
        assert!(record_checks(3, Some(&r), 10, true, &MEM).is_ok());
        assert_eq!(
            record_checks(3, Some(&r), 11, true, &MEM)
                .unwrap_err()
                .class,
            "BlockLengthMismatch"
        );
        assert_eq!(
            record_checks(3, Some(&r), 9, true, &MEM).unwrap_err().class,
            "BlockLengthMismatch"
        );
        // (3) not last: above the bound only.
        assert!(record_checks(3, Some(&r), 11, false, &MEM).is_ok());
        assert_eq!(
            record_checks(3, Some(&r), 9, false, &MEM)
                .unwrap_err()
                .class,
            "PayloadTooLarge"
        );
        // (4) corrections longer than original_len, before (5).
        let long = rec(b"0123456789", u64::MAX, vec![1; 11]);
        assert_eq!(
            record_checks(3, Some(&long), 10, true, &MEM)
                .unwrap_err()
                .detail,
            "bad record 3: corrections"
        );
        // (5) a plain_len whose term the decode memory cannot hold.
        let big = rec(b"0123456789", 1 << 40, vec![]);
        assert_eq!(
            record_checks(3, Some(&big), 10, true, &MEM)
                .unwrap_err()
                .class,
            "Refused"
        );
    }

    /// A raw Deflate stream of `plain`, its corrections from the cited library's analysis (test
    /// only: the decoder never analyses).
    fn peel(plain: &[u8], level: u32) -> (Vec<u8>, Vec<u8>) {
        use std::io::Write;
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
        e.write_all(plain).unwrap();
        let stream = e.finish().unwrap();
        let (res, _) = preflate_rs::preflate_whole_deflate_stream(
            &stream,
            &preflate_rs::PreflateConfig::default(),
        )
        .unwrap();
        (stream, res.corrections)
    }

    #[test]
    fn checks_6_to_8() {
        let plain: Vec<u8> = (0..2_000u32)
            .flat_map(|i| {
                format!(
                    "line {i}: {} words
",
                    i * 37 % 101
                )
                .into_bytes()
            })
            .collect();
        let (stream, corr) = peel(&plain, 6);
        let r = rec(&stream, plain.len() as u64, corr.clone());
        assert_eq!(recreate(0, &r, &plain).unwrap(), stream);
        // (6) input of another length.
        assert_eq!(
            recreate(0, &r, &plain[1..]).unwrap_err().detail,
            "bad record 0: plain_len"
        );
        // (7) an original_len below the recreated length: stopped, original_len.
        let short = DeflateRecord {
            original_len: stream.len() as u64 - 1,
            ..r.clone()
        };
        assert_eq!(
            recreate(0, &short, &plain).unwrap_err().detail,
            "bad record 0: original_len"
        );
        // (8) a longer original_len: the output is shorter.
        let long = DeflateRecord {
            original_len: stream.len() as u64 + 1,
            ..r.clone()
        };
        assert_eq!(
            recreate(0, &long, &plain).unwrap_err().detail,
            "bad record 0: original_len"
        );
        // (8) another hash.
        let mut h = r.clone();
        h.original_hash[0] ^= 1;
        assert_eq!(
            recreate(0, &h, &plain).unwrap_err().detail,
            "bad record 0: original_hash"
        );
        // (7) corrections the library refuses.
        let bad = DeflateRecord {
            corrections: vec![0xFF; 8],
            ..r.clone()
        };
        assert_eq!(
            recreate(0, &bad, &plain).unwrap_err().detail,
            "bad record 0: corrections"
        );
        let empty = DeflateRecord {
            corrections: Vec::new(),
            ..r
        };
        assert_eq!(
            recreate(0, &empty, &plain).unwrap_err().detail,
            "bad record 0: corrections"
        );
    }

    /// Damaged corrections never panic out of the step or pass: each one byte flip is refused or
    /// recreates a stream that the hash check rejects, unless it recreates the same stream.
    #[test]
    fn corrections_flips_are_contained() {
        let plain: Vec<u8> = b"the quick brown fox jumps over the lazy dog. ".repeat(200);
        let (stream, corr) = peel(&plain, 9);
        let r = rec(&stream, plain.len() as u64, corr.clone());
        for i in 0..corr.len() {
            for bit in [0x01u8, 0x80] {
                let mut c = corr.clone();
                c[i] ^= bit;
                let m = DeflateRecord {
                    corrections: c,
                    ..r.clone()
                };
                match recreate(0, &m, &plain) {
                    Ok(out) => assert_eq!(out, stream),
                    Err(e) => assert_eq!(e.class, "BadRecord"),
                }
            }
        }
    }
}
