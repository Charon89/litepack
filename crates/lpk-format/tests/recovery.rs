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
