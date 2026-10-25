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

/// Three versions of a two-file tree (v1, v2, v10: natural order) beside an unrelated folder.
fn make_versions(root: &Path) {
    for (v, seed) in [("v1", 10), ("v2", 20), ("v10", 30)] {
        std::fs::create_dir_all(root.join(v).join("lib")).unwrap();
        std::fs::write(root.join(v).join("README.txt"), letters(seed, 100_000)).unwrap();
        std::fs::write(
            root.join(v).join("lib").join("x.txt"),
            letters(seed + 1, 100_000),
        )
        .unwrap();
    }
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs").join("notes.txt"), letters(99, 100_000)).unwrap();
}

#[test]
fn the_versions_order_puts_a_files_versions_together() {
    let dir = tempfile::tempdir().unwrap();
    make_versions(dir.path());
    for balanced in [false, true] {
        let v = first_chunks(&run(balanced, FileOrder::Versions, dir.path()), dir.path());
        // (relative path, rank): README.txt of v1, v2, v10, then docs/notes.txt, then
        // lib/x.txt of v1, v2, v10.
        let order = [
            "v1/README.txt",
            "v2/README.txt",
            "v10/README.txt",
            "docs/notes.txt",
            "v1/lib/x.txt",
            "v2/lib/x.txt",
            "v10/lib/x.txt",
        ];
        for w in order.windows(2) {
            assert!(v[w[0]] < v[w[1]], "{} before {}", w[0], w[1]);
        }
    }
}

/// Runs the Balanced pipeline over the contents of `dir` minus `exclude`.
fn run_excluding(ordering: FileOrder, dir: &Path, exclude: &[&str]) -> Vec<u8> {
    let sources = lpk_core::Sources {
        roots: vec![lpk_core::Root::contents(dir)],
        exclude: if exclude.is_empty() {
            lpk_core::Selection::all()
        } else {
            lpk_core::Selection::new(exclude).unwrap()
        },
    };
    let mut out = Vec::new();
    pipeline(true, ordering)
        .run_sources(&sources, &mut out)
        .unwrap();
    out
}

#[test]
fn the_sibling_threshold_counts_the_files_left_after_exclude() {
    // a has six files; b shares two of them and has a third of its own plus two images.
    // Counting the images, b's five files share two with a (under half): no group. With
    // `--exclude **/*.iso` b has three files and shares two (at least half): a group.
    let dir = tempfile::tempdir().unwrap();
    for i in 1..=6 {
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(
            dir.path().join("a").join(format!("{i}.txt")),
            letters(i, 20_000),
        )
        .unwrap();
    }
    std::fs::create_dir_all(dir.path().join("b")).unwrap();
    for (name, seed) in [("1.txt", 41), ("2.txt", 42), ("z.txt", 43)] {
        std::fs::write(dir.path().join("b").join(name), letters(seed, 20_000)).unwrap();
    }
    for name in ["big1.iso", "big2.iso"] {
        std::fs::write(dir.path().join("b").join(name), letters(7, 5_000)).unwrap();
    }
    let _settle = run_excluding(FileOrder::None, dir.path(), &[]);
    // Without the exclusion: no group, the default bytes.
    assert!(
        run_excluding(FileOrder::Versions, dir.path(), &[])
            == run_excluding(FileOrder::None, dir.path(), &[])
    );
    // With it: a/1.txt, b/1.txt, a/2.txt, b/2.txt, ... (relative path, rank).
    let bytes = run_excluding(FileOrder::Versions, dir.path(), &["**/*.iso"]);
    let none = run_excluding(FileOrder::None, dir.path(), &["**/*.iso"]);
    assert!(bytes != none);
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let t = a.entry_table().unwrap();
    let first: BTreeMap<String, u64> = t
        .table()
        .unwrap()
        .iter()
        .map(|e| e.unwrap())
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| (e.path.clone(), e.chunks[0]))
        .collect();
    assert!(!first.contains_key("b/big1.iso"));
    assert!(first["a/1.txt"] < first["b/1.txt"]);
    assert!(first["b/1.txt"] < first["a/2.txt"]);
    assert!(first["a/2.txt"] < first["b/2.txt"]);
}

#[test]
fn the_versions_order_without_sibling_roots_is_the_default_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    for balanced in [false, true] {
        // A first walk right after the tree is written can see different directory times (the
        // entry table records them) than the next: compare two runs after a settling one.
        let _settle = run(balanced, FileOrder::None, dir.path());
        let (a, b) = (
            run(balanced, FileOrder::Versions, dir.path()),
            run(balanced, FileOrder::None, dir.path()),
        );
        assert!(a == b, "balanced {balanced}: the archives differ");
    }
}
