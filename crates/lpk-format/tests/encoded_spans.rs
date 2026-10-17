//! E2-10 task A items 3 and 4: given blocks over several spans of an entry
//! (`Writer::add_parts_encoded`) and the read-only dedup query
//! (`Writer::part_known`).
#![allow(clippy::unwrap_used)]

use lpk_format::{
    Archive, Entry, EntryFlags, FormatError, StoreEncoder, Writer, WriterOptions, WriterSummary,
};
use std::io::Cursor;

fn options(dedup: bool) -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16384,
        archive_id: [9; 16],
        dedup,
        ..WriterOptions::default()
    }
}

/// Pseudo-random bytes (no repeats inside).
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

fn open(bytes: Vec<u8>) -> Archive<Cursor<Vec<u8>>> {
    Archive::open(Cursor::new(bytes), &lpk_format::Resources::default()).unwrap()
}

fn entries(a: &mut Archive<Cursor<Vec<u8>>>) -> Vec<Entry> {
    let t = a.entry_table().unwrap();
    let v = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    v
}

fn extract(a: &mut Archive<Cursor<Vec<u8>>>, e: &Entry) -> Vec<u8> {
    let mut out = Vec::new();
    a.extract(e, &mut out).unwrap();
    out
}

const FILE_LEN: u64 = 30_000;
/// Three streams of one block (5000 + 7000 + 3000 bytes, under the
/// 16384-byte block) with framing between and around them.
const SPANS: [(u64, u64); 3] = [(1_000, 5_000), (9_000, 7_000), (20_000, 3_000)];

/// The framing: every byte of the file outside `SPANS`.
fn gaps() -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (o, l) in SPANS {
        out.push((at, o - at));
        at = o + l;
    }
    out.push((at, FILE_LEN - at));
    out
}

fn slice(file: &[u8], (o, l): (u64, u64)) -> &[u8] {
    &file[o as usize..(o + l) as usize]
}

fn spans_plain(file: &[u8]) -> Vec<u8> {
    SPANS
        .iter()
        .flat_map(|&s| slice(file, s).to_vec())
        .collect()
}

/// Write `file` as entry `path`: the first two framing gaps, then the
/// multi-span block, then the other gaps. Returns the block's chunks.
fn write_peeled(w: &mut Writer<&mut Vec<u8>>, path: &str, file: &[u8]) -> Vec<u64> {
    w.begin_entry(path, EntryFlags::EMPTY, 0).unwrap();
    let g = gaps();
    for &gap in &g[..2] {
        w.add_part(gap.0, &mut slice(file, gap)).unwrap();
    }
    let plain = spans_plain(file);
    let chunks = w
        .add_parts_encoded(&SPANS, &plain, StoreEncoder::wrap(plain.clone()), 77)
        .unwrap();
    for &gap in &g[2..] {
        w.add_part(gap.0, &mut slice(file, gap)).unwrap();
    }
    w.end_entry().unwrap();
    chunks
}

/// The file offsets each chunk of `e` covers.
fn chunk_ranges(a: &Archive<Cursor<Vec<u8>>>, e: &Entry) -> Vec<(u64, u64, u64)> {
    let mut at = 0;
    e.chunks
        .iter()
        .map(|&c| {
            let len = a.chunks().record(c).unwrap().plain_len;
            at += len;
            (c, at - len, at)
        })
        .collect()
}

#[test]
fn a_multi_span_block_interleaves_with_plain_parts_and_extracts() {
    let file = noise(1, FILE_LEN as usize);
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(false)).unwrap();
    let chunks = write_peeled(&mut w, "f", &file);
    // Chunks cut per span: 4096 + 904, 4096 + 2904, 3000.
    assert_eq!(chunks.len(), 5);
    w.finish().unwrap();
    let mut a = open(out);
    let es = entries(&mut a);
    let e = &es[0];
    assert_eq!(e.size, FILE_LEN);
    assert_eq!(extract(&mut a, e), file);
    a.verify().unwrap();
    // The block's chunks form one block of their own, and no chunk crosses a
    // span boundary.
    let block = a.chunks().locate(chunks[0]).unwrap().block;
    for &c in &chunks {
        assert_eq!(a.chunks().locate(c).unwrap().block, block);
    }
    assert_eq!(
        a.block_chunks(block).unwrap().count(),
        chunks.len(),
        "the block holds the spans' chunks only"
    );
    let bounds: Vec<u64> = SPANS.iter().flat_map(|&(o, l)| [o, o + l]).collect();
    let ranges = chunk_ranges(&a, e);
    for &(c, s, t) in &ranges {
        for &b in &bounds {
            assert!(!(s < b && b < t), "chunk {c} [{s}, {t}) crosses {b}");
        }
    }
    // Every span is exactly the block's chunks placed at its offset.
    let in_spans: Vec<u64> = ranges
        .iter()
        .filter(|&&(_, s, _)| SPANS.iter().any(|&(o, l)| s >= o && s < o + l))
        .map(|&(c, _, _)| c)
        .collect();
    assert_eq!(in_spans, chunks);
    // The envelope took the block's memory term.
    assert!(a.index().envelope.decode_memory >= 77);
}

#[test]
fn one_span_is_add_part_encoded_byte_for_byte() {
    let file = noise(2, 9_000);
    let build = |multi: bool| {
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, options(true)).unwrap();
        w.begin_entry("f", EntryFlags::EMPTY, 0).unwrap();
        w.add_part(0, &mut &file[..2_000]).unwrap();
        let plain = file[2_000..].to_vec();
        let enc = StoreEncoder::wrap(plain.clone());
        if multi {
            w.add_parts_encoded(&[(2_000, 7_000)], &plain, enc, 5)
                .unwrap();
        } else {
            w.add_part_encoded(2_000, &plain, enc, 5).unwrap();
        }
        w.end_entry().unwrap();
        let s = w.finish().unwrap();
        (out, s)
    };
    assert_eq!(build(true), build(false));
}

#[test]
fn bad_spans_are_refused() {
    let plain = noise(3, 300);
    for spans in [
        vec![],
        vec![(100u64, 100u64), (0, 200)], // unsorted
        vec![(0, 200), (150, 100)],       // overlapping
        vec![(0, 300), (400, 0)],         // empty span
        vec![(0, 100), (200, 100)],       // lengths do not add up
        vec![(0, 400)],                   // longer than plain
        vec![(u64::MAX, 300)],            // past the end of any file
    ] {
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, options(false)).unwrap();
        w.begin_entry("f", EntryFlags::EMPTY, 0).unwrap();
        let e = w
            .add_parts_encoded(&spans, &plain, StoreEncoder::wrap(plain.clone()), 0)
            .unwrap_err();
        assert!(
            matches!(
                e,
                FormatError::BadOptions {
                    reason: "encoded part spans"
                }
            ),
            "{spans:?}: {e:?}"
        );
    }
    // The length rules come first, as for one span.
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(false)).unwrap();
    w.begin_entry("f", EntryFlags::EMPTY, 0).unwrap();
    assert!(matches!(
        w.add_parts_encoded(&[(0, 0)], &[], StoreEncoder::wrap(vec![]), 0),
        Err(FormatError::BadOptions {
            reason: "encoded part length"
        })
    ));
}

#[test]
fn an_identical_multi_span_stream_dedups_span_by_span() {
    let file = noise(4, FILE_LEN as usize);
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(true)).unwrap();
    let chunks = write_peeled(&mut w, "a", &file);
    // The second copy: each span is known (chunks were cut per span), so
    // each is referenced without a record or a block.
    w.begin_entry("b", EntryFlags::EMPTY, 0).unwrap();
    let mut known = Vec::new();
    for s in SPANS {
        let q = w.part_known(slice(&file, s)).unwrap().unwrap();
        let got = w.add_part_known(s.0, slice(&file, s)).unwrap().unwrap();
        assert_eq!(q, got);
        known.extend(got);
    }
    assert_eq!(known, chunks);
    for gap in gaps() {
        w.add_part(gap.0, &mut slice(&file, gap)).unwrap();
    }
    w.end_entry().unwrap();
    let s = w.finish().unwrap();
    let mut a = open(out);
    let es = entries(&mut a);
    assert_eq!(es[0].chunks, es[1].chunks);
    for e in &es {
        assert_eq!(extract(&mut a, e), file);
    }
    a.verify().unwrap();
    // Nothing of the second copy was stored again.
    assert_eq!(s.chunks, chunks_of(&es[0]));
}

fn chunks_of(e: &Entry) -> u64 {
    let mut c = e.chunks.clone();
    c.sort_unstable();
    c.dedup();
    c.len() as u64
}

/// Writes "a", then "b" in parts (one part known, one new), with
/// `part_known` asked around every call when `query` is set; returns the
/// archive, its summary and the answers.
fn query_run(query: bool) -> (Vec<u8>, WriterSummary, Vec<Option<Vec<u64>>>) {
    let a = noise(5, 10_000);
    let fresh = noise(6, 3_000);
    let mut answers = Vec::new();
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(true)).unwrap();
    if query {
        answers.push(w.part_known(&a).unwrap());
    }
    w.add_file("a", EntryFlags::EMPTY, 0, &mut &a[..]).unwrap();
    if query {
        answers.push(w.part_known(&a).unwrap());
        answers.push(w.part_known(&fresh).unwrap());
        answers.push(w.part_known(&[]).unwrap());
        // A part whose start is shifted is cut differently: not known.
        answers.push(w.part_known(&a[1..]).unwrap());
    }
    w.begin_entry("b", EntryFlags::EMPTY, 0).unwrap();
    let got = w.add_part_known(0, &a).unwrap();
    if query {
        answers.push(got);
        answers.push(w.part_known(&fresh).unwrap());
    }
    assert!(w.add_part_known(10_000, &fresh).unwrap().is_none());
    w.add_part(10_000, &mut &fresh[..]).unwrap();
    w.end_entry().unwrap();
    if query {
        answers.push(w.part_known(&fresh).unwrap());
    }
    let s = w.finish().unwrap();
    (out, s, answers)
}

#[test]
fn part_known_answers_as_add_part_known_acts_and_changes_nothing() {
    let (with, s_with, answers) = query_run(true);
    let (without, s_without, _) = query_run(false);
    // The archive and every counter are those of a run without the queries.
    assert_eq!(with, without);
    assert_eq!(s_with, s_without);
    let ids_a = answers[1].clone().unwrap();
    assert_eq!(ids_a.len(), 3); // 4096 + 4096 + 1808
    assert_eq!(
        answers,
        vec![
            None,                // nothing stored yet
            Some(ids_a.clone()), // "a" stored
            None,                // fresh bytes
            None,                // empty
            None,                // shifted
            Some(ids_a),         // what add_part_known then referenced
            None,                // still fresh
            Some(vec![3]),       // stored by add_part
        ]
    );
}

#[test]
fn part_known_is_none_without_dedup() {
    let a = noise(7, 5_000);
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(false)).unwrap();
    w.add_file("a", EntryFlags::EMPTY, 0, &mut &a[..]).unwrap();
    assert_eq!(w.part_known(&a).unwrap(), None);
}
