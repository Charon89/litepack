//! The recovery vector: store graph, 20 percent recovery, 1 KiB shards in groups of 12,
//! so the body is covered by several recovery frames.
#![allow(dead_code, clippy::unwrap_used)]

use super::{pattern, vectors_dir};
use lpk_format::{Archive, RecoveryOptions, Resources, WriterOptions};
use std::io::Cursor;

pub const RECOVERY_VECTOR: &str = "recovery-groups.lpk";
pub const RECOVERY_DAMAGED: &str = "malformed-recovery-damaged.lpk";

pub fn recovery_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("r/a.txt", pattern(91, 6_000)),
        ("r/b.txt", pattern(92, 9_000)),
        ("r/c.txt", pattern(93, 15_000)),
    ]
}

pub fn recovery_options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 4096,
        archive_id: [0x52; 16],
        recovery: RecoveryOptions {
            percent: 20,
            shard_len: 1024,
            group_shards: 12,
        },
        ..WriterOptions::default()
    }
}

pub fn build_recovery_vector() -> Vec<u8> {
    super::sealed::write_archive_seeded(recovery_options(), &recovery_files(), 0x2EC0)
}

/// The recovery vector with one bit flipped inside its first block's frame: one data
/// shard is damaged.
pub fn damaged_from(good: &[u8]) -> Vec<u8> {
    let a = Archive::open(Cursor::new(good.to_vec()), &Resources::default()).unwrap();
    let b = a.index().blocks[0];
    let mut bytes = good.to_vec();
    bytes[(b.frame_offset + b.frame_len / 2) as usize] ^= 0x01;
    bytes
}

pub fn read_vector(name: &str) -> Vec<u8> {
    std::fs::read(vectors_dir().join(name)).unwrap()
}
