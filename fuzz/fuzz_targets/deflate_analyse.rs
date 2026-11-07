#![no_main]
//! The Deflate peel's analysis (`lpk_core::peel::deflate::examine`: the walker, then per stream
//! preflate's analysis and the bounded recreation compared with the stream) over arbitrary files:
//! no panic (the walker's and the library's panics are caught and counted by `examine`, but
//! libFuzzer's hook aborts on any panic, so a caught one is a finding too); every candidate is
//! counted exactly once; a reconstructed stream's spans are sorted, disjoint, inside the file and
//! as long as its stream, and its plain text is within the plain-text limit. Memory has no
//! byte bound in the code beyond the plain-text limit and the work budget, so `-rss_limit_mb`
//! is the check.
//!
//! Layout: byte 0 picks the extension (a ZIP kind, png, gz, pdf or none) and, in bit 7, a small
//! file plain budget (64 KiB) so later streams are skipped; the rest is the file. Seeds: the
//! committed Deflate fixtures of `lpk-core`.
use libfuzzer_sys::fuzz_target;
use lpk_core::peel::deflate::{examine, Limits, KINDS};

/// A run's plain-text limit per stream (small, so runs stay fast).
const PLAIN_TEXT_LIMIT: u64 = 1 << 20;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    let exts = ["docx", "jar", "apk", "zip", "png", "gz", "pdf", "bin"];
    let ext = exts[usize::from(pick & 0x7f) % exts.len()];
    let limits = Limits {
        plain_text_limit: PLAIN_TEXT_LIMIT,
        file_plain_budget: if pick & 0x80 != 0 { 1 << 16 } else { 4 << 20 },
    };
    let mut done = 0u64;
    let ex = examine(bytes, ext, &limits, |r| {
        done += 1;
        let mut end = 0;
        let mut len = 0;
        for &(a, b) in &r.spans {
            assert!(a >= end && a <= b && b <= bytes.len());
            end = b;
            len += b - a;
        }
        assert_eq!(len, r.deflate_len);
        assert!(r.plain.len() as u64 <= PLAIN_TEXT_LIMIT);
    });
    assert!(KINDS.contains(&ex.kind));
    let c = &ex.counts;
    assert_eq!(c.walker_panics, 0, "the walker panicked");
    assert!(!c.failed.contains_key(lpk_core::peel::deflate::PANIC));
    assert_eq!(c.reconstructed, done);
    assert_eq!(c.found, c.reconstructed + c.skipped + c.failed_total());
});
