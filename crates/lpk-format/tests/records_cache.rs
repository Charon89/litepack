//! The records cache of the reference reader (E2-10 task A item 2): the
//! `Records` frame is read and parsed once per opened archive and shared with
//! its forks, and a lookup of record `n` reports exactly what the walk from
//! record 0 reports.

#![allow(clippy::unwrap_used)]

mod common;

use common::{pattern, write_archive};
use lpk_format::{
    Archive, ArchiveChunks, BlockEncoder, ChunkSource, DecodeContext, Encoded, FormatError, Graph,
    GraphResources, PrimitiveDecoder, PrimitiveId, Record, RecordBody, RecordsWriter, Resources,
    Step, Utf16Record, WriterOptions,
};
use std::io::Cursor;
use std::sync::{Arc, Mutex};

fn utf16(n: u64) -> Record {
    Record::new(RecordBody::Utf16(Utf16Record {
        endian: 0,
        bom: 1,
        original_len: n,
        original_hash: [(n % 251) as u8; 32],
    }))
}

/// One reconstruction step naming record 0 (`utf16`, a primitive of
/// revision 1.0 that no shipped reader decodes).
struct Recon;

impl BlockEncoder for Recon {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        Ok(Encoded {
            graph: Graph {
                steps: vec![Step {
                    primitive: PrimitiveId::Utf16,
                    params: vec![0],
                }],
            },
            bytes: plain.to_vec(),
            resources: GraphResources::default(),
        })
    }
}

/// An identity decoder that looks up every id of `ids` through the context
/// and keeps what each lookup returned (as `Debug` text for errors).
struct Lookups {
    ids: Vec<u64>,
    seen: Arc<Mutex<Vec<Result<Record, String>>>>,
}

impl PrimitiveDecoder for Lookups {
    fn decode(
        &self,
        _: &[u8],
        input: &[u8],
        _: u64,
        _: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        Ok(input.to_vec())
    }

    fn decode_in(
        &self,
        _: &[u8],
        input: &[u8],
        _: u64,
        _: bool,
        _: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        let mut seen = self.seen.lock().unwrap();
        for &id in &self.ids {
            seen.push(ctx.record(id).map_err(|e| format!("{e:?}")));
        }
        Ok(input.to_vec())
    }
}

fn archive(records: Vec<Record>) -> Vec<u8> {
    let o = WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [8; 16],
        records,
        encoder: Box::new(Recon),
        ..WriterOptions::default()
    };
    write_archive(o, &[("a", pattern(1, 5000))])
}

type Seen = Arc<Mutex<Vec<Result<Record, String>>>>;

fn open_with_lookups(bytes: &[u8], ids: Vec<u64>) -> (Archive<Cursor<Vec<u8>>>, Seen) {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    let seen: Seen = Arc::default();
    a.registry_mut().register(
        PrimitiveId::Utf16,
        Box::new(Lookups {
            ids,
            seen: Arc::clone(&seen),
        }),
    );
    (a, seen)
}

/// What the walk from record 0 (`RecordsTable::get`, the reader's lookup
/// before the cache) gives for `id`.
fn walked(a: &mut Archive<Cursor<Vec<u8>>>, id: u64) -> Result<Record, String> {
    let owned = a.records().unwrap().unwrap();
    let count = owned.len();
    owned
        .table()
        .unwrap()
        .get(id)
        .and_then(|r| r.ok_or(FormatError::RecordOutOfRange { record: id, count }))
        .map_err(|e| format!("{e:?}"))
}

#[test]
fn many_records_are_parsed_once_and_every_lookup_answers() {
    const N: u64 = 100_000;
    let recs: Vec<Record> = (0..N).map(utf16).collect();
    let bytes = archive(recs.clone());
    let ids: Vec<u64> = (0..=N).collect();
    let (mut a, seen) = open_with_lookups(&bytes, ids);
    assert_eq!(a.records_parse_count(), 0);
    ArchiveChunks::new(&mut a).chunk(0).unwrap();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len() as u64, N + 1);
        for (i, got) in seen.iter().take(N as usize).enumerate() {
            assert_eq!(got.as_ref().unwrap(), &recs[i]);
        }
        assert_eq!(
            seen[N as usize].as_ref().unwrap_err(),
            &format!(
                "{:?}",
                FormatError::RecordOutOfRange {
                    record: N,
                    count: N
                }
            )
        );
    }
    // One read of the frame for the block's record check and every lookup.
    assert_eq!(a.records_parse_count(), 1);
    a.verify().unwrap();
    assert_eq!(a.records_parse_count(), 1);
}

#[test]
fn a_fork_shares_the_cache() {
    let recs: Vec<Record> = (0..50).map(utf16).collect();
    let bytes = archive(recs);
    // Parsed by the parent, then used by a fork.
    let (mut a, seen) = open_with_lookups(&bytes, vec![0, 49]);
    ArchiveChunks::new(&mut a).chunk(0).unwrap();
    assert_eq!(a.records_parse_count(), 1);
    let mut f = a.fork(Cursor::new(bytes.clone()));
    ArchiveChunks::new(&mut f).chunk(1).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 4);
    assert_eq!(f.records_parse_count(), 1);
    // A fork made before anything was parsed shares it too.
    let (a, _) = open_with_lookups(&bytes, vec![3]);
    let mut f1 = a.fork(Cursor::new(bytes.clone()));
    let mut f2 = a.fork(Cursor::new(bytes.clone()));
    ArchiveChunks::new(&mut f1).chunk(0).unwrap();
    ArchiveChunks::new(&mut f2).chunk(0).unwrap();
    assert_eq!(a.records_parse_count(), 1);
    assert_eq!(f2.records_parse_count(), 1);
}

/// Recompute the hash of the frame at `offset` (`len` bytes in all).
fn rehash_frame(bytes: &mut [u8], offset: u64, len: u64) {
    let (o, l) = (offset as usize, len as usize);
    let vl = (1..=10)
        .find(|&vl| lpk_format::varint::len((l - 36 - vl) as u64) == vl)
        .unwrap();
    let payload = bytes[o + 4 + vl..o + l - 32].to_vec();
    let h = blake3::hash(&payload);
    bytes[o + l - 32..o + l].copy_from_slice(h.as_bytes());
}

/// The archive of `count` records with record `bad` damaged by `damage`
/// (applied to the record's encoded bytes inside the frame payload); the
/// frame's own hash is made right again.
fn damaged(count: u64, bad: u64, damage: fn(&mut [u8])) -> Vec<u8> {
    let recs: Vec<Record> = (0..count).map(utf16).collect();
    let mut bytes = archive(recs.clone());
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let loc = a.index().records.unwrap();
    let (o, l) = (loc.offset as usize, loc.len as usize);
    let vl = (1..=10)
        .find(|&vl| lpk_format::varint::len((l - 36 - vl) as u64) == vl)
        .unwrap();
    let payload_at = o + 4 + vl;
    // The record `bad` starts after the count and the records before it.
    let before = RecordsWriter::encode(&recs[..bad as usize]);
    let lead = lpk_format::varint::len(count) - lpk_format::varint::len(bad);
    let start = payload_at + before.len() + lead;
    let one = RecordsWriter::encode(&recs[bad as usize..=bad as usize]);
    let rec_len = one.len() - lpk_format::varint::len(1);
    damage(&mut bytes[start..start + rec_len]);
    rehash_frame(&mut bytes, loc.offset, loc.len);
    bytes
}

#[test]
fn a_damaged_record_gives_the_walks_outcome_for_every_id() {
    // A body hash that fails, reserved flag bits, an unknown kind.
    let damages: [fn(&mut [u8]); 3] = [
        |r| {
            let n = r.len();
            r[n - 1] ^= 1;
        },
        |r| r[2] = 1,
        |r| r[0] = 0xEE,
    ];
    for damage in damages {
        let bytes = damaged(8, 3, damage);
        let ids: Vec<u64> = (0..10).collect();
        let (mut a, seen) = open_with_lookups(&bytes, ids.clone());
        // The block names record 0, which is intact; the decode runs.
        ArchiveChunks::new(&mut a).chunk(0).unwrap();
        let seen = seen.lock().unwrap().clone();
        let want: Vec<_> = ids.iter().map(|&id| walked(&mut a, id)).collect();
        assert_eq!(seen, want);
        assert!(seen[2].is_ok());
        assert!(seen[3].is_err());
        assert!(seen[9].as_ref().unwrap_err().contains("RecordOutOfRange"));
        assert_eq!(a.records_parse_count(), 1);
    }
}

#[test]
fn a_damaged_records_frame_is_read_again_on_each_lookup() {
    let recs: Vec<Record> = (0..4).map(utf16).collect();
    let mut bytes = archive(recs);
    let loc = Archive::open(Cursor::new(bytes.clone()), &Resources::default())
        .unwrap()
        .index()
        .records
        .unwrap();
    bytes[(loc.offset + loc.len - 40) as usize] ^= 1;
    let (mut a, _) = open_with_lookups(&bytes, vec![0]);
    for _ in 0..2 {
        assert!(matches!(
            ArchiveChunks::new(&mut a).chunk(0).unwrap_err(),
            FormatError::HashMismatch { kind: 3 }
        ));
    }
    // Nothing was cached from a frame that failed its hash.
    assert_eq!(a.records_parse_count(), 0);
}
