#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_core::peel::deflate::{find_streams, KINDS};

// Layout: byte 0 picks the extension (a ZIP kind, png, gz, pdf or none), the rest is the file.
// The walker must not panic, and its spans must be sorted, disjoint and inside the file.
fuzz_target!(|data: &[u8]| {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    let exts = ["docx", "jar", "apk", "zip", "png", "gz", "pdf", "bin"];
    let ext = exts[usize::from(pick) % exts.len()];
    let found = find_streams(bytes, ext);
    assert!(KINDS.contains(&found.kind));
    for c in &found.candidates {
        let mut end = 0;
        for &(a, b) in &c.spans {
            assert!(a >= end && a <= b && b <= bytes.len());
            end = b;
        }
    }
});
