#![no_main]
//! The delta stage's format-1 patch decoder (`lpk_core::delta::apply`, design
//! `docs/notes/e2-9-design.md` section 3) over arbitrary input: no panic, and nothing allocated
//! past the output, which a passing header bounds by the patch's own length (a counting
//! allocator; no cover table).
//!
//! Layout: bytes 0..2 = the base length (u16 LE, taken from the front of the rest, as far as
//! it goes); then the base; then the patch.
use libfuzzer_sys::fuzz_target;
use lpk_core::delta::{apply, parse};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let want = usize::from(u16::from_le_bytes([data[0], data[1]]));
    let rest = &data[2..];
    let split = want.min(rest.len());
    let (base, patch) = rest.split_at(split);
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let r = apply(base, patch);
    let peak = (ALLOC.peak() - before) as u64;
    match r {
        Ok(out) => {
            assert!(
                peak <= out.len() as u64,
                "peak {peak} over the output {}",
                out.len()
            );
            assert!(out.len() <= patch.len());
            let h = parse(patch, base.len() as u64).expect("a patch that applied parses");
            assert_eq!(h.out_len, out.len() as u64);
        }
        Err(_) => assert_eq!(peak, 0, "an error after an allocation of {peak} bytes"),
    }
});
