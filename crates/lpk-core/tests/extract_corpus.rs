//! The block-ordered extraction against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test extract_corpus -- --ignored --nocapture`
//!
//! For every class: the Fast tier writes an archive file, the new extraction (default pool)
//! writes a tree whose every file is checked by BLAKE3 against the corpus, and the old
//! path-order extraction (the format tool's `extract`, the one `lpk x` used before) writes a
//! second tree. Prints the wall seconds and MB/s of both, the plan's counts and the pool size;
//! the figures are a report only (the runner's G4 row is the measurement).
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{archive_fast_file, extract_file, DefaultPolicy, ExtractOptions, FastOptions};
use lpk_format::{Archive, EntryKind, Resources};

fn hash_file(p: &Path) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    std::io::copy(&mut BufReader::new(File::open(p).unwrap()), &mut h).unwrap();
    h.finalize()
}

fn join(dir: &Path, rel: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for c in rel.split('/') {
        p.push(c);
    }
    p
}

#[test]
#[ignore]
fn extract_every_class_block_ordered() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut classes: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    classes.sort();
    assert!(!classes.is_empty());
    println!("| class | files | MB | blocks | workers+writers | nested | old s | new s | new MB/s | old/new |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for dir in classes {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let arch = work.path().join("a.lpk");
        archive_fast_file(&dir, &arch, FastOptions::default()).unwrap();

        let new = work.path().join("new");
        let t0 = Instant::now();
        let s = extract_file(&arch, &new, &[], &DefaultPolicy, &ExtractOptions::default()).unwrap();
        let new_secs = t0.elapsed().as_secs_f64();

        let old = work.path().join("old");
        let t0 = Instant::now();
        let mut sink = Vec::new();
        let code = lpk_format::cli::run_with(
            [
                std::ffi::OsString::from("lpk-decode"),
                "extract".into(),
                arch.clone().into(),
                old.clone().into(),
            ],
            &mut sink,
            &mut std::io::stderr(),
            lpk_core::register_full_reader::<File>,
        );
        let old_secs = t0.elapsed().as_secs_f64();
        assert_eq!(code, 0, "{name}: old extraction failed");

        // Every file of the new tree against the corpus.
        let mut a = Archive::open(File::open(&arch).unwrap(), &Resources::default()).unwrap();
        let t = a.entry_table().unwrap();
        let mut files = 0u64;
        for e in t.table().unwrap().iter() {
            let e = e.unwrap();
            if e.kind == EntryKind::File {
                assert_eq!(
                    hash_file(&join(&new, &e.path)),
                    hash_file(&join(&dir, &e.path)),
                    "{name}/{}",
                    e.path
                );
                files += 1;
            }
        }
        assert_eq!(files, s.files, "{name}");
        assert_eq!(s.blocks_decoded, s.blocks_needed, "{name}");
        let mb = s.bytes as f64 / 1e6;
        println!(
            "| {name} | {files} | {mb:.1} | {} | {}+{} | {} | {old_secs:.2} | {new_secs:.2} | {:.0} | {:.1} |",
            s.blocks,
            s.workers,
            s.writers,
            s.nested_decodes,
            mb / new_secs.max(1e-9),
            old_secs / new_secs.max(1e-9)
        );
    }
}
