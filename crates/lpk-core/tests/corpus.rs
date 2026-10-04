//! Corpus round trip, run by hand:
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test corpus -- --ignored --nocapture`
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::PathBuf;
use std::time::Instant;

use lpk_core::{archive_store, StoreOptions};
use lpk_format::{Archive, EntryKind, Resources};

#[test]
#[ignore]
fn corpus_round_trip_every_class() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let mut classes: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    classes.sort();
    assert!(!classes.is_empty());
    for dir in classes {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let t0 = Instant::now();
        let mut bytes: Vec<u8> = Vec::new();
        let s = archive_store(&dir, &mut bytes, StoreOptions::default()).unwrap();
        let pack = t0.elapsed();

        let mut a = Archive::open(Cursor::new(&bytes[..]), &Resources::default()).unwrap();
        let table = a.entry_table().unwrap();
        let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
        // Independent recursion over the class directory: same entries as the archive.
        let mut expected: Vec<String> = Vec::new();
        let mut stack = vec![(dir.clone(), String::new())];
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
                }
                expected.push(rel);
            }
        }
        expected.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        let got: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(got, expected, "{name}: entry list differs from read_dir");
        let mut plain = 0u64;
        let mut files = 0u64;
        for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
            let mut out = Vec::new();
            a.extract(e, &mut out).unwrap();
            let mut f = BufReader::new(File::open(dir.join(&e.path)).unwrap());
            let mut h = blake3::Hasher::new();
            std::io::copy(&mut f, &mut h).unwrap();
            assert_eq!(blake3::hash(&out), h.finalize(), "{name}/{}", e.path);
            plain += out.len() as u64;
            files += 1;
        }
        a.verify().unwrap();
        let mbps = plain as f64 / 1e6 / pack.as_secs_f64();
        println!(
            "{name}: {files} files, {} entries, plain {plain} B, archive {} B; store path {:.3} s ({mbps:.0} MB/s)",
            s.entries,
            s.archive_len,
            pack.as_secs_f64()
        );
    }
}
