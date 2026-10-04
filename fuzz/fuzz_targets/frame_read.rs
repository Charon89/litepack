#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::{Frame, ReadLimits};

fuzz_target!(|data: &[u8]| {
    let mut r = data;
    let _ = Frame::read(&mut r, &ReadLimits::default());
});
