#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut r = data;
    let _ = lpk_format::varint::read(&mut r);
});
