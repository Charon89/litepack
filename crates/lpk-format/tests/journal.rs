//! Generations (spec section 15): append with dedup, the trailer chain,
//! rollback, the per-generation index nonce, recovery across generations,
//! half-written appends and the committed vector.
#![allow(clippy::unwrap_used)]

mod common;

use common::journal::*;
use common::sealed::seal_options;
use common::{pattern, write_archive};
use lpk_format::cli::run;
use lpk_format::{
    index_sequence, repair, rollback, Archive, Credentials, EntryFlags, FormatError,
    RecoveryOptions, Resources, Suite, Trailer, Writer, TRAILER_FRAME_LEN,
};
use std::collections::HashSet;
use std::io::{Cursor, Write};

fn open(bytes: &[u8]) -> Archive<Cursor<Vec<u8>>> {
    Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap()
}

fn plain() -> (Vec<Vec<u8>>, Vec<lpk_format::WriterSummary>) {
    build_journal(&|| options(0), None)
}

fn temp_file(bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
    f
}

fn read(f: &tempfile::NamedTempFile) -> Vec<u8> {
    std::fs::read(f.path()).unwrap()
}

fn cli(args: &[&str]) -> (i32, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut argv = vec!["lpk-decode"];
    argv.extend_from_slice(args);
    let code = run(argv, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn three_generations_hold_the_expected_entries_and_bytes() {
    let (snaps, sums) = plain();
    let last = &snaps[2];
    let got = extract_all(last, None);
    let want = final_files();
    assert_eq!(got.len(), want.len());
    for ((p, d), (wp, wd)) in got.iter().zip(&want) {
        assert_eq!(p, wp);
        assert!(d == wd, "{p}");
    }
    // The replaced file has its new content; the deleted path is gone.
    assert!(!got.iter().any(|(p, _)| p == "c.txt"));
    let mut a = open(last);
    assert_eq!(a.generation(), 2);
    // The deleted file's chunks stay in the chunk table.
    let by_hash = a.chunks().by_hash();
    for chunk in pattern(3, 5_000).chunks(4096) {
        assert!(by_hash.contains_key(blake3::hash(chunk).as_bytes()));
    }
    // The old content of the replaced file stays too.
    assert!(by_hash.contains_key(blake3::hash(&pattern(2, 9_000)[..4096]).as_bytes()));
    let v = a.verify().unwrap();
    assert!(v.chunks_checked);
    assert_eq!(v.entries, 5);
    // The summaries.
    assert_eq!(
        sums.iter().map(|s| s.generation).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(sums[0].reused_chunks, 0);
    assert_eq!(sums[1].reused_chunks, 0);
    // f.txt is a copy of a.txt: both of its chunks (4096 + 1904 bytes) are reused.
    assert_eq!(sums[2].reused_chunks, 2);
    assert_eq!(sums[2].chunks, sums[2].new_chunks + sums[1].chunks);
    assert_eq!(sums[2].archive_len, last.len() as u64);
    // Generation 1 on its own reads as generation 1.
    let got1 = extract_all(&snaps[1], None);
    let names: Vec<_> = got1.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(names, ["a.txt", "b.txt", "d/e", "e.txt"]);
    assert!(got1[1].1 == pattern(12, 7_000));
}

#[test]
fn history_lists_the_chain_newest_first() {
    let (snaps, _) = plain();
    let mut a = open(&snaps[2]);
    let h = a.history().unwrap();
    assert_eq!(
        h.iter().map(|g| g.generation).collect::<Vec<_>>(),
        [2, 1, 0]
    );
    assert_eq!(h[0].end(), snaps[2].len() as u64);
    assert_eq!(h[1].end(), snaps[1].len() as u64);
    assert_eq!(h[2].end(), snaps[0].len() as u64);
    assert!(h[0].trailer_offset > h[1].trailer_offset && h[1].trailer_offset > h[2].trailer_offset);
    // Each generation's index hash is the one its own archive has.
    for (g, snap) in h.iter().zip(snaps.iter().rev()) {
        let b = open(snap);
        assert_eq!(g.index_hash, b.trailer().index_hash);
        assert_eq!(g.index_offset, b.trailer().index_offset);
    }
    assert_eq!(open(&snaps[0]).history().unwrap().len(), 1);
    assert_eq!(a.trailer().previous_trailer_offset, h[1].trailer_offset);
}

#[test]
fn appended_archive_is_close_to_a_one_shot_archive_of_the_same_tree() {
    let (snaps, _) = plain();
    let one_shot = write_archive(options(0), &final_files());
    let mut a = open(&snaps[2]);
    let h = a.history().unwrap();
    // Tolerance: the one-shot size, plus the bytes of the replaced and the
    // deleted files that stay in the archive, plus for each earlier
    // generation its index, its trailer and 1 KiB for its entry table and
    // the block headers.
    let dead = 9_000 + 5_000 + 3_000;
    let earlier: u64 = h[1..]
        .iter()
        .map(|g| (g.trailer_offset - g.index_offset) + TRAILER_FRAME_LEN + 1024)
        .sum();
    let allowed = one_shot.len() as u64 + dead + earlier;
    assert!(
        snaps[2].len() as u64 <= allowed,
        "{} > {allowed}",
        snaps[2].len()
    );
    // And it is not smaller than the one-shot archive minus the chunks the
    // append deduplicated (a.txt's two chunks).
    assert!(snaps[2].len() as u64 + 6_000 >= one_shot.len() as u64);
}

#[test]
fn delete_then_add_keeps_the_new_entry_and_delete_needs_an_append() {
    let (snaps, _) = plain();
    let (g3, _) = append_gen(&snaps[2], options(0), None, |w| {
        w.delete_path("a.txt").unwrap();
        w.delete_path("nothing").unwrap();
        w.add_file("a.txt", EntryFlags::EMPTY, 5, &mut &b"new"[..])
            .unwrap();
    });
    let got = extract_all(&g3, None);
    assert_eq!(got.len(), 5);
    assert_eq!(got[0], ("a.txt".to_string(), b"new".to_vec()));
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options(0)).unwrap();
    assert!(matches!(
        w.delete_path("x"),
        Err(FormatError::BadOptions { .. })
    ));
}

#[test]
fn rollback_reproduces_each_generation_exactly() {
    let (snaps, _) = plain();
    let f = temp_file(&snaps[2]);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.path())
        .unwrap();
    let g = rollback(&mut file, 1).unwrap();
    assert_eq!((g.generation, g.end()), (1, snaps[1].len() as u64));
    assert!(read(&f) == snaps[1]);
    // The rolled-back file opens as generation 1 and verifies.
    let mut a = open(&read(&f));
    assert_eq!(a.generation(), 1);
    a.verify().unwrap();
    // A generation the file no longer has.
    assert!(matches!(
        rollback(&mut file, 2),
        Err(FormatError::NoSuchGeneration {
            requested: 2,
            latest: 1
        })
    ));
    assert!(matches!(
        rollback(&mut file, 9),
        Err(FormatError::NoSuchGeneration {
            requested: 9,
            latest: 1
        })
    ));
    // Rolling back to the latest is a no-op; to 0 gives the first write.
    rollback(&mut file, 1).unwrap();
    assert!(read(&f) == snaps[1]);
    rollback(&mut file, 0).unwrap();
    assert!(read(&f) == snaps[0]);
}

#[test]
fn half_appended_file_is_truncated_and_rollback_repairs_it() {
    let (snaps, _) = plain();
    let mut a = open(&snaps[2]);
    let h = a.history().unwrap();
    // Cuts: inside the new trailer, just before it, and inside the new data.
    let cuts = [
        snaps[2].len() - 10,
        h[0].trailer_offset as usize,
        snaps[1].len() + 3,
        snaps[1].len() + 1_500,
    ];
    for cut in cuts {
        let broken = &snaps[2][..cut];
        assert!(
            matches!(
                Archive::open(Cursor::new(broken.to_vec()), &Resources::default()),
                Err(FormatError::Truncated { what: "trailer" })
            ),
            "cut at {cut}"
        );
        let d = Archive::diagnose(Cursor::new(broken.to_vec()), &Default::default());
        assert!(d.trailer_seen, "the old trailer is seen (cut at {cut})");
        assert_eq!(d.last_trailer_end, snaps[1].len() as u64);
        assert!(matches!(
            d.error,
            Some(FormatError::Truncated { what: "trailer" })
        ));
        let f = temp_file(broken);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(f.path())
            .unwrap();
        let g = rollback(&mut file, 1).unwrap();
        assert_eq!(g.generation, 1);
        assert!(read(&f) == snaps[1], "cut at {cut}");
        open(&read(&f)).verify().unwrap();
    }
    // The half-appended file cannot be rolled back to a generation it never had.
    let f = temp_file(&snaps[2][..snaps[1].len() + 1_500]);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.path())
        .unwrap();
    assert!(matches!(
        rollback(&mut file, 2),
        Err(FormatError::NoSuchGeneration {
            requested: 2,
            latest: 1
        })
    ));
}

#[test]
fn garbage_after_a_trailer_is_still_trailing_bytes() {
    let (snaps, _) = plain();
    let mut bytes = snaps[0].clone();
    bytes.extend_from_slice(&[0xEE; 40]);
    let d = Archive::diagnose(Cursor::new(bytes), &Default::default());
    assert!(d.trailer_seen);
    assert!(matches!(
        d.error,
        Some(FormatError::TrailingBytes { what: "archive" }) | Some(FormatError::Truncated { .. })
    ));
}

#[test]
fn a_broken_chain_is_refused() {
    let (snaps, _) = plain();
    let mut a = open(&snaps[2]);
    let h = a.history().unwrap();
    // The middle trailer claims generation 5.
    let mut forged = snaps[2].clone();
    let old = Trailer::read_at(&mut Cursor::new(&forged), h[1].trailer_offset).unwrap();
    let mut t = Vec::new();
    Trailer {
        generation: 5,
        ..old
    }
    .write(&mut t)
    .unwrap();
    let at = h[1].trailer_offset as usize;
    forged[at..at + t.len()].copy_from_slice(&t);
    let mut b = open(&forged);
    assert!(matches!(
        b.history(),
        Err(FormatError::GenerationMismatch {
            expected: 1,
            found: 5
        })
    ));
    // A trailer of another archive in the chain.
    let mut other = snaps[2].clone();
    let mut t = Vec::new();
    Trailer {
        archive_id: [1; 16],
        ..old
    }
    .write(&mut t)
    .unwrap();
    other[at..at + t.len()].copy_from_slice(&t);
    assert!(matches!(
        open(&other).history(),
        Err(FormatError::ArchiveIdMismatch)
    ));
    // A damaged old trailer.
    let mut dmg = snaps[2].clone();
    dmg[at + 20] ^= 1;
    assert!(open(&dmg).history().is_err());
}

fn sealed_opts(listable: bool) -> lpk_format::WriterOptions {
    let mut o = seal_options(Suite::AesGcm, listable, "pw", None);
    o.archive_id = [0x4A; 16];
    o
}

fn creds() -> Credentials {
    Credentials::password(b"pw".to_vec())
}

#[test]
fn encrypted_append_needs_credentials_and_keeps_nonces_unique() {
    let c = creds();
    let (snaps, _) = build_journal(&|| sealed_opts(false), Some(&c));
    // Without credentials: the archive opens keyless, and appending is refused.
    let keyless =
        Archive::open_with(Cursor::new(snaps[1].clone()), &Resources::default(), None).unwrap();
    let mut tail = Vec::new();
    assert!(matches!(
        Writer::append(keyless, &mut tail, options(0), None),
        Err(FormatError::AppendNeedsCredentials)
    ));
    let opened = Archive::open_with(
        Cursor::new(snaps[1].clone()),
        &Resources::default(),
        Some(&c),
    )
    .unwrap();
    assert!(matches!(
        Writer::append(opened, &mut tail, options(0), None),
        Err(FormatError::AppendNeedsCredentials)
    ));
    // With them, old and new files extract under the password.
    let got = extract_all(&snaps[2], Some(&c));
    let want = final_files();
    assert_eq!(got.len(), want.len());
    for ((p, d), (wp, wd)) in got.iter().zip(&want) {
        assert_eq!(p, wp);
        assert!(d == wd, "{p}");
    }
    let mut a = Archive::open_with(
        Cursor::new(snaps[2].clone()),
        &Resources::default(),
        Some(&c),
    )
    .unwrap();
    assert!(a.verify().unwrap().chunks_checked);
    assert_eq!(a.history().unwrap().len(), 3);
    // The three indexes are sealed under different sequences and salts, so the
    // nonce bytes stored at the start of each index frame differ.
    let nl = Suite::AesGcm.nonce_len();
    let nonces: Vec<Vec<u8>> = a
        .history()
        .unwrap()
        .iter()
        .map(|g| stored_nonce(&snaps[2], g.index_offset, nl))
        .collect();
    assert!(nonces[0] != nonces[1] && nonces[1] != nonces[2] && nonces[0] != nonces[2]);
    assert_eq!(index_sequence(0), u64::MAX);
    // Every sealed frame has its own (kind, sequence): the sequences the index
    // lists are unique and ascend across the generations.
    let idx = a.index();
    let mut seqs: Vec<u64> = idx.blocks.iter().map(|b| b.sequence).collect();
    seqs.push(idx.entry_table.sequence);
    let unique: HashSet<_> = seqs.iter().collect();
    assert_eq!(unique.len(), seqs.len());
    let first_new = open_sealed_blocks(&snaps[1], &c);
    assert!(idx.blocks[first_new.0..]
        .iter()
        .all(|b| b.sequence > first_new.1));
    // A wrong password does not open it.
    assert!(Archive::open_with(
        Cursor::new(snaps[2].clone()),
        &Resources::default(),
        Some(&Credentials::password(b"nope".to_vec()))
    )
    .is_err());
}

/// The number of blocks of an archive and the largest sequence it lists.
fn open_sealed_blocks(bytes: &[u8], c: &Credentials) -> (usize, u64) {
    let a =
        Archive::open_with(Cursor::new(bytes.to_vec()), &Resources::default(), Some(c)).unwrap();
    let i = a.index();
    let max = i
        .blocks
        .iter()
        .map(|b| b.sequence)
        .chain([i.entry_table.sequence])
        .max()
        .unwrap();
    (i.blocks.len(), max)
}

#[test]
fn a_listable_archive_with_generations_lists_without_the_key() {
    let c = creds();
    let (snaps, _) = build_journal(&|| sealed_opts(true), Some(&c));
    let mut a =
        Archive::open_with(Cursor::new(snaps[2].clone()), &Resources::default(), None).unwrap();
    assert!(a.is_keyless());
    let t = a.entry_table().unwrap();
    let names: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap().path).collect();
    assert_eq!(names, ["a.txt", "b.txt", "d/e", "e.txt", "f.txt"]);
    // Frame hashes over all generations verify without the key.
    assert!(!a.verify().unwrap().chunks_checked);
    // Rollback needs no credentials.
    let f = temp_file(&snaps[2]);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.path())
        .unwrap();
    rollback(&mut file, 0).unwrap();
    assert!(read(&f) == snaps[0]);
}

fn rec_opts() -> lpk_format::WriterOptions {
    options(20)
}

fn flip(bytes: &mut [u8], at: usize) {
    bytes[at] ^= 0xFF;
}

#[test]
fn recovery_covers_every_generation_and_repairs_damage_in_each() {
    let (snaps, _) = build_journal(&rec_opts, None);
    let last = &snaps[2];
    let mut a = open(last);
    a.verify().unwrap();
    let r = a.check_recovery().unwrap();
    assert_eq!((r.shards_damaged, r.frames_unusable), (0, 0));
    // Generation 2 wrote frames of its own, and generation 1's are still listed.
    let n0 = open(&snaps[0]).recovery_frames().len();
    let n1 = open(&snaps[1]).recovery_frames().len();
    let n2 = a.recovery_frames().len();
    assert!(n0 > 0 && n1 > n0 && n2 > n1, "{n0} {n1} {n2}");
    // The first frame of each later generation starts after the previous trailer.
    let frames = a.recovery_frames().to_vec();
    let h = a.history().unwrap();
    assert!(frames[n0].offset > h[2].end() - 1);
    // Damage a data block of each generation, one at a time.
    let blocks = a.index().blocks.clone();
    let b0 = blocks.first().unwrap();
    let b2 = blocks.last().unwrap();
    assert!(b0.frame_offset < h[2].end() && b2.frame_offset >= h[1].end());
    let mut mid = None;
    for b in &blocks {
        if b.frame_offset >= h[2].end() && b.frame_offset < h[1].end() {
            mid = Some(b);
        }
    }
    let b1 = mid.unwrap();
    for b in [b0, b1, b2] {
        let mut bad = last.clone();
        flip(&mut bad, b.frame_offset as usize + 200);
        let mut out = Cursor::new(Vec::new());
        let report =
            repair(Cursor::new(bad), &mut out, &Resources::default()).unwrap_or_else(|e| {
                panic!("block at {}: {e}", b.frame_offset);
            });
        assert!(report.shards_repaired >= 1);
        assert!(&out.into_inner() == last);
    }
    // The tiny rollback case: damage lives in generation 1's data, roll back and repair there.
    let mut bad = snaps[1].clone();
    flip(&mut bad, b1.frame_offset as usize + 200);
    let mut out = Cursor::new(Vec::new());
    repair(Cursor::new(bad), &mut out, &Resources::default()).unwrap();
    assert!(out.into_inner() == snaps[1]);
}

#[test]
fn generations_may_switch_recovery_on_and_off() {
    // Generation 0 with recovery, generation 1 without, generation 2 with.
    let mut g0 = Vec::new();
    let mut w = Writer::new(&mut g0, options(20)).unwrap();
    for (p, d) in gen0_files() {
        w.add_file(p, EntryFlags::EMPTY, 0, &mut d.as_slice())
            .unwrap();
    }
    w.finish().unwrap();
    let (g1, _) = append_gen(&g0, options(0), None, |w| {
        w.add_file("x1", EntryFlags::EMPTY, 0, &mut &pattern(31, 10_000)[..])
            .unwrap();
    });
    let mut a = open(&g1);
    a.verify().unwrap();
    let n0 = a.recovery_frames().len();
    assert!(n0 > 0);
    let r = a.check_recovery().unwrap();
    assert_eq!((r.shards_damaged, r.frames_unusable), (0, 0));
    let (g2, _) = append_gen(&g1, options(20), None, |w| {
        w.add_file("x2", EntryFlags::EMPTY, 0, &mut &pattern(32, 10_000)[..])
            .unwrap();
    });
    let mut a = open(&g2);
    a.verify().unwrap();
    assert!(a.recovery_frames().len() > n0);
    let r = a.check_recovery().unwrap();
    assert_eq!((r.shards_damaged, r.frames_unusable), (0, 0));
    // Generation 0 without recovery, generation 1 with.
    let g0 = write_archive(options(0), &gen0_files());
    let (g1, _) = append_gen(&g0, options(20), None, |w| {
        w.add_file("x1", EntryFlags::EMPTY, 0, &mut &pattern(31, 10_000)[..])
            .unwrap();
    });
    let mut a = open(&g1);
    a.verify().unwrap();
    let r = a.check_recovery().unwrap();
    assert_eq!((r.frames_unusable, r.shards_damaged), (0, 0));
    assert!(r.frames > 0);
    let _ = RecoveryOptions::default();
}

#[test]
fn append_with_a_reused_chunk_only_and_an_empty_append() {
    let (snaps, _) = plain();
    // An append that adds nothing still makes a new generation.
    let (g3, s) = append_gen(&snaps[2], options(0), None, |_| {});
    assert_eq!((s.generation, s.new_chunks, s.reused_chunks), (3, 0, 0));
    let mut a = open(&g3);
    a.verify().unwrap();
    assert_eq!(a.history().unwrap().len(), 4);
    assert!(extract_all(&g3, None) == extract_all(&snaps[2], None));
    // A file made only of old chunks adds no data block.
    let (g4, s) = append_gen(&g3, options(0), None, |w| {
        w.add_file("z", EntryFlags::EMPTY, 0, &mut &pattern(2, 9_000)[..])
            .unwrap();
    });
    assert_eq!(s.new_chunks, 0);
    assert_eq!(s.reused_chunks, 3);
    assert_eq!(
        open(&g4).index().blocks.len(),
        open(&g3).index().blocks.len()
    );
    assert!(extract_all(&g4, None)
        .iter()
        .any(|(p, d)| p == "z" && d == &pattern(2, 9_000)));
}

#[test]
fn the_committed_journal_vector_decodes_and_is_reproduced() {
    let bytes = std::fs::read(common::vectors_dir().join(JOURNAL_VECTOR)).unwrap();
    let mut a = open(&bytes);
    assert_eq!(a.generation(), 2);
    assert_eq!(a.history().unwrap().len(), 3);
    a.verify().unwrap();
    let got = extract_all(&bytes, None);
    let want = final_files();
    assert_eq!(got.len(), want.len());
    for ((p, d), (wp, wd)) in got.iter().zip(&want) {
        assert_eq!(p, wp);
        assert!(d == wd, "{p}");
    }
    assert!(plain().0[2] == bytes, "regenerate with gen_vectors");
    // The tool reports the generation and the chain.
    let path = common::vectors_dir().join(JOURNAL_VECTOR);
    let (code, out, _) = cli(&["info", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.contains("generation: 2\n"), "{out}");
    assert!(out.contains("chain length: 3\n"), "{out}");
    let (code, out, _) = cli(&["verify", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn the_tool_rolls_back_and_reports_missing_generations() {
    let (snaps, _) = plain();
    let f = temp_file(&snaps[2]);
    let p = f.path().to_str().unwrap().to_string();
    let (code, _, err) = cli(&["rollback", &p, "7"]);
    assert_eq!(code, 1);
    assert!(err.contains("no generation 7"), "{err}");
    assert!(read(&f) == snaps[2]);
    let (code, out, _) = cli(&["rollback", &p, "1"]);
    assert_eq!(code, 0);
    assert!(out.contains("rolled back to generation 1"), "{out}");
    assert!(read(&f) == snaps[1]);
    let (code, out, _) = cli(&["info", &p]);
    assert_eq!(code, 0);
    assert!(out.contains("generation: 1\n") && out.contains("chain length: 2\n"));
}

#[test]
fn trailer_frame_len_is_133_and_a_short_tail_is_not_accepted() {
    assert_eq!(TRAILER_FRAME_LEN, 133);
    let (snaps, _) = plain();
    // A 109-byte trailer-shaped tail (the old layout) is not a trailer.
    let cut = &snaps[0][..snaps[0].len() - 8];
    assert!(Archive::open(Cursor::new(cut.to_vec()), &Resources::default()).is_err());
}

/// The nonce stored at the start of the payload of the frame at `offset`.
fn stored_nonce(bytes: &[u8], offset: u64, len: usize) -> Vec<u8> {
    let mut r = &bytes[offset as usize + 4..];
    let plen = lpk_format::varint::read(&mut r).unwrap();
    assert!(plen as usize > len);
    r[..len].to_vec()
}

fn open_keyed(bytes: &[u8], c: &Credentials) -> Archive<Cursor<Vec<u8>>> {
    Archive::open_with(Cursor::new(bytes.to_vec()), &Resources::default(), Some(c)).unwrap()
}

#[test]
fn nonces_do_not_repeat_after_a_rollback_and_a_different_append() {
    let c = creds();
    let (snaps, _) = build_journal(&|| sealed_opts(false), Some(&c));
    let old = open_keyed(&snaps[2], &c);
    let old_idx = old.trailer().index_offset;
    let old_block = *old.index().blocks.last().unwrap();
    // Roll back to generation 1 and append different content for generation 2.
    let (again, _) = append_gen(&snaps[1], sealed_opts(false), Some(&c), |w| {
        w.add_file("d/e", EntryFlags::EMPTY, 1, &mut &pattern(77, 2_000)[..])
            .unwrap();
        w.add_file("f.txt", EntryFlags::EMPTY, 1, &mut &pattern(78, 6_000)[..])
            .unwrap();
    });
    let new = open_keyed(&again, &c);
    let new_block = *new.index().blocks.last().unwrap();
    assert_eq!(new.generation(), old.generation());
    // Same position, same sequence: the stored nonces still differ.
    assert_eq!(new_block.sequence, old_block.sequence);
    assert_eq!(new_block.frame_offset, old_block.frame_offset);
    let nl = Suite::AesGcm.nonce_len();
    assert!(
        stored_nonce(&snaps[2], old_block.frame_offset, nl)
            != stored_nonce(&again, new_block.frame_offset, nl)
    );
    assert!(
        stored_nonce(&snaps[2], old_idx, nl)
            != stored_nonce(&again, new.trailer().index_offset, nl)
    );
    assert!(old.trailer().salt != new.trailer().salt);
    // The re-appended archive reads fine, new content included.
    let got = extract_all(&again, Some(&c));
    assert!(got
        .iter()
        .any(|(p, d)| p == "f.txt" && d == &pattern(78, 6_000)));
    open_keyed(&again, &c).verify().unwrap();
}

#[test]
fn a_wrong_password_cannot_append() {
    let c = creds();
    let (snaps, _) = build_journal(&|| sealed_opts(false), Some(&c));
    let a = open_keyed(&snaps[2], &c);
    let mut tail = Vec::new();
    let wrong = Credentials::password(b"not the password".to_vec());
    assert!(matches!(
        Writer::append(a, &mut tail, options(0), Some(&wrong)),
        Err(FormatError::WrongKey)
    ));
    assert!(tail.is_empty());
}

fn resign(bytes: &mut [u8], at: u64, t: Trailer) {
    let mut v = Vec::new();
    t.write(&mut v).unwrap();
    bytes[at as usize..at as usize + v.len()].copy_from_slice(&v);
}

#[test]
fn previous_trailer_offset_must_point_at_a_trailer_before_the_index() {
    let (snaps, _) = plain();
    let a = open(&snaps[2]);
    let t = *a.trailer();
    let at = (snaps[2].len() as u64) - TRAILER_FRAME_LEN;
    // At or after the index of the same generation.
    for bad in [t.index_offset, t.index_offset + 1, at] {
        let mut forged = snaps[2].clone();
        resign(
            &mut forged,
            at,
            Trailer {
                previous_trailer_offset: bad,
                ..t
            },
        );
        assert!(matches!(
            open(&forged).history(),
            Err(FormatError::BadFrameLocation { what: "trailer" })
        ));
    }
    // Inside a frame: not a trailer.
    let mut forged = snaps[2].clone();
    resign(
        &mut forged,
        at,
        Trailer {
            previous_trailer_offset: t.previous_trailer_offset + 7,
            ..t
        },
    );
    assert!(open(&forged).history().is_err());
}

#[test]
fn an_absurd_generation_is_refused_at_open() {
    let (snaps, _) = plain();
    let t = *open(&snaps[0]).trailer();
    let at = (snaps[0].len() as u64) - TRAILER_FRAME_LEN;
    let mut forged = snaps[0].clone();
    resign(
        &mut forged,
        at,
        Trailer {
            generation: u64::MAX,
            ..t
        },
    );
    assert!(matches!(
        Archive::open(Cursor::new(forged), &Resources::default()),
        Err(FormatError::BadTrailer {
            reason: "generation"
        })
    ));
}

#[test]
fn info_reports_a_damaged_old_trailer_instead_of_failing() {
    let (snaps, _) = plain();
    let h = open(&snaps[2]).history().unwrap();
    let mut dmg = snaps[2].clone();
    dmg[h[1].trailer_offset as usize + 20] ^= 1;
    let f = temp_file(&dmg);
    let (code, out, _) = cli(&["info", f.path().to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("generation: 2\n") && out.contains("chain: error"),
        "{out}"
    );
}

#[test]
fn append_file_appends_in_place_and_syncs() {
    let (snaps, _) = plain();
    let f = temp_file(&snaps[1]);
    let mut w = Writer::append_file(f.path(), options(0), None).unwrap();
    w.add_file(
        "d/e",
        EntryFlags::EMPTY,
        1_000,
        &mut &pattern(14, 2_000)[..],
    )
    .unwrap();
    w.add_file(
        "f.txt",
        EntryFlags::EMPTY,
        1_000,
        &mut &pattern(1, 6_000)[..],
    )
    .unwrap();
    let s = w.finish().unwrap();
    assert_eq!(s.generation, 2);
    let bytes = read(&f);
    assert!(bytes == snaps[2], "same bytes as the in-memory append");
}

mod model {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    const NAMES: [&str; 5] = ["a", "b", "c", "d/x", "e"];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10))]

        #[test]
        fn three_generations_match_a_model(
            first in proptest::collection::vec((0u64..40, 1usize..9_000), 5),
            ops in proptest::collection::vec(
                proptest::collection::vec((0u8..3, 0u64..40, 1usize..9_000), 5), 2),
        ) {
            let mut model: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            let mut g0 = Vec::new();
            let mut w = Writer::new(&mut g0, options(0)).unwrap();
            for (n, (seed, len)) in NAMES.iter().zip(&first) {
                let d = pattern(*seed, *len);
                w.add_file(n, EntryFlags::EMPTY, 0, &mut &d[..]).unwrap();
                model.insert((*n).to_string(), d);
            }
            w.finish().unwrap();
            let mut snaps = vec![g0.clone()];
            let mut models = vec![model.clone()];
            let mut cur = g0;
            for gen_ops in &ops {
                let (next, _) = append_gen(&cur, options(0), None, |w| {
                    for (n, (op, seed, len)) in NAMES.iter().zip(gen_ops) {
                        match op {
                            1 => {
                                let d = pattern(*seed, *len);
                                w.add_file(n, EntryFlags::EMPTY, 0, &mut &d[..]).unwrap();
                                model.insert((*n).to_string(), d);
                            }
                            2 => {
                                w.delete_path(n).unwrap();
                                model.remove(*n);
                            }
                            _ => {}
                        }
                    }
                });
                cur = next;
                snaps.push(cur.clone());
                models.push(model.clone());
            }
            for (snap, m) in snaps.iter().zip(&models) {
                let got = extract_all(snap, None);
                let want: Vec<_> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                prop_assert!(got == want);
                open(snap).verify().unwrap();
            }
            // Rolling the last archive back reproduces each earlier one.
            let f = temp_file(&cur);
            let mut file = std::fs::OpenOptions::new().read(true).write(true).open(f.path()).unwrap();
            for g in [1u64, 0] {
                rollback(&mut file, g).unwrap();
                prop_assert!(read(&f) == snaps[g as usize]);
            }
        }
    }
}
