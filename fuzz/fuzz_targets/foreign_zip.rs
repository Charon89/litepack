#![no_main]
//! E4-3: arbitrary bytes as a ZIP archive through `lpk-foreign`'s reader (the worker's
//! parser): open, every listing page, every file member streamed. No panic; errors are fine.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lpk_format_fuzz::foreign(data, "zip");
});
