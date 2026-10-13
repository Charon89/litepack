//! The journal against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test journal_corpus -- --ignored --nocapture`
//!
//! The `backup-versions` class (folders `v1`, `v2`, `v3`) through the Fast tier: `v1` as
//! generation 0, then `v2` and `v3` appended (generations 1 and 2, each a snapshot of its
//! folder), next to the one-shot archive of the class directory (the three folders in one
//! generation, dedup on). The appended archive must be within the per-generation overhead
//! (index + trailer + entry table + records frame of generations 1 and 2, read from the
//! archive) of the one-shot; rollback to generation 0 then extracts bit-exactly to `v1`
//! (BLAKE3 per file). Sizes and times are printed for the report only.
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{extract_file, DefaultPolicy, ExtractOptions, FastOptions, Pipeline, RunSummary};
use lpk_format::{Archive, Resources, TRAILER_FRAME_LEN};

fn hash_file(p: &Path) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    std::io::copy(&mut BufReader::new(File::open(p).unwrap()), &mut h).unwrap();
    h.finalize()
}

/// Every file under `dir` (relative, `/`-separated), sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, prefix)) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let n = e.file_name().into_string().unwrap();
            let rel = if prefix.is_empty() {
                n
            } else {
                format!("{prefix}/{n}")
            };
            if e.file_type().unwrap().is_dir() {
                stack.push((e.path(), rel));
            } else {
                out.push(rel);
            }
        }
    }
    out.sort();
    out
}

fn join(dir: &Path, rel: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for c in rel.split('/') {
        p.push(c);
    }
    p
}

fn open(path: &Path) -> Archive<File> {
    Archive::open(File::open(path).unwrap(), &Resources::default()).unwrap()
}

/// Index + trailer + entry table (+ records) of `generation`, read from a copy rolled back to
/// it.
fn generation_overhead(archive: &Path, generation: u64, scratch: &Path) -> u64 {
    let copy = scratch.join(format!("gen-{generation}.lpk"));
    std::fs::copy(archive, &copy).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&copy)
        .unwrap();
    assert_eq!(
        lpk_format::rollback(&mut f, generation).unwrap().generation,
        generation
    );
    drop(f);
    let a = open(&copy);
    let index = a.index();
    let start = index
        .generations
        .iter()
        .find(|g| g.generation == generation)
        .unwrap()
        .start_offset;
    let mut n = a.trailer().index_len + TRAILER_FRAME_LEN + index.entry_table.len;
    if let Some(r) = index.records {
        if r.offset >= start {
            n += r.len;
        }
    }
    std::fs::remove_file(&copy).unwrap();
    n
}

fn timed(f: impl FnOnce() -> RunSummary) -> (RunSummary, f64) {
    let t = Instant::now();
    let s = f();
    (s, t.elapsed().as_secs_f64())
}

#[test]
#[ignore]
fn backup_versions_as_three_generations_is_within_the_overhead_and_rolls_back_to_v1() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let class = root.join("backup-versions");
    let tmp = tempfile::tempdir().unwrap();
    let appended = tmp.path().join("appended.lpk");
    let one_shot = tmp.path().join("one-shot.lpk");
    let fast = || Pipeline::fast(FastOptions::default());

    let (s0, t0) = timed(|| fast().run_file(&class.join("v1"), &appended).unwrap());
    let (s1, t1) = timed(|| {
        fast()
            .append_file(&class.join("v2"), &appended, None)
            .unwrap()
    });
    let (s2, t2) = timed(|| {
        fast()
            .append_file(&class.join("v3"), &appended, None)
            .unwrap()
    });
    let (o, to) = timed(|| fast().run_file(&class, &one_shot).unwrap());
    assert_eq!((s0.writer.generation, s1.writer.generation), (0, 1));
    assert_eq!(s2.writer.generation, 2);
    let appended_len = std::fs::metadata(&appended).unwrap().len();
    let one_shot_len = std::fs::metadata(&one_shot).unwrap().len();
    let overhead: u64 = (1..=2)
        .map(|g| generation_overhead(&appended, g, tmp.path()))
        .sum();
    println!("| step | archive B | seconds | entries | new chunks | reused chunks | deduped chunks | blocks |");
    println!("|---|---|---|---|---|---|---|---|");
    for (name, s, t) in [
        ("v1 (generation 0)", &s0, t0),
        ("+v2 (generation 1)", &s1, t1),
        ("+v3 (generation 2)", &s2, t2),
        ("one-shot v1+v2+v3", &o, to),
    ] {
        let w = &s.writer;
        println!(
            "| {name} | {} | {t:.2} | {} | {} | {} | {} | {} |",
            w.archive_len, w.entries, w.new_chunks, w.reused_chunks, w.deduped_chunks, w.blocks
        );
    }
    println!(
        "appended {appended_len} B, one-shot {one_shot_len} B, difference {} B, overhead of generations 1 and 2 (index + trailer + entry table + records) {overhead} B",
        appended_len as i64 - one_shot_len as i64
    );
    // Where the rest of a difference comes from: the block data (the chunk table and the plain
    // bytes say whether the dedup found the same; the frame bytes say what the coder made of
    // the changed chunks, which an appended generation codes without the earlier ones in its
    // window).
    for (name, p) in [("appended", &appended), ("one-shot", &one_shot)] {
        let a = open(p);
        let ix = a.index();
        println!(
            "{name}: {} chunks, {} blocks, {} plain B in blocks, {} block frame B",
            a.chunks().len(),
            ix.blocks.len(),
            ix.blocks.iter().map(|b| b.plain_len).sum::<u64>(),
            ix.blocks.iter().map(|b| b.frame_len).sum::<u64>()
        );
    }
    let within = appended_len <= one_shot_len + overhead;

    // Rollback to generation 0 reproduces v1 bit-exactly (checked before the size relation is
    // asserted, so a size failure still leaves the rollback proven).
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&appended)
        .unwrap();
    let g = lpk_format::rollback(&mut f, 0).unwrap();
    assert_eq!(g.generation, 0);
    drop(f);
    assert_eq!(
        std::fs::metadata(&appended).unwrap().len(),
        s0.writer.archive_len
    );
    let out = tmp.path().join("x1");
    let t = Instant::now();
    extract_file(
        &appended,
        &out,
        &[],
        None,
        &DefaultPolicy,
        &ExtractOptions::default(),
    )
    .unwrap();
    let tx = t.elapsed().as_secs_f64();
    let v1 = class.join("v1");
    let expected = files(&v1);
    assert_eq!(files(&out), expected);
    for rel in &expected {
        assert_eq!(
            hash_file(&join(&out, rel)),
            hash_file(&join(&v1, rel)),
            "{rel}"
        );
    }
    println!(
        "rollback to generation 0: {} B, {} files extracted bit-exact in {tx:.2} s",
        s0.writer.archive_len,
        expected.len()
    );
    assert!(
        within,
        "appended {appended_len} > one-shot {one_shot_len} + overhead {overhead}"
    );
}
