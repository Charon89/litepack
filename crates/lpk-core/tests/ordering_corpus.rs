//! File ordering against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test ordering_corpus -- --ignored --nocapture`
//!
//! Archives four classes with the Balanced tier in path order and in extension order, extracts
//! each archive and compares it to the source, and prints the archive sizes (report only; the
//! runner's rows are the acceptance evidence). Asserts only the round trips.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_core::{register_full_reader, BalancedOptions, Dedup, FileOrder, FoldOptions, Pipeline};
use lpk_format::{Archive, EntryKind, Resources};

const CLASSES: [&str; 4] = [
    "software-installed",
    "game-assets",
    "small-files",
    "source-git",
];

fn run(dir: &Path, ordering: FileOrder) -> Vec<u8> {
    let mut p = Pipeline::balanced(BalancedOptions::default());
    p.fold = Some(Box::new(Dedup::new(FoldOptions {
        ordering,
        ..FoldOptions::default()
    })));
    let mut out = Vec::new();
    p.run(dir, &mut out).unwrap();
    out
}

fn round_trip(bytes: &[u8], dir: &Path) -> u64 {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let mut files = 0;
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut got = Vec::new();
        a.extract(e, &mut got).unwrap();
        assert!(
            got == std::fs::read(dir.join(&e.path)).unwrap(),
            "{}",
            e.path
        );
        files += 1;
    }
    a.verify().unwrap();
    files
}

#[test]
#[ignore]
fn orders_on_four_classes() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    println!("| class | files | none (path) B | extension B |");
    println!("|---|---|---|---|");
    for class in CLASSES {
        let dir = root.join(class);
        let none = run(&dir, FileOrder::None);
        let ext = run(&dir, FileOrder::Extension);
        let files = round_trip(&none, &dir);
        round_trip(&ext, &dir);
        println!("| {class} | {files} | {} | {} |", none.len(), ext.len());
    }
}
