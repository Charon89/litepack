//! Global chunk dedup within one archive (`WriterOptions::dedup`, E2-7): identical chunks are
//! stored once and referenced by index; the reader needs nothing new.
#![allow(clippy::unwrap_used)]

use lpk_format::{Archive, Entry, EntryFlags, Resources, Writer, WriterOptions, WriterSummary};
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

fn write(dedup: bool, files: &[(&str, &[u8])]) -> (Vec<u8>, WriterSummary) {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(dedup)).unwrap();
    for (i, (p, d)) in files.iter().enumerate() {
        w.add_file(p, EntryFlags::EMPTY, i as i64, &mut &d[..])
            .unwrap();
    }
    let s = w.finish().unwrap();
    (out, s)
}

fn entries(a: &mut Archive<Cursor<Vec<u8>>>) -> Vec<Entry> {
    let t = a.entry_table().unwrap();
    let v = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    v
}

/// Verify and extract every file through `lpk_format` and through `lpk-check`.
fn check_both(bytes: &[u8], files: &[(&str, &[u8])]) {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    a.verify().unwrap();
    for e in entries(&mut a) {
        let want = files.iter().find(|f| f.0 == e.path).unwrap().1;
        let mut got = Vec::new();
        a.extract(&e, &mut got).unwrap();
        assert_eq!(got, want, "{}", e.path);
    }
    let mut c =
        lpk_check::archive::Archive::open(bytes.to_vec(), lpk_check::archive::Options::default())
            .unwrap();
    c.verify().unwrap();
    for e in c.entries().unwrap() {
        if e.kind != lpk_check::entries::EntryKind::File {
            continue;
        }
        let want = files.iter().find(|f| f.0 == e.path).unwrap().1;
        assert_eq!(c.read_file(&e).unwrap(), want, "check: {}", e.path);
    }
}

#[test]
fn two_identical_files_share_one_set_of_chunks() {
    let data = noise(1, 10_000);
    let files: [(&str, &[u8]); 2] = [("a", &data), ("b", &data)];
    let (plain, sp) = write(false, &files);
    let (dd, sd) = write(true, &files);
    assert_eq!((sp.chunks, sp.deduped_chunks), (6, 0));
    assert_eq!(
        (sd.chunks, sd.deduped_chunks, sd.deduped_bytes),
        (3, 3, 10_000)
    );
    assert!(dd.len() < plain.len());
    let mut a = Archive::open(Cursor::new(dd.clone()), &Resources::default()).unwrap();
    let es = entries(&mut a);
    assert_eq!(es[0].chunks, es[1].chunks);
    assert_eq!(es[1].size, 10_000);
    check_both(&dd, &files);
    check_both(&plain, &files);
}

#[test]
fn a_file_that_repeats_a_block_of_itself_stores_the_block_once() {
    let block = noise(2, 4096);
    let mut data = block.clone();
    data.extend_from_slice(&noise(3, 4096));
    data.extend_from_slice(&block);
    data.extend_from_slice(&block);
    let files: [(&str, &[u8]); 1] = [("r", &data)];
    let (dd, s) = write(true, &files);
    assert_eq!((s.chunks, s.deduped_chunks, s.deduped_bytes), (2, 2, 8192));
    check_both(&dd, &files);
}

#[test]
fn dedup_off_is_the_old_behaviour_byte_for_byte() {
    let data = noise(4, 9000);
    let files: [(&str, &[u8]); 2] = [("a", &data), ("b", &data)];
    let (off, _) = write(false, &files);
    let mut out = Vec::new();
    let mut w = Writer::new(
        &mut out,
        WriterOptions {
            chunk_size: 4096,
            block_size: 16384,
            archive_id: [9; 16],
            ..WriterOptions::default()
        },
    )
    .unwrap();
    for (i, (p, d)) in files.iter().enumerate() {
        w.add_file(p, EntryFlags::EMPTY, i as i64, &mut &d[..])
            .unwrap();
    }
    w.finish().unwrap();
    assert_eq!(off, out);
    assert!(!WriterOptions::default().dedup);
}

#[test]
fn dedup_works_across_blocks_and_with_an_append_against_the_old_table() {
    // Small blocks: the duplicate's chunks live in earlier, closed blocks.
    let big = noise(5, 60_000);
    let other = noise(6, 5000);
    let files: [(&str, &[u8]); 3] = [("a", &big), ("o", &other), ("b", &big)];
    let (dd, s) = write(true, &files);
    assert!(s.blocks > 1);
    assert_eq!(s.deduped_bytes, 60_000);
    check_both(&dd, &files);

    // An append: a copy of `big` is found in the old table (reused), and a
    // duplicate inside the appended data is found too.
    let fresh = noise(7, 8192);
    let mut file = Cursor::new(dd.clone());
    let existing = Archive::open(&mut file, &Resources::default()).unwrap();
    let mut out = Cursor::new(dd.clone());
    out.set_position(dd.len() as u64);
    let mut w = Writer::append(existing, &mut out, options(true), None).unwrap();
    w.add_file("c", EntryFlags::EMPTY, 9, &mut &big[..])
        .unwrap();
    w.add_file("d", EntryFlags::EMPTY, 9, &mut &fresh[..])
        .unwrap();
    w.add_file("e", EntryFlags::EMPTY, 9, &mut &fresh[..])
        .unwrap();
    let s2 = w.finish().unwrap();
    assert_eq!(s2.reused_chunks, 15);
    assert_eq!(
        (s2.new_chunks, s2.deduped_chunks, s2.deduped_bytes),
        (2, 2, 8192)
    );
    let all: [(&str, &[u8]); 6] = [
        ("a", &big),
        ("o", &other),
        ("b", &big),
        ("c", &big),
        ("d", &fresh),
        ("e", &fresh),
    ];
    check_both(&out.into_inner(), &all);
}

#[test]
fn distinct_files_are_untouched_by_dedup() {
    let (a, b) = (noise(11, 9000), noise(12, 9000));
    let files: [(&str, &[u8]); 2] = [("a", &a), ("b", &b)];
    let (dd, s) = write(true, &files);
    let (plain, p) = write(false, &files);
    assert_eq!(s.chunks, p.chunks);
    assert_eq!(s.deduped_chunks, 0);
    check_both(&dd, &files);
    assert!(dd.len() <= plain.len());
}
