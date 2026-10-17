//! Parallel block encoding against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test parallel_corpus -- --ignored --nocapture`
//!
//! Every class with the Fast tier at one thread and at twenty-four: the archives must be byte
//! for byte the same; the wall seconds of both runs are printed (report only, not a gate).
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{FastOptions, Pipeline, RunSummary};

fn run(dir: &Path, threads: usize) -> (Vec<u8>, RunSummary, f64) {
    let t0 = Instant::now();
    let mut out = Vec::new();
    let s = Pipeline::fast(FastOptions {
        threads: Some(threads),
        ..FastOptions::default()
    })
    .run(dir, &mut out)
    .unwrap();
    (out, s, t0.elapsed().as_secs_f64())
}

#[test]
#[ignore]
fn fast_archives_are_identical_at_one_thread_and_twenty_four() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut classes: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    classes.sort();
    assert!(!classes.is_empty());
    println!("| class | archive B | blocks | s (1 thread) | s (24 threads) | identical |");
    println!("|---|---|---|---|---|---|");
    let mut failed = Vec::new();
    for dir in classes {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let (one, s1, t1) = run(&dir, 1);
        let (many, s24, t24) = run(&dir, 24);
        let same = one == many && s1.fast == s24.fast && s1.writer == s24.writer;
        assert_eq!((s1.threads, s1.blocks_in_flight), (1, 1));
        assert_eq!((s24.threads, s24.blocks_in_flight), (24, 24));
        println!(
            "| {name} | {} | {} | {t1:.2} | {t24:.2} | {} |",
            s1.writer.archive_len,
            s1.writer.blocks,
            if same { "yes" } else { "NO" }
        );
        if !same {
            failed.push(name);
        }
    }
    assert!(
        failed.is_empty(),
        "classes whose archives differ: {failed:?}"
    );
}

fn run_balanced(dir: &Path, threads: usize, memory: Option<u64>) -> (Vec<u8>, RunSummary, f64) {
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut p = Pipeline::balanced(lpk_core::BalancedOptions {
        threads: Some(threads),
        ..lpk_core::BalancedOptions::default()
    });
    p.seal.memory = memory;
    let s = p.run(dir, &mut out).unwrap();
    (out, s, t0.elapsed().as_secs_f64())
}

/// Design `docs/notes/e2-10-11-design.md` section 7: `office-pdf` and `photo-raw-png` through the
/// default Balanced pipeline (its Deflate peel on) at one thread and at twenty-four, the latter
/// both at the default budget and at a budget wide enough for twenty-four blocks in flight: the
/// archives must be byte for byte the same.
#[test]
#[ignore]
fn balanced_archives_with_the_deflate_peel_are_identical_at_one_thread_and_twenty_four() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    println!("| class | archive B | blocks | kept streams | in flight (24, default) | s (1) | s (24, default) | s (24, wide) | identical |");
    println!("|---|---|---|---|---|---|---|---|---|");
    let mut failed = Vec::new();
    for name in ["office-pdf", "photo-raw-png"] {
        let dir = root.join(name);
        let (one, s1, t1) = run_balanced(&dir, 1, None);
        let (def, sd, td) = run_balanced(&dir, 24, None);
        let (wide, sw, tw) = run_balanced(&dir, 24, Some(1 << 40));
        assert_eq!(sw.blocks_in_flight, 24);
        let same = one == def
            && one == wide
            && s1.peel == sd.peel
            && s1.peel == sw.peel
            && s1.writer == sw.writer;
        println!(
            "| {name} | {} | {} | {} | {} | {t1:.1} | {td:.1} | {tw:.1} | {} |",
            s1.writer.archive_len,
            s1.writer.blocks,
            s1.peel.deflate.outcomes.kept,
            sd.blocks_in_flight,
            if same { "yes" } else { "NO" }
        );
        assert!(
            s1.peel.deflate.outcomes.kept > 0,
            "{name}: the peel kept nothing"
        );
        if !same {
            failed.push(name);
        }
    }
    assert!(
        failed.is_empty(),
        "classes whose archives differ: {failed:?}"
    );
}
