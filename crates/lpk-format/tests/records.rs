#![allow(clippy::unwrap_used)]

mod common;

use common::{pattern, write_archive};
use lpk_format::{
    Archive, BlockEncoder, DeflateRecord, FormatError, Graph, GraphResources, PrimitiveId, Record,
    RecordBody, RecordKind, Resources, Step, Utf16Record, Writer, WriterOptions,
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
    fn graph(&self) -> Graph {
        Graph {
            steps: vec![Step {
                primitive: self.primitive,
                params: vec![self.record],
            }],
        }
    }
    fn encode(&mut self, plain: &[u8]) -> Result<Vec<u8>, FormatError> {
        Ok(plain.to_vec())
    }
    fn resources(&self) -> GraphResources {
        GraphResources::default()
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
fn a_damaged_records_frame_is_a_hash_mismatch_and_the_archive_still_opens() {
    let bytes = write_archive(options(vec![utf16(1)]), &[("a", pattern(1, 5000))]);
    let loc = open(bytes.clone()).index().records.unwrap();
    let mut bad = bytes;
    bad[(loc.offset + loc.len - 40) as usize] ^= 1;
    let mut a = open(bad);
    assert!(matches!(
        a.records().unwrap_err(),
        FormatError::HashMismatch { kind: 3 }
    ));
    // Blocks that name no record do not read the frame.
    a.verify().unwrap();
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
        let e = Writer::new(&mut out, o).err().unwrap();
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

#[test]
fn record_ids_in_range_pass_and_the_header_check_rejects_out_of_range() {
    // Write with two records, then swap the records frame for a one-record
    // frame of the same length class: a block that names id 1 now fails.
    let mut o = options(vec![utf16(1), utf16(2)]);
    o.encoder = Box::new(Recon {
        primitive: PrimitiveId::Utf16,
        record: 1,
    });
    let two = write_archive(o, &[("a", pattern(1, 5000))]);
    let mut o = options(vec![utf16(1)]);
    o.encoder = Box::new(Recon {
        primitive: PrimitiveId::Utf16,
        record: 0,
    });
    // Same writer, one record: id 0 is in range and the block header parses.
    let one = write_archive(o, &[("a", pattern(1, 5000))]);
    let mut a = open(one);
    assert!(matches!(
        a.verify().unwrap_err(),
        FormatError::UnimplementedPrimitive { id: 11 }
    ));
    // The two-record archive is fine as written.
    let mut a = open(two);
    assert!(matches!(
        a.verify().unwrap_err(),
        FormatError::UnimplementedPrimitive { id: 11 }
    ));
    // A header naming id 1 against a count of 1 is `RecordOutOfRange`.
    let payload = lpk_format::BlockHeader {
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
        lpk_format::BlockHeader::parse(&payload, 0, 1).unwrap_err(),
        FormatError::RecordOutOfRange {
            record: 1,
            count: 1
        }
    ));
}
