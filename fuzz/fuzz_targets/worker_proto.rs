#![no_main]
//! E4-2: arbitrary worker output through the broker (`lpk_core::foreign::replay`): the frame
//! decoder, the limits (entries, listing charge, declared output), the listing checks, the
//! policy pass and the writer over a counting sink — plus detection over the same bytes. No
//! panic, nothing written for a refused listing, and memory under a stated bound (a counting
//! allocator).
//!
//! Layout: bytes 0..4 = the archive length the broker assumes (u32 LE); byte 4 = flags (bit
//! 0: `--max-entries 64`); the rest is the worker's output (frames).
use libfuzzer_sys::fuzz_target;
use lpk_core::foreign::{self, Limits};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// The memory budget given to the broker (its listing charge).
const BUDGET: u64 = 16 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() < 5 {
        return;
    }
    let archive_len = u64::from(u32::from_le_bytes([data[0], data[1], data[2], data[3]]));
    let limits = Limits {
        memory: Some(BUDGET),
        max_entries: if data[4] & 1 != 0 { Some(64) } else { None },
        ..Limits::default()
    };
    let output = &data[5..];
    let _ = foreign::detect(output);
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let (r, written) =
        foreign::replay(output, archive_len, None, &lpk_core::DefaultPolicy, &limits);
    let peak = (ALLOC.peak() - before) as u64;
    // The listing (charged against BUDGET, held as records and as entries: at most three
    // times the charge) plus a frame buffer, one decoded frame and one page of records
    // (each frame at most 1 MiB; a page's records at most three times its bytes).
    assert!(peak <= 3 * BUDGET + (8 << 20), "peak {peak}");
    if let Ok(s) = &r {
        assert_eq!(s.bytes, written);
    }
    // Nothing is written past what the archive length allows under the default expansion.
    assert!(
        written
            <= archive_len
                .max(1)
                .saturating_mul(foreign::DEFAULT_MAX_EXPANSION)
    );
});
