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
        // With files the JPEG peel examines, the peel pool takes the threads the blocks leave
        // (at most half go to the blocks: the width ruling, design
        // `docs/notes/e2-10-peel-pool-design.md` section 4).
        assert_eq!(s24.threads, 24);
        if s24.peel_pool.workers == 0 {
            // The blocks the default budget allows (D-67), at most the threads.
            assert!(s24.blocks_in_flight <= 24, "{name}");
            assert_eq!(s24.blocks_in_flight, s24.memory.blocks_in_flight, "{name}");
        } else {
            assert_eq!(s24.blocks_in_flight + s24.peel_pool.workers, 24, "{name}");
            assert!(s24.blocks_in_flight <= 11, "{name}");
        }
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

/// Design `docs/notes/e2-10-peel-pool-design.md` task 4: both JPEG classes through the Fast
/// and the Balanced pipelines (their JPEG files through the peel pool) at one thread and at
/// twenty-four, at the smallest budget accepted, the default and a wide one: the archives must
/// be byte for byte the same.
#[test]
#[ignore]
fn jpeg_classes_are_identical_at_one_thread_and_twenty_four_across_budgets() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    println!("| class | tier | archive B | peeled | pool workers (24, default) | s (1) | s (24, min) | s (24, default) | s (24, wide) | identical |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    let mut failed = Vec::new();
    for name in ["photo-jpeg", "photo-jpeg-edited"] {
        let dir = root.join(name);
        for tier in ["fast", "balanced"] {
            let go = |threads: usize, memory: Option<u64>| {
                if tier == "fast" {
                    let t0 = Instant::now();
                    let mut out = Vec::new();
                    let mut p = Pipeline::fast(FastOptions {
                        threads: Some(threads),
                        ..FastOptions::default()
                    });
                    p.seal.memory = memory;
                    let s = p.run(&dir, &mut out).unwrap();
                    (out, s, t0.elapsed().as_secs_f64())
                } else {
                    run_balanced(&dir, threads, memory)
                }
            };
            let (one, s1, t1) = go(1, None);
            let (min, sm, tm) = go(24, Some(s1.memory.minimum()));
            let (def, sd, td) = go(24, None);
            let (wide, sw, tw) = go(24, Some(1 << 40));
            let same = one == min
                && one == def
                && one == wide
                && s1.peel == sm.peel
                && s1.peel == sd.peel
                && s1.peel == sw.peel;
            println!(
                "| {name} | {tier} | {} | {} | {} | {t1:.1} | {tm:.1} | {td:.1} | {tw:.1} | {} |",
                s1.writer.archive_len,
                s1.peel.peeled.files,
                sd.peel_pool.workers,
                if same { "yes" } else { "NO" }
            );
            assert!(sd.peel_pool.workers >= 1, "{name} {tier}: no pool");
            if !same {
                failed.push((name, tier));
            }
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

/// The one-thread archives' BLAKE3 pinned from the serial peel (the same values as
/// `deflate_pinned_corpus.rs`; they hold for the `small` corpus of the machine that pinned them,
/// whose files' modification times the archives hold).
const PINNED: [(&str, &str); 2] = [
    (
        "office-pdf",
        "5c266d1461dffb9ff0468843e674affa9f3a78aef027b6d2b402bb3ff3cbc94c",
    ),
    (
        "photo-raw-png",
        "b9e0c17bb3b3dc422ce61b59b3d5464375d03653114893904b46dae4c4dce0d2",
    ),
];

/// Design `docs/notes/e2-10-11-design.md` section 7: `office-pdf` and `photo-raw-png` through the
/// default Balanced pipeline (its Deflate peel on) at one thread and at twenty-four, the latter
/// both at the default budget and at a budget wide enough for eleven blocks in flight and
/// the peel pool: the archives must be byte for byte the same and the one-thread archive must
/// match the pinned hash (the peel pool, `docs/notes/e2-10-peel-pool-design.md`, task 2).
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
        // The blocks get at most half the threads, the Deflate peel pool the rest (design
        // `docs/notes/e2-10-peel-pool-design.md`, section 2).
        assert_eq!((sw.blocks_in_flight, sw.peel_pool.workers), (11, 13));
        assert!(sd.peel_pool.workers >= 1);
        let hash = blake3::hash(&one).to_hex().to_string();
        let pinned = PINNED.iter().find(|(n, _)| *n == name).unwrap().1;
        println!(
            "{name}: one thread {hash} (pinned {pinned}); pool at 24 threads: {} workers, P {} bytes, {} worker waits, {} feeding waits, {} unused encodes",
            sd.peel_pool.workers,
            sd.peel_pool.pool_bytes,
            sd.peel_pool.worker_waits,
            sd.peel_pool.feeding_waits,
            sd.peel_pool.waste.encodes
        );
        // `LPK_PIN_PRINT` (as in `deflate_pinned_corpus.rs`): print the hash without checking
        // it, for another machine's corpus build (D-13).
        let print_only = std::env::var_os("LPK_PIN_PRINT").is_some();
        let same = (print_only || hash == pinned)
            && one == def
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
