//! Recovery frames: the writer's incremental encoding, repair and detection (spec section 13).
#![allow(clippy::unwrap_used)]

use lpk_format::{
    Archive, EntryFlags, FormatError, FrameKind, RecoveryFrame, RecoveryOptions, Resources, Writer,
    WriterOptions,
};
use std::io::Cursor;

/// Deterministic pseudo-random bytes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 24) as u8).collect()
    }
}

pub const SHARD: u32 = 4096;

pub fn options(percent: u8) -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [9; 16],
        recovery: RecoveryOptions {
            percent,
            shard_len: SHARD,
        },
        ..WriterOptions::default()
    }
}

/// A tree of files of mixed sizes (about 700 KiB in all).
pub fn tree() -> Vec<(String, Vec<u8>)> {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let sizes = [0usize, 1, 4095, 4096, 4097, 70_000, 123_456, 300_000, 9_999];
    let mut files: Vec<(String, Vec<u8>)> = sizes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let data = if i % 3 == 0 {
                vec![(i % 251) as u8; *n]
            } else {
                rng.bytes(*n)
            };
            (format!("f{i:02}"), data)
        })
        .collect();
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    files
}

pub fn pack(options: WriterOptions, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options).unwrap();
    for (path, data) in files {
        w.add_file(path, EntryFlags::EMPTY, 5, &mut data.as_slice())
            .unwrap();
    }
    w.finish().unwrap();
    out
}

pub fn open(bytes: &[u8]) -> Archive<Cursor<Vec<u8>>> {
    Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap()
}

/// The recovery frame of an archive written with recovery.
pub fn frame_of(bytes: &[u8]) -> RecoveryFrame {
    let mut a = open(bytes);
    let at = a.recovery_frames()[0];
    let index_at = a.trailer().index_offset;
    let f = a.read_frame_at(at, FrameKind::Recovery).unwrap();
    RecoveryFrame::parse(&f.payload, index_at).unwrap()
}

#[test]
fn no_recovery_by_default() {
    let files = tree();
    let plain = pack(WriterOptions::default(), &files);
    let a = open(&plain);
    assert!(a.recovery_frames().is_empty());
    let zero = pack(options(0), &files);
    let a = open(&zero);
    assert!(a.recovery_frames().is_empty());
}

#[test]
fn writer_makes_one_frame_with_the_right_geometry_and_hashes() {
    let files = tree();
    let bytes = pack(options(5), &files);
    let mut a = open(&bytes);
    assert_eq!(a.recovery_frames().len(), 1);
    a.verify().unwrap();
    let f = frame_of(&bytes);
    // The covered range runs from the end of the header to the index.
    assert_eq!(f.cover_offset, 32);
    let index_at = a.trailer().index_offset;
    let loc = a.recovery_frames()[0];
    assert_eq!(f.cover_offset + f.cover_len, loc.offset);
    assert!(loc.offset + loc.len <= index_at);
    assert_eq!(f.shard_len, SHARD);
    assert_eq!(u64::from(f.data_shards), f.cover_len.div_ceil(4096));
    assert_eq!(
        u64::from(f.recovery_shards),
        (u64::from(f.data_shards) * 5).div_ceil(100)
    );
    // Each stored hash is the hash of the zero-padded shard.
    let mut shards: Vec<Vec<u8>> = Vec::new();
    for i in 0..f.data_shards {
        let (off, len) = f.shard_range(i);
        let mut s = bytes[off as usize..off as usize + len].to_vec();
        s.resize(SHARD as usize, 0);
        assert_eq!(f.shard_hashes[i as usize], *blake3::hash(&s).as_bytes());
        shards.push(s);
    }
    // The recovery shards are what the library makes of the same shards.
    let want =
        reed_solomon_simd::encode(f.data_shards as usize, f.recovery_shards as usize, &shards)
            .unwrap();
    let got: Vec<&[u8]> = f.recovery.chunks(SHARD as usize).collect();
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(*g, w.as_slice());
    }
}

#[test]
fn bad_options_are_refused() {
    assert!(matches!(
        Writer::new(Vec::new(), options(21)),
        Err(FormatError::BadOptions { .. })
    ));
    for shard_len in [0u32, 100, 4097] {
        let mut o = options(5);
        o.recovery.shard_len = shard_len;
        assert!(matches!(
            Writer::new(Vec::new(), o),
            Err(FormatError::BadOptions { .. })
        ));
    }
    // With percent 0 the shard length is not looked at.
    let mut o = options(0);
    o.recovery.shard_len = 100;
    assert!(Writer::new(Vec::new(), o).is_ok());
}

#[test]
fn the_shard_cap_is_an_options_error() {
    let mut o = options(1);
    o.recovery.shard_len = 64;
    let mut sink = Vec::new();
    let mut w = Writer::new(&mut sink, o).unwrap();
    let data = vec![7u8; 2 * 1024 * 1024 + 100_000];
    let r = w.add_file("big", EntryFlags::EMPTY, 0, &mut data.as_slice());
    assert!(matches!(
        r,
        Err(FormatError::BadOptions {
            reason: "recovery shards"
        })
    ));
    // Every later call fails the same way.
    assert!(matches!(
        w.add_file("y", EntryFlags::EMPTY, 0, &mut [1u8].as_slice()),
        Err(FormatError::BadOptions {
            reason: "recovery shards"
        })
    ));
    assert!(matches!(
        w.finish(),
        Err(FormatError::BadOptions {
            reason: "recovery shards"
        })
    ));
}

#[test]
fn the_writer_holds_at_most_one_shard_of_covered_bytes() {
    let mut o = options(5);
    o.recovery.shard_len = 1 << 16;
    o.chunk_size = 1 << 16;
    o.block_size = 1 << 20;
    let mut sink = Vec::new();
    let mut w = Writer::new(&mut sink, o).unwrap();
    let mut rng = Rng(77);
    let mut peak = 0usize;
    // 20 MiB, one 1 MiB file at a time.
    for i in 0..20 {
        let data = rng.bytes(1 << 20);
        w.add_file(
            &format!("f{i:02}"),
            EntryFlags::EMPTY,
            0,
            &mut data.as_slice(),
        )
        .unwrap();
        peak = peak.max(w.recovery_buffered());
    }
    assert!(peak <= 1 << 16, "peak {peak}");
    let summary = w.finish().unwrap();
    assert!(summary.archive_len > 20 << 20);
    let mut a = open(&sink);
    a.verify().unwrap();
    assert_eq!(a.recovery_frames().len(), 1);
}

// ---- repair and detection ----

fn repair_bytes(bytes: &[u8]) -> (Result<lpk_format::RepairReport, FormatError>, Vec<u8>) {
    let mut out = Cursor::new(Vec::new());
    let r = lpk_format::repair(Cursor::new(bytes.to_vec()), &mut out, &Resources::default());
    (r, out.into_inner())
}

/// Damage every byte of shard `s`.
fn smash_shard(bytes: &mut [u8], f: &RecoveryFrame, s: u32) {
    let (off, len) = f.shard_range(s);
    for b in &mut bytes[off as usize..off as usize + len] {
        *b ^= 0xA5;
    }
}

/// Damage a few scattered bytes of shard `s`.
fn nick_shard(bytes: &mut [u8], f: &RecoveryFrame, s: u32, rng: &mut Rng) {
    let (off, len) = f.shard_range(s);
    for _ in 0..1 + rng.next() % 7 {
        let at = off as usize + (rng.next() % len as u64) as usize;
        bytes[at] ^= 1 + (rng.next() % 255) as u8;
    }
}

/// `k` distinct shard numbers below `n`.
fn pick(rng: &mut Rng, n: u32, k: usize) -> Vec<u32> {
    let mut v: Vec<u32> = Vec::new();
    while v.len() < k {
        let s = (rng.next() % u64::from(n)) as u32;
        if !v.contains(&s) {
            v.push(s);
        }
    }
    v.sort_unstable();
    v
}

#[test]
fn an_undamaged_archive_is_copied_unchanged() {
    let bytes = pack(options(5), &tree());
    let (r, copy) = repair_bytes(&bytes);
    let r = r.unwrap();
    assert_eq!(copy, bytes);
    assert_eq!(
        (
            r.frames,
            r.frames_unusable,
            r.shards_damaged,
            r.shards_repaired
        ),
        (1, 0, 0, 0)
    );
    let c = open(&bytes).check_recovery().unwrap();
    assert_eq!(c, r);
}

#[test]
fn archives_without_recovery_are_copied_and_report_nothing() {
    let bytes = pack(options(0), &tree());
    let (r, copy) = repair_bytes(&bytes);
    assert_eq!(r.unwrap(), lpk_format::RepairReport::default());
    assert_eq!(copy, bytes);
}

#[test]
fn damage_up_to_capacity_is_repaired_exactly() {
    let files = tree();
    let good = pack(options(5), &files);
    let f = frame_of(&good);
    let cap = f.recovery_shards as usize;
    assert!(cap >= 2 && f.data_shards > 20);
    let mut rng = Rng(0xDEAD_BEEF);
    for round in 0..24 {
        let k = if round == 0 {
            cap
        } else {
            1 + (rng.next() as usize % cap)
        };
        let mut shards = pick(&mut rng, f.data_shards, k);
        if round == 1 {
            // The last shard is shorter than the others.
            shards[0] = f.data_shards - 1;
            shards.sort_unstable();
            shards.dedup();
        }
        let mut bad = good.clone();
        for &s in &shards {
            if round % 2 == 0 {
                smash_shard(&mut bad, &f, s);
            } else {
                nick_shard(&mut bad, &f, s, &mut rng);
            }
        }
        let a = open(&bad).check_recovery().unwrap();
        assert_eq!(a.shards_damaged, shards.len() as u64);
        assert_eq!(a.shards_repaired, 0);
        let (r, copy) = repair_bytes(&bad);
        let r = r.unwrap();
        assert_eq!(r.shards_damaged, shards.len() as u64, "round {round}");
        assert_eq!(r.shards_repaired, shards.len() as u64);
        assert!(copy == good, "round {round}: copy differs");
        let mut fixed = open(&copy);
        fixed.verify().unwrap();
        assert_eq!(fixed.check_recovery().unwrap().shards_damaged, 0);
    }
}

#[test]
fn one_more_than_capacity_is_unrepairable() {
    let good = pack(options(5), &tree());
    let f = frame_of(&good);
    let cap = u64::from(f.recovery_shards);
    let mut rng = Rng(5);
    let shards = pick(&mut rng, f.data_shards, cap as usize + 1);
    let mut bad = good.clone();
    for &s in &shards {
        smash_shard(&mut bad, &f, s);
    }
    let (r, copy) = repair_bytes(&bad);
    match r {
        Err(FormatError::Unrepairable {
            frame,
            damaged,
            capacity,
        }) => {
            assert_eq!((frame, damaged, capacity), (0, cap + 1, cap));
        }
        other => panic!("expected Unrepairable, got {other:?}"),
    }
    // Nothing could be rebuilt: the copy is the damaged archive, written whole.
    assert_eq!(copy, bad);
}

#[test]
fn damage_in_the_recovery_frame_leaves_the_data_alone() {
    let good = pack(options(5), &tree());
    let f = frame_of(&good);
    let loc = open(&good).recovery_frames()[0];
    // Damage a recovery shard byte, and a data shard as well.
    let mut bad = good.clone();
    bad[(loc.offset + loc.len - 100) as usize] ^= 0xFF;
    smash_shard(&mut bad, &f, 3);
    let (r, copy) = repair_bytes(&bad);
    let r = r.unwrap();
    assert_eq!((r.frames, r.frames_unusable), (1, 1));
    assert_eq!((r.shards_damaged, r.shards_repaired), (0, 0));
    assert_eq!(copy, bad, "no false repair");
    let c = open(&bad).check_recovery().unwrap();
    assert_eq!(c.frames_unusable, 1);
}

#[test]
fn damage_in_the_index_is_the_open_error() {
    let good = pack(options(5), &tree());
    let a = open(&good);
    let at = a.trailer().index_offset as usize;
    let mut bad = good.clone();
    bad[at + 10] ^= 0xFF;
    let want = Archive::open(Cursor::new(bad.clone()), &Resources::default()).unwrap_err();
    let (r, copy) = repair_bytes(&bad);
    let got = r.unwrap_err();
    assert_eq!(got.to_string(), want.to_string());
    assert!(copy.is_empty());
}

mod two_frames {
    use super::*;
    use lpk_format::{
        merkle_root, ArchiveSizes, ChunkTableWriter, Envelope, Frame, FrameFlags, FrameLocation,
        GraphResources, Header, HeaderFlags, Index, Trailer,
    };

    fn frame_bytes(kind: FrameKind, payload: Vec<u8>) -> Vec<u8> {
        let mut v = Vec::new();
        Frame {
            kind,
            flags: FrameFlags::EMPTY,
            payload,
        }
        .write(&mut v)
        .unwrap();
        v
    }

    /// An archive with two recovery frames, each over half of the body.
    pub fn build() -> (Vec<u8>, Vec<RecoveryFrame>) {
        let id = [3u8; 16];
        let mut rng = Rng(99);
        let mut bytes = Vec::new();
        Header::new(HeaderFlags::EMPTY, id)
            .write(&mut bytes)
            .unwrap();
        let entry_off = bytes.len() as u64;
        let e = frame_bytes(FrameKind::EntryTable, rng.bytes(3000));
        bytes.extend_from_slice(&e);
        let rec_off = bytes.len() as u64;
        let r = frame_bytes(FrameKind::Records, rng.bytes(3000));
        bytes.extend_from_slice(&r);
        let body_end = bytes.len() as u64;
        let mid = 32 + (body_end - 32) / 2;
        let mut frames = Vec::new();
        let mut locs = Vec::new();
        for (from, to) in [(32u64, mid), (mid, body_end)] {
            let cover = &bytes[from as usize..to as usize];
            let data = cover.len().div_ceil(64);
            let mut shards: Vec<Vec<u8>> = cover.chunks(64).map(<[u8]>::to_vec).collect();
            for s in &mut shards {
                s.resize(64, 0);
            }
            let rec = 3usize;
            let coded = reed_solomon_simd::encode(data, rec, &shards).unwrap();
            let frame = RecoveryFrame {
                cover_offset: from,
                cover_len: to - from,
                shard_len: 64,
                data_shards: data as u32,
                recovery_shards: rec as u32,
                shard_hashes: shards.iter().map(|s| *blake3::hash(s).as_bytes()).collect(),
                recovery: coded.concat(),
            };
            let fb = frame_bytes(FrameKind::Recovery, frame.encode().unwrap());
            locs.push(FrameLocation {
                offset: bytes.len() as u64,
                len: fb.len() as u64,
            });
            bytes.extend_from_slice(&fb);
            frames.push(frame);
        }
        let index_off = bytes.len() as u64;
        let mut index = Index {
            chunk_table: ChunkTableWriter::encode(&[]).into(),
            merkle_root: merkle_root(&[]),
            envelope: Envelope {
                max_window: 0,
                max_bwt_block: 0,
                max_block_plain: 0,
                max_frame_payload: 0,
                decode_memory: 0,
                threads_hint: 0,
            },
            priors: vec![],
            blocks: vec![],
            entry_table: FrameLocation {
                offset: entry_off,
                len: e.len() as u64,
            },
            records: Some(FrameLocation {
                offset: rec_off,
                len: r.len() as u64,
            }),
            recovery: locs.clone(),
        };
        let mut guess = 0u64;
        let payload = loop {
            index.envelope = Envelope::for_archive(
                &[],
                ArchiveSizes {
                    index_payload_len: guess,
                    entry_table_len: e.len() as u64,
                    records_len: r.len() as u64,
                    recovery_len: locs.iter().map(|l| l.len).max().unwrap(),
                },
                GraphResources::default(),
                0,
                0,
            );
            let p = index.encode().unwrap();
            if p.len() as u64 == guess {
                break p;
            }
            guess = p.len() as u64;
        };
        let hash = *blake3::hash(&payload).as_bytes();
        let ib = frame_bytes(FrameKind::Index, payload);
        bytes.extend_from_slice(&ib);
        Trailer {
            index_offset: index_off,
            index_len: ib.len() as u64,
            index_hash: hash,
            generation: 0,
            archive_id: id,
        }
        .write(&mut bytes)
        .unwrap();
        (bytes, frames)
    }
}

#[test]
fn two_frames_are_repaired_independently() {
    let (good, frames) = two_frames::build();
    let mut a = open(&good);
    assert_eq!(a.recovery_frames().len(), 2);
    assert_eq!(a.check_recovery().unwrap().shards_damaged, 0);
    // Both frames damaged within capacity: both repaired.
    let mut bad = good.clone();
    smash_shard(&mut bad, &frames[0], 0);
    smash_shard(&mut bad, &frames[0], 2);
    smash_shard(&mut bad, &frames[1], 1);
    let (r, copy) = repair_bytes(&bad);
    let r = r.unwrap();
    assert_eq!((r.frames, r.shards_damaged, r.shards_repaired), (2, 3, 3));
    assert_eq!(copy, good);
    // Frame 0 beyond capacity, frame 1 within: frame 1 is still repaired.
    let mut bad = good.clone();
    for s in 0..4 {
        smash_shard(&mut bad, &frames[0], s);
    }
    smash_shard(&mut bad, &frames[1], 0);
    let (r, copy) = repair_bytes(&bad);
    match r {
        Err(FormatError::Unrepairable {
            frame: 0,
            damaged: 4,
            capacity: 3,
        }) => {}
        other => panic!("expected Unrepairable for frame 0, got {other:?}"),
    }
    let (off, len) = frames[1].shard_range(0);
    let range = off as usize..off as usize + len;
    assert_eq!(copy[range.clone()], good[range]);
    let c = frames[0].cover_len as usize + 32;
    assert_eq!(copy[32..c], bad[32..c], "frame 0's coverage is as damaged");
}
