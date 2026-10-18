#![no_main]
//! Revision 1.3's `bwt` step over arbitrary input (spec section 8, "Decoding `bwt`"): no panic,
//! and nothing allocated past the output bound plus the memory term (a counting allocator).
//!
//! Layout: byte 0 = the block size minus 1 (1..=256, so a run holds many short blocks); bytes
//! 1..3 = the output bound (u16 LE); byte 3 = flags (bit 0: the step is the last one, so the
//! bound is exact; bit 1: the decode memory is cut to the term minus one, so the step is
//! `Refused`); the rest is the step's input.
use libfuzzer_sys::fuzz_target;
use lpk_codecs_bwt::layout::memory_term;
use lpk_core::BwtDecoder;
use lpk_format::{PrimitiveDecoder, Resources};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let block_size = u32::from(data[0]) + 1;
    let bound = u64::from(u16::from_le_bytes([data[1], data[2]]));
    let last = data[3] & 1 != 0;
    let starve = data[3] & 2 != 0;
    let input = &data[4..];
    let params = block_size.to_le_bytes();
    let term = memory_term(bound, u64::from(block_size));
    let max_block_plain = 1 << 16;
    let memory = if starve {
        max_block_plain + term - 1
    } else {
        max_block_plain + term
    };
    let limits = Resources {
        max_window: 1 << 20,
        max_bwt_block: 1 << 20,
        max_block_plain,
        max_frame_payload: 1 << 22,
        memory,
    };
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let r = BwtDecoder.decode_step(&params, input, bound, last, &limits);
    let peak = (ALLOC.peak() - before) as u64;
    // The output (at most the bound) plus the successor table and small tables (the term; the
    // step's input is the fuzzer's, outside the count).
    assert!(
        peak <= bound + term,
        "peak {peak} over bound {bound} + term {term}"
    );
    match r {
        Ok(out) => {
            assert!(!starve);
            if last {
                assert_eq!(out.len() as u64, bound);
            } else {
                assert!(out.len() as u64 <= bound);
            }
        }
        Err(lpk_format::FormatError::Refused(_)) => assert!(starve),
        Err(_) => {}
    }
});
