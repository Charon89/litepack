//! The Deflate peel's stream share against Phase 0's probe (design `docs/notes/e2-10-11-design.md`,
//! section 7), run by hand in release in the workspace build:
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test deflate_share_corpus -- --ignored --nocapture`
//!
//! Each of the probe's classes goes through the pipeline with the Deflate peel on, dedup off and
//! `record_files` on (the Fast tier: the counts do not depend on the model). Asserted: the probe
//! settings recorded in `bench/results/2026-10-03-megatron-3/probe-deflate.json` equal the moved
//! constants; no file of the probe's classes exceeds the 256 MiB cap; every file the probe found
//! streams in was walked; for every walked file, kind, found, reconstructed, skipped, failed by
//! name, structural refusals, scan false hits and walker panics equal that file's probe record;
//! the class sums equal.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lpk_core::peel::deflate::{
    DeflatePeel, FILE_PLAIN_BUDGET, MAX_CHAIN_LENGTH, MAX_FILE, MIN_SCANNED_PLAIN, PLAIN_TEXT_LIMIT,
};
use lpk_core::{FastOptions, Pipeline};

/// The comparable counts of one file: kind, found, reconstructed, skipped, failed by name,
/// structural refusals, scan false hits, walker panics.
type Key = (String, u64, u64, u64, BTreeMap<String, u64>, u64, u64, u64);

fn probe_key(f: &serde_json::Value) -> Key {
    let s = &f["streams"];
    let n = |k: &str| s[k].as_u64().unwrap();
    let failed = s["failed"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_u64().unwrap()))
        .collect();
    (
        f["kind"].as_str().unwrap().to_string(),
        n("found"),
        n("reconstructed"),
        n("skipped"),
        failed,
        n("structural_refusals"),
        n("scan_false_hits"),
        n("walker_panics"),
    )
}

fn file_count(dir: &Path) -> (u64, u64) {
    let mut n = 0;
    let mut largest = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let t = e.file_type().unwrap();
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() {
                n += 1;
                largest = largest.max(e.metadata().unwrap().len());
            }
        }
    }
    (n, largest)
}

#[test]
#[ignore]
fn the_stream_counts_equal_the_probes_on_every_walked_file() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let probe_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bench/results/2026-10-03-megatron-3/probe-deflate.json");
    let probe: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(probe_path).unwrap()).unwrap();
    let set = &probe["data"]["settings"];
    assert_eq!(set["plain_text_limit"].as_u64(), Some(PLAIN_TEXT_LIMIT));
    assert_eq!(set["file_plain_budget"].as_u64(), Some(FILE_PLAIN_BUDGET));
    assert_eq!(
        set["max_chain_length"].as_u64(),
        Some(u64::from(MAX_CHAIN_LENGTH))
    );
    assert_eq!(set["min_scanned_plain"].as_u64(), Some(MIN_SCANNED_PLAIN));
    println!("| class | probe files with streams | walked | found | reconstructed | probe reconstructed | kept | no gain | block cap | memory cap | already stored | file gate | file share |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for class in probe["data"]["classes"].as_array().unwrap() {
        let name = class["class"].as_str().unwrap();
        let dir = root.join(name);
        let (_, largest) = file_count(&dir);
        assert!(largest <= MAX_FILE, "{name}: a file above the size cap");
        let mut p = Pipeline::fast(FastOptions::default());
        p.fold = None;
        p.deflate = Some(DeflatePeel {
            record_files: true,
            ..DeflatePeel::default()
        });
        let s = p.run(&dir, std::io::sink()).unwrap();
        let d = &s.peel.deflate;
        assert_eq!(d.size_cap.files, 0, "{name}");
        assert_eq!(
            d.streams.work_budget_stopped, 0,
            "{name}: a file reached its work budget"
        );
        let ours: BTreeMap<String, Key> = d
            .files
            .as_ref()
            .unwrap()
            .iter()
            .map(|f| {
                let c = &f.counts;
                (
                    format!("{name}/{}", f.path),
                    (
                        f.kind.to_string(),
                        c.found,
                        c.reconstructed,
                        c.skipped,
                        c.failed.clone(),
                        c.structural_refusals,
                        c.scan_false_hits,
                        c.walker_panics,
                    ),
                )
            })
            .collect();
        let theirs: BTreeMap<String, Key> = class["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["path"].as_str().unwrap().to_string(), probe_key(f)))
            .collect();
        let mut with_streams = 0;
        let (mut probe_found, mut probe_rec) = (0, 0);
        for (path, key) in &theirs {
            probe_found += key.1;
            probe_rec += key.2;
            if key.1 > 0 {
                with_streams += 1;
                assert!(
                    ours.contains_key(path),
                    "{path}: the probe found streams, not walked"
                );
            }
        }
        for (path, key) in &ours {
            match theirs.get(path) {
                Some(want) => assert_eq!(key, want, "{path}"),
                None => assert_eq!(key.1, 0, "{path}: streams the probe did not see"),
            }
        }
        let (found, rec): (u64, u64) = ours.values().fold((0, 0), |a, k| (a.0 + k.1, a.1 + k.2));
        assert_eq!((found, rec), (probe_found, probe_rec), "{name}: class sums");
        assert_eq!(d.outcomes.total(), d.streams.reconstructed, "{name}");
        let o = &d.outcomes;
        println!(
            "| {name} | {with_streams} | {} | {found} | {rec} | {probe_rec} | {} | {} | {} | {} | {} | {} | {} |",
            ours.len(),
            o.kept,
            o.no_gain,
            o.block_cap,
            o.memory_cap,
            o.already_stored,
            o.file_gate,
            o.file_share
        );
    }
}
