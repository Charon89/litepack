#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::Index;

fuzz_target!(|data: &[u8]| {
    // The first two bytes pick a plausible index offset; the rest is the payload.
    if data.len() < 2 {
        return;
    }
    let offset = 64 + u64::from(u16::from_le_bytes([data[0], data[1]])) * 16;
    let _ = Index::parse(&data[2..], offset);
});
