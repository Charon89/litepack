#![no_main]
//! E4-3: arbitrary bytes as a tar or compressed-stream archive through `lpk-foreign`'s reader (the worker's
//! parser): open, every listing page, every file member streamed. No panic; errors are fine.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The first byte picks the stream: plain tar or one of the compressed ones.
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let formats = ["tar", "gz", "bz2", "xz", "zst", "lz", "lzma"];
    lpk_format_fuzz::foreign(rest, formats[usize::from(pick) % formats.len()]);
});
