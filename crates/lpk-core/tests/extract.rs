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
        let decoders = if n == 1 { 1 } else { n - (n / 4).clamp(1, 4) };
        assert_eq!(s.workers, decoders.min(blocks as usize), "{s:?}");
        assert!(s.workers + s.writers <= n.max(2), "{s:?}");
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
        assert_eq!(s.nested_decodes, 0, "{s:?}");
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
        println!(
            "threads {n}: {} blocks decoded, {} nested decodes",
            s.blocks_decoded, s.nested_decodes
        );
    }
    // The same through a file on disk (each worker opens it again).
    let w = tempfile::tempdir().unwrap();
    let arch = w.path().join("j.lpk");
    std::fs::write(&arch, &bytes).unwrap();
    let out = w.path().join("out");
    extract_file(&arch, &out, &[], None, &DefaultPolicy, &threads(4)).unwrap();
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
            matches!(&e, CoreError::Decode { block: 2, path, source: FormatError::ChunkMismatch { chunk } } if *chunk == last && !path.is_empty()),
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
    let blocks = a.index().blocks.len();
    // The terms do not depend on the budget: read them from a run with room for every block.
    let d = tempfile::tempdir().unwrap();
    let wide = ExtractOptions {
        threads: Some(8),
        memory: Some(u64::MAX),
        ..ExtractOptions::default()
    };
    let m = extract(&bytes, &d.path().join("out"), &wide)
        .unwrap()
        .memory;
    // A store archive: the plain bytes, the frame they are copied from, no decoder beyond.
    assert_eq!((m.plain, m.decoder, m.nested), (env.max_block_plain, 0, 0));
    let largest = a.index().blocks.iter().map(|b| b.frame_len).max().unwrap();
    assert_eq!(m.encoded, largest);
    assert_eq!(m.step_input, m.plain);
    assert_eq!(
        m.per_block,
        m.encoded + m.plain + m.step_input + lpk_core::extract::BLOCK_ALLOWANCE
    );
    assert_eq!(m.held, m.plain + lpk_core::extract::BLOCK_ALLOWANCE);
    let cost = m.per_block + m.per_worker;
    // A given budget below one block in flight is refused with the figure; nothing is created.
    for memory in [1, m.total(1) - 1] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let e = extract(
            &bytes,
            &out,
            &ExtractOptions {
                threads: Some(8),
                memory: Some(memory),
                ..ExtractOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(e, CoreError::InvalidOption(_)), "{e}");
        assert!(
            e.to_string().contains(&format!(
                "below what one block in flight needs: {} bytes",
                m.total(1)
            )),
            "{e}"
        );
        assert!(
            !out.exists(),
            "{memory}: nothing is created before the refusal"
        );
    }
    for (memory, workers) in [
        (m.total(1), 1),
        (m.total(2), 2),
        (m.total(2) - 1, 1),
        (u64::MAX, blocks.min(6)),
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
        // The permits: the workers plus the held blocks the rest of the budget pays for, at
        // most eight per worker and at most the blocks (E2-25).
        let held =
            usize::try_from(memory.saturating_sub(m.total(workers)) / m.held).unwrap_or(usize::MAX);
        let in_flight = workers
            .saturating_add(held)
            .min((workers * lpk_core::extract::HELD_PER_WORKER).min(blocks))
            .max(workers);
        assert_eq!((s.workers, s.in_flight), (workers, in_flight), "{memory}");
        // Measured: the blocks held at once never exceed what the budget allows (E2-20): at
        // most `workers` of them decoding, the others held.
        assert!(
            (1..=in_flight).contains(&s.peak_in_flight),
            "{memory}: peak {} of {in_flight}",
            s.peak_in_flight
        );
        let decoding = s.peak_in_flight.min(workers) as u64;
        let waiting = s.peak_in_flight.saturating_sub(workers) as u64;
        assert!(
            s.memory.fixed_shared + decoding * cost + waiting * m.held <= memory.max(m.total(1)),
            "{memory}: peak {}",
            s.peak_in_flight
        );
        assert!(s.memory.total_in_flight(s.workers, s.in_flight) <= memory.max(m.total(1)));
        assert_eq!(
            s.memory.fixed,
            m.fixed_shared + workers as u64 * m.per_worker
        );
        assert_eq!(s.memory.per_block, m.per_block);
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
    fn linked_parent(&self, dir: &Path) -> Result<(), FormatError> {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        self.0.lock().unwrap().push(format!("linked {name}"));
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

/// A store decoder that panics on its `at`-th call (counting from one).
struct PanicStore {
    calls: std::sync::atomic::AtomicUsize,
    at: usize,
}

impl lpk_format::PrimitiveDecoder for PanicStore {
    fn decode(
        &self,
        _params: &[u8],
        input: &[u8],
        _expected_len: u64,
        _limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        assert!(n != self.at, "test decoder panics on call {n}");
        Ok(input.to_vec())
    }
}

#[test]
fn a_panicking_decoder_stops_the_pool_cleans_up_and_resumes_the_panic() {
    let files = mixed();
    let bytes = pack(&files, options());
    let want = expect(&files);
    for n in [1, 4, 8] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let (tx, rx) = std::sync::mpsc::channel();
        let (b, o) = (bytes.clone(), out.clone());
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(|| {
                let mut a = Archive::open(Cursor::new(b.clone()), &Resources::default()).unwrap();
                a.registry_mut().register(
                    lpk_format::PrimitiveId::Store,
                    Box::new(PanicStore {
                        calls: std::sync::atomic::AtomicUsize::new(0),
                        at: 4,
                    }),
                );
                extract_archive(
                    &mut a,
                    || Ok(Cursor::new(b.clone())),
                    &o,
                    &DefaultPolicy,
                    &threads(n),
                )
            });
            let _ = tx.send(r.is_err());
        });
        let panicked = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the extraction hung");
        assert!(panicked, "threads {n}: the panic was not resumed");
        for (p, h) in tree(&out) {
            assert_eq!(
                want.get(&p),
                Some(&h),
                "threads {n}: {p} is partial or wrong"
            );
        }
    }
}

#[test]
fn a_writer_error_fails_the_extraction_and_cleans_up() {
    let files = mixed();
    let bytes = pack(&files, options());
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let last = a.index().blocks.len() - 1;
    // c.txt is the last file written: its chunk lies in the last block.
    let mut a = a;
    let entries: Vec<Entry> = a
        .entry_table()
        .unwrap()
        .table()
        .unwrap()
        .iter()
        .map(|e| e.unwrap())
        .filter(|e| e.path == "c.txt")
        .collect();
    assert_eq!(a.chunks().locate(entries[0].chunks[0]).unwrap().block, last);
    let want = expect(&files);
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("c.txt"), b"mine").unwrap();
    let p = Recording::default();
    let e = extract_with(&bytes, &out, &threads(8), &p).unwrap_err();
    assert!(
        matches!(&e, CoreError::Io { path, source } if path.ends_with("c.txt") && source.kind() == std::io::ErrorKind::AlreadyExists),
        "{e:?}"
    );
    assert_eq!(std::fs::read(out.join("c.txt")).unwrap(), b"mine");
    for (p, h) in tree(&out) {
        if p != "c.txt" {
            assert_eq!(want.get(&p), Some(&h), "{p} is partial or wrong");
        }
    }
}

#[test]
fn a_linked_parent_is_refused() {
    let files = mixed();
    let bytes = pack(&files, options());
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let elsewhere = d.path().join("elsewhere");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let link = out.join("a");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    #[cfg(windows)]
    {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&elsewhere)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !made {
            println!("SKIPPED: mklink /J refused on this machine");
            return;
        }
    }
    let e = extract(&bytes, &out, &threads(4)).unwrap_err();
    assert!(
        matches!(
            &e,
            CoreError::Format(FormatError::UnsafePath {
                reason: "linked parent",
                ..
            })
        ),
        "{e:?}"
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(!out.join("c.txt").exists());
}

#[test]
fn a_panic_in_the_dispatcher_does_not_hang_and_cleans_up() {
    let files = mixed();
    let bytes = pack(&files, options());
    let want = expect(&files);
    for n in [4, 8] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let (tx, rx) = std::sync::mpsc::channel();
        let (b, o) = (bytes.clone(), out.clone());
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(|| {
                extract(
                    &b,
                    &o,
                    &ExtractOptions {
                        threads: Some(n),
                        panic_in_dispatch_at: Some(3),
                        ..ExtractOptions::default()
                    },
                )
            });
            let _ = tx.send(r.is_err());
        });
        let panicked = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the extraction hung");
        assert!(panicked, "threads {n}: the panic was not resumed");
        for (p, h) in tree(&out) {
            assert_eq!(
                want.get(&p),
                Some(&h),
                "threads {n}: {p} is partial or wrong"
            );
        }
    }
}

/// The distinct blocks holding the chunks of the entry `path`, in index order.
fn blocks_of(bytes: &[u8], path: &str) -> Vec<usize> {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    let entries: Vec<Entry> = a
        .entry_table()
        .unwrap()
        .table()
        .unwrap()
        .iter()
        .map(|e| e.unwrap())
        .collect();
    let e = entries.iter().find(|e| e.path == path).unwrap();
    let table = a.chunks();
    let mut blocks: Vec<usize> = e
        .chunks
        .iter()
        .map(|&c| table.locate(c).unwrap().block)
        .collect();
    blocks.sort_unstable();
    blocks.dedup();
    blocks
}

/// E3-4: a selection is fed to the plan, so only the blocks the selected entries need are
/// decoded: one file selected out of an archive of several blocks, the decode count equals
/// the blocks that hold it; a directory pattern selects its subtree and the directories a
/// selected path implies are created; a pattern matching nothing is refused before any output.
#[test]
fn a_selection_decodes_only_the_blocks_its_entries_need() {
    let files = mixed();
    let bytes = pack(&files, options());
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let blocks = a.index().blocks.len() as u64;
    assert!(blocks >= 5, "{blocks}");
    // One file: c.txt lies in one block; a/one.bin in several (40 000 bytes over 16 KiB
    // blocks), fewer than the archive has.
    for (path, data) in [("c.txt", &files[4].1), ("a/one.bin", &files[0].1)] {
        let held = blocks_of(&bytes, path);
        assert!(
            !held.is_empty() && held.len() < blocks as usize,
            "{path}: {held:?}"
        );
        for n in [1, 8] {
            let d = tempfile::tempdir().unwrap();
            let out = d.path().join("out");
            let opts = ExtractOptions {
                select: lpk_core::Selection::new(&[path]).unwrap(),
                ..threads(n)
            };
            let s = extract(&bytes, &out, &opts).unwrap();
            assert_eq!(
                s.blocks_needed,
                held.len() as u64,
                "{path}, threads {n}: {s:?}"
            );
            assert_eq!(
                s.blocks_decoded,
                held.len() as u64,
                "{path}, threads {n}: {s:?}"
            );
            assert_eq!(s.files, 1);
            assert_eq!(s.blocks, blocks);
            let want = expect(&[(path.to_string(), data.clone())]);
            assert_eq!(tree(&out), want, "{path}, threads {n}");
        }
    }
    // A directory pattern: the subtree, with the directories its paths imply.
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let opts = ExtractOptions {
        select: lpk_core::Selection::new(&["a"]).unwrap(),
        ..threads(2)
    };
    let s = extract(&bytes, &out, &opts).unwrap();
    assert_eq!(s.files, 2);
    let mut held = blocks_of(&bytes, "a/one.bin");
    held.extend(blocks_of(&bytes, "a/sub/two.bin"));
    held.sort_unstable();
    held.dedup();
    assert_eq!(s.blocks_decoded, held.len() as u64, "{s:?}");
    assert_eq!(tree(&out), expect(&files[..2]));
    // No match: refused by name, nothing created.
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let opts = ExtractOptions {
        select: lpk_core::Selection::new(&["a", "nothing/*"]).unwrap(),
        ..threads(2)
    };
    let e = extract(&bytes, &out, &opts).unwrap_err();
    assert!(
        matches!(e, CoreError::NoMatch { ref pattern } if pattern == "nothing/*"),
        "{e}"
    );
    assert!(!out.exists());
}
