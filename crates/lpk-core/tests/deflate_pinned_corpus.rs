//! The Deflate peel's pinned archives on the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test deflate_pinned_corpus -- --ignored --nocapture`
//!
//! `office-pdf` and `photo-raw-png` through the Balanced pipeline (its Deflate peel on, the BWT
//! stage off, see `hash_of`) at one thread: the archives' BLAKE3 must equal the values pinned
//! from the serial peel (D-70, `main` 54b1163) before the peel pool (design
//! `docs/notes/e2-10-peel-pool-design.md`, task 1).
//! The archive holds every file's modification time, so the pins hold for the `small` corpus
//! of the machine that pinned them (host `Megatron`); set `LPK_PIN_PRINT=1` to print the hashes
//! without checking them on another corpus build.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{BalancedOptions, Pipeline};

/// The pinned BLAKE3 per class.
pub const PINNED: [(&str, &str); 2] = [
    (
        "office-pdf",
        "5c266d1461dffb9ff0468843e674affa9f3a78aef027b6d2b402bb3ff3cbc94c",
    ),
    (
        "photo-raw-png",
        "b9e0c17bb3b3dc422ce61b59b3d5464375d03653114893904b46dae4c4dce0d2",
    ),
];

fn hash_of(dir: &Path, threads: usize, skips: bool) -> (String, f64) {
    let t0 = Instant::now();
    let mut out = Vec::new();
    // The pins are the Deflate peel's one-thread archives without the BWT stage: D-81 (3)
    // makes the header say minor 3 whenever the stage is on (the default since then), which
    // would move every hash by that one header byte.
    let mut p = Pipeline::balanced(BalancedOptions {
        threads: Some(threads),
        bwt: false,
        ..BalancedOptions::default()
    });
    if let Some(d) = p.deflate.as_mut() {
        d.exact_skips = skips;
        d.hints = skips;
    }
    p.run(dir, &mut out).unwrap();
    (
        blake3::hash(&out).to_hex().to_string(),
        t0.elapsed().as_secs_f64(),
    )
}

#[test]
#[ignore]
fn the_one_thread_balanced_archives_match_the_pinned_hashes() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let print_only = std::env::var_os("LPK_PIN_PRINT").is_some();
    let mut differ = Vec::new();
    // The exact skips and the hints on (the default) and off (design
    // `docs/notes/e2-10-peel-pool-design.md`, task 3).
    for skips in [true, false] {
        for (name, want) in PINNED {
            let (got, s) = hash_of(&root.join(name), 1, skips);
            println!("{name}: {got} ({s:.1} s, 1 thread, skips and hints {skips})");
            if got != want {
                differ.push((name, skips));
            }
        }
    }
    assert!(
        print_only || differ.is_empty(),
        "differ from the pins: {differ:?}"
    );
}
