//! `probe entropy-gate` (PLAN P0-4): how reliably a cheap test predicts that a 1 MiB block does
//! not compress, and how fast the corpus can be read from disk with the operating system's cache
//! bypassed (the denominator of the D-07 video gate).
//!
//! Per block of every file (a final partial block counts when it is at least 64 KiB): order-0
//! Shannon entropy of the whole block, the same on a sample (the first 4 KiB of every 64 KiB),
//! zstd level 1 output size, and xz preset 9 output size. The xz size is the ground truth: a block
//! is "incompressible" when xz keeps at least 95, 98 or 99 percent of it. A gate "says
//! incompressible" when the entropy is at or above its threshold, or when zstd level 1 keeps at
//! least its percentage. What is stored are counts (true/false positives and negatives, positive =
//! incompressible) per class, gate, threshold and ground-truth definition; precision and recall
//! appear only in the table. The cost of each gate is timed alone, single-threaded, per block
//! already in memory. The xz sizes are computed on several threads and are not timed.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::codec::{xz_size, XzSettings, ZstdContext, ZstdSettings};
use super::{mbps, md_header, md_table, par_map, pct, timed, Ctx, Envelope, Output};

pub const NAME: &str = "entropy-gate";

/// Block size of the evaluation.
pub const BLOCK: usize = 1 << 20;
/// A final partial block counts only from this size.
pub const MIN_TAIL: usize = 64 << 10;
/// The sample takes `SAMPLE` bytes at the start of every `SAMPLE_PERIOD` bytes.
pub const SAMPLE: usize = 4 << 10;
pub const SAMPLE_PERIOD: usize = 64 << 10;
/// Classes of the main table, in this order; every other class goes to the second table.
pub const MAIN_CLASSES: [&str; 2] = ["video", "encrypted-random"];
/// Entropy thresholds in bits per byte.
pub const ENTROPY_THRESHOLDS: [f64; 6] = [7.0, 7.5, 7.8, 7.9, 7.95, 7.99];
/// zstd level 1 output as a percentage of the input.
pub const ZSTD_PERCENTS: [u32; 4] = [90, 95, 98, 99];
/// Ground truth: xz output at least this percentage of the input.
pub const TRUTH_PERCENTS: [u32; 3] = [95, 98, 99];
/// Blocks held in memory at once (xz sizes of one batch run in parallel).
const BATCH: usize = 64;
/// Upper bound of the threads that run xz preset 9 at once (each holds a large encoder state).
const MAX_XZ_THREADS: u32 = 8;
/// Raw-read buffer and its alignment.
pub const READ_BUFFER: usize = 4 << 20;
pub const READ_ALIGN: usize = 4096;
const PASSES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    Entropy,
    SampledEntropy,
    Zstd1,
}

impl Gate {
    fn as_str(self) -> &'static str {
        match self {
            Gate::Entropy => "entropy",
            Gate::SampledEntropy => "sampled_entropy",
            Gate::Zstd1 => "zstd1",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Gate::Zstd1 => "% of input",
            _ => "bits/byte",
        }
    }
}

const GATES: [Gate; 3] = [Gate::Entropy, Gate::SampledEntropy, Gate::Zstd1];

/// Every (gate, threshold) in the order the counts are stored.
fn grid() -> Vec<(Gate, f64)> {
    let mut g = Vec::new();
    for t in ENTROPY_THRESHOLDS {
        g.push((Gate::Entropy, t));
    }
    for t in ENTROPY_THRESHOLDS {
        g.push((Gate::SampledEntropy, t));
    }
    for p in ZSTD_PERCENTS {
        g.push((Gate::Zstd1, f64::from(p)));
    }
    g
}

/// Counts of one gate at one threshold against one ground-truth definition. Positive means
/// "incompressible".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateCount {
    pub gate: String,
    /// Bits per byte for the entropy gates, percent of input for `zstd1`.
    pub threshold: f64,
    /// The ground truth: xz output at least this percentage of the input.
    pub truth_percent: u32,
    pub true_pos: u64,
    pub false_pos: u64,
    pub true_neg: u64,
    pub false_neg: u64,
}

/// One class (or "not in this corpus").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRec {
    pub class: String,
    /// `main` (video, encrypted-random) or `other`.
    pub role: String,
    pub present: bool,
    pub files: u64,
    /// Bytes of all files of the class.
    pub bytes: u64,
    /// Blocks of exactly 1 MiB.
    pub full_blocks: u64,
    /// Final partial blocks of at least 64 KiB that were evaluated.
    pub tail_blocks: u64,
    /// Bytes in those partial blocks.
    pub tail_kept_bytes: u64,
    /// Final partial blocks under 64 KiB that were left out, and their bytes.
    pub dropped_tails: u64,
    pub dropped_bytes: u64,
    /// Bytes of all evaluated blocks.
    pub block_input_bytes: u64,
    /// Sum over the evaluated blocks of the zstd level 1 / xz preset 9 output.
    pub zstd1_bytes: u64,
    pub xz_bytes: u64,
    /// Blocks that are incompressible by the ground truth, one entry per `truth_percents`.
    pub truth_positive: Vec<u64>,
    /// One entry per gate, threshold and truth percentage, in the order of the grid.
    pub gates: Vec<GateCount>,
}

impl ClassRec {
    fn new(class: &str, present: bool) -> ClassRec {
        let mut gates = Vec::new();
        if present {
            for (g, t) in grid() {
                for truth in TRUTH_PERCENTS {
                    gates.push(GateCount {
                        gate: g.as_str().to_string(),
                        threshold: t,
                        truth_percent: truth,
                        true_pos: 0,
                        false_pos: 0,
                        true_neg: 0,
                        false_neg: 0,
                    });
                }
            }
        }
        ClassRec {
            class: class.to_string(),
            role: if MAIN_CLASSES.contains(&class) {
                "main"
            } else {
                "other"
            }
            .to_string(),
            present,
            files: 0,
            bytes: 0,
            full_blocks: 0,
            tail_blocks: 0,
            tail_kept_bytes: 0,
            dropped_tails: 0,
            dropped_bytes: 0,
            block_input_bytes: 0,
            zstd1_bytes: 0,
            xz_bytes: 0,
            truth_positive: if present {
                vec![0; TRUTH_PERCENTS.len()]
            } else {
                Vec::new()
            },
            gates,
        }
    }

    pub fn blocks(&self) -> u64 {
        self.full_blocks + self.tail_blocks
    }

    fn add_block(&mut self, b: &BlockRec) {
        self.block_input_bytes += b.len;
        self.zstd1_bytes += b.zstd;
        self.xz_bytes += b.xz;
        for (ti, truth) in TRUTH_PERCENTS.iter().enumerate() {
            if b.xz * 100 >= u64::from(*truth) * b.len {
                self.truth_positive[ti] += 1;
            }
        }
        for (gi, (gate, thr)) in grid().into_iter().enumerate() {
            let says = says_incompressible(gate, thr, b);
            for (ti, truth) in TRUTH_PERCENTS.iter().enumerate() {
                let truth_pos = b.xz * 100 >= u64::from(*truth) * b.len;
                let c = &mut self.gates[gi * TRUTH_PERCENTS.len() + ti];
                match (says, truth_pos) {
                    (true, true) => c.true_pos += 1,
                    (true, false) => c.false_pos += 1,
                    (false, false) => c.true_neg += 1,
                    (false, true) => c.false_neg += 1,
                }
            }
        }
    }
}

/// What the probe knows of one evaluated block.
#[derive(Debug, Clone, Copy)]
struct BlockRec {
    len: u64,
    entropy: f64,
    sampled: f64,
    zstd: u64,
    xz: u64,
}

fn says_incompressible(gate: Gate, threshold: f64, b: &BlockRec) -> bool {
    match gate {
        Gate::Entropy => b.entropy >= threshold,
        Gate::SampledEntropy => b.sampled >= threshold,
        // Thresholds of this gate are whole percentages.
        Gate::Zstd1 => b.zstd * 100 >= (threshold as u64) * b.len,
    }
}

/// Cost of one gate over all evaluated blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateCost {
    pub gate: String,
    pub blocks: u64,
    /// Bytes of the blocks the gate was run on (the sampled gate reads a part of each).
    pub bytes: u64,
    pub seconds: f64,
}

/// One class's share of a raw-read pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawClass {
    pub class: String,
    pub bytes: u64,
    pub seconds: f64,
}

/// One sequential pass over every corpus file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPass {
    pub files: u64,
    pub bytes: u64,
    /// Sum of the per-class seconds (file opens included).
    pub seconds: f64,
    pub classes: Vec<RawClass>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRead {
    /// `unbuffered` (operating-system cache bypassed) or `buffered`.
    pub mode: String,
    /// The cache bypass was in effect; a `buffered` pass may be served from the cache.
    pub cache_bypassed: bool,
    pub buffer_bytes: u64,
    pub alignment: u64,
    /// Why unbuffered reading was not used, when it was not.
    pub fallback_note: Option<String>,
    pub passes: Vec<RawPass>,
}

/// The probe's own part of the result file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub block_bytes: u64,
    pub min_tail_bytes: u64,
    pub sample_bytes: u64,
    pub sample_period_bytes: u64,
    pub entropy_thresholds: Vec<f64>,
    pub zstd_percents: Vec<u32>,
    pub truth_percents: Vec<u32>,
    pub zstd: ZstdSettings,
    pub xz: XzSettings,
    pub classes: Vec<ClassRec>,
    pub gate_cost: Vec<GateCost>,
    pub raw_read: RawRead,
}

// ---------------------------------------------------------------------------------------------
// The gates

/// Order-0 Shannon entropy in bits per byte; empty input gives 0.
pub fn entropy_bits(data: &[u8]) -> f64 {
    let mut h = [[0u32; 256]; 4];
    let (quads, rest) = data.as_chunks::<4>();
    for c in quads {
        h[0][usize::from(c[0])] += 1;
        h[1][usize::from(c[1])] += 1;
        h[2][usize::from(c[2])] += 1;
        h[3][usize::from(c[3])] += 1;
    }
    for &b in rest {
        h[0][usize::from(b)] += 1;
    }
    let mut counts = [0u64; 256];
    for (i, c) in counts.iter_mut().enumerate() {
        *c = u64::from(h[0][i]) + u64::from(h[1][i]) + u64::from(h[2][i]) + u64::from(h[3][i]);
    }
    entropy_from_counts(&counts, data.len() as u64)
}

fn entropy_from_counts(counts: &[u64; 256], n: u64) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let n = n as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Entropy of the first `SAMPLE` bytes of every `SAMPLE_PERIOD` bytes of `data`.
pub fn sampled_entropy_bits(data: &[u8]) -> f64 {
    let mut counts = [0u64; 256];
    let mut n = 0u64;
    for period in data.chunks(SAMPLE_PERIOD) {
        let take = &period[..period.len().min(SAMPLE)];
        for &b in take {
            counts[usize::from(b)] += 1;
        }
        n += take.len() as u64;
    }
    entropy_from_counts(&counts, n)
}

// ---------------------------------------------------------------------------------------------
// Evaluation

struct Runner {
    zstd: ZstdContext,
    xz: XzSettings,
    xz_threads: usize,
    batch: Vec<Vec<u8>>,
    cost: [GateCost; 3],
}

impl Runner {
    fn new(threads: u32) -> Result<Runner> {
        let cost = GATES.map(|g| GateCost {
            gate: g.as_str().to_string(),
            blocks: 0,
            bytes: 0,
            seconds: 0.0,
        });
        Ok(Runner {
            zstd: ZstdSettings::level(1).context()?,
            xz: XzSettings::preset9(),
            xz_threads: threads.clamp(1, MAX_XZ_THREADS) as usize,
            batch: Vec::new(),
            cost,
        })
    }

    /// One block of a file of `rec`'s class.
    fn push(&mut self, rec: &mut ClassRec, block: &[u8]) -> Result<()> {
        if block.len() < MIN_TAIL {
            // Only the last block of a file can be this short.
            rec.dropped_tails += 1;
            rec.dropped_bytes += block.len() as u64;
            return Ok(());
        }
        if block.len() < BLOCK {
            rec.tail_blocks += 1;
            rec.tail_kept_bytes += block.len() as u64;
        } else {
            rec.full_blocks += 1;
        }
        self.batch.push(block.to_vec());
        if self.batch.len() >= BATCH {
            self.flush(rec)?;
        }
        Ok(())
    }

    /// Evaluate the blocks held: xz sizes in parallel (untimed), then each gate alone and timed.
    fn flush(&mut self, rec: &mut ClassRec) -> Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let xz = self.xz.clone();
        let sizes = par_map(&self.batch, self.xz_threads, |_, b| xz_size(b, &xz));
        for (b, x) in self.batch.drain(..).zip(sizes) {
            let xz_bytes = x?;
            let (entropy, t0) = timed(|| entropy_bits(&b));
            let (sampled, t1) = timed(|| sampled_entropy_bits(&b));
            let (z, t2) = timed(|| self.zstd.compress(&b));
            let zstd = z?.len() as u64;
            for (c, t) in self.cost.iter_mut().zip([t0, t1, t2]) {
                c.blocks += 1;
                c.bytes += b.len() as u64;
                c.seconds += t;
            }
            rec.add_block(&BlockRec {
                len: b.len() as u64,
                entropy,
                sampled,
                zstd,
                xz: xz_bytes,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Raw read speed

/// `FILE_FLAG_NO_BUFFERING` and `FILE_FLAG_SEQUENTIAL_SCAN` of the Windows API.
#[cfg(windows)]
const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
#[cfg(windows)]
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;

/// Open `path` for reading with the operating system's cache bypassed.
#[cfg(windows)]
fn open_unbuffered(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_unbuffered(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(path)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
fn open_unbuffered(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no cache-bypass flag is known for this platform",
    ))
}

/// A buffer whose start is aligned: an aligned sub-slice of a larger `Vec`.
struct AlignedBuf {
    v: Vec<u8>,
    off: usize,
    len: usize,
}

impl AlignedBuf {
    fn new(len: usize, align: usize) -> AlignedBuf {
        let v = vec![0u8; len + align];
        let off = v.as_ptr().align_offset(align);
        // `align_offset` may give up with usize::MAX; the buffer is then simply unaligned and
        // the unbuffered read will be refused and reported.
        let off = if off <= align { off } else { 0 };
        AlignedBuf { v, off, len }
    }

    fn slice(&mut self) -> &mut [u8] {
        &mut self.v[self.off..self.off + self.len]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadMode {
    Unbuffered,
    Buffered,
}

/// Read a whole file sequentially into `buf`, discarding the data; returns the bytes read.
fn read_through(path: &Path, mode: ReadMode, buf: &mut [u8]) -> std::io::Result<u64> {
    let mut f = match mode {
        ReadMode::Unbuffered => open_unbuffered(path)?,
        ReadMode::Buffered => File::open(path)?,
    };
    let mut total = 0u64;
    loop {
        match f.read(buf) {
            Ok(0) => return Ok(total),
            Ok(n) => total += n as u64,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

fn raw_read(ctx: &Ctx<'_>) -> Result<RawRead> {
    raw_read_with(ctx, None)
}

/// The three passes. `refused` forces the buffered fallback with that reason (tests); otherwise
/// the first non-empty file decides whether the file system accepts unbuffered reads.
fn raw_read_with(ctx: &Ctx<'_>, refused: Option<String>) -> Result<RawRead> {
    let mut buf = AlignedBuf::new(READ_BUFFER, READ_ALIGN);
    let classes: Vec<(&String, &crate::corpus::manifest::ClassEntry)> =
        ctx.corpus.manifest.classes.iter().collect();
    let mut mode = ReadMode::Unbuffered;
    let mut note = refused;
    if note.is_none() {
        let first = classes
            .iter()
            .flat_map(|(_, c)| c.files.iter())
            .find(|f| f.bytes > 0);
        if let Some(f) = first {
            let p = ctx.corpus.files_root().join(&f.path);
            match read_through(&p, ReadMode::Unbuffered, buf.slice()) {
                Ok(n) if n == f.bytes => {}
                Ok(n) => bail!(
                    "unbuffered read of a corpus file returned {n} bytes, the manifest says {}",
                    f.bytes
                ),
                Err(e) => note = Some(format!("unbuffered read refused: {e}")),
            }
        }
    }
    if note.is_some() {
        mode = ReadMode::Buffered;
    }
    let mut passes = Vec::new();
    for _ in 0..PASSES {
        let mut pass = RawPass {
            files: 0,
            bytes: 0,
            seconds: 0.0,
            classes: Vec::new(),
        };
        for (class, entry) in &classes {
            let mut rc = RawClass {
                class: (*class).clone(),
                bytes: 0,
                seconds: 0.0,
            };
            for f in &entry.files {
                let p = ctx.corpus.files_root().join(&f.path);
                let start = Instant::now();
                let n = read_through(&p, mode, buf.slice());
                let secs = start.elapsed().as_secs_f64();
                let n = match n {
                    Ok(n) => n,
                    Err(e) => bail!(
                        "class `{class}`: raw read of `{}` failed: {e}",
                        ctx.label(f).unwrap_or_else(|| "(private file)".into())
                    ),
                };
                if n != f.bytes {
                    bail!(
                        "class `{class}`: raw read of `{}` returned {n} bytes, the manifest says {}",
                        ctx.label(f).unwrap_or_else(|| "(private file)".into()),
                        f.bytes
                    );
                }
                pass.files += 1;
                rc.bytes += n;
                rc.seconds += secs;
            }
            pass.bytes += rc.bytes;
            pass.seconds += rc.seconds;
            pass.classes.push(rc);
        }
        passes.push(pass);
    }
    Ok(RawRead {
        mode: match mode {
            ReadMode::Unbuffered => "unbuffered",
            ReadMode::Buffered => "buffered",
        }
        .to_string(),
        cache_bypassed: mode == ReadMode::Unbuffered,
        buffer_bytes: READ_BUFFER as u64,
        alignment: READ_ALIGN as u64,
        fallback_note: note,
        passes,
    })
}

// ---------------------------------------------------------------------------------------------
// The probe

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let mut order: Vec<String> = MAIN_CLASSES.iter().map(|s| s.to_string()).collect();
    for c in ctx.corpus.manifest.classes.keys() {
        if !order.contains(c) {
            order.push(c.clone());
        }
    }
    let mut runner = Runner::new(ctx.threads)?;
    let mut classes = Vec::new();
    for class in &order {
        let files = ctx.class_files(class);
        let mut rec = ClassRec::new(class, files.is_some());
        for f in files.unwrap_or(&[]) {
            rec.files += 1;
            rec.bytes += f.bytes;
            ctx.read_blocks(class, f, BLOCK, |b| runner.push(&mut rec, b))?;
            runner.flush(&mut rec)?;
        }
        classes.push(rec);
    }
    let raw = raw_read(ctx)?;
    let mut out_notes = Vec::new();
    for c in classes.iter().filter(|c| !c.present) {
        out_notes.push(format!("class `{}` is not in this corpus", c.class));
    }
    if let Some(n) = &raw.fallback_note {
        out_notes.push(format!(
            "raw read: {n}; the passes are buffered and may be served from the cache"
        ));
    }
    if ctx.threads > MAX_XZ_THREADS {
        out_notes.push(format!(
            "xz preset 9 sizes ran on at most {MAX_XZ_THREADS} threads (encoder memory)"
        ));
    }
    let data = Data {
        block_bytes: BLOCK as u64,
        min_tail_bytes: MIN_TAIL as u64,
        sample_bytes: SAMPLE as u64,
        sample_period_bytes: SAMPLE_PERIOD as u64,
        entropy_thresholds: ENTROPY_THRESHOLDS.to_vec(),
        zstd_percents: ZSTD_PERCENTS.to_vec(),
        truth_percents: TRUTH_PERCENTS.to_vec(),
        zstd: ZstdSettings::level(1),
        xz: XzSettings::preset9(),
        classes,
        gate_cost: runner.cost.to_vec(),
        raw_read: raw,
    };
    let mut out = Output::new(data, 1).with_zstd().with_xz();
    out.notes = out_notes;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// The table

fn precision_recall(c: &GateCount) -> (String, String) {
    (
        pct(c.true_pos, c.true_pos + c.false_pos),
        pct(c.true_pos, c.true_pos + c.false_neg),
    )
}

fn threshold_label(g: &str, t: f64) -> String {
    if g == "zstd1" {
        format!("{t}%")
    } else {
        format!("{t}")
    }
}

/// Sum the counts of several classes (all with the full grid).
fn pooled(classes: &[&ClassRec]) -> Vec<GateCount> {
    let mut out: Vec<GateCount> = Vec::new();
    for c in classes.iter().filter(|c| c.present) {
        if out.is_empty() {
            out = c.gates.clone();
            continue;
        }
        for (o, g) in out.iter_mut().zip(&c.gates) {
            o.true_pos += g.true_pos;
            o.false_pos += g.false_pos;
            o.true_neg += g.true_neg;
            o.false_neg += g.false_neg;
        }
    }
    out
}

/// Rows: gate and threshold; columns: precision and recall for each truth percentage.
fn pr_table(gates: &[GateCount]) -> String {
    let mut headers = vec!["gate".to_string(), "threshold".to_string()];
    for t in TRUTH_PERCENTS {
        headers.push(format!("precision (xz>={t}%)"));
        headers.push(format!("recall (xz>={t}%)"));
    }
    let h: Vec<&str> = headers.iter().map(String::as_str).collect();
    let per = TRUTH_PERCENTS.len();
    let mut rows = Vec::new();
    for chunk in gates.chunks(per) {
        let Some(first) = chunk.first() else { continue };
        let mut row = vec![
            first.gate.clone(),
            threshold_label(&first.gate, first.threshold),
        ];
        for c in chunk {
            let (p, r) = precision_recall(c);
            row.push(p);
            row.push(r);
        }
        rows.push(row);
    }
    md_table(&h, &rows)
}

fn truth_line(blocks: u64, truth: &[u64]) -> String {
    let parts: Vec<String> = TRUTH_PERCENTS
        .iter()
        .zip(truth)
        .map(|(t, n)| format!("xz>={t}%: {n}"))
        .collect();
    format!(
        "{blocks} blocks; incompressible blocks by ground truth ({})",
        parts.join(", ")
    )
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    s.push_str(
        "Blocks of 1 MiB (a final partial block counts from 64 KiB). Positive means \
         incompressible: a gate predicts it when the entropy is at or above the threshold \
         (bits/byte) or when zstd level 1 keeps at least the percentage of the block; the ground \
         truth is xz preset 9 keeping at least the percentage given in the column. Precision = \
         TP/(TP+FP), recall = TP/(TP+FN).\n\n",
    );
    s.push_str("## Main table: video and encrypted-random\n\n");
    let main: Vec<&ClassRec> = d.classes.iter().filter(|c| c.role == "main").collect();
    for c in &main {
        s.push_str(&format!("### `{}`\n\n", c.class));
        if !c.present {
            s.push_str("class not in this corpus\n\n");
        } else if c.blocks() == 0 {
            s.push_str("no block of at least 64 KiB\n\n");
        } else {
            s.push_str(&format!(
                "{}\n\n",
                truth_line(c.blocks(), &c.truth_positive)
            ));
            s.push_str(&pr_table(&c.gates));
            s.push('\n');
        }
    }
    let live: Vec<&ClassRec> = main.iter().copied().filter(|c| c.blocks() > 0).collect();
    if live.len() > 1 {
        let blocks: u64 = live.iter().map(|c| c.blocks()).sum();
        let truth: Vec<u64> = (0..TRUTH_PERCENTS.len())
            .map(|i| live.iter().map(|c| c.truth_positive[i]).sum())
            .collect();
        s.push_str("### both classes pooled\n\n");
        s.push_str(&format!("{}\n\n", truth_line(blocks, &truth)));
        s.push_str(&pr_table(&pooled(&live)));
        s.push('\n');
    }
    s.push_str("## All other classes (false-positive check)\n\n");
    let other: Vec<&ClassRec> = d
        .classes
        .iter()
        .filter(|c| c.role != "main" && c.present && c.blocks() > 0)
        .collect();
    if other.is_empty() {
        s.push_str("no other class with a block of at least 64 KiB\n\n");
    } else {
        let blocks: u64 = other.iter().map(|c| c.blocks()).sum();
        let truth: Vec<u64> = (0..TRUTH_PERCENTS.len())
            .map(|i| other.iter().map(|c| c.truth_positive[i]).sum())
            .collect();
        s.push_str(&format!("### pooled\n\n{}\n\n", truth_line(blocks, &truth)));
        s.push_str(&pr_table(&pooled(&other)));
        s.push_str(
            "\n### Per class: blocks a gate calls incompressible although xz>=95% says they are \
             not (false positives)\n\n",
        );
        let probe_gates = [
            ("entropy", 7.95),
            ("sampled_entropy", 7.95),
            ("zstd1", 98.0),
        ];
        let mut headers = vec!["class".to_string(), "blocks".to_string()];
        headers.push("xz>=95% blocks".to_string());
        for (g, t) in probe_gates {
            headers.push(format!("FP {g} {}", threshold_label(g, t)));
        }
        let h: Vec<&str> = headers.iter().map(String::as_str).collect();
        let rows: Vec<Vec<String>> = other
            .iter()
            .map(|c| {
                let mut row = vec![
                    c.class.clone(),
                    c.blocks().to_string(),
                    c.truth_positive.first().copied().unwrap_or(0).to_string(),
                ];
                for (g, t) in probe_gates {
                    let fp = c
                        .gates
                        .iter()
                        .find(|x| x.gate == g && x.threshold == t && x.truth_percent == 95)
                        .map_or(0, |x| x.false_pos);
                    row.push(fp.to_string());
                }
                row
            })
            .collect();
        s.push_str(&md_table(&h, &rows));
        s.push('\n');
    }
    s.push_str("## Gate cost (single thread, blocks in memory)\n\n");
    let rows: Vec<Vec<String>> = d
        .gate_cost
        .iter()
        .map(|c| {
            vec![
                c.gate.clone(),
                c.blocks.to_string(),
                c.bytes.to_string(),
                format!("{:.3}", c.seconds),
                mbps(c.bytes, c.seconds),
            ]
        })
        .collect();
    s.push_str(&md_table(
        &["gate", "blocks", "bytes", "seconds", "MB/s of block bytes"],
        &rows,
    ));
    s.push_str("\n## Raw read speed (sequential, whole corpus)\n\n");
    let r = &d.raw_read;
    s.push_str(&format!(
        "mode: {}, cache bypassed: {}, buffer {} bytes aligned to {}{}\n\n",
        r.mode,
        r.cache_bypassed,
        r.buffer_bytes,
        r.alignment,
        r.fallback_note
            .as_ref()
            .map(|n| format!(", note: {n}"))
            .unwrap_or_default()
    ));
    let rows: Vec<Vec<String>> = r
        .passes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let video = p.classes.iter().find(|c| c.class == "video");
            vec![
                (i + 1).to_string(),
                p.files.to_string(),
                p.bytes.to_string(),
                format!("{:.3}", p.seconds),
                mbps(p.bytes, p.seconds),
                video.map_or("n/a".to_string(), |v| {
                    format!(
                        "{} bytes in {:.3} s = {} MB/s",
                        v.bytes,
                        v.seconds,
                        mbps(v.bytes, v.seconds)
                    )
                }),
            ]
        })
        .collect();
    s.push_str(&md_table(
        &["pass", "files", "bytes", "seconds", "MB/s", "video class"],
        &rows,
    ));
    s
}

// ---------------------------------------------------------------------------------------------
// Consistency rules

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    let mut bad = |at: String, msg: String| p.push(format!("{at}: {msg}"));
    for (what, ok) in [
        ("block_bytes", d.block_bytes == BLOCK as u64),
        ("min_tail_bytes", d.min_tail_bytes == MIN_TAIL as u64),
        ("sample_bytes", d.sample_bytes == SAMPLE as u64),
        (
            "sample_period_bytes",
            d.sample_period_bytes == SAMPLE_PERIOD as u64,
        ),
        (
            "entropy_thresholds",
            d.entropy_thresholds == ENTROPY_THRESHOLDS,
        ),
        ("zstd_percents", d.zstd_percents == ZSTD_PERCENTS),
        ("truth_percents", d.truth_percents == TRUTH_PERCENTS),
        ("zstd", d.zstd == ZstdSettings::level(1)),
        ("xz", d.xz == XzSettings::preset9()),
    ] {
        if !ok {
            bad(
                format!("/data/{what}"),
                "differs from this probe's definition".into(),
            );
        }
    }
    for m in MAIN_CLASSES {
        if !d.classes.iter().any(|c| c.class == m) {
            bad(
                "/data/classes".into(),
                format!("main class `{m}` has no entry"),
            );
        }
    }
    let expect = grid();
    let mut total_blocks = 0u64;
    let mut total_block_bytes = 0u64;
    let mut class_bytes = 0u64;
    for (i, c) in d.classes.iter().enumerate() {
        let at = |f: &str| format!("/data/classes/{i}/{f}");
        let role = if MAIN_CLASSES.contains(&c.class.as_str()) {
            "main"
        } else {
            "other"
        };
        if c.role != role {
            bad(at("role"), format!("`{}` for class `{}`", c.role, c.class));
        }
        if !c.present {
            if c.files != 0 || c.bytes != 0 || c.blocks() != 0 || !c.gates.is_empty() {
                bad(at("present"), "an absent class carries data".into());
            }
            continue;
        }
        class_bytes += c.bytes;
        let mib = BLOCK as u64;
        if c.bytes != c.full_blocks * mib + c.tail_kept_bytes + c.dropped_bytes {
            bad(
                at("bytes"),
                "file bytes differ from full blocks, kept tails and dropped tails".into(),
            );
        }
        if c.block_input_bytes != c.full_blocks * mib + c.tail_kept_bytes {
            bad(
                at("block_input_bytes"),
                "differs from the bytes of the evaluated blocks".into(),
            );
        }
        let tail_ok = if c.tail_blocks == 0 {
            c.tail_kept_bytes == 0
        } else {
            c.tail_kept_bytes >= c.tail_blocks * MIN_TAIL as u64
                && c.tail_kept_bytes < c.tail_blocks * mib
        };
        if !tail_ok {
            bad(
                at("tail_kept_bytes"),
                "not between 64 KiB and 1 MiB per kept tail".into(),
            );
        }
        let dropped_ok = if c.dropped_tails == 0 {
            c.dropped_bytes == 0
        } else {
            c.dropped_bytes < c.dropped_tails * MIN_TAIL as u64
        };
        if !dropped_ok {
            bad(at("dropped_bytes"), "a dropped tail is under 64 KiB".into());
        }
        if c.tail_blocks + c.dropped_tails > c.files {
            bad(at("files"), "more final partial blocks than files".into());
        }
        if c.files == 0 && c.bytes != 0 {
            bad(at("bytes"), "bytes without files".into());
        }
        if c.blocks() == 0 && (c.xz_bytes != 0 || c.zstd1_bytes != 0) {
            bad(at("xz_bytes"), "compressed bytes without blocks".into());
        }
        if c.blocks() > 0 && (c.xz_bytes == 0 || c.zstd1_bytes == 0) {
            bad(at("xz_bytes"), "blocks without compressed bytes".into());
        }
        total_blocks += c.blocks();
        total_block_bytes += c.block_input_bytes;
        if c.truth_positive.len() != TRUTH_PERCENTS.len() {
            bad(
                at("truth_positive"),
                "one entry per ground-truth percentage".into(),
            );
            continue;
        }
        for (ti, n) in c.truth_positive.iter().enumerate() {
            if *n > c.blocks() {
                bad(
                    at(&format!("truth_positive/{ti}")),
                    "more than the blocks".into(),
                );
            }
            if ti > 0 && *n > c.truth_positive[ti - 1] {
                bad(
                    at(&format!("truth_positive/{ti}")),
                    "a stricter truth has more positives".into(),
                );
            }
        }
        if c.gates.len() != expect.len() * TRUTH_PERCENTS.len() {
            bad(
                at("gates"),
                format!(
                    "{} rows, expected {}",
                    c.gates.len(),
                    expect.len() * TRUTH_PERCENTS.len()
                ),
            );
            continue;
        }
        for (gi, (g, t)) in expect.iter().enumerate() {
            for (ti, truth) in TRUTH_PERCENTS.iter().enumerate() {
                let k = gi * TRUTH_PERCENTS.len() + ti;
                let r = &c.gates[k];
                let here = at(&format!("gates/{k}"));
                if r.gate != g.as_str() || r.threshold != *t || r.truth_percent != *truth {
                    bad(here, "not the expected gate, threshold and truth".into());
                    continue;
                }
                if r.true_pos + r.false_pos + r.true_neg + r.false_neg != c.blocks() {
                    bad(here.clone(), "counts do not add up to the blocks".into());
                }
                if r.true_pos + r.false_neg != c.truth_positive[ti] {
                    bad(here, "true positives plus false negatives differ from the ground-truth positives".into());
                }
            }
        }
    }
    if d.gate_cost.len() != GATES.len() {
        bad("/data/gate_cost".into(), "one entry per gate".into());
    }
    for (i, (c, g)) in d.gate_cost.iter().zip(GATES).enumerate() {
        let at = format!("/data/gate_cost/{i}");
        if c.gate != g.as_str() {
            bad(at.clone(), format!("expected gate `{}`", g.as_str()));
        }
        if c.blocks != total_blocks || c.bytes != total_block_bytes {
            bad(
                at.clone(),
                "does not cover exactly the evaluated blocks".into(),
            );
        }
        if !(c.seconds.is_finite() && c.seconds >= 0.0) {
            bad(
                format!("{at}/seconds"),
                "must be a non-negative number".into(),
            );
        }
    }
    let r = &d.raw_read;
    if r.passes.len() != PASSES {
        bad(
            "/data/raw_read/passes".into(),
            format!("expected {PASSES} passes"),
        );
    }
    match (r.mode.as_str(), r.cache_bypassed, &r.fallback_note) {
        ("unbuffered", true, None) | ("buffered", false, Some(_)) => {}
        _ => bad(
            "/data/raw_read/mode".into(),
            "mode, cache_bypassed and fallback_note disagree".into(),
        ),
    }
    if r.buffer_bytes != READ_BUFFER as u64 || r.alignment != READ_ALIGN as u64 {
        bad(
            "/data/raw_read/buffer_bytes".into(),
            "differs from the probe's buffer".into(),
        );
    }
    for (i, ps) in r.passes.iter().enumerate() {
        let at = format!("/data/raw_read/passes/{i}");
        if ps.bytes != class_bytes {
            bad(
                format!("{at}/bytes"),
                "differs from the bytes of the corpus classes".into(),
            );
        }
        let sum: u64 = ps.classes.iter().map(|c| c.bytes).sum();
        let secs: f64 = ps.classes.iter().map(|c| c.seconds).sum();
        if sum != ps.bytes || (secs - ps.seconds).abs() > 1e-6 * (1.0 + ps.seconds) {
            bad(format!("{at}/classes"), "do not add up to the pass".into());
        }
        if !(ps.seconds.is_finite() && ps.seconds >= 0.0) {
            bad(
                format!("{at}/seconds"),
                "must be a non-negative number".into(),
            );
        }
        let present: Vec<(&str, u64)> = d
            .classes
            .iter()
            .filter(|c| c.present)
            .map(|c| (c.class.as_str(), c.bytes))
            .collect();
        for (cl, (name, bytes)) in ps.classes.iter().map(|c| (c, (c.class.as_str(), c.bytes))) {
            if !present.contains(&(name, bytes)) {
                bad(
                    format!("{at}/classes"),
                    format!("class `{}` differs from the class table", cl.class),
                );
            }
        }
        let files: u64 = d.classes.iter().map(|c| c.files).sum();
        if ps.files != files {
            bad(
                format!("{at}/files"),
                "differs from the files of the classes".into(),
            );
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::corpus::manifest::{Manifest, ManifestFile};
    use crate::run::exec::load_corpus;

    fn mf(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile {
            blake3: blake3::hash(data).to_hex().to_string(),
            bytes: data.len() as u64,
            licence: "CC0-1.0".to_string(),
            path: path.to_string(),
            source: "test".to_string(),
        }
    }

    fn random(n: usize, mut x: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    /// video: random, one full block and a 70 KiB tail; encrypted-random: one 70 KiB block;
    /// text-prose: one 200 KiB text block; small-files: zeros of 100 KiB and a 10 KiB file that
    /// is dropped.
    fn corpus(tmp: &Path) -> PathBuf {
        let dir = tmp.join("gatecorpus");
        let text: Vec<u8> = (0..200 * 1024 / 40 + 1)
            .flat_map(|i| format!("line {i} of the quick brown fox..\n").into_bytes())
            .take(200 * 1024)
            .collect();
        let items: Vec<(&str, &str, Vec<u8>)> = vec![
            ("video", "a.bin", random(BLOCK + 70 * 1024, 1)),
            ("encrypted-random", "k.bin", random(70 * 1024, 2)),
            ("text-prose", "t.txt", text),
            ("small-files", "z.bin", vec![0u8; 100 * 1024]),
            ("small-files", "tiny.bin", random(10 * 1024, 3)),
        ];
        let mut files = Vec::new();
        for (class, name, data) in items {
            let rel = format!("{class}/{name}");
            let p = dir.join(&rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(&p, &data).expect("write");
            files.push((class.to_string(), mf(&rel, &data)));
        }
        let m = Manifest::with_profile_name("small", files);
        std::fs::write(dir.join("manifest.json"), m.render()).expect("manifest");
        dir
    }

    fn with_run(f: impl FnOnce(&Ctx<'_>, Output<Data>)) {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = corpus(tmp.path());
        let corpus = load_corpus(&dir).expect("corpus");
        let ctx = Ctx {
            corpus: &corpus,
            threads: 2,
            scratch: tmp.path().to_path_buf(),
            local_tools: PathBuf::from("none.toml"),
            tool_timeout: Duration::from_secs(60),
        };
        let out = run(&ctx).expect("run");
        f(&ctx, out);
    }

    fn envelope(data: Data) -> Envelope<Data> {
        let out = Output::new(data, 1).with_zstd().with_xz();
        Envelope {
            probe: NAME.to_string(),
            format_version: super::super::FORMAT_VERSION,
            corpus: super::super::CorpusId {
                profile: "small".into(),
                manifest_blake3: "0".repeat(64),
                private: false,
            },
            build: "test".into(),
            build_profile: super::super::BuildProfile::current(true),
            host: "h".into(),
            date: "2026-01-01T00:00:00Z".into(),
            threads: 1,
            library_threads: out.library_threads,
            libraries: out.libraries,
            elapsed_seconds: 0.0,
            notes: out.notes,
            data: out.data,
        }
    }

    fn row<'a>(c: &'a ClassRec, gate: &str, t: f64, truth: u32) -> &'a GateCount {
        c.gates
            .iter()
            .find(|g| g.gate == gate && g.threshold == t && g.truth_percent == truth)
            .expect("row")
    }

    #[test]
    fn entropy_of_known_inputs() {
        assert_eq!(entropy_bits(&[]), 0.0);
        assert_eq!(entropy_bits(&vec![7u8; 1000]), 0.0);
        let all: Vec<u8> = (0..=255u8).cycle().take(256 * 10 + 3).collect();
        assert!((entropy_bits(&all[..2560]) - 8.0).abs() < 1e-12);
        let two: Vec<u8> = (0..1000).map(|i| (i % 2) as u8).collect();
        assert!((entropy_bits(&two) - 1.0).abs() < 1e-12);
        assert!(entropy_bits(&random(1 << 20, 9)) > 7.999);
    }

    #[test]
    fn the_sample_is_the_start_of_every_period() {
        // Random in the sampled parts, zeros elsewhere: the sample sees only the random parts.
        let mut data = vec![0u8; 3 * SAMPLE_PERIOD];
        for k in 0..3 {
            let r = random(SAMPLE, 40 + k as u64);
            data[k * SAMPLE_PERIOD..k * SAMPLE_PERIOD + SAMPLE].copy_from_slice(&r);
        }
        assert!(sampled_entropy_bits(&data) > 7.9);
        assert!(entropy_bits(&data) < 3.0);
        // A short last period contributes what it has.
        assert_eq!(sampled_entropy_bits(&vec![5u8; SAMPLE_PERIOD + 10]), 0.0);
    }

    #[test]
    fn synthetic_classes_are_counted_and_evaluated() {
        with_run(|_, out| {
            let d = &out.data;
            let by = |n: &str| d.classes.iter().find(|c| c.class == n).expect("class");
            let video = by("video");
            assert_eq!(
                (video.full_blocks, video.tail_blocks, video.files),
                (1, 1, 1)
            );
            assert_eq!(video.tail_kept_bytes, 70 * 1024);
            let small = by("small-files");
            assert_eq!(
                (small.tail_blocks, small.dropped_tails, small.dropped_bytes),
                (1, 1, 10 * 1024)
            );
            assert_eq!(small.files, 2);
            // Random data: every gate at every threshold up to 7.9 calls it incompressible, and
            // the ground truth agrees.
            assert_eq!(video.truth_positive[0], 2);
            for t in [7.0, 7.5, 7.8, 7.9] {
                let r = row(video, "entropy", t, 95);
                assert_eq!((r.true_pos, r.false_pos, r.false_neg), (2, 0, 0));
            }
            let enc = by("encrypted-random");
            let r = row(enc, "zstd1", 98.0, 95);
            assert_eq!((r.true_pos, r.false_pos), (1, 0));
            // Text and zeros compress: no gate fires at a high threshold, nothing is positive.
            for n in ["text-prose", "small-files"] {
                let c = by(n);
                assert_eq!(c.truth_positive, vec![0, 0, 0]);
                let r = row(c, "entropy", 7.0, 95);
                assert_eq!((r.true_pos, r.false_pos, r.true_neg), (0, 0, c.blocks()));
                let r = row(c, "zstd1", 90.0, 95);
                assert_eq!(r.false_pos, 0);
                assert!(c.xz_bytes < c.block_input_bytes / 4);
            }
            // The gate cost covers every evaluated block.
            assert_eq!(d.gate_cost.len(), 3);
            assert!(d.gate_cost.iter().all(|c| c.blocks == 5));
            assert!(out.libraries.contains_key("libzstd") && out.libraries.contains_key("liblzma"));
        });
    }

    #[test]
    fn a_whole_run_passes_its_own_check_and_renders_every_section() {
        with_run(|_, out| {
            let e = envelope(out.data);
            assert_eq!(check(&e), Vec::<String>::new());
            let md = render(&e);
            for needle in [
                "## Main table",
                "### `video`",
                "false-positive check",
                "sampled_entropy",
                "zstd1",
                "## Gate cost",
                "## Raw read speed",
                "precision (xz>=95%)",
                "video class",
            ] {
                assert!(md.contains(needle), "{needle}");
            }
            // Counts survive the JSON round trip and no name key leaks.
            let json = serde_json::to_value(&e.data).expect("json");
            let back: Data = serde_json::from_value(json.clone()).expect("back");
            assert_eq!(back, e.data);
            let mut problems = Vec::new();
            super::super::private_names(&json, "/data", "f", &mut problems);
            assert!(problems.is_empty(), "{problems:?}");
        });
    }

    #[test]
    fn check_catches_inconsistent_data() {
        with_run(|_, out| {
            let base = out.data;
            let mut d = base.clone();
            d.classes[0].gates[0].true_pos += 1;
            d.classes[0].full_blocks += 1;
            d.classes[1].gates.pop();
            d.classes[1].files = 0;
            d.gate_cost[1].blocks = 0;
            d.gate_cost.truncate(3);
            d.raw_read.passes.pop();
            d.raw_read.mode = "buffered".into();
            let p = check(&envelope(d));
            for needle in [
                "/data/classes/0/bytes",
                "/data/classes/0/gates/0",
                "/data/classes/1/gates",
                "/data/gate_cost/1",
                "/data/raw_read/passes",
                "/data/raw_read/mode",
            ] {
                assert!(p.iter().any(|m| m.starts_with(needle)), "{needle}: {p:?}");
            }
            let mut d = base.clone();
            d.classes.retain(|c| c.class != "video");
            d.entropy_thresholds.pop();
            let p = check(&envelope(d));
            assert!(p.iter().any(|m| m.contains("main class `video`")));
            assert!(p.iter().any(|m| m.starts_with("/data/entropy_thresholds")));
            let mut d = base;
            d.classes[2].gates.swap(0, 3);
            d.classes[2].truth_positive[1] = d.classes[2].blocks() + 1;
            let p = check(&envelope(d));
            assert!(p.iter().any(|m| m.starts_with("/data/classes/2/gates/0")));
            assert!(p
                .iter()
                .any(|m| m.starts_with("/data/classes/2/truth_positive/1")));
        });
    }

    #[test]
    fn evaluation_counts_follow_the_gate_definitions() {
        let mut c = ClassRec::new("x", true);
        let b = |len: u64, e: f64, z: u64, x: u64| BlockRec {
            len,
            entropy: e,
            sampled: e,
            zstd: z,
            xz: x,
        };
        // Incompressible by every definition, and a compressible one with a high entropy.
        c.add_block(&b(1000, 7.999, 1000, 1000));
        c.add_block(&b(1000, 7.96, 600, 500));
        // xz keeps exactly 98%: positive for 95 and 98, not for 99; zstd keeps exactly 99%.
        c.add_block(&b(1000, 7.0, 990, 980));
        assert_eq!(c.truth_positive, vec![2, 2, 1]);
        let r = row(&c, "entropy", 7.95, 95);
        assert_eq!(
            (r.true_pos, r.false_pos, r.true_neg, r.false_neg),
            (1, 1, 0, 1)
        );
        let r = row(&c, "entropy", 7.0, 99);
        assert_eq!(
            (r.true_pos, r.false_pos, r.true_neg, r.false_neg),
            (1, 2, 0, 0)
        );
        let r = row(&c, "zstd1", 99.0, 98);
        assert_eq!(
            (r.true_pos, r.false_pos, r.true_neg, r.false_neg),
            (2, 0, 1, 0)
        );
        let r = row(&c, "zstd1", 90.0, 99);
        assert_eq!((r.true_pos, r.false_pos), (1, 1));
        assert_eq!(precision_recall(r), ("50.00%".into(), "100.00%".into()));
    }

    #[test]
    fn the_aligned_buffer_is_aligned_and_sized() {
        let mut b = AlignedBuf::new(READ_BUFFER, READ_ALIGN);
        let s = b.slice();
        assert_eq!(s.len(), READ_BUFFER);
        assert_eq!(s.as_ptr() as usize % READ_ALIGN, 0);
    }

    #[test]
    fn both_read_modes_return_the_file_size_where_the_platform_allows() {
        let tmp = tempfile::tempdir().expect("tmp");
        let p = tmp.path().join("f.bin");
        let n = 2 * READ_BUFFER + 12_345;
        std::fs::write(&p, random(n, 5)).expect("write");
        let mut buf = AlignedBuf::new(READ_BUFFER, READ_ALIGN);
        assert_eq!(
            read_through(&p, ReadMode::Buffered, buf.slice()).expect("buffered"),
            n as u64
        );
        // The file system may refuse the cache bypass (a tmpfs does); that is the fallback case.
        match read_through(&p, ReadMode::Unbuffered, buf.slice()) {
            Ok(got) => assert_eq!(got, n as u64),
            Err(e) => eprintln!("unbuffered read refused here: {e}"),
        }
        let empty = tmp.path().join("e.bin");
        std::fs::write(&empty, b"").expect("write");
        assert_eq!(
            read_through(&empty, ReadMode::Buffered, buf.slice()).expect("empty"),
            0
        );
    }

    #[test]
    fn raw_read_records_three_passes_and_the_fallback() {
        with_run(|ctx, _| {
            let r = raw_read(ctx).expect("raw read");
            assert_eq!(r.passes.len(), 3);
            assert_eq!(r.cache_bypassed, r.mode == "unbuffered");
            assert_eq!(r.fallback_note.is_some(), r.mode == "buffered");
            let total: u64 = ctx
                .corpus
                .manifest
                .classes
                .values()
                .flat_map(|c| c.files.iter())
                .map(|f| f.bytes)
                .sum();
            assert!(r.passes.iter().all(|p| p.bytes == total && p.files == 5));
            let f = raw_read_with(ctx, Some("refused for the test".into())).expect("fallback");
            assert_eq!(f.mode, "buffered");
            assert!(!f.cache_bypassed);
            assert_eq!(f.fallback_note.as_deref(), Some("refused for the test"));
            assert_eq!(f.passes.len(), 3);
        });
    }
}
