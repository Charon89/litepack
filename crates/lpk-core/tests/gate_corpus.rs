//! The gate against xz ground truth on the corpus, run by hand:
//! `LPK_CORPUS=<path to bench/corpus/small> cargo test -p lpk-core --release --test gate_corpus -- --ignored --nocapture`
//!
//! Method of the Phase 0 `entropy-gate` probe: blocks of 1 MiB per file (a final partial block
//! counts from 64 KiB), a block is incompressible when xz preset 9 keeps at least 95 percent of
//! it; positive = incompressible.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use lpk_core::{walk, Gate, IngestOptions, Source, GATE_BLOCK};
use lpk_format::EntryKind;

const MIN_TAIL: usize = 64 << 10;

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

#[test]
#[ignore]
fn gate_against_xz_on_the_corpus() {
    let root = PathBuf::from(std::env::var_os("LPK_CORPUS").unwrap());
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
    let mut per_class: BTreeMap<String, Counts> = BTreeMap::new();
    for dir in dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        // Read this class's blocks.
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        for i in walk(&dir, &IngestOptions::default())
            .unwrap()
            .iter()
            .filter(|i| i.kind == EntryKind::File)
        {
            let mut r = Source::new().open(i).unwrap();
            loop {
                let mut buf = Vec::new();
                let n = (&mut r)
                    .take(GATE_BLOCK as u64)
                    .read_to_end(&mut buf)
                    .unwrap();
                if n == 0 || (n < GATE_BLOCK && n < MIN_TAIL) {
                    break;
                }
                blocks.push(buf);
                if n < GATE_BLOCK {
                    break;
                }
            }
        }
        // Ground truth on several threads.
        let truth: Mutex<Vec<bool>> = Mutex::new(vec![false; blocks.len()]);
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    if k >= blocks.len() {
                        break;
                    }
                    let t = xz_keeps_95(&blocks[k]);
                    truth.lock().unwrap()[k] = t;
                });
            }
        });
        let truth = truth.into_inner().unwrap();
        // The gate, single-threaded.
        let c = per_class.entry(name).or_default();
        for (b, &t) in blocks.iter().zip(&truth) {
            c.add(t, Gate::DEFAULT.is_incompressible(b));
        }
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

    let get = |n: &str| per_class.get(n).copied();
    if let Some(c) = get("encrypted-random") {
        assert_eq!(c.fn_, 0, "encrypted-random recall must be 100%");
        assert!(c.tp > 0);
    }
    if let Some(c) = get("video") {
        assert!(c.tp + c.fn_ > 0);
        assert!(
            c.tp * 100 >= (c.tp + c.fn_) * 99,
            "video recall below 99%: {} of {}",
            c.tp,
            c.tp + c.fn_
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
        if let Some(c) = get(n) {
            assert_eq!(c.fp, 0, "{n}: false positives");
        }
    }
}
