//! The extraction side's memory budget (E2-20, ruling R19 (3)): the peak heap use of
//! `extract_file`, measured by a counting global allocator, against the budget and the engine's
//! own count of what it holds ([`lpk_core::ExtractMemory::total`]). Its own test binary, so no
//! other test allocates meanwhile; the tests in it take a lock, so `cargo test`'s threads do not
//! overlap either.
//!
//! Always run: a store, a Fast, a Balanced and a peeled archive extracted at 24 threads under a
//! small `--memory` that allows a few blocks of many. Ignored (heavy, run with `cargo nextest
//! run --release -p lpk-core --test extract_memory --run-ignored all`): a store, a Fast and a
//! Balanced archive extracted at 24 threads under the default budget (the ceiling of the tier
//! the envelope shows), with several blocks more than that budget lets decode at once. In every
//! case the extracted bytes are the source's, at one thread and at 24, under every budget.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lpk_core::memory::{Budget, BudgetSource, EnvelopeTier, BALANCED_CEILING, FAST_CEILING};
use lpk_core::{
    extract_file, BalancedOptions, DefaultPolicy, ExtractOptions, ExtractSummary, FastOptions,
    Pipeline, StoreOptions,
};
use lpk_format::{Archive, Envelope, Resources, WriterOptions};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// One measurement at a time.
static SERIAL: Mutex<()> = Mutex::new(());

const MIB: u64 = 1 << 20;
const THREADS: usize = 24;

/// One MiB of word salad (compressible, as text is).
fn unit() -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(MIB as usize + 8);
    while out.len() < MIB as usize {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(WORDS[(x % 8) as usize].as_bytes());
        out.push(if x & 0x700 == 0 { b'\n' } else { b' ' });
    }
    out.truncate(MIB as usize);
    out
}

/// `files` files of `mib` MiB each under `root`: the unit repeated, each MiB stamped with its
/// file and position so no two are alike.
fn write_tree(root: &Path, files: u64, mib: u64) {
    let u = unit();
    for f in 0..files {
        let mut data = Vec::with_capacity((mib * MIB) as usize);
        for m in 0..mib {
            let at = data.len();
            data.extend_from_slice(&u);
            data[at..at + 16].copy_from_slice(&[f.to_le_bytes(), m.to_le_bytes()].concat());
        }
        std::fs::write(root.join(format!("f{f:03}.txt")), &data).unwrap();
    }
}

enum Tier {
    Store { block: u64 },
    Fast { block: u64 },
    Balanced { block: u64, dict: u32 },
}

fn pack(tier: &Tier, src: &Path, arch: &Path) {
    let p = match *tier {
        Tier::Store { block } => Pipeline::store(StoreOptions {
            writer: WriterOptions {
                block_size: block,
                dedup: false,
                ..WriterOptions::default()
            },
            threads: Some(THREADS),
            ..StoreOptions::default()
        }),
        Tier::Fast { block } => {
            let mut p = Pipeline::fast(FastOptions {
                block_size: block,
                level: 1,
                threads: Some(THREADS),
                ..FastOptions::default()
            });
            p.fold = None;
            p
        }
        Tier::Balanced { block, dict } => {
            let mut p = Pipeline::balanced(BalancedOptions {
                block_size: block,
                dict_size: dict,
                sample_len: 1 << 16,
                min_block_before_boundary: Some(0),
                threads: Some(THREADS),
                ..BalancedOptions::default()
            });
            p.fold = None;
            p
        }
    };
    p.run_file(src, arch).unwrap();
}

fn envelope(arch: &Path) -> Envelope {
    let f = std::fs::File::open(arch).unwrap();
    Archive::open(f, &Resources::default())
        .unwrap()
        .index()
        .envelope
}

/// Relative path -> BLAKE3 of every file under `root`.
fn hashes(root: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        let bytes = std::fs::read(&p).unwrap();
        out.insert(
            p.strip_prefix(root).unwrap().to_path_buf(),
            *blake3::hash(&bytes).as_bytes(),
        );
    }
    out
}

/// Extract `arch` under a fresh `out` and return the summary and the peak heap use above what
/// was allocated before.
fn measure(arch: &Path, out: &Path, threads: usize, memory: Option<u64>) -> (ExtractSummary, u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let opts = ExtractOptions {
        threads: Some(threads),
        memory,
        ..ExtractOptions::default()
    };
    let before = ALLOC.current();
    ALLOC.reset_peak();
    let s = extract_file(arch, out, &[], None, &DefaultPolicy, &opts).unwrap();
    let peak = (ALLOC.peak() - before) as u64;
    (s, peak)
}

/// The bound binds (fewer workers than the threads and the blocks would allow), the peak is
/// within the engine's count and the count within the budget.
fn check_bound(name: &str, s: &ExtractSummary, peak: u64) {
    let m = &s.memory;
    let room = (THREADS - s.writers).min(s.blocks_needed as usize);
    println!(
        "{name}: budget {} ({:?}, tier {}), per_block {} (encoded {}, plain {}, decoder {}, nested {}), fixed {}, workers {} of {room}, blocks {}, peak in flight {}, measured peak {peak}, counted {}",
        m.budget.bytes,
        m.budget.source,
        m.tier.name(),
        m.per_block,
        m.encoded,
        m.plain,
        m.decoder,
        m.nested,
        m.fixed,
        s.workers,
        s.blocks_needed,
        s.peak_in_flight,
        m.total(s.workers)
    );
    assert!(
        s.workers + 2 <= room,
        "{name}: the bound must bind with blocks to spare"
    );
    assert!(
        peak <= m.total(s.workers),
        "{name}: measured {peak} above counted {}",
        m.total(s.workers)
    );
    assert!(
        m.total(s.workers) <= m.budget.bytes,
        "{name}: counted above budget"
    );
}

/// A small `--memory` (room for three blocks in flight) binds at 24 threads; the bytes are the
/// source's at 24 threads under it, at 24 under no bound, and at one thread.
fn small_budget_case(name: &str, tier: Tier, files: u64, mib: u64, expect: EnvelopeTier) {
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), files, mib);
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("a.lpk");
    pack(&tier, src.path(), &arch);
    assert_eq!(EnvelopeTier::of(&envelope(&arch)), expect, "{name}");
    let want = hashes(src.path());
    // Out directories of one length, so the plan's terms match across the runs.
    let (wide, _) = measure(&arch, &work.path().join("o0"), THREADS, Some(u64::MAX / 2));
    let budget = wide.memory.total(3);
    let (s, peak) = measure(&arch, &work.path().join("o1"), THREADS, Some(budget));
    assert_eq!(s.memory.budget.source, BudgetSource::Given, "{name}");
    assert_eq!(s.workers, 3, "{name}");
    check_bound(name, &s, peak);
    let (_, _) = measure(&arch, &work.path().join("o2"), 1, Some(budget));
    for o in ["o0", "o1", "o2"] {
        assert_eq!(hashes(&work.path().join(o)), want, "{name} {o}");
    }
}

#[test]
fn a_small_budget_binds_for_a_store_archive() {
    small_budget_case(
        "store",
        Tier::Store { block: MIB },
        4,
        6,
        EnvelopeTier::Store,
    );
}

#[test]
fn a_small_budget_binds_for_a_fast_archive() {
    small_budget_case("fast", Tier::Fast { block: MIB }, 4, 6, EnvelopeTier::Fast);
}

#[test]
fn a_small_budget_binds_for_a_balanced_archive() {
    small_budget_case(
        "balanced",
        Tier::Balanced {
            block: MIB,
            dict: 1 << 20,
        },
        4,
        6,
        EnvelopeTier::Fast,
    );
}

/// A peeled archive (JPEG records): the nested lower block is counted, and the bound binds.
#[test]
fn a_small_budget_binds_for_a_peeled_archive() {
    let src = tempfile::tempdir().unwrap();
    let jpeg = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("baseline.jpg"),
    )
    .unwrap();
    // Without deduplication each copy is peeled and stored on its own.
    for i in 0..8 {
        std::fs::write(src.path().join(format!("p{i}.jpg")), &jpeg).unwrap();
    }
    write_tree(src.path(), 4, 2);
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("p.lpk");
    let mut p = Pipeline::fast(FastOptions {
        block_size: MIB,
        threads: Some(THREADS),
        ..FastOptions::default()
    });
    p.fold = None;
    let run = p.run_file(src.path(), &arch).unwrap();
    assert!(run.peel.peeled.files >= 1, "{:?}", run.peel);
    let want = hashes(src.path());
    let (wide, _) = measure(&arch, &work.path().join("o0"), THREADS, Some(u64::MAX / 2));
    assert!(wide.memory.nested > 0, "{:?}", wide.memory);
    let budget = wide.memory.total(3);
    let (s, peak) = measure(&arch, &work.path().join("o1"), THREADS, Some(budget));
    assert_eq!(s.workers, 3);
    check_bound("peeled", &s, peak);
    measure(&arch, &work.path().join("o2"), 1, Some(budget));
    for o in ["o0", "o1", "o2"] {
        assert_eq!(hashes(&work.path().join(o)), want, "peeled {o}");
    }
}

/// Under the default budget at 24 threads, with several blocks more than the budget lets decode
/// at once: `per_block` estimates the blocks the budget holds, and the tree has four more.
fn default_budget_case(name: &str, tier: Tier, block: u64, per_block_estimate: u64, ceiling: u64) {
    let budget = Budget::resolve(None, ceiling).bytes;
    let blocks = budget / per_block_estimate + 4;
    let block_mib = block / MIB;
    let src = tempfile::tempdir().unwrap();
    write_tree(src.path(), blocks, block_mib);
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("a.lpk");
    pack(&tier, src.path(), &arch);
    let want = hashes(src.path());
    drop(src);
    let (s, peak) = measure(&arch, &work.path().join("o1"), THREADS, None);
    assert!(
        matches!(s.memory.budget.source, BudgetSource::Default { ceiling: c, .. } if c == ceiling),
        "{name}: {:?}",
        s.memory
    );
    assert_eq!(s.memory.budget.bytes, budget, "{name}");
    check_bound(name, &s, peak);
    assert_eq!(hashes(&work.path().join("o1")), want, "{name}");
    std::fs::remove_dir_all(work.path().join("o1")).unwrap();
    measure(&arch, &work.path().join("o2"), 1, None);
    assert_eq!(
        hashes(&work.path().join("o2")),
        want,
        "{name} at one thread"
    );
}

#[test]
#[ignore = "heavy: about half the store ceiling of data; run in release with --run-ignored"]
fn the_default_budget_binds_for_a_store_archive() {
    let block = lpk_format::DEFAULT_BLOCK_SIZE;
    default_budget_case(
        "store default",
        Tier::Store { block },
        block,
        2 * block,
        lpk_core::memory::STORE_CEILING,
    );
}

#[test]
#[ignore = "heavy: about half the Fast ceiling of data; run in release with --run-ignored"]
fn the_default_budget_binds_for_a_fast_archive() {
    let block = lpk_format::DEFAULT_BLOCK_SIZE;
    default_budget_case(
        "fast default",
        Tier::Fast { block },
        block,
        2 * block + 2 * MIB,
        FAST_CEILING,
    );
}

#[test]
#[ignore = "heavy: about half the Balanced ceiling of data; run in release with --run-ignored"]
fn the_default_budget_binds_for_a_balanced_archive() {
    let block = 128 * MIB;
    default_budget_case(
        "balanced default",
        Tier::Balanced {
            block,
            dict: block as u32,
        },
        block,
        2 * block + 2 * MIB,
        BALANCED_CEILING,
    );
}
