//! Every class of the corpus through the default Balanced pipeline (its Deflate peel on), run by
//! hand in release (design `docs/notes/e2-10-11-design.md`, section 7):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test deflate_corpus -- --ignored --nocapture`
//!
//! Asserted: the D-54 engine extracts each archive and every file equals its source (0
//! mismatches); `verify` is clean. `lpk-check` reads the same archives: every file it extracts
//! equals its source, and the only refusal allowed is `deflate-reconstruct` (id 8) while
//! `lpk-check` does not implement revision 1.2 yet (task E); the count is printed. Printed per
//! class: raw and archive bytes, the Deflate summary (kept by class, not kept by cause), seconds.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{extract_file, BalancedOptions, DefaultPolicy, ExtractOptions, Pipeline};

fn files(dir: &Path) -> Vec<(String, PathBuf)> {
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
            let t = e.file_type().unwrap();
            if t.is_dir() {
                stack.push((e.path(), rel));
            } else if t.is_file() {
                out.push((rel, e.path()));
            }
        }
    }
    out.sort();
    out
}

#[test]
#[ignore]
fn every_class_round_trips_through_the_default_balanced_pipeline() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut classes: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    classes.sort();
    println!("| class | files | raw B | archive B | walked | reconstructed | kept | kept by class | no gain | block cap | memory cap | already stored | file gate | size cap | lpk-check refused (id 8) | s compress | s extract |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut mismatches = 0;
    for dir in classes {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let arch = work.path().join("a.lpk");
        let t = Instant::now();
        let s = Pipeline::balanced(BalancedOptions::default())
            .run_file(&dir, &arch)
            .unwrap();
        let compress = t.elapsed().as_secs_f64();
        let out = work.path().join("x");
        let t = Instant::now();
        extract_file(
            &arch,
            &out,
            &[],
            None,
            &DefaultPolicy,
            &ExtractOptions::default(),
        )
        .unwrap();
        let extract = t.elapsed().as_secs_f64();
        let src = files(&dir);
        let raw: u64 = src
            .iter()
            .map(|(_, p)| std::fs::metadata(p).unwrap().len())
            .sum();
        for (rel, p) in &src {
            let a = std::fs::read(p).unwrap();
            let b = std::fs::read(out.join(rel)).unwrap_or_default();
            if a != b {
                mismatches += 1;
                println!("MISMATCH {name}/{rel}");
            }
        }
        let mut a = lpk_format::Archive::open(
            std::fs::File::open(&arch).unwrap(),
            &lpk_format::Resources::default(),
        )
        .unwrap();
        lpk_core::register_full_reader(&mut a);
        a.verify().unwrap();
        // The independent decoder.
        let bytes = std::fs::read(&arch).unwrap();
        let mut c =
            lpk_check::archive::Archive::open(bytes, lpk_check::archive::Options::default())
                .unwrap();
        let mut refused = 0;
        for e in c.entries().unwrap() {
            if e.kind != lpk_check::entries::EntryKind::File {
                continue;
            }
            match c.read_file(&e) {
                Ok(v) => assert!(
                    v == std::fs::read(dir.join(&e.path)).unwrap(),
                    "lpk-check: {name}/{}",
                    e.path
                ),
                Err(err) => {
                    let m = format!("{err:?} {err}");
                    assert!(
                        m.contains("Unimplemented") && m.contains('8'),
                        "lpk-check: {name}/{}: {m}",
                        e.path
                    );
                    refused += 1;
                }
            }
        }
        let d = &s.peel.deflate;
        let o = &d.outcomes;
        let walked: u64 = d.walked_by_kind.values().sum();
        let classes: Vec<String> = d
            .kept_by_class
            .iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect();
        println!(
            "| {name} | {} | {raw} | {} | {walked} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {refused} | {compress:.1} | {extract:.1} |",
            src.len(),
            s.writer.archive_len,
            d.streams.reconstructed,
            o.kept,
            classes.join(", "),
            o.no_gain,
            o.block_cap,
            o.memory_cap,
            o.already_stored,
            o.file_gate,
            d.size_cap.files
        );
        assert_eq!(o.total(), d.streams.reconstructed, "{name}");
    }
    println!("mismatches: {mismatches}");
    assert_eq!(mismatches, 0);
}
