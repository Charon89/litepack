//! A three-generation archive shared by the journal tests and the vector.
#![allow(dead_code, clippy::unwrap_used)]

use super::pattern;
use super::sealed::SeedRng;
use lpk_format::{
    Archive, Credentials, EntryFlags, RecoveryOptions, Resources, Writer, WriterOptions,
    WriterSummary,
};
use std::io::Cursor;

pub const JOURNAL_VECTOR: &str = "journal-3gen.lpk";

/// Store-encoded options with 4 KiB chunks, 16 KiB blocks and, when
/// `percent > 0`, recovery in groups of 16 shards of 4 KiB.
pub fn options(percent: u8) -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [0x4A; 16],
        recovery: RecoveryOptions {
            percent,
            shard_len: 4096,
            group_shards: 16,
        },
        ..WriterOptions::default()
    }
}

/// The files of generation 0 (sorted by path).
pub fn gen0_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.txt", pattern(1, 6_000)),
        ("b.txt", pattern(2, 9_000)),
        ("c.txt", pattern(3, 5_000)),
        ("d/e", pattern(4, 3_000)),
    ]
}

/// The files the last generation holds (sorted by path): generation 1 replaced
/// `b.txt`, added `e.txt` and deleted `c.txt`; generation 2 replaced `d/e` and
/// added `f.txt`, a copy of `a.txt`.
pub fn final_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.txt", pattern(1, 6_000)),
        ("b.txt", pattern(12, 7_000)),
        ("d/e", pattern(14, 2_000)),
        ("e.txt", pattern(5, 4_000)),
        ("f.txt", pattern(1, 6_000)),
    ]
}

/// Append one generation to `old`: the new bytes go to a buffer and are
/// concatenated, which is what appending to the file does.
pub fn append_gen(
    old: &[u8],
    opts: WriterOptions,
    creds: Option<&Credentials>,
    f: impl FnOnce(&mut Writer<&mut Vec<u8>>),
) -> (Vec<u8>, WriterSummary) {
    let a = Archive::open_with(Cursor::new(old.to_vec()), &Resources::default(), creds).unwrap();
    let mut tail = Vec::new();
    let mut w = Writer::append(a, &mut tail, opts, creds).unwrap();
    f(&mut w);
    let summary = w.finish().unwrap();
    let mut all = old.to_vec();
    all.extend(tail);
    (all, summary)
}

fn add(w: &mut Writer<&mut Vec<u8>>, path: &str, data: &[u8]) {
    w.add_file(path, EntryFlags::EMPTY, 1_000, &mut &data[..])
        .unwrap();
}

/// The archive after each of the three generations, and the writer summaries.
/// `mk` gives the options of every write (sealed or not); `creds` are the
/// credentials of an encrypted archive.
pub fn build_journal(
    mk: &dyn Fn() -> WriterOptions,
    creds: Option<&Credentials>,
) -> (Vec<Vec<u8>>, Vec<WriterSummary>) {
    let mut g0 = Vec::new();
    let mut w = Writer::new_with_rng(&mut g0, mk(), &mut SeedRng(7)).unwrap();
    for (p, d) in gen0_files() {
        add(&mut w, p, &d);
    }
    let s0 = w.finish().unwrap();
    let (g1, s1) = append_gen(&g0, mk(), creds, |w| {
        add(w, "b.txt", &pattern(12, 7_000));
        add(w, "e.txt", &pattern(5, 4_000));
        w.delete_path("c.txt").unwrap();
    });
    let (g2, s2) = append_gen(&g1, mk(), creds, |w| {
        add(w, "d/e", &pattern(14, 2_000));
        add(w, "f.txt", &pattern(1, 6_000));
    });
    (vec![g0, g1, g2], vec![s0, s1, s2])
}

/// Every entry of the archive with its extracted bytes.
pub fn extract_all(bytes: &[u8], creds: Option<&Credentials>) -> Vec<(String, Vec<u8>)> {
    let mut a =
        Archive::open_with(Cursor::new(bytes.to_vec()), &Resources::default(), creds).unwrap();
    let table = a.entry_table().unwrap();
    let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
    entries
        .iter()
        .map(|e| {
            let mut got = Vec::new();
            a.extract(e, &mut got).unwrap();
            (e.path.clone(), got)
        })
        .collect()
}
