//! The Deflate peel in the pipeline (design `docs/notes/e2-10-11-design.md`, sections 1.2, 1.3
//! and 3.2 to 3.9): per file of the classes it applies to, the size cap, whole-file dedup, then
//! [`examine`]; per reconstructed stream the classifier on its inflated text, the dedup probe,
//! the part encoder and the stream gates; then the file gate; then the layout of section 1.3
//! (the framing first, through `add_part`; per kept stream its record and its block through
//! `add_parts_encoded`), or the whole file through `add_file` when nothing is kept.
//!
//! Everything runs on the feeding thread in input order, so the archive does not depend on the
//! thread count (D-60).
//!
//! **Records cap.** The writer holds every record it adds (each with its correction bytes) until
//! `finish`, and then encodes them into one `Records` frame. A stream whose record would take the
//! records this run keeps past [`RECORDS_CAP`] (their encoded bytes, as the frame holds them) is
//! not kept and is counted `records_cap`. The cap is 64 MiB: the memory model counts twice the
//! cap (the held records and the frame's encoding), which stays a small share of every tier's
//! ceiling, and the frame stays far below the reader's default frame limit of 1 GiB; it is the
//! same size as the plain text one stream may hold, so a run reaches it only with very many
//! peeled streams.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::Write;
use std::time::Instant;

use lpk_format::envelope::DEFAULT_MEMORY;
use lpk_format::{DeflateRecord, Encoded, PrimitiveId, Record, RecordBody, Step, Writer};

use super::{examine, memory_term, Counts, Limits, PartEncoder, PartModel};
use crate::classify::{classify, Class};
use crate::error::CoreError;
use crate::ingest::Input;
use crate::peel::Count;

/// The largest file the stage reads whole; a longer one is refused unread (streamed through
/// the normal path and counted as `size cap`).
pub const MAX_FILE: u64 = 256 << 20;
/// The fixed part of a kept stream's cost beyond M's bytes and its record: the frame, the block
/// header with both steps, the index entry, one extra framing chunk record and a sealing tag (an
/// estimate, design 3.3).
pub const STREAM_OVERHEAD: u64 = 160;
/// A file's streams are kept only when their total gain exceeds this (the early plain-block
/// close and the extra framing chunk).
pub const FILE_GATE: u64 = 256;
/// Bytes of an inflated text the classifier reads.
pub const CLASSIFY_LEN: usize = 1 << 20;
/// The most record bytes (as encoded in the `Records` frame) one run keeps (see the module
/// text).
pub const RECORDS_CAP: u64 = 64 << 20;

/// The Deflate peel stage's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeflatePeel {
    /// The largest file read whole ([`MAX_FILE`]).
    pub max_file: u64,
    /// The analysis limits (the probe's, [`Limits::standard`]).
    pub limits: Limits,
    /// Fill [`DeflateSummary::files`] (one record per walked file; for the share test).
    pub record_files: bool,
    /// The records cap ([`RECORDS_CAP`]).
    pub records_cap: u64,
}

impl Default for DeflatePeel {
    fn default() -> Self {
        DeflatePeel {
            max_file: MAX_FILE,
            limits: Limits::standard(),
            record_files: false,
            records_cap: RECORDS_CAP,
        }
    }
}

impl DeflatePeel {
    /// True for the classes the stage walks (`DeflateContainer`, `Png`) or scans for embedded
    /// zlib streams (`Executable`, `Other`, `ImageRaw`, `HighEntropy`); never `Text`, `Jpeg`,
    /// `Audio`, `Video` or `Compressed`.
    pub fn applies_to(&self, class: Class) -> bool {
        matches!(
            class,
            Class::DeflateContainer
                | Class::Png
                | Class::Executable
                | Class::Other
                | Class::ImageRaw
                | Class::HighEntropy
        )
    }

    /// The plain text one stream may hold.
    pub fn plain_limit(&self) -> u64 {
        self.limits.plain_text_limit
    }
}

/// The pipeline's partition of the reconstructed streams: every reconstructed stream is in
/// exactly one bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Outcomes {
    /// Written as a peeled stream: its record and its block.
    pub kept: u64,
    /// The stream gate: M's bytes, the record and the overhead not below the stream's length.
    pub no_gain: u64,
    /// Longer than the tier's block size.
    pub block_cap: u64,
    /// The memory term plus the block size above the reader's default memory.
    pub memory_cap: u64,
    /// Its bytes were stored already: referenced, no record, no block.
    pub already_stored: u64,
    /// Kept or already stored, but the file gate sent the file whole through the normal path.
    pub file_gate: u64,
    /// Would have been kept, but its record would take the run's records past [`RECORDS_CAP`].
    pub records_cap: u64,
}

impl Outcomes {
    /// The sum of the buckets (equals `reconstructed`).
    pub fn total(&self) -> u64 {
        self.kept
            + self.no_gain
            + self.block_cap
            + self.memory_cap
            + self.already_stored
            + self.file_gate
            + self.records_cap
    }

    fn add(&mut self, o: &Outcomes) {
        self.kept += o.kept;
        self.no_gain += o.no_gain;
        self.block_cap += o.block_cap;
        self.memory_cap += o.memory_cap;
        self.already_stored += o.already_stored;
        self.file_gate += o.file_gate;
        self.records_cap += o.records_cap;
    }
}

/// One walked file's streams (only with [`DeflatePeel::record_files`]).
#[derive(Debug, Clone, PartialEq)]
pub struct FileStreams {
    /// The entry path.
    pub path: String,
    /// The walker's kind.
    pub kind: &'static str,
    /// The probe's counts.
    pub counts: Counts,
    /// The pipeline's partition.
    pub outcomes: Outcomes,
    /// Kept streams by the class of their inflated text.
    pub kept_by_class: BTreeMap<&'static str, u64>,
}

/// What the Deflate peel did. Equality ignores the seconds (two runs of the same input compare
/// equal).
#[derive(Debug, Clone, Default)]
pub struct DeflateSummary {
    /// Files walked, by the walker's kind.
    pub walked_by_kind: BTreeMap<&'static str, u64>,
    /// Files written with at least one peeled or referenced stream.
    pub peeled: Count,
    /// Files above the size cap, refused unread.
    pub size_cap: Count,
    /// Whole duplicates: referenced, never walked.
    pub deduplicated: Count,
    /// Walked files with no reconstructed stream.
    pub no_streams: u64,
    /// Walked files with reconstructed streams none of which passed the stream gates.
    pub none_kept: u64,
    /// Walked files sent whole through the normal path by the file gate.
    pub file_gate: u64,
    /// The probe's stream counts, summed over the walked files (walker panics included).
    pub streams: Counts,
    /// The partition of `streams.reconstructed`.
    pub outcomes: Outcomes,
    /// Kept streams by the class of their inflated text.
    pub kept_by_class: BTreeMap<&'static str, u64>,
    /// Bytes of the kept streams (their original compressed bytes).
    pub stream_bytes: u64,
    /// Inflated bytes of the kept streams.
    pub plain_bytes: u64,
    /// M's bytes of the kept streams.
    pub model_bytes: u64,
    /// Correction bytes of the kept streams.
    pub correction_bytes: u64,
    /// Encoded bytes of the kept streams' records in the `Records` frame.
    pub record_bytes: u64,
    /// Part blocks whose M is `lzma`.
    pub lzma_blocks: u64,
    /// Part blocks whose M is `zstd`.
    pub zstd_blocks: u64,
    /// Plain bytes the part encoder's trials fed to each candidate.
    pub sample_bytes: u64,
    /// Seconds in the part encoder (every encoded stream, kept or not).
    pub encode_seconds: f64,
    /// One record per walked file, in input order (`None` unless `record_files`).
    pub files: Option<Vec<FileStreams>>,
}

fn untimed_counts(c: &Counts) -> Counts {
    Counts {
        analyse_seconds: 0.0,
        recreate_seconds: 0.0,
        unreconstructed_seconds: 0.0,
        ..c.clone()
    }
}

impl DeflateSummary {
    /// A copy with every seconds field 0.
    pub fn untimed(&self) -> DeflateSummary {
        DeflateSummary {
            streams: untimed_counts(&self.streams),
            encode_seconds: 0.0,
            files: self.files.as_ref().map(|v| {
                v.iter()
                    .map(|f| FileStreams {
                        counts: untimed_counts(&f.counts),
                        ..f.clone()
                    })
                    .collect()
            }),
            ..self.clone()
        }
    }

    fn key(&self) -> impl PartialEq + '_ {
        (
            (
                &self.walked_by_kind,
                self.peeled,
                self.size_cap,
                self.deduplicated,
                self.no_streams,
                self.none_kept,
                self.file_gate,
            ),
            (&self.outcomes, &self.kept_by_class),
            (
                self.stream_bytes,
                self.plain_bytes,
                self.model_bytes,
                self.correction_bytes,
                self.record_bytes,
                self.lzma_blocks,
                self.zstd_blocks,
                self.sample_bytes,
            ),
        )
    }

    pub(crate) fn note_size_cap(&mut self, bytes: u64) {
        self.size_cap.files += 1;
        self.size_cap.bytes += bytes;
    }
}

impl PartialEq for DeflateSummary {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (self.untimed(), other.untimed());
        a.key() == b.key() && a.streams == b.streams && a.files == b.files
    }
}

/// The exact length of `record` in the `Records` frame: kind, flags, the length varint, the
/// body and its BLAKE3.
fn record_len(record: &Record) -> u64 {
    let body = record.encode().len() as u64;
    let mut v = Vec::new();
    // Writing to a vector cannot fail.
    let _ = lpk_format::varint::write(&mut v, body);
    4 + v.len() as u64 + body + 32
}

fn contiguous<'a>(bytes: &'a [u8], spans: &[(usize, usize)]) -> Cow<'a, [u8]> {
    match spans {
        [(a, b)] => Cow::Borrowed(&bytes[*a..*b]),
        _ => Cow::Owned(
            spans
                .iter()
                .flat_map(|(a, b)| bytes[*a..*b].iter().copied())
                .collect(),
        ),
    }
}

/// The lower-case extension of a path, without the dot.
fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

enum Kind {
    Stored,
    Kept {
        record: DeflateRecord,
        encoded: Encoded,
        class: Class,
        model: PartModel,
        record_len: u64,
    },
}

/// A stream that passed the stream gates.
struct Decided {
    spans: Vec<(usize, usize)>,
    gain: u64,
    kind: Kind,
}

#[cfg(test)]
thread_local! {
    /// Test hook: when set, the pre-encode gate is skipped (every stream past the caps is
    /// encoded), to show it changes nothing but time.
    static NO_PREGATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the exact pre-encode gate runs (always, outside the test hook).
fn pregate() -> bool {
    #[cfg(test)]
    {
        !NO_PREGATE.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        true
    }
}

/// What the stage needs from the run.
pub(crate) struct Context<'a> {
    pub(crate) stage: &'a DeflatePeel,
    pub(crate) encoder: &'a mut PartEncoder,
    pub(crate) block_size: u64,
}

/// Peel one file read whole (at most the size cap) and add it to the open writer.
pub(crate) fn peel_file<O: Write>(
    w: &mut Writer<O>,
    cx: &mut Context<'_>,
    input: &Input,
    data: &[u8],
    s: &mut DeflateSummary,
) -> Result<(), CoreError> {
    // A whole duplicate is referenced and never walked.
    if w.add_file_if_known(&input.path, input.flags, input.mtime_ns, data)? {
        s.deduplicated.files += 1;
        s.deduplicated.bytes += data.len() as u64;
        return Ok(());
    }
    let ext = extension(&input.path);
    let mut decided: Vec<Decided> = Vec::new();
    let mut out = Outcomes::default();
    let mut kept_by_class = BTreeMap::new();
    let mut err: Option<CoreError> = None;
    let block_size = cx.block_size;
    let records_cap = cx.stage.records_cap;
    let encoder = &mut *cx.encoder;
    let examined = examine(data, &ext, &cx.stage.limits, |r| {
        if err.is_some() {
            return;
        }
        let len = r.deflate_len as u64;
        if r.spans.is_empty() || len == 0 {
            out.no_gain += 1;
            return;
        }
        let plain = r.plain.text();
        let class = classify(&plain[..plain.len().min(CLASSIFY_LEN)]);
        let stream = contiguous(data, &r.spans);
        // The dedup probe: single-span streams only (a multi-span stream gets whole-file
        // dedup only).
        if r.spans.len() == 1 {
            match w.part_known(&stream) {
                Ok(Some(_)) => {
                    decided.push(Decided {
                        spans: r.spans,
                        gain: len,
                        kind: Kind::Stored,
                    });
                    return;
                }
                Ok(None) => {}
                Err(e) => {
                    err = Some(e.into());
                    return;
                }
            }
        }
        if len > block_size {
            out.block_cap += 1;
            return;
        }
        let plain_len = plain.len() as u64;
        let term = memory_term(plain_len, r.corrections.len() as u64);
        if term.saturating_add(block_size) > DEFAULT_MEMORY {
            out.memory_cap += 1;
            return;
        }
        let record = DeflateRecord {
            original_len: len,
            plain_len,
            corrections: r.corrections,
            library: 0,
            original_hash: *blake3::hash(&stream).as_bytes(),
        };
        let rec_len = record_len(&Record::new(RecordBody::Deflate(record.clone())));
        // The exact pre-encode gate: when the record and the overhead alone reach the stream's
        // length, no model output can pass the stream gate, so the stream is "no gain" without
        // the encode (the same outcome and the same bytes, only sooner).
        if pregate() && rec_len.saturating_add(STREAM_OVERHEAD) >= len {
            out.no_gain += 1;
            return;
        }
        let t = Instant::now();
        let p = match encoder.encode(plain) {
            Ok(p) => p,
            Err(e) => {
                err = Some(e.into());
                return;
            }
        };
        s.encode_seconds += t.elapsed().as_secs_f64();
        s.sample_bytes += p.sample_bytes;
        let cost = (p.encoded.bytes.len() as u64)
            .saturating_add(rec_len)
            .saturating_add(STREAM_OVERHEAD);
        if cost >= len {
            out.no_gain += 1;
            return;
        }
        let pending: u64 = decided
            .iter()
            .map(|d| match &d.kind {
                Kind::Kept { record_len, .. } => *record_len,
                Kind::Stored => 0,
            })
            .sum();
        if s.record_bytes + pending + rec_len > records_cap {
            out.records_cap += 1;
            return;
        }
        decided.push(Decided {
            spans: r.spans,
            gain: len - cost,
            kind: Kind::Kept {
                record,
                encoded: p.encoded,
                class,
                model: p.model,
                record_len: rec_len,
            },
        });
    });
    if let Some(e) = err {
        return Err(e);
    }
    let gain: u64 = decided.iter().map(|d| d.gain).sum();
    *s.walked_by_kind.entry(examined.kind).or_default() += 1;
    if decided.is_empty() || gain <= FILE_GATE {
        out.file_gate += decided.len() as u64;
        if examined.counts.reconstructed == 0 {
            s.no_streams += 1;
        } else if decided.is_empty() {
            s.none_kept += 1;
        } else {
            s.file_gate += 1;
        }
        w.add_file(&input.path, input.flags, input.mtime_ns, &mut &data[..])?;
    } else {
        write_peeled(w, input, data, decided, &mut out, &mut kept_by_class, s)?;
        s.peeled.files += 1;
        s.peeled.bytes += data.len() as u64;
    }
    s.streams.add(&examined.counts);
    s.outcomes.add(&out);
    if let Some(files) = s.files.as_mut() {
        files.push(FileStreams {
            path: input.path.clone(),
            kind: examined.kind,
            counts: examined.counts,
            outcomes: out,
            kept_by_class,
        });
    }
    Ok(())
}

/// The layout of design 1.3: the framing (every byte outside the written streams) first, in
/// file order, through `add_part`; then per stream in file order a reference to its stored
/// chunks, or its record and its block `[M, deflate-reconstruct {id}]`, whose envelope memory
/// is the record's memory term (spec section 8).
fn write_peeled<O: Write>(
    w: &mut Writer<O>,
    input: &Input,
    data: &[u8],
    decided: Vec<Decided>,
    out: &mut Outcomes,
    file_classes: &mut BTreeMap<&'static str, u64>,
    s: &mut DeflateSummary,
) -> Result<(), CoreError> {
    w.begin_entry(&input.path, input.flags, input.mtime_ns)?;
    let mut spans: Vec<(usize, usize)> = decided.iter().flat_map(|d| d.spans.clone()).collect();
    spans.sort_unstable();
    let mut at = 0;
    for (a, b) in spans
        .into_iter()
        .chain(std::iter::once((data.len(), data.len())))
    {
        if a > at {
            w.add_part(at as u64, &mut &data[at..a])?;
        }
        at = at.max(b);
    }
    for d in decided {
        match d.kind {
            Kind::Stored => {
                out.already_stored += 1;
                let (a, b) = d.spans[0];
                if w.add_part_known(a as u64, &data[a..b])?.is_none() {
                    // Not reachable (stored chunks stay stored); written plainly if it were.
                    w.add_part(a as u64, &mut &data[a..b])?;
                }
            }
            Kind::Kept {
                record,
                encoded,
                class,
                model,
                record_len,
            } => {
                out.kept += 1;
                *s.kept_by_class.entry(class.as_str()).or_default() += 1;
                *file_classes.entry(class.as_str()).or_default() += 1;
                s.stream_bytes += record.original_len;
                s.plain_bytes += record.plain_len;
                s.model_bytes += encoded.bytes.len() as u64;
                s.correction_bytes += record.corrections.len() as u64;
                s.record_bytes += record_len;
                match model {
                    PartModel::Lzma => s.lzma_blocks += 1,
                    PartModel::Zstd => s.zstd_blocks += 1,
                }
                let term = memory_term(record.plain_len, record.corrections.len() as u64);
                let id = w.add_record(Record::new(RecordBody::Deflate(record)))?;
                let mut params = Vec::new();
                lpk_format::varint::write(&mut params, id)?;
                let mut graph = encoded.graph;
                graph.steps.push(Step {
                    primitive: PrimitiveId::DeflateReconstruct,
                    params,
                });
                let block = Encoded {
                    resources: graph.resources(),
                    memory: encoded.memory,
                    graph,
                    bytes: encoded.bytes,
                };
                let stream = contiguous(data, &d.spans);
                let parts: Vec<(u64, u64)> = d
                    .spans
                    .iter()
                    .map(|&(a, b)| (a as u64, (b - a) as u64))
                    .collect();
                w.add_parts_encoded(&parts, &stream, block, term)?;
            }
        }
    }
    w.end_entry()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_and_record_length() {
        assert_eq!(extension("a/b/Doc.DOCX"), "docx");
        assert_eq!(extension("a.b/noext"), "");
        let r = Record::new(RecordBody::Deflate(DeflateRecord {
            original_len: 10,
            plain_len: 20,
            corrections: vec![1; 300],
            library: 0,
            original_hash: [0; 32],
        }));
        let frame = lpk_format::RecordsWriter::encode(std::slice::from_ref(&r));
        // The frame's count varint (one byte for one record) plus the record.
        assert_eq!(frame.len() as u64, 1 + record_len(&r));
    }

    /// The pre-encode gate changes neither the archive nor the summary (sample bytes and
    /// seconds aside, which count the encodes it saves).
    #[test]
    fn the_pre_encode_gate_changes_nothing_but_time() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deflate");
        for name in ["doc.zip", "image.png", "notes.txt.gz"] {
            std::fs::copy(fixtures.join(name), dir.path().join(name)).unwrap();
        }
        // Small members: the record and the overhead alone exceed some of them.
        for (i, len) in [40usize, 120, 400, 1500, 6000].into_iter().enumerate() {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
            e.write_all(&b"lorem ipsum dolor sit amet ".repeat(len / 27 + 1)[..len])
                .unwrap();
            std::fs::write(dir.path().join(format!("s{i}.gz")), e.finish().unwrap()).unwrap();
        }
        let run = |skip: bool| {
            NO_PREGATE.with(|c| c.set(skip));
            let mut p = crate::Pipeline::balanced(crate::BalancedOptions {
                block_size: 4 << 20,
                dict_size: 1 << 20,
                sample_len: 1 << 16,
                threads: Some(1),
                ..crate::BalancedOptions::default()
            });
            p.deflate = Some(DeflatePeel {
                record_files: true,
                ..DeflatePeel::default()
            });
            let mut out = Vec::new();
            let s = p.run(dir.path(), &mut out).unwrap();
            NO_PREGATE.with(|c| c.set(false));
            (out, s.peel.deflate)
        };
        let (with, a) = run(false);
        let (without, b) = run(true);
        assert!(with == without, "the archives differ");
        assert!(a.outcomes.no_gain >= 2, "{a:?}");
        assert_eq!(a.outcomes, b.outcomes);
        assert_eq!(a.untimed().files, b.untimed().files);
        assert!(a.sample_bytes < b.sample_bytes, "the gate saved encodes");
    }

    #[test]
    fn the_stage_applies_to_the_designs_classes() {
        let d = DeflatePeel::default();
        for c in [
            Class::DeflateContainer,
            Class::Png,
            Class::Executable,
            Class::Other,
            Class::ImageRaw,
            Class::HighEntropy,
        ] {
            assert!(d.applies_to(c), "{c:?}");
        }
        for c in [
            Class::Text,
            Class::Jpeg,
            Class::Audio,
            Class::Video,
            Class::Compressed,
        ] {
            assert!(!d.applies_to(c), "{c:?}");
        }
        assert_eq!(d.max_file, 256 << 20);
        assert_eq!(d.plain_limit(), super::super::PLAIN_TEXT_LIMIT);
    }
}
