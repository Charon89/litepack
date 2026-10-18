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

/// The default settings of each tier at its ceiling with 24 threads (no inventory): the figures
/// the report quotes, and every plan within its ceiling.
#[test]
fn default_settings_fit_their_ceilings() {
    use lpk_core::memory::{BALANCED_CEILING, STORE_CEILING};
    let ceiling = |c: u64| Budget::default_for(c, None);
    let f = FastOptions::default();
    let b = BalancedOptions::default();
    let (enc, _) = lpk_core::BalancedEncoder::new(&b).unwrap();
    let base = |block: u64, encoder: EncoderMemory, peel: Option<u64>| ModelInputs {
        block_size: block,
        chunk_size: 512 << 10,
        avg_chunk: 64 << 10,
        encoder,
        peel_max_file: peel,
        deflate: None,
        entries: 0,
        path_bytes: 0,
        input_bytes: 0,
        prefetch: Default::default(),
        recovery: 0,
        base_chunks: 0,
    };
    let store = ModelInputs {
        chunk_size: lpk_format::DEFAULT_CHUNK_SIZE,
        avg_chunk: lpk_format::DEFAULT_CHUNK_SIZE,
        ..base(lpk_format::DEFAULT_BLOCK_SIZE, EncoderMemory::Store, None)
    };
    let fast = base(
        f.block_size,
        EncoderMemory::Zstd {
            level: f.level,
            window_log: window_for(f.window_log, f.block_size),
            dictionaries: 0,
            dictionary_bytes: 0,
        },
        Some(f.jpeg_max_file),
    );
    let balanced = base(
        b.block_size,
        EncoderMemory::Balanced {
            lzma: enc.lzma_memory(),
            lzma_sample: enc.lzma_sample_memory(),
            zstd_sample_window_log: window_for(b.dict_size.ilog2(), enc.stripe_len() as u64),
            zstd_level: b.zstd_level,
            zstd_window_log: window_for(b.dict_size.ilog2(), b.block_size),
            bwt: None,
        },
        Some(b.jpeg_max_file),
    );
    // Balanced with a `Text` cluster: the BWT stage's terms (revision 1.3, design section 4).
    let balanced_bwt = ModelInputs {
        encoder: match balanced.encoder {
            EncoderMemory::Balanced {
                lzma,
                lzma_sample,
                zstd_sample_window_log,
                zstd_level,
                zstd_window_log,
                ..
            } => EncoderMemory::Balanced {
                lzma,
                lzma_sample,
                zstd_sample_window_log,
                zstd_level,
                zstd_window_log,
                bwt: Some(enc.bwt_memory().unwrap()),
            },
            _ => unreachable!(),
        },
        ..balanced
    };
    // Balanced with its default Deflate peel (design 3.2, note 3 of task D).
    let d = lpk_core::peel::deflate::DeflatePeel::default();
    let part = lpk_core::peel::deflate::PartEncoder::balanced(&b, d.plain_limit()).unwrap();
    let balanced_deflate = ModelInputs {
        deflate: Some(lpk_core::memory::DeflateMemory {
            max_file: d.max_file,
            plain_limit: d.plain_limit(),
            encoder: part.memory(),
            records: d.records_cap,
        }),
        ..balanced
    };
    for (name, m, c) in [
        ("store", store, STORE_CEILING),
        ("fast", fast, FAST_CEILING),
        ("balanced", balanced, BALANCED_CEILING),
        ("balanced+deflate", balanced_deflate, BALANCED_CEILING),
        (
            "balanced+bwt (a Text cluster)",
            balanced_bwt,
            BALANCED_CEILING,
        ),
    ] {
        let p = m.plan(ceiling(c), 24).unwrap();
        println!(
            "{name}: ceiling {c}, per_block {}, encoder {}, fixed_sequential {}, fixed_pipelined {}, blocks in flight {}, threads_hint {}",
            p.per_block,
            m.encoder_memory(),
            p.fixed_sequential,
            p.fixed_pipelined,
            p.blocks_in_flight,
            m.threads_hint(c)
        );
        assert!(
            p.fixed() + p.per_block * p.blocks_in_flight as u64 <= c,
            "{name}"
        );
    }
}

const MIB: u64 = 1 << 20;

/// Spec section 9: every stored block declares no working memory, so an archive of stored
/// blocks — the store tier's, and the Fast and Balanced tiers' when every block is stored —
/// declares `max_window` 0, `max_bwt_block` 0 and `decode_memory` equal to `max_block_plain`.
#[test]
fn stored_blocks_declare_no_working_memory() {
    let dir = tempfile::tempdir().unwrap();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let noise: Vec<u8> = (0..3 * MIB)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(dir.path().join("r.bin"), &noise).unwrap();
    let store = Pipeline::store(StoreOptions {
        threads: Some(1),
        ..StoreOptions::default()
    });
    for (name, p) in [
        ("store", store),
        ("fast", fast(1, None)),
        ("balanced", balanced(1, None)),
    ] {
        let (bytes, _) = run(p, dir.path()).unwrap();
        let e = envelope(&bytes);
        assert_eq!(
            (e.max_window, e.max_bwt_block, e.decode_memory),
            (0, 0, e.max_block_plain),
            "{name}"
        );
        assert!(
            e.threads_hint >= 1,
            "{name}: the hint is written for any archive"
        );
    }
}

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
    let expect = Budget::default_for(FAST_CEILING, lpk_core::memory::physical_memory());
    assert_eq!(s.memory.budget, expect);
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
        deflate: None,
        entries: 0,
        path_bytes: 0,
        input_bytes: 0,
        prefetch: Default::default(),
        recovery: 0,
        base_chunks: 0,
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
    if b.bwt_blocks > 0 {
        // A `[lzma, bwt]` block declares the larger of section 8's term for its plain length
        // and one BWT block (the block is shorter than the stage's 64 MiB) and the `lzma`
        // step's own figure: above any plain block's figure, at most the term of the largest
        // block (c(n) varies by a few indices).
        let term = lpk_codecs_bwt::layout::memory_term(p, p);
        assert!(
            e.decode_memory > p + lzma_decode_memory(1 << 20, p)
                && e.decode_memory <= p + term + 4 * 32,
            "{e:?}"
        );
        assert!(
            e.max_bwt_block >= 1 && e.max_bwt_block <= p,
            "B = min(bwt_block, plain_len): {e:?}"
        );
    } else if b.zstd_blocks == 0 {
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

/// The Deflate stage's memory term counts only when a file of a class it examines is present
/// (design 3.2, task D fix round 1): a text-only tree plans as without the stage, a tree with a
/// gzip member plans the stage's term, sized to that file.
#[test]
fn the_deflate_term_counts_only_with_an_examined_file() {
    let plan = |dir: &Path| {
        let mut p = Pipeline::balanced(BalancedOptions {
            threads: Some(24),
            ..BalancedOptions::default()
        });
        p.seal.memory = Some(lpk_core::memory::BALANCED_CEILING);
        let off = {
            let mut q = Pipeline::balanced(BalancedOptions {
                threads: Some(24),
                ..BalancedOptions::default()
            });
            q.seal.memory = Some(lpk_core::memory::BALANCED_CEILING);
            q.deflate = None;
            run(q, dir).unwrap().1
        };
        (run(p, dir).unwrap().1, off)
    };
    let text = tempfile::tempdir().unwrap();
    std::fs::write(text.path().join("a.txt"), prose(1, 200_000)).unwrap();
    let (on, off) = plan(text.path());
    println!(
        "text only: blocks in flight {} (peel off {}), fixed_pipelined {} (peel off {})",
        on.blocks_in_flight,
        off.blocks_in_flight,
        on.memory.fixed_pipelined,
        off.memory.fixed_pipelined
    );
    assert_eq!(on.memory, off.memory);
    let gz = tempfile::tempdir().unwrap();
    std::fs::write(gz.path().join("a.txt"), prose(1, 200_000)).unwrap();
    std::fs::write(
        gz.path().join("n.gz"),
        std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deflate/notes.txt.gz"),
        )
        .unwrap(),
    )
    .unwrap();
    let (on, off) = plan(gz.path());
    println!(
        "with a gzip member: blocks in flight {} (peel off {}), fixed_pipelined {} (peel off {})",
        on.blocks_in_flight,
        off.blocks_in_flight,
        on.memory.fixed_pipelined,
        off.memory.fixed_pipelined
    );
    // A small gzip member can only hold a small text: the stage's set (sized to it) stays below
    // the JPEG peel's, which the plan already counts, so the plan is the one without the stage
    // (but for the peel pool, which takes one of the threads: design
    // `docs/notes/e2-10-peel-pool-design.md`, section 2).
    let m = on.memory;
    assert_eq!(
        lpk_core::memory::MemoryPlan {
            blocks_in_flight: off.memory.blocks_in_flight,
            peel_pool: 0,
            peel_workers: 0,
            ..m
        },
        off.memory
    );
    if on.threads > 1 {
        assert_eq!(
            m.blocks_in_flight,
            off.memory.blocks_in_flight.min(on.threads - 1)
        );
        assert_eq!(m.peel_workers, on.threads - m.blocks_in_flight);
        assert!(m.peel_pool > 0);
    }
}

/// The part encoder plans for the largest text the examined files can hold (Deflate's
/// expansion of the largest file, at most the plain-text limit): a tree of small office files
/// against one holding a large PDF.
#[test]
fn small_examined_files_plan_a_smaller_part_encoder() {
    let plan = |dir: &Path| {
        let mut p = Pipeline::balanced(BalancedOptions {
            threads: Some(24),
            ..BalancedOptions::default()
        });
        p.seal.memory = Some(lpk_core::memory::BALANCED_CEILING);
        run(p, dir).unwrap().1
    };
    let docx = |seed: u64| {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("word/document.xml", o).unwrap();
        std::io::Write::write_all(&mut w, &prose(seed, 40_000)).unwrap();
        w.finish().unwrap().into_inner()
    };
    let small = tempfile::tempdir().unwrap();
    for i in 0..4u64 {
        std::fs::write(small.path().join(format!("d{i}.docx")), docx(i + 1)).unwrap();
    }
    let s = plan(small.path());
    let large = tempfile::tempdir().unwrap();
    for i in 0..4u64 {
        std::fs::write(large.path().join(format!("d{i}.docx")), docx(i + 1)).unwrap();
    }
    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.extend_from_slice(&prose(9, 300_000));
    std::fs::write(large.path().join("big.pdf"), pdf).unwrap();
    let l = plan(large.path());
    println!(
        "small office files: blocks in flight {}, fixed_pipelined {}; with a large PDF: blocks in flight {}, fixed_pipelined {}",
        s.blocks_in_flight, s.memory.fixed_pipelined, l.blocks_in_flight, l.memory.fixed_pipelined
    );
    assert!(s.memory.fixed_pipelined < l.memory.fixed_pipelined);
}
