//! The store path against the corpus, run by hand (release build, once):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test store_path_corpus -- --ignored --nocapture`
//!
//! `video` and `encrypted-random` with the Fast tier at one thread and at twenty-four: the
//! archives must be byte for byte the same; the wall seconds are printed (report only). Each
//! is written twice: to memory (the reader thread, no write thread) and to a file with
//! `run_file` (the reader thread and the write thread); all four must agree. The archive's
//! BLAKE3 is printed, to compare with an earlier build's output.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::Instant;

use lpk_core::{FastOptions, Pipeline};

#[test]
#[ignore]
fn stored_classes_are_identical_at_one_thread_and_twenty_four() {
    let root = PathBuf::from(
        std::env::var_os("LPK_CORPUS").expect("set LPK_CORPUS to the bench/corpus/small path"),
    );
    println!("| class | archive B | s (1 thread) | s (24 threads) | identical |");
    println!("|---|---|---|---|---|");
    for class in ["video", "encrypted-random"] {
        let dir = root.join(class);
        let mut runs = Vec::new();
        let tmp = tempfile::tempdir().unwrap();
        for threads in [1usize, 24] {
            let fast = || {
                Pipeline::fast(FastOptions {
                    threads: Some(threads),
                    ..FastOptions::default()
                })
            };
            let t0 = Instant::now();
            let file = tmp.path().join(format!("t{threads}.lpk"));
            let s = fast().run_file(&dir, &file).unwrap();
            let secs = t0.elapsed().as_secs_f64();
            let out = std::fs::read(&file).unwrap();
            let mut mem = Vec::new();
            let m = fast().run(&dir, &mut mem).unwrap();
            assert!(
                mem == out && m.writer == s.writer,
                "{class}: memory and file differ"
            );
            runs.push((out, s, secs));
        }
        let same = runs[0].0 == runs[1].0 && runs[0].1.writer == runs[1].1.writer;
        println!("{class}: blake3 {}", blake3::hash(&runs[0].0).to_hex());
        // Round 1's archives (the sequential writer before the pipeline) of the locked
        // `small` corpus, byte for byte, except the index envelope's `threads_hint`: D-67 (3)
        // made store-only archives carry it (0 became 1), which moves the three hashes that
        // follow from it (the index frame's, the trailer's `index_hash`, the trailer frame's).
        // Every data byte and the length are round 1's.
        let want = match class {
            "video" => "5e9e959c6d23687c8a52918d56dd3bd6b6680cd1d46ce3c8baf5647f64f1ab6f",
            _ => "e642c44583d6d1df415d03390a43a3f258f20667a1f49b7ea1e64dec505049c3",
        };
        assert_eq!(blake3::hash(&runs[0].0).to_hex().as_str(), want, "{class}");
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
