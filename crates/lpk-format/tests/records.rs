#![allow(clippy::unwrap_used)]

mod common;

use common::{pattern, write_archive};
use lpk_format::{
    Archive, ArchiveChunks, BlockEncoder, BlockHeader, ChunkSource, ContainerMember,
    ContainerRecord, DeflateRecord, Encoded, FormatError, Graph, GraphResources, PrimitiveId,
    Record, RecordBody, RecordKind, Resources, Step, Utf16Record, Writer, WriterOptions,
};
use std::io::Cursor;

fn utf16(n: u8) -> Record {
    Record::new(RecordBody::Utf16(Utf16Record {
        endian: 0,
        bom: 1,
        original_len: u64::from(n),
        original_hash: [n; 32],
    }))
}

fn deflate() -> Record {
    Record::new(RecordBody::Deflate(DeflateRecord {
        original_len: 9,
        plain_len: 20,
        corrections: vec![1, 2, 3],
        library: 0,
        original_hash: [7; 32],
    }))
}

fn options(records: Vec<Record>) -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [8; 16],
        records,
        ..WriterOptions::default()
    }
}

/// An encoder whose graph is one reconstruction step naming `record`.
struct Recon {
    primitive: PrimitiveId,
    record: u8,
}

impl BlockEncoder for Recon {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        Ok(Encoded {
            graph: Graph {
                steps: vec![Step {
                    primitive: self.primitive,
                    params: vec![self.record],
                }],
            },
            bytes: plain.to_vec(),
            resources: GraphResources::default(),
        })
    }
}

fn open(bytes: Vec<u8>) -> Archive<Cursor<Vec<u8>>> {
    Archive::open(Cursor::new(bytes), &Resources::default()).unwrap()
}

#[test]
fn an_archive_without_records_has_none() {
    let bytes = write_archive(options(vec![]), &[("a", pattern(1, 5000))]);
    let mut a = open(bytes);
    assert!(a.index().records.is_none());
    assert!(a.records().unwrap().is_none());
    a.verify().unwrap();
}

#[test]
fn an_archive_with_records_returns_the_table() {
    let recs = vec![utf16(1), deflate(), utf16(3)];
    let bytes = write_archive(options(recs.clone()), &[("a", pattern(1, 9000))]);
    let mut a = open(bytes);
    let loc = a.index().records.unwrap();
    // The frame sits between the entry table and the index.
    assert!(loc.offset > a.index().entry_table.offset);
    assert!(loc.offset + loc.len <= a.trailer().index_offset);
    let owned = a.records().unwrap().unwrap();
    assert_eq!(owned.len(), 3);
    assert!(!owned.is_empty());
    let table = owned.table().unwrap();
    table.validate().unwrap();
    let got: Vec<Record> = table.iter().map(|r| r.unwrap()).collect();
    assert_eq!(got, recs);
    assert_eq!(got[1].kind, RecordKind::Deflate);
    // The archive still verifies, and the envelope admits the frame.
    a.verify().unwrap();
    assert!(a.index().envelope.max_frame_payload >= loc.len - 37);
}

#[test]
fn a_damaged_records_frame_is_a_hash_mismatch_found_by_verify_and_the_archive_still_opens() {
    let bytes = write_archive(options(vec![utf16(1)]), &[("a", pattern(1, 5000))]);
    let loc = open(bytes.clone()).index().records.unwrap();
    let mut bad = bytes;
    bad[(loc.offset + loc.len - 40) as usize] ^= 1;
    let mut a = open(bad);
    assert!(matches!(
        a.records().unwrap_err(),
        FormatError::HashMismatch { kind: 3 }
    ));
    // Whole-archive verification reads the frame whenever the index lists it.
    assert!(matches!(
        a.verify().unwrap_err(),
        FormatError::HashMismatch { kind: 3 }
    ));
    // A block that names no record never reads it: chunks still extract.
    let mut src = ArchiveChunks::new(&mut a);
    assert_eq!(src.chunk(0).unwrap().len(), 4096);
}

#[test]
fn the_writer_refuses_a_graph_naming_a_missing_record() {
    for (records, id) in [(vec![], 0u8), (vec![utf16(1)], 1)] {
        let mut o = options(records.clone());
        o.encoder = Box::new(Recon {
            primitive: PrimitiveId::Utf16,
            record: id,
        });
        let mut out = Vec::new();
        // The graph is checked when the block closes.
        let mut w = Writer::new(&mut out, o).unwrap();
        w.add_file("a", lpk_format::EntryFlags::EMPTY, 0, &mut &b"data"[..])
            .unwrap();
        let e = w.close_block().unwrap_err();
        assert!(
            matches!(e, FormatError::RecordOutOfRange { record, count } if record == u64::from(id) && count == records.len() as u64),
            "{e:?}"
        );
    }
}

#[test]
fn a_reconstruction_block_passes_the_record_check_and_stops_at_the_missing_decoder() {
    let mut o = options(vec![utf16(1), deflate()]);
    o.encoder = Box::new(Recon {
        primitive: PrimitiveId::DeflateReconstruct,
        record: 1,
    });
    let bytes = write_archive(o, &[("a", pattern(1, 5000))]);
    let mut a = open(bytes);
    assert_eq!(a.records().unwrap().unwrap().len(), 2);
    assert!(matches!(
        a.verify().unwrap_err(),
        FormatError::UnimplementedPrimitive { id: 8 }
    ));
}

/// An archive of one 5000-byte file (chunks of 4096 and 904 bytes, one
/// block) whose blocks name `record` of `records` with `primitive`.
fn recon_archive(primitive: PrimitiveId, record: u8, records: Vec<Record>) -> Vec<u8> {
    let mut o = options(records);
    o.encoder = Box::new(Recon { primitive, record });
    write_archive(o, &[("a", pattern(1, 5000))])
}

#[test]
fn a_block_naming_a_record_the_frame_lacks_is_out_of_range_end_to_end() {
    let mut bytes = recon_archive(PrimitiveId::Utf16, 0, vec![utf16(1)]);
    // Patch the block's record id (graph: count, id u16, flags, params_len,
    // params) and recompute the block frame's hash.
    let loc = open(bytes.clone()).index().blocks[0];
    let off = loc.frame_offset as usize;
    let mut rest = &bytes[off + 4..];
    let plen = lpk_format::varint::read(&mut rest).unwrap() as usize;
    let payload_at = off + 4 + (bytes[off + 4..].len() - rest.len());
    bytes[payload_at + 5] = 5;
    let hash = *blake3::hash(&bytes[payload_at..payload_at + plen]).as_bytes();
    bytes[payload_at + plen..payload_at + plen + 32].copy_from_slice(&hash);
    let mut a = open(bytes);
    let want = |e: FormatError| {
        assert!(
            matches!(
                e,
                FormatError::RecordOutOfRange {
                    record: 5,
                    count: 1
                }
            ),
            "{e:?}"
        )
    };
    want(a.verify().unwrap_err());
    // The block reader raises it too, when the block is read.
    want(ArchiveChunks::new(&mut a).chunk(0).unwrap_err());
}

#[test]
fn the_block_header_check_rejects_an_id_at_the_count() {
    let payload = BlockHeader {
        graph: Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Utf16,
                params: vec![1],
            }],
        },
        plain_len: 0,
        encoded_len: 0,
    }
    .encode();
    assert!(matches!(
        BlockHeader::parse(&payload, 0, 1).unwrap_err(),
        FormatError::RecordOutOfRange {
            record: 1,
            count: 1
        }
    ));
    BlockHeader::parse(&payload, 0, 2).unwrap();
}

fn container(len: u64, chunks: Vec<u64>) -> Record {
    Record::new(RecordBody::Container(ContainerRecord {
        format: 0,
        original_len: len,
        framing: vec![],
        members: vec![ContainerMember {
            offset: 0,
            len,
            chunks,
        }],
        original_hash: [1; 32],
    }))
}

#[test]
fn verify_checks_the_chunks_a_record_names() {
    let check = |len: u64, chunks: Vec<u64>| {
        let bytes = recon_archive(
            PrimitiveId::ContainerReconstruct,
            0,
            vec![container(len, chunks)],
        );
        open(bytes).verify().unwrap_err()
    };
    // Chunk 99 does not exist (the archive has two chunks).
    assert!(matches!(
        check(10, vec![99]),
        FormatError::ChunkIndexOutOfRange { chunk: 99, len: 2 }
    ));
    // Chunk 1 holds 904 bytes, not 10.
    assert!(matches!(
        check(10, vec![1]),
        FormatError::BadRecord {
            record: 0,
            reason: "chunk lengths"
        }
    ));
    // The only block names the record, so it cannot hold chunks of the record.
    for chunks in [vec![0, 1], vec![1]] {
        let len = if chunks.len() == 2 { 5000 } else { 904 };
        assert!(matches!(
            check(len, chunks),
            FormatError::BadRecord {
                record: 0,
                reason: "chunk order"
            }
        ));
    }
}

#[test]
fn the_writer_refuses_invalid_records_and_mismatched_kinds() {
    let bad = |r: Record| {
        let mut out = Vec::new();
        Writer::new(&mut out, options(vec![r])).err().unwrap()
    };
    let RecordBody::Deflate(mut d) = deflate().body else {
        unreachable!()
    };
    d.library = 3;
    assert!(matches!(
        bad(Record::new(RecordBody::Deflate(d))),
        FormatError::BadRecord {
            record: 0,
            reason: "library"
        }
    ));
    let mut r = deflate();
    r.kind = RecordKind::Jpeg;
    assert!(matches!(
        bad(r),
        FormatError::BadOptions {
            reason: "record kind"
        }
    ));
}

#[test]
fn an_append_may_not_change_or_drop_old_records() {
    let old = vec![utf16(1), deflate()];
    let bytes = write_archive(options(old.clone()), &[("a", pattern(1, 5000))]);
    let try_append = |records: Vec<Record>| {
        let a = open(bytes.clone());
        let mut tail = Vec::new();
        Writer::append(a, &mut tail, options(records), None).map(|_| ())
    };
    assert!(matches!(
        try_append(vec![utf16(2), deflate()]),
        Err(FormatError::BadRecord { record: 0, .. })
    ));
    assert!(matches!(
        try_append(vec![utf16(1)]),
        Err(FormatError::RecordOutOfRange {
            record: 1,
            count: 1
        })
    ));
    // Extending the list keeps the old records in place.
    let a = open(bytes.clone());
    let mut tail = Vec::new();
    let mut w = Writer::append(
        a,
        &mut tail,
        options(vec![utf16(1), deflate(), utf16(3)]),
        None,
    )
    .unwrap();
    w.add_file(
        "b",
        lpk_format::EntryFlags::EMPTY,
        0,
        &mut &pattern(2, 5000)[..],
    )
    .unwrap();
    w.finish().unwrap();
    let mut all = bytes.clone();
    all.extend(tail);
    let mut a = open(all);
    assert_eq!(a.records().unwrap().unwrap().len(), 3);
    a.verify().unwrap();
}
