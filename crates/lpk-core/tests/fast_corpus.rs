//! Fast tier against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test fast_corpus -- --ignored --nocapture`
//!
//! Prints one row per class: raw bytes, the store-path archive bytes, the Fast archive bytes,
//! the Fast blocks, blocks stored by the gate / by class / for no gain, and the wall seconds of
//! the Fast run. No dictionary is used (the default policy).
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{archive_fast, archive_store, FastOptions, FastSummary, StoreOptions};
use lpk_format::{Archive, EntryKind, Resources};

fn walk_names(dir: &Path) -> Vec<String> {
    let mut expected: Vec<String> = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, prefix)) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let n = e.file_name().into_string().unwrap();
            let rel = if prefix.is_empty() {
                n
            } else {
                format!("{prefix}/{n}")
            };
            if e.file_type().unwrap().is_dir() {
                stack.push((e.path(), rel.clone()));
            }
            expected.push(rel);
        }
    }
    expected.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    expected
}

struct Fast {
    raw: u64,
    archive: u64,
    blocks: u64,
    summary: FastSummary,
    secs: f64,
}

/// Pack `dir` with the Fast tier, check every file and the entry list.
fn run_fast(dir: &Path, name: &str) -> Fast {
    let t0 = Instant::now();
    let mut bytes: Vec<u8> = Vec::new();
    let (s, fs) = archive_fast(dir, &mut bytes, FastOptions::default()).unwrap();
    let secs = t0.elapsed().as_secs_f64();
    let mut a = Archive::open(Cursor::new(&bytes[..]), &Resources::default()).unwrap();
    lpk_core::register_full_reader(&mut a);
    let table = a.entry_table().unwrap();
    let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let got: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        got,
        walk_names(dir),
        "{name}: entry list differs from read_dir"
    );
    let mut raw = 0u64;
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut out = Vec::new();
        a.extract(e, &mut out).unwrap();
        let mut f = BufReader::new(File::open(dir.join(&e.path)).unwrap());
        let mut h = blake3::Hasher::new();
        std::io::copy(&mut f, &mut h).unwrap();
        assert_eq!(blake3::hash(&out), h.finalize(), "{name}/{}", e.path);
        raw += out.len() as u64;
    }
    a.verify().unwrap();
    Fast {
        raw,
        archive: s.archive_len,
        blocks: a.index().blocks.len() as u64,
        summary: fs,
        secs,
    }
}

#[test]
#[ignore]
fn fast_round_trip_every_class() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut classes: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    classes.sort();
    assert!(!classes.is_empty());
    println!("| class | raw B | store-path B | fast B | blocks | gate/class/no-gain | s (fast) |");
    println!("|---|---|---|---|---|---|---|");
    for dir in classes {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let f = run_fast(&dir, &name);
        let mut sink = Vec::new();
        let store = archive_store(&dir, &mut sink, StoreOptions::default())
            .unwrap()
            .archive_len;
        let s = f.summary;
        println!(
            "| {name} | {} | {store} | {} | {} | {}/{}/{} | {:.2} |",
            f.raw,
            f.archive,
            f.blocks,
            s.stored_by_gate,
            s.stored_by_class,
            s.stored_no_gain,
            f.secs
        );
        // The Fast archive is at most the store-path archive plus 64 bytes per block (the zstd
        // step's header is longer than store's).
        assert!(
            f.archive <= store + 64 * f.blocks,
            "{name}: {} > {store} + 64 * {}",
            f.archive,
            f.blocks
        );
        if matches!(name.as_str(), "text-prose" | "source-git" | "logs-text") {
            assert!(f.archive < f.raw, "{name}: not smaller than raw");
        }
    }
}
