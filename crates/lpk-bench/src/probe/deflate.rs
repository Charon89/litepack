//! `probe deflate` (PLAN P0-4): how much Deflate streams inside other files gain from
//! `preflate-rs` (bit-exact recompression: inflate, keep a small correction record, compress the
//! plain data better). The walker and the per-stream analysis described here live in
//! `lpk_core::peel::deflate` (moved there unchanged for the Deflate peel); this file keeps the
//! A/B compression, the validation rules, the rendering and the estimate labels.
//!
//! `preflate-rs` 0.7.6 on crates.io has only the stream-level API, so this probe finds the
//! streams itself, by content and one level deep (nothing inside inflated data is scanned):
//!
//! * ZIP family: every entry stored with method 8; the raw compressed bytes. Labelled `apk`,
//!   `office`, `jar` or `zip` by entry names first (`AndroidManifest.xml`, `[Content_Types].xml`
//!   or an ODF `mimetype`, `META-INF/MANIFEST.MF`), then by file extension;
//! * PNG: the concatenated `IDAT` payload, a zlib stream;
//! * gzip: every member (RFC 1952 header, raw deflate, 8-byte trailer);
//! * PDF: after each `stream` keyword and its end-of-line, any valid zlib header (method 8,
//!   window field at most 7, header divisible by 31, no preset dictionary). A stream that
//!   inflates cleanly is cut exactly; one that does not is sent to the library with the bytes up
//!   to the next `endstream` (or the end of the file), so it gets a named outcome;
//! * any other file, and a ZIP that cannot be parsed: a scan for the four common zlib headers
//!   (`78 01`, `78 5E`, `78 9C`, `78 DA`) at any offset, accepted only when the stream inflates
//!   cleanly (checksum included) to at least [`MIN_SCANNED_PLAIN`] bytes (everything else
//!   counts as `scan_false_hits`). The scan resumes after an accepted stream.
//!
//! The library has no "recognised encoder" flag. It estimates parameters for every stream it
//! accepts, and the cost of a poor estimate shows as correction overhead. This probe therefore
//! reports the estimate labels (strategy, hash, add policy, matching type) and overhead
//! buckets, and draws no recognised/unrecognised line. ZIP entries that are stored (method 0)
//! are not examined: the probe goes one level deep, so the result is a lower bound. The
//! per-stream metadata (container headers, the parameter header) is not counted.
//!
//! Per stream: analysis with `PreflateStreamProcessor` and its `is_done` (the library's own verification is
//! off; the probe verifies by itself), then `recreate_whole_deflate_stream` from the plain data
//! and the corrections, compared byte for byte with the original stream. The library returns
//! `Ok` with a shortened stream when the input is truncated or the plain-text limit is hit
//! after a first block, so the probe requires the stream to be complete (`is_done`) and the plain
//! size to equal the known one. A stream the library rejects is `failed` under the name of its
//! `ExitCode`; one that is incomplete is `failed` as `ShortRead`, or `skipped` when it is a
//! complete stream whose plain size is over the limit; `PlainTextLimit` and a declared or
//! measured plain size over the limit are `skipped`; a recreation that returns other bytes (or a
//! plain size other than the known one) is `failed` as `RoundtripMismatch`; a panic in the
//! library is `Panic`.
//!
//! Net gain per file: A = the file compressed whole with zstd (the baseline's `-19`) and xz
//! preset 9, one thread each; B = the file with every reconstructed stream replaced in place by
//! its plain data (the second and later `IDAT` payloads are cut out, their chunk frames stay),
//! compressed the same way; the corrections are added when rendering. Only analysis and
//! recreation are timed, alone, on data in memory; the A/B compression runs under `par_map`
//! and is not timed.

use std::collections::BTreeMap;

use anyhow::Result;
use lpk_core::peel::deflate::{
    examine, Limits, PlainText, TokenPredictorParameters, MAX_CHAIN_LENGTH, PREFLATE_VERSION,
};
pub use lpk_core::peel::deflate::{FAILURE_NAMES, KINDS, MIN_SCANNED_PLAIN};
use serde::{Deserialize, Serialize};

use super::codec::{xz_size, zstd_size, XzSettings, ZstdSettings};
use super::{mbps, md_header, md_table, par_map, pct, Ctx, Envelope, Output};

pub const NAME: &str = "deflate";

/// The classes this probe reads, in the order of the result.
pub const CLASSES: [&str; 5] = [
    "office-pdf",
    "photo-raw-png",
    "game-assets",
    "archives-nested",
    "software-installed",
];

/// Overhead buckets: corrections as a share of the Deflate stream size.
pub const BUCKETS: [&str; 6] = [
    "under_1_pct",
    "1_to_5_pct",
    "5_to_10_pct",
    "10_to_25_pct",
    "25_to_50_pct",
    "50_pct_or_more",
];

/// Text of `Settings::recognition`.
pub const RECOGNITION: &str = "the library has no recognised/unrecognised flag: every accepted stream gets an estimate; judge by the overhead buckets";

/// Input bytes (original and replaced) held before the A/B compression of a batch runs.
const BATCH_BYTES: u64 = 256 * 1024 * 1024;
/// xz preset 9 needs hundreds of MiB per encoder: fewer parallel encoders than cores.
const COMPRESS_THREADS: usize = 4;

// ---------------------------------------------------------------------------------------------
// The result

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// What the labels mean: the library gives no recognised/unrecognised flag.
    pub recognition: String,
    pub max_chain_length: u32,
    pub plain_text_limit: u64,
    pub file_plain_budget: u64,
    pub min_scanned_plain: u64,
    /// The library's own round-trip verification (off: the probe verifies each stream itself).
    pub library_verification: bool,
    pub zstd: ZstdSettings,
    pub xz: XzSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub settings: Settings,
    /// One entry per class of [`CLASSES`], in that order.
    pub classes: Vec<ClassRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRecord {
    pub class: String,
    /// Files the manifest lists for the class (0 when the corpus has none); `files` has one
    /// record for each, by index 0..manifest_files.
    pub manifest_files: u32,
    pub files: Vec<FileRecord>,
}

/// Counts and sizes of streams of one kind of outcome.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub streams: u64,
    pub deflate_bytes: u64,
    pub plain_bytes: u64,
    pub correction_bytes: u64,
}

impl Group {
    fn add(&mut self, other: &Group) {
        self.streams += other.streams;
        self.deflate_bytes += other.deflate_bytes;
        self.plain_bytes += other.plain_bytes;
        self.correction_bytes += other.correction_bytes;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamStats {
    pub found: u64,
    pub reconstructed: u64,
    pub skipped: u64,
    /// Failed streams by the library's error name (or `RoundtripMismatch`).
    pub failed: BTreeMap<String, u64>,
    /// ZIP entries refused before analysis (encrypted, outside the file, unreadable) and
    /// overlapping candidates.
    pub structural_refusals: u64,
    /// Zlib-scan hits that did not inflate cleanly to the minimum size.
    pub scan_false_hits: u64,
    /// The file walker panicked (the file is then recorded as kind `other` with no streams).
    pub walker_panics: u64,
    pub found_deflate_bytes: u64,
    pub reconstructed_deflate_bytes: u64,
    pub skipped_deflate_bytes: u64,
    pub failed_deflate_bytes: u64,
    pub reconstructed_plain_bytes: u64,
    pub correction_bytes: u64,
    /// `preflate_whole_deflate_stream` on the reconstructed streams.
    pub analyse_seconds: f64,
    /// `recreate_whole_deflate_stream` on the reconstructed streams.
    pub recreate_seconds: f64,
    /// Analysis (and recreation, when reached) of the streams that did not reconstruct.
    pub unreconstructed_seconds: f64,
    /// Reconstructed streams by the library's encoder estimate.
    pub estimates: BTreeMap<String, Group>,
    /// Reconstructed streams by [`BUCKETS`].
    pub overhead: BTreeMap<String, Group>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sizes {
    pub zstd_bytes: u64,
    pub xz_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    pub index: u32,
    /// Manifest path; absent for a private corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// One of [`KINDS`].
    pub kind: String,
    pub bytes: u64,
    pub streams: StreamStats,
    /// The original file, compressed whole.
    pub a: Sizes,
    /// Size of the file with the reconstructed streams replaced by their plain data.
    pub b_input_bytes: u64,
    /// That file compressed (corrections not included: they are `streams.correction_bytes`).
    pub b: Sizes,
}

// ---------------------------------------------------------------------------------------------
// One file (the walker and the analysis are `lpk_core::peel::deflate`)

fn extension(path: &str) -> String {
    path.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// The variant name of a `Debug` text: everything before the first `(`, ` {` or space.
fn variant(debug: &str) -> &str {
    debug.split(['(', ' ', '{']).next().unwrap_or(debug)
}

/// The library's estimate of the encoder, by names only (no numeric parameters), so that
/// streams of one encoder family group together.
fn estimate_label(p: Option<&TokenPredictorParameters>) -> String {
    match p {
        None => "none".to_string(),
        Some(p) => format!(
            "strategy={} hash={} add={} match={} zlib_compatible={}",
            variant(&format!("{:?}", p.strategy)),
            variant(&format!("{:?}", p.hash_algorithm)),
            variant(&format!("{:?}", p.add_policy)),
            variant(&format!("{:?}", p.matching_type)),
            p.zlib_compatible
        ),
    }
}

fn bucket(correction: u64, deflate: u64) -> &'static str {
    let c = u128::from(correction) * 100;
    let d = u128::from(deflate.max(1));
    if c < d {
        BUCKETS[0]
    } else if c < d * 5 {
        BUCKETS[1]
    } else if c < d * 10 {
        BUCKETS[2]
    } else if c < d * 25 {
        BUCKETS[3]
    } else if c < d * 50 {
        BUCKETS[4]
    } else {
        BUCKETS[5]
    }
}

struct FileResult {
    kind: &'static str,
    streams: StreamStats,
    /// The file with reconstructed streams replaced; `None` when nothing was reconstructed.
    replaced: Option<Vec<u8>>,
}

fn process_file(bytes: &[u8], ext: &str, limits: &Limits) -> FileResult {
    let mut estimates: BTreeMap<String, Group> = BTreeMap::new();
    let mut overhead: BTreeMap<String, Group> = BTreeMap::new();
    // (start, end, plain) for every edit of the replaced file.
    let mut edits: Vec<(usize, usize, Option<PlainText>)> = Vec::new();
    let examined = examine(bytes, ext, limits, |r| {
        let (corr, defl, plain_len) = (
            r.corrections.len() as u64,
            r.deflate_len as u64,
            r.plain.text().len() as u64,
        );
        let g = Group {
            streams: 1,
            deflate_bytes: defl,
            plain_bytes: plain_len,
            correction_bytes: corr,
        };
        estimates
            .entry(estimate_label(r.parameters.as_ref()))
            .or_default()
            .add(&g);
        overhead
            .entry(bucket(corr, defl).to_string())
            .or_default()
            .add(&g);
        let mut plain = Some(r.plain);
        for (a, b) in &r.spans {
            edits.push((*a, *b, plain.take()));
        }
    });
    let c = examined.counts;
    let st = StreamStats {
        found: c.found,
        reconstructed: c.reconstructed,
        skipped: c.skipped,
        failed: c.failed,
        structural_refusals: c.structural_refusals,
        scan_false_hits: c.scan_false_hits,
        walker_panics: c.walker_panics,
        found_deflate_bytes: c.found_deflate_bytes,
        reconstructed_deflate_bytes: c.reconstructed_deflate_bytes,
        skipped_deflate_bytes: c.skipped_deflate_bytes,
        failed_deflate_bytes: c.failed_deflate_bytes,
        reconstructed_plain_bytes: c.reconstructed_plain_bytes,
        correction_bytes: c.correction_bytes,
        analyse_seconds: c.analyse_seconds,
        recreate_seconds: c.recreate_seconds,
        unreconstructed_seconds: c.unreconstructed_seconds,
        estimates,
        overhead,
    };
    let replaced = (!edits.is_empty()).then(|| {
        let mut edits = edits;
        edits.sort_by_key(|e| e.0);
        // The final size is known: the file minus every replaced span plus every plain text.
        let removed: usize = edits.iter().map(|(a, b, _)| b - a).sum();
        let added: usize = edits
            .iter()
            .map(|(_, _, p)| p.as_ref().map_or(0, |p| p.text().len()))
            .sum();
        let mut out = Vec::with_capacity(bytes.len() - removed + added);
        let mut pos = 0;
        for (a, b, plain) in edits {
            out.extend_from_slice(&bytes[pos..a]);
            if let Some(p) = plain {
                out.extend_from_slice(p.text());
                // Each plain text is dropped as soon as it is copied.
            }
            pos = b;
        }
        out.extend_from_slice(&bytes[pos..]);
        out
    });
    FileResult {
        kind: examined.kind,
        streams: st,
        replaced,
    }
}

// ---------------------------------------------------------------------------------------------
// The run

struct Pending {
    record: FileRecord,
    bytes: Vec<u8>,
    replaced: Option<Vec<u8>>,
}

fn flush(
    batch: &mut Vec<Pending>,
    sink: &mut Vec<FileRecord>,
    zs: &ZstdSettings,
    xs: &XzSettings,
) -> Result<()> {
    let sizes = par_map(batch, COMPRESS_THREADS, |_, p| -> Result<[u64; 4]> {
        let a = (zstd_size(&p.bytes, zs)?, xz_size(&p.bytes, xs)?);
        let b = match &p.replaced {
            Some(r) => (zstd_size(r, zs)?, xz_size(r, xs)?),
            None => a,
        };
        Ok([a.0, a.1, b.0, b.1])
    });
    for (mut p, s) in batch.drain(..).zip(sizes) {
        let s = s?;
        p.record.a = Sizes {
            zstd_bytes: s[0],
            xz_bytes: s[1],
        };
        p.record.b = Sizes {
            zstd_bytes: s[2],
            xz_bytes: s[3],
        };
        sink.push(p.record);
    }
    Ok(())
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let limits = Limits::standard();
    let zs = ZstdSettings::level19();
    let xs = XzSettings::preset9();
    let mut classes = Vec::new();
    let mut notes = Vec::new();
    for class in CLASSES {
        let files = ctx.class_files(class);
        if files.is_none() {
            notes.push(format!("the corpus has no class `{class}`"));
        }
        let files = files.unwrap_or(&[]);
        let mut rec = ClassRecord {
            class: class.to_string(),
            manifest_files: u32::try_from(files.len())?,
            files: Vec::new(),
        };
        let mut batch: Vec<Pending> = Vec::new();
        let mut held = 0u64;
        for (i, f) in files.iter().enumerate() {
            let bytes = ctx.read_file(class, f)?;
            let r = process_file(&bytes, &extension(&f.path), &limits);
            let b_len = r.replaced.as_ref().map_or(bytes.len(), Vec::len);
            let this = (bytes.len() + b_len) as u64;
            if held > 0 && held + this > BATCH_BYTES {
                // A very large file is compressed alone.
                flush(&mut batch, &mut rec.files, &zs, &xs)?;
                held = 0;
            }
            held += this;
            batch.push(Pending {
                record: FileRecord {
                    index: u32::try_from(i)?,
                    path: ctx.label(f),
                    kind: r.kind.to_string(),
                    bytes: bytes.len() as u64,
                    streams: r.streams,
                    a: Sizes::default(),
                    b_input_bytes: b_len as u64,
                    b: Sizes::default(),
                },
                bytes,
                replaced: r.replaced,
            });
            if held >= BATCH_BYTES {
                flush(&mut batch, &mut rec.files, &zs, &xs)?;
                held = 0;
            }
        }
        flush(&mut batch, &mut rec.files, &zs, &xs)?;
        classes.push(rec);
    }
    let data = Data {
        settings: Settings {
            recognition: RECOGNITION.to_string(),
            max_chain_length: MAX_CHAIN_LENGTH,
            plain_text_limit: limits.plain_text_limit,
            file_plain_budget: limits.file_plain_budget,
            min_scanned_plain: MIN_SCANNED_PLAIN,
            library_verification: false,
            zstd: zs,
            xz: xs,
        },
        classes,
    };
    let mut out = Output::new(data, 1).with_zstd().with_xz();
    out.libraries
        .insert("preflate-rs".to_string(), PREFLATE_VERSION.to_string());
    out.notes = notes;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Validation rules

fn finite(p: &mut Vec<String>, at: &str, name: &str, v: f64) {
    if !(v.is_finite() && v >= 0.0) {
        p.push(format!("{at}/{name}: must be a non-negative number"));
    }
}

fn check_groups(
    p: &mut Vec<String>,
    at: &str,
    name: &str,
    g: &BTreeMap<String, Group>,
    s: &StreamStats,
    keys: Option<&[&str]>,
) {
    let mut sum = Group::default();
    for (k, v) in g {
        sum.add(v);
        if keys.is_some_and(|ks| !ks.contains(&k.as_str())) {
            p.push(format!("{at}/{name}/{k}: not a known key"));
        }
    }
    let want = Group {
        streams: s.reconstructed,
        deflate_bytes: s.reconstructed_deflate_bytes,
        plain_bytes: s.reconstructed_plain_bytes,
        correction_bytes: s.correction_bytes,
    };
    if sum != want {
        p.push(format!(
            "{at}/{name}: the groups do not add up to the reconstructed streams"
        ));
    }
}

// The keys allowed in `failed` are `FAILURE_NAMES` (from `lpk-core`).

/// The variant names of preflate-rs 0.7.6's parameter types (as `Debug` prints them, cut at the
/// first parenthesis): re-check them when the library is upgraded.
const STRATEGIES: [&str; 4] = ["Default", "RleOnly", "HuffOnly", "Store"];
const HASHES: [&str; 8] = [
    "None",
    "Zlib",
    "MiniZFast",
    "Libdeflate4Fast",
    "Libdeflate4",
    "ZlibNG",
    "RandomVector",
    "Crc32cHash",
];
const ADD_POLICIES: [&str; 5] = [
    "AddAll",
    "AddFirst",
    "AddFirstAndLast",
    "AddFirstExcept4kBoundary",
    "AddFirstWith32KBoundary",
];
const MATCHES: [&str; 2] = ["Greedy", "Lazy"];

fn estimate_label_ok(k: &str) -> bool {
    if k == "none" {
        return true;
    }
    let parts: Vec<&str> = k.split(' ').collect();
    let value_in = |part: &str, prefix: &str, names: &[&str]| {
        part.strip_prefix(prefix)
            .is_some_and(|v| names.contains(&v))
    };
    parts.len() == 5
        && value_in(parts[0], "strategy=", &STRATEGIES)
        && value_in(parts[1], "hash=", &HASHES)
        && value_in(parts[2], "add=", &ADD_POLICIES)
        && value_in(parts[3], "match=", &MATCHES)
        && matches!(parts[4], "zlib_compatible=true" | "zlib_compatible=false")
}

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    if e.library_threads != 1 {
        p.push("/library_threads: this probe compresses on one thread".to_string());
    }
    if d.settings.zstd != ZstdSettings::level19() || d.settings.xz != XzSettings::preset9() {
        p.push("/data/settings: zstd must be level19 and xz preset9, one thread each".to_string());
    }
    if d.settings.library_verification {
        p.push("/data/settings/library_verification: the probe verifies by itself".to_string());
    }
    let names: Vec<&str> = d.classes.iter().map(|c| c.class.as_str()).collect();
    if names != CLASSES {
        p.push(format!("/data/classes: expected the classes {CLASSES:?}"));
    }
    let private = e.corpus.private;
    for (ci, c) in d.classes.iter().enumerate() {
        let at = format!("/data/classes/{ci}");
        let indices: Vec<u32> = c.files.iter().map(|f| f.index).collect();
        if indices != (0..c.manifest_files).collect::<Vec<_>>() {
            p.push(format!(
                "{at}/files: the indices must cover 0..{} exactly, in order",
                c.manifest_files
            ));
        }
        for (fi, f) in c.files.iter().enumerate() {
            let at = format!("{at}/files/{fi}");
            let s = &f.streams;
            if private == f.path.is_some() {
                p.push(format!(
                    "{at}/path: present exactly when the corpus is public"
                ));
            }
            if !KINDS.contains(&f.kind.as_str()) {
                p.push(format!("{at}/kind: `{}` is not a known kind", f.kind));
            }
            let failed: u64 = s.failed.values().sum();
            if s.found != s.reconstructed + failed + s.skipped {
                p.push(format!(
                    "{at}/streams/found: {} but reconstructed + failed + skipped is {}",
                    s.found,
                    s.reconstructed + failed + s.skipped
                ));
            }
            if s.found_deflate_bytes
                != s.reconstructed_deflate_bytes + s.skipped_deflate_bytes + s.failed_deflate_bytes
            {
                p.push(format!(
                    "{at}/streams/found_deflate_bytes: not the sum of the three outcomes"
                ));
            }
            for k in s.failed.keys() {
                if !FAILURE_NAMES.contains(&k.as_str()) {
                    p.push(format!("{at}/streams/failed/{k}: not a library error name"));
                }
            }
            for k in s.estimates.keys() {
                if !estimate_label_ok(k) {
                    p.push(format!("{at}/streams/estimates/{k}: not an estimate label"));
                }
            }
            if f.bytes > 0 && f.a.xz_bytes == 0 {
                p.push(format!("{at}/a/xz_bytes: zero for a non-empty file"));
            }
            if s.found_deflate_bytes > f.bytes {
                p.push(format!(
                    "{at}/streams/found_deflate_bytes: more than the file"
                ));
            }
            if s.reconstructed == 0 && (s.analyse_seconds != 0.0 || s.recreate_seconds != 0.0) {
                p.push(format!(
                    "{at}/streams: analysis time recorded though nothing was reconstructed"
                ));
            }
            check_groups(&mut p, &at, "streams/estimates", &s.estimates, s, None);
            check_groups(
                &mut p,
                &at,
                "streams/overhead",
                &s.overhead,
                s,
                Some(&BUCKETS),
            );
            for (n, v) in [
                ("analyse_seconds", s.analyse_seconds),
                ("recreate_seconds", s.recreate_seconds),
                ("unreconstructed_seconds", s.unreconstructed_seconds),
            ] {
                finite(&mut p, &format!("{at}/streams"), n, v);
            }
            if s.reconstructed == 0 && (f.b != f.a || f.b_input_bytes != f.bytes) {
                p.push(format!(
                    "{at}/b: no stream was reconstructed, so B must equal A and the input"
                ));
            }
            if s.reconstructed > 0
                && f.b_input_bytes + s.reconstructed_deflate_bytes
                    != f.bytes + s.reconstructed_plain_bytes
            {
                // PNG frames of later IDAT chunks stay, so the replaced file is exactly the
                // original with the stream bytes swapped for the plain bytes.
                p.push(format!(
                    "{at}/b_input_bytes: not the file size with the streams swapped for plain data"
                ));
            }
        }
    }
    p
}

// ---------------------------------------------------------------------------------------------
// The table

#[derive(Default)]
struct Summary {
    files: u64,
    found: u64,
    reconstructed: u64,
    skipped: u64,
    failed: BTreeMap<String, u64>,
    rejected: u64,
    false_hits: u64,
    panics: u64,
    bytes: u64,
    deflate: u64,
    reconstructed_deflate: u64,
    plain: u64,
    corrections: u64,
    a: (u64, u64),
    b: (u64, u64),
    analyse: f64,
    recreate: f64,
    estimates: BTreeMap<String, Group>,
    overhead: BTreeMap<String, Group>,
}

impl Summary {
    fn add(&mut self, f: &FileRecord) {
        let s = &f.streams;
        self.files += 1;
        self.found += s.found;
        self.reconstructed += s.reconstructed;
        self.skipped += s.skipped;
        for (k, v) in &s.failed {
            *self.failed.entry(k.clone()).or_default() += v;
        }
        self.rejected += s.structural_refusals;
        self.false_hits += s.scan_false_hits;
        self.panics += s.walker_panics;
        self.bytes += f.bytes;
        self.deflate += s.found_deflate_bytes;
        self.reconstructed_deflate += s.reconstructed_deflate_bytes;
        self.plain += s.reconstructed_plain_bytes;
        self.corrections += s.correction_bytes;
        self.a.0 += f.a.zstd_bytes;
        self.a.1 += f.a.xz_bytes;
        self.b.0 += f.b.zstd_bytes + s.correction_bytes;
        self.b.1 += f.b.xz_bytes + s.correction_bytes;
        self.analyse += s.analyse_seconds;
        self.recreate += s.recreate_seconds;
        for (k, v) in &s.estimates {
            self.estimates.entry(k.clone()).or_default().add(v);
        }
        for (k, v) in &s.overhead {
            self.overhead.entry(k.clone()).or_default().add(v);
        }
    }

    fn failed_total(&self) -> u64 {
        self.failed.values().sum()
    }
}

fn summary_row(label: &str, s: &Summary) -> Vec<String> {
    vec![
        label.to_string(),
        s.files.to_string(),
        s.found.to_string(),
        s.reconstructed.to_string(),
        s.failed_total().to_string(),
        s.skipped.to_string(),
        s.bytes.to_string(),
        s.a.0.to_string(),
        s.b.0.to_string(),
        pct(s.b.0, s.a.0),
        s.a.1.to_string(),
        s.b.1.to_string(),
        pct(s.b.1, s.a.1),
    ]
}

const SUMMARY_HEAD: [&str; 13] = [
    "",
    "files",
    "streams found",
    "reconstructed",
    "failed",
    "skipped",
    "original bytes",
    "A zstd",
    "B zstd + corr",
    "B/A zstd",
    "A xz",
    "B xz + corr",
    "B/A xz",
];

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    let s_set = &d.settings;
    s.push_str(&format!(
        "Settings: max chain {}, plain text limit {} bytes per stream, {} bytes per file; library \
         verification {}; zstd level {}, xz preset {}; A = original file compressed whole, \
         B = reconstructed streams replaced by plain data, plus the correction bytes. Streams \
         found by the zlib scan must inflate to at least {} bytes. Recognition: {} Stored ZIP \
         entries are not examined (one level deep), so the gains are a lower bound; per-stream \
         metadata is not counted.\n\n",
        s_set.max_chain_length,
        s_set.plain_text_limit,
        s_set.file_plain_budget,
        if s_set.library_verification {
            "on"
        } else {
            "off"
        },
        s_set.zstd.level,
        s_set.xz.preset,
        s_set.min_scanned_plain,
        s_set.recognition
    ));
    let mut by_kind: BTreeMap<&str, Summary> = BTreeMap::new();
    let mut by_class: Vec<(&str, Summary)> = Vec::new();
    let mut all = Summary::default();
    for c in &d.classes {
        let mut cs = Summary::default();
        for f in &c.files {
            cs.add(f);
            all.add(f);
            by_kind.entry(f.kind.as_str()).or_default().add(f);
        }
        by_class.push((c.class.as_str(), cs));
    }
    s.push_str("## By container kind\n\n");
    let mut rows: Vec<Vec<String>> = KINDS
        .iter()
        .filter_map(|k| by_kind.get(k).map(|v| summary_row(k, v)))
        .collect();
    rows.push(summary_row("all", &all));
    s.push_str(&md_table(&SUMMARY_HEAD, &rows));
    s.push_str("\n## By class\n\n");
    let mut rows: Vec<Vec<String>> = by_class.iter().map(|(c, v)| summary_row(c, v)).collect();
    rows.push(summary_row("all", &all));
    s.push_str(&md_table(&SUMMARY_HEAD, &rows));

    s.push_str("\n## Failures and skips by cause\n\n");
    let mut causes: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
    for (k, v) in &by_kind {
        for (cause, n) in &v.failed {
            causes.entry(cause).or_default().insert(k, *n);
        }
        if v.skipped > 0 {
            causes
                .entry("(skipped for size)")
                .or_default()
                .insert(k, v.skipped);
        }
    }
    let mut head = vec!["cause"];
    head.extend(KINDS);
    head.push("total");
    let rows: Vec<Vec<String>> = causes
        .iter()
        .map(|(c, m)| {
            let mut r = vec![c.to_string()];
            r.extend(
                KINDS
                    .iter()
                    .map(|k| m.get(k).map_or("0".to_string(), |n| n.to_string())),
            );
            r.push(m.values().sum::<u64>().to_string());
            r
        })
        .collect();
    s.push_str(&md_table(&head, &rows));
    s.push_str(&format!(
        "\nStructural refusals (encrypted, out-of-range or overlapping entries): {}; zlib-scan false hits: {}; walker panics: {}.\n",
        all.rejected, all.false_hits, all.panics
    ));

    s.push_str("\n## Reconstructed streams by the library's encoder estimate\n\n");
    let rows: Vec<Vec<String>> = all.estimates.iter().map(|(k, g)| group_row(k, g)).collect();
    s.push_str(&md_table(&GROUP_HEAD, &rows));
    s.push_str("\n## Reconstructed streams by correction overhead\n\n");
    let rows: Vec<Vec<String>> = BUCKETS
        .iter()
        .filter_map(|k| all.overhead.get(*k).map(|g| group_row(k, g)))
        .collect();
    s.push_str(&md_table(&GROUP_HEAD, &rows));

    s.push_str("\n## Speed on reconstructed streams, by container kind\n\n");
    let mut rows = Vec::new();
    for k in KINDS {
        if let Some(v) = by_kind.get(k) {
            rows.push(speed_row(k, v));
        }
    }
    rows.push(speed_row("all", &all));
    s.push_str(&md_table(
        &[
            "",
            "deflate bytes",
            "plain bytes",
            "analyse s",
            "analyse MB/s (deflate in)",
            "recreate s",
            "recreate MB/s (deflate out)",
        ],
        &rows,
    ));
    s
}

const GROUP_HEAD: [&str; 6] = [
    "",
    "streams",
    "deflate bytes",
    "plain bytes",
    "correction bytes",
    "corrections / deflate",
];

fn group_row(k: &str, g: &Group) -> Vec<String> {
    vec![
        k.to_string(),
        g.streams.to_string(),
        g.deflate_bytes.to_string(),
        g.plain_bytes.to_string(),
        g.correction_bytes.to_string(),
        pct(g.correction_bytes, g.deflate_bytes),
    ]
}

fn speed_row(k: &str, v: &Summary) -> Vec<String> {
    vec![
        k.to_string(),
        v.reconstructed_deflate.to_string(),
        v.plain.to_string(),
        format!("{:.3}", v.analyse),
        mbps(v.reconstructed_deflate, v.analyse),
        format!("{:.3}", v.recreate),
        mbps(v.reconstructed_deflate, v.recreate),
    ]
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::*;
    use crate::corpus::manifest::{Manifest, ManifestFile};
    use crate::probe::{execute, Config};
    use lpk_core::peel::deflate::PANIC;

    /// Deterministic compressible text.
    fn text(n: usize, seed: u32) -> Vec<u8> {
        let words = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
        ];
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
        let mut out = Vec::new();
        while out.len() < n {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            out.extend_from_slice(words[(x >> 24) as usize % words.len()].as_bytes());
            out.push(b' ');
        }
        out.truncate(n);
        out
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        e.write_all(data).expect("write");
        e.finish().expect("finish")
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        e.write_all(data).expect("write");
        e.finish().expect("finish")
    }

    fn zip_with(names: &[&str], data: &[u8]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (i, n) in names.iter().enumerate() {
            w.start_file(*n, deflated).expect("start");
            w.write_all(&text(data.len(), i as u32 + 1)).expect("write");
        }
        w.start_file("raw.bin", stored).expect("start");
        w.write_all(b"stored entry").expect("write");
        w.finish().expect("finish").into_inner()
    }

    fn png_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 64, 64);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().expect("header");
            let img: Vec<u8> = (0..64 * 64 * 3).map(|i| ((i / 7) % 251) as u8).collect();
            w.write_image_data(&img).expect("data");
        }
        out
    }

    fn pdf_bytes(stream: &[u8]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n1 0 obj\n<< /Filter /FlateDecode >>\nstream\n".to_vec();
        out.extend_from_slice(stream);
        out.extend_from_slice(
            b"\nendstream\nendobj\n2 0 obj\n<< >>\nstream\nplain text\nendstream\n",
        );
        out
    }

    fn limits() -> Limits {
        Limits::standard()
    }

    fn check_stats(r: &FileResult) {
        let s = &r.streams;
        let failed: u64 = s.failed.values().sum();
        assert_eq!(s.found, s.reconstructed + failed + s.skipped);
    }

    #[test]
    fn zlib_and_gzip_and_pdf_streams_are_reconstructed_and_replaced() {
        let plain = text(30_000, 1);
        let z = zlib(&plain);
        let mut file = b"HEADER".to_vec();
        file.extend_from_slice(&z);
        file.extend_from_slice(b"TRAILER");
        let r = process_file(&file, "bin", &limits());
        assert_eq!(r.kind, "other");
        assert_eq!(r.streams.reconstructed, 1, "{:?}", r.streams);
        let replaced = r.replaced.expect("replaced");
        // header(2) and checksum(4) stay, the deflate bytes become the plain bytes.
        let defl = r.streams.reconstructed_deflate_bytes as usize;
        assert_eq!(replaced.len(), file.len() - defl + plain.len());
        assert!(replaced.windows(plain.len()).any(|w| w == plain));

        let g = gzip(&plain);
        let r = process_file(&g, "gz", &limits());
        assert_eq!(r.kind, "gzip");
        assert_eq!((r.streams.found, r.streams.reconstructed), (1, 1));

        let two = [gzip(&plain), gzip(&text(5000, 9))].concat();
        let r = process_file(&two, "gz", &limits());
        assert_eq!((r.streams.found, r.streams.reconstructed), (2, 2));

        let r = process_file(&pdf_bytes(&z), "pdf", &limits());
        assert_eq!(r.kind, "pdf");
        assert_eq!((r.streams.found, r.streams.reconstructed), (1, 1));
        assert_eq!(r.streams.reconstructed_plain_bytes, plain.len() as u64);
        check_stats(&r);
    }

    #[test]
    fn zip_kinds_are_told_by_entry_names_then_extension() {
        let d = vec![0u8; 3000];
        let apk = zip_with(&["AndroidManifest.xml", "classes.dex"], &d);
        let office = zip_with(&["[Content_Types].xml", "word/document.xml"], &d);
        let jar = zip_with(&["META-INF/MANIFEST.MF", "A.class"], &d);
        let plain = zip_with(&["a.txt"], &d);
        assert_eq!(process_file(&apk, "zip", &limits()).kind, "apk");
        assert_eq!(process_file(&office, "zip", &limits()).kind, "office");
        assert_eq!(process_file(&jar, "zip", &limits()).kind, "jar");
        assert_eq!(process_file(&plain, "zip", &limits()).kind, "zip");
        assert_eq!(process_file(&plain, "docx", &limits()).kind, "office");
        assert_eq!(process_file(&plain, "jar", &limits()).kind, "jar");
        assert_eq!(process_file(&plain, "apk", &limits()).kind, "apk");
        // Packages that share `[Content_Types].xml` with Office are not Office.
        let msix = zip_with(&["[Content_Types].xml", "AppxManifest.xml"], &d);
        assert_eq!(process_file(&msix, "msix", &limits()).kind, "zip");
        assert_eq!(process_file(&msix, "nupkg", &limits()).kind, "zip");
        let r = process_file(&jar, "jar", &limits());
        assert_eq!(r.streams.found, 2, "the stored entry is not a stream");
        check_stats(&r);
        let one = process_file(&plain, "zip", &limits());
        assert_eq!((one.streams.found, one.streams.reconstructed), (1, 1));
        // The entry is replaced by plain data, so the file grows.
        assert!(one.replaced.expect("replaced").len() > plain.len());
    }

    #[test]
    fn overlapping_zip_entries_are_structural_refusals() {
        // Two central-directory entries pointing at the same data.
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("a.txt", o).expect("start");
        w.write_all(&text(5000, 1)).expect("write");
        w.start_file("b.txt", o).expect("start");
        w.write_all(&text(5000, 1)).expect("write");
        let mut bytes = w.finish().expect("finish").into_inner();
        // Make the second central entry's local header offset the first's (0).
        let mut seen = 0;
        for i in 0..bytes.len() - 46 {
            if bytes[i..].starts_with(b"PK\x01\x02") {
                seen += 1;
                if seen == 2 {
                    bytes[i + 42..i + 46].copy_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let r = process_file(&bytes, "zip", &limits());
        assert_eq!(
            r.streams.found + r.streams.structural_refusals,
            2,
            "{:?}",
            r.streams
        );
        assert!(r.streams.structural_refusals >= 1, "{:?}", r.streams);
        check_stats(&r);
    }

    #[test]
    fn a_png_is_one_stream_over_all_idat_chunks() {
        let png = png_bytes();
        let r = process_file(&png, "png", &limits());
        assert_eq!(r.kind, "png");
        assert_eq!((r.streams.found, r.streams.reconstructed), (1, 1));
        check_stats(&r);
        // Several IDAT chunks: split the payload by hand.
        let idat = {
            let mut pos = 8;
            let mut found = None;
            while pos + 8 <= png.len() {
                let len = u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]])
                    as usize;
                if &png[pos + 4..pos + 8] == b"IDAT" {
                    found = Some((pos, len));
                    break;
                }
                pos += 12 + len;
            }
            found.expect("idat")
        };
        let (pos, len) = idat;
        let payload = &png[pos + 8..pos + 8 + len];
        let mut split = png[..pos].to_vec();
        for part in [&payload[..len / 2], &payload[len / 2..]] {
            split.extend_from_slice(&(part.len() as u32).to_be_bytes());
            split.extend_from_slice(b"IDAT");
            split.extend_from_slice(part);
            split.extend_from_slice(&[0; 4]); // checksum is not checked by the probe
        }
        split.extend_from_slice(&png[pos + 12 + len..]);
        let r2 = process_file(&split, "png", &limits());
        assert_eq!(r2.streams.found, 1);
        assert_eq!(r2.streams.reconstructed, r.streams.reconstructed);
        assert_eq!(
            r2.streams.found_deflate_bytes, r.streams.found_deflate_bytes,
            "the concatenated payload is the same stream"
        );
    }

    #[test]
    fn the_scan_needs_a_clean_stream_of_enough_plain_data() {
        let small = zlib(&text(200, 3));
        let r = process_file(&[b"xx".as_slice(), &small].concat(), "bin", &limits());
        assert_eq!(r.streams.found, 0, "plain data under the minimum");
        assert!(r.streams.scan_false_hits >= 1);
        let mut broken = zlib(&text(20_000, 4));
        let mid = broken.len() / 2;
        broken[mid] ^= 0xff;
        let r = process_file(&broken, "bin", &limits());
        assert_eq!(r.streams.found, 0, "a corrupt stream is not a stream");
        let none = process_file(&text(5000, 5), "txt", &limits());
        assert_eq!((none.kind, none.streams.found), ("other", 0));
        assert!(none.replaced.is_none());
    }

    #[test]
    fn a_stream_over_the_limit_is_skipped_and_counted() {
        let plain = text(30_000, 6);
        let file = [b"zz".as_slice(), &zlib(&plain)].concat();
        let small = Limits {
            plain_text_limit: 1000,
            file_plain_budget: 1 << 30,
        };
        let r = process_file(&file, "bin", &small);
        assert_eq!((r.streams.found, r.streams.skipped), (1, 1));
        assert!(r.streams.skipped_deflate_bytes > 0 && r.replaced.is_none());
        // Here the probe's own pre-check fires (the entry's declared size is over the limit); the
        // library's plain-text limit is covered by the multi-block test.
        let zipf = zip_with(&["a.txt"], &plain);
        let r = process_file(&zipf, "zip", &small);
        assert_eq!(r.streams.skipped, 1);
        // A per-file budget of zero skips everything.
        let none = Limits {
            plain_text_limit: 1 << 30,
            file_plain_budget: 0,
        };
        let r = process_file(&file, "bin", &none);
        assert_eq!(r.streams.skipped, 1);
        check_stats(&r);
    }

    #[test]
    fn a_stream_the_library_rejects_is_a_failure_with_its_name() {
        // A "Deflated" ZIP entry whose bytes are not Deflate.
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file(
            "x.bin",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .expect("start");
        w.write_all(&[0xff; 64]).expect("write");
        let mut bytes = w.finish().expect("finish").into_inner();
        // Patch the method fields (local and central header) from 0 (stored) to 8 (deflated).
        for i in 0..bytes.len().saturating_sub(10) {
            if bytes[i..].starts_with(b"PK\x03\x04") {
                bytes[i + 8] = 8;
            }
            if bytes[i..].starts_with(b"PK\x01\x02") {
                bytes[i + 10] = 8;
            }
        }
        let r = process_file(&bytes, "zip", &limits());
        assert_eq!(r.kind, "zip");
        assert_eq!(r.streams.found, 1, "{:?}", r.streams);
        assert_eq!(r.streams.reconstructed, 0);
        let failed: u64 = r.streams.failed.values().sum();
        assert_eq!(failed, 1, "{:?}", r.streams);
        assert!(r.streams.failed_deflate_bytes > 0);
        let cause = r.streams.failed.keys().next().expect("cause");
        assert!(!cause.is_empty() && cause.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(r.replaced.is_none());
    }

    #[test]
    fn a_truncated_gzip_member_is_named_by_the_library() {
        let g = gzip(&text(400_000, 7));
        let r = process_file(&g[..g.len() / 2], "gz", &limits());
        assert_eq!(r.kind, "gzip");
        assert_eq!(r.streams.found, 1);
        assert_eq!(r.streams.reconstructed, 0, "{:?}", r.streams);
        assert_eq!(
            r.streams.failed.get("ShortRead"),
            Some(&1),
            "{:?}",
            r.streams
        );
        assert_eq!(
            r.streams.found_deflate_bytes,
            r.streams.failed_deflate_bytes
        );
        assert!(r.replaced.is_none());
        check_stats(&r);
    }

    fn raw_deflate(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        e.write_all(data).expect("write");
        e.finish().expect("finish")
    }

    #[test]
    fn pdf_streams_that_do_not_inflate_get_a_named_outcome() {
        let plain = text(300_000, 3);
        let z = zlib(&plain);
        // Truncated stream, a plausible but arbitrary header, and a keyword inside a string.
        let mut pdf = b"%PDF-1.4\n(a stream\nnot data) ".to_vec();
        pdf.extend_from_slice(b"1 0 obj\nstream\r\n");
        pdf.extend_from_slice(&z[..z.len() / 2]);
        pdf.extend_from_slice(
            b"\nendstream\n2 0 obj\n<< /Filter /FlateDecode >>\nstream\n\x58\x85 garbage\nendstream\n",
        );
        // The same header in an object that does not say Flate is not a stream at all.
        let mut without = pdf.clone();
        without.extend_from_slice(b"3 0 obj\n<< >>\nstream\n\x58\x85 more\nendstream\n");
        assert_eq!(process_file(&without, "pdf", &limits()).streams.found, 2);
        let r = process_file(&pdf, "pdf", &limits());
        assert_eq!(r.kind, "pdf");
        // The 0x58 0x85 header is valid (method 8, window 5, divisible by 31).
        assert_eq!(r.streams.found, 2, "{:?}", r.streams);
        assert_eq!(r.streams.reconstructed, 0);
        let failed: u64 = r.streams.failed.values().sum();
        assert_eq!(failed, 2, "{:?}", r.streams);
        check_stats(&r);
    }

    #[test]
    fn a_png_with_a_huge_declared_chunk_length_is_not_followed() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&u32::MAX.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&[1, 2, 3]);
        let r = process_file(&png, "png", &limits());
        assert_eq!((r.kind, r.streams.found), ("png", 0));
    }

    #[test]
    fn gzip_headers_with_extra_and_name_fields_are_walked() {
        let plain = text(30_000, 11);
        let body = raw_deflate(&plain);
        let mut g = vec![0x1f, 0x8b, 8, 4 | 8, 0, 0, 0, 0, 0, 3];
        g.extend_from_slice(&3u16.to_le_bytes());
        g.extend_from_slice(b"abc");
        g.extend_from_slice(b"name.txt\0");
        g.extend_from_slice(&body);
        g.extend_from_slice(&[0; 8]);
        let r = process_file(&g, "gz", &limits());
        assert_eq!(r.kind, "gzip");
        assert_eq!(
            (r.streams.found, r.streams.reconstructed),
            (1, 1),
            "{:?}",
            r.streams
        );
        assert_eq!(r.streams.reconstructed_deflate_bytes, body.len() as u64);
    }

    #[test]
    fn check_rejects_foreign_failure_names_and_labels() {
        assert!(estimate_label_ok("none"));
        assert!(estimate_label_ok(
            "strategy=Default hash=Zlib add=AddAll match=Lazy zlib_compatible=true"
        ));
        assert!(!estimate_label_ok("hash=Zlib"));
        assert!(!estimate_label_ok(
            "strategy=Default hash=Zlib add=AddAll match=Lazy zlib_compatible=maybe"
        ));
        assert!(FAILURE_NAMES.contains(&"ShortRead") && FAILURE_NAMES.contains(&PANIC));
    }

    #[test]
    fn the_overhead_buckets_cover_every_share() {
        assert_eq!(bucket(0, 1000), "under_1_pct");
        assert_eq!(bucket(9, 1000), "under_1_pct");
        assert_eq!(bucket(10, 1000), "1_to_5_pct");
        assert_eq!(bucket(50, 1000), "5_to_10_pct");
        assert_eq!(bucket(100, 1000), "10_to_25_pct");
        assert_eq!(bucket(250, 1000), "25_to_50_pct");
        assert_eq!(bucket(500, 1000), "50_pct_or_more");
        assert_eq!(bucket(5, 0), "50_pct_or_more");
        assert_eq!(variant("Zlib { hash_mask: 1 }"), "Zlib");
        assert_eq!(variant("AddFirst(3)"), "AddFirst");
        assert_eq!(variant("None"), "None");
    }

    /// A PNG built by hand (no encoder), its zlib stream split over three `IDAT` chunks.
    fn multi_idat_png() -> Vec<u8> {
        let (w, h) = (48u32, 32u32);
        let mut raw = Vec::new();
        for y in 0..h {
            raw.push(0); // filter type None
            for x in 0..w * 3 {
                raw.push(((x * 7 + y * 13) % 251) as u8);
            }
        }
        let z = zlib(&raw);
        let chunk = |out: &mut Vec<u8>, ty: &[u8], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(ty);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0; 4]); // the walker does not check CRCs
        };
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        let third = z.len() / 3;
        chunk(&mut out, b"IDAT", &z[..third]);
        chunk(&mut out, b"tEXt", b"Comment\0between");
        chunk(&mut out, b"IDAT", &z[third..2 * third]);
        chunk(&mut out, b"IDAT", &z[2 * third..]);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// The five synthetic files of the before/after comparison, with their extensions.
    fn golden_inputs() -> Vec<(&'static str, &'static str, Vec<u8>)> {
        let mut bin = b"MZ\x90\x00binary head".to_vec();
        let mut x = 7u32;
        for _ in 0..4000 {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            bin.push((x >> 24) as u8);
        }
        bin.extend_from_slice(&zlib(&text(30_000, 21)));
        bin.extend_from_slice(b"binary tail");
        vec![
            ("zip", "zip", zip_with(&["a.txt", "b.txt"], &[0u8; 6000])),
            ("png", "png", multi_idat_png()),
            ("gzip", "gz", gzip(&text(30_000, 22))),
            ("pdf", "pdf", pdf_bytes(&zlib(&text(20_000, 23)))),
            ("bin", "bin", bin),
        ]
    }

    /// Every count of a file result except the timings, plus the replaced file's length and hash.
    fn golden_snapshot(r: &FileResult) -> String {
        let s = &r.streams;
        let replaced = r
            .replaced
            .as_ref()
            .map(|b| format!("{} {}", b.len(), blake3::hash(b).to_hex()));
        format!(
            "kind={} found={} reconstructed={} skipped={} failed={:?} refusals={} false_hits={} \
             walker_panics={} found_deflate={} reconstructed_deflate={} skipped_deflate={} \
             failed_deflate={} plain={} corrections={} estimates={:?} overhead={:?} replaced={:?}",
            r.kind,
            s.found,
            s.reconstructed,
            s.skipped,
            s.failed,
            s.structural_refusals,
            s.scan_false_hits,
            s.walker_panics,
            s.found_deflate_bytes,
            s.reconstructed_deflate_bytes,
            s.skipped_deflate_bytes,
            s.failed_deflate_bytes,
            s.reconstructed_plain_bytes,
            s.correction_bytes,
            s.estimates,
            s.overhead,
            replaced
        )
    }

    /// The probe's counts for the synthetic files, recorded from the probe code before the walker
    /// moved to `lpk-core` (`lpk_core::peel::deflate`); the inputs are also `lpk-core`'s fixtures.
    const GOLDEN: [(&str, &[u8], &str); 5] = [
        (
            "zip",
            include_bytes!("../../../lpk-core/tests/fixtures/deflate/zip.zip"),
            "kind=zip found=2 reconstructed=2 skipped=0 failed={} refusals=0 false_hits=0 walker_panics=0 found_deflate=1525 reconstructed_deflate=1525 skipped_deflate=0 failed_deflate=0 plain=12000 corrections=48 estimates={\"strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true\": Group { streams: 2, deflate_bytes: 1525, plain_bytes: 12000, correction_bytes: 48 }} overhead={\"1_to_5_pct\": Group { streams: 2, deflate_bytes: 1525, plain_bytes: 12000, correction_bytes: 48 }} replaced=Some(\"12296 527a2dfd1ed0c0c06e5e621c242a286d857fa3de02096d5abe8826de6b014007\")",
        ),
        (
            "png",
            include_bytes!("../../../lpk-core/tests/fixtures/deflate/png.png"),
            "kind=png found=1 reconstructed=1 skipped=0 failed={} refusals=0 false_hits=0 walker_panics=0 found_deflate=463 reconstructed_deflate=463 skipped_deflate=0 failed_deflate=0 plain=4640 corrections=49 estimates={\"strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true\": Group { streams: 1, deflate_bytes: 463, plain_bytes: 4640, correction_bytes: 49 }} overhead={\"10_to_25_pct\": Group { streams: 1, deflate_bytes: 463, plain_bytes: 4640, correction_bytes: 49 }} replaced=Some(\"4754 28fbea47ecff8ce408dd920ef2908dd44fe947d8644a490d107766efd00dac7e\")",
        ),
        (
            "gzip",
            include_bytes!("../../../lpk-core/tests/fixtures/deflate/gzip.gz"),
            "kind=gzip found=1 reconstructed=1 skipped=0 failed={} refusals=0 false_hits=0 walker_panics=0 found_deflate=3236 reconstructed_deflate=3236 skipped_deflate=0 failed_deflate=0 plain=30000 corrections=24 estimates={\"strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true\": Group { streams: 1, deflate_bytes: 3236, plain_bytes: 30000, correction_bytes: 24 }} overhead={\"under_1_pct\": Group { streams: 1, deflate_bytes: 3236, plain_bytes: 30000, correction_bytes: 24 }} replaced=Some(\"30018 53b01ce9e19b7fdf6cad6bec488dd5d05077c464a587f800f893da34e75457c8\")",
        ),
        (
            "pdf",
            include_bytes!("../../../lpk-core/tests/fixtures/deflate/pdf.pdf"),
            "kind=pdf found=1 reconstructed=1 skipped=0 failed={} refusals=0 false_hits=0 walker_panics=0 found_deflate=2205 reconstructed_deflate=2205 skipped_deflate=0 failed_deflate=0 plain=20000 corrections=38 estimates={\"strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true\": Group { streams: 1, deflate_bytes: 2205, plain_bytes: 20000, correction_bytes: 38 }} overhead={\"1_to_5_pct\": Group { streams: 1, deflate_bytes: 2205, plain_bytes: 20000, correction_bytes: 38 }} replaced=Some(\"20117 c659bda7d83ea7b9e5cfc91c629d58a24b54b8b3539a7b72b3ca4c1c8d202d16\")",
        ),
        (
            "bin",
            include_bytes!("../../../lpk-core/tests/fixtures/deflate/bin.bin"),
            "kind=other found=1 reconstructed=1 skipped=0 failed={} refusals=0 false_hits=0 walker_panics=0 found_deflate=3223 reconstructed_deflate=3223 skipped_deflate=0 failed_deflate=0 plain=30000 corrections=32 estimates={\"strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true\": Group { streams: 1, deflate_bytes: 3223, plain_bytes: 30000, correction_bytes: 32 }} overhead={\"under_1_pct\": Group { streams: 1, deflate_bytes: 3223, plain_bytes: 30000, correction_bytes: 32 }} replaced=Some(\"34032 a48039e84157720e0c6a2908ae3baba5640896f977d367f25d5c5c51ce812491\")",
        ),
    ];

    #[test]
    fn golden_counts_of_synthetic_files_are_unchanged() {
        let inputs = golden_inputs();
        assert_eq!(inputs.len(), GOLDEN.len());
        for ((name, ext, bytes), (g_name, fixture, want)) in inputs.iter().zip(GOLDEN) {
            assert_eq!(*name, g_name);
            assert!(
                bytes == fixture,
                "{name}: the generated input differs from the fixture"
            );
            let r = process_file(bytes, ext, &limits());
            assert_eq!(golden_snapshot(&r), want, "{name}");
        }
    }

    /// A ZIP for the non-success counts: a "Deflated" member whose bytes are not Deflate
    /// (failed by name), a member that reconstructs, one over the plain-text limit, one skipped
    /// by the per-file budget, and a central entry pointing at the second member's data
    /// (a structural refusal).
    fn troubled_zip() -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        w.start_file("bad.bin", stored).expect("start");
        w.write_all(&[0xff; 64]).expect("write");
        for (name, len, seed) in [
            ("a.txt", 5000, 31),
            ("big.txt", 20_000, 32),
            ("c.txt", 3000, 33),
        ] {
            w.start_file(name, deflated).expect("start");
            w.write_all(&text(len, seed)).expect("write");
        }
        w.start_file("dup.txt", deflated).expect("start");
        w.write_all(&text(4000, 34)).expect("write");
        let mut b = w.finish().expect("finish").into_inner();
        let central: Vec<usize> = (0..b.len() - 46)
            .filter(|i| b[*i..].starts_with(b"PK\x01\x02"))
            .collect();
        // bad.bin is the first member: method 0 -> 8 in its local and central headers.
        b[8] = 8;
        b[central[0] + 10] = 8;
        // dup.txt's central entry points at a.txt's local header.
        let a_off: [u8; 4] = b[central[1] + 42..central[1] + 46].try_into().expect("4");
        b[central[4] + 42..central[4] + 46].copy_from_slice(&a_off);
        b
    }

    /// A binary for the zlib scan: a stream under the minimum plain size and a corrupt one (scan
    /// false hits), then a clean one.
    fn troubled_scan() -> Vec<u8> {
        let mut b = b"scan head ".to_vec();
        b.extend_from_slice(&zlib(&text(300, 41)));
        b.extend_from_slice(b" gap ");
        let mut broken = zlib(&text(8000, 42));
        let mid = broken.len() / 2;
        broken[mid] ^= 0xff;
        b.extend_from_slice(&broken);
        b.extend_from_slice(b" gap ");
        b.extend_from_slice(&zlib(&text(6000, 43)));
        b.extend_from_slice(b" tail");
        b
    }

    /// The non-success golden cases, with the limits each runs under.
    fn golden_troubled() -> Vec<(&'static str, &'static str, Vec<u8>, Limits)> {
        vec![
            (
                "troubled",
                "zip",
                troubled_zip(),
                Limits {
                    plain_text_limit: 10_000,
                    file_plain_budget: 5000,
                },
            ),
            ("troubled-scan", "bin", troubled_scan(), Limits::standard()),
        ]
    }

    /// The non-success cases' counts, recorded by running this test code on a scratch checkout of
    /// 8423a14 (the probe before the walker moved).
    const GOLDEN_TROUBLED: [(&str, &[u8], &str); 2] = [
        (
            "troubled",
            include_bytes!("../../../lpk-core/tests/fixtures/deflate/troubled.zip"),
            "kind=zip found=4 reconstructed=1 skipped=2 failed={\"InvalidDeflate\": 1} refusals=1 false_hits=0 walker_panics=0 found_deflate=3380 reconstructed_deflate=652 skipped_deflate=2664 failed_deflate=64 plain=5000 corrections=24 estimates={\"strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true\": Group { streams: 1, deflate_bytes: 652, plain_bytes: 5000, correction_bytes: 24 }} overhead={\"1_to_5_pct\": Group { streams: 1, deflate_bytes: 652, plain_bytes: 5000, correction_bytes: 24 }} replaced=Some(\"8742 fdf04005632224e8a8de4e5cbc59018f7271688007368f57541ff83401e008f4\")",
        ),
        (
            "troubled-scan",
            include_bytes!("../../../lpk-core/tests/fixtures/deflate/troubled-scan.bin"),
            "kind=other found=1 reconstructed=1 skipped=0 failed={} refusals=0 false_hits=2 walker_panics=0 found_deflate=762 reconstructed_deflate=762 skipped_deflate=0 failed_deflate=0 plain=6000 corrections=24 estimates={\"strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true\": Group { streams: 1, deflate_bytes: 762, plain_bytes: 6000, correction_bytes: 24 }} overhead={\"1_to_5_pct\": Group { streams: 1, deflate_bytes: 762, plain_bytes: 6000, correction_bytes: 24 }} replaced=Some(\"7106 50bcf0f8bfb965abc33116b0e77d559bddcc238ddd279787e046dece6fa9d30c\")",
        ),
    ];

    #[test]
    fn golden_counts_of_the_non_success_paths_are_unchanged() {
        let inputs = golden_troubled();
        assert_eq!(inputs.len(), GOLDEN_TROUBLED.len());
        for ((name, ext, bytes, l), (g_name, fixture, want)) in inputs.iter().zip(GOLDEN_TROUBLED) {
            assert_eq!(*name, g_name);
            assert!(
                bytes == fixture,
                "{name}: the generated input differs from the fixture"
            );
            let r = process_file(bytes, ext, l);
            assert_eq!(golden_snapshot(&r), want, "{name}");
        }
    }

    fn manifest_file(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile {
            blake3: blake3::hash(data).to_hex().to_string(),
            bytes: data.len() as u64,
            licence: "CC0-1.0".to_string(),
            path: path.to_string(),
            source: "test".to_string(),
        }
    }

    fn corpus(tmp: &Path) -> PathBuf {
        let dir = tmp.join("corpus");
        let plain = text(40_000, 8);
        let files: Vec<(&str, &str, Vec<u8>)> = vec![
            ("office-pdf", "a.pdf", pdf_bytes(&zlib(&plain))),
            (
                "office-pdf",
                "b.docx",
                zip_with(&["[Content_Types].xml", "w.xml"], &plain),
            ),
            ("photo-raw-png", "c.png", png_bytes()),
            ("archives-nested", "d.gz", gzip(&plain)),
            ("software-installed", "e.bin", text(3000, 2)),
        ];
        let mut entries = Vec::new();
        for (class, name, data) in &files {
            let p = dir.join(class).join(name);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(&p, data).expect("write");
            entries.push((
                class.to_string(),
                manifest_file(&format!("{class}/{name}"), data),
            ));
        }
        let m = Manifest::with_profile_name("small", entries);
        std::fs::write(dir.join("manifest.json"), m.render()).expect("manifest");
        dir
    }

    #[test]
    fn end_to_end_writes_a_valid_result_with_every_class() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path().join("results");
        let cfg = Config {
            probes: vec!["deflate".to_string()],
            corpus: corpus(tmp.path()),
            into: None,
            results_root: root.clone(),
            tmp_root: root.join("tmp"),
            threads: 2,
            allow_dirty: true,
            allow_debug_build: true,
            local_tools: PathBuf::from("no-such-local-tools.toml"),
            tool_timeout: Duration::from_secs(60),
        };
        let out = execute(&cfg).expect("execute");
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(
            out.problems,
            0,
            "{:?}",
            crate::run::validate::validate_dir(&out.results_dir)
                .expect("validate")
                .problems
        );
        let json =
            std::fs::read_to_string(out.results_dir.join("probe-deflate.json")).expect("json");
        let env: Envelope<Data> = serde_json::from_str(&json).expect("typed");
        let names: Vec<&str> = env.data.classes.iter().map(|c| c.class.as_str()).collect();
        assert_eq!(names, CLASSES);
        let kinds: Vec<&str> = env
            .data
            .classes
            .iter()
            .flat_map(|c| c.files.iter().map(|f| f.kind.as_str()))
            .collect();
        assert_eq!(kinds, ["pdf", "office", "png", "gzip", "other"]);
        let md = std::fs::read_to_string(out.results_dir.join("probe-deflate.md")).expect("md");
        assert!(md.contains("## By container kind") && md.contains("## By class"));
        assert_eq!(
            md,
            crate::probe::render_file("deflate", &json).expect("render")
        );
        assert!(check(&env).is_empty(), "{:?}", check(&env));
        // Broken rules are reported.
        let mut bad = env.clone();
        bad.data.classes[0].files[0].streams.found += 1;
        bad.data.classes[1].manifest_files += 1;
        let p = check(&bad);
        assert!(p.iter().any(|m| m.contains("streams/found")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("indices")), "{p:?}");
    }
}
