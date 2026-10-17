//! Parallel block encoding through the pipeline (E2-4b): the Fast and Balanced tiers write the
//! same bytes with one thread and eight, with peeled JPEGs and duplicates in the tree, and
//! their summaries agree; the memory budget sets the blocks in flight (E2-20).
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_core::{register_full_reader, BalancedOptions, FastOptions, Pipeline, RunSummary};
use lpk_format::{Archive, EntryKind, Resources};

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut out = Vec::new();
    for b in noise(seed, len) {
        out.extend_from_slice(WORDS[(b % 8) as usize].as_bytes());
        out.push(if b & 0x70 == 0 { b'\n' } else { b' ' });
        if out.len() >= len {
            break;
        }
    }
    out.truncate(len);
    out
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// Several blocks' worth of prose (two clusters: text and markdown), noise, duplicate files, a
/// file repeating its own block, JPEGs (two peel, one with trailing bytes, one a copy). Flat:
/// a directory entry carries a modification time the file system moves between two walks.
fn make_tree(root: &Path) {
    for i in 0..6u64 {
        std::fs::write(root.join(format!("t{i}.txt")), prose(i + 1, 700_000)).unwrap();
        std::fs::write(root.join(format!("m{i}.md")), prose(i + 11, 300_000)).unwrap();
    }
    let big = noise(7, 900_000);
    std::fs::write(root.join("r.bin"), &big).unwrap();
    std::fs::write(root.join("r-again.bin"), &big).unwrap();
    std::fs::write(root.join("t0-again.txt"), prose(1, 700_000)).unwrap();
    let mut twice = noise(8, 300_000);
    let again = twice.clone();
    twice.extend_from_slice(&again);
    std::fs::write(root.join("twice.bin"), twice).unwrap();
    std::fs::write(root.join("zero"), b"").unwrap();
    let jpeg = fixture("baseline.jpg");
    std::fs::write(root.join("p.jpg"), &jpeg).unwrap();
    std::fs::write(root.join("p-copy.jpg"), &jpeg).unwrap();
    let mut trailing = jpeg;
    trailing.extend_from_slice(b"some trailing bytes after the EOI");
    std::fs::write(root.join("p-trailing.jpg"), trailing).unwrap();
    std::fs::write(root.join("q.jpg"), fixture("progressive.jpg")).unwrap();
    // Deflate containers for the Balanced tier's Deflate peel (a ZIP, a multi-IDAT PNG, a gzip
    // member, and a gzip member that pays for its record).
    for name in ["doc.zip", "image.png", "notes.txt.gz"] {
        std::fs::write(root.join(name), fixture(&format!("deflate/{name}"))).unwrap();
    }
    let mut text = Vec::new();
    for _ in 0..6 {
        text.extend_from_slice(&prose(3, 40_000));
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut gz, &text).unwrap();
    std::fs::write(root.join("text.gz"), gz.finish().unwrap()).unwrap();
}

fn fast(threads: usize) -> Pipeline {
    Pipeline::fast(FastOptions {
        block_size: 1 << 20,
        threads: Some(threads),
        ..FastOptions::default()
    })
}

fn balanced(threads: usize) -> Pipeline {
    Pipeline::balanced(BalancedOptions {
        block_size: 1 << 20,
        dict_size: 1 << 20,
        sample_len: 1 << 16,
        min_block_before_boundary: Some(0),
        threads: Some(threads),
        ..BalancedOptions::default()
    })
}

fn run(p: Pipeline, root: &Path) -> (Vec<u8>, RunSummary) {
    let mut out = Vec::new();
    let s = p.run(root, &mut out).unwrap();
    (out, s)
}

/// Every file reads back as written.
fn check(bytes: Vec<u8>, root: &Path) {
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut v = Vec::new();
        a.extract(e, &mut v).unwrap();
        assert_eq!(v, std::fs::read(root.join(&e.path)).unwrap(), "{}", e.path);
    }
    a.verify().unwrap();
}

#[test]
fn the_fast_tier_writes_the_same_bytes_with_one_thread_and_eight() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let (one, s1) = run(fast(1), dir.path());
    let (eight, s8) = run(fast(8), dir.path());
    assert_eq!(one, eight);
    assert!(s1.writer.blocks >= 4, "{} blocks", s1.writer.blocks);
    assert_eq!(s1.writer, s8.writer);
    assert_eq!(s1.fast, s8.fast);
    assert_eq!(s1.peel, s8.peel);
    let f = s1.fast.unwrap();
    assert!(f.zstd_blocks > 0 && f.stored_by_class > 0, "{f:?}");
    assert!(f.peel.peeled.files >= 2, "{:?}", f.peel);
    assert!(s1.writer.deduped_chunks > 0);
    assert_eq!((s1.threads, s1.blocks_in_flight), (1, 1));
    assert_eq!((s8.threads, s8.blocks_in_flight), (8, 8));
    check(eight, dir.path());
}

#[test]
fn the_balanced_tier_writes_the_same_bytes_with_one_thread_and_eight_capped_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let (one, s1) = run(balanced(1), dir.path());
    let (eight, s8) = run(balanced(8), dir.path());
    assert_eq!(one, eight);
    assert!(s1.writer.blocks >= 4, "{} blocks", s1.writer.blocks);
    assert_eq!(s1.writer, s8.writer);
    assert_eq!(s1.balanced, s8.balanced);
    assert_eq!(s1.peel, s8.peel);
    let d = &s1.peel.deflate;
    assert!(
        d.streams.reconstructed >= 4 && d.outcomes.kept >= 1,
        "{d:?}"
    );
    let b = s1.balanced.unwrap();
    assert!(
        b.lzma_blocks + b.zstd_blocks > 0 && b.sample_bytes > 0,
        "{b:?}"
    );
    assert_eq!((s1.threads, s1.blocks_in_flight), (1, 1));
    // No cap of four any more: the budget allows eight blocks this small.
    assert_eq!((s8.threads, s8.blocks_in_flight), (8, 8));
    check(eight, dir.path());
}

#[test]
fn the_default_thread_count_is_the_machines_and_writes_the_same_bytes() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let (one, _) = run(fast(1), dir.path());
    let (auto, s) = run(
        Pipeline::fast(FastOptions {
            block_size: 1 << 20,
            ..FastOptions::default()
        }),
        dir.path(),
    );
    assert_eq!(one, auto);
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    assert_eq!(s.threads, cores);
}
