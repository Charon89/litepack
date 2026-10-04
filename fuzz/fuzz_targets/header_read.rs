#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::Header;

fuzz_target!(|data: &[u8]| {
    let mut r = data;
    let _ = Header::read(&mut r);
});
