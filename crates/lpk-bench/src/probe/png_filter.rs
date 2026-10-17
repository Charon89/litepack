//! `probe png-filter` (PLAN E2-16, task P; design `docs/notes/e2-16-design.md`, section 1): the
//! measured gate of the PNG row-filter undo, with no format change.
//!
//! Per file of walker kind `png` of the Deflate probe's classes (above the stage's `MAX_FILE`
//! skipped, as the stage does), `lpk_core::peel::deflate::examine` with `Limits::standard()`
//! yields the streams the 1.2 Deflate peel reconstructs, with inflated text and corrections.
//! Each eligible stream (IHDR first; compression and filter method 0; a legal pair; width and
//! height at least 1; the filtered length equal to the inflated length; every filter byte at
//! most 4) is undone with `lpk_core::peel::png` and re-filtered: a text that does not come back
//! byte for byte fails the run (filtering is fully specified, so a mismatch is a bug).
//!
//! What is compressed, each column one part through the stage's own Balanced part encoder
//! (`PartEncoder::balanced(&BalancedOptions::default(), PLAIN_TEXT_LIMIT)`: the four-stripe
//! trial between the tier's LZMA and zstd, the winner encoding the whole text):
//! * `A`: the inflated text, filtered rows (what the stage writes today);
//! * `B(raw)`: the raw rows without filter bytes, Adam7 in pass order;
//! * `B(sub|up|average|paeth)`: the raw rows re-filtered with one filter throughout;
//! * `B(raster)`: Adam7 images de-interlaced (the raw rows for the rest).
//!
//! Side information of an undone stream, exact: the `png-filter` record as the `Records` frame
//! holds it (`lpk_format::PngFilterRecord` through `stage::record_len`) plus the third graph
//! step, counted as [`STEP_BYTES`]. The stage's gate rules apply per stream (M plus the Deflate
//! record plus `STREAM_OVERHEAD`, plus the side information when undone, kept only below the
//! stream's length) and per file (gain over `FILE_GATE`, at least `1 / FILE_SHARE` of the file).
//! Three writers per layout: today (`A`), always (`B` for every eligible stream), per stream
//! (the cheaper); their class totals predict the row differences up to varint widths.
//!
//! Sizes only: the files run under `par_map`, the workers capped so that their encoders'
//! working sets fit half the physical memory; the undo and re-filter seconds are measured
//! inside that pass (informative, not a timed section of the probe's rules).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use lpk_core::peel::deflate::stage::{
    record_len, FILE_GATE, FILE_SHARE, MAX_EXPANSION, MAX_FILE, STREAM_OVERHEAD,
};
use lpk_core::peel::deflate::{examine, Limits, PartEncoder, PartModel, Plain, PLAIN_TEXT_LIMIT};
use lpk_core::peel::png::{
    Header, Layout, PngError, FILTER_NAMES, FILTER_PAETH, FILTER_SUB, SIGNATURE,
};
use lpk_core::BalancedOptions;
use lpk_format::{DeflateRecord, PngFilterRecord, Record, RecordBody};
use serde::{Deserialize, Serialize};

use super::{mbps, md_header, md_table, par_map, pct, timed, Ctx, Envelope, Output};

pub const NAME: &str = "png-filter";

/// The classes, in table order (the Deflate probe's).
pub const CLASSES: [&str; 5] = super::deflate::CLASSES;

/// The class whose per-stream saving decides the gate.
pub const GATE_CLASS: &str = "photo-raw-png";

/// The layouts of the undone rows, in table order: id and what it is.
pub const LAYOUTS: [(&str, &str); 6] = [
    ("raw", "raw rows, no filter bytes (Adam7 in pass order)"),
    ("sub", "raw rows re-filtered with Sub throughout"),
    ("up", "raw rows re-filtered with Up throughout"),
    ("average", "raw rows re-filtered with Average throughout"),
    ("paeth", "raw rows re-filtered with Paeth throughout"),
    ("raster", "Adam7 images de-interlaced (raw rows otherwise)"),
];

/// The bytes counted for the third graph step (`png-filter {record}`): an upper bound for
/// record ids below 2^21.
pub const STEP_BYTES: u64 = 7;

/// Folder levels inside a class that make a source group (Kenney, Kodak, converted photos).
pub const GROUP_DEPTH: usize = 1;

/// The settings the probe ran with, all the stage's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub stream_overhead: u64,
    pub file_gate: u64,
    pub file_share: u64,
    pub step_bytes: u64,
    pub max_file: u64,
    pub plain_text_limit: u64,
    pub dict_size: u32,
    pub zstd_level: i32,
    pub sample_len: u64,
    /// Files examined and encoded at once.
    pub workers: u32,
}

/// One undone layout's encoding of a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub layout: String,
    /// `lzma` or `zstd`.
    pub model: String,
    pub bytes: u64,
}

/// The image of an eligible stream and the side information its undo costs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_type: u8,
    pub interlace: u8,
    pub scanlines: u64,
    /// Bytes of the raw rows.
    pub raw_bytes: u64,
    /// The `png-filter` record in the `Records` frame plus the graph step.
    pub side_bytes: u64,
    /// Scanlines per filter type (None, Sub, Up, Average, Paeth).
    pub filter_counts: [u64; 5],
}

impl Geometry {
    /// A key for the per-geometry tables.
    pub fn label(&self) -> String {
        format!(
            "ct{}-d{}-i{}",
            self.color_type, self.bit_depth, self.interlace
        )
    }

    /// Every scanline uses filter None.
    pub fn all_none(&self) -> bool {
        self.filter_counts[1..].iter().all(|n| *n == 0)
    }

    /// A palette image or a bit depth below 8, where the specification prefers filter None.
    pub fn low_depth(&self) -> bool {
        self.color_type == 3 || self.bit_depth < 8
    }
}

/// One reconstructed stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    /// IDAT chunks (spans) the stream covers.
    pub spans: u32,
    /// The stream's Deflate bytes.
    pub len: u64,
    /// The inflated text (the filtered rows).
    pub plain_len: u64,
    /// The `deflate-reconstruct` record in the `Records` frame.
    pub record_bytes: u64,
    /// `A`: M's bytes on the filtered rows, and the model that won.
    pub a_bytes: u64,
    pub a_model: String,
    /// Why the stream cannot be undone (`None` when eligible).
    pub ineligible: Option<String>,
    pub geometry: Option<Geometry>,
    /// `B(layout)` for every layout (`raster` only when interlaced).
    pub candidates: Vec<Candidate>,
    pub undo_seconds: f64,
    pub refilter_seconds: f64,
}

impl Stream {
    /// Today's cost of the stream as the stage writes it, when kept.
    pub fn today_cost(&self, set: &Settings) -> Option<u64> {
        let cost = self
            .a_bytes
            .saturating_add(self.record_bytes)
            .saturating_add(set.stream_overhead);
        (cost < self.len).then_some(cost)
    }

    /// The cost of the stream undone in `layout`, when eligible and kept.
    pub fn undone_cost(&self, layout: &str, set: &Settings) -> Option<u64> {
        let g = self.geometry.as_ref()?;
        let b = self.candidate(layout)?;
        let cost = b
            .bytes
            .saturating_add(self.record_bytes)
            .saturating_add(g.side_bytes)
            .saturating_add(set.stream_overhead);
        (cost < self.len).then_some(cost)
    }

    /// `B(layout)`: `raster` falls back to `raw` for a stream that is not interlaced.
    pub fn candidate(&self, layout: &str) -> Option<&Candidate> {
        self.candidates
            .iter()
            .find(|c| c.layout == layout)
            .or_else(|| {
                (layout == "raster")
                    .then(|| self.candidates.iter().find(|c| c.layout == "raw"))
                    .flatten()
            })
    }
}

/// One file of a class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    /// Index in the class's manifest list.
    pub index: u32,
    /// The manifest path; absent for a private corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub bytes: u64,
    /// `png`, `not_png` (another signature: not walked) or `size_cap` (above `max_file`).
    pub kind: String,
    pub streams: Vec<Stream>,
}

/// The three writers under one layout: the class's bytes (every file's length
/// less its kept gain), and what each undoes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Writers {
    pub layout: String,
    /// `A` for every stream (the stage today).
    pub today: u64,
    /// `B` for every eligible stream, `A` for the rest.
    pub always: u64,
    /// The cheaper of the two per stream.
    pub per_stream: u64,
    /// Streams undone and kept by the `always` writer.
    pub always_undone: u64,
    /// Streams undone and kept by the per-stream writer, and of those the ones the cheap
    /// rules would name (all filters None; a palette image or a depth below 8).
    pub per_stream_undone: u64,
    pub per_stream_undone_all_none: u64,
    pub per_stream_undone_low_depth: u64,
}

/// What a set of files adds up to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Totals {
    pub label: String,
    pub files: u64,
    pub png_files: u64,
    pub bytes: u64,
    pub streams: u64,
    pub multi_span_streams: u64,
    pub eligible: u64,
    pub ineligible: BTreeMap<String, u64>,
    /// Eligible streams with every filter None; with a palette or a depth below 8.
    pub all_none: u64,
    pub low_depth: u64,
    /// Inflated bytes and raw bytes of the eligible streams.
    pub filtered_bytes: u64,
    pub raw_bytes: u64,
    pub side_bytes: u64,
    /// Scanlines per filter type over the eligible streams.
    pub filter_counts: [u64; 5],
    /// Winning models, keyed `<column>/<model>` (`today` for `A`).
    pub models: BTreeMap<String, u64>,
    pub undo_seconds: f64,
    pub refilter_seconds: f64,
    pub writers: Vec<Writers>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRecord {
    pub class: String,
    pub present: bool,
    pub manifest_files: u32,
    pub files: Vec<FileRecord>,
    pub totals: Totals,
    /// By source folder inside the class ([`GROUP_DEPTH`]).
    pub by_group: Vec<Totals>,
    /// By (colour type, bit depth, interlace) of the eligible streams.
    pub by_geometry: Vec<Totals>,
}

/// The gate's figures on [`GATE_CLASS`]: the owner's margin decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    pub class: String,
    pub present: bool,
    /// The `today` writer's bytes.
    pub today: u64,
    /// The layout whose per-stream writer is smallest (first on a tie), and its bytes.
    pub best_layout: Option<String>,
    pub per_stream: u64,
    /// `today - per_stream`.
    pub saving: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub settings: Settings,
    pub classes: Vec<ClassRecord>,
    pub gate: Gate,
    /// Eligible streams whose re-filtered rows differed from the inflated text (always 0: a
    /// mismatch fails the run).
    pub mismatches: u64,
}

// ---------------------------------------------------------------------------------------------
// Per file

fn model_name(m: PartModel) -> &'static str {
    match m {
        PartModel::Lzma => "lzma",
        PartModel::Zstd => "zstd",
    }
}

/// The lower-case extension of a path, without the dot.
fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// The part encoders of the workers, one taken per file.
struct Encoders(Mutex<Vec<PartEncoder>>);

impl Encoders {
    fn take(&self) -> Result<PartEncoder> {
        let taken = self
            .0
            .lock()
            .map_err(|_| anyhow!("encoder pool poisoned"))?
            .pop();
        match taken {
            Some(e) => Ok(e),
            None => new_encoder(),
        }
    }

    fn give(&self, e: PartEncoder) {
        if let Ok(mut v) = self.0.lock() {
            v.push(e);
        }
    }
}

fn new_encoder() -> Result<PartEncoder> {
    PartEncoder::balanced(&BalancedOptions::default(), PLAIN_TEXT_LIMIT)
        .map_err(|e| anyhow!("part encoder: {e}"))
}

fn encode(enc: &mut PartEncoder, text: &[u8]) -> Result<(u64, &'static str)> {
    let p = enc.encode(text).map_err(|e| anyhow!("part encode: {e}"))?;
    Ok((p.encoded.bytes.len() as u64, model_name(p.model)))
}

/// The deflate record's length in the `Records` frame (its hash and corrections as the stage
/// writes them).
fn deflate_record_len(stream: &[u8], plain_len: u64, corrections: Vec<u8>) -> u64 {
    record_len(&Record::new(RecordBody::Deflate(DeflateRecord {
        original_len: stream.len() as u64,
        plain_len,
        corrections,
        library: 0,
        original_hash: *blake3::hash(stream).as_bytes(),
    })))
}

/// The side information of an undone stream: the `png-filter` record plus the graph step.
fn side_bytes(layout: &Layout, filters: &[u8], text: &[u8]) -> u64 {
    record_len(&Record::new(RecordBody::PngFilter(PngFilterRecord {
        width: layout.width(),
        height: layout.height(),
        bit_depth: layout.bit_depth(),
        color_type: layout.color_type(),
        interlace: layout.interlace(),
        filters: filters.to_vec(),
        original_hash: *blake3::hash(text).as_bytes(),
    })))
    .saturating_add(STEP_BYTES)
}

/// A reconstructed stream held until the file's kind is known.
struct Held {
    spans: usize,
    stream: Vec<u8>,
    plain: Plain,
    corrections: Vec<u8>,
}

fn contiguous(bytes: &[u8], spans: &[(usize, usize)]) -> Vec<u8> {
    spans
        .iter()
        .flat_map(|(a, b)| bytes[*a..*b].iter().copied())
        .collect()
}

/// The file's rows as the `png` crate decodes them with no transformation (the raster: rows
/// packed, 16-bit big-endian, palette indices), or the decoder's error.
fn crate_rows(file: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut dec = png::Decoder::new_with_limits(
        std::io::Cursor::new(file),
        png::Limits { bytes: usize::MAX },
    );
    dec.set_transformations(png::Transformations::IDENTITY);
    // Only the rows are needed: no ancillary chunk is inflated (an iCCP or zTXt bomb would be
    // the one allocation the layout check before this call does not bound).
    dec.set_ignore_iccp_chunk(true);
    dec.set_ignore_text_chunk(true);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "output size overflows".to_string())?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    Ok(buf)
}

/// One stream: `A`, the eligibility, the undo verified three ways (re-filtering gives the
/// text back; the `png` crate's rows are the de-interlaced raw rows; each uniform text undoes
/// to the raw rows), every `B`.
fn process_stream(
    file: &[u8],
    header: &Result<Header, PngError>,
    h: Held,
    enc: &mut PartEncoder,
    mismatches: &mut u64,
) -> Result<Stream> {
    let plain = h.plain.text();
    let plain_len = plain.len() as u64;
    let record_bytes = deflate_record_len(&h.stream, plain_len, h.corrections);
    let (a_bytes, a_model) = encode(enc, plain)?;
    let mut out = Stream {
        spans: u32::try_from(h.spans)?,
        len: h.stream.len() as u64,
        plain_len,
        record_bytes,
        a_bytes,
        a_model: a_model.to_string(),
        ineligible: None,
        geometry: None,
        candidates: Vec::new(),
        undo_seconds: 0.0,
        refilter_seconds: 0.0,
    };
    let undone = header
        .as_ref()
        .map_err(|e| *e)
        .and_then(Header::layout)
        .and_then(|layout| {
            let (u, undo_seconds) = timed(|| layout.undo(plain));
            u.map(|u| (layout, u, undo_seconds))
        });
    let (layout, undone, undo_seconds) = match undone {
        Ok(v) => v,
        Err(e) => {
            out.ineligible = Some(e.name().to_string());
            return Ok(out);
        }
    };
    // A mismatch is a bug in lpk_core::peel::png, never a fallback: it fails the run.
    let mismatch = |mismatches: &mut u64, what: &str| -> anyhow::Error {
        *mismatches += 1;
        anyhow!(
            "{what} ({}x{} depth {} colour type {} interlace {}): a bug in lpk_core::peel::png",
            layout.width(),
            layout.height(),
            layout.bit_depth(),
            layout.color_type(),
            layout.interlace()
        )
    };
    // The `png` crate's rows (no transformation) are the de-interlaced raw rows.
    let theirs = match crate_rows(file) {
        Ok(rows) => rows,
        Err(_) => {
            out.ineligible = Some("png_crate".to_string());
            return Ok(out);
        }
    };
    let raster = layout
        .deinterlace(&undone.raw)
        .map_err(|e| anyhow!("de-interlace: {e}"))?;
    if theirs != raster {
        return Err(mismatch(
            mismatches,
            "the undone rows differ from the png crate's",
        ));
    }
    drop(theirs);
    drop(raster);
    // Re-filtering gives the inflated text back byte for byte.
    let (back, refilter_seconds) = timed(|| layout.refilter(&undone.raw, &undone.filters));
    let back = back.map_err(|e| anyhow!("re-filter: {e}"))?;
    if back != plain {
        return Err(mismatch(
            mismatches,
            "the re-filtered rows differ from the inflated text",
        ));
    }
    drop(back);
    let mut filter_counts = [0u64; 5];
    for f in &undone.filters {
        filter_counts[usize::from(*f)] += 1;
    }
    out.geometry = Some(Geometry {
        width: layout.width(),
        height: layout.height(),
        bit_depth: layout.bit_depth(),
        color_type: layout.color_type(),
        interlace: layout.interlace(),
        scanlines: layout.scanlines(),
        raw_bytes: undone.raw.len() as u64,
        side_bytes: side_bytes(&layout, &undone.filters, plain),
        filter_counts,
    });
    out.undo_seconds = undo_seconds;
    out.refilter_seconds = refilter_seconds;
    drop(h.plain);
    let mut push = |layout_id: &str, text: &[u8]| -> Result<()> {
        let (bytes, model) = encode(enc, text)?;
        out.candidates.push(Candidate {
            layout: layout_id.to_string(),
            model: model.to_string(),
            bytes,
        });
        Ok(())
    };
    push("raw", &undone.raw)?;
    for f in FILTER_SUB..=FILTER_PAETH {
        let name = FILTER_NAMES[usize::from(f)];
        let text = layout
            .refilter_uniform(&undone.raw, f)
            .map_err(|e| anyhow!("re-filter {name}: {e}"))?;
        // Each uniform text undoes to the raw rows with that filter on every scanline.
        let again = layout
            .undo(&text)
            .map_err(|e| anyhow!("undo of the {name} text: {e}"))?;
        if again.raw != undone.raw || again.filters.iter().any(|g| *g != f) {
            return Err(mismatch(
                mismatches,
                &format!("the {name} text does not undo to the raw rows"),
            ));
        }
        drop(again);
        push(name, &text)?;
    }
    if layout.interlace() == 1 {
        let text = layout
            .deinterlace(&undone.raw)
            .map_err(|e| anyhow!("de-interlace: {e}"))?;
        push("raster", &text)?;
    }
    Ok(out)
}

/// One file: its kind, then every stream.
fn process_file(
    bytes: &[u8],
    ext: &str,
    limits: &Limits,
    set: &Settings,
    encoders: &Encoders,
) -> Result<(String, Vec<Stream>, u64)> {
    if bytes.len() as u64 > set.max_file {
        return Ok(("size_cap".to_string(), Vec::new(), 0));
    }
    if !bytes.starts_with(&SIGNATURE) {
        return Ok(("not_png".to_string(), Vec::new(), 0));
    }
    let mut held = Vec::new();
    let examined = examine(bytes, ext, limits, |r| {
        held.push(Held {
            spans: r.spans.len(),
            stream: contiguous(bytes, &r.spans),
            plain: r.plain,
            corrections: r.corrections,
        });
    });
    if examined.kind != "png" {
        return Ok(("not_png".to_string(), Vec::new(), 0));
    }
    let header = Header::parse(bytes);
    let mut enc = encoders.take()?;
    let mut mismatches = 0u64;
    let mut streams = Vec::with_capacity(held.len());
    let mut failed = None;
    for h in held {
        match process_stream(bytes, &header, h, &mut enc, &mut mismatches) {
            Ok(s) => streams.push(s),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    encoders.give(enc);
    match failed {
        Some(e) => Err(e),
        None => Ok(("png".to_string(), streams, mismatches)),
    }
}

// ---------------------------------------------------------------------------------------------
// The writers (pure functions of the records)

/// A file's bytes under the three writers of `layout`: the per-stream gates, then the file
/// gate and the share rule as the stage applies them.
fn file_writers(f: &FileRecord, layout: &str, set: &Settings) -> Writers {
    let mut w = Writers {
        layout: layout.to_string(),
        today: 0,
        always: 0,
        per_stream: 0,
        always_undone: 0,
        per_stream_undone: 0,
        per_stream_undone_all_none: 0,
        per_stream_undone_low_depth: 0,
    };
    let (mut g_today, mut g_always, mut g_per) = (0u64, 0u64, 0u64);
    let (mut u_always, mut u_per, mut u_none, mut u_low) = (0u64, 0u64, 0u64, 0u64);
    for s in &f.streams {
        let today = s.today_cost(set);
        let undone = s.undone_cost(layout, set);
        g_today += today.map_or(0, |c| s.len - c);
        let always = if s.geometry.is_some() { undone } else { today };
        g_always += always.map_or(0, |c| s.len - c);
        let per = match (today, undone) {
            (Some(a), Some(b)) if b < a => Some((b, true)),
            (Some(a), _) => Some((a, false)),
            (None, Some(b)) => Some((b, true)),
            (None, None) => None,
        };
        g_per += per.map_or(0, |(c, _)| s.len - c);
        if s.geometry.is_some() && undone.is_some() {
            u_always += 1;
        }
        if let (Some((_, true)), Some(g)) = (per, s.geometry.as_ref()) {
            u_per += 1;
            if g.all_none() {
                u_none += 1;
            }
            if g.low_depth() {
                u_low += 1;
            }
        }
    }
    let gate = |gain: u64| -> u64 {
        if gain <= set.file_gate || gain.saturating_mul(set.file_share) < f.bytes {
            0
        } else {
            gain
        }
    };
    w.today = f.bytes - gate(g_today);
    w.always = f.bytes - gate(g_always);
    w.per_stream = f.bytes - gate(g_per);
    if gate(g_always) > 0 {
        w.always_undone = u_always;
    }
    if gate(g_per) > 0 {
        w.per_stream_undone = u_per;
        w.per_stream_undone_all_none = u_none;
        w.per_stream_undone_low_depth = u_low;
    }
    w
}

fn add_writers(acc: &mut Writers, w: &Writers) {
    acc.today += w.today;
    acc.always += w.always;
    acc.per_stream += w.per_stream;
    acc.always_undone += w.always_undone;
    acc.per_stream_undone += w.per_stream_undone;
    acc.per_stream_undone_all_none += w.per_stream_undone_all_none;
    acc.per_stream_undone_low_depth += w.per_stream_undone_low_depth;
}

fn empty_totals(label: &str) -> Totals {
    Totals {
        label: label.to_string(),
        files: 0,
        png_files: 0,
        bytes: 0,
        streams: 0,
        multi_span_streams: 0,
        eligible: 0,
        ineligible: BTreeMap::new(),
        all_none: 0,
        low_depth: 0,
        filtered_bytes: 0,
        raw_bytes: 0,
        side_bytes: 0,
        filter_counts: [0; 5],
        models: BTreeMap::new(),
        undo_seconds: 0.0,
        refilter_seconds: 0.0,
        writers: LAYOUTS
            .iter()
            .map(|(id, _)| Writers {
                layout: id.to_string(),
                today: 0,
                always: 0,
                per_stream: 0,
                always_undone: 0,
                per_stream_undone: 0,
                per_stream_undone_all_none: 0,
                per_stream_undone_low_depth: 0,
            })
            .collect(),
    }
}

/// The totals of `files`. `keep` selects which streams count; with `filtered`, a file counts
/// only when one of its streams passes (its bytes and writers computed over those streams),
/// else every file counts (the class totals, over the class's files).
fn totals_of(
    label: &str,
    files: &[&FileRecord],
    set: &Settings,
    filtered: bool,
    keep: impl Fn(&Stream) -> bool,
) -> Totals {
    let mut t = empty_totals(label);
    for f in files {
        let streams: Vec<Stream> = f.streams.iter().filter(|s| keep(s)).cloned().collect();
        if filtered && streams.is_empty() {
            continue;
        }
        let sub = FileRecord {
            index: f.index,
            path: None,
            bytes: f.bytes,
            kind: f.kind.clone(),
            streams,
        };
        t.files += 1;
        if sub.kind == "png" {
            t.png_files += 1;
        }
        t.bytes += sub.bytes;
        for s in &sub.streams {
            t.streams += 1;
            if s.spans > 1 {
                t.multi_span_streams += 1;
            }
            *t.models.entry(format!("today/{}", s.a_model)).or_default() += 1;
            match (&s.ineligible, &s.geometry) {
                (Some(cause), _) => *t.ineligible.entry(cause.clone()).or_default() += 1,
                (None, Some(g)) => {
                    t.eligible += 1;
                    if g.all_none() {
                        t.all_none += 1;
                    }
                    if g.low_depth() {
                        t.low_depth += 1;
                    }
                    t.filtered_bytes += s.plain_len;
                    t.raw_bytes += g.raw_bytes;
                    t.side_bytes += g.side_bytes;
                    for (a, b) in t.filter_counts.iter_mut().zip(g.filter_counts) {
                        *a += b;
                    }
                    for c in &s.candidates {
                        *t.models
                            .entry(format!("{}/{}", c.layout, c.model))
                            .or_default() += 1;
                    }
                    t.undo_seconds += s.undo_seconds;
                    t.refilter_seconds += s.refilter_seconds;
                }
                (None, None) => {}
            }
        }
        for w in &mut t.writers {
            let layout = w.layout.clone();
            add_writers(w, &file_writers(&sub, &layout, set));
        }
    }
    t
}

/// A class's totals, source groups and geometry tables from its files.
fn summarise(
    ctx: &Ctx<'_>,
    class: &str,
    files: &[FileRecord],
    set: &Settings,
) -> (Totals, Vec<Totals>, Vec<Totals>) {
    let all: Vec<&FileRecord> = files.iter().collect();
    let totals = totals_of("all", &all, set, false, |_| true);
    let mut by_group = Vec::new();
    for g in ctx.groups(class, GROUP_DEPTH) {
        let members: Vec<&FileRecord> = g
            .files
            .iter()
            .filter_map(|m| {
                let i = ctx
                    .class_files(class)?
                    .iter()
                    .position(|f| std::ptr::eq(f, *m))?;
                files.iter().find(|f| f.index as usize == i)
            })
            .collect();
        by_group.push(totals_of(&g.label, &members, set, false, |_| true));
    }
    let mut labels: Vec<String> = files
        .iter()
        .flat_map(|f| f.streams.iter())
        .filter_map(|s| s.geometry.as_ref().map(Geometry::label))
        .collect();
    labels.sort();
    labels.dedup();
    let by_geometry = labels
        .iter()
        .map(|l| {
            totals_of(l, &all, set, true, |s| {
                s.geometry.as_ref().is_some_and(|g| g.label() == *l)
            })
        })
        .collect();
    (totals, by_group, by_geometry)
}

/// The gate's figures from the gate class's totals.
fn gate_of(classes: &[ClassRecord]) -> Gate {
    let c = classes.iter().find(|c| c.class == GATE_CLASS);
    let mut g = Gate {
        class: GATE_CLASS.to_string(),
        present: false,
        today: 0,
        best_layout: None,
        per_stream: 0,
        saving: 0,
    };
    let Some(c) = c.filter(|c| c.present) else {
        return g;
    };
    g.present = true;
    g.today = c.totals.writers.first().map_or(0, |w| w.today);
    if let Some(best) = c.totals.writers.iter().min_by_key(|w| w.per_stream) {
        g.best_layout = Some(best.layout.clone());
        g.per_stream = best.per_stream;
        g.saving = g.today.saturating_sub(best.per_stream);
    }
    g
}

// ---------------------------------------------------------------------------------------------
// The run

/// Workers such that their encoders' working sets fit half the physical memory (at least 1,
/// at most `threads`); `largest` is the largest file of the run.
pub fn workers_for(threads: u32, largest: u64, physical: Option<u64>) -> Result<u32> {
    let enc = new_encoder()?;
    let plain = largest.saturating_mul(MAX_EXPANSION).min(PLAIN_TEXT_LIMIT);
    // The encoder plus the texts a worker holds at once (the file, the inflated text, the raw
    // rows and one re-filtered candidate).
    let per_worker = enc
        .memory_for(plain)
        .saturating_add(largest)
        .saturating_add(plain.saturating_mul(3));
    let fit = physical.map_or(u64::from(threads), |p| (p / 2) / per_worker.max(1));
    Ok(u32::try_from(fit.min(u64::from(threads)))
        .unwrap_or(u32::MAX)
        .max(1))
}

fn settings(workers: u32) -> Settings {
    let o = BalancedOptions::default();
    Settings {
        stream_overhead: STREAM_OVERHEAD,
        file_gate: FILE_GATE,
        file_share: FILE_SHARE,
        step_bytes: STEP_BYTES,
        max_file: MAX_FILE,
        plain_text_limit: PLAIN_TEXT_LIMIT,
        dict_size: o.dict_size,
        zstd_level: o.zstd_level,
        sample_len: o.sample_len as u64,
        workers,
    }
}

/// The probe over one class: every file's record, in manifest order.
fn run_class(ctx: &Ctx<'_>, class: &str, set: &Settings) -> Result<(Vec<FileRecord>, u64)> {
    let files = ctx.class_files(class).unwrap_or(&[]);
    let limits = Limits::standard();
    let encoders = Encoders(Mutex::new(Vec::new()));
    let results = par_map(files, set.workers as usize, |i, f| -> Result<_> {
        let bytes = ctx.read_file(class, f)?;
        let (kind, streams, mismatches) =
            process_file(&bytes, &extension(&f.path), &limits, set, &encoders)
                .with_context(|| format!("class `{class}`: `{}`", f.path))?;
        Ok((
            FileRecord {
                index: u32::try_from(i)?,
                path: ctx.label(f),
                bytes: bytes.len() as u64,
                kind,
                streams,
            },
            mismatches,
        ))
    });
    let mut out = Vec::with_capacity(files.len());
    let mut mismatches = 0;
    for r in results {
        let (f, m) = r?;
        out.push(f);
        mismatches += m;
    }
    Ok((out, mismatches))
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let largest = CLASSES
        .iter()
        .flat_map(|c| ctx.class_files(c).unwrap_or(&[]).iter().map(|f| f.bytes))
        .max()
        .unwrap_or(0)
        .min(MAX_FILE);
    let workers = workers_for(ctx.threads, largest, lpk_core::memory::physical_memory())?;
    let set = settings(workers);
    let mut classes = Vec::new();
    let mut notes = Vec::new();
    let mut mismatches = 0;
    for class in CLASSES {
        let present = ctx.class_files(class).is_some();
        if !present {
            notes.push(format!("the corpus has no class `{class}`"));
        }
        let (files, m) = run_class(ctx, class, &set)?;
        mismatches += m;
        let (totals, by_group, by_geometry) = summarise(ctx, class, &files, &set);
        classes.push(ClassRecord {
            class: class.to_string(),
            present,
            manifest_files: u32::try_from(files.len())?,
            files,
            totals,
            by_group,
            by_geometry,
        });
    }
    if mismatches > 0 {
        bail!("{mismatches} re-filtered streams differed from their inflated text");
    }
    let gate = gate_of(&classes);
    let mut out = Output::new(
        Data {
            settings: set,
            classes,
            gate,
            mismatches,
        },
        1,
    )
    .with_zstd()
    .with_xz()
    .note(format!(
        "undo and re-filter seconds were measured inside the parallel pass ({workers} workers), \
         not alone"
    ));
    out.libraries.insert(
        "preflate-rs".to_string(),
        lpk_core::peel::deflate::PREFLATE_VERSION.to_string(),
    );
    for n in notes {
        out = out.note(n);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Checks and rendering

fn seconds_ok(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    let set = &d.settings;
    if set.stream_overhead != STREAM_OVERHEAD
        || set.file_gate != FILE_GATE
        || set.file_share != FILE_SHARE
        || set.step_bytes != STEP_BYTES
    {
        p.push("/data/settings: must be the stage's constants".to_string());
    }
    if set.workers == 0 {
        p.push("/data/settings/workers: at least 1".to_string());
    }
    if d.mismatches != 0 {
        p.push("/data/mismatches: a mismatch fails the run".to_string());
    }
    let names: Vec<&str> = d.classes.iter().map(|c| c.class.as_str()).collect();
    if names != CLASSES {
        p.push(format!("/data/classes: expected the classes {CLASSES:?}"));
    }
    let layout_ids: Vec<&str> = LAYOUTS.iter().map(|(id, _)| *id).collect();
    for (ci, c) in d.classes.iter().enumerate() {
        let at = format!("/data/classes/{ci}");
        if c.manifest_files as usize != c.files.len() {
            p.push(format!("{at}/manifest_files: must equal the file count"));
        }
        for (fi, f) in c.files.iter().enumerate() {
            let at = format!("{at}/files/{fi}");
            if f.index as usize != fi {
                p.push(format!("{at}/index: indices must be 0, 1, 2, ..."));
            }
            if !matches!(f.kind.as_str(), "png" | "not_png" | "size_cap") {
                p.push(format!("{at}/kind: unknown kind `{}`", f.kind));
            }
            if f.kind != "png" && !f.streams.is_empty() {
                p.push(format!("{at}/streams: only a png file has streams"));
            }
            for (si, s) in f.streams.iter().enumerate() {
                let at = format!("{at}/streams/{si}");
                if s.len == 0 || s.spans == 0 {
                    p.push(format!("{at}: a stream has bytes and spans"));
                }
                if !matches!(s.a_model.as_str(), "lzma" | "zstd") {
                    p.push(format!("{at}/a_model: lzma or zstd"));
                }
                if !seconds_ok(s.undo_seconds) || !seconds_ok(s.refilter_seconds) {
                    p.push(format!("{at}: seconds must be non-negative numbers"));
                }
                match (&s.ineligible, &s.geometry) {
                    (Some(_), None) => {
                        if !s.candidates.is_empty() {
                            p.push(format!("{at}/candidates: none for an ineligible stream"));
                        }
                    }
                    (None, Some(g)) => {
                        let want: Vec<&str> = if g.interlace == 1 {
                            layout_ids.clone()
                        } else {
                            layout_ids[..5].to_vec()
                        };
                        let got: Vec<&str> =
                            s.candidates.iter().map(|c| c.layout.as_str()).collect();
                        if got != want {
                            p.push(format!("{at}/candidates: expected {want:?}, found {got:?}"));
                        }
                        if g.filter_counts.iter().sum::<u64>() != g.scanlines {
                            p.push(format!(
                                "{at}/geometry/filter_counts: must sum to the scanlines"
                            ));
                        }
                        if g.raw_bytes + g.scanlines != s.plain_len {
                            p.push(format!(
                                "{at}/geometry/raw_bytes: raw bytes plus scanlines is the plain \
                                 length"
                            ));
                        }
                    }
                    _ => p.push(format!(
                        "{at}: exactly one of ineligible and geometry is set"
                    )),
                }
            }
        }
        // The totals follow from the files.
        let (totals, _, by_geometry) = summarise_pure(&c.files, set);
        if totals != c.totals {
            p.push(format!("{at}/totals: do not follow from the files"));
        }
        if by_geometry != c.by_geometry {
            p.push(format!("{at}/by_geometry: does not follow from the files"));
        }
        let group_files: u64 = c.by_group.iter().map(|g| g.files).sum();
        if group_files != c.files.len() as u64 {
            p.push(format!(
                "{at}/by_group: the groups must cover the files once"
            ));
        }
        for (gi, g) in c.by_group.iter().enumerate() {
            let ids: Vec<&str> = g.writers.iter().map(|w| w.layout.as_str()).collect();
            if ids != layout_ids {
                p.push(format!(
                    "{at}/by_group/{gi}/writers: expected {layout_ids:?}"
                ));
            }
        }
    }
    if gate_of(&d.classes) != d.gate {
        p.push("/data/gate: does not follow from the gate class's totals".to_string());
    }
    p
}

/// [`summarise`] without the corpus (no source groups): the totals and the geometry tables.
fn summarise_pure(files: &[FileRecord], set: &Settings) -> (Totals, Vec<Totals>, Vec<Totals>) {
    let all: Vec<&FileRecord> = files.iter().collect();
    let totals = totals_of("all", &all, set, false, |_| true);
    let mut labels: Vec<String> = files
        .iter()
        .flat_map(|f| f.streams.iter())
        .filter_map(|s| s.geometry.as_ref().map(Geometry::label))
        .collect();
    labels.sort();
    labels.dedup();
    let by_geometry = labels
        .iter()
        .map(|l| {
            totals_of(l, &all, set, true, |s| {
                s.geometry.as_ref().is_some_and(|g| g.label() == *l)
            })
        })
        .collect();
    (totals, Vec::new(), by_geometry)
}

fn saving(today: u64, other: u64) -> String {
    if other == today {
        format!("0 ({})", pct(0, today))
    } else if other < today {
        format!("-{} ({})", today - other, pct(today - other, today))
    } else {
        format!("+{} ({})", other - today, pct(other - today, today))
    }
}

fn writers_table(t: &Totals) -> String {
    let rows: Vec<Vec<String>> = t
        .writers
        .iter()
        .map(|w| {
            vec![
                w.layout.clone(),
                w.today.to_string(),
                w.always.to_string(),
                saving(w.today, w.always),
                w.per_stream.to_string(),
                saving(w.today, w.per_stream),
                w.always_undone.to_string(),
                format!(
                    "{} (all None {}, palette or depth < 8 {})",
                    w.per_stream_undone,
                    w.per_stream_undone_all_none,
                    w.per_stream_undone_low_depth
                ),
            ]
        })
        .collect();
    md_table(
        &[
            "layout",
            "today",
            "always",
            "always vs today",
            "per stream",
            "per stream vs today",
            "undone (always)",
            "undone (per stream)",
        ],
        &rows,
    )
}

fn best_row(t: &Totals) -> (String, u64, u64) {
    let today = t.writers.first().map_or(0, |w| w.today);
    t.writers
        .iter()
        .min_by_key(|w| w.per_stream)
        .map_or(("-".to_string(), today, 0), |w| {
            (
                w.layout.clone(),
                w.per_stream,
                today.saturating_sub(w.per_stream),
            )
        })
}

fn compact_table(rows_in: &[Totals]) -> String {
    let rows: Vec<Vec<String>> = rows_in
        .iter()
        .map(|t| {
            let (best, per, saved) = best_row(t);
            let today = t.writers.first().map_or(0, |w| w.today);
            vec![
                t.label.clone(),
                t.files.to_string(),
                t.streams.to_string(),
                t.eligible.to_string(),
                today.to_string(),
                best,
                per.to_string(),
                format!("{saved} ({})", pct(saved, today)),
            ]
        })
        .collect();
    md_table(
        &[
            "group",
            "files",
            "streams",
            "eligible",
            "today",
            "best layout",
            "per stream",
            "saving",
        ],
        &rows,
    )
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let set = &d.settings;
    let mut s = md_header(e);
    s.push_str(&format!(
        "The E2-16 measured gate: per PNG stream the 1.2 Deflate peel reconstructs, the stage's \
         Balanced part encoder (LZMA dictionary {} bytes, zstd level {}, sample {} bytes) on \
         today's filtered rows (`A`) against the undone rows in each layout (`B`), plus the exact \
         side information (the `png-filter` record and {} bytes for its graph step). The stage's \
         gates: a stream is kept below its length after M, the Deflate record and {} bytes; a \
         file's streams gain more than {} bytes and at least 1/{} of the file. Writers: `today` \
         (`A`), `always` (`B` for every eligible stream), `per stream` (the cheaper). Class bytes \
         are the class's files' lengths less their kept gains, so the differences between writers \
         predict the row differences up to varint widths. Files above {} bytes are skipped as \
         the stage does; {} workers.\n\n",
        set.dict_size,
        set.zstd_level,
        set.sample_len,
        set.step_bytes,
        set.stream_overhead,
        set.file_gate,
        set.file_share,
        set.max_file,
        set.workers
    ));
    s.push_str("## Gate\n\n");
    let g = &d.gate;
    if g.present {
        s.push_str(&format!(
            "`{}`: today {} bytes; best layout `{}` per stream {} bytes; saving {} bytes ({} of \
             today). The owner's margin decides.\n\n",
            g.class,
            g.today,
            g.best_layout.as_deref().unwrap_or("-"),
            g.per_stream,
            g.saving,
            pct(g.saving, g.today)
        ));
    } else {
        s.push_str(&format!("`{}` is not in this corpus.\n\n", g.class));
    }
    s.push_str("## Layouts\n\n");
    let rows: Vec<Vec<String>> = LAYOUTS
        .iter()
        .map(|(id, what)| vec![id.to_string(), what.to_string()])
        .collect();
    s.push_str(&md_table(&["layout", "what"], &rows));
    s.push('\n');
    for c in &d.classes {
        s.push_str(&format!("## Class `{}`\n\n", c.class));
        if !c.present {
            s.push_str("Not in this corpus.\n\n");
            continue;
        }
        let t = &c.totals;
        let causes: Vec<String> = t
            .ineligible
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        let filters: Vec<String> = FILTER_NAMES
            .iter()
            .zip(t.filter_counts)
            .map(|(n, c)| format!("{n} {c}"))
            .collect();
        let models: Vec<String> = t.models.iter().map(|(k, v)| format!("{k} {v}")).collect();
        s.push_str(&format!(
            "- files {} (PNG {}), {} bytes; streams {} (over several IDATs {}); eligible {} \
             (all filters None {}, palette or depth below 8 {}); ineligible: {}\n\
             - eligible streams: filtered {} bytes, raw {} bytes, side information {} bytes; \
             scanlines by filter: {}\n\
             - winning models: {}\n\
             - undo {} MB/s ({:.2} s), re-filter {} MB/s ({:.2} s), of the filtered bytes\n\n",
            t.files,
            t.png_files,
            t.bytes,
            t.streams,
            t.multi_span_streams,
            t.eligible,
            t.all_none,
            t.low_depth,
            if causes.is_empty() {
                "none".to_string()
            } else {
                causes.join(", ")
            },
            t.filtered_bytes,
            t.raw_bytes,
            t.side_bytes,
            filters.join(", "),
            if models.is_empty() {
                "none".to_string()
            } else {
                models.join(", ")
            },
            mbps(t.filtered_bytes, t.undo_seconds),
            t.undo_seconds,
            mbps(t.filtered_bytes, t.refilter_seconds),
            t.refilter_seconds,
        ));
        s.push_str("### Writers\n\n");
        s.push_str(&writers_table(t));
        s.push('\n');
        if !c.by_group.is_empty() {
            s.push_str("### By source group\n\n");
            s.push_str(&compact_table(&c.by_group));
            s.push('\n');
        }
        if !c.by_geometry.is_empty() {
            s.push_str("### By colour type, bit depth and interlace\n\n");
            s.push_str(&compact_table(&c.by_geometry));
            s.push('\n');
        }
    }
    s
}

// ---------------------------------------------------------------------------------------------
// The validation against the pipeline (run with the corpus)

/// The `today` writer's kept streams, M bytes and record bytes over `files` (every layout
/// gives the same `today`; the first is used).
pub fn today_kept(files: &[FileRecord], set: &Settings) -> (u64, u64, u64) {
    let (mut kept, mut model, mut record) = (0u64, 0u64, 0u64);
    for f in files {
        let w = file_writers(f, "raw", set);
        if w.today == f.bytes {
            continue;
        }
        for s in &f.streams {
            if s.today_cost(set).is_some() {
                kept += 1;
                model += s.a_bytes;
                record += s.record_bytes;
            }
        }
    }
    (kept, model, record)
}

/// Where the local corpus is (`LPK_BENCH_CORPUS`, else `bench/corpus/small`).
pub fn local_corpus() -> std::path::PathBuf {
    std::env::var_os("LPK_BENCH_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("bench")
                .join("corpus")
                .join("small")
        })
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::corpus::manifest::{Manifest, ManifestFile};
    use crate::probe::{execute, Config};
    use crate::run::exec::load_corpus;
    use lpk_core::peel::png::PAIRS;

    /// xorshift64, deterministic.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    fn chunk(out: &mut Vec<u8>, name: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(name);
        crc.update(body);
        out.extend_from_slice(name);
        out.extend_from_slice(body);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        e.write_all(data).expect("write");
        e.finish().expect("finish")
    }

    /// A PNG file of `layout` with the given raw rows and filters, its IDAT split in two.
    fn png_file(layout: &Layout, raw: &[u8], filters: &[u8]) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&layout.width().to_be_bytes());
        ihdr.extend_from_slice(&layout.height().to_be_bytes());
        ihdr.extend_from_slice(&[
            layout.bit_depth(),
            layout.color_type(),
            0,
            0,
            layout.interlace(),
        ]);
        chunk(&mut out, b"IHDR", &ihdr);
        if layout.color_type() == 3 {
            let n = 1usize << layout.bit_depth();
            let plte: Vec<u8> = (0..n)
                .flat_map(|i| [i as u8, (i * 3) as u8, 255 - i as u8])
                .collect();
            chunk(&mut out, b"PLTE", &plte);
        }
        let text = layout.refilter(raw, filters).expect("refilter");
        let z = zlib(&text);
        let (a, b) = z.split_at(z.len() / 2);
        chunk(&mut out, b"IDAT", a);
        chunk(&mut out, b"IDAT", b);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// The inflated IDAT text of a PNG file (every IDAT body concatenated, then zlib).
    fn idat_text(file: &[u8]) -> Vec<u8> {
        use std::io::Read;
        let mut z = Vec::new();
        let mut pos = 8;
        while pos + 8 <= file.len() {
            let head = &file[pos..pos + 8];
            let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
            let name = &head[4..8];
            let body = &file[pos + 8..pos + 8 + len];
            if name == b"IDAT" {
                z.extend_from_slice(body);
            }
            pos += 12 + len;
        }
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(&z[..])
            .read_to_end(&mut out)
            .expect("inflate");
        out
    }

    /// Smooth, compressible raw rows (a gradient with a little noise) and mixed filters.
    fn image(layout: &Layout, rng: &mut Rng) -> (Vec<u8>, Vec<u8>) {
        let n = layout.raw_len().expect("raw") as usize;
        let raw: Vec<u8> = (0..n)
            .map(|i| ((i / 3) % 97) as u8 ^ ((rng.next() >> 61) as u8))
            .collect();
        let filters: Vec<u8> = (0..layout.scanlines())
            .map(|_| (rng.next() % 5) as u8)
            .collect();
        (raw, filters)
    }

    #[test]
    fn the_png_crate_decodes_every_pair_to_the_undone_rows() {
        let mut rng = Rng(7);
        for (depth, ct) in PAIRS {
            for interlace in 0..=1u8 {
                for (w, h) in [(1u32, 1u32), (13, 7), (17, 9), (8, 8)] {
                    let layout = Layout::new(w, h, depth, ct, interlace).expect("layout");
                    let (raw, filters) = image(&layout, &mut rng);
                    let file = png_file(&layout, &raw, &filters);
                    let dec = png::Decoder::new(std::io::Cursor::new(&file));
                    let mut reader = dec.read_info().expect("info");
                    let mut buf = vec![0u8; reader.output_buffer_size().expect("size")];
                    let info = reader.next_frame(&mut buf).expect("frame");
                    let theirs = &buf[..info.buffer_size()];
                    let ours = layout.deinterlace(&raw).expect("raster");
                    assert_eq!(theirs, &ours[..], "{layout:?}");
                    // And the inflated IDAT text undoes to the rows and filters the file was
                    // made from, through the header the probe parses.
                    let hdr = Header::parse(&file)
                        .expect("header")
                        .layout()
                        .expect("layout");
                    assert_eq!(hdr, layout);
                    let u = hdr.undo(&idat_text(&file)).expect("undo");
                    assert_eq!((u.raw, u.filters), (raw, filters), "{layout:?}");
                }
            }
        }
    }

    fn stream(len: u64, a: u64, b: Option<u64>, geometry: Option<Geometry>) -> Stream {
        let candidates = match (b, &geometry) {
            (Some(b), Some(g)) => {
                let mut v: Vec<Candidate> = LAYOUTS[..5]
                    .iter()
                    .map(|(id, _)| Candidate {
                        layout: id.to_string(),
                        model: "lzma".to_string(),
                        bytes: b,
                    })
                    .collect();
                if g.interlace == 1 {
                    v.push(Candidate {
                        layout: "raster".to_string(),
                        model: "zstd".to_string(),
                        bytes: b / 2,
                    });
                }
                v
            }
            _ => Vec::new(),
        };
        Stream {
            spans: 1,
            len,
            plain_len: len * 3,
            record_bytes: 60,
            a_bytes: a,
            a_model: "lzma".to_string(),
            ineligible: geometry.is_none().then(|| "bad_pair".to_string()),
            geometry,
            candidates,
            undo_seconds: 0.0,
            refilter_seconds: 0.0,
        }
    }

    fn geometry(interlace: u8, side: u64, counts: [u64; 5]) -> Geometry {
        Geometry {
            width: 4,
            height: 4,
            bit_depth: 8,
            color_type: 2,
            interlace,
            scanlines: counts.iter().sum(),
            raw_bytes: 48,
            side_bytes: side,
            filter_counts: counts,
        }
    }

    #[test]
    fn the_writers_apply_the_stream_gates_then_the_file_gates() {
        let set = settings(1);
        // Stream 1: A kept (cost 1000 + 60 + 160 = 1220 < 10000); B cheaper (500 + 60 + 100 +
        // 160 = 820). Stream 2: ineligible, A kept. Stream 3: A not kept (no gain); B kept.
        let f = FileRecord {
            index: 0,
            path: None,
            bytes: 40_000,
            kind: "png".to_string(),
            streams: vec![
                stream(
                    10_000,
                    1000,
                    Some(500),
                    Some(geometry(0, 100, [4, 0, 0, 0, 0])),
                ),
                stream(10_000, 2000, None, None),
                stream(
                    10_000,
                    9_900,
                    Some(5000),
                    Some(geometry(1, 100, [1, 1, 1, 1, 0])),
                ),
            ],
        };
        let w = file_writers(&f, "raw", &set);
        let today_gain = (10_000 - 1220) + (10_000 - 2220);
        assert_eq!(w.today, 40_000 - today_gain);
        let always_gain = (10_000 - 820) + (10_000 - 2220) + (10_000 - 5320);
        assert_eq!(w.always, 40_000 - always_gain);
        assert_eq!(w.per_stream, w.always);
        assert_eq!((w.always_undone, w.per_stream_undone), (2, 2));
        assert_eq!(
            (w.per_stream_undone_all_none, w.per_stream_undone_low_depth),
            (1, 0)
        );
        // `raster` falls back to `raw` for the non-interlaced stream and is its own column for
        // the interlaced one.
        let w = file_writers(&f, "raster", &set);
        assert_eq!(
            w.always,
            40_000 - ((10_000 - 820) + (10_000 - 2220) + (10_000 - 2820))
        );
        // The file gate: a gain of at most 256 is nothing.
        let small = FileRecord {
            bytes: 2000,
            streams: vec![stream(
                1000,
                600,
                Some(500),
                Some(geometry(0, 100, [1, 0, 0, 0, 0])),
            )],
            ..f.clone()
        };
        let w = file_writers(&small, "raw", &set);
        assert_eq!((w.today, w.always, w.per_stream), (2000, 2000, 2000));
        assert_eq!(w.per_stream_undone, 0);
        // The share rule: a gain below 1/32 of the file goes whole.
        let share = FileRecord {
            bytes: 100_000,
            streams: vec![stream(
                10_000,
                7000,
                Some(6000),
                Some(geometry(0, 100, [4, 0, 0, 0, 0])),
            )],
            ..f.clone()
        };
        let w = file_writers(&share, "raw", &set);
        assert_eq!(w.today, 100_000);
        // Undone, the stream gains 1/32 of the file: kept.
        assert_eq!(w.per_stream, 100_000 - (10_000 - 6320));
        let ok = FileRecord {
            bytes: 50_000,
            ..share
        };
        let w = file_writers(&ok, "raw", &set);
        assert_eq!(w.today, 50_000 - (10_000 - 7220));
        assert_eq!(w.per_stream, 50_000 - (10_000 - 6320));
        // The validation view: kept streams, M bytes, record bytes of `today`.
        assert_eq!(
            today_kept(&[f.clone(), small, ok], &set),
            (3, 1000 + 2000 + 7000, 180)
        );
    }

    #[test]
    fn workers_fit_half_the_physical_memory() {
        assert_eq!(workers_for(8, 1 << 20, None).expect("w"), 8);
        assert_eq!(workers_for(8, 1 << 20, Some(1 << 20)).expect("w"), 1);
        assert_eq!(workers_for(1, 1 << 30, Some(1 << 40)).expect("w"), 1);
        let w = workers_for(16, 1 << 20, Some(1 << 40)).expect("w");
        assert_eq!(w, 16);
        assert!(workers_for(16, 1 << 20, Some(8 << 30)).expect("w") <= 16);
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

    /// A tiny corpus: two source groups of `photo-raw-png`, a palette image and a non-PNG in
    /// `game-assets`, the other classes absent.
    fn corpus(tmp: &Path) -> PathBuf {
        let dir = tmp.join("corpus");
        let mut rng = Rng(11);
        let rgb = Layout::new(64, 48, 8, 2, 0).expect("layout");
        let (raw, filters) = image(&rgb, &mut rng);
        let a = png_file(&rgb, &raw, &filters);
        let grey = Layout::new(40, 40, 2, 0, 1).expect("layout");
        let (raw, filters) = image(&grey, &mut rng);
        let b = png_file(&grey, &raw, &filters);
        let rgba16 = Layout::new(24, 20, 16, 6, 1).expect("layout");
        let (raw, _) = image(&rgba16, &mut rng);
        let filters = vec![0u8; rgba16.scanlines() as usize];
        let c = png_file(&rgba16, &raw, &filters);
        let pal = Layout::new(64, 64, 4, 3, 0).expect("layout");
        let (raw, _) = image(&pal, &mut rng);
        let filters = vec![0u8; 64];
        let d = png_file(&pal, &raw, &filters);
        // A PNG whose IHDR names filter method 1: the walker finds the stream, the undo refuses.
        let mut e = a.clone();
        e[8 + 8 + 11] = 1;
        let files: Vec<(&str, &str, Vec<u8>)> = vec![
            ("photo-raw-png", "kodak/a.png", a),
            ("photo-raw-png", "kodak/b.png", b),
            ("photo-raw-png", "converted/c.png", c),
            ("photo-raw-png", "converted/e.png", e),
            ("game-assets", "sprites/d.png", d),
            (
                "game-assets",
                "sprites/notes.txt",
                b"not a png at all".to_vec(),
            ),
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
    fn end_to_end_writes_a_valid_result_with_the_gate_and_every_table() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path().join("results");
        let cfg = Config {
            probes: vec![NAME.to_string()],
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
            std::fs::read_to_string(out.results_dir.join("probe-png-filter.json")).expect("json");
        let env: Envelope<Data> = serde_json::from_str(&json).expect("typed");
        let d = &env.data;
        let names: Vec<&str> = d.classes.iter().map(|c| c.class.as_str()).collect();
        assert_eq!(names, CLASSES);
        assert_eq!(d.mismatches, 0);
        let photo = &d.classes[1];
        assert!(photo.present && d.classes[0].files.is_empty() && !d.classes[0].present);
        let kinds: Vec<&str> = photo.files.iter().map(|f| f.kind.as_str()).collect();
        assert_eq!(kinds, ["png", "png", "png", "png"]);
        let t = &photo.totals;
        assert_eq!((t.files, t.png_files, t.streams, t.eligible), (4, 4, 4, 3));
        assert_eq!(t.multi_span_streams, 4);
        assert_eq!(t.ineligible.get("filter_method"), Some(&1));
        assert_eq!(t.all_none, 1);
        let labels: Vec<&str> = photo.by_group.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["converted", "kodak"]);
        let geoms: Vec<&str> = photo.by_geometry.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(geoms, ["ct0-d2-i1", "ct2-d8-i0", "ct6-d16-i1"]);
        let ids: Vec<&str> = t.writers.iter().map(|w| w.layout.as_str()).collect();
        assert_eq!(ids, LAYOUTS.map(|(id, _)| id));
        // Interlaced streams carry the raster column, the rest do not.
        for f in &photo.files {
            for s in &f.streams {
                if let Some(g) = &s.geometry {
                    assert_eq!(s.candidates.len(), if g.interlace == 1 { 6 } else { 5 });
                    assert_eq!(g.raw_bytes + g.scanlines, s.plain_len);
                    // Kind, flags, a length varint, the 11 fixed body bytes, a length
                    // varint, the filters, two hashes, the graph step.
                    assert!(g.side_bytes >= 4 + 1 + 11 + 1 + g.scanlines + 32 + 32 + STEP_BYTES);
                }
            }
        }
        let game = &d.classes[2];
        let kinds: Vec<&str> = game.files.iter().map(|f| f.kind.as_str()).collect();
        assert_eq!(kinds, ["png", "not_png"]);
        assert_eq!(game.totals.low_depth, 1);
        assert_eq!(d.gate.class, GATE_CLASS);
        assert!(d.gate.present && d.gate.best_layout.is_some());
        assert_eq!(d.gate.today, t.writers[0].today);
        assert!(d.gate.per_stream <= d.gate.today);
        // Every writer's "today" is the same whatever the layout; per stream never exceeds it.
        for w in &t.writers {
            assert_eq!(w.today, t.writers[0].today);
            assert!(w.per_stream <= w.today && w.per_stream <= w.always);
        }
        let md = std::fs::read_to_string(out.results_dir.join("probe-png-filter.md")).expect("md");
        assert_eq!(md, crate::probe::render_file(NAME, &json).expect("render"));
        assert!(md.contains("## Gate") && md.contains("### By source group"));
        assert!(check(&env).is_empty(), "{:?}", check(&env));
        // Broken rules are reported.
        let mut bad = env.clone();
        bad.data.classes[1].totals.eligible += 1;
        bad.data.gate.saving += 1;
        bad.data.classes[1].files[0].index = 9;
        let p = check(&bad);
        assert!(p.iter().any(|m| m.contains("/totals")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("/data/gate")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("/index")), "{p:?}");
    }

    /// The validation the design names: the `today` writer equals `Pipeline::balanced` on
    /// `photo-raw-png` with dedup off (kept streams, M bytes, record bytes). Needs the corpus
    /// (`LPK_BENCH_CORPUS` or `bench/corpus/small`); minutes of LZMA.
    #[test]
    #[ignore = "needs the benchmark corpus; run with --ignored"]
    fn the_today_writer_equals_the_balanced_pipeline_on_the_gate_class() {
        let dir = local_corpus();
        let corpus = load_corpus(&dir).expect("corpus");
        let tmp = tempfile::tempdir().expect("tmp");
        let ctx = Ctx {
            corpus: &corpus,
            threads: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
            scratch: tmp.path().to_path_buf(),
            local_tools: PathBuf::from("no-such-local-tools.toml"),
            tool_timeout: Duration::from_secs(60),
        };
        let largest = ctx
            .class_files(GATE_CLASS)
            .expect("class")
            .iter()
            .map(|f| f.bytes)
            .max()
            .unwrap_or(0);
        let workers =
            workers_for(ctx.threads, largest, lpk_core::memory::physical_memory()).expect("w");
        let set = settings(workers);
        let (files, mismatches) = run_class(&ctx, GATE_CLASS, &set).expect("probe");
        assert_eq!(mismatches, 0);
        let mine = today_kept(&files, &set);
        let mut pipeline = lpk_core::Pipeline::balanced(BalancedOptions::default());
        pipeline.fold = None;
        let root = corpus.files_root().join(GATE_CLASS);
        let summary = pipeline.run(&root, std::io::sink()).expect("pipeline");
        let d = &summary.peel.deflate;
        assert_eq!(
            mine,
            (d.outcomes.kept, d.model_bytes, d.record_bytes),
            "probe (kept, model, record) against the pipeline"
        );
    }
}
