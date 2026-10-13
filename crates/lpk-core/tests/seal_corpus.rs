//! Encryption against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test seal_corpus -- --ignored --nocapture`
//!
//! For every class: the Fast tier writes a plain archive and an encrypted one (a password,
//! AES-256-GCM, the format's default Argon2id cost); the encrypted archive is extracted with
//! `extract_file` and every file is checked by BLAKE3 against the corpus. Prints the sizes and
//! the wall seconds of both runs side by side; the figures are a report only (the runner's
//! `fast-encrypted` row is the measurement).
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{extract_file, DefaultPolicy, ExtractOptions, FastOptions, Pipeline};
use lpk_format::{Argon2Params, Credentials, SealOptions, Suite};

fn hash_file(p: &Path) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    std::io::copy(&mut BufReader::new(File::open(p).unwrap()), &mut h).unwrap();
    h.finalize()
}

/// Every file under `dir` (relative, `/`-separated).
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

#[test]
#[ignore]
fn every_class_encrypted_round_trips_bit_exact() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut classes: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    classes.sort();
    assert!(!classes.is_empty());
    let creds = Credentials::password("lpk-corpus-test");
    println!("| class | files | MB | plain B | plain s | encrypted B | encrypted s | delta B | extract s |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for dir in classes {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let plain = work.path().join("plain.lpk");
        let t0 = Instant::now();
        let p = Pipeline::fast(FastOptions::default())
            .run_file(&dir, &plain)
            .unwrap();
        let plain_secs = t0.elapsed().as_secs_f64();

        let sealed = work.path().join("sealed.lpk");
        let mut pipeline = Pipeline::fast(FastOptions::default());
        pipeline.seal.writer.seal = Some(SealOptions {
            suite: Suite::AesGcm,
            argon2: Argon2Params::default(),
            listable: false,
            credentials: creds.clone(),
        });
        let t0 = Instant::now();
        let s = pipeline.run_file(&dir, &sealed).unwrap();
        let sealed_secs = t0.elapsed().as_secs_f64();
        assert_eq!(p.peel, s.peel, "{name}: the peel differs under encryption");
        assert_eq!(p.writer.chunks, s.writer.chunks, "{name}");
        assert_eq!(p.writer.blocks, s.writer.blocks, "{name}");
        assert_eq!(p.writer.deduped_bytes, s.writer.deduped_bytes, "{name}");

        let out = work.path().join("out");
        let t0 = Instant::now();
        let x = extract_file(
            &sealed,
            &out,
            &[],
            Some(&creds),
            &DefaultPolicy,
            &ExtractOptions::default(),
        )
        .unwrap();
        let extract_secs = t0.elapsed().as_secs_f64();
        let list = files(&dir);
        assert_eq!(x.files, list.len() as u64, "{name}");
        let mut bytes = 0u64;
        for rel in &list {
            let src = join(&dir, rel);
            bytes += std::fs::metadata(&src).unwrap().len();
            assert_eq!(hash_file(&src), hash_file(&join(&out, rel)), "{name}/{rel}");
        }
        println!(
            "| {name} | {} | {:.1} | {} | {plain_secs:.2} | {} | {sealed_secs:.2} | {} | {extract_secs:.2} |",
            list.len(),
            bytes as f64 / 1e6,
            p.writer.archive_len,
            s.writer.archive_len,
            s.writer.archive_len as i64 - p.writer.archive_len as i64,
        );
    }
}
