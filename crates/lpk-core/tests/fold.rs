//! The Dedup fold stage through the Fast and Balanced pipelines (E2-7): duplicate files are
//! stored once, identical JPEGs need no Lepton block and no record, round trips are bit-exact.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_core::{
    register_full_reader, ChunkerKind, Dedup, FastOptions, FoldOptions, Pipeline, RunSummary,
};
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

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

fn with_secondary() -> Vec<u8> {
    let base = fixture("baseline.jpg");
    let mut v = base[..2].to_vec();
    v.extend_from_slice(&[0xFF, 0xE2, 0x00, 0x0A]);
    v.extend_from_slice(b"MPF\0\0\0\0\0");
    v.extend_from_slice(&base[2..]);
    v.extend_from_slice(b"gap");
    v.extend_from_slice(&fixture("secondary.jpg"));
    v.extend_from_slice(b"tail bytes");
    v
}

fn run(p: Pipeline, dir: &Path) -> (Vec<u8>, RunSummary) {
    let mut out = Vec::new();
    let s = p.run(dir, &mut out).unwrap();
    (out, s)
}

/// Every file of the archive through the full reader, with `verify`; and `lpk-check` on the
/// archive's frames when it holds no revision 1.1 primitive.
fn check(bytes: &[u8], dir: &Path, expect_files: usize, with_check: bool) {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let mut n = 0;
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut got = Vec::new();
        a.extract(e, &mut got).unwrap();
        assert!(
            got == std::fs::read(dir.join(&e.path)).unwrap(),
            "{}",
            e.path
        );
        n += 1;
    }
    assert_eq!(n, expect_files);
    a.verify().unwrap();
    if with_check {
        let mut c = lpk_check::archive::Archive::open(
            bytes.to_vec(),
            lpk_check::archive::Options::default(),
        )
        .unwrap();
        c.verify().unwrap();
        for e in c.entries().unwrap() {
            if e.kind == lpk_check::entries::EntryKind::File {
                let want = std::fs::read(dir.join(&e.path)).unwrap();
                assert!(c.read_file(&e).unwrap() == want, "check: {}", e.path);
            }
        }
    }
}

fn duplicate_tree(dir: &Path) {
    let big = noise(1, 700_000);
    std::fs::write(dir.join("a.bin"), &big).unwrap();
    std::fs::write(dir.join("b.bin"), &big).unwrap();
    std::fs::write(dir.join("c.bin"), noise(2, 100_000)).unwrap();
    // A file that repeats a block of itself.
    let mut r = noise(3, 100_000);
    let again = r.clone();
    r.extend_from_slice(&again);
    std::fs::write(dir.join("r.bin"), r).unwrap();
}

#[test]
fn dedup_is_the_default_of_both_tiers_and_no_fold_turns_it_off() {
    let dir = tempfile::tempdir().unwrap();
    duplicate_tree(dir.path());
    let (on, s_on) = run(Pipeline::fast(FastOptions::default()), dir.path());
    let mut p = Pipeline::fast(FastOptions::default());
    p.fold = None;
    let (off, s_off) = run(p, dir.path());
    assert_eq!(s_off.writer.deduped_chunks, 0);
    // The copy is entirely deduplicated (every chunk of b.bin is one of a.bin's, or the reverse).
    assert!(s_on.writer.deduped_bytes >= 700_000, "{:?}", s_on.writer);
    assert!(on.len() + 600_000 < off.len(), "{} {}", on.len(), off.len());
    check(&on, dir.path(), 4, true);
    check(&off, dir.path(), 4, true);

    let (bal, sb) = run(
        Pipeline::balanced(lpk_core::BalancedOptions::default()),
        dir.path(),
    );
    assert!(sb.writer.deduped_bytes >= 700_000);
    check(&bal, dir.path(), 4, true);
}

#[test]
fn fixed_chunks_and_bad_chunker_options() {
    let dir = tempfile::tempdir().unwrap();
    duplicate_tree(dir.path());
    let mut p = Pipeline::fast(FastOptions::default());
    p.fold = Some(Box::new(Dedup::new(FoldOptions {
        dedup: true,
        chunker: ChunkerKind::Fixed(65_536),
    })));
    let (out, s) = run(p, dir.path());
    assert!(s.writer.deduped_bytes >= 700_000);
    check(&out, dir.path(), 4, true);

    let mut p = Pipeline::fast(FastOptions::default());
    p.fold = Some(Box::new(Dedup::new(FoldOptions {
        dedup: true,
        chunker: ChunkerKind::Cdc {
            min: 3,
            avg: 65_536,
            max: 524_288,
        },
    })));
    let e = p.run(dir.path(), Vec::new()).unwrap_err();
    assert!(matches!(e, lpk_core::CoreError::InvalidOption(_)));
}

fn records_of(bytes: &[u8]) -> usize {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    a.records()
        .unwrap()
        .map_or(0, |t| t.table().unwrap().iter().count())
}

#[test]
fn identical_jpegs_need_no_lepton_block_and_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let base = fixture("baseline.jpg");
    std::fs::write(dir.path().join("a1.jpg"), &base).unwrap();
    std::fs::write(dir.path().join("a2.jpg"), &base).unwrap();
    std::fs::write(dir.path().join("a3.jpg"), &base).unwrap();
    std::fs::write(dir.path().join("c1.jpg"), with_secondary()).unwrap();
    std::fs::write(dir.path().join("c2.jpg"), with_secondary()).unwrap();
    let (on, s_on) = run(Pipeline::fast(FastOptions::default()), dir.path());
    let mut p = Pipeline::fast(FastOptions::default());
    p.fold = None;
    let (off, s_off) = run(p, dir.path());
    // Every input is peeled in both runs; with dedup the copies write no record.
    assert_eq!(s_on.peel.peeled.files, 5);
    assert_eq!(s_off.peel.peeled.files, 5);
    assert_eq!(records_of(&off), 5);
    assert_eq!(records_of(&on), 2);
    assert!(s_on.writer.blocks < s_off.writer.blocks);
    assert!(on.len() < off.len());
    // The copies' chunk lists are the originals'.
    let mut a = Archive::open(Cursor::new(on.clone()), &Resources::default()).unwrap();
    let t = a.entry_table().unwrap();
    let es: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let by = |n: &str| es.iter().find(|e| e.path == n).unwrap().chunks.clone();
    assert_eq!(by("a1.jpg"), by("a2.jpg"));
    assert_eq!(by("a1.jpg"), by("a3.jpg"));
    assert_eq!(by("c1.jpg"), by("c2.jpg"));
    check(&on, dir.path(), 5, false);
    check(&off, dir.path(), 5, false);
}
