//! The memory budget through the pipeline (E2-20): a budget that allows fewer blocks in flight
//! writes the same bytes, a budget below one block in flight is refused with the figure, and the
//! envelope's `decode_memory` and `threads_hint` follow the formulas for Fast, Balanced and a
//! peeled archive.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_core::memory::{
    lzma_decode_memory, window_for, zstd_decode_memory, Budget, BudgetSource, EncoderMemory,
    ModelInputs, FAST_CEILING,
};
use lpk_core::{BalancedOptions, CoreError, FastOptions, Pipeline, RunSummary, StoreOptions};
use lpk_format::{Archive, Envelope, Resources};

const MIB: u64 = 1 << 20;

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(WORDS[(x % 8) as usize].as_bytes());
        out.push(if x & 0x700 == 0 { b'\n' } else { b' ' });
    }
    out.truncate(len);
    out
}

fn tree(root: &Path) {
    for i in 0..5u64 {
        std::fs::write(root.join(format!("t{i}.txt")), prose(i + 1, 900_000)).unwrap();
    }
}

fn fast(threads: usize, memory: Option<u64>) -> Pipeline {
    let mut p = Pipeline::fast(FastOptions {
        block_size: MIB,
        threads: Some(threads),
        ..FastOptions::default()
    });
    p.seal.memory = memory;
    p
}

fn balanced(threads: usize, memory: Option<u64>) -> Pipeline {
    let mut p = Pipeline::balanced(BalancedOptions {
        block_size: MIB,
        dict_size: 1 << 20,
        sample_len: 1 << 16,
        min_block_before_boundary: Some(0),
        threads: Some(threads),
        ..BalancedOptions::default()
    });
    p.seal.memory = memory;
    p
}

fn run(p: Pipeline, root: &Path) -> Result<(Vec<u8>, RunSummary), CoreError> {
    let mut out = Vec::new();
    let s = p.run(root, &mut out)?;
    Ok((out, s))
}

fn envelope(bytes: &[u8]) -> Envelope {
    let a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    a.index().envelope
}

#[test]
fn a_budget_for_one_block_writes_the_same_bytes_sequentially() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let (wide, s) = run(fast(8, Some(u64::MAX / 2)), dir.path()).unwrap();
    assert_eq!(s.blocks_in_flight, 8);
    assert_eq!(s.memory.budget.source, BudgetSource::Given);
    // Exactly what one block in flight needs: accepted, sequential.
    let one = s.memory.minimum();
    let (narrow, n) = run(fast(8, Some(one)), dir.path()).unwrap();
    assert_eq!((n.threads, n.blocks_in_flight, n.io_threads), (8, 1, 0));
    assert_eq!(n.memory.fixed(), n.memory.fixed_sequential);
    assert_eq!(wide, narrow);
    // Room for exactly three pipelined blocks.
    let three = s.memory.fixed_pipelined + 3 * s.memory.per_block;
    let (mid, m) = run(fast(8, Some(three)), dir.path()).unwrap();
    assert_eq!(m.blocks_in_flight, 3);
    assert_eq!(mid, wide);
    // The Balanced tier likewise.
    let (b8, sb) = run(balanced(8, None), dir.path()).unwrap();
    let (b1, sb1) = run(balanced(8, Some(sb.memory.minimum())), dir.path()).unwrap();
    assert_eq!(sb1.blocks_in_flight, 1);
    assert_eq!(b8, b1);
}

#[test]
fn a_budget_below_one_block_is_refused_with_the_figure() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let (_, s) = run(fast(4, None), dir.path()).unwrap();
    let need = s.memory.minimum();
    let e = run(fast(4, Some(need - 1)), dir.path()).unwrap_err();
    assert!(matches!(e, CoreError::InvalidOption(_)), "{e}");
    assert!(e.to_string().contains(&need.to_string()), "{e}");
    let mut store = Pipeline::store(StoreOptions::default());
    store.seal.memory = Some(1000);
    let e = run(store, dir.path()).unwrap_err();
    assert!(
        e.to_string()
            .contains("below what one block in flight needs"),
        "{e}"
    );
}

#[test]
fn the_default_budget_is_the_tiers() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let (_, s) = run(fast(2, None), dir.path()).unwrap();
    let expect = Budget::default_for(FAST_CEILING, lpk_procstat_sys_free_physical());
    assert_eq!(s.memory.budget, expect);
}

/// The physical memory as the budget saw it (through the budget's own resolution).
fn lpk_procstat_sys_free_physical() -> Option<u64> {
    match Budget::resolve(None, FAST_CEILING).source {
        BudgetSource::Default { physical, .. } => physical,
        BudgetSource::Given => None,
    }
}

#[test]
fn fast_envelope_follows_the_formula() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let (bytes, s) = run(fast(1, None), dir.path()).unwrap();
    let e = envelope(&bytes);
    // Blocks just under 1 MiB (closed before the chunk that would exceed it), each zstd with a
    // 2^20 window: the largest block, its window (the block itself) and the allowance.
    let p = e.max_block_plain;
    assert!(p > MIB / 2 && p <= MIB, "{p}");
    assert_eq!(e.decode_memory, p + zstd_decode_memory(20, p));
    let model = ModelInputs {
        entries: 0,
        path_bytes: 0,
        input_bytes: 0,
        ..fast_model()
    };
    let hint = model.threads_hint(FAST_CEILING);
    assert!(hint > 1);
    assert_eq!(
        u64::from(e.threads_hint),
        s.writer.blocks.min(u64::from(hint))
    );
}

fn fast_model() -> ModelInputs {
    let o = FastOptions::default();
    ModelInputs {
        block_size: MIB,
        chunk_size: 512 << 10,
        avg_chunk: 64 << 10,
        encoder: EncoderMemory::Zstd {
            level: o.level,
            window_log: window_for(o.window_log, MIB),
            dictionaries: 0,
            dictionary_bytes: 0,
        },
        peel_max_file: Some(o.jpeg_max_file),
        entries: 0,
        path_bytes: 0,
        input_bytes: 0,
        prefetch: Default::default(),
    }
}

#[test]
fn balanced_envelope_follows_the_formula() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let (bytes, s) = run(balanced(1, None), dir.path()).unwrap();
    let b = s.balanced.unwrap();
    let e = envelope(&bytes);
    let p = e.max_block_plain;
    assert!(p > MIB / 2 && p <= MIB, "{p}");
    if b.zstd_blocks == 0 {
        assert_eq!(e.decode_memory, p + lzma_decode_memory(1 << 20, p));
    } else {
        // Both decoders keep at most the block as their window: between the allowance and the
        // block plus the allowance beyond the block.
        assert!(
            e.decode_memory > p && e.decode_memory <= p + zstd_decode_memory(20, p),
            "{e:?}"
        );
    }
    assert!(e.threads_hint >= 1 && u64::from(e.threads_hint) <= s.writer.blocks);
}

#[test]
fn a_peeled_archive_declares_the_images_allowance() {
    let dir = tempfile::tempdir().unwrap();
    let jpeg = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("baseline.jpg"),
    )
    .unwrap();
    std::fs::write(dir.path().join("p.jpg"), &jpeg).unwrap();
    std::fs::write(dir.path().join("t.txt"), prose(3, 200_000)).unwrap();
    let (bytes, s) = run(fast(1, None), dir.path()).unwrap();
    assert!(s.peel.peeled.files >= 1, "{:?}", s.peel);
    let e = envelope(&bytes);
    assert!(
        e.decode_memory >= e.max_block_plain + lpk_core::peel::MODEL_ALLOWANCE,
        "{e:?}"
    );
    assert_eq!(
        u64::from(e.threads_hint),
        s.writer.blocks.min(u64::from(e.threads_hint))
    );
    assert!(e.threads_hint >= 1);
}
