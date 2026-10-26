#![no_main]
//! The JPEG peel's compress side (`JpegPeel::peel`: the marker scan, the memory gate, Lepton's
//! encode and the verifying decode, the secondary-image search) over arbitrary files: no panic
//! (a panic the peel catches is still a finding: libFuzzer's hook aborts on it); a plan covers
//! the file exactly (primary, then nested parts in order, contiguous, to the end), its stream is
//! shorter than the primary image, and its memory is within the limit; nothing is allocated past
//! the peel's memory limit (a counting allocator: the peel refuses an image whose memory term
//! plus the part does not fit it).
//!
//! Layout: byte 0 = flags (bit 0: progressive files refused; bit 1: the part bound is one byte
//! short of the file); the rest is the file. Seeds: the committed JPEG fixtures of `lpk-core`.
use libfuzzer_sys::fuzz_target;
use lpk_core::peel::{JpegPeel, PeelStage};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// The peel's memory limit in a run: the model allowance and room for a few megapixels.
const MEMORY_LIMIT: u64 = 160 << 20;

fuzz_target!(|data: &[u8]| {
    let Some((&flags, file)) = data.split_first() else {
        return;
    };
    let peel = JpegPeel {
        progressive: flags & 1 == 0,
        memory_limit: MEMORY_LIMIT,
        max_file: 1 << 24,
        processor_threads: 1,
    };
    let max_part = if flags & 2 != 0 {
        (file.len() as u64).saturating_sub(1)
    } else {
        file.len() as u64
    };
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let r = peel.peel(file, max_part);
    let peak = (ALLOC.peak() - before) as u64;
    assert!(
        peak <= MEMORY_LIMIT,
        "peak {peak} over the memory limit {MEMORY_LIMIT}"
    );
    if let Ok(plan) = r {
        assert!(plan.primary_len <= max_part);
        assert!((plan.stream.len() as u64) < plan.primary_len);
        assert!(plan.memory.saturating_add(max_part) <= MEMORY_LIMIT);
        assert_eq!(plan.original_len, file.len() as u64);
        let mut at = plan.primary_len;
        for p in &plan.nested {
            assert_eq!(p.offset, at);
            assert!(p.len > 0);
            at += p.len;
        }
        assert_eq!(at, file.len() as u64);
    }
});
