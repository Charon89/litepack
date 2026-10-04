#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::Trailer;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let len = data.len() as u64;
    let _ = Trailer::read_tail(&mut Cursor::new(data), len);
    // A length that disagrees with the reader.
    let _ = Trailer::read_tail(&mut Cursor::new(data), len.saturating_add(7));
    let _ = Trailer::read_tail(&mut Cursor::new(data), len / 2);
});
