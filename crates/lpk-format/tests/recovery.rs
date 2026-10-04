//! Recovery groups: the writer's group encoding, repair and detection (spec section 13).
#![allow(clippy::unwrap_used)]

use lpk_format::{
    Archive, EntryFlags, FormatError, FrameKind, RecoveryFrame, RecoveryOptions, Resources, Writer,
    WriterOptions,
};
use std::cell::RefCell;
use std::io::{Cursor, Write};
use std::rc::Rc;

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

const SHARD: u32 = 4096;
const GROUP: u32 = 16;

fn options(percent: u8) -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [9; 16],
        recovery: RecoveryOptions {
            percent,
            shard_len: SHARD,
            group_shards: GROUP,
        },
        ..WriterOptions::default()
    }
}

/// A tree of files of mixed sizes (about 700 KiB in all).
fn tree() -> Vec<(String, Vec<u8>)> {
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

fn pack(options: WriterOptions, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options).unwrap();
    for (path, data) in files {
        w.add_file(path, EntryFlags::EMPTY, 5, &mut data.as_slice())
            .unwrap();
    }
    w.finish().unwrap();
    out
}

fn open(bytes: &[u8]) -> Archive<Cursor<Vec<u8>>> {
    Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap()
}

/// The recovery frames of an archive, in the index's order.
fn frames_of(bytes: &[u8]) -> Vec<RecoveryFrame> {
    let mut a = open(bytes);
    let index_at = a.trailer().index_offset;
    let locs = a.recovery_frames().to_vec();
    locs.into_iter()
        .map(|at| {
            let f = a.read_frame_at(at, FrameKind::Recovery).unwrap();
            RecoveryFrame::parse(&f.payload, index_at).unwrap()
        })
        .collect()
}

#[test]
fn no_recovery_by_default() {
    let files = tree();
    assert!(open(&pack(WriterOptions::default(), &files))
        .recovery_frames()
        .is_empty());
    assert!(open(&pack(options(0), &files)).recovery_frames().is_empty());
}

#[test]
fn writer_makes_one_frame_per_group_with_the_right_shards() {
    let bytes = pack(options(10), &tree());
    let mut a = open(&bytes);
    a.verify().unwrap();
    let frames = frames_of(&bytes);
    let locs = a.recovery_frames().to_vec();
    assert!(frames.len() >= 8, "{}", frames.len());
    // Groups are consecutive, from the end of the header to the first frame.
    let mut next = 32u64;
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(f.cover_offset, next);
        next += f.cover_len;
        assert_eq!(f.shard_len, SHARD);
        assert_eq!(f.group_shards, GROUP);
        assert_eq!(f.recovery_shards, 2); // ceil(16 * 10 / 100)
        if i + 1 < frames.len() {
            assert_eq!(f.data_shards, GROUP);
            assert_eq!(f.cover_len, u64::from(GROUP * SHARD));
        } else {
            assert!(f.data_shards <= GROUP);
        }
        if i > 0 {
            assert_eq!(locs[i].offset, locs[i - 1].offset + locs[i - 1].len);
        }
    }
    assert_eq!(next, locs[0].offset);
    // Hashes are of the zero-padded shards; recovery is the library's coding
    // of the group padded with implicit zero shards.
    for f in &frames {
        let mut shards: Vec<Vec<u8>> = Vec::new();
        for i in 0..f.data_shards {
            let (off, len) = f.shard_range(i);
            let mut s = bytes[off as usize..off as usize + len].to_vec();
            s.resize(SHARD as usize, 0);
            assert_eq!(f.shard_hashes[i as usize], *blake3::hash(&s).as_bytes());
            shards.push(s);
        }
        shards.resize(GROUP as usize, vec![0; SHARD as usize]);
        let want = reed_solomon_simd::encode(GROUP as usize, 2, &shards).unwrap();
        let got: Vec<&[u8]> = f.recovery.chunks(SHARD as usize).collect();
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(*g, w.as_slice());
        }
    }
}

#[test]
fn bad_options_are_refused() {
    let refused = |f: &dyn Fn(&mut WriterOptions)| {
        let mut o = options(5);
        f(&mut o);
        assert!(matches!(
            Writer::new(Vec::new(), o),
            Err(FormatError::BadOptions { .. })
        ));
    };
    refused(&|o| o.recovery.percent = 21);
    refused(&|o| o.recovery.shard_len = 0);
    refused(&|o| o.recovery.shard_len = 100);
    refused(&|o| o.recovery.shard_len = 4097);
    refused(&|o| o.recovery.shard_len = (16 << 20) + 64);
    refused(&|o| o.recovery.group_shards = 0);
    refused(&|o| o.recovery.group_shards = 32769);
    // 32768 shards of 64 KiB is 2 GiB: above the group cap.
    refused(&|o| {
        o.recovery.group_shards = 32768;
        o.recovery.shard_len = 65536;
    });
    // With percent 0 the other settings are not looked at.
    let mut o = options(0);
    o.recovery.shard_len = 100;
    assert!(Writer::new(Vec::new(), o).is_ok());
}

/// A sink the test can look at while the writer owns it.
#[derive(Clone)]
struct Shared(Rc<RefCell<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_writer_keeps_no_covered_bytes_beyond_one_shard_and_one_group_encoder() {
    // Groups of 4 shards of 64 KiB, one recovery shard each (25 percent).
    let mut o = options(20);
    o.recovery = RecoveryOptions {
        percent: 20,
        shard_len: 1 << 16,
        group_shards: 4,
    };
    o.chunk_size = 1 << 16;
    o.block_size = 1 << 20;
    let sink = Shared(Rc::new(RefCell::new(Vec::new())));
    let mut w = Writer::new(sink.clone(), o).unwrap();
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
        // Finished groups hold only their recovery shards: a quarter of the
        // covered bytes written so far, never the covered bytes themselves.
        let written = sink.0.borrow().len() as u64;
        assert!(
            w.recovery_held() <= written / 4,
            "held {}",
            w.recovery_held()
        );
        assert!(i < 2 || w.recovery_held() > 0);
    }
    assert!(peak <= 1 << 16, "peak {peak}");
    // The encoder's work buffer is one group's, whatever the archive's size.
    assert_eq!(lpk_format::encoder_work_bytes(4, 1, 1 << 16), 4 << 16);
    w.finish().unwrap();
    let bytes = sink.0.borrow().clone();
    let mut a = open(&bytes);
    a.verify().unwrap();
    assert!(a.recovery_frames().len() > 70);
    assert_eq!(a.check_recovery().unwrap().shards_damaged, 0);
}

// ---- repair and detection ----

fn repair_with(
    bytes: &[u8],
    resources: &Resources,
) -> (Result<lpk_format::RepairReport, FormatError>, Vec<u8>) {
    let mut out = Cursor::new(Vec::new());
    let r = lpk_format::repair(Cursor::new(bytes.to_vec()), &mut out, resources);
    (r, out.into_inner())
}

fn repair_bytes(bytes: &[u8]) -> (Result<lpk_format::RepairReport, FormatError>, Vec<u8>) {
    repair_with(bytes, &Resources::default())
}

/// Damage every byte of shard `s` of `f`.
fn smash_shard(bytes: &mut [u8], f: &RecoveryFrame, s: u32) {
    let (off, len) = f.shard_range(s);
    for b in &mut bytes[off as usize..off as usize + len] {
        *b ^= 0xA5;
    }
}

/// Damage a few scattered bytes of shard `s` of `f`.
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
    let bytes = pack(options(10), &tree());
    let n = frames_of(&bytes).len() as u64;
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
        (n, 0, 0, 0)
    );
    assert_eq!(open(&bytes).check_recovery().unwrap(), r);
}

#[test]
fn archives_without_recovery_are_copied_and_report_nothing() {
    let bytes = pack(options(0), &tree());
    let (r, copy) = repair_bytes(&bytes);
    assert_eq!(r.unwrap(), lpk_format::RepairReport::default());
    assert_eq!(copy, bytes);
}

#[test]
fn damage_up_to_capacity_in_every_group_is_repaired_exactly() {
    let good = pack(options(10), &tree());
    let frames = frames_of(&good);
    let mut rng = Rng(0xDEAD_BEEF);
    for round in 0..16 {
        let mut bad = good.clone();
        let mut total = 0u64;
        for (gi, f) in frames.iter().enumerate() {
            // Some groups untouched, the others up to capacity.
            if (gi + round) % 3 == 0 {
                continue;
            }
            let k = if round == 0 {
                f.recovery_shards as usize
            } else {
                1 + (rng.next() as usize % f.recovery_shards as usize)
            };
            let mut shards = pick(&mut rng, f.data_shards, k.min(f.data_shards as usize));
            if gi + 1 == frames.len() {
                // The last shard of the last group is the short one.
                shards[0] = f.data_shards - 1;
                shards.sort_unstable();
                shards.dedup();
            }
            for &s in &shards {
                if round % 2 == 0 {
                    smash_shard(&mut bad, f, s);
                } else {
                    nick_shard(&mut bad, f, s, &mut rng);
                }
            }
            total += shards.len() as u64;
        }
        let c = open(&bad).check_recovery().unwrap();
        assert_eq!((c.shards_damaged, c.shards_repaired), (total, 0));
        let (r, copy) = repair_bytes(&bad);
        let r = r.unwrap();
        assert_eq!((r.shards_damaged, r.shards_repaired), (total, total));
        assert!(copy == good, "round {round}: copy differs");
        let mut fixed = open(&copy);
        fixed.verify().unwrap();
        assert_eq!(fixed.check_recovery().unwrap().shards_damaged, 0);
    }
}

#[test]
fn one_group_beyond_capacity_leaves_the_others_repaired() {
    let good = pack(options(10), &tree());
    let frames = frames_of(&good);
    assert!(frames.len() > 6);
    let cap = u64::from(frames[2].recovery_shards);
    let mut bad = good.clone();
    // Group 2: one more than it can rebuild. Groups 0 and 5: within capacity.
    for s in 0..cap as u32 + 1 {
        smash_shard(&mut bad, &frames[2], s);
    }
    smash_shard(&mut bad, &frames[0], 3);
    smash_shard(&mut bad, &frames[5], 1);
    let (r, copy) = repair_bytes(&bad);
    match r {
        Err(FormatError::Unrepairable {
            frame,
            damaged,
            capacity,
        }) => assert_eq!((frame, damaged, capacity), (2, cap + 1, cap)),
        other => panic!("expected Unrepairable, got {other:?}"),
    }
    // Groups 0 and 5 are as in the good archive, group 2 as damaged.
    let range =
        |f: &RecoveryFrame| f.cover_offset as usize..(f.cover_offset + f.cover_len) as usize;
    assert_eq!(copy[range(&frames[0])], good[range(&frames[0])]);
    assert_eq!(copy[range(&frames[5])], good[range(&frames[5])]);
    assert_eq!(copy[range(&frames[2])], bad[range(&frames[2])]);
}

#[test]
fn damage_in_a_recovery_frame_leaves_its_data_alone() {
    let good = pack(options(10), &tree());
    let frames = frames_of(&good);
    let loc = open(&good).recovery_frames()[1];
    let mut bad = good.clone();
    bad[(loc.offset + loc.len - 100) as usize] ^= 0xFF;
    // A data shard of that frame's group, and one of another group.
    smash_shard(&mut bad, &frames[1], 3);
    smash_shard(&mut bad, &frames[4], 3);
    let (r, copy) = repair_bytes(&bad);
    let r = r.unwrap();
    assert_eq!(r.frames_unusable, 1);
    assert_eq!((r.shards_damaged, r.shards_repaired), (1, 1));
    // Frame 1's coverage is left as it is (no false repair); group 4 is fixed.
    let (off, len) = frames[1].shard_range(3);
    assert_eq!(
        copy[off as usize..off as usize + len],
        bad[off as usize..off as usize + len]
    );
    let (off, len) = frames[4].shard_range(3);
    assert_eq!(
        copy[off as usize..off as usize + len],
        good[off as usize..off as usize + len]
    );
    assert_eq!(open(&bad).check_recovery().unwrap().frames_unusable, 1);
}

#[test]
fn damage_in_the_index_is_the_open_error() {
    let good = pack(options(10), &tree());
    let at = open(&good).trailer().index_offset as usize;
    let mut bad = good.clone();
    bad[at + 10] ^= 0xFF;
    let want = Archive::open(Cursor::new(bad.clone()), &Resources::default()).unwrap_err();
    let (r, copy) = repair_bytes(&bad);
    assert_eq!(r.unwrap_err().to_string(), want.to_string());
    assert!(copy.is_empty());
}

#[test]
fn a_group_the_decoder_cannot_fit_in_memory_is_refused() {
    let good = pack(options(10), &tree());
    let frames = frames_of(&good);
    let mut bad = good.clone();
    smash_shard(&mut bad, &frames[0], 0);
    let needed = lpk_format::decoder_work_bytes(GROUP, 2, SHARD);
    let tight = Resources {
        memory: needed - 1,
        ..Resources::default()
    };
    match repair_with(&bad, &tight).0 {
        Err(FormatError::Refused(r)) => {
            assert_eq!((r.field, r.needed), ("recovery group", needed));
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    let enough = Resources {
        memory: needed,
        ..Resources::default()
    };
    assert!(repair_with(&bad, &enough).0.is_ok());
}

mod hand_built {
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

    /// An archive with two recovery frames over the two halves of the body.
    /// The first group is coded with 2 implicit zero shards beyond its real
    /// ones. With `overlap`, the second frame also covers 40 bytes of the first
    /// recovery frame.
    pub fn build(overlap: bool) -> (Vec<u8>, Vec<RecoveryFrame>) {
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
        for (n, (from, to)) in [(32u64, mid), (mid, body_end)].into_iter().enumerate() {
            let to = if overlap && n == 1 { to + 40 } else { to };
            let cover = &bytes[from as usize..to as usize];
            let data = cover.len().div_ceil(64);
            let group = if n == 0 { data + 2 } else { data };
            let mut shards: Vec<Vec<u8>> = cover.chunks(64).map(<[u8]>::to_vec).collect();
            for s in &mut shards {
                s.resize(64, 0);
            }
            let hashes = shards.iter().map(|s| *blake3::hash(s).as_bytes()).collect();
            shards.resize(group, vec![0; 64]);
            let coded = reed_solomon_simd::encode(group, 3, &shards).unwrap();
            let frame = RecoveryFrame {
                cover_offset: from,
                cover_len: to - from,
                shard_len: 64,
                data_shards: data as u32,
                group_shards: group as u32,
                recovery_shards: 3,
                shard_hashes: hashes,
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
fn a_short_group_with_implicit_zero_shards_is_repaired() {
    let (good, frames) = hand_built::build(false);
    assert!(frames[0].group_shards > frames[0].data_shards);
    let mut a = open(&good);
    assert_eq!(a.recovery_frames().len(), 2);
    assert_eq!(a.check_recovery().unwrap().shards_damaged, 0);
    let mut bad = good.clone();
    smash_shard(&mut bad, &frames[0], 0);
    smash_shard(&mut bad, &frames[0], frames[0].data_shards - 1);
    smash_shard(&mut bad, &frames[1], 1);
    let (r, copy) = repair_bytes(&bad);
    let r = r.unwrap();
    assert_eq!((r.frames, r.shards_damaged, r.shards_repaired), (2, 3, 3));
    assert_eq!(copy, good);
}

#[test]
fn a_frame_covering_a_recovery_frame_is_unusable_not_fatal() {
    let (good, frames) = hand_built::build(true);
    let mut bad = good.clone();
    smash_shard(&mut bad, &frames[0], 0);
    let (r, copy) = repair_bytes(&bad);
    let r = r.unwrap();
    // Frame 1 breaks the coverage rule: counted unusable; frame 0 still repairs.
    assert_eq!((r.frames, r.frames_unusable), (2, 1));
    assert_eq!((r.shards_damaged, r.shards_repaired), (1, 1));
    assert_eq!(copy, good);
}
