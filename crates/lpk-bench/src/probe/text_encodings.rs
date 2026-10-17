//! `probe text-encodings` (PLAN E2-17 task P; design `docs/notes/e2-17-design.md`, section 1):
//! how much base64 and UTF-16 text each class holds, and what a writer applying the frozen
//! `base64` and `utf16` records would save against today's Balanced archive. No format change:
//! the transforms are `lpk_core::peel::text`.
//!
//! Per class, every file is read once (files above [`MAX_READ`] in blocks). Base64 is scanned
//! in `Text` files and text-like `Other` files (printable share at least
//! [`TEXT_LIKE_PRINTABLE`] over the first [`SAMPLE`] bytes): a candidate is a maximal run of at
//! least `MIN_RUN` characters over one RFC 4648 alphabet, labelled by what precedes it,
//! hex-only runs counted apart; the regularity rule sorts the rest into regular runs and
//! irregular causes; each regular run is decoded and re-encoded from its record's fields (a
//! mismatch fails the run). UTF-16 texts are detected in every file (a mark, or alternating
//! zero bytes) and converted to generalized UTF-8; UTF-16 runs inside binaries and base64 runs
//! in depth-1 inflated texts of Deflate containers are counted only.
//!
//! Costs, exact, with the Deflate stage's part encoder (`PartEncoder::balanced`, the tier's
//! LZMA against its zstd in the four-stripe trial): per stream M(text alone), M(payload), the
//! payload's length, the record's encoded length and the stage's `STREAM_OVERHEAD`. The stream
//! gate keeps a stream when `min(M(payload), payload length) + record + overhead` is below
//! M(text alone); the file gate (D-70) keeps a file when its kept streams together gain more
//! than [`FILE_GATE_MIN`] bytes and at least one part in [`FILE_SHARE`] of the file.
//!
//! Rows per block, on a real Balanced archive of the class (the default pipeline, written to
//! scratch, its index read back): **A** the block's frame as written, **A-reencoded** M of the
//! block's plain bytes (assembled from the files through the chunk table and checked against
//! every chunk's hash), **B-cut** the writer that would ship (the block without its kept
//! streams, split after each peeled file — D-70's cut — plus the streams' costs) and
//! **B-defer** the same without the split (design task W). A class's prediction is A of the
//! whole archive with each affected block's A-reencoded replaced by its B. UTF-16 per file:
//! A = M(the file), A' = the best of a grid of LZMA `lc`/`lp`/`pb` (no revision), B = M(the
//! generalized UTF-8) + record + overhead; per class the UTF-16 cluster's A, A' and B' (one
//! stream for the cluster block). The archive is built only when the class has a kept stream
//! or a UTF-16 text (otherwise every block is unchanged and the prediction equals A).
//!
//! Gate per primitive (owner question 1's recommendation): open when B-cut saves at least
//! [`MARGIN`] of a class's archive and no class is predicted larger.

use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use flate2::{Decompress, FlushDecompress, Status};
use lpk_core::peel::deflate::stage::STREAM_OVERHEAD;
use lpk_core::peel::deflate::{find_streams, PartEncoder};
use lpk_core::peel::text::{base64, utf16};
use lpk_core::{classify, BalancedOptions, Class, Features, LzmaEncoder, Pipeline};
use lpk_format::{
    Archive, Base64Record, BlockEncoder, EntryKind, EntryTable, FrameKind, FrameLocation, Graph,
    Record, RecordBody, RecordsWriter, Resources, Utf16Record,
};
use serde::{Deserialize, Serialize};

use super::{md_header, md_table, pct, Ctx, Envelope, Output};
use crate::corpus::manifest::ManifestFile;

pub const NAME: &str = "text-encodings";

/// The gate's margin: the saving B-cut must reach, as a share of a class's archive.
pub const MARGIN: f64 = 0.01;
/// Printable share from which an `Other` file is scanned as text.
pub const TEXT_LIKE_PRINTABLE: f64 = 0.9;
/// Bytes the classifier and the text-likeness look at.
pub const SAMPLE: usize = 64 << 10;
/// Files up to this size are read whole; larger ones are streamed (UTF-16 detection on the
/// first block, binary runs per block, no base64 scan).
pub const MAX_READ: u64 = 256 << 20;
/// Block size of the streamed read.
pub const BLOCK_READ: usize = 16 << 20;
/// Plain bytes one inflated stream may reach while counting depth-1 texts.
pub const INFLATE_CAP: usize = 32 << 20;
/// Plain bytes one container's streams may reach together.
pub const FILE_INFLATE_CAP: u64 = 256 << 20;
/// The file gate's absolute part (D-70): the kept streams must gain more than this.
pub const FILE_GATE_MIN: u64 = 256;
/// The file gate's share (D-70): the kept streams must gain at least the file's length over
/// this.
pub const FILE_SHARE: u64 = 32;
/// The most of a UTF-16 cluster the `lc`/`lp`/`pb` grid encodes.
pub const GRID_SAMPLE: usize = 16 << 20;

/// A named count of runs and their bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Named {
    pub id: String,
    pub runs: u64,
    pub bytes: u64,
}

/// One regular base64 run and its costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamRow {
    /// Manifest path (public corpora only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The file's index in the class (manifest order).
    pub file: u64,
    pub offset: u64,
    pub label: String,
    pub variant: String,
    pub line_len: u16,
    pub line_ending: String,
    pub padding: bool,
    /// The run's bytes.
    pub text_len: u64,
    pub payload_len: u64,
    pub payload_class: String,
    /// M(text alone).
    pub m_text: u64,
    /// M(payload).
    pub m_payload: u64,
    pub record_bytes: u64,
    pub overhead_bytes: u64,
    /// `min(m_payload, payload_len) + record_bytes + overhead_bytes`.
    pub stream_cost: u64,
    /// `stream_cost < m_text` (the stream gate).
    pub kept: bool,
}

impl StreamRow {
    fn cost(&self) -> u64 {
        self.m_payload
            .min(self.payload_len)
            .saturating_add(self.record_bytes)
            .saturating_add(self.overhead_bytes)
    }
}

/// One UTF-16 text and its costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Utf16FileRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub file: u64,
    pub bytes: u64,
    pub endian: String,
    pub bom: bool,
    /// An odd last byte stays framing.
    pub odd_tail: bool,
    pub wtf8_len: u64,
    pub code_points: u64,
    pub cjk_code_points: u64,
    /// A: M(the UTF-16 bytes alone).
    pub a: u64,
    /// A': the best `[lzma]` over the `lc`/`lp`/`pb` grid, and its parameters.
    pub a_prime: u64,
    pub a_prime_params: String,
    pub m_wtf8: u64,
    pub record_bytes: u64,
    pub overhead_bytes: u64,
    /// B: `m_wtf8 + record_bytes + overhead_bytes`.
    pub b: u64,
    /// `b < a` and the generalized UTF-8 is not longer than the text (the writer's rule).
    pub kept: bool,
}

/// The class's UTF-16 texts as one cluster (path order).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Utf16Cluster {
    pub files: u64,
    pub utf16_bytes: u64,
    pub wtf8_bytes: u64,
    pub code_points: u64,
    pub cjk_code_points: u64,
    /// A: M(the texts concatenated).
    pub a: u64,
    /// A': the grid's best on the first `a_prime_sample_bytes` of the concatenation.
    pub a_prime: u64,
    pub a_prime_params: String,
    pub a_prime_sample_bytes: u64,
    /// The sum of the per-file B rows.
    pub b_sum: u64,
    /// B': M(the generalized UTF-8 concatenated) + one record + overhead.
    pub b_prime: u64,
}

/// One block of the real archive that holds a kept item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockRow {
    pub block: u64,
    pub plain_len: u64,
    /// The block's frame as written.
    pub a_real: u64,
    /// M of the block's plain bytes.
    pub a_reencoded: u64,
    /// The segments after the cut, plus the streams.
    pub b_cut: u64,
    /// One segment, plus the streams.
    pub b_defer: u64,
    pub streams: u64,
    pub stream_bytes: u64,
    pub segments: u64,
}

/// The prediction of one primitive on a class's archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prediction {
    pub blocks: Vec<BlockRow>,
    /// Kept files whose chunks could not be placed in one block (deduplicated, split, or in a
    /// block that could not be assembled): left as they are.
    pub files_unmapped: u64,
    /// `a_total - sum(a_reencoded) + sum(b_cut)`.
    pub predicted_cut: u64,
    /// `a_total - sum(a_reencoded) + sum(b_defer)`.
    pub predicted_defer: u64,
}

/// The real Balanced archive of the class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveRows {
    pub archive_bytes: u64,
    pub blocks: u64,
    /// The sum of the blocks' frames.
    pub a_total: u64,
    pub base64: Prediction,
    pub utf16: Prediction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Base64Counts {
    pub files_scanned: u64,
    pub bytes_scanned: u64,
    pub candidates: u64,
    pub candidate_bytes: u64,
    pub hex_only: u64,
    pub hex_only_bytes: u64,
    pub by_label: Vec<Named>,
    pub regular: u64,
    pub regular_bytes: u64,
    pub irregular: Vec<Named>,
    /// Regular runs that pass the stream gate.
    pub kept_streams: u64,
    pub files_with_candidates: u64,
    /// Files with a kept stream that pass the file gate.
    pub kept_files: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inflated {
    pub containers: u64,
    pub streams: u64,
    pub text_streams: u64,
    pub runs: u64,
    pub run_bytes: u64,
    pub by_label: Vec<Named>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryUtf16 {
    pub files: u64,
    pub runs: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRun {
    pub class: String,
    pub present: bool,
    pub files: u64,
    pub bytes: u64,
    pub files_streamed: u64,
    pub base64: Base64Counts,
    pub streams: Vec<StreamRow>,
    pub inflated: Inflated,
    pub binary_utf16: BinaryUtf16,
    pub utf16_files: Vec<Utf16FileRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utf16_cluster: Option<Utf16Cluster>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveRows>,
    /// Why there is no archive row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_note: Option<String>,
    /// `a_total - predicted_cut` (0 without an archive).
    pub base64_saving: i64,
    pub base64_passes: bool,
    pub base64_larger: bool,
    pub utf16_saving: i64,
    pub utf16_passes: bool,
    pub utf16_larger: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub margin: f64,
    pub min_run: u64,
    pub stream_overhead: u64,
    pub base64_record_bytes: u64,
    pub utf16_record_bytes: u64,
    pub classes: Vec<ClassRun>,
    /// Regular runs whose re-encoding differed (counted under the `mismatch` cause too).
    pub mismatches: u64,
    pub base64_gate_open: bool,
    pub utf16_gate_open: bool,
}

// ---------------------------------------------------------------------------------------------
// Costs

/// What one record adds to the `Records` frame (D-37): its kind, flags, body length, body
/// and body hash, as the format's own writer lays them out.
fn record_bytes(record: Record) -> u64 {
    let empty = RecordsWriter::encode(&[]).len();
    (RecordsWriter::encode(&[record]).len() - empty) as u64
}

/// The exact cost of a base64 record in the `Records` frame (its body is fixed-width).
pub fn base64_record_bytes() -> u64 {
    record_bytes(Record::new(RecordBody::Base64(Base64Record {
        variant: 0,
        line_len: 0,
        line_ending: 2,
        padding: 1,
        original_len: 0,
        original_hash: [0; 32],
    })))
}

/// The exact cost of a utf16 record in the `Records` frame.
pub fn utf16_record_bytes() -> u64 {
    record_bytes(Record::new(RecordBody::Utf16(Utf16Record {
        endian: 0,
        bom: 0,
        original_len: 0,
        original_hash: [0; 32],
    })))
}

/// The model M: the Deflate stage's Balanced part encoder.
struct Model(PartEncoder);

impl Model {
    fn new() -> Result<Model> {
        let o = BalancedOptions::default();
        PartEncoder::balanced(&o, o.block_size)
            .map(Model)
            .map_err(|e| anyhow!("part encoder: {e}"))
    }

    fn m(&mut self, plain: &[u8]) -> Result<u64> {
        if plain.is_empty() {
            return Ok(0);
        }
        let e = self.0.encode(plain).map_err(|e| anyhow!("encoding: {e}"))?;
        Ok(e.encoded.bytes.len() as u64)
    }
}

/// The `lc`/`lp`/`pb` grid of A': every `lc <= 4`, `lp <= 2`, `pb <= 2` with `lc + lp <= 4`.
pub fn grid() -> Vec<(u8, u8, u8)> {
    let mut g = Vec::new();
    for lc in 0..=4u8 {
        for lp in 0..=2u8 {
            if lc + lp > 4 {
                continue;
            }
            for pb in 0..=2u8 {
                g.push((lc, lp, pb));
            }
        }
    }
    g
}

/// The smallest `[lzma]` over the grid on `plain` (the tier's dictionary), and its parameters.
fn best_lzma(plain: &[u8]) -> Result<(u64, String)> {
    let dict = BalancedOptions::default().dict_size;
    let mut best: Option<(u64, String)> = None;
    for (lc, lp, pb) in grid() {
        let mut enc = LzmaEncoder::new(dict, lc, lp, pb).map_err(|e| anyhow!("{e}"))?;
        let len = enc.encode(plain).map_err(|e| anyhow!("{e}"))?.bytes.len() as u64;
        if best.as_ref().is_none_or(|b| len < b.0) {
            best = Some((len, format!("lc{lc} lp{lp} pb{pb}")));
        }
    }
    best.ok_or_else(|| anyhow!("empty grid"))
}

// ---------------------------------------------------------------------------------------------
// The scan

/// What one file contributed.
#[derive(Debug, Default)]
struct FileScan {
    /// The regular runs (their rows carry the costs).
    streams: Vec<StreamRow>,
    /// The file passes the file gate.
    kept: bool,
    utf16: Option<Utf16FileRow>,
    /// The generalized UTF-8 of the UTF-16 text (kept for the cluster rows).
    wtf8: Vec<u8>,
}

#[derive(Debug, Default)]
struct Tally {
    labels: Vec<(String, u64, u64)>,
    causes: Vec<(String, u64, u64)>,
}

fn bump(v: &mut Vec<(String, u64, u64)>, id: &str, bytes: u64) {
    match v.iter_mut().find(|x| x.0 == id) {
        Some(x) => {
            x.1 += 1;
            x.2 += bytes;
        }
        None => v.push((id.to_string(), 1, bytes)),
    }
}

fn named(v: &[(String, u64, u64)], order: &[&str]) -> Vec<Named> {
    order
        .iter()
        .map(|id| {
            let (runs, bytes) = v.iter().find(|x| x.0 == *id).map_or((0, 0), |x| (x.1, x.2));
            Named {
                id: id.to_string(),
                runs,
                bytes,
            }
        })
        .collect()
}

/// The class and text-likeness of `bytes` from its first [`SAMPLE`] bytes.
fn text_like(bytes: &[u8]) -> (Class, bool) {
    let class = classify(bytes);
    let sample = &bytes[..bytes.len().min(SAMPLE)];
    let like = match class {
        Class::Text => true,
        Class::Other => Features::of(sample).printable_ratio >= TEXT_LIKE_PRINTABLE,
        _ => false,
    };
    (class, like)
}

/// Inflate a raw Deflate stream up to `cap` plain bytes (`None` when it does not inflate).
fn inflate_raw(data: &[u8], cap: usize) -> Option<Vec<u8>> {
    let mut d = Decompress::new(false);
    let mut out: Vec<u8> = Vec::with_capacity(data.len().min(cap).max(1024));
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let consumed = usize::try_from(d.total_in()).ok()?;
        let before = d.total_out();
        let st = d
            .decompress(
                &data[consumed.min(data.len())..],
                &mut buf,
                FlushDecompress::None,
            )
            .ok()?;
        let got = usize::try_from(d.total_out() - before).ok()?;
        if out.len() + got > cap {
            out.extend_from_slice(&buf[..cap - out.len()]);
            return Some(out);
        }
        out.extend_from_slice(&buf[..got]);
        match st {
            Status::StreamEnd => return Some(out),
            Status::BufError => return Some(out),
            Status::Ok => {
                if got == 0 && consumed >= data.len() {
                    return Some(out);
                }
            }
        }
    }
}

struct ClassScan {
    model: Model,
    b64_record: u64,
    u16_record: u64,
    files: Vec<FileScan>,
    base64: Base64Counts,
    tally: Tally,
    inflated: Inflated,
    inflated_tally: Vec<(String, u64, u64)>,
    binary: BinaryUtf16,
    files_streamed: u64,
    mismatches: u64,
}

/// Scan the base64 runs of `bytes` (a text-like file) into `scan` and the class's counts.
fn scan_base64(
    ctx: &Ctx<'_>,
    f: &ManifestFile,
    idx: usize,
    bytes: &[u8],
    out: &mut ClassScan,
    scan: &mut FileScan,
) -> Result<()> {
    let ClassScan {
        model,
        base64: counts,
        tally: t,
        mismatches,
        b64_record,
        ..
    } = out;
    let record_bytes = *b64_record;
    let runs = base64::find_runs(bytes);
    if !runs.is_empty() {
        counts.files_with_candidates += 1;
    }
    for run in runs {
        counts.candidates += 1;
        counts.candidate_bytes += run.len as u64;
        if run.hex_only {
            counts.hex_only += 1;
            counts.hex_only_bytes += run.len as u64;
            continue;
        }
        bump(&mut t.labels, run.label.label(), run.len as u64);
        let text = &bytes[run.offset..run.offset + run.len];
        let a = match base64::analyse(text) {
            Ok(a) => a,
            Err(cause) => {
                if cause == base64::Irregular::Mismatch {
                    *mismatches += 1;
                }
                bump(&mut t.causes, cause.label(), run.len as u64);
                continue;
            }
        };
        counts.regular += 1;
        counts.regular_bytes += run.len as u64;
        let m_text = model.m(text)?;
        let m_payload = model.m(&a.payload)?;
        let mut row = StreamRow {
            path: ctx.label(f),
            file: idx as u64,
            offset: run.offset as u64,
            label: run.label.label().to_string(),
            variant: a.layout.alphabet.label().to_string(),
            line_len: a.layout.line_len,
            line_ending: a
                .layout
                .line_ending
                .map_or("none", base64::LineEnding::label)
                .to_string(),
            padding: a.layout.padding,
            text_len: run.len as u64,
            payload_len: a.payload.len() as u64,
            payload_class: classify(&a.payload).as_str().to_string(),
            m_text,
            m_payload,
            record_bytes,
            overhead_bytes: STREAM_OVERHEAD,
            stream_cost: 0,
            kept: false,
        };
        row.stream_cost = row.cost();
        row.kept = row.stream_cost < row.m_text;
        if row.kept {
            counts.kept_streams += 1;
        }
        scan.streams.push(row);
    }
    Ok(())
}

/// The file gate (D-70) on a file's kept streams.
fn file_gate(streams: &[StreamRow], file_len: u64) -> bool {
    let gain: u64 = streams
        .iter()
        .filter(|s| s.kept)
        .map(|s| s.m_text - s.stream_cost)
        .sum();
    streams.iter().any(|s| s.kept) && gain > FILE_GATE_MIN && gain >= file_len.div_ceil(FILE_SHARE)
}

/// Count the base64 runs of the depth-1 inflated texts of a Deflate container.
fn scan_inflated(
    bytes: &[u8],
    ext: &str,
    inflated: &mut Inflated,
    t: &mut Vec<(String, u64, u64)>,
) {
    inflated.containers += 1;
    let found = find_streams(bytes, ext);
    let mut budget = FILE_INFLATE_CAP;
    for c in &found.candidates {
        inflated.streams += 1;
        if budget == 0 {
            break;
        }
        let data: Vec<u8> = c
            .spans
            .iter()
            .flat_map(|&(a, b)| bytes[a..b].iter().copied())
            .collect();
        let cap = usize::try_from(budget.min(INFLATE_CAP as u64)).unwrap_or(INFLATE_CAP);
        let Some(plain) = inflate_raw(&data, cap) else {
            continue;
        };
        budget = budget.saturating_sub(plain.len() as u64);
        let (_, like) = text_like(&plain);
        if !like {
            continue;
        }
        inflated.text_streams += 1;
        for run in base64::find_runs(&plain) {
            if run.hex_only {
                continue;
            }
            inflated.runs += 1;
            inflated.run_bytes += run.len as u64;
            bump(t, run.label.label(), run.len as u64);
        }
    }
}

/// The UTF-16 rows of a detected text.
fn utf16_rows(
    ctx: &Ctx<'_>,
    f: &ManifestFile,
    idx: usize,
    bytes: &[u8],
    d: utf16::Detected,
    model: &mut Model,
    record_bytes: u64,
) -> Result<(Utf16FileRow, Vec<u8>)> {
    let c = utf16::convert(bytes, d);
    let even = &bytes[..bytes.len() & !1];
    let a = model.m(even)?;
    let (a_prime, a_prime_params) = best_lzma(&even[..even.len().min(GRID_SAMPLE)])?;
    let m_wtf8 = model.m(&c.wtf8)?;
    let b = m_wtf8
        .saturating_add(record_bytes)
        .saturating_add(STREAM_OVERHEAD);
    let row = Utf16FileRow {
        path: ctx.label(f),
        file: idx as u64,
        bytes: even.len() as u64,
        endian: d.endian.label().to_string(),
        bom: d.bom,
        odd_tail: c.tail.is_some(),
        wtf8_len: c.wtf8.len() as u64,
        code_points: c.code_points,
        cjk_code_points: c.cjk_code_points,
        a,
        a_prime,
        a_prime_params,
        m_wtf8,
        record_bytes,
        overhead_bytes: STREAM_OVERHEAD,
        b,
        kept: b < a && c.wtf8.len() <= even.len(),
    };
    Ok((row, c.wtf8))
}

fn scan_class(ctx: &Ctx<'_>, class: &str, files: &[ManifestFile]) -> Result<ClassScan> {
    let mut out = ClassScan {
        model: Model::new()?,
        b64_record: base64_record_bytes(),
        u16_record: utf16_record_bytes(),
        files: Vec::with_capacity(files.len()),
        base64: Base64Counts {
            files_scanned: 0,
            bytes_scanned: 0,
            candidates: 0,
            candidate_bytes: 0,
            hex_only: 0,
            hex_only_bytes: 0,
            by_label: Vec::new(),
            regular: 0,
            regular_bytes: 0,
            irregular: Vec::new(),
            kept_streams: 0,
            files_with_candidates: 0,
            kept_files: 0,
        },
        tally: Tally::default(),
        inflated: Inflated {
            containers: 0,
            streams: 0,
            text_streams: 0,
            runs: 0,
            run_bytes: 0,
            by_label: Vec::new(),
        },
        inflated_tally: Vec::new(),
        binary: BinaryUtf16 {
            files: 0,
            runs: 0,
            bytes: 0,
        },
        files_streamed: 0,
        mismatches: 0,
    };
    for (idx, f) in files.iter().enumerate() {
        let mut scan = FileScan::default();
        if f.bytes > MAX_READ {
            // Streamed: too large to convert or scan whole, so only its UTF-16 runs are
            // counted, block by block (a run crossing a block boundary counts twice).
            out.files_streamed += 1;
            let mut runs = 0u64;
            let mut run_bytes = 0u64;
            ctx.read_blocks(class, f, BLOCK_READ, |block| {
                for (_, len) in utf16::find_binary_runs(block) {
                    runs += 1;
                    run_bytes += len as u64;
                }
                Ok(())
            })?;
            if runs > 0 {
                out.binary.files += 1;
                out.binary.runs += runs;
                out.binary.bytes += run_bytes;
            }
            out.files.push(scan);
            continue;
        }
        let bytes = ctx.read_file(class, f)?;
        let ext = f.path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if let Some(d) = utf16::detect(&bytes) {
            let (row, wtf8) = utf16_rows(ctx, f, idx, &bytes, d, &mut out.model, out.u16_record)?;
            scan.utf16 = Some(row);
            scan.wtf8 = wtf8;
        } else {
            let (class_of, like) = text_like(&bytes);
            if like {
                out.base64.files_scanned += 1;
                out.base64.bytes_scanned += bytes.len() as u64;
                scan_base64(ctx, f, idx, &bytes, &mut out, &mut scan)?;
                scan.kept = file_gate(&scan.streams, bytes.len() as u64);
                if scan.kept {
                    out.base64.kept_files += 1;
                }
            } else {
                let runs = utf16::find_binary_runs(&bytes);
                if !runs.is_empty() {
                    out.binary.files += 1;
                    out.binary.runs += runs.len() as u64;
                    out.binary.bytes += runs.iter().map(|r| r.1 as u64).sum::<u64>();
                }
                if class_of == Class::DeflateContainer {
                    scan_inflated(&bytes, &ext, &mut out.inflated, &mut out.inflated_tally);
                }
            }
        }
        out.files.push(scan);
    }
    let labels: Vec<&str> = base64::Label::ALL.iter().map(|l| l.label()).collect();
    let causes: Vec<&str> = base64::Irregular::ALL.iter().map(|c| c.label()).collect();
    out.base64.by_label = named(&out.tally.labels, &labels);
    out.base64.irregular = named(&out.tally.causes, &causes);
    out.inflated.by_label = named(&out.inflated_tally, &labels);
    Ok(out)
}

/// The cluster rows of a class's UTF-16 texts.
fn cluster_rows(
    scans: &[FileScan],
    files: &[ManifestFile],
    ctx: &Ctx<'_>,
    class: &str,
) -> Result<Option<Utf16Cluster>> {
    let rows: Vec<(&FileScan, &ManifestFile)> = scans
        .iter()
        .zip(files)
        .filter(|(s, _)| s.utf16.is_some())
        .collect();
    if rows.is_empty() {
        return Ok(None);
    }
    let mut model = Model::new()?;
    let mut utf16_all = Vec::new();
    let mut wtf8_all = Vec::new();
    let (mut cps, mut cjk, mut b_sum) = (0u64, 0u64, 0u64);
    for (s, f) in &rows {
        let bytes = ctx.read_file(class, f)?;
        utf16_all.extend_from_slice(&bytes[..bytes.len() & !1]);
        wtf8_all.extend_from_slice(&s.wtf8);
        if let Some(r) = &s.utf16 {
            cps += r.code_points;
            cjk += r.cjk_code_points;
            b_sum = b_sum.saturating_add(r.b);
        }
    }
    let a = model.m(&utf16_all)?;
    let sample = &utf16_all[..utf16_all.len().min(GRID_SAMPLE)];
    let (a_prime, a_prime_params) = best_lzma(sample)?;
    let b_prime = model
        .m(&wtf8_all)?
        .saturating_add(utf16_record_bytes())
        .saturating_add(STREAM_OVERHEAD);
    Ok(Some(Utf16Cluster {
        files: rows.len() as u64,
        utf16_bytes: utf16_all.len() as u64,
        wtf8_bytes: wtf8_all.len() as u64,
        code_points: cps,
        cjk_code_points: cjk,
        a,
        a_prime,
        a_prime_params,
        a_prime_sample_bytes: sample.len() as u64,
        b_sum,
        b_prime,
    }))
}

// ---------------------------------------------------------------------------------------------
// The real archive and the block rows

/// A kept item inside a placed file: a run (or the whole UTF-16 text) and its stream cost.
#[derive(Debug, Clone, Copy)]
struct Item {
    file: usize,
    offset: u64,
    len: u64,
    cost: u64,
}

/// A file whose chunks are its own, consecutive and contiguous in one block.
#[derive(Debug, Clone, Copy)]
struct Placed {
    block: usize,
    offset_in_block: u64,
    len: u64,
}

struct Mapped {
    archive_bytes: u64,
    /// Per block: the frame's length, the plain length, whether its graph reconstructs.
    blocks: Vec<(u64, u64, bool)>,
    /// Per chunk: the file (class index) and file offset it is first read from.
    chunk_owner: Vec<Option<(usize, u64)>>,
    chunk_hash: Vec<[u8; 32]>,
    chunk_len: Vec<u64>,
    first_chunk: Vec<u64>,
    placed: HashMap<usize, Placed>,
}

fn build_archive(ctx: &Ctx<'_>, class: &str) -> Result<PathBuf> {
    let root = ctx.corpus.files_root().join(class);
    let dir = ctx.scratch_dir(&format!("text-encodings-{class}"))?;
    let path = dir.join(format!("{class}.lpk"));
    let file =
        std::fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut out = BufWriter::new(file);
    Pipeline::balanced(BalancedOptions::default())
        .run(&root, &mut out)
        .map_err(|e| anyhow!("balanced archive of `{class}`: {e}"))?;
    out.flush()?;
    Ok(path)
}

fn map_archive(path: &PathBuf, by_path: &HashMap<String, usize>) -> Result<Mapped> {
    let file = std::fs::File::open(path)?;
    let archive_bytes = file.metadata()?.len();
    let mut archive = Archive::open(file, &Resources::default())
        .map_err(|e| anyhow!("opening {}: {e}", path.display()))?;
    let index = archive.index().clone();
    let mut blocks = Vec::with_capacity(index.blocks.len());
    for b in &index.blocks {
        let frame = archive
            .read_frame_at(
                FrameLocation {
                    offset: b.frame_offset,
                    len: b.frame_len,
                    sequence: b.sequence,
                },
                FrameKind::ChunkData,
            )
            .map_err(|e| anyhow!("reading a block: {e}"))?;
        let (graph, _) = Graph::parse(&frame.payload).map_err(|e| anyhow!("block graph: {e}"))?;
        let reconstruct = graph.steps.iter().any(|s| s.primitive.is_reconstruction());
        blocks.push((b.frame_len, b.plain_len, reconstruct));
    }
    let chunks = archive.chunks().clone();
    let n = usize::try_from(chunks.len()).unwrap_or(0);
    let mut chunk_owner: Vec<Option<(usize, u64)>> = vec![None; n];
    let mut chunk_hash = Vec::with_capacity(n);
    let mut chunk_len = Vec::with_capacity(n);
    for c in 0..chunks.len() {
        let r = chunks
            .record(c)
            .ok_or_else(|| anyhow!("chunk {c} missing"))?;
        chunk_hash.push(r.hash);
        chunk_len.push(r.plain_len);
    }
    let payload = archive.entries().map_err(|e| anyhow!("entries: {e}"))?;
    let table = EntryTable::parse(&payload).map_err(|e| anyhow!("entry table: {e}"))?;
    let mut placed = HashMap::new();
    for e in table.iter() {
        let e = e.map_err(|e| anyhow!("entry: {e}"))?;
        if e.kind != EntryKind::File || e.chunks.is_empty() {
            continue;
        }
        let Some(&file) = by_path.get(&e.path) else {
            bail!("archive entry `{}` is not in the manifest", e.path);
        };
        let mut off = 0u64;
        let mut own_all = true;
        let mut contiguous = true;
        let mut first: Option<lpk_format::ChunkPlace> = None;
        let mut prev_end = 0u64;
        let mut prev_chunk: Option<u64> = None;
        for &c in &e.chunks {
            let place = chunks
                .locate(c)
                .ok_or_else(|| anyhow!("chunk {c} of `{}` is not in the table", e.path))?;
            let i = usize::try_from(c)?;
            if chunk_owner[i].is_none() {
                chunk_owner[i] = Some((file, off));
            } else {
                own_all = false;
            }
            match first {
                None => first = Some(place),
                Some(f) => {
                    if place.block != f.block
                        || place.offset_in_block != prev_end
                        || prev_chunk.is_some_and(|p| p + 1 != c)
                    {
                        contiguous = false;
                    }
                }
            }
            prev_end = place.offset_in_block + place.plain_len;
            prev_chunk = Some(c);
            off += place.plain_len;
        }
        if let Some(f) = first {
            if own_all && contiguous && off == e.size && !blocks[f.block].2 {
                placed.insert(
                    file,
                    Placed {
                        block: f.block,
                        offset_in_block: f.offset_in_block,
                        len: e.size,
                    },
                );
            }
        }
    }
    Ok(Mapped {
        archive_bytes,
        blocks,
        chunk_owner,
        chunk_hash,
        chunk_len,
        first_chunk: index.blocks.iter().map(|b| b.first_chunk).collect(),
        placed,
    })
}

/// The plain bytes of block `b`, assembled from the files and checked chunk by chunk.
fn assemble_block(
    m: &Mapped,
    b: usize,
    read: &mut dyn FnMut(usize) -> Result<std::rc::Rc<Vec<u8>>>,
) -> Result<Option<Vec<u8>>> {
    let first = usize::try_from(m.first_chunk[b])?;
    let end = m
        .first_chunk
        .get(b + 1)
        .map_or(m.chunk_len.len(), |&f| usize::try_from(f).unwrap_or(0));
    let mut out = Vec::with_capacity(usize::try_from(m.blocks[b].1).unwrap_or(0));
    for c in first..end {
        let Some((file, off)) = m.chunk_owner[c] else {
            return Ok(None);
        };
        let bytes = read(file)?;
        let (start, len) = (usize::try_from(off)?, usize::try_from(m.chunk_len[c])?);
        let Some(piece) = bytes.get(start..start + len) else {
            return Ok(None);
        };
        if blake3::hash(piece).as_bytes() != &m.chunk_hash[c] {
            return Ok(None);
        }
        out.extend_from_slice(piece);
    }
    Ok(Some(out))
}

/// The block rows for `items` (sorted by file, then offset), per block.
fn predict(
    m: &Mapped,
    items: &[Item],
    kept_files: &[usize],
    model: &mut Model,
    read: &mut dyn FnMut(usize) -> Result<std::rc::Rc<Vec<u8>>>,
) -> Result<Prediction> {
    let a_total: u64 = m.blocks.iter().map(|b| b.0).sum();
    let mut per_block: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut unmapped = 0u64;
    for &f in kept_files {
        match m.placed.get(&f) {
            Some(p) => per_block.entry(p.block).or_default().push(f),
            None => unmapped += 1,
        }
    }
    let mut rows = Vec::new();
    let mut order: Vec<usize> = per_block.keys().copied().collect();
    order.sort_unstable();
    for b in order {
        let Some(plain) = assemble_block(m, b, read)? else {
            unmapped += per_block[&b].len() as u64;
            continue;
        };
        let mut files: Vec<(Placed, usize)> =
            per_block[&b].iter().map(|&f| (m.placed[&f], f)).collect();
        files.sort_by_key(|(p, _)| p.offset_in_block);
        let a_reencoded = model.m(&plain)?;
        let mut cur: Vec<u8> = Vec::new();
        let mut rest: Vec<u8> = Vec::new();
        let mut b_cut = 0u64;
        let mut segments = 0u64;
        let (mut streams, mut stream_bytes, mut stream_cost) = (0u64, 0u64, 0u64);
        let mut pos = 0usize;
        for (p, f) in &files {
            let start = usize::try_from(p.offset_in_block)?;
            let end = start + usize::try_from(p.len)?;
            cur.extend_from_slice(&plain[pos..start]);
            let mut at = start;
            for it in items.iter().filter(|i| i.file == *f) {
                let (s, e) = (
                    start + usize::try_from(it.offset)?,
                    start + usize::try_from(it.offset + it.len)?,
                );
                cur.extend_from_slice(&plain[at..s]);
                at = e;
                streams += 1;
                stream_bytes += it.len;
                stream_cost = stream_cost.saturating_add(it.cost);
            }
            cur.extend_from_slice(&plain[at..end]);
            rest.extend_from_slice(&cur);
            if !cur.is_empty() {
                b_cut = b_cut.saturating_add(model.m(&cur)?);
                segments += 1;
            }
            cur.clear();
            pos = end;
        }
        cur.extend_from_slice(&plain[pos..]);
        rest.extend_from_slice(&cur);
        if !cur.is_empty() {
            b_cut = b_cut.saturating_add(model.m(&cur)?);
            segments += 1;
        }
        let b_defer = model.m(&rest)?.saturating_add(stream_cost);
        rows.push(BlockRow {
            block: b as u64,
            plain_len: m.blocks[b].1,
            a_real: m.blocks[b].0,
            a_reencoded,
            b_cut: b_cut.saturating_add(stream_cost),
            b_defer,
            streams,
            stream_bytes,
            segments,
        });
    }
    let mut p = Prediction {
        blocks: rows,
        files_unmapped: unmapped,
        predicted_cut: 0,
        predicted_defer: 0,
    };
    let (cut, defer) = predicted(&p, a_total);
    p.predicted_cut = cut;
    p.predicted_defer = defer;
    Ok(p)
}

/// `a_total` with each block's A-reencoded replaced by its B.
fn predicted(p: &Prediction, a_total: u64) -> (u64, u64) {
    let re: u64 = p.blocks.iter().map(|b| b.a_reencoded).sum();
    let cut: u64 = p.blocks.iter().map(|b| b.b_cut).sum();
    let defer: u64 = p.blocks.iter().map(|b| b.b_defer).sum();
    (
        a_total.saturating_sub(re).saturating_add(cut),
        a_total.saturating_sub(re).saturating_add(defer),
    )
}

fn archive_rows(
    ctx: &Ctx<'_>,
    class: &str,
    files: &[ManifestFile],
    scans: &[FileScan],
) -> Result<ArchiveRows> {
    let path = build_archive(ctx, class)?;
    let by_path: HashMap<String, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (ctx.rel_path(class, f), i))
        .collect();
    let m = map_archive(&path, &by_path)?;
    let mut cache: HashMap<usize, std::rc::Rc<Vec<u8>>> = HashMap::new();
    let mut read = |i: usize| -> Result<std::rc::Rc<Vec<u8>>> {
        if let Some(b) = cache.get(&i) {
            return Ok(b.clone());
        }
        let b = std::rc::Rc::new(ctx.read_file(class, &files[i])?);
        cache.insert(i, b.clone());
        Ok(b)
    };
    let mut model = Model::new()?;
    let mut b64_items = Vec::new();
    let mut b64_files = Vec::new();
    let mut u16_items = Vec::new();
    let mut u16_files = Vec::new();
    for (i, s) in scans.iter().enumerate() {
        if s.kept {
            b64_files.push(i);
            for r in s.streams.iter().filter(|r| r.kept) {
                b64_items.push(Item {
                    file: i,
                    offset: r.offset,
                    len: r.text_len,
                    cost: r.stream_cost,
                });
            }
        }
        if let Some(u) = s.utf16.as_ref().filter(|u| u.kept) {
            u16_files.push(i);
            u16_items.push(Item {
                file: i,
                offset: 0,
                len: u.bytes,
                cost: u.b,
            });
        }
    }
    let base64 = predict(&m, &b64_items, &b64_files, &mut model, &mut read)?;
    let utf16 = predict(&m, &u16_items, &u16_files, &mut model, &mut read)?;
    let _ = std::fs::remove_file(&path);
    Ok(ArchiveRows {
        archive_bytes: m.archive_bytes,
        blocks: m.blocks.len() as u64,
        a_total: m.blocks.iter().map(|b| b.0).sum(),
        base64,
        utf16,
    })
}

// ---------------------------------------------------------------------------------------------
// The verdict

/// `(saving, passes, larger)` of one prediction.
fn verdict(a_total: u64, predicted: u64, margin: f64) -> (i64, bool, bool) {
    let saving =
        i64::try_from(a_total).unwrap_or(i64::MAX) - i64::try_from(predicted).unwrap_or(i64::MAX);
    let passes = a_total > 0 && saving > 0 && (saving as f64) >= margin * (a_total as f64);
    (saving, passes, predicted > a_total)
}

fn apply_verdict(c: &mut ClassRun, margin: f64) {
    let (b, u) = match &c.archive {
        Some(a) => (
            verdict(a.a_total, a.base64.predicted_cut, margin),
            verdict(a.a_total, a.utf16.predicted_cut, margin),
        ),
        None => ((0, false, false), (0, false, false)),
    };
    (c.base64_saving, c.base64_passes, c.base64_larger) = b;
    (c.utf16_saving, c.utf16_passes, c.utf16_larger) = u;
}

fn gate(
    classes: &[ClassRun],
    passes: impl Fn(&ClassRun) -> bool,
    larger: impl Fn(&ClassRun) -> bool,
) -> bool {
    classes.iter().any(&passes) && !classes.iter().any(&larger)
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let mut classes = Vec::new();
    let mut notes = Vec::new();
    let mut mismatches = 0u64;
    let names: Vec<String> = ctx.corpus.manifest.classes.keys().cloned().collect();
    for class in &names {
        let files = ctx.class_files(class).unwrap_or(&[]);
        let scan = scan_class(ctx, class, files).with_context(|| format!("class `{class}`"))?;
        mismatches += scan.mismatches;
        let utf16_cluster = cluster_rows(&scan.files, files, ctx, class)?;
        let needs_archive = scan.base64.kept_files > 0
            || scan
                .files
                .iter()
                .any(|f| f.utf16.as_ref().is_some_and(|u| u.kept));
        let (archive, archive_note) = if !needs_archive {
            (
                None,
                Some("no kept stream: every block is unchanged".to_string()),
            )
        } else if ctx.private() || !ctx.corpus.files_root().join(class).is_dir() {
            notes.push(format!(
                "class `{class}`: no class folder to archive (a private corpus); no block rows"
            ));
            (None, Some("no class folder to archive".to_string()))
        } else {
            (Some(archive_rows(ctx, class, files, &scan.files)?), None)
        };
        let mut c = ClassRun {
            class: class.clone(),
            present: !files.is_empty(),
            files: files.len() as u64,
            bytes: files.iter().map(|f| f.bytes).sum(),
            files_streamed: scan.files_streamed,
            base64: scan.base64,
            streams: scan
                .files
                .iter()
                .flat_map(|f| f.streams.iter().cloned())
                .collect(),
            inflated: scan.inflated,
            binary_utf16: scan.binary,
            utf16_files: scan.files.iter().filter_map(|f| f.utf16.clone()).collect(),
            utf16_cluster,
            archive,
            archive_note,
            base64_saving: 0,
            base64_passes: false,
            base64_larger: false,
            utf16_saving: 0,
            utf16_passes: false,
            utf16_larger: false,
        };
        apply_verdict(&mut c, MARGIN);
        classes.push(c);
    }
    let base64_gate_open = gate(&classes, |c| c.base64_passes, |c| c.base64_larger);
    let utf16_gate_open = gate(&classes, |c| c.utf16_passes, |c| c.utf16_larger);
    let mut out = Output::new(
        Data {
            margin: MARGIN,
            min_run: base64::MIN_RUN as u64,
            stream_overhead: STREAM_OVERHEAD,
            base64_record_bytes: base64_record_bytes(),
            utf16_record_bytes: utf16_record_bytes(),
            classes,
            mismatches,
            base64_gate_open,
            utf16_gate_open,
        },
        1,
    )
    .with_zstd()
    .with_xz();
    for n in notes {
        out = out.note(n);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Validation and rendering

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    if !(d.margin > 0.0 && d.margin < 1.0) {
        p.push("/data/margin: must be a share between 0 and 1".to_string());
    }
    let labels: Vec<&str> = base64::Label::ALL.iter().map(|l| l.label()).collect();
    let causes: Vec<&str> = base64::Irregular::ALL.iter().map(|c| c.label()).collect();
    let mut mismatches = 0u64;
    for (i, c) in d.classes.iter().enumerate() {
        let at = format!("/data/classes/{i}");
        let b = &c.base64;
        let ids = |v: &[Named]| v.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
        if ids(&b.by_label) != labels || ids(&c.inflated.by_label) != labels {
            p.push(format!("{at}: by_label must list {labels:?}"));
        }
        if ids(&b.irregular) != causes {
            p.push(format!("{at}/base64/irregular: must list {causes:?}"));
        }
        let irregular: u64 = b.irregular.iter().map(|n| n.runs).sum();
        if b.hex_only + b.regular + irregular != b.candidates {
            p.push(format!(
                "{at}/base64: hex_only + regular + irregular runs must equal candidates"
            ));
        }
        if b.regular != c.streams.len() as u64 {
            p.push(format!("{at}/streams: one row per regular run"));
        }
        mismatches += b
            .irregular
            .iter()
            .find(|n| n.id == "mismatch")
            .map_or(0, |n| n.runs);
        let mut kept = 0u64;
        for (j, s) in c.streams.iter().enumerate() {
            let at = format!("{at}/streams/{j}");
            if s.stream_cost != s.cost() || s.kept != (s.stream_cost < s.m_text) {
                p.push(format!(
                    "{at}: stream_cost and kept must follow from the costs"
                ));
            }
            if s.overhead_bytes != d.stream_overhead || s.record_bytes != d.base64_record_bytes {
                p.push(format!(
                    "{at}: record and overhead bytes must be the data's"
                ));
            }
            if e.corpus.private && s.path.is_some() {
                p.push(format!("{at}/path: a private corpus carries no names"));
            }
            kept += u64::from(s.kept);
        }
        if kept != b.kept_streams {
            p.push(format!(
                "{at}/base64/kept_streams: must count the kept rows"
            ));
        }
        for (j, u) in c.utf16_files.iter().enumerate() {
            let at = format!("{at}/utf16_files/{j}");
            let b = u
                .m_wtf8
                .saturating_add(u.record_bytes)
                .saturating_add(u.overhead_bytes);
            if u.b != b || u.kept != (u.b < u.a && u.wtf8_len <= u.bytes) {
                p.push(format!("{at}: b and kept must follow from the costs"));
            }
            if u.record_bytes != d.utf16_record_bytes || u.overhead_bytes != d.stream_overhead {
                p.push(format!(
                    "{at}: record and overhead bytes must be the data's"
                ));
            }
            if u.a_prime > u.a {
                p.push(format!(
                    "{at}/a_prime: the grid holds the default parameters, so it cannot exceed a"
                ));
            }
        }
        if let Some(cl) = &c.utf16_cluster {
            if cl.files != c.utf16_files.len() as u64 {
                p.push(format!("{at}/utf16_cluster/files: must count utf16_files"));
            }
            if cl.b_sum
                != c.utf16_files
                    .iter()
                    .fold(0u64, |a, u| a.saturating_add(u.b))
            {
                p.push(format!("{at}/utf16_cluster/b_sum: must sum the per-file b"));
            }
        } else if !c.utf16_files.is_empty() {
            p.push(format!(
                "{at}/utf16_cluster: present when there are UTF-16 texts"
            ));
        }
        if let Some(a) = &c.archive {
            if a.blocks < a.base64.blocks.len() as u64 || a.blocks < a.utf16.blocks.len() as u64 {
                p.push(format!("{at}/archive: more block rows than blocks"));
            }
            for (name, pr) in [("base64", &a.base64), ("utf16", &a.utf16)] {
                let (cut, defer) = predicted(pr, a.a_total);
                if pr.predicted_cut != cut || pr.predicted_defer != defer {
                    p.push(format!(
                        "{at}/archive/{name}: the predictions must follow from the block rows"
                    ));
                }
                for (k, b) in pr.blocks.iter().enumerate() {
                    if b.streams == 0 {
                        p.push(format!(
                            "{at}/archive/{name}/blocks/{k}: a block row has streams"
                        ));
                    }
                }
            }
        }
        let mut again = c.clone();
        apply_verdict(&mut again, d.margin);
        if (
            again.base64_saving,
            again.base64_passes,
            again.base64_larger,
        ) != (c.base64_saving, c.base64_passes, c.base64_larger)
            || (again.utf16_saving, again.utf16_passes, again.utf16_larger)
                != (c.utf16_saving, c.utf16_passes, c.utf16_larger)
        {
            p.push(format!(
                "{at}: saving, passes and larger must follow from the archive rows"
            ));
        }
    }
    if mismatches != d.mismatches {
        p.push("/data/mismatches: must equal the `mismatch` cause over the classes".to_string());
    }
    if d.base64_gate_open != gate(&d.classes, |c| c.base64_passes, |c| c.base64_larger) {
        p.push(
            "/data/base64_gate_open: must be true exactly when a class passes and none is larger"
                .to_string(),
        );
    }
    if d.utf16_gate_open != gate(&d.classes, |c| c.utf16_passes, |c| c.utf16_larger) {
        p.push(
            "/data/utf16_gate_open: must be true exactly when a class passes and none is larger"
                .to_string(),
        );
    }
    p
}

fn opt(v: Option<u64>) -> String {
    v.map_or("-".to_string(), |x| x.to_string())
}

/// The tables, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    s.push_str(&format!(
        "E2-17's probe: base64 runs (at least {} characters over one RFC 4648 alphabet) in text \
         files and UTF-16 texts in every file, priced with the Deflate stage's Balanced part \
         encoder; a stream costs min(M(payload), payload) + record ({} bytes base64, {} utf16) + \
         {} overhead and is kept when that is below M(text alone); a file is kept when its kept \
         streams gain more than {} bytes and at least one part in {} of it. A is a real Balanced \
         archive of the class; B-cut re-encodes each affected block without its kept streams, \
         split after each peeled file; B-defer without the split. The gate of a primitive opens \
         when B-cut saves at least {} of a class's archive and no class is predicted larger.\n\n",
        d.min_run,
        d.base64_record_bytes,
        d.utf16_record_bytes,
        d.stream_overhead,
        FILE_GATE_MIN,
        FILE_SHARE,
        pct((d.margin * 1_000_000.0) as u64, 1_000_000)
    ));
    let mut rows = Vec::new();
    for c in &d.classes {
        let b = &c.base64;
        let a = c.archive.as_ref();
        rows.push(vec![
            c.class.clone(),
            c.files.to_string(),
            b.files_scanned.to_string(),
            format!("{} ({})", b.candidates, b.candidate_bytes),
            b.hex_only.to_string(),
            format!("{} ({})", b.regular, b.regular_bytes),
            b.irregular.iter().map(|n| n.runs).sum::<u64>().to_string(),
            format!("{} / {}", b.kept_streams, b.kept_files),
            opt(a.map(|a| a.a_total)),
            opt(a.map(|a| a.base64.predicted_cut)),
            opt(a.map(|a| a.base64.predicted_defer)),
            match a {
                Some(a) => format!(
                    "{} ({})",
                    c.base64_saving,
                    pct(c.base64_saving.unsigned_abs(), a.a_total)
                ),
                None => c.archive_note.clone().unwrap_or_default(),
            },
            if c.base64_passes {
                "passes"
            } else if c.base64_larger {
                "LARGER"
            } else {
                "no"
            }
            .to_string(),
        ]);
    }
    s.push_str("Base64 per class:\n\n");
    s.push_str(&md_table(
        &[
            "class",
            "files",
            "text files",
            "candidates (bytes)",
            "hex-only",
            "regular (bytes)",
            "irregular",
            "kept streams / files",
            "A",
            "B-cut",
            "B-defer",
            "saving",
            "gate",
        ],
        &rows,
    ));
    s.push_str("\nBase64 runs by what precedes them (runs, bytes; inflated: in depth-1 texts of Deflate containers, counted only):\n\n");
    let mut rows = Vec::new();
    for c in &d.classes {
        if c.base64.candidates == 0 && c.inflated.runs == 0 {
            continue;
        }
        let mut row = vec![c.class.clone()];
        for n in &c.base64.by_label {
            row.push(format!("{} ({})", n.runs, n.bytes));
        }
        row.push(format!("{} ({})", c.inflated.runs, c.inflated.run_bytes));
        rows.push(row);
    }
    let mut headers: Vec<&str> = vec!["class"];
    for l in &base64::Label::ALL {
        headers.push(l.label());
    }
    headers.push("inflated");
    s.push_str(&md_table(&headers, &rows));
    s.push_str("\nIrregular runs by cause (runs):\n\n");
    let mut rows = Vec::new();
    for c in &d.classes {
        if c.base64.irregular.iter().all(|n| n.runs == 0) {
            continue;
        }
        let mut row = vec![c.class.clone()];
        for n in &c.base64.irregular {
            row.push(n.runs.to_string());
        }
        rows.push(row);
    }
    let mut headers: Vec<&str> = vec!["class"];
    for cause in &base64::Irregular::ALL {
        headers.push(cause.label());
    }
    s.push_str(&md_table(&headers, &rows));
    s.push_str("\nStreams (regular runs; the first rows per class):\n\n");
    let mut rows = Vec::new();
    for c in &d.classes {
        for r in c.streams.iter().take(40) {
            rows.push(vec![
                c.class.clone(),
                r.path.clone().unwrap_or_else(|| format!("file {}", r.file)),
                r.offset.to_string(),
                r.label.clone(),
                format!(
                    "{} {} {} {}",
                    r.variant,
                    r.line_len,
                    r.line_ending,
                    if r.padding { "pad" } else { "nopad" }
                ),
                r.text_len.to_string(),
                format!("{} ({})", r.payload_len, r.payload_class),
                r.m_text.to_string(),
                r.m_payload.to_string(),
                r.stream_cost.to_string(),
                if r.kept { "kept" } else { "no" }.to_string(),
            ]);
        }
        if c.streams.len() > 40 {
            rows.push(vec![
                c.class.clone(),
                format!("... {} more", c.streams.len() - 40),
            ]);
        }
    }
    s.push_str(&md_table(
        &[
            "class",
            "file",
            "offset",
            "label",
            "layout",
            "text",
            "payload (class)",
            "M(text)",
            "M(payload)",
            "stream cost",
            "gate",
        ],
        &rows,
    ));
    s.push_str("\nBlocks (A as written, A re-encoded, B-cut, B-defer):\n\n");
    let mut rows = Vec::new();
    for c in &d.classes {
        if let Some(a) = &c.archive {
            for (name, pr) in [("base64", &a.base64), ("utf16", &a.utf16)] {
                for b in &pr.blocks {
                    rows.push(vec![
                        c.class.clone(),
                        name.to_string(),
                        b.block.to_string(),
                        b.plain_len.to_string(),
                        b.a_real.to_string(),
                        b.a_reencoded.to_string(),
                        b.b_cut.to_string(),
                        b.b_defer.to_string(),
                        format!("{} ({})", b.streams, b.stream_bytes),
                        b.segments.to_string(),
                    ]);
                }
                if pr.files_unmapped > 0 {
                    rows.push(vec![
                        c.class.clone(),
                        name.to_string(),
                        format!("{} kept files not placed", pr.files_unmapped),
                    ]);
                }
            }
        }
    }
    s.push_str(&md_table(
        &[
            "class",
            "primitive",
            "block",
            "plain",
            "A",
            "A re-encoded",
            "B-cut",
            "B-defer",
            "streams (bytes)",
            "segments",
        ],
        &rows,
    ));
    s.push_str("\nUTF-16 per class (texts; runs inside binaries counted only; cluster rows A, A' (grid), B (per file), B' (one stream)):\n\n");
    let mut rows = Vec::new();
    for c in &d.classes {
        if c.utf16_files.is_empty() && c.binary_utf16.runs == 0 {
            continue;
        }
        let cl = c.utf16_cluster.as_ref();
        rows.push(vec![
            c.class.clone(),
            c.utf16_files.len().to_string(),
            opt(cl.map(|x| x.utf16_bytes)),
            cl.map_or("-".to_string(), |x| pct(x.cjk_code_points, x.code_points)),
            c.utf16_files.iter().filter(|u| u.kept).count().to_string(),
            opt(cl.map(|x| x.a)),
            cl.map_or("-".to_string(), |x| {
                format!("{} ({})", x.a_prime, x.a_prime_params)
            }),
            opt(cl.map(|x| x.b_sum)),
            opt(cl.map(|x| x.b_prime)),
            format!(
                "{} / {} ({})",
                c.binary_utf16.files, c.binary_utf16.runs, c.binary_utf16.bytes
            ),
            c.archive.as_ref().map_or("-".to_string(), |a| {
                format!("{} -> {}", a.a_total, a.utf16.predicted_cut)
            }),
            if c.utf16_passes {
                "passes"
            } else if c.utf16_larger {
                "LARGER"
            } else {
                "no"
            }
            .to_string(),
        ]);
    }
    s.push_str(&md_table(
        &[
            "class",
            "texts",
            "bytes",
            "cjk",
            "kept",
            "A",
            "A'",
            "B",
            "B'",
            "binary files / runs (bytes)",
            "A -> B-cut",
            "gate",
        ],
        &rows,
    ));
    s.push_str(&format!(
        "\nRe-encode mismatches: {}\n\nGate base64: {}\n\nGate utf16: {}\n",
        d.mismatches,
        if d.base64_gate_open { "OPEN" } else { "CLOSED" },
        if d.utf16_gate_open { "OPEN" } else { "CLOSED" },
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_lengths_are_the_designs() {
        // D-37's frame fields (kind 2, flags 2, body_len 1, body_hash 32) around the
        // fixed-width bodies (45 and 42); the design's figures agree.
        assert_eq!(base64_record_bytes(), 2 + 2 + 1 + 45 + 32);
        assert_eq!(utf16_record_bytes(), 2 + 2 + 1 + 42 + 32);
    }

    #[test]
    fn the_grid_holds_the_default_and_respects_the_bound() {
        let g = grid();
        assert!(g.contains(&(3, 0, 2)));
        assert!(g.iter().all(|&(lc, lp, _)| lc + lp <= 4));
        assert_eq!(g.len(), 36);
    }

    #[test]
    fn the_stream_gate_file_gate_and_verdict_follow_the_rules() {
        let row = |m_text: u64, m_payload: u64, payload_len: u64| {
            let mut r = StreamRow {
                path: None,
                file: 0,
                offset: 0,
                label: "bare".into(),
                variant: "standard".into(),
                line_len: 0,
                line_ending: "none".into(),
                padding: true,
                text_len: payload_len * 4 / 3,
                payload_len,
                payload_class: "other".into(),
                m_text,
                m_payload,
                record_bytes: 82,
                overhead_bytes: 160,
                stream_cost: 0,
                kept: false,
            };
            r.stream_cost = r.cost();
            r.kept = r.stream_cost < r.m_text;
            r
        };
        // The payload's stored length counts when it is smaller than M(payload).
        let r = row(1000, 900, 700);
        assert_eq!(r.stream_cost, 700 + 82 + 160);
        assert!(r.kept);
        assert!(!row(900, 800, 700).kept);
        // The file gate: more than the minimum and the share.
        assert!(file_gate(&[row(1000, 500, 700)], 1000));
        assert!(!file_gate(&[row(1000, 500, 700)], 1_000_000));
        assert!(!file_gate(&[row(1000, 900, 700)], 100));
        assert!(!file_gate(&[], 100));
        // The verdict.
        assert_eq!(verdict(10_000, 9_900, 0.01), (100, true, false));
        assert_eq!(verdict(10_000, 9_901, 0.01), (99, false, false));
        assert_eq!(verdict(10_000, 10_001, 0.01), (-1, false, true));
        assert_eq!(verdict(0, 0, 0.01), (0, false, false));
    }

    #[test]
    fn inflating_counts_the_runs_of_a_zipped_text() {
        let text = format!(
            "header\n{}\n",
            String::from_utf8(
                base64::Layout {
                    alphabet: base64::Alphabet::Standard,
                    line_len: 76,
                    line_ending: Some(base64::LineEnding::Lf),
                    padding: true,
                }
                .encode(&vec![7u8; 900])
            )
            .expect("test value")
        );
        let mut raw = Vec::new();
        {
            let mut enc =
                flate2::write::DeflateEncoder::new(&mut raw, flate2::Compression::default());
            enc.write_all(text.as_bytes()).expect("test value");
            enc.finish().expect("test value");
        }
        assert_eq!(
            inflate_raw(&raw, 1 << 20).expect("test value"),
            text.as_bytes()
        );
        assert_eq!(inflate_raw(&raw, 10).expect("test value").len(), 10);
        assert!(inflate_raw(b"\xff\xff\xff", 100).is_none());
        let mut zip = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut zip));
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("a.txt", o).expect("test value");
            w.write_all(text.as_bytes()).expect("test value");
            w.finish().expect("test value");
        }
        let mut inflated = Inflated {
            containers: 0,
            streams: 0,
            text_streams: 0,
            runs: 0,
            run_bytes: 0,
            by_label: Vec::new(),
        };
        let mut t = Vec::new();
        scan_inflated(&zip, "zip", &mut inflated, &mut t);
        assert_eq!(
            (
                inflated.containers,
                inflated.streams,
                inflated.text_streams,
                inflated.runs
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(t, [("bare".to_string(), 1, inflated.run_bytes)]);
    }
}
