//! The envelope as the reference writer fills it (E2-20): `decode_memory` is `max_block_plain`
//! plus the largest decoder working memory a block declares (`Encoded::memory`, or the `memory`
//! of `add_part_encoded`), and `threads_hint` is the smaller of the archive's blocks and
//! `WriterOptions::threads_hint` (0 writes 0) — the same whatever `encode_threads` is. The
//! reference tool's `--memory` refuses an archive whose envelope exceeds it.
#![allow(clippy::unwrap_used)]

mod common;

use lpk_format::{
    Archive, BlockEncoder, Encoded, EntryFlags, FormatError, Resources, StoreEncoder, Writer,
    WriterOptions,
};
use std::io::Cursor;

const CHUNK: u64 = 4096;
const BLOCK: u64 = 16384;

/// Stores every block and declares `memory` for block `n` as `per_block[n % len]`.
struct Declaring {
    per_block: Vec<u64>,
    next: usize,
}

impl BlockEncoder for Declaring {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let mut e = StoreEncoder.encode(plain)?;
        e.memory = self.per_block[self.next % self.per_block.len()];
        self.next += 1;
        Ok(e)
    }
}

fn write(threads: usize, hint: u32, per_block: Vec<u64>, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let opts = WriterOptions {
        chunk_size: CHUNK,
        block_size: BLOCK,
        archive_id: [9; 16],
        encoder: Box::new(Declaring { per_block, next: 0 }),
        encode_threads: threads,
        threads_hint: hint,
        ..WriterOptions::default()
    };
    let mut w = Writer::new(&mut out, opts).unwrap();
    w.add_file("f", EntryFlags::EMPTY, 0, &mut &data[..])
        .unwrap();
    w.finish().unwrap();
    out
}

fn envelope(bytes: &[u8]) -> lpk_format::Envelope {
    let a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    a.index().envelope
}

#[test]
fn decode_memory_is_the_largest_block_plus_the_largest_declared_memory() {
    // Five blocks of 16 KiB (the last shorter), declaring 100, 7000, 30 bytes in turn.
    let data = common::pattern(5, 4 * BLOCK as usize + 1000);
    let bytes = write(1, 0, vec![100, 7000, 30], &data);
    let e = envelope(&bytes);
    assert_eq!(e.max_block_plain, BLOCK);
    assert_eq!(e.decode_memory, BLOCK + 7000);
    assert_eq!(e.threads_hint, 0, "no hint asked for");
}

#[test]
fn a_block_declaring_nothing_leaves_decode_memory_at_the_block() {
    let data = common::pattern(6, 2 * BLOCK as usize);
    let e = envelope(&write(1, 0, vec![0], &data));
    assert_eq!(e.decode_memory, e.max_block_plain);
}

#[test]
fn threads_hint_is_the_blocks_capped_at_the_option() {
    let data = common::pattern(7, 4 * BLOCK as usize + 1000); // five blocks
    assert_eq!(envelope(&write(1, 3, vec![0], &data)).threads_hint, 3);
    assert_eq!(envelope(&write(1, 64, vec![0], &data)).threads_hint, 5);
    let one = common::pattern(8, 1000); // one block
    assert_eq!(envelope(&write(1, 8, vec![0], &one)).threads_hint, 1);
}

#[test]
fn the_envelope_does_not_depend_on_the_blocks_in_flight() {
    let data = common::pattern(9, 6 * BLOCK as usize + 77);
    let one = write(1, 4, vec![10, 20_000], &data);
    for threads in [2, 3, 8] {
        assert_eq!(write(threads, 4, vec![10, 20_000], &data), one, "{threads}");
    }
    let e = envelope(&one);
    assert_eq!(e.decode_memory, BLOCK + 20_000);
    assert_eq!(e.threads_hint, 4);
}

#[test]
fn add_part_encoded_memory_and_encoded_memory_both_count() {
    let mut out = Vec::new();
    let opts = WriterOptions {
        chunk_size: CHUNK,
        block_size: BLOCK,
        archive_id: [9; 16],
        ..WriterOptions::default()
    };
    let mut w = Writer::new(&mut out, opts).unwrap();
    let a = common::pattern(10, 5000);
    w.begin_entry("a", EntryFlags::EMPTY, 0).unwrap();
    let mut e = StoreEncoder.encode(&a).unwrap();
    e.memory = 900;
    w.add_part_encoded(0, &a, e, 400).unwrap();
    w.end_entry().unwrap();
    let b = common::pattern(11, 6000);
    w.begin_entry("b", EntryFlags::EMPTY, 0).unwrap();
    w.add_part_encoded(0, &b, StoreEncoder.encode(&b).unwrap(), 1200)
        .unwrap();
    w.end_entry().unwrap();
    w.finish().unwrap();
    let env = envelope(&out);
    assert_eq!(env.max_block_plain, 6000);
    assert_eq!(env.decode_memory, 6000 + 1200);
}

#[test]
fn the_reference_tool_refuses_above_its_memory_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.lpk");
    let data = common::pattern(12, 2 * BLOCK as usize);
    std::fs::write(&path, write(1, 0, vec![1 << 20], &data)).unwrap();
    let need = BLOCK + (1 << 20);
    let run = |memory: u64| {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = lpk_format::cli::run(
            [
                "lpk-decode".to_string(),
                format!("--memory={memory}"),
                "verify".to_string(),
                path.display().to_string(),
            ],
            &mut out,
            &mut err,
        );
        (code, String::from_utf8(err).unwrap())
    };
    let (code, err) = run(need - 1);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("decode_memory"), "{err}");
    assert!(err.contains(&need.to_string()), "{err}");
    assert!(err.contains(&(need - 1).to_string()), "{err}");
    let (code, err) = run(need);
    assert_eq!(code, 0, "{err}");
}
