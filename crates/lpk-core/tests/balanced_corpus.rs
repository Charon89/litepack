//! Balanced tier against the corpus, run by hand (release build):
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test balanced_corpus -- --ignored --nocapture`
//!
//! Packs `text-prose`, `logs-text`, `office-pdf` and `source-git` with the Balanced tier, extracts
//! every file and checks it bit for bit and the entry list, and prints per class the raw bytes,
//! the Balanced and Fast archive bytes, the block outcomes and the wall seconds of compress and
//! extract. For context it also prints the committed 7-Zip Ultra median archive bytes of the same
//! classes (`bench/results/2026-10-03-megatron/7z-ultra-<class>.json`); only the round trip is
//! asserted. The runner row is the acceptance's evidence.
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::time::Instant;

use lpk_core::{archive_balanced, archive_fast, BalancedOptions, FastOptions};
use lpk_format::{Archive, EntryKind, Resources};

fn walk_names(dir: &Path) -> Vec<String> {
    let mut expected: Vec<String> = Vec::new();
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
            }
            expected.push(rel);
        }
    }
    expected.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    expected
}

/// The committed 7-Zip Ultra median archive bytes of `class`, if the result file is there.
fn ultra_bytes(class: &str) -> Option<u64> {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/results/2026-10-03-megatron");
    let text = std::fs::read_to_string(root.join(format!("7z-ultra-{class}.json"))).ok()?;
    let rest = &text[text.find("\"median\"")?..];
    let key = "\"archive_bytes\"";
    let rest = &rest[rest.find(key)? + key.len()..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[test]
#[ignore]
fn balanced_round_trip_four_classes() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    println!(
        "| class | raw B | balanced B | fast B | 7z ultra B (committed) | lzma/zstd/gate/class/no-gain | sample B | s compress | s extract |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for name in ["text-prose", "logs-text", "office-pdf", "source-git"] {
        let dir = root.join(name);
        let t0 = Instant::now();
        let mut bytes: Vec<u8> = Vec::new();
        let (s, b) = archive_balanced(&dir, &mut bytes, BalancedOptions::default()).unwrap();
        let compress = t0.elapsed().as_secs_f64();
        let mut fast: Vec<u8> = Vec::new();
        let fast_len = archive_fast(&dir, &mut fast, FastOptions::default())
            .unwrap()
            .0
            .archive_len;
        drop(fast);

        let mut a = Archive::open(Cursor::new(&bytes[..]), &Resources::default()).unwrap();
        lpk_core::register_full_reader(&mut a);
        let table = a.entry_table().unwrap();
        let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
        let got: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(got, walk_names(&dir), "{name}: entry list differs");
        let mut raw = 0u64;
        let mut extract = 0.0;
        for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
            let t = Instant::now();
            let mut out = Vec::new();
            a.extract(e, &mut out).unwrap();
            extract += t.elapsed().as_secs_f64();
            let mut f = BufReader::new(File::open(dir.join(&e.path)).unwrap());
            let mut h = blake3::Hasher::new();
            std::io::copy(&mut f, &mut h).unwrap();
            assert_eq!(blake3::hash(&out), h.finalize(), "{name}/{}", e.path);
            raw += out.len() as u64;
        }
        a.verify().unwrap();
        let ultra = ultra_bytes(name).map_or("n/a".to_string(), |v| v.to_string());
        println!(
            "| {name} | {raw} | {} | {fast_len} | {ultra} | {}/{}/{}/{}/{} | {} | {compress:.2} | {extract:.2} |",
            s.archive_len,
            b.lzma_blocks,
            b.zstd_blocks,
            b.stored_by_gate,
            b.stored_by_class,
            b.stored_no_gain,
            b.sample_bytes
        );
    }
}
