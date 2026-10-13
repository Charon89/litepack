//! Reconstruction records in an appended generation (spec section 12 and
//! "Layout of an append"): a record added by an append follows the old
//! records in a new `Records` frame; an append that adds none keeps the old
//! frame; a record's part never deduplicates against a chunk of an old
//! reconstruction block, and a record naming such a chunk is refused.
#![allow(clippy::unwrap_used)]

mod common;

use common::journal::{build_journal, options as journal_options, JOURNAL_VECTOR};
use common::sealed::seal_options;
use common::{pattern, vectors_dir, write_archive};
use lpk_format::{
    Archive, Credentials, DecodeContext, DeflateRecord, Encoded, EntryFlags, FixedChunker,
    FormatError, Graph, GraphResources, JpegRecord, PrimitiveDecoder, PrimitiveId, Record,
    RecordBody, Resources, Step, Suite, Utf16Record, Writer, WriterOptions,
};
use std::io::Cursor;

fn options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [9; 16],
        ..WriterOptions::default()
    }
}

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

/// A `jpeg` record whose primary is `primary_len` bytes and whose trailing
/// data lies in `chunks`.
fn jpeg(original_len: u64, primary_len: u64, chunks: Vec<u64>) -> Record {
    Record::new(RecordBody::Jpeg(JpegRecord {
        original_len,
        primary_len,
        trailing: Vec::new(),
        nested_trailing_chunks: chunks,
        gainmaps: Vec::new(),
        lepton_version: 0,
        original_hash: [0; 32],
    }))
}

/// A block of its own: `primary` reversed, under a `jpeg-reconstruct` step
/// naming `record`.
fn encoded(record: u64, primary: &[u8]) -> Encoded {
    let mut params = Vec::new();
    lpk_format::varint::write(&mut params, record).unwrap();
    Encoded {
        graph: Graph {
            steps: vec![Step {
                primitive: PrimitiveId::JpegReconstruct,
                params,
            }],
        },
        bytes: primary.iter().rev().copied().collect(),
        resources: GraphResources::default(),
    }
}

/// A stand-in `jpeg-reconstruct` decoder: the primary is the stream reversed;
/// every nested trailing chunk is read through the context, as the real one
/// reads them.
struct Reverse;

impl PrimitiveDecoder for Reverse {
    fn decode(&self, _: &[u8], _: &[u8], _: u64, _: &Resources) -> Result<Vec<u8>, FormatError> {
        Err(FormatError::UnimplementedPrimitive { id: 7 })
    }

    fn decode_in(
        &self,
        params: &[u8],
        input: &[u8],
        _: u64,
        _: bool,
        _: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        let id = lpk_format::varint::read(&mut &params[..])?;
        let RecordBody::Jpeg(j) = ctx.record(id)?.body else {
            return Err(FormatError::UnimplementedPrimitive { id: 7 });
        };
        for &c in &j.nested_trailing_chunks {
            ctx.chunk(id, c)?;
        }
        Ok(input.iter().rev().copied().collect())
    }
}

fn open(bytes: &[u8]) -> Archive<Cursor<Vec<u8>>> {
    open_with(bytes, None)
}

fn open_with(bytes: &[u8], creds: Option<&Credentials>) -> Archive<Cursor<Vec<u8>>> {
    let mut a =
        Archive::open_with(Cursor::new(bytes.to_vec()), &Resources::default(), creds).unwrap();
    a.registry_mut()
        .register(PrimitiveId::JpegReconstruct, Box::new(Reverse));
    a
}

/// A revision 1.1 archive (header minor 1, so an append may use a
/// reconstruction primitive) of `files`, with `records`.
fn write_rev1(records: Vec<Record>, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let opts = WriterOptions {
        records,
        ..options()
    };
    let mut w = Writer::new_revision(&mut out, opts, 1).unwrap();
    for (path, data) in files {
        w.add_file(path, EntryFlags::EMPTY, 0, &mut &data[..])
            .unwrap();
    }
    w.finish().unwrap();
    out
}

fn records_of(bytes: &[u8]) -> Vec<Record> {
    let mut a = open(bytes);
    let owned = a.records().unwrap().unwrap();
    owned.table().unwrap().iter().map(|r| r.unwrap()).collect()
}

/// Append a generation to `old` through `f`; the new bytes are concatenated.
fn append(
    old: &[u8],
    opts: WriterOptions,
    f: impl FnOnce(&mut Writer<&mut Vec<u8>>) -> Result<(), FormatError>,
) -> Result<Vec<u8>, FormatError> {
    append_with(old, opts, None, f)
}

fn append_with(
    old: &[u8],
    opts: WriterOptions,
    creds: Option<&Credentials>,
    f: impl FnOnce(&mut Writer<&mut Vec<u8>>) -> Result<(), FormatError>,
) -> Result<Vec<u8>, FormatError> {
    let a = open_with(old, creds);
    let mut tail = Vec::new();
    let mut w = Writer::append(a, &mut tail, opts, creds)?;
    f(&mut w)?;
    w.finish()?;
    let mut all = old.to_vec();
    all.extend(tail);
    Ok(all)
}

/// Generation 0 of the nesting tests: entry `a` is a peeled file whose primary
/// (`p`, chunk 0) is block 0, a reconstruction block naming record 0; entry
/// `z` (`q`, chunk 1) is block 1, a plain block.
fn gen0_with_recon_block(p: &[u8], q: &[u8]) -> Vec<u8> {
    gen0_with_recon_block_opts(p, q, options())
}

fn gen0_with_recon_block_opts(p: &[u8], q: &[u8], opts: WriterOptions) -> Vec<u8> {
    let creds = opts.seal.as_ref().map(|s| s.credentials.clone());
    let mut out = Vec::new();
    let chunker = Box::new(FixedChunker::new(4096));
    let mut w = Writer::new_revision_with_chunker(&mut out, opts, 1, chunker).unwrap();
    w.begin_entry("a", EntryFlags::EMPTY, 0).unwrap();
    let id = w
        .add_record(jpeg(p.len() as u64, p.len() as u64, Vec::new()))
        .unwrap();
    assert_eq!(id, 0);
    w.add_part_encoded(0, p, encoded(id, p), 0).unwrap();
    w.end_entry().unwrap();
    w.add_file("z", EntryFlags::EMPTY, 0, &mut &q[..]).unwrap();
    w.finish().unwrap();
    let a = open_with(&out, creds.as_ref());
    assert_eq!(a.index().blocks.len(), 2);
    assert_eq!(a.chunks().len(), 2);
    out
}

/// The nesting rules over an encrypted archive: the old block's graph is read
/// through the archive key (the frame unsealed), with the same outcomes as in
/// a plain archive.
#[test]
fn sealed_old_reconstruction_blocks_are_read_through_the_key() {
    let (p, q) = (vec![7u8; 1000], pattern(3, 900));
    let opts = || WriterOptions {
        block_size: 16 * 1024,
        ..seal_options(Suite::AesGcm, false, "nested", None)
    };
    let creds = opts().seal.unwrap().credentials;
    let g0 = gen0_with_recon_block_opts(&p, &q, opts());
    assert!(open_with(&g0, Some(&creds)).is_encrypted());
    // A record part equal to chunk 0 (a reconstruction block) is stored anew;
    // one equal to chunk 1 (a plain block) is referenced.
    let seen = std::cell::Cell::new((Vec::new(), Vec::new()));
    let g1 = append_with(&g0, opts(), Some(&creds), |w| {
        w.begin_entry("b", EntryFlags::EMPTY, 0)?;
        let again = w.add_part(1000, &mut &p[..])?;
        let reused = w.add_part(2000, &mut &q[..])?;
        seen.set((again.clone(), reused.clone()));
        let id = w.add_record(jpeg(2900, 1000, [again, reused].concat()))?;
        w.add_part_encoded(0, &p, encoded(id, &p), 0)?;
        w.end_entry()
    })
    .unwrap();
    assert_eq!(seen.into_inner(), (vec![2], vec![1]));
    let mut a = open_with(&g1, Some(&creds));
    assert_eq!(a.chunks().len(), 4);
    a.verify().unwrap();
    let t = a.entry_table().unwrap();
    let b = t
        .table()
        .unwrap()
        .iter()
        .map(|e| e.unwrap())
        .nth(1)
        .unwrap();
    let mut got = Vec::new();
    a.extract(&b, &mut got).unwrap();
    assert_eq!(got, [p.clone(), p.clone(), q].concat());
    // A record naming chunk 0 is refused when the block that names it closes.
    let e = append_with(&g0, opts(), Some(&creds), |w| {
        w.begin_entry("b", EntryFlags::EMPTY, 0)?;
        let id = w.add_record(jpeg(2000, 1000, vec![0]))?;
        w.add_part_encoded(0, &p, encoded(id, &p), 0)?;
        w.end_entry()
    })
    .unwrap_err();
    assert!(
        matches!(
            e,
            FormatError::BadRecord {
                record: 1,
                reason: "nested record"
            }
        ),
        "{e:?}"
    );
}

#[test]
fn an_append_adds_a_record_after_the_old_ones_and_a_later_append_keeps_that_frame() {
    let old = vec![utf16(1), deflate()];
    let g0 = write_rev1(old.clone(), &[("a", pattern(1, 5000))]);
    let frame0 = open(&g0).index().records.unwrap();
    let p = vec![7u8; 1000];
    // Generation 1 adds a peeled entry: its record follows the old two, and
    // the block that names it is a block of this append.
    let g1 = append(&g0, options(), |w| {
        w.begin_entry("b", EntryFlags::EMPTY, 0)?;
        let id = w.add_record(jpeg(1000, 1000, Vec::new()))?;
        assert_eq!(id, 2);
        w.add_part_encoded(0, &p, encoded(id, &p), 0)?;
        w.end_entry()
    })
    .unwrap();
    let want = vec![utf16(1), deflate(), jpeg(1000, 1000, Vec::new())];
    assert_eq!(records_of(&g1), want);
    let mut a = open(&g1);
    let frame1 = a.index().records.unwrap();
    assert!(
        frame1.offset >= g0.len() as u64,
        "a new frame in the append"
    );
    assert_ne!(frame1, frame0);
    a.verify().unwrap();
    let mut got = Vec::new();
    let t = a.entry_table().unwrap();
    let b = t
        .table()
        .unwrap()
        .iter()
        .map(|e| e.unwrap())
        .nth(1)
        .unwrap();
    assert_eq!(b.path, "b");
    a.extract(&b, &mut got).unwrap();
    assert_eq!(got, p);
    // Before a record is added the writer counts the old ones: a block naming
    // id 2 is out of range; after one is added, id 3 is.
    let e = append(&g0, options(), |w| {
        w.begin_entry("c", EntryFlags::EMPTY, 0)?;
        w.add_part_encoded(0, &p, encoded(2, &p), 0)?;
        w.end_entry()
    })
    .unwrap_err();
    assert!(
        matches!(
            e,
            FormatError::RecordOutOfRange {
                record: 2,
                count: 2
            }
        ),
        "{e:?}"
    );
    let e = append(&g0, options(), |w| {
        w.begin_entry("c", EntryFlags::EMPTY, 0)?;
        w.add_record(jpeg(1000, 1000, Vec::new()))?;
        w.add_part_encoded(0, &p, encoded(3, &p), 0)?;
        w.end_entry()
    })
    .unwrap_err();
    assert!(
        matches!(
            e,
            FormatError::RecordOutOfRange {
                record: 3,
                count: 3
            }
        ),
        "{e:?}"
    );
    // Generation 2 adds no record: its index keeps generation 1's frame.
    let g2 = append(&g1, options(), |w| {
        w.add_file("d", EntryFlags::EMPTY, 0, &mut &pattern(4, 3000)[..])
    })
    .unwrap();
    let mut a = open(&g2);
    assert_eq!(a.index().records.unwrap(), frame1);
    assert_eq!(a.generation(), 2);
    assert_eq!(records_of(&g2), want);
    a.verify().unwrap();
    // Records given to the append's options still replace the frame, restating
    // the old ones; a changed old record is refused as before.
    let g3 = append(
        &g2,
        WriterOptions {
            records: vec![utf16(1), deflate(), jpeg(1000, 1000, Vec::new()), utf16(4)],
            ..options()
        },
        |w| w.add_file("e", EntryFlags::EMPTY, 0, &mut &pattern(5, 100)[..]),
    )
    .unwrap();
    assert_eq!(records_of(&g3).len(), 4);
    assert_ne!(open(&g3).index().records.unwrap(), frame1);
    open(&g3).verify().unwrap();
    let e = append(
        &g2,
        WriterOptions {
            records: vec![utf16(2), deflate(), jpeg(1000, 1000, Vec::new())],
            ..options()
        },
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(
        matches!(e, FormatError::BadRecord { record: 0, .. }),
        "{e:?}"
    );
}

#[test]
fn an_append_without_records_writes_the_bytes_it_wrote_before() {
    // The committed three-generation vector was written before records were
    // loaded into an append: the loading changes nothing an append writes.
    let (snaps, _) = build_journal(&|| journal_options(0), None);
    let committed = std::fs::read(vectors_dir().join(JOURNAL_VECTOR)).unwrap();
    assert!(snaps[2] == committed, "journal-3gen.lpk differs");
    // With old records: the appended bytes hold no `Records` frame, and the
    // index points at generation 0's.
    let g0 = write_archive(
        WriterOptions {
            records: vec![utf16(1), deflate()],
            ..options()
        },
        &[("a", pattern(1, 5000))],
    );
    let frame0 = open(&g0).index().records.unwrap();
    let g1 = append(&g0, options(), |w| {
        w.add_file("b", EntryFlags::EMPTY, 0, &mut &pattern(2, 5000)[..])
    })
    .unwrap();
    let a = open(&g1);
    assert_eq!(a.index().records.unwrap(), frame0);
    let mut r = Cursor::new(&g1[g0.len()..]);
    let mut frames = 0;
    while let Some(f) = lpk_format::Frame::read(&mut r, &Default::default()).unwrap() {
        assert!(
            !matches!(f, lpk_format::ReadFrame::Known(ref k) if k.kind == lpk_format::FrameKind::Records),
            "a Records frame in the append"
        );
        frames += 1;
    }
    assert!(frames >= 4, "block, entry table, index, trailer");
    assert_eq!(records_of(&g1), vec![utf16(1), deflate()]);
}

#[test]
fn a_record_part_equal_to_a_chunk_of_an_old_reconstruction_block_is_stored_anew() {
    let (p, q) = (vec![7u8; 1000], pattern(3, 900));
    let g0 = gen0_with_recon_block(&p, &q);
    let seen = std::cell::Cell::new((Vec::new(), Vec::new()));
    let g1 = append(&g0, options(), |w| {
        w.begin_entry("b", EntryFlags::EMPTY, 0)?;
        // `p` equals chunk 0, which lies in a reconstruction block: a record's
        // part may not reference it, so it is stored again. `q` equals chunk 1
        // of a plain block and is referenced.
        let again = w.add_part(1000, &mut &p[..])?;
        let reused = w.add_part(2000, &mut &q[..])?;
        seen.set((again.clone(), reused.clone()));
        let id = w.add_record(jpeg(2900, 1000, [again, reused].concat()))?;
        w.add_part_encoded(0, &p, encoded(id, &p), 0)?;
        w.end_entry()
    })
    .unwrap();
    let (again, reused) = seen.into_inner();
    assert_eq!(again, vec![2], "stored anew, after the two old chunks");
    assert_eq!(reused, vec![1], "the plain block's chunk is referenced");
    let mut a = open(&g1);
    assert_eq!(a.chunks().len(), 4, "p again, then the new primary");
    a.verify().unwrap();
    let t = a.entry_table().unwrap();
    let b = t
        .table()
        .unwrap()
        .iter()
        .map(|e| e.unwrap())
        .nth(1)
        .unwrap();
    assert_eq!(b.path, "b");
    let mut got = Vec::new();
    a.extract(&b, &mut got).unwrap();
    assert_eq!(got, [p.clone(), p, q].concat());
    assert_eq!(records_of(&g1).len(), 2);
}

#[test]
fn a_block_naming_a_record_with_a_chunk_in_an_old_reconstruction_block_is_refused() {
    let (p, q) = (vec![7u8; 1000], pattern(3, 900));
    let g0 = gen0_with_recon_block(&p, &q);
    // Record 1 names old chunk 0, which lies in a reconstruction block: the
    // block closing with a graph that names record 1 is refused, as it is for
    // a block of the same write.
    let e = append(&g0, options(), |w| {
        w.begin_entry("b", EntryFlags::EMPTY, 0)?;
        let id = w.add_record(jpeg(2000, 1000, vec![0]))?;
        assert_eq!(id, 1);
        w.add_part_encoded(0, &p, encoded(id, &p), 0)?;
        w.end_entry()
    })
    .unwrap_err();
    assert!(
        matches!(
            e,
            FormatError::BadRecord {
                record: 1,
                reason: "nested record"
            }
        ),
        "{e:?}"
    );
    // Old chunk 1 lies in a plain block: a record may name it.
    let g1 = append(&g0, options(), |w| {
        w.begin_entry("b", EntryFlags::EMPTY, 0)?;
        let id = w.add_record(jpeg(1900, 1000, vec![1]))?;
        w.add_part_encoded(0, &p, encoded(id, &p), 0)?;
        w.add_part(1000, &mut &q[..])?;
        w.end_entry()
    })
    .unwrap();
    let mut a = open(&g1);
    a.verify().unwrap();
    assert_eq!(a.chunks().len(), 3, "q was referenced, not stored");
}
