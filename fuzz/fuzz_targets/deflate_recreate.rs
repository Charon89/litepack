#![no_main]
use libfuzzer_sys::fuzz_target;
use preflate_rs::recreate_whole_deflate_stream_limited_report;

/// The largest plain text a run takes (small, so runs stay fast).
const MAX_PLAIN: usize = 1 << 16;

// Layout: bytes 0..2 = the output limit (u16 LE), bytes 2..6 = the plain text's length (u32 LE,
// cut to MAX_PLAIN and to what is there), then the plain text, then the corrections. The patched
// limited recreation (D-66) must never return more than the limit and its writer's buffer must
// never exceed `limit + 8` bytes; errors are fine. Panics are caught here as the decoder catches
// them (they are `BadRecord`, `corrections`); what this target also looks for is hangs
// (libFuzzer's -timeout) and memory growth (-rss_limit_mb).
fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        return;
    }
    let limit = usize::from(u16::from_le_bytes([data[0], data[1]]));
    let rest = &data[6..];
    let declared = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    let plain_len = usize::try_from(declared)
        .unwrap_or(usize::MAX)
        .min(MAX_PLAIN)
        .min(rest.len());
    let (plain, corrections) = rest.split_at(plain_len);
    let r = std::panic::catch_unwind(|| {
        recreate_whole_deflate_stream_limited_report(plain, corrections, limit)
    });
    if let Ok((result, capacity)) = r {
        assert!(capacity <= limit + 8, "capacity {capacity} over limit {limit} + 8");
        if let Ok(out) = result {
            assert!(out.len() <= limit);
        }
    }
});
