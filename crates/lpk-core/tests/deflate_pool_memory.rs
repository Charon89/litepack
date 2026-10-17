//! The Deflate peel pool against the memory budget (design
//! `docs/notes/e2-10-peel-pool-design.md`, task 2): at twenty-four threads and the smallest
//! budget the run accepts, where the pool's bytes P bind (workers wait for admission), the peak
//! heap measured by a counting global allocator stays within the budget and the pool's own
//! reservations within P; the archive is the one-thread archive. Its own test binary, so no
//! other test allocates meanwhile. Only the Rust heap is counted (liblzma and libzstd allocate
//! through the C allocator).
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::Path;

use lpk_core::peel::deflate::DeflatePeel;
use lpk_core::{BalancedOptions, Pipeline, RunSummary};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

fn text(n: usize, seed: u32) -> Vec<u8> {
    let words = [
        "alpha ", "bravo ", "charlie ", "delta ", "echo ", "foxtrot ",
    ];
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    let mut out = Vec::with_capacity(n + 16);
    while out.len() < n {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(words[(x >> 24) as usize % words.len()].as_bytes());
    }
    out.truncate(n);
    out
}

/// A 40 KiB passage repeated with one change per copy (kept streams).
fn repetitive(seed: u32, len: usize) -> Vec<u8> {
    let base = text(40_000, seed);
    let mut out = Vec::with_capacity(len + base.len());
    let mut copy = 0usize;
    while out.len() < len {
        let at = out.len();
        out.extend_from_slice(&base);
        out[at + (copy * 7919) % base.len()] = b'#';
        copy += 1;
    }
    out.truncate(len);
    out
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn tree(root: &Path) {
    for i in 0..48u32 {
        std::fs::write(
            root.join(format!("g{i:02}.gz")),
            gzip(&repetitive(i + 1, 200_000 + 20_000 * i as usize)),
        )
        .unwrap();
    }
    // Incompressible members: encoded, refused by the stream gate, never held (review B1).
    for i in 0..16u32 {
        let mut x = i.wrapping_mul(2_654_435_761) | 1;
        let noise: Vec<u8> = (0..400_000)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect();
        std::fs::write(root.join(format!("n{i:02}.gz")), gzip(&noise)).unwrap();
    }
    for e in std::fs::read_dir(root).unwrap() {
        let f = std::fs::File::options()
            .write(true)
            .open(e.unwrap().path())
            .unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
    }
}

fn run(root: &Path, threads: usize, memory: Option<u64>) -> (Vec<u8>, RunSummary) {
    let mut p = Pipeline::balanced(BalancedOptions {
        block_size: 4 << 20,
        dict_size: 1 << 20,
        sample_len: 1 << 16,
        threads: Some(threads),
        ..BalancedOptions::default()
    });
    p.deflate = Some(DeflatePeel::default());
    p.seal.memory = memory;
    let mut out = Vec::new();
    let s = p.run(root, &mut out).unwrap();
    (out, s)
}

#[test]
fn twenty_four_threads_with_the_pool_binding_stay_within_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let (one, s1) = run(dir.path(), 1, None);
    assert!(
        s1.peel.deflate.outcomes.kept >= 40,
        "{:?}",
        s1.peel.deflate.outcomes
    );
    let budget = s1.memory.minimum();
    ALLOC.reset_peak();
    let base = ALLOC.current();
    // Workers delayed pseudo-randomly: results complete out of order.
    lpk_core::peel::deflate::pool::set_delay_seed(0xA110C);
    let (many, s) = run(dir.path(), 24, Some(budget));
    lpk_core::peel::deflate::pool::set_delay_seed(0);
    assert!(
        s.peel.deflate.outcomes.no_gain >= 16,
        "{:?}",
        s.peel.deflate.outcomes
    );
    let peak = (ALLOC.peak() - base) as u64;
    let p = &s.peel_pool;
    println!(
        "budget {budget}, heap peak {peak}, P {}, R {}, pool peak {}, worker waits {}, workers {}",
        p.pool_bytes, p.reserve, p.peak_bytes, p.worker_waits, p.workers
    );
    assert!(many == one, "the archives differ");
    assert_eq!(s.peel, s1.peel);
    assert_eq!((s.blocks_in_flight, p.workers), (1, 23));
    // P binds: the workers waited for the bytes, and never held more than P.
    assert!(p.worker_waits > 0, "{p:?}");
    assert!(p.peak_bytes <= p.pool_bytes, "{p:?}");
    assert!(peak <= budget, "heap peak {peak} above the budget {budget}");
}
