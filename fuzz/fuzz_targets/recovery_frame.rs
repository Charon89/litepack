#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::RecoveryFrame;

// `RecoveryFrame::parse` runs every field consistency rule of spec section 13.
fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let offset = 64 + u64::from(u16::from_le_bytes([data[0], data[1]])) * 16;
    let _ = RecoveryFrame::parse(&data[2..], offset);
});
