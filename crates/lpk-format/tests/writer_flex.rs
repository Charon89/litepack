//! The writer's freedoms (E2-3a): a decode graph per block, adds in any path
//! order, and explicit block boundaries. The format itself does not change.
#![allow(clippy::unwrap_used)]

mod common;

use common::{pattern, ZstdTestEncoder};
use lpk_format::{
    prior_id, Archive, BlockEncoder, Encoded, Entry, EntryFlags, FormatError, Graph, MemoryPriors,
    PrimitiveId, Resources, Step, StoreEncoder, Writer, WriterOptions,
};
use std::io::Cursor;

fn options(encoder: Box<dyn BlockEncoder>) -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 8192,
        archive_id: [3; 16],
        encoder,
        ..WriterOptions::default()
    }
}

fn add(w: &mut Writer<&mut Vec<u8>>, path: &str, data: &[u8]) -> Result<(), FormatError> {
    w.add_file(path, EntryFlags::EMPTY, 7, &mut &*data)
}

fn open(bytes: Vec<u8>) -> Archive<Cursor<Vec<u8>>> {
    Archive::open(Cursor::new(bytes), &Resources::default()).unwrap()
}

fn paths(a: &mut Archive<Cursor<Vec<u8>>>) -> Vec<String> {
    let t = a.entry_table().unwrap();
    let v = t.table().unwrap().iter().map(|e| e.unwrap().path).collect();
    v
}

fn entries(a: &mut Archive<Cursor<Vec<u8>>>) -> Vec<Entry> {
    let t = a.entry_table().unwrap();
    let v = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    v
}

fn block_graph(a: &mut Archive<Cursor<Vec<u8>>>, i: usize) -> Graph {
    let b = a.index().blocks[i];
    let at = lpk_format::FrameLocation {
        offset: b.frame_offset,
        len: b.frame_len,
        sequence: b.sequence,
    };
    let f = a
        .read_frame_at(at, lpk_format::FrameKind::ChunkData)
        .unwrap();
    lpk_format::BlockHeader::parse(&f.payload, i, 0)
        .unwrap()
        .0
        .graph
}

/// Store for even blocks, zstd with a dictionary for odd ones.
struct Alternating {
    n: usize,
    zstd: ZstdTestEncoder,
}

impl BlockEncoder for Alternating {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let n = self.n;
        self.n += 1;
        if n.is_multiple_of(2) {
            StoreEncoder.encode(plain)
        } else {
            self.zstd.encode(plain)
        }
    }
}

#[test]
fn blocks_may_use_different_graphs() {
    let dict = common::trained_dictionary(5);
    let zstd = ZstdTestEncoder::new(3, 20, Some(dict.clone()));
    let window = zstd.graph().resources().window;
    assert!(window > 0);
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(Box::new(Alternating { n: 0, zstd }))).unwrap();
    let files: Vec<(String, Vec<u8>)> = (0..4)
        .map(|i| (format!("f{i}"), pattern(i as u64 + 1, 8192)))
        .collect();
    for (p, d) in &files {
        add(&mut w, p, d).unwrap();
    }
    let s = w.finish().unwrap();
    assert_eq!(s.blocks, 4);

    let mut a = open(out);
    let mut store = MemoryPriors::new();
    store.insert(dict.clone());
    a.set_priors(Box::new(store));
    // The prior list holds the dictionary once, however many blocks used it.
    assert_eq!(a.priors(), &[prior_id(&dict)]);
    assert_eq!(a.index().envelope.max_window, window);
    for i in 0..4 {
        let g = block_graph(&mut a, i);
        let want = if i % 2 == 0 {
            PrimitiveId::Store
        } else {
            PrimitiveId::Zstd
        };
        assert_eq!(g.steps[0].primitive, want, "block {i}");
    }
    a.verify().unwrap();
    for e in entries(&mut a) {
        let mut got = Vec::new();
        a.extract(&e, &mut got).unwrap();
        let want = &files.iter().find(|(p, _)| *p == e.path).unwrap().1;
        assert!(&got == want, "{}", e.path);
    }
}

/// A block whose graph names a record the archive does not have.
struct NamesRecord;

impl BlockEncoder for NamesRecord {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        Ok(Encoded {
            graph: Graph {
                steps: vec![Step {
                    primitive: PrimitiveId::Utf16,
                    params: vec![5],
                }],
            },
            bytes: plain.to_vec(),
            resources: lpk_format::GraphResources::default(),
        })
    }
}

#[test]
fn a_graph_naming_an_unlisted_record_fails_at_the_call() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(Box::new(NamesRecord))).unwrap();
    add(&mut w, "a", &pattern(1, 100)).unwrap();
    assert!(matches!(
        w.close_block(),
        Err(FormatError::RecordOutOfRange { .. })
    ));
    // The archive is abandoned.
    assert!(w.finish().is_err());
}

#[test]
fn adds_in_any_order_give_a_sorted_table() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(Box::new(StoreEncoder))).unwrap();
    add(&mut w, "c", b"cc").unwrap();
    add(&mut w, "a", b"aaa").unwrap();
    w.add_directory("b", EntryFlags::EMPTY, 0).unwrap();
    w.add_symlink("a2", EntryFlags::EMPTY, 0, b"a").unwrap();
    add(&mut w, "b/x", b"x").unwrap();
    assert!(matches!(
        add(&mut w, "a", b"again"),
        Err(FormatError::DuplicateEntry { path }) if path == "a"
    ));
    assert!(matches!(
        w.add_directory("c", EntryFlags::EMPTY, 0),
        Err(FormatError::DuplicateEntry { .. })
    ));
    w.finish().unwrap();
    let mut a = open(out);
    assert_eq!(paths(&mut a), ["a", "a2", "b", "b/x", "c"]);
    a.verify().unwrap();
    for e in entries(&mut a) {
        let mut got = Vec::new();
        a.extract(&e, &mut got).unwrap();
        let want: &[u8] = match e.path.as_str() {
            "a" => b"aaa",
            "c" => b"cc",
            "b/x" => b"x",
            _ => b"",
        };
        assert_eq!(got, want, "{}", e.path);
    }
}

#[test]
fn append_after_an_unsorted_first_generation_merges_by_path() {
    let mut first = Vec::new();
    let mut w = Writer::new(&mut first, options(Box::new(StoreEncoder))).unwrap();
    add(&mut w, "m", b"m1").unwrap();
    add(&mut w, "d", b"d1").unwrap();
    add(&mut w, "x", b"x1").unwrap();
    w.finish().unwrap();

    let existing = open(first.clone());
    let mut tail = Vec::new();
    let mut w = Writer::append(existing, &mut tail, options(Box::new(StoreEncoder)), None).unwrap();
    add(&mut w, "z", b"z2").unwrap();
    add(&mut w, "m", b"m2").unwrap();
    add(&mut w, "a", b"a2").unwrap();
    w.finish().unwrap();
    first.extend_from_slice(&tail);

    let mut a = open(first);
    assert_eq!(paths(&mut a), ["a", "d", "m", "x", "z"]);
    a.verify().unwrap();
    for e in entries(&mut a) {
        let mut got = Vec::new();
        a.extract(&e, &mut got).unwrap();
        let want: &[u8] = match e.path.as_str() {
            "a" => b"a2",
            "d" => b"d1",
            "m" => b"m2",
            "x" => b"x1",
            _ => b"z2",
        };
        assert_eq!(got, want, "{}", e.path);
    }
}

#[test]
fn close_block_ends_the_block_early() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(Box::new(StoreEncoder))).unwrap();
    // As the first call, and on an empty block: nothing.
    w.close_block().unwrap();
    add(&mut w, "a", &pattern(1, 4096)).unwrap();
    w.close_block().unwrap();
    w.close_block().unwrap();
    add(&mut w, "b", &pattern(2, 4096)).unwrap();
    let s = w.finish().unwrap();
    assert_eq!((s.chunks, s.blocks), (2, 2));
    let a = open(out);
    let blocks = &a.index().blocks;
    assert_eq!(blocks.len(), 2);
    assert_eq!((blocks[0].first_chunk, blocks[0].chunk_count), (0, 1));
    assert_eq!((blocks[1].first_chunk, blocks[1].chunk_count), (1, 1));
}

#[test]
fn close_block_alone_writes_no_block() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(Box::new(StoreEncoder))).unwrap();
    w.close_block().unwrap();
    assert_eq!(w.finish().unwrap().blocks, 0);
}
