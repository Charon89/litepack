#![no_main]
//! E4-4: arbitrary bytes through the libarchive reader of `lpk-worker-libarchive`, as each of
//! its wire formats in turn (the first byte picks one): open, every listing page, every file
//! member streamed. No panic and no sanitizer report; errors are fine. Built against the
//! vendored libarchive; `fuzz/run.sh` and the fuzz workflow compile that C code (and zlib, liblzma
//! and libzstd) with clang and `-fsanitize=address,fuzzer-no-link`. With the sources absent it
//! exercises the RAR pre-check and the stub.
use libfuzzer_sys::fuzz_target;

static LOCALE: std::sync::Once = std::sync::Once::new();

fuzz_target!(|data: &[u8]| {
    // As the worker does at start (once): the production name path.
    LOCALE.call_once(|| {
        let _ = lpk_worker_libarchive::use_utf8_locale();
    });
    let formats = lpk_worker_libarchive::FORMATS;
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let format = formats[usize::from(pick) % formats.len()];
    lpk_format_fuzz::foreign_with(rest, format, &|f, args| {
        lpk_worker_libarchive::open(f, &[], args)
    });
});
