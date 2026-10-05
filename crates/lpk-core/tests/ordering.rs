//! File ordering inside clusters (E2-8) through the Fast and Balanced pipelines: both orders
//! round-trip to the same tree, and the orders lay the files out differently.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use lpk_core::{
    register_full_reader, BalancedOptions, Dedup, FastOptions, FileOrder, FoldOptions, Pipeline,
};
use lpk_format::{Archive, EntryKind, Resources};

/// Letters and spaces from a xorshift stream: text with no repeated stretches.
fn letters(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let b = (x >> 20) as u8 % 28;
        v.push(if b >= 26 { b' ' } else { b'a' + b });
    }
    v
}

/// Text files whose path order (a/z.txt, b/a.md, c/m.txt) differs from the extension order
/// (b/a.md first, then the .txt files by name).
fn make_tree(root: &Path) {
    for (dir, name, seed) in [("a", "z.txt", 1), ("b", "a.md", 2), ("c", "m.TXT", 3)] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join(name), letters(seed, 300_000)).unwrap();
    }
}

fn pipeline(balanced: bool, ordering: FileOrder) -> Pipeline {
    let mut p = if balanced {
        Pipeline::balanced(BalancedOptions::default())
    } else {
        Pipeline::fast(FastOptions::default())
    };
    p.fold = Some(Box::new(Dedup::new(FoldOptions {
        ordering,
        ..FoldOptions::default()
    })));
    p
}

/// Round-trips `bytes` against `dir` and returns the first chunk index of each file.
fn first_chunks(bytes: &[u8], dir: &Path) -> BTreeMap<String, u64> {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let mut out = BTreeMap::new();
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut got = Vec::new();
        a.extract(e, &mut got).unwrap();
        assert!(
            got == std::fs::read(dir.join(&e.path)).unwrap(),
            "{}",
            e.path
        );
        out.insert(e.path.clone(), e.chunks[0]);
    }
    a.verify().unwrap();
    out
}

fn run(balanced: bool, ordering: FileOrder, dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    pipeline(balanced, ordering).run(dir, &mut out).unwrap();
    out
}

#[test]
fn both_orders_round_trip_and_the_layouts_differ() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    for balanced in [false, true] {
        let none = first_chunks(&run(balanced, FileOrder::None, dir.path()), dir.path());
        let ext = first_chunks(&run(balanced, FileOrder::Extension, dir.path()), dir.path());
        // Path order: a/z.txt, b/a.md, c/m.TXT.
        assert!(none["a/z.txt"] < none["b/a.md"] && none["b/a.md"] < none["c/m.TXT"]);
        // Extension order: a.md, then m.TXT, then z.txt.
        assert!(ext["b/a.md"] < ext["c/m.TXT"] && ext["c/m.TXT"] < ext["a/z.txt"]);
        assert_ne!(none, ext);
    }
}

#[test]
fn the_default_fold_keeps_path_order() {
    assert_eq!(FoldOptions::default().ordering, FileOrder::None);
}
