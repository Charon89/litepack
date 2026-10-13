//! Recovery records against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test recovery_corpus -- --ignored --nocapture`
//!
//! The `backup-versions` class through the Fast tier at 0, 3 and 10 percent recovery (the
//! format's default geometry: 64 KiB shards, 2048 shards per group). For 3 and 10: every
//! group's data shards are overwritten at random up to that group's bound, the archive is
//! repaired, the copy equals the original and verifies with the full reader; the size overhead
//! against the plain archive is reported as parity (the configured share of the covered bytes,
//! rounded up to whole shards once per group) plus the per-frame fixed cost. Figures are a
//! report only.
#![allow(clippy::unwrap_used)]

use std::io::{Cursor, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{register_full_reader, FastOptions, Pipeline};
use lpk_format::recovery::RECOVERY_HEAD_LEN;
use lpk_format::{
    repair_with_report, Archive, Frame, FrameKind, FrameLocation, ReadFrame, ReadLimits,
    RecoveryFrame, Resources,
};

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
        out.push((
            *loc,
            RecoveryFrame::parse(&f.payload, index_offset).unwrap(),
        ));
    }
    out
}

/// Overwrite `count` distinct data shards of `frame` with random runs of 1 to 256 bytes.
fn damage(bytes: &mut [u8], frame: &RecoveryFrame, count: u32, seed: u64) -> u64 {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut shards: Vec<u32> = Vec::new();
    while shards.len() < count.min(frame.data_shards) as usize {
        let s = (next() % u64::from(frame.data_shards)) as u32;
        if !shards.contains(&s) {
            shards.push(s);
        }
    }
    let cover_end = frame.cover_offset + frame.cover_len;
    for &s in &shards {
        let start = frame.cover_offset + u64::from(s) * u64::from(frame.shard_len);
        let end = (start + u64::from(frame.shard_len)).min(cover_end);
        let len = 1 + (next() % 256).min(end - start - 1);
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
    shards.len() as u64
}

fn archive(dir: &Path, path: &Path, percent: u8) -> (u64, f64) {
    let mut p = Pipeline::fast(FastOptions::default());
    p.seal.writer.recovery.percent = percent;
    let t0 = Instant::now();
    let s = p.run_file(dir, path).unwrap();
    (s.writer.archive_len, t0.elapsed().as_secs_f64())
}

#[test]
#[ignore]
fn backup_versions_damaged_to_the_bound_repairs_and_the_overhead_is_the_percentage() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let dir = root.join("backup-versions");
    assert!(dir.is_dir(), "{}", dir.display());
    let work = tempfile::tempdir().unwrap();
    let (plain_len, plain_secs) = archive(&dir, &work.path().join("0.lpk"), 0);
    println!("| percent | archive B | write s | overhead B | parity B | nominal B | fixed B | groups | shards damaged | repair s |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    println!("| 0 | {plain_len} | {plain_secs:.2} | 0 | 0 | 0 | 0 | 0 | - | - |");
    for percent in [3u8, 10] {
        let path = work.path().join(format!("{percent}.lpk"));
        let (len, secs) = archive(&dir, &path, percent);
        let good = std::fs::read(&path).unwrap();
        let frames = frames(&good);
        let mut parity = 0u64;
        let mut covered = 0u64;
        let mut exact = 0u64;
        for (loc, f) in &frames {
            let payload = RECOVERY_HEAD_LEN as u64
                + 32 * u64::from(f.data_shards)
                + u64::from(f.recovery_shards) * u64::from(f.shard_len);
            assert_eq!(
                4 + lpk_format::varint::len(payload) as u64 + payload + 32,
                loc.len
            );
            exact += loc.len
                + (lpk_format::varint::len(loc.offset)
                    + lpk_format::varint::len(loc.len)
                    + lpk_format::varint::len(loc.sequence)) as u64;
            parity += u64::from(f.recovery_shards) * u64::from(f.shard_len);
            covered += f.cover_len;
        }
        let overhead = len - plain_len;
        assert_eq!(
            overhead, exact,
            "{percent}%: the overhead is the frames and their index entries"
        );
        let nominal = covered * u64::from(percent) / 100;
        assert!(parity >= nominal);
        assert!(
            parity - nominal <= frames.len() as u64 * u64::from(frames[0].1.shard_len),
            "{percent}%: parity {parity}, nominal {nominal}, {} groups",
            frames.len()
        );
        let mut bytes = good.clone();
        let mut damaged = 0u64;
        for (i, (_, f)) in frames.iter().enumerate() {
            damaged += damage(&mut bytes, f, f.recovery_shards, 4242 + i as u64);
        }
        let t0 = Instant::now();
        let mut out = Cursor::new(Vec::new());
        let (report, unrepairable) =
            repair_with_report(Cursor::new(bytes), &mut out, &Resources::default()).unwrap();
        let repair_secs = t0.elapsed().as_secs_f64();
        assert!(unrepairable.is_none(), "{percent}%: {unrepairable:?}");
        assert_eq!(report.shards_damaged, damaged);
        assert_eq!(report.shards_repaired, damaged);
        let repaired = out.into_inner();
        assert!(
            repaired == good,
            "{percent}%: the repaired copy is the original"
        );
        let mut a = Archive::open(Cursor::new(repaired), &Resources::default()).unwrap();
        register_full_reader(&mut a);
        a.verify().unwrap();
        println!(
            "| {percent} | {len} | {secs:.2} | {overhead} | {parity} | {nominal} | {} | {} | {damaged} | {repair_secs:.2} |",
            overhead - parity,
            frames.len()
        );
    }
}
