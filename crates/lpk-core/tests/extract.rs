//! The block-ordered extraction engine (E2-19): files across block boundaries, deduplicated
//! chunks placed at offsets, peeled JPEGs, each block decoded once, `--threads 1` against a pool,
//! a damaged chunk, the memory bound, the policy hooks and the modification time.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use lpk_core::{
    archive_fast, extract_archive, extract_file, register_full_reader, CoreError, DefaultPolicy,
    ExtractOptions, ExtractPolicy, ExtractSummary, FastOptions,
};
use lpk_format::{Archive, Entry, EntryFlags, FormatError, Resources, Writer, WriterOptions};

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

fn options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16384,
        archive_id: [5; 16],
        dedup: true,
        ..WriterOptions::default()
    }
}

/// Write `files` (path, bytes) with the store graph; every file gets `mtime_ns` = its index.
fn pack(files: &[(String, Vec<u8>)], o: WriterOptions) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, o).unwrap();
    for (i, (p, d)) in files.iter().enumerate() {
        w.add_file(p, EntryFlags::EMPTY, i as i64 * 1000, &mut &d[..])
            .unwrap();
    }
    w.finish().unwrap();
    out
}

fn extract(bytes: &[u8], dir: &Path, opts: &ExtractOptions) -> Result<ExtractSummary, CoreError> {
    extract_with(bytes, dir, opts, &DefaultPolicy)
}

fn extract_with(
    bytes: &[u8],
    dir: &Path,
    opts: &ExtractOptions,
    policy: &dyn ExtractPolicy,
) -> Result<ExtractSummary, CoreError> {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    extract_archive(
        &mut a,
        || Ok(Cursor::new(bytes.to_vec())),
        dir,
        policy,
        opts,
    )
}

fn threads(n: usize) -> ExtractOptions {
    ExtractOptions {
        threads: Some(n),
        ..ExtractOptions::default()
    }
}

/// Relative path -> BLAKE3 of every file under `root` (directories as `None`).
fn tree(root: &Path) -> BTreeMap<String, Option<[u8; 32]>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(rel, None);
                stack.push(p);
            } else {
                out.insert(
                    rel,
                    Some(*blake3::hash(&std::fs::read(&p).unwrap()).as_bytes()),
                );
            }
        }
    }
    out
}

fn expect(files: &[(String, Vec<u8>)]) -> BTreeMap<String, Option<[u8; 32]>> {
    let mut m = BTreeMap::new();
    for (p, d) in files {
        let mut acc = String::new();
        let parts: Vec<&str> = p.split('/').collect();
        for c in &parts[..parts.len() - 1] {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(c);
            m.insert(acc.clone(), None);
        }
        m.insert(p.clone(), Some(*blake3::hash(d).as_bytes()));
    }
    m
}

/// Files spanning blocks, a file made of deduplicated chunks of earlier blocks (in reverse
/// block order), an empty file, nested directories.
fn mixed() -> Vec<(String, Vec<u8>)> {
    let a = noise(1, 40_000);
    let b = noise(2, 50_000);
    let mut c = b[8192..16384].to_vec();
    c.extend_from_slice(&a[0..8192]);
    c.extend_from_slice(&noise(3, 1000));
    vec![
        ("a/one.bin".into(), a),
        ("a/sub/two.bin".into(), b),
        ("b/dedup.bin".into(), c),
        ("b/empty".into(), Vec::new()),
        ("c.txt".into(), b"small file\n".to_vec()),
    ]
}

#[test]
fn files_span_blocks_and_deduplicated_chunks_land_at_their_offsets() {
    let files = mixed();
    let bytes = pack(&files, options());
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let blocks = a.index().blocks.len() as u64;
    assert!(blocks >= 5, "{blocks}");
    for n in [1, 2, 8] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let s = extract(&bytes, &out, &threads(n)).unwrap();
        assert_eq!(tree(&out), expect(&files), "threads {n}");
        assert_eq!(s.files, 5);
        assert_eq!(s.blocks, blocks);
        // Each needed block decoded exactly once.
        assert_eq!(s.blocks_decoded, s.blocks_needed);
        assert_eq!(s.blocks_needed, blocks);
        // The dedup file's chunks are placed again: more placements than stored chunks.
        assert!(s.placements > a.chunks().len(), "{s:?}");
        assert_eq!(s.workers, n.min(blocks as usize), "{s:?}");
    }
}

#[test]
fn ten_thousand_small_files_decode_each_block_once() {
    let files: Vec<(String, Vec<u8>)> = (0..10_000)
        .map(|i| {
            (
                format!("d{:02}/f{i:05}.txt", i % 37),
                format!("file {i} {}\n", "x".repeat(i % 50)).into_bytes(),
            )
        })
        .collect();
    let bytes = pack(
        &files,
        WriterOptions {
            block_size: 1 << 18,
            ..options()
        },
    );
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let blocks = a.index().blocks.len() as u64;
    assert!((2..=10).contains(&blocks), "{blocks}");
    let mut trees = Vec::new();
    for n in [1, 8] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let s = extract(
            &bytes,
            &out,
            &ExtractOptions {
                threads: Some(n),
                max_open_files: 16,
                ..ExtractOptions::default()
            },
        )
        .unwrap();
        assert_eq!(s.files, 10_000);
        assert_eq!(s.blocks_decoded, blocks, "{s:?}");
        assert_eq!(s.blocks_needed, blocks, "{s:?}");
        let t = tree(&out);
        assert_eq!(t, expect(&files));
        trees.push(t);
    }
    assert_eq!(trees[0], trees[1]);
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

#[test]
fn peeled_jpegs_next_to_plain_files_through_the_pool() {
    let src = tempfile::tempdir().unwrap();
    let base = fixture("baseline.jpg");
    std::fs::write(src.path().join("a.jpg"), &base).unwrap();
    let mut t = base.clone();
    t.extend_from_slice(b"trailing bytes");
    std::fs::write(src.path().join("b-trailing.jpg"), t).unwrap();
    std::fs::write(src.path().join("c.jpg"), fixture("secondary.jpg")).unwrap();
    std::fs::write(src.path().join("d.jpg"), fixture("progressive.jpg")).unwrap();
    std::fs::write(src.path().join("notes.txt"), "plain text\n".repeat(400)).unwrap();
    std::fs::write(src.path().join("noise.bin"), noise(9, 70_000)).unwrap();
    let mut bytes = Vec::new();
    let (_, fast) = archive_fast(src.path(), &mut bytes, FastOptions::default()).unwrap();
    assert!(fast.peel.peeled.files >= 2, "{:?}", fast.peel);
    let want = tree(src.path());
    for n in [1, 4] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let s = extract(&bytes, &out, &threads(n)).unwrap();
        assert_eq!(tree(&out), want, "threads {n}");
        assert_eq!(s.blocks_decoded, s.blocks_needed);
    }
    // The same through a file on disk (each worker opens it again).
    let w = tempfile::tempdir().unwrap();
    let arch = w.path().join("j.lpk");
    std::fs::write(&arch, &bytes).unwrap();
    let out = w.path().join("out");
    extract_file(&arch, &out, &[], &DefaultPolicy, &threads(4)).unwrap();
    assert_eq!(tree(&out), want);
}

/// Recompute the hash of the frame at `offset` of whole length `len`.
fn rehash_frame(bytes: &mut [u8], offset: u64, len: u64) {
    let (o, l) = (offset as usize, len as usize);
    let vl = (1..=10)
        .find(|&vl| lpk_format::varint::len((l - 36 - vl) as u64) == vl)
        .unwrap();
    let payload = bytes[o + 4 + vl..o + l - 32].to_vec();
    let h = blake3::hash(&payload);
    bytes[o + l - 32..o + l].copy_from_slice(h.as_bytes());
}

#[test]
fn a_corrupted_chunk_fails_with_chunk_mismatch_and_leaves_no_partial_file() {
    let files = mixed();
    let bytes = pack(&files, options());
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    // A middle block: a file spanning it has chunks written before and after it.
    let victim = a.index().blocks[2];
    let last = victim.first_chunk + victim.chunk_count - 1;
    let mut bad = bytes.clone();
    bad[(victim.frame_offset + victim.frame_len - 33) as usize] ^= 0x55;
    rehash_frame(&mut bad, victim.frame_offset, victim.frame_len);
    let want = expect(&files);
    for n in [1, 8] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let e = extract(&bad, &out, &threads(n)).unwrap_err();
        assert!(
            matches!(e, CoreError::Format(FormatError::ChunkMismatch { chunk }) if chunk == last),
            "{e:?}"
        );
        // Whatever is left is complete and right; the files of the damaged block are gone.
        for (p, h) in tree(&out) {
            assert_eq!(want.get(&p), Some(&h), "{p} is partial or wrong");
        }
        assert!(!out.join("a").join("one.bin").exists() || !out.join("a/sub/two.bin").exists());
    }
}

#[test]
fn the_memory_bound_reduces_the_pool() {
    let files = mixed();
    let bytes = pack(&files, options());
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let env = a.index().envelope;
    let cost = env.max_block_plain + env.decode_memory;
    let blocks = a.index().blocks.len();
    for (memory, workers) in [
        (1, 1),
        (cost, 1),
        (2 * cost + 1, 2),
        (u64::MAX, blocks.min(8)),
    ] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let s = extract(
            &bytes,
            &out,
            &ExtractOptions {
                threads: Some(8),
                memory: Some(memory),
                ..ExtractOptions::default()
            },
        )
        .unwrap();
        assert_eq!((s.workers, s.in_flight), (workers, workers), "{memory}");
        assert_eq!(tree(&out), expect(&files));
    }
}

fn packed_with(add: impl FnOnce(&mut Writer<&mut Vec<u8>>)) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options()).unwrap();
    w.add_file("ok.txt", EntryFlags::EMPTY, 0, &mut &b"fine"[..])
        .unwrap();
    add(&mut w);
    w.finish().unwrap();
    out
}

#[test]
fn the_default_policy_refuses_before_writing_anything() {
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            packed_with(|w| {
                w.add_file("a/CON.txt", EntryFlags::EMPTY, 0, &mut &b"x"[..])
                    .unwrap();
            }),
            "device",
        ),
        (
            packed_with(|w| {
                w.add_file("a/b:c", EntryFlags::EMPTY, 0, &mut &b"x"[..])
                    .unwrap();
            }),
            "colon",
        ),
        (
            packed_with(|w| {
                w.add_symlink("link", EntryFlags::EMPTY, 0, b"ok.txt")
                    .unwrap();
            }),
            "symlink",
        ),
    ];
    for (bytes, what) in cases {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let e = extract(&bytes, &out, &threads(4)).unwrap_err();
        match what {
            "symlink" => assert!(
                matches!(e, CoreError::Format(FormatError::SymlinkRefused { .. })),
                "{e:?}"
            ),
            _ => assert!(
                matches!(e, CoreError::Format(FormatError::UnsafePath { .. })),
                "{what}: {e:?}"
            ),
        }
        assert!(!out.exists(), "{what}: something was written");
    }
    // An existing file: refused, left untouched, nothing else written.
    let files = mixed();
    let bytes = pack(&files, options());
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    std::fs::create_dir_all(out.join("b")).unwrap();
    std::fs::write(out.join("b").join("dedup.bin"), b"mine").unwrap();
    let e = extract(&bytes, &out, &threads(4)).unwrap_err();
    assert!(
        matches!(&e, CoreError::Format(FormatError::Io(io)) if io.kind() == std::io::ErrorKind::AlreadyExists),
        "{e:?}"
    );
    assert_eq!(
        std::fs::read(out.join("b").join("dedup.bin")).unwrap(),
        b"mine"
    );
    assert!(!out.join("c.txt").exists());
    assert!(!out.join("a").exists());
}

/// A policy that refuses nothing but records what it was asked.
#[derive(Default)]
struct Recording(std::sync::Mutex<Vec<String>>);

impl ExtractPolicy for Recording {
    fn check_path(&self, path: &str) -> Result<(), FormatError> {
        self.0.lock().unwrap().push(format!("path {path}"));
        Ok(())
    }
    fn symlink(&self, entry: &Entry) -> Result<(), FormatError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("symlink {}", entry.path));
        Ok(())
    }
    fn overwrite(&self, target: &Path) -> Result<(), FormatError> {
        let name = target.file_name().unwrap().to_string_lossy().into_owned();
        self.0.lock().unwrap().push(format!("overwrite {name}"));
        Ok(())
    }
    fn device(&self, path: &str) -> Result<(), FormatError> {
        self.0.lock().unwrap().push(format!("device {path}"));
        Ok(())
    }
}

#[test]
fn the_hooks_are_called_for_every_entry() {
    let bytes = packed_with(|w| {
        w.add_symlink("link", EntryFlags::EMPTY, 0, b"ok.txt")
            .unwrap();
    });
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("other"), b"x").unwrap();
    let p = Recording::default();
    let s = extract_with(&bytes, &out, &threads(1), &p).unwrap();
    assert_eq!(s.files, 1);
    let calls = p.0.lock().unwrap().clone();
    assert!(calls.contains(&"symlink link".to_string()), "{calls:?}");
    assert!(calls.contains(&"path ok.txt".to_string()), "{calls:?}");
    assert!(calls.contains(&"device link".to_string()), "{calls:?}");
    // No existing target: no overwrite question.
    assert!(
        !calls.iter().any(|c| c.starts_with("overwrite")),
        "{calls:?}"
    );
    // The symlink is never created.
    assert!(!out.join("link").exists());
}

#[test]
fn modification_times_are_restored() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options()).unwrap();
    let t1: i64 = 1_600_000_000_123_456_700;
    let t2: i64 = 1_500_000_000_000_000_000;
    let big = noise(4, 30_000);
    w.add_file("big.bin", EntryFlags::EMPTY, t1, &mut &big[..])
        .unwrap();
    w.add_file("empty", EntryFlags::EMPTY, t2, &mut &b""[..])
        .unwrap();
    w.add_file("unknown", EntryFlags::EMPTY, i64::MIN, &mut &b"u"[..])
        .unwrap();
    w.finish().unwrap();
    for n in [1, 3] {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("out");
        extract(&out, &dir, &threads(n)).unwrap();
        let m = |p: &str| std::fs::metadata(dir.join(p)).unwrap().modified().unwrap();
        assert_eq!(m("big.bin"), UNIX_EPOCH + Duration::from_nanos(t1 as u64));
        assert_eq!(m("empty"), UNIX_EPOCH + Duration::from_nanos(t2 as u64));
        assert_eq!(std::fs::read(dir.join("big.bin")).unwrap(), big);
        assert_eq!(std::fs::read(dir.join("unknown")).unwrap(), b"u");
    }
}
