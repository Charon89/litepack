//! The `lpk` binary against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-cli --release --test corpus -- --ignored --nocapture`
//!
//! For every class directory: `lpk a` (Fast), `lpk x`, `lpk t`; the extracted tree must equal the
//! source tree (BLAKE3 of every file) and the archive's entry count must equal an independent
//! walk of the source.
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use lpk_format::{Archive, Resources};

/// Every path under `dir` (relative, `/`-separated), with the BLAKE3 of files.
fn walk(dir: &Path) -> Vec<(String, Option<[u8; 32]>)> {
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
                stack.push((e.path(), rel.clone()));
                out.push((rel, None));
            } else {
                let mut h = blake3::Hasher::new();
                std::io::copy(&mut BufReader::new(File::open(e.path()).unwrap()), &mut h).unwrap();
                out.push((rel, Some(*h.finalize().as_bytes())));
            }
        }
    }
    out.sort();
    out
}

fn lpk(args: &[&Path], pre: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lpk"))
        .arg(pre)
        .args(args)
        .output()
        .unwrap()
}

#[test]
#[ignore]
fn cli_round_trip_every_class() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut classes: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    classes.sort();
    assert!(!classes.is_empty());
    let work = tempfile::tempdir().unwrap();
    println!("| class | entries | files | archive B | a s | x s | t s |");
    println!("|---|---|---|---|---|---|---|");
    for dir in classes {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let arch = work.path().join(format!("{name}.lpk"));
        let out = work.path().join(&name);
        let t = Instant::now();
        let a = lpk(&[&arch, &dir], "a");
        let ta = t.elapsed().as_secs_f64();
        assert_eq!(a.status.code(), Some(0), "{name}: {:?}", a.stderr);
        let t = Instant::now();
        let x = lpk(&[&arch, &out], "x");
        let tx = t.elapsed().as_secs_f64();
        assert_eq!(x.status.code(), Some(0), "{name}: {:?}", x.stderr);
        let t = Instant::now();
        let v = lpk(&[&arch], "t");
        let tt = t.elapsed().as_secs_f64();
        assert_eq!(v.status.code(), Some(0), "{name}: {:?}", v.stderr);
        let want = walk(&dir);
        assert_eq!(walk(&out), want, "{name}: extracted tree differs");
        let mut ar = Archive::open(File::open(&arch).unwrap(), &Resources::default()).unwrap();
        let entries = ar.entry_table().unwrap().table().unwrap().iter().count();
        assert_eq!(entries, want.len(), "{name}: entry count");
        let files = want.iter().filter(|(_, h)| h.is_some()).count();
        println!(
            "| {name} | {entries} | {files} | {} | {ta:.2} | {tx:.2} | {tt:.2} |",
            std::fs::metadata(&arch).unwrap().len()
        );
        std::fs::remove_dir_all(&out).unwrap();
        std::fs::remove_file(&arch).unwrap();
    }
}
