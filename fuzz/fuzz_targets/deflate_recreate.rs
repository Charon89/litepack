#![no_main]
use libfuzzer_sys::fuzz_target;
use preflate_rs::recreate_whole_deflate_stream_limited;

// Layout: bytes 0..2 = the output limit (u16 LE), bytes 2..4 = the plain text's length (u16 LE,
// cut to what is there), then the plain text, then the corrections. The patched limited
// recreation (D-66) must never return more than the limit; errors are fine. Panics are caught
// here as the decoder catches them (they are `BadRecord`, `corrections`); what this target looks
// for is output past the limit, hangs (libFuzzer's -timeout) and memory growth (-rss_limit_mb).
fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let limit = usize::from(u16::from_le_bytes([data[0], data[1]]));
    let rest = &data[4..];
    let plain_len = usize::from(u16::from_le_bytes([data[2], data[3]])).min(rest.len());
    let (plain, corrections) = rest.split_at(plain_len);
    let r = std::panic::catch_unwind(|| {
        recreate_whole_deflate_stream_limited(plain, corrections, limit)
    });
    if let Ok(Ok(out)) = r {
        assert!(out.len() <= limit);
    }
});
