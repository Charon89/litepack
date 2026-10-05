//! File ordering inside clusters (E2-8) through the Fast and Balanced pipelines: every order
//! round-trips to the same tree, and the orders lay the files out differently.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use lpk_core::{
    register_full_reader, BalancedOptions, Dedup, FastOptions, FoldOptions, Ordering, Pipeline,
    RunSummary,
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

/// `a.txt` is the base, `b.txt` is unrelated, `c.txt` is `a.txt` with a changed start and a
/// longer tail: in extension order `b` sits between the pair.
fn make_tree(root: &Path) {
    let a = letters(1, 1_500_000);
    let mut c = a.clone();
    c[..64].copy_from_slice(&letters(9, 64));
    c.extend_from_slice(&letters(10, 200_000));
    std::fs::write(root.join("a.txt"), &a).unwrap();
    std::fs::write(root.join("b.txt"), letters(2, 1_500_000)).unwrap();
    std::fs::write(root.join("c.txt"), &c).unwrap();
    std::fs::write(root.join("d.txt"), letters(3, 2_000)).unwrap();
}

fn pipeline(balanced: bool, ordering: Ordering) -> Pipeline {
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

fn run(balanced: bool, ordering: Ordering, dir: &Path) -> (Vec<u8>, RunSummary) {
    let mut out = Vec::new();
    let s = pipeline(balanced, ordering).run(dir, &mut out).unwrap();
    (out, s)
}

#[test]
fn every_order_round_trips_and_the_layouts_differ() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    for balanced in [false, true] {
        let (sim, ss) = run(balanced, Ordering::Similarity, dir.path());
        let (ext, se) = run(balanced, Ordering::Extension, dir.path());
        let (none, sn) = run(balanced, Ordering::None, dir.path());
        let (fs, fe, fnn) = (
            first_chunks(&sim, dir.path()),
            first_chunks(&ext, dir.path()),
            first_chunks(&none, dir.path()),
        );
        // Similarity: the larger of the pair first, its partner right after, then the stranger.
        assert_eq!(ss.ordering.groups, 1, "balanced {balanced}");
        assert_eq!(ss.ordering.files_sketched, 3);
        assert!(ss.ordering.bytes_read >= 4_000_000);
        assert!(
            fs["c.txt"] < fs["a.txt"] && fs["a.txt"] < fs["b.txt"],
            "{fs:?}"
        );
        // Extension and path order agree here: a, b, c.
        assert!(
            fe["a.txt"] < fe["b.txt"] && fe["b.txt"] < fe["c.txt"],
            "{fe:?}"
        );
        assert_eq!(fe, fnn);
        assert_ne!(fs, fe);
        // Only similarity reads files again.
        assert_eq!(se.ordering.bytes_read, 0);
        assert_eq!(sn.ordering, Default::default());
        // Dedup is by content: the same bytes were found redundant under every order.
        assert!(ss.writer.deduped_bytes > 1_000_000);
        assert!(se.writer.deduped_bytes > 1_000_000);
    }
}

#[test]
fn the_default_fold_orders_by_similarity() {
    assert_eq!(FoldOptions::default().ordering, Ordering::Similarity);
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let mut out = Vec::new();
    let s = Pipeline::fast(FastOptions::default())
        .run(dir.path(), &mut out)
        .unwrap();
    assert_eq!(s.ordering.groups, 1);
}
