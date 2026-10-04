//! The gate against xz ground truth on the corpus, run by hand:
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test gate_corpus -- --ignored --nocapture`
//!
//! Method of the Phase 0 `entropy-gate` probe: blocks of 1 MiB per file (a final partial block
//! counts from 64 KiB), a block is incompressible when xz preset 9 keeps at least 95 percent of
//! it; positive = incompressible. The measured tables are in the task report, not here.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use lpk_core::{walk, Gate, IngestOptions, Source, GATE_BLOCK};
use lpk_format::EntryKind;

const MIN_TAIL: usize = 64 << 10;
/// Blocks held in memory at once.
const BATCH: usize = 32;

/// The `"profile"` value of the corpus manifest, read without a JSON dependency: the key appears
/// once, at the top level, with a plain string value. The test fails when it cannot be found, so a
/// moved or missing manifest never silently turns the profile-dependent assertions off.
fn manifest_profile(root: &std::path::Path) -> String {
    let text = std::fs::read_to_string(root.join("manifest.json")).expect("manifest.json");
    let key = "\"profile\"";
    let at = text.find(key).expect("manifest has a profile field") + key.len();
    let rest = text[at..]
        .trim_start()
        .strip_prefix(':')
        .expect("profile colon");
    let rest = rest.trim_start().strip_prefix('"').expect("profile string");
    let end = rest.find('"').expect("profile string end");
    rest[..end].to_string()
}

#[derive(Default, Clone, Copy)]
struct Counts {
    tp: u64,
    fp: u64,
    fn_: u64,
    tn: u64,
}

impl Counts {
    fn add(&mut self, truth: bool, said: bool) {
        match (truth, said) {
            (true, true) => self.tp += 1,
            (false, true) => self.fp += 1,
            (true, false) => self.fn_ += 1,
            (false, false) => self.tn += 1,
        }
    }
    fn merge(&mut self, o: &Counts) {
        self.tp += o.tp;
        self.fp += o.fp;
        self.fn_ += o.fn_;
        self.tn += o.tn;
    }
    fn ratio(num: u64, den: u64) -> String {
        if den == 0 {
            "n/a".into()
        } else {
            format!("{:.2}%", 100.0 * num as f64 / den as f64)
        }
    }
    fn line(&self, name: &str) -> String {
        format!(
            "{name:<22} {:>6} {:>6} {:>6} {:>6} {:>10} {:>8}",
            self.tp,
            self.fp,
            self.fn_,
            self.tn,
            Self::ratio(self.tp, self.tp + self.fp),
            Self::ratio(self.tp, self.tp + self.fn_)
        )
    }
}

fn xz_keeps_95(block: &[u8]) -> bool {
    let mut enc = liblzma::write::XzEncoder::new(Vec::new(), 9);
    enc.write_all(block).unwrap();
    let out = enc.finish().unwrap().len();
    out * 100 >= block.len() * 95
}

/// Ground truth of every block, on several threads.
fn truths(blocks: &[Vec<u8>], threads: usize) -> Vec<bool> {
    let out: Mutex<Vec<bool>> = Mutex::new(vec![false; blocks.len()]);
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                if k >= blocks.len() {
                    break;
                }
                let t = xz_keeps_95(&blocks[k]);
                out.lock().unwrap()[k] = t;
            });
        }
    });
    out.into_inner().unwrap()
}

#[test]
#[ignore]
fn gate_against_xz_on_the_corpus() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
    let full_profile = manifest_profile(&root) == "full";
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty());
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8);
    let gate = Gate::DEFAULT;
    let mut per_class: BTreeMap<String, Counts> = BTreeMap::new();
    // False negatives whose whole-block entropy is at or above the full threshold, i.e. ones the
    // sample stage caused (watched on video and encrypted-random).
    let mut sample_caused_fn = 0u64;
    for dir in dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let watch = name == "video" || name == "encrypted-random";
        let c = per_class.entry(name).or_default();
        let mut flush = |batch: &mut Vec<Vec<u8>>, c: &mut Counts| {
            let truth = truths(batch, threads);
            for (b, &t) in batch.iter().zip(&truth) {
                let said = gate.is_incompressible(b);
                if watch && t && !said && lpk_core::entropy(b) >= gate.full_threshold {
                    sample_caused_fn += 1;
                }
                c.add(t, said);
            }
            batch.clear();
        };
        let mut batch: Vec<Vec<u8>> = Vec::new();
        for i in walk(&dir, &IngestOptions::default())
            .unwrap()
            .iter()
            .filter(|i| i.kind == EntryKind::File)
        {
            let source = Source::new();
            let mut r = source.open(i).unwrap();
            loop {
                let mut buf = Vec::new();
                let n = (&mut r)
                    .take(GATE_BLOCK as u64)
                    .read_to_end(&mut buf)
                    .unwrap();
                if n < MIN_TAIL.min(GATE_BLOCK) {
                    break;
                }
                batch.push(buf);
                if batch.len() == BATCH {
                    flush(&mut batch, c);
                }
                if n < GATE_BLOCK {
                    break;
                }
            }
        }
        flush(&mut batch, c);
    }
    println!(
        "{:<22} {:>6} {:>6} {:>6} {:>6} {:>10} {:>8}",
        "class", "TP", "FP", "FN", "TN", "precision", "recall"
    );
    let mut pooled = Counts::default();
    for (n, c) in &per_class {
        println!("{}", c.line(n));
        pooled.merge(c);
    }
    println!("{}", pooled.line("pooled"));
    println!(
        "profile full: {full_profile}; false negatives caused by the sample stage: {sample_caused_fn}"
    );

    let get = |n: &str| {
        *per_class
            .get(n)
            .unwrap_or_else(|| panic!("class {n} is missing from the corpus"))
    };
    // The method's guarantee on any profile: the sample stage causes no false negative.
    assert_eq!(
        sample_caused_fn, 0,
        "the sample stage caused false negatives"
    );
    let enc = get("encrypted-random");
    assert!(enc.tp > 0);
    assert_eq!(enc.fn_, 0, "encrypted-random recall must be 100%");
    let video = get("video");
    assert!(video.tp + video.fn_ > 0);
    // The recall floor holds on the profile Phase 0 measured; elsewhere it is printed only.
    if full_profile {
        assert!(
            video.tp * 100 >= (video.tp + video.fn_) * 99,
            "video recall below 99%: {} of {}",
            video.tp,
            video.tp + video.fn_
        );
    }
    for n in [
        "text-prose",
        "logs-text",
        "model-weights",
        "photo-raw-png",
        "audio",
        "software-installed",
    ] {
        assert_eq!(get(n).fp, 0, "{n}: false positives");
    }
}
