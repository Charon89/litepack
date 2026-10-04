#![allow(clippy::unwrap_used)]

use lpk_format::{
    Archive, ChunkSource, Chunker, Entry, EntryFlags, EntryKind, FixedChunker, FormatError,
    Resources, Writer, WriterOptions, WriterSummary,
};
use std::cell::Cell;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::rc::Rc;

/// Deterministic pseudo-random bytes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 24) as u8).collect()
    }
}

/// An output that can only be written: the writer must not need more.
struct WriteOnly(Vec<u8>);

impl Write for WriteOnly {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Counts the bytes read.
struct Counting<R> {
    inner: R,
    n: Rc<Cell<u64>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n.set(self.n.get() + k as u64);
        Ok(k)
    }
}

impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn small_options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [7; 16],
        ..WriterOptions::default()
    }
}

enum Item {
    File(Vec<u8>),
    Dir,
    Link(Vec<u8>),
}

/// A sink the test can read back after the writer is done.
#[derive(Clone)]
struct Shared(Rc<std::cell::RefCell<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn pack(items: &[(String, Item)], options: WriterOptions) -> (Vec<u8>, WriterSummary) {
    let sink = Shared(Rc::new(std::cell::RefCell::new(Vec::new())));
    let mut w = Writer::new(sink.clone(), options).unwrap();
    for (path, item) in items {
        match item {
            Item::File(d) => w
                .add_file(path, EntryFlags::EMPTY, 5, &mut d.as_slice())
                .unwrap(),
            Item::Dir => w.add_directory(path, EntryFlags::EMPTY, 6).unwrap(),
            Item::Link(t) => w.add_symlink(path, EntryFlags::EMPTY, 7, t).unwrap(),
        }
    }
    let summary = w.finish().unwrap();
    let bytes = sink.0.borrow().clone();
    (bytes, summary)
}

fn tree() -> Vec<(String, Item)> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut sizes: Vec<usize> = vec![
        0,
        1,
        4095,
        4096,
        (1 << 20) - 1,
        1 << 20,
        (1 << 20) + 1,
        3 << 20,
    ];
    while sizes.len() < 50 {
        sizes.push((rng.next() % 200_000) as usize);
    }
    let mut items: Vec<(String, Item)> = Vec::new();
    for d in 0..5 {
        items.push((format!("d{d}"), Item::Dir));
    }
    for (i, n) in sizes.iter().enumerate() {
        let data = if i % 3 == 0 {
            vec![(i % 251) as u8; *n]
        } else {
            rng.bytes(*n)
        };
        items.push((format!("d{}/f{i:02}", i % 5), Item::File(data)));
    }
    items.push(("link".to_string(), Item::Link(b"d0/f00".to_vec())));
    items.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    items
}

fn entries_of<R: Read + Seek>(a: &mut Archive<R>) -> Vec<Entry> {
    let t = a.entry_table().unwrap();
    t.table().unwrap().iter().map(|e| e.unwrap()).collect()
}

#[test]
fn round_trip_in_memory() {
    let items = tree();
    let (bytes, summary) = pack(&items, small_options());
    assert_eq!(summary.archive_len, bytes.len() as u64);
    assert_eq!(summary.entries, 56);
    assert!(summary.blocks > 100, "several blocks: {}", summary.blocks);
    assert!(summary.chunks >= summary.blocks);

    let counter = Rc::new(Cell::new(0));
    let reader = Counting {
        inner: Cursor::new(bytes.clone()),
        n: Rc::clone(&counter),
    };
    let mut a = Archive::open(reader, &Resources::default()).unwrap();
    assert!(
        counter.get() * 10 < bytes.len() as u64,
        "open read {} of {}",
        counter.get(),
        bytes.len()
    );
    assert_eq!(a.header().archive_id, [7; 16]);

    let entries = entries_of(&mut a);
    assert_eq!(entries.len(), items.len());
    let mut files = 0;
    for (e, (path, item)) in entries.iter().zip(&items) {
        assert_eq!(&e.path, path);
        match item {
            Item::File(d) => {
                files += 1;
                assert_eq!(e.kind, EntryKind::File);
                let mut out = Vec::new();
                a.extract(e, &mut out).unwrap();
                assert!(out == *d, "{path} differs");
            }
            Item::Dir => assert_eq!(e.kind, EntryKind::Directory),
            Item::Link(t) => {
                assert_eq!(e.kind, EntryKind::Symlink);
                assert_eq!(e.symlink_target.as_deref(), Some(t.as_slice()));
            }
        }
    }
    assert_eq!(files, 50);
    let v = a.verify().unwrap();
    assert_eq!(v.entries, summary.entries);
    assert_eq!(v.chunks, summary.chunks);
    assert_eq!(v.blocks, summary.blocks);
    assert_eq!(a.chunks().len(), summary.chunks);
}

#[test]
fn chunks_are_fixed_size_and_blocks_close_before_overflow() {
    let data = vec![9u8; 3 * 4096 + 10];
    let items = vec![("f".to_string(), Item::File(data))];
    let mut o = small_options();
    o.block_size = 10_000; // two 4096-byte chunks fit, a third does not
    let (bytes, s) = pack(&items, o);
    assert_eq!((s.chunks, s.blocks), (4, 2));
    let a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let lens: Vec<u64> = (0..4)
        .map(|i| a.chunks().record(i).unwrap().plain_len)
        .collect();
    assert_eq!(lens, [4096, 4096, 4096, 10]);
    let b = &a.index().blocks;
    assert_eq!((b[0].chunk_count, b[0].plain_len), (2, 8192));
    assert_eq!((b[1].chunk_count, b[1].plain_len), (2, 4096 + 10));
    assert_eq!(a.index().envelope.max_block_plain, 8192);
    assert_eq!(a.index().envelope.decode_memory, 8192);
    let e = a.index().envelope;
    assert_eq!((e.max_window, e.max_bwt_block, e.threads_hint), (0, 0, 0));
}

#[test]
fn empty_archives_and_archives_without_chunks() {
    let (bytes, s) = pack(&[], small_options());
    assert_eq!((s.entries, s.chunks, s.blocks), (0, 0, 0));
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let v = a.verify().unwrap();
    assert_eq!((v.entries, v.chunks, v.blocks), (0, 0, 0));

    let items = vec![
        ("a".to_string(), Item::Dir),
        ("a/empty".to_string(), Item::File(Vec::new())),
    ];
    let (bytes, s) = pack(&items, small_options());
    assert_eq!((s.entries, s.chunks, s.blocks), (2, 0, 0));
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let es = entries_of(&mut a);
    let mut out = vec![1];
    a.extract(&es[1], &mut out).unwrap();
    assert_eq!(out, [1]);
    a.verify().unwrap();
}

#[test]
fn a_tail_is_carried_across_segments() {
    // Segments are 16 KiB (the block size); 3000-byte chunks do not divide it,
    // so a chunk spans the end of the first segment.
    let mut rng = Rng(5);
    let data = rng.bytes(31_000);
    let sink = Shared(Rc::new(std::cell::RefCell::new(Vec::new())));
    let mut w = Writer::with_chunker(
        sink.clone(),
        small_options(),
        Box::new(FixedChunker::new(3000)),
    )
    .unwrap();
    w.add_file("f", EntryFlags::EMPTY, 0, &mut data.as_slice())
        .unwrap();
    // The chunker holds state per file: the next file starts afresh.
    let other = rng.bytes(3500);
    w.add_file("g", EntryFlags::EMPTY, 0, &mut other.as_slice())
        .unwrap();
    let s = w.finish().unwrap();
    assert_eq!(s.chunks, 11 + 2);
    let bytes = sink.0.borrow().clone();
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let lens: Vec<u64> = (0..13)
        .map(|i| a.chunks().record(i).unwrap().plain_len)
        .collect();
    assert_eq!(lens[..10], [3000; 10]);
    assert_eq!(lens[10..], [1000, 3000, 500]);
    let es = entries_of(&mut a);
    for (e, want) in es.iter().zip([&data, &other]) {
        let mut out = Vec::new();
        a.extract(e, &mut out).unwrap();
        assert_eq!(&out, want);
    }
    a.verify().unwrap();
}

/// A chunker that misbehaves in a chosen way.
struct Rogue(&'static str);

impl Chunker for Rogue {
    fn feed(&mut self, bytes: &[u8], eof: bool) -> Vec<usize> {
        let n = bytes.len();
        match self.0 {
            // One chunk of the whole input.
            "oversize" => {
                if n > 0 {
                    vec![n]
                } else {
                    vec![]
                }
            }
            // Never cuts.
            "hoard" => vec![],
            // Cuts at the end only when told it is the end... of nothing.
            "uncut at eof" => {
                if eof {
                    vec![]
                } else {
                    vec![n.min(4096)]
                }
            }
            "not increasing" => vec![10, 10],
            "past the end" => vec![n + 1],
            "zero" => vec![0],
            _ => vec![],
        }
    }
    fn reset(&mut self) {}
}

#[test]
fn chunkers_that_break_the_rules_are_refused() {
    let data = vec![1u8; 30_000];
    // Longer than chunk_size (4096).
    let mut w = Writer::with_chunker(
        WriteOnly(Vec::new()),
        small_options(),
        Box::new(FixedChunker::new(5000)),
    )
    .unwrap();
    assert!(matches!(
        w.add_file("f", EntryFlags::EMPTY, 0, &mut data.as_slice()),
        Err(FormatError::BadChunk { .. })
    ));
    for rule in [
        "oversize",
        "hoard",
        "uncut at eof",
        "not increasing",
        "past the end",
        "zero",
    ] {
        let mut w = Writer::with_chunker(
            WriteOnly(Vec::new()),
            small_options(),
            Box::new(Rogue(rule)),
        )
        .unwrap();
        let r = w.add_file("f", EntryFlags::EMPTY, 0, &mut data.as_slice());
        assert!(
            matches!(r, Err(FormatError::BadChunk { .. })),
            "{rule}: {r:?}"
        );
    }
}

fn flip(bytes: &mut [u8], at: u64) {
    bytes[at as usize] ^= 0x55;
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

fn corruption_fixture() -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let mut rng = Rng(77);
    let files: Vec<(String, Vec<u8>)> = (0..12)
        .map(|i| (format!("f{i:02}"), rng.bytes(3000 + i * 4000)))
        .collect();
    let items: Vec<(String, Item)> = files
        .iter()
        .map(|(p, d)| (p.clone(), Item::File(d.clone())))
        .collect();
    let (bytes, s) = pack(&items, small_options());
    assert!(s.blocks >= 8);
    (bytes, files)
}

/// Extract every file; the names of those that failed with the error `is`
/// accepts (any other error panics).
fn failing_files(
    bytes: Vec<u8>,
    files: &[(String, Vec<u8>)],
    is: fn(&FormatError) -> bool,
) -> Vec<String> {
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let mut failed = Vec::new();
    for e in entries_of(&mut a) {
        let mut out = Vec::new();
        match a.extract(&e, &mut out) {
            Ok(()) => {
                let want = &files.iter().find(|(p, _)| *p == e.path).unwrap().1;
                assert_eq!(&out, want, "{} extracted wrongly", e.path);
            }
            Err(err) if is(&err) => failed.push(e.path.clone()),
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
    failed
}

#[test]
fn a_damaged_block_fails_only_the_files_it_holds() {
    let (bytes, files) = corruption_fixture();
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let blocks = a.index().blocks.clone();
    let victim = blocks[3];
    let range = victim.first_chunk..victim.first_chunk + victim.chunk_count;
    let expect: Vec<String> = {
        let mut a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
        entries_of(&mut a)
            .into_iter()
            .filter(|e| e.chunks.iter().any(|c| range.contains(c)))
            .map(|e| e.path)
            .collect()
    };
    assert!(!expect.is_empty() && expect.len() < files.len());

    // Frame hash left alone: the block frame itself fails its hash, and
    // extraction reports that frame's HashMismatch { kind: 2 } (spec section
    // 9, E1-14d ruling 1), not a chunk's mismatch.
    let mut bad = bytes.clone();
    flip(&mut bad, victim.frame_offset + victim.frame_len - 33);
    assert_eq!(
        failing_files(bad.clone(), &files, |e| matches!(
            e,
            FormatError::HashMismatch { kind: 2 }
        )),
        expect
    );
    let mut a = Archive::open(Cursor::new(bad), &Resources::default()).unwrap();
    assert!(matches!(
        a.verify(),
        Err(FormatError::HashMismatch { kind }) if kind == 2
    ));

    // Frame hash recomputed: the chunk hash catches it.
    let mut bad = bytes.clone();
    flip(&mut bad, victim.frame_offset + victim.frame_len - 33);
    rehash_frame(&mut bad, victim.frame_offset, victim.frame_len);
    let last = victim.first_chunk + victim.chunk_count - 1;
    // Only the block's last chunk holds the flipped byte.
    let expect_last: Vec<String> = {
        let mut a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
        entries_of(&mut a)
            .into_iter()
            .filter(|e| e.chunks.contains(&last))
            .map(|e| e.path)
            .collect()
    };
    assert_eq!(
        failing_files(bad.clone(), &files, |e| matches!(
            e,
            FormatError::ChunkMismatch { .. }
        )),
        expect_last
    );
    let mut a = Archive::open(Cursor::new(bad), &Resources::default()).unwrap();
    assert!(matches!(
        a.verify(),
        Err(FormatError::ChunkMismatch { chunk }) if chunk == last
    ));
}

#[test]
fn a_damaged_entry_table_is_a_hash_mismatch() {
    let (mut bytes, _) = corruption_fixture();
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let at = a.index().entry_table.offset + 6;
    flip(&mut bytes, at);
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    assert!(matches!(
        a.entry_table(),
        Err(FormatError::HashMismatch { .. })
    ));
    assert!(matches!(a.verify(), Err(FormatError::HashMismatch { .. })));
}

#[test]
fn writer_refusals() {
    let new = |o: WriterOptions| Writer::new(WriteOnly(Vec::new()), o);
    let mut o = small_options();
    o.chunk_size = 1024;
    assert!(matches!(new(o), Err(FormatError::BadOptions { .. })));
    let mut o = small_options();
    o.block_size = 4095;
    assert!(matches!(new(o), Err(FormatError::BadOptions { .. })));
    // An encoder whose graph does not validate is refused before the header.
    struct BadGraph(Vec<lpk_format::Step>);
    impl lpk_format::BlockEncoder for BadGraph {
        fn graph(&self) -> lpk_format::Graph {
            lpk_format::Graph {
                steps: self.0.clone(),
            }
        }
        fn encode(&mut self, plain: &[u8]) -> Result<Vec<u8>, FormatError> {
            Ok(plain.to_vec())
        }
        fn resources(&self) -> lpk_format::GraphResources {
            lpk_format::GraphResources::default()
        }
    }
    let zstd_step = |params: Vec<u8>| lpk_format::Step {
        primitive: lpk_format::PrimitiveId::Zstd,
        params,
    };
    let mut o = small_options();
    o.encoder = Box::new(BadGraph(vec![zstd_step(vec![20; 5])]));
    assert!(matches!(new(o), Err(FormatError::BadParams { id: 1, .. })));
    let mut o = small_options();
    o.encoder = Box::new(BadGraph(vec![]));
    assert!(matches!(new(o), Err(FormatError::BadGraph { .. })));

    let mut w = new(small_options()).unwrap();
    let mut empty: &[u8] = b"";
    w.add_file("b", EntryFlags::EMPTY, 0, &mut empty).unwrap();
    assert!(matches!(
        w.add_file("a", EntryFlags::EMPTY, 0, &mut empty),
        Err(FormatError::UnsortedEntries { index: 1 })
    ));
    assert!(matches!(
        w.add_directory("b", EntryFlags::EMPTY, 0),
        Err(FormatError::UnsortedEntries { index: 1 })
    ));
    for bad in ["", "/x", "x/", "a\\b", "x/../y", "c//d"] {
        assert!(
            matches!(
                w.add_directory(bad, EntryFlags::EMPTY, 0),
                Err(FormatError::InvalidPath { index: 1, .. })
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        w.add_symlink("c", EntryFlags::EMPTY, 0, b""),
        Err(FormatError::InvalidPath { .. })
    ));
    // Refused entries leave the writer usable.
    w.add_directory("c", EntryFlags::EMPTY, 0).unwrap();
    assert_eq!(w.finish().unwrap().entries, 2);
}

/// A reader that fails after some bytes.
struct Failing(usize);

impl Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0 == 0 {
            return Err(std::io::Error::other("boom"));
        }
        let n = buf.len().min(self.0);
        buf[..n].fill(1);
        self.0 -= n;
        Ok(n)
    }
}

#[test]
fn a_failing_input_is_an_io_error_and_poisons_the_writer() {
    let mut w = Writer::new(WriteOnly(Vec::new()), small_options()).unwrap();
    let r = w.add_file("f", EntryFlags::EMPTY, 0, &mut Failing(10_000));
    assert!(matches!(r, Err(FormatError::Io(_))));
    let mut empty: &[u8] = b"";
    assert!(matches!(
        w.add_file("g", EntryFlags::EMPTY, 0, &mut empty),
        Err(FormatError::Io(e)) if e.to_string().contains("boom")
    ));
    assert!(matches!(w.finish(), Err(FormatError::Io(_))));
}

/// An output that fails after some bytes.
struct FailingOut(usize);

impl Write for FailingOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.0 == 0 {
            return Err(std::io::Error::other("disk full"));
        }
        let n = buf.len().min(self.0);
        self.0 -= n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_output_error_poisons_the_writer() {
    let mut w = Writer::new(FailingOut(40), small_options()).unwrap();
    let data = vec![2u8; 100_000];
    let r = w.add_file("f", EntryFlags::EMPTY, 0, &mut data.as_slice());
    assert!(matches!(r, Err(FormatError::Io(_))), "{r:?}");
    let mut empty: &[u8] = b"";
    assert!(matches!(
        w.add_file("g", EntryFlags::EMPTY, 0, &mut empty),
        Err(FormatError::Io(e)) if e.to_string().contains("disk full")
    ));
}

#[test]
fn archive_chunks_is_a_chunk_source() {
    let items = vec![("f".to_string(), Item::File(vec![3u8; 10_000]))];
    let (bytes, _) = pack(&items, small_options());
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let mut src = lpk_format::ArchiveChunks::new(&mut a);
    assert_eq!(src.chunk(2).unwrap(), vec![3u8; 10_000 - 8192]);
    assert!(matches!(
        src.chunk(3),
        Err(FormatError::ChunkIndexOutOfRange { chunk: 3, len: 3 })
    ));
}

/// Pack two corpus directories, extract, compare. Run by hand:
/// `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-format --release --test roundtrip -- --ignored --nocapture`
#[test]
#[ignore]
fn corpus_round_trip() {
    let root = std::path::PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    for name in ["small-files", "source-git"] {
        let dir = root.join(name);
        let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
        let mut dirs: Vec<String> = Vec::new();
        let mut stack = vec![(dir.clone(), String::new())];
        while let Some((d, prefix)) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let e = e.unwrap();
                let n = e.file_name().into_string().unwrap();
                let rel = if prefix.is_empty() {
                    n.clone()
                } else {
                    format!("{prefix}/{n}")
                };
                let ty = e.file_type().unwrap();
                if ty.is_dir() {
                    dirs.push(rel.clone());
                    stack.push((e.path(), rel));
                } else if ty.is_file() {
                    files.push((rel, e.path()));
                }
            }
        }
        let mut all: Vec<(String, Option<std::path::PathBuf>)> = files
            .into_iter()
            .map(|(r, p)| (r, Some(p)))
            .chain(dirs.into_iter().map(|r| (r, None)))
            .collect();
        all.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

        let t0 = std::time::Instant::now();
        let sink = Shared(Rc::new(std::cell::RefCell::new(Vec::new())));
        let mut w = Writer::new(sink.clone(), WriterOptions::default()).unwrap();
        let mut plain = 0u64;
        for (rel, path) in &all {
            match path {
                Some(p) => {
                    let mut f = std::fs::File::open(p).unwrap();
                    w.add_file(rel, EntryFlags::EMPTY, 0, &mut f).unwrap();
                    plain += f.metadata().unwrap().len();
                }
                None => w.add_directory(rel, EntryFlags::EMPTY, 0).unwrap(),
            }
        }
        let s = w.finish().unwrap();
        let pack_time = t0.elapsed();
        let bytes = sink.0.borrow().clone();

        let t1 = std::time::Instant::now();
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        let mut compared = 0u64;
        for e in entries_of(&mut a) {
            if e.kind != EntryKind::File {
                continue;
            }
            let mut out = Vec::new();
            a.extract(&e, &mut out).unwrap();
            let orig = std::fs::read(dir.join(&e.path)).unwrap();
            assert_eq!(
                blake3::hash(&out),
                blake3::hash(&orig),
                "{name}/{} differs",
                e.path
            );
            compared += 1;
        }
        let extract_time = t1.elapsed();
        let t2 = std::time::Instant::now();
        a.verify().unwrap();
        println!(
            "{name}: {compared} files, plain {plain} B, archive {} B, {} entries, {} chunks, {} blocks; pack {pack_time:?}, extract+compare {extract_time:?}, verify {:?}",
            s.archive_len, s.entries, s.chunks, s.blocks, t2.elapsed()
        );
    }
}

#[test]
fn a_bad_chunk_abandons_the_writer() {
    let data = vec![1u8; 30_000];
    let mut w = Writer::with_chunker(
        WriteOnly(Vec::new()),
        small_options(),
        Box::new(Rogue("oversize")),
    )
    .unwrap();
    assert!(matches!(
        w.add_file("f", EntryFlags::EMPTY, 0, &mut data.as_slice()),
        Err(FormatError::BadChunk { .. })
    ));
    let mut empty: &[u8] = b"";
    assert!(matches!(
        w.add_file("g", EntryFlags::EMPTY, 0, &mut empty),
        Err(FormatError::BadChunk { .. })
    ));
    assert!(matches!(w.finish(), Err(FormatError::BadChunk { .. })));
}
