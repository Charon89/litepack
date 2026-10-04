//! Fast tier against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test fast_corpus -- --ignored --nocapture`
//!
//! Prints one row per class: raw bytes, Fast archive bytes with the bundled dictionaries and
//! without, blocks stored by the gate and by class, and wall seconds of each run.
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{archive_fast, BundledPriors, DictionaryPolicy, FastOptions, FastSummary};
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

/// Pack `dir`, check every file and the entry list; returns (raw bytes, archive bytes, summary,
/// seconds).
fn run(dir: &Path, name: &str, policy: DictionaryPolicy) -> (u64, u64, FastSummary, f64) {
    let t0 = Instant::now();
    let mut bytes: Vec<u8> = Vec::new();
    let options = FastOptions {
        dictionaries: policy,
        ..FastOptions::default()
    };
    let (s, fs) = archive_fast(dir, &mut bytes, options).unwrap();
    let secs = t0.elapsed().as_secs_f64();
    let mut a = Archive::open(Cursor::new(&bytes[..]), &Resources::default()).unwrap();
    a.set_priors(Box::new(BundledPriors::new()));
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
    (raw, s.archive_len, fs, secs)
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
    println!(
        "| class | raw B | fast+dict B | fast B (no dict) | gate/class blocks (dict) | s (dict) | s (no dict) |"
    );
    println!("|---|---|---|---|---|---|---|");
    for dir in classes {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let (raw, with, fs, t_with) = run(&dir, &name, DictionaryPolicy::Bundled);
        let (raw2, without, _, t_without) = run(&dir, &name, DictionaryPolicy::None);
        assert_eq!(raw, raw2);
        println!(
            "| {name} | {raw} | {with} | {without} | {}/{} | {t_with:.2} | {t_without:.2} |",
            fs.stored_by_gate, fs.stored_by_class
        );
        // Incompressible data is stored, so the archive exceeds the raw bytes only by the
        // container's own overhead (frame headers, hashes, chunk table, entry table, index):
        // allow 0.1 % plus 64 KiB.
        assert!(
            with <= raw + raw / 1000 + 65536,
            "{name}: {with} exceeds {raw} by more than the container overhead"
        );
        if matches!(name.as_str(), "text-prose" | "source-git" | "logs-text") {
            assert!(with < raw, "{name}: not smaller");
        }
    }
}
