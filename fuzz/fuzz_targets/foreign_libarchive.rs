#![no_main]
//! E4-4: arbitrary bytes through the libarchive reader of `lpk-worker-libarchive`, as each of
//! its wire formats in turn (the first byte picks one): open, every listing page, every file
//! member streamed. No panic and no sanitizer report; errors are fine. Built against the
//! vendored libarchive (E4-6 builds it with `-fsanitize=fuzzer-no-link,address`); with the
//! sources absent it exercises the RAR pre-check and the stub.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let formats = lpk_worker_libarchive::FORMATS;
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let format = formats[usize::from(pick) % formats.len()];
    lpk_format_fuzz::foreign_with(rest, format, &|f, args| {
        lpk_worker_libarchive::open(f, &[], args)
    });
});
