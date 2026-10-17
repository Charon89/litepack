//! The store path against the corpus, run by hand (release build, once):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test store_path_corpus -- --ignored --nocapture`
//!
//! `video` and `encrypted-random` with the Fast tier at one thread and at twenty-four: the
//! archives must be byte for byte the same; the wall seconds are printed (report only).
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::Instant;

use lpk_core::{FastOptions, Pipeline};

#[test]
#[ignore]
fn stored_classes_are_identical_at_one_thread_and_twenty_four() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    println!("| class | archive B | s (1 thread) | s (24 threads) | identical |");
    println!("|---|---|---|---|---|");
    for class in ["video", "encrypted-random"] {
        let dir = root.join(class);
        let mut runs = Vec::new();
        for threads in [1usize, 24] {
            let t0 = Instant::now();
            let mut out = Vec::new();
            let s = Pipeline::fast(FastOptions {
                threads: Some(threads),
                ..FastOptions::default()
            })
            .run(&dir, &mut out)
            .unwrap();
            runs.push((out, s, t0.elapsed().as_secs_f64()));
        }
        let same = runs[0].0 == runs[1].0 && runs[0].1.writer == runs[1].1.writer;
        println!(
            "| {class} | {} | {:.3} | {:.3} | {} |",
            runs[0].1.writer.archive_len,
            runs[0].2,
            runs[1].2,
            if same { "yes" } else { "NO" }
        );
        assert!(same, "{class}: the archives differ");
    }
}
