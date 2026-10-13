//! Recovery records through the pipeline (E2-13): archives at 3 and 10 percent, random
//! overwrites up to each group's bound repaired and verified, damage beyond the bound reported,
//! and the size overhead equal to the configured percentage within the frame rounding
//! (spec section 13: `ceil(data_shards * percent / 100)` recovery shards per group, at least
//! one, each a whole shard; plus the frame's envelope, head and one hash per data shard; plus
//! the frame's index entry).
#![allow(clippy::unwrap_used)]

use std::io::{Cursor, Seek, SeekFrom};
use std::path::Path;

use lpk_core::{
    extract_file, register_full_reader, ChunkerKind, Dedup, DefaultPolicy, ExtractOptions,
    FastOptions, FoldOptions, Pipeline, RunSummary,
};
use lpk_format::recovery::RECOVERY_HEAD_LEN;
use lpk_format::{
    repair_with_report, Archive, FormatError, Frame, FrameKind, FrameLocation, ReadFrame,
    ReadLimits, RecoveryFrame, RecoveryOptions, Resources,
};

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

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut out = Vec::new();
    for b in noise(seed, len) {
        out.extend_from_slice(WORDS[(b % 8) as usize].as_bytes());
        out.push(b' ');
        if out.len() >= len {
            break;
        }
    }
    out.truncate(len);
    out
}

/// A tree whose Fast archive spans several recovery groups at the test's geometry.
fn make_tree(root: &Path) {
    std::fs::create_dir_all(root.join("d")).unwrap();
    std::fs::write(root.join("a.txt"), prose(1, 900_000)).unwrap();
    std::fs::write(root.join("d/b.md"), prose(2, 300_000)).unwrap();
    std::fs::write(root.join("d/r.bin"), noise(3, 700_000)).unwrap();
    std::fs::write(root.join("d/s.bin"), noise(4, 333_333)).unwrap();
    std::fs::write(root.join("zero"), b"").unwrap();
}

/// Small shards and groups, so a test archive has several groups and a group's bound is a
/// few shards: shard 4096 bytes, 64 shards (256 KiB) per group, 128 KiB blocks (and chunks of
/// at most 64 KiB, so a block holds several).
const SHARD: u32 = 4096;
const GROUP: u32 = 64;

fn archive(src: &Path, path: &Path, percent: u8) -> RunSummary {
    let mut p = Pipeline::fast(FastOptions {
        block_size: 128 << 10,
        ..FastOptions::default()
    });
    p.fold = Some(Box::new(Dedup::new(FoldOptions {
        chunker: ChunkerKind::Cdc {
            min: 8 << 10,
            avg: 32 << 10,
            max: 64 << 10,
        },
        ..FoldOptions::default()
    })));
    p.seal.writer.recovery = RecoveryOptions {
        percent,
        shard_len: SHARD,
        group_shards: GROUP,
    };
    p.run_file(src, path).unwrap()
}

/// The recovery frames of an archive: location and parsed payload.
fn frames(bytes: &[u8]) -> Vec<(FrameLocation, RecoveryFrame)> {
    let a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    let index_offset = a.trailer().index_offset;
    let limits = ReadLimits {
        max_payload: Resources::default().max_frame_payload,
    };
    let mut out = Vec::new();
    for loc in a.recovery_frames() {
        let mut r = Cursor::new(bytes);
        r.seek(SeekFrom::Start(loc.offset)).unwrap();
        let f = match Frame::read(&mut r, &limits).unwrap().unwrap() {
            ReadFrame::Known(f) => f,
            ReadFrame::Unknown { kind, .. } => panic!("unknown frame kind {kind}"),
        };
        assert_eq!(f.kind, FrameKind::Recovery);
        assert_eq!(f.encoded_len(), loc.len);
        out.push((
            *loc,
            RecoveryFrame::parse(&f.payload, index_offset).unwrap(),
        ));
    }
    out
}

/// Overwrite `count` distinct data shards of `frame` with random bytes (a run of 1 to 64
/// bytes at a random place inside each shard), picked with the test's own generator.
fn damage(bytes: &mut [u8], frame: &RecoveryFrame, count: u32, seed: u64) -> Vec<u32> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    assert!(count <= frame.data_shards);
    let mut shards: Vec<u32> = Vec::new();
    while shards.len() < count as usize {
        let s = (next() % u64::from(frame.data_shards)) as u32;
        if !shards.contains(&s) {
            shards.push(s);
        }
    }
    let cover_end = frame.cover_offset + frame.cover_len;
    for &s in &shards {
        let start = frame.cover_offset + u64::from(s) * u64::from(frame.shard_len);
        let end = (start + u64::from(frame.shard_len)).min(cover_end);
        let len = 1 + (next() % 64).min(end - start - 1);
        let at = start + next() % (end - start - len + 1);
        for i in 0..len {
            let i = (at + i) as usize;
            let before = bytes[i];
            let mut b = (next() & 0xFF) as u8;
            if b == before {
                b ^= 0x5A;
            }
            bytes[i] = b;
        }
    }
    shards
}

fn verify_full(bytes: Vec<u8>) -> Result<(), FormatError> {
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default())?;
    register_full_reader(&mut a);
    a.verify().map(|_| ())
}

fn repair(bytes: &[u8]) -> (Vec<u8>, lpk_format::RepairReport, Option<FormatError>) {
    let mut out = Cursor::new(Vec::new());
    let (report, unrepairable) =
        repair_with_report(Cursor::new(bytes.to_vec()), &mut out, &Resources::default()).unwrap();
    (out.into_inner(), report, unrepairable)
}

#[test]
fn damage_up_to_every_group_bound_repairs_and_verifies() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    for percent in [3u8, 10] {
        let path = work.path().join(format!("{percent}.lpk"));
        archive(src.path(), &path, percent);
        let good = std::fs::read(&path).unwrap();
        verify_full(good.clone()).unwrap();
        let frames = frames(&good);
        assert!(frames.len() >= 4, "{percent}%: {} groups", frames.len());
        let mut bytes = good.clone();
        let mut damaged = 0u64;
        for (i, (_, f)) in frames.iter().enumerate() {
            let bound = u64::from(f.data_shards) * u64::from(percent);
            assert_eq!(
                u64::from(f.recovery_shards),
                bound.div_ceil(100).max(1),
                "group {i}"
            );
            damaged +=
                u64::from(damage(&mut bytes, f, f.recovery_shards, 1000 + i as u64).len() as u32);
        }
        assert!(bytes != good);
        // Damaged: the frame hashes fail, so verify fails and the extraction is refused.
        assert!(verify_full(bytes.clone()).is_err(), "{percent}%");
        let (repaired, report, unrepairable) = repair(&bytes);
        assert!(unrepairable.is_none(), "{percent}%: {unrepairable:?}");
        assert_eq!(report.frames, frames.len() as u64);
        assert_eq!(report.frames_unusable, 0);
        assert_eq!(report.shards_damaged, damaged, "{percent}%");
        assert_eq!(report.shards_repaired, damaged, "{percent}%");
        assert!(
            repaired == good,
            "{percent}%: the repaired copy is the original"
        );
        verify_full(repaired.clone()).unwrap();
        let fixed = work.path().join(format!("{percent}-fixed.lpk"));
        std::fs::write(&fixed, &repaired).unwrap();
        let out = work.path().join(format!("{percent}-out"));
        extract_file(
            &fixed,
            &out,
            &[],
            None,
            &DefaultPolicy,
            &ExtractOptions::default(),
        )
        .unwrap();
        for name in ["a.txt", "d/b.md", "d/r.bin", "d/s.bin", "zero"] {
            assert!(
                std::fs::read(out.join(name)).unwrap()
                    == std::fs::read(src.path().join(name)).unwrap(),
                "{percent}%: {name}"
            );
        }
    }
}

#[test]
fn damage_beyond_a_group_bound_is_reported_not_repaired() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("a.lpk");
    archive(src.path(), &path, 3);
    let good = std::fs::read(&path).unwrap();
    let frames = frames(&good);
    let mut bytes = good.clone();
    // One shard too many in the second group; the first group within its bound.
    let (_, f0) = &frames[0];
    let (_, f1) = &frames[1];
    let n0 = damage(&mut bytes, f0, f0.recovery_shards, 7).len() as u64;
    let over = f1.recovery_shards + 1;
    assert!(over <= f1.data_shards);
    let n1 = damage(&mut bytes, f1, over, 8).len() as u64;
    let (repaired, report, unrepairable) = repair(&bytes);
    match unrepairable {
        Some(FormatError::Unrepairable {
            frame,
            damaged,
            capacity,
        }) => {
            assert_eq!(frame, 1);
            assert_eq!(damaged, u64::from(over));
            assert_eq!(capacity, u64::from(f1.recovery_shards));
        }
        other => panic!("expected Unrepairable, got {other:?}"),
    }
    assert_eq!(report.shards_damaged, n0 + n1);
    assert_eq!(report.shards_repaired, n0);
    // The copy carries the first group's repair, not the second's; it still fails to verify.
    assert!(repaired != good);
    assert!(verify_full(repaired.clone()).is_err());
    let r0 = f0.cover_offset as usize..(f0.cover_offset + f0.cover_len) as usize;
    assert!(repaired[r0.clone()] == good[r0]);
    // `check` on the damaged bytes reports every damaged shard and repairs nothing.
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
    let c = a.check_recovery().unwrap();
    assert_eq!(c.shards_damaged, n0 + n1);
    assert_eq!(c.shards_repaired, 0);
}

#[test]
fn the_overhead_is_the_configured_percentage_within_the_frame_rounding() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let plain_path = work.path().join("0.lpk");
    let plain = archive(src.path(), &plain_path, 0);
    assert!(frames(&std::fs::read(&plain_path).unwrap()).is_empty());
    for percent in [3u8, 10, 20] {
        let path = work.path().join(format!("{percent}.lpk"));
        let s = archive(src.path(), &path, percent);
        let bytes = std::fs::read(&path).unwrap();
        let frames = frames(&bytes);
        let overhead = s.writer.archive_len - plain.writer.archive_len;
        // Exact: every recovery frame as written plus its index entry (three varints).
        let mut exact = 0u64;
        let mut parity = 0u64;
        let mut covered = 0u64;
        let mut fixed = 0u64;
        for (loc, f) in &frames {
            let payload = RECOVERY_HEAD_LEN as u64
                + 32 * u64::from(f.data_shards)
                + u64::from(f.recovery_shards) * u64::from(f.shard_len);
            let frame_len = 4 + lpk_format::varint::len(payload) as u64 + payload + 32;
            assert_eq!(frame_len, loc.len);
            let entry = (lpk_format::varint::len(loc.offset)
                + lpk_format::varint::len(loc.len)
                + lpk_format::varint::len(loc.sequence)) as u64;
            exact += frame_len + entry;
            parity += u64::from(f.recovery_shards) * u64::from(f.shard_len);
            covered += f.cover_len;
            fixed += frame_len + entry - u64::from(f.recovery_shards) * u64::from(f.shard_len);
        }
        assert_eq!(overhead, exact, "{percent}%");
        // The percentage: the parity is `percent` of the covered bytes, rounded up to whole
        // shards once per group (at least one shard per group); the rest of the overhead is
        // the per-frame fixed cost (envelope, head, one hash per data shard, index entry).
        let nominal = covered * u64::from(percent) / 100;
        assert!(parity >= nominal, "{percent}%: {parity} < {nominal}");
        let rounding = frames.len() as u64 * u64::from(SHARD);
        assert!(
            parity - nominal <= rounding,
            "{percent}%: parity {parity}, nominal {nominal}, {} groups",
            frames.len()
        );
        assert_eq!(overhead - parity, fixed);
        println!(
            "{percent}%: plain {} B, with recovery {} B, overhead {overhead} B = parity {parity} (nominal {nominal}) + fixed {fixed}; {} groups over {covered} covered bytes",
            plain.writer.archive_len,
            s.writer.archive_len,
            frames.len()
        );
    }
}

#[test]
fn the_default_geometry_covers_a_fast_archive_in_one_group() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("a.lpk");
    let mut p = Pipeline::fast(FastOptions::default());
    p.seal.writer.recovery.percent = 5;
    p.run_file(src.path(), &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let frames = frames(&bytes);
    assert_eq!(frames.len(), 1);
    let (_, f) = &frames[0];
    assert_eq!(f.shard_len, lpk_format::DEFAULT_SHARD_LEN);
    assert_eq!(f.cover_offset, 32);
    let a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    assert_eq!(f.cover_offset + f.cover_len, frames[0].0.offset);
    assert!(frames[0].0.offset + frames[0].0.len <= a.trailer().index_offset);
    // A damaged shard inside the one group repairs.
    let mut damaged = bytes.clone();
    damage(&mut damaged, f, 1, 5);
    let (repaired, _, unrepairable) = repair(&damaged);
    assert!(unrepairable.is_none());
    assert!(repaired == bytes);
}
