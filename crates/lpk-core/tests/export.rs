//! The entry-ordered export engine (E3-6): a collecting sink against the archive's files on
//! the sequential and the parallel paths, under a tight and a wide memory budget (every
//! block decoded once when the cache holds them all; back references decoded again when it
//! cannot), a sink that stops early, a damaged chunk, a sealed archive without its key.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use lpk_core::{
    export_archive, register_full_reader, CoreError, ExportSummary, ExtractOptions, Sink,
};
use lpk_format::{
    Archive, Argon2Params, Credentials, Entry, EntryFlags, EntryKind, FormatError, Resources,
    SealOptions, Suite, Writer, WriterOptions,
};

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
        archive_id: [7; 16],
        dedup: true,
        ..WriterOptions::default()
    }
}

/// Files whose later entries refer back to chunks of earlier blocks: `a/one` and `z/last`
/// share content, `m/mid` repeats a chunk of `a/one` in its middle, and a few are empty or
/// span several blocks.
fn mixed() -> Vec<(String, Vec<u8>)> {
    let one = noise(1, 40_000);
    let mut mid = noise(2, 10_000);
    mid.extend_from_slice(&one[..8192]);
    mid.extend_from_slice(&noise(3, 9_000));
    vec![
        ("a/one.bin".into(), one.clone()),
        ("a/sub/two.bin".into(), noise(4, 50_000)),
        ("empty".into(), Vec::new()),
        ("m/mid.bin".into(), mid),
        ("small.txt".into(), b"hello".to_vec()),
        ("z/last.bin".into(), one),
    ]
}

/// The archive of `files`, with a directory and a symlink entry; every file's mtime is its
/// index times 1000 ns.
fn pack(files: &[(String, Vec<u8>)], o: WriterOptions) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, o).unwrap();
    w.add_directory("a", EntryFlags::EMPTY, 5).unwrap();
    for (i, (p, d)) in files.iter().enumerate() {
        w.add_file(p, EntryFlags::EMPTY, i as i64 * 1000, &mut &d[..])
            .unwrap();
    }
    w.add_symlink("link", EntryFlags::EMPTY, 9, b"a/one.bin")
        .unwrap();
    w.finish().unwrap();
    out
}

/// What the sink was handed, in order: kind, path, content (a symlink's target), mtime.
#[derive(Debug, Default)]
struct Collect {
    got: Vec<(EntryKind, String, Vec<u8>, i64)>,
    /// Read the files in pieces of this size (0: `read_to_end`).
    piece: usize,
    /// Stop reading after this many files (the rest unread).
    stop_after: Option<usize>,
}

impl Sink for Collect {
    fn directory(&mut self, e: &Entry) -> Result<(), CoreError> {
        self.got
            .push((e.kind, e.path.clone(), Vec::new(), e.mtime_ns));
        Ok(())
    }

    fn symlink(&mut self, e: &Entry) -> Result<(), CoreError> {
        let t = e.symlink_target.clone().unwrap_or_default();
        self.got.push((e.kind, e.path.clone(), t, e.mtime_ns));
        Ok(())
    }

    fn file(&mut self, e: &Entry, content: &mut dyn Read) -> Result<(), CoreError> {
        assert!(e.chunks.is_empty());
        let files = self.got.iter().filter(|g| g.0 == EntryKind::File).count();
        if self.stop_after == Some(files) {
            return Ok(());
        }
        let mut data = Vec::new();
        if self.piece == 0 {
            content.read_to_end(&mut data).unwrap();
        } else {
            let mut buf = vec![0u8; self.piece];
            loop {
                let n = content.read(&mut buf).map_err(|err| CoreError::Export {
                    path: "sink".into(),
                    entry: e.path.clone(),
                    message: err.to_string(),
                })?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
            }
        }
        assert_eq!(data.len() as u64, e.size);
        self.got.push((e.kind, e.path.clone(), data, e.mtime_ns));
        Ok(())
    }
}

fn export(
    bytes: &[u8],
    sink: &mut dyn Sink,
    opts: &ExtractOptions,
) -> Result<ExportSummary, CoreError> {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    export_archive(&mut a, || Ok(Cursor::new(bytes.to_vec())), sink, opts)
}

fn threads(n: usize) -> ExtractOptions {
    ExtractOptions {
        threads: Some(n),
        ..ExtractOptions::default()
    }
}

fn expect(files: &[(String, Vec<u8>)]) -> Vec<(EntryKind, String, Vec<u8>, i64)> {
    let mut v: Vec<(EntryKind, String, Vec<u8>, i64)> = files
        .iter()
        .enumerate()
        .map(|(i, (p, d))| (EntryKind::File, p.clone(), d.clone(), i as i64 * 1000))
        .collect();
    v.push((EntryKind::Directory, "a".into(), Vec::new(), 5));
    v.push((EntryKind::Symlink, "link".into(), b"a/one.bin".to_vec(), 9));
    v.sort_by(|x, y| x.1.cmp(&y.1));
    v
}

#[test]
fn the_sink_receives_every_entry_in_order_on_both_paths() {
    let files = mixed();
    let bytes = pack(&files, options());
    let want = expect(&files);
    for n in [1, 2, 8] {
        for piece in [0, 1000] {
            let mut sink = Collect {
                piece,
                ..Collect::default()
            };
            let s = export(&bytes, &mut sink, &threads(n)).unwrap();
            assert_eq!(sink.got, want, "threads {n}, piece {piece}");
            assert_eq!(s.files, 6);
            assert_eq!(s.directories, 1);
            assert_eq!(s.symlinks, 1);
            assert_eq!(s.bytes, files.iter().map(|f| f.1.len() as u64).sum::<u64>());
            assert!(s.blocks_needed >= 4, "{s:?}");
            assert!(s.visits > s.blocks_needed, "{s:?}");
            assert_eq!(s.workers, if n == 1 { 1 } else { n - 1 }.min(s.in_flight));
            assert!(s.peak_in_flight <= s.in_flight, "{s:?}");
            // The default budget holds every needed block (plus the one in transit on the
            // parallel path): each decoded once.
            assert_eq!(
                s.in_flight as u64,
                s.blocks_needed + u64::from(n > 1),
                "{s:?}"
            );
            assert_eq!(s.blocks_decoded, s.blocks_needed, "{s:?}");
            assert_eq!(s.redecodes, 0);
            assert_eq!(
                s.counted(),
                s.memory.fixed + s.in_flight as u64 * s.memory.per_block
            );
        }
    }
}

#[test]
fn a_tight_budget_holds_one_block_and_decodes_back_references_again() {
    let files = mixed();
    let bytes = pack(&files, options());
    let want = expect(&files);
    let wide = export(&bytes, &mut Collect::default(), &threads(1)).unwrap();
    let one = wide.memory.total(1);
    for n in [1, 2, 8] {
        let mut sink = Collect::default();
        let opts = ExtractOptions {
            threads: Some(n),
            memory: Some(one),
            ..ExtractOptions::default()
        };
        let s = export(&bytes, &mut sink, &opts).unwrap();
        assert_eq!(sink.got, want, "threads {n}");
        assert_eq!(s.in_flight, 1, "{s:?}");
        assert_eq!(s.workers, 1);
        assert_eq!(s.peak_in_flight, 1);
        // Nothing cached: one decode per visit.
        assert_eq!(s.blocks_decoded, s.visits, "{s:?}");
        assert!(s.redecodes > 0, "{s:?}");
        // A budget below one block in flight is refused with the figure.
        let e = export(
            &bytes,
            &mut Collect::default(),
            &ExtractOptions {
                memory: Some(one - 1),
                ..opts
            },
        )
        .unwrap_err();
        assert!(
            matches!(&e, CoreError::InvalidOption(m) if m.contains("below what one block in flight needs")),
            "{e:?}"
        );
    }
}

#[test]
fn a_cache_of_two_blocks_decodes_fewer_times_than_none_and_more_than_all() {
    let files = mixed();
    let bytes = pack(&files, options());
    let wide = export(&bytes, &mut Collect::default(), &threads(1)).unwrap();
    let m = wide.memory;
    let per = m.per_block + m.per_worker;
    let none = export(
        &bytes,
        &mut Collect::default(),
        &ExtractOptions {
            threads: Some(1),
            memory: Some(m.fixed_shared + per),
            ..ExtractOptions::default()
        },
    )
    .unwrap();
    // Two held: the sequential path caches both; the parallel path caches one and keeps one
    // in transit.
    for n in [1, 4] {
        let mut sink = Collect::default();
        let two = export(
            &bytes,
            &mut sink,
            &ExtractOptions {
                threads: Some(n),
                memory: Some(m.fixed_shared + 2 * per),
                ..ExtractOptions::default()
            },
        )
        .unwrap();
        assert_eq!(sink.got, expect(&files));
        assert_eq!(two.in_flight, 2, "{two:?}");
        assert!(two.peak_in_flight <= 2);
        assert!(
            two.blocks_decoded <= none.blocks_decoded && two.blocks_decoded >= wide.blocks_decoded,
            "none {} two {} all {}",
            none.blocks_decoded,
            two.blocks_decoded,
            wide.blocks_decoded
        );
    }
}

#[test]
fn a_sink_that_does_not_read_a_file_to_the_end_is_an_error() {
    let bytes = pack(&mixed(), options());
    for n in [1, 4] {
        let mut sink = Collect {
            stop_after: Some(1),
            ..Collect::default()
        };
        let e = export(&bytes, &mut sink, &threads(n)).unwrap_err();
        assert!(
            matches!(&e, CoreError::Extract(m) if m.contains("did not read the whole file")),
            "{e:?}"
        );
    }
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
fn a_damaged_chunk_is_the_engines_decode_error_not_the_sinks() {
    let bytes = pack(&mixed(), options());
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let victim = a.index().blocks[2];
    let last = victim.first_chunk + victim.chunk_count - 1;
    let mut bad = bytes.clone();
    bad[(victim.frame_offset + victim.frame_len - 33) as usize] ^= 0x55;
    rehash_frame(&mut bad, victim.frame_offset, victim.frame_len);
    for n in [1, 8] {
        // A sink reading in pieces wraps the reader's error; the engine's own is returned.
        let mut sink = Collect {
            piece: 777,
            ..Collect::default()
        };
        let e = export(&bad, &mut sink, &threads(n)).unwrap_err();
        assert!(
            matches!(&e, CoreError::Decode { block: 2, path, source: FormatError::ChunkMismatch { chunk } } if *chunk == last && !path.is_empty()),
            "{e:?}"
        );
    }
}

#[test]
fn a_sealed_archive_without_its_key_is_refused_before_the_sink_is_called() {
    let credentials = Credentials {
        password: b"pw".to_vec(),
        keyfile: None,
    };
    let o = WriterOptions {
        seal: Some(SealOptions {
            suite: Suite::AesGcm,
            argon2: Argon2Params {
                t: 1,
                m_kib: 8192,
                p: 1,
            },
            listable: true,
            credentials: credentials.clone(),
        }),
        ..options()
    };
    let files = mixed();
    let bytes = pack(&files, o);
    let mut sink = Collect::default();
    let e = export(&bytes, &mut sink, &threads(2)).unwrap_err();
    assert!(
        matches!(e, CoreError::Format(FormatError::PasswordRequired)),
        "{e:?}"
    );
    assert!(sink.got.is_empty());
    // With the key: the same entries.
    let mut a = Archive::open_with(
        Cursor::new(bytes.clone()),
        &Resources::default(),
        Some(&credentials),
    )
    .unwrap();
    register_full_reader(&mut a);
    let mut sink = Collect::default();
    export_archive(
        &mut a,
        || Ok(Cursor::new(bytes.clone())),
        &mut sink,
        &threads(2),
    )
    .unwrap();
    assert_eq!(sink.got, expect(&files));
}

#[test]
fn files_sharing_every_chunk_are_served_from_the_same_blocks() {
    // Ten copies of one file: one block holds the content, every copy visits it.
    let one = noise(11, 30_000);
    let files: Vec<(String, Vec<u8>)> = (0..10)
        .map(|i| (format!("copy-{i:02}.bin"), one.clone()))
        .collect();
    let bytes = pack(&files, options());
    let mut sink = Collect::default();
    let s = export(&bytes, &mut sink, &threads(3)).unwrap();
    let got: BTreeMap<String, Vec<u8>> = sink
        .got
        .into_iter()
        .filter(|g| g.0 == EntryKind::File)
        .map(|g| (g.1, g.2))
        .collect();
    assert_eq!(got.len(), 10);
    assert!(got.values().all(|d| *d == one));
    assert_eq!(s.blocks_decoded, s.blocks_needed, "{s:?}");
}

#[test]
fn a_last_use_block_under_a_full_cache_does_not_wait_for_a_permit() {
    // Runs A, B, A: A is cached after its first run (it comes back), B is used once, so the
    // schedule does not cache it — but B's run holds a permit. With one permit (a budget of
    // one block) the cache held it all and B's decode waited for the eviction that never
    // came: the sequential path hung. The export runs on a helper thread and fails by a
    // deadline instead of hanging the test; the thread count is explicit, not the host's.
    let x = noise(21, 30_000);
    let y = noise(22, 30_000);
    let files = vec![
        ("1-x.bin".to_string(), x.clone()),
        ("2-y.bin".to_string(), y),
        ("3-x-again.bin".to_string(), x),
    ];
    let bytes = pack(&files, options());
    let wide = export(&bytes, &mut Collect::default(), &threads(1)).unwrap();
    assert!(wide.blocks_needed >= 2 && wide.visits >= 3, "{wide:?}");
    let one = wide.memory.total(1);
    let want = expect(&files);
    for n in [1, 2] {
        let bytes = bytes.clone();
        let want = want.clone();
        let worker = std::thread::spawn(move || {
            let mut sink = Collect::default();
            let opts = ExtractOptions {
                threads: Some(n),
                memory: Some(one),
                ..ExtractOptions::default()
            };
            let s = export(&bytes, &mut sink, &opts).unwrap();
            assert_eq!(sink.got, want, "threads {n}");
            s
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !worker.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "threads {n}: the export did not finish in 120 s (a permit never returned)"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let s = worker.join().unwrap();
        assert_eq!(s.in_flight, 1, "{s:?}");
        assert_eq!(s.peak_in_flight, 1, "{s:?}");
        assert_eq!(s.blocks_decoded, s.visits, "{s:?}");
    }
}

/// E3-4: `lpk export` takes the same selection as `x`: only the selected entries reach the
/// sink and only their blocks are decoded; a pattern matching nothing is refused before the
/// sink sees an entry.
#[test]
fn a_selection_exports_its_entries_only() {
    let files = mixed();
    let bytes = pack(&files, options());
    let all = export(&bytes, &mut Collect::default(), &threads(1)).unwrap();
    let mut sink = Collect::default();
    let opts = ExtractOptions {
        select: lpk_core::Selection::new(&["a", "small.txt"]).unwrap(),
        ..threads(2)
    };
    let s = export(&bytes, &mut sink, &opts).unwrap();
    let got: Vec<&str> = sink.got.iter().map(|g| g.1.as_str()).collect();
    assert_eq!(got, ["a", "a/one.bin", "a/sub/two.bin", "small.txt"]);
    assert_eq!((s.files, s.directories, s.symlinks), (3, 1, 0));
    assert!(s.blocks_needed < all.blocks_needed, "{s:?} vs {all:?}");
    assert_eq!(s.blocks_decoded, s.blocks_needed, "{s:?}");
    let mut sink = Collect::default();
    let opts = ExtractOptions {
        select: lpk_core::Selection::new(&["a", "none"]).unwrap(),
        ..threads(2)
    };
    let e = export(&bytes, &mut sink, &opts).unwrap_err();
    assert!(
        matches!(e, CoreError::NoMatch { ref pattern } if pattern == "none"),
        "{e}"
    );
    assert!(sink.got.is_empty());
}
