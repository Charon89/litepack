//! The Dedup fold stage against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test fold_corpus -- --ignored --nocapture`
//!
//! Archives `backup-versions` with the Fast tier and dedup, round-trips it bit for bit, and
//! asserts that the bytes the writer deduplicated equal the Phase 0 dedup probe's figure for the
//! class (bytes minus unique chunk bytes, same chunker parameters: same cuts, same hashes), read
//! from the committed `bench/results/2026-10-03-megatron-3/probe-dedup.json`. Then prints, for
//! every class of the corpus, the archive bytes with and without dedup (report only).
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_core::{register_full_reader, FastOptions, Pipeline};
use lpk_format::{Archive, EntryKind, Resources};

const PROBE: &str = "bench/results/2026-10-03-megatron-3/probe-dedup.json";
const CLASS: &str = "backup-versions";

/// `(bytes, unique_chunk_bytes)` of `class` in the committed probe file.
fn probe_row(class: &str) -> (u64, u64) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(PROBE);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let row = v["data"]["classes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["class"] == class)
        .unwrap();
    (
        row["bytes"].as_u64().unwrap(),
        row["unique_chunk_bytes"].as_u64().unwrap(),
    )
}

fn no_dedup() -> Pipeline {
    let mut p = Pipeline::fast(FastOptions::default());
    p.fold = None;
    p
}

#[test]
#[ignore]
fn backup_versions_dedup_equals_the_probe_and_round_trips() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let dir = root.join(CLASS);
    let mut out = Vec::new();
    let s = Pipeline::fast(FastOptions::default())
        .run(&dir, &mut out)
        .unwrap();
    let (bytes, unique) = probe_row(CLASS);
    let probe_saved = bytes - unique;
    println!(
        "{CLASS}: writer deduplicated {} bytes in {} chunk references; probe saved by chunk dedup {probe_saved} bytes (class bytes {bytes}, unique chunk bytes {unique}); archive {} bytes",
        s.writer.deduped_bytes, s.writer.deduped_chunks, s.writer.archive_len
    );

    // Round trip, bit for bit, through the full reader.
    let mut a = Archive::open(Cursor::new(out), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let mut mismatches = 0u64;
    let mut files = 0u64;
    let mut total = 0u64;
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut got = Vec::new();
        a.extract(e, &mut got).unwrap();
        let want = std::fs::read(dir.join(&e.path)).unwrap();
        files += 1;
        total += want.len() as u64;
        if got != want {
            mismatches += 1;
        }
    }
    a.verify().unwrap();
    println!("round trip: {files} files, {total} bytes, {mismatches} mismatches");
    assert_eq!(mismatches, 0);
    assert_eq!(total, bytes, "the walk saw every byte the probe counted");
    assert_eq!(s.writer.deduped_bytes, probe_saved);
}

#[test]
#[ignore]
fn every_class_with_and_without_dedup() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut classes: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_dir())
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    classes.sort();
    println!("| class | raw B | archive B without dedup | archive B with dedup | deduplicated B | chunk references | probe saved B |");
    println!("|---|---|---|---|---|---|---|");
    for class in classes {
        let dir = root.join(&class);
        let off = no_dedup().run(&dir, std::io::sink()).unwrap();
        let on = Pipeline::fast(FastOptions::default())
            .run(&dir, std::io::sink())
            .unwrap();
        let (raw, probe) = std::panic::catch_unwind(|| probe_row(&class))
            .map(|(b, u)| (b.to_string(), (b - u).to_string()))
            .unwrap_or_default();
        println!(
            "| {class} | {raw} | {} | {} | {} | {} | {probe} |",
            off.writer.archive_len,
            on.writer.archive_len,
            on.writer.deduped_bytes,
            on.writer.deduped_chunks
        );
    }
}
