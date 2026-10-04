//! Revision 1.1's writer and reader hooks (E2-5a): entries written in parts,
//! records added while writing, the header's `version_minor`, and the decode
//! context a reconstruction decoder reads its record and nested chunks through.
#![allow(clippy::unwrap_used)]

use lpk_format::{
    Archive, DecodeContext, Encoded, Entry, EntryFlags, FormatError, Graph, GraphResources,
    JpegRecord, PrimitiveDecoder, PrimitiveId, Record, RecordBody, Resources, Step, Writer,
    WriterOptions,
};
use std::io::Cursor;

fn options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16384,
        archive_id: [9; 16],
        ..WriterOptions::default()
    }
}

fn bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn open(bytes: Vec<u8>) -> Archive<Cursor<Vec<u8>>> {
    Archive::open(Cursor::new(bytes), &Resources::default()).unwrap()
}

fn entries(a: &mut Archive<Cursor<Vec<u8>>>) -> Vec<Entry> {
    let t = a.entry_table().unwrap();
    let v = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    v
}

fn extract(a: &mut Archive<Cursor<Vec<u8>>>, e: &Entry) -> Result<Vec<u8>, FormatError> {
    let mut out = Vec::new();
    a.extract(e, &mut out)?;
    Ok(out)
}

#[test]
fn parts_written_in_reverse_order_round_trip_in_file_order() {
    let file = bytes(1, 20_000);
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options()).unwrap();
    w.add_file("a", EntryFlags::EMPTY, 1, &mut &bytes(2, 100)[..])
        .unwrap();
    w.begin_entry("b", EntryFlags::EMPTY, 2).unwrap();
    // Refused while an entry is open.
    assert!(matches!(
        w.add_directory("c", EntryFlags::EMPTY, 0),
        Err(FormatError::BadOptions {
            reason: "an entry is open"
        })
    ));
    let mut ids = Vec::new();
    for (off, end) in [(15_000, 20_000), (6_000, 15_000), (0, 6_000)] {
        ids.push(w.add_part(off as u64, &mut &file[off..end]).unwrap());
        w.close_block().unwrap();
    }
    w.end_entry().unwrap();
    w.add_file("c", EntryFlags::EMPTY, 3, &mut &bytes(3, 10)[..])
        .unwrap();
    w.finish().unwrap();
    let mut a = open(out);
    assert_eq!(a.header().version.minor, 0);
    let es = entries(&mut a);
    let b = es.iter().find(|e| e.path == "b").unwrap();
    assert_eq!(b.size, 20_000);
    let want: Vec<u64> = ids.iter().rev().flatten().copied().collect();
    assert_eq!(b.chunks, want);
    assert!(
        b.chunks.windows(2).any(|w| w[0] > w[1]),
        "not in write order"
    );
    assert_eq!(extract(&mut a, b).unwrap(), file);
    a.verify().unwrap();
}

#[test]
fn parts_that_leave_a_gap_or_overlap_are_refused() {
    for parts in [vec![(0u64, 10usize), (11, 20)], vec![(0, 10), (5, 20)]] {
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, options()).unwrap();
        w.begin_entry("x", EntryFlags::EMPTY, 0).unwrap();
        for (off, len) in parts {
            w.add_part(off, &mut &bytes(0, len)[..]).unwrap();
        }
        assert!(matches!(
            w.end_entry(),
            Err(FormatError::BadOptions {
                reason: "entry parts"
            })
        ));
    }
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options()).unwrap();
    assert!(matches!(
        w.add_part(0, &mut &b"x"[..]),
        Err(FormatError::BadOptions {
            reason: "no entry is open"
        })
    ));
}

fn jpeg_graph(record: u8) -> Graph {
    Graph {
        steps: vec![Step {
            primitive: PrimitiveId::JpegReconstruct,
            params: vec![record],
        }],
    }
}

fn jpeg_record(original: &[u8], primary_len: u64, trailing_chunks: Vec<u64>) -> Record {
    Record::new(RecordBody::Jpeg(JpegRecord {
        original_len: original.len() as u64,
        primary_len,
        trailing: Vec::new(),
        nested_trailing_chunks: trailing_chunks,
        gainmaps: Vec::new(),
        lepton_version: 0,
        original_hash: *blake3::hash(original).as_bytes(),
    }))
}

/// A stand-in for a full reader's `jpeg-reconstruct`: the "Lepton stream" is
/// the primary image reversed; it reads its record and the nested trailing
/// chunks through the context and checks `original_hash`.
struct Reverse;

impl PrimitiveDecoder for Reverse {
    fn decode(&self, _: &[u8], _: &[u8], _: u64, _: &Resources) -> Result<Vec<u8>, FormatError> {
        Err(FormatError::UnimplementedPrimitive { id: 7 })
    }

    fn decode_in(
        &self,
        params: &[u8],
        input: &[u8],
        _expected: u64,
        _last: bool,
        _limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        let id = u64::from(params[0]);
        let RecordBody::Jpeg(j) = ctx.record(id)?.body else {
            return Err(FormatError::UnimplementedPrimitive { id: 7 });
        };
        let primary: Vec<u8> = input.iter().rev().copied().collect();
        let mut h = blake3::Hasher::new();
        h.update(&primary);
        for &c in &j.nested_trailing_chunks {
            h.update(&ctx.chunk(id, c)?);
        }
        if h.finalize().as_bytes() != &j.original_hash {
            return Err(FormatError::BadRecord {
                record: id,
                reason: "original_hash",
            });
        }
        Ok(primary)
    }
}

/// One "peeled" file: trailing part first, the record, then the primary as a
/// block of its own with the jpeg graph.
fn peeled_archive(minor: u16, primary: &[u8], trailing: &[u8]) -> Result<Vec<u8>, FormatError> {
    let original = [primary, trailing].concat();
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(), minor)?;
    w.begin_entry("p.jpg", EntryFlags::EMPTY, 5)?;
    let tc = w.add_part(primary.len() as u64, &mut &trailing[..])?;
    let id = w.add_record(jpeg_record(&original, primary.len() as u64, tc))?;
    assert_eq!(id, 0);
    let encoded = Encoded {
        graph: jpeg_graph(id as u8),
        bytes: primary.iter().rev().copied().collect(),
        resources: GraphResources::default(),
    };
    w.add_part_encoded(0, primary, encoded, 12_345)?;
    w.end_entry()?;
    w.finish()?;
    Ok(out)
}

#[test]
fn a_peeled_part_extracts_through_the_decode_context_and_sets_minor_1() {
    let primary = bytes(7, 9_000);
    let trailing = bytes(8, 5_000);
    let out = peeled_archive(1, &primary, &trailing).unwrap();
    let mut a = open(out.clone());
    assert_eq!(a.header().version.minor, 1);
    // The envelope counts the declared working memory beside the block buffer.
    let env = a.index().envelope;
    assert_eq!(env.decode_memory, env.max_block_plain + 12_345);
    let e = entries(&mut a).remove(0);
    assert_eq!(e.size, 14_000);
    // The 1.0 registry: the record is valid but the primitive has no decoder.
    assert!(matches!(
        extract(&mut a, &e),
        Err(FormatError::UnimplementedPrimitive { id: 7 })
    ));
    assert!(matches!(
        a.verify(),
        Err(FormatError::UnimplementedPrimitive { id: 7 })
    ));
    // With the hook the file comes back whole and verifies.
    a.registry_mut()
        .register(PrimitiveId::JpegReconstruct, Box::new(Reverse));
    assert_eq!(
        extract(&mut a, &e).unwrap(),
        [primary.clone(), trailing.clone()].concat()
    );
    let s = a.verify().unwrap();
    assert_eq!((s.entries, s.blocks), (1, 2));
}

#[test]
fn version_minor_rules() {
    let p = bytes(1, 100);
    let t = bytes(2, 10);
    assert!(matches!(
        peeled_archive(0, &p, &t),
        Err(FormatError::BadOptions {
            reason: "version_minor"
        })
    ));
    assert!(matches!(
        Writer::new_revision(Vec::new(), options(), 2),
        Err(FormatError::BadOptions {
            reason: "version_minor"
        })
    ));
    // Minor 1 declared but no 1.1 primitive used: the minor is the writer's
    // revision, not a fact about the blocks (spec section 2), so it is written
    // as declared.
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(), 1).unwrap();
    assert_eq!(w.version_minor(), 1);
    w.add_file("a", EntryFlags::EMPTY, 0, &mut &p[..]).unwrap();
    w.finish().unwrap();
    assert_eq!(open(out).header().version.minor, 1);
}

#[test]
fn add_record_ids_follow_the_options_records_and_bound_the_graphs() {
    let rec = jpeg_record(&bytes(1, 10), 10, Vec::new());
    let mut o = options();
    o.records = vec![rec.clone()];
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, o, 1).unwrap();
    w.begin_entry("x", EntryFlags::EMPTY, 0).unwrap();
    // Record 1 does not exist yet: the graph check refuses it.
    let e = Encoded {
        graph: jpeg_graph(1),
        bytes: vec![1],
        resources: GraphResources::default(),
    };
    assert!(matches!(
        w.add_part_encoded(0, &[1], e, 0),
        Err(FormatError::RecordOutOfRange {
            record: 1,
            count: 1
        })
    ));
    let mut out = Vec::new();
    let mut o = options();
    o.records = vec![rec.clone()];
    let mut w = Writer::new_revision(&mut out, o, 1).unwrap();
    assert_eq!(w.add_record(rec.clone()).unwrap(), 1);
    assert_eq!(w.add_record(rec).unwrap(), 2);
    w.finish().unwrap();
    let mut a = open(out);
    assert_eq!(a.records().unwrap().unwrap().len(), 3);
}

#[test]
fn the_writer_refuses_a_record_chunk_in_a_later_block() {
    // The record names a chunk that is not yet written when the primary's block
    // closes: the writer refuses the block and abandons the archive (the
    // reader-side refusal is tested in lpk-format's writer unit tests).
    let primary = bytes(3, 5_000);
    let trailing = bytes(4, 300);
    let original = [primary.clone(), trailing.clone()].concat();
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(), 1).unwrap();
    w.begin_entry("p", EntryFlags::EMPTY, 0).unwrap();
    // Chunk indices are given in order: the primary takes 0 and 1, the
    // trailing part 2, which lies in the later block.
    let id = w
        .add_record(jpeg_record(&original, 5_000, vec![2]))
        .unwrap();
    let e = Encoded {
        graph: jpeg_graph(id as u8),
        bytes: primary.iter().rev().copied().collect(),
        resources: GraphResources::default(),
    };
    assert!(matches!(
        w.add_part_encoded(0, &primary, e, 0),
        Err(FormatError::BadRecord {
            reason: "chunk order",
            ..
        })
    ));
    assert!(w.add_part(5_000, &mut &trailing[..]).is_err());
}
