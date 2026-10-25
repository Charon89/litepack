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
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use lpk_format::envelope::DEFAULT_MEMORY;
use lpk_format::{
    DeflateRecord, Encoded, PngFilterRecord, PrimitiveId, Record, RecordBody, Step, Writer,
};

use super::{examine_with, memory_term, Counts, Limits, PartEncoder, PartModel, Reconstructed};
use crate::classify::{classify, Class};
use crate::error::CoreError;
use crate::ingest::Input;
use crate::peel::png::{Header, FILTER_UP};
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
/// The file share rule: a file's streams are kept only when their total gain is at least
/// `1 / FILE_SHARE` of the file's length. Peeling a file closes the open solid block (a part
/// block takes the next chunk indices) and codes each kept stream alone, so the file's framing
/// and its streams lose the long-range context the normal path gives them across files (the
/// same members zipped at several levels, versions of a container). That loss grows with the
/// file's length, not with the streams' gains, so a gain that is a small share of the file
/// does not pay for it; a file that fails the rule goes whole through the normal path.
pub const FILE_SHARE: u64 = 32;
/// Deflate's largest expansion, plain bytes per compressed byte (a 258-byte match per
/// two-bit code, rounded up): a file of `n` bytes holds no stream whose text exceeds
/// `MAX_EXPANSION × n`, which sizes the memory the part encoder plans.
pub const MAX_EXPANSION: u64 = 1032;
/// Bytes of an inflated text the classifier reads.
pub const CLASSIFY_LEN: usize = 1 << 20;
/// The most record bytes (as encoded in the `Records` frame) one run keeps (see the module
/// text).
pub const RECORDS_CAP: u64 = 64 << 20;
/// The bytes counted for a `png-filter` graph step in a stream's cost: an upper bound (ID 2,
/// flags 1, params length 1, a record id varint below 2^21), as E2-16's probe counted it.
pub const PNG_STEP_BYTES: u64 = 7;

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
    /// The exact skips of design `docs/notes/e2-10-peel-pool-design.md` section 3 (no encode
    /// for a stream of a file whose bound decides it; the archive is the same either way).
    pub exact_skips: bool,
    /// The peel pool's hints (design section 1: a worker skips what the commits already wrote;
    /// the archive is the same either way).
    pub hints: bool,
    /// The PNG row-filter re-encoding (E2-16, revision 1.4): a PNG's IDAT stream is also encoded
    /// with every scanline filtered with Up, and written `[M, png-filter, deflate-reconstruct]`
    /// when that costs less, side information counted ([`png_candidate`] for the images tried).
    /// Off by default; [`crate::Pipeline::balanced`] turns it on. Needs an archive of minor 4.
    pub png_filter: bool,
}

impl Default for DeflatePeel {
    fn default() -> Self {
        DeflatePeel {
            max_file: MAX_FILE,
            limits: Limits::standard(),
            record_files: false,
            records_cap: RECORDS_CAP,
            exact_skips: true,
            hints: true,
            png_filter: false,
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
    /// Kept or already stored, past the file gate, but the file share rule
    /// ([`FILE_SHARE`]) sent the file whole through the normal path.
    pub file_share: u64,
    /// Would have been kept, but its record would take the run's records past [`RECORDS_CAP`].
    pub records_cap: u64,
    /// Not encoded: its file's bound (design `docs/notes/e2-10-peel-pool-design.md`, section 3)
    /// sent the file whole through the normal path whatever the encode would say.
    pub file_bound: u64,
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
            + self.file_share
            + self.records_cap
            + self.file_bound
    }

    fn add(&mut self, o: &Outcomes) {
        self.kept += o.kept;
        self.no_gain += o.no_gain;
        self.block_cap += o.block_cap;
        self.memory_cap += o.memory_cap;
        self.already_stored += o.already_stored;
        self.file_gate += o.file_gate;
        self.file_share += o.file_share;
        self.records_cap += o.records_cap;
        self.file_bound += o.file_bound;
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
    /// Walked files sent whole through the normal path by the file share rule.
    pub file_share: u64,
    /// Walked files sent whole through the normal path by their bound, without the encodes
    /// (design `docs/notes/e2-10-peel-pool-design.md`, section 3).
    pub file_bound: u64,
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
    /// Kept streams written with their row filters re-encoded (`png-filter`, revision 1.4).
    pub png_filtered: u64,
    /// Encoded bytes of their `png-filter` records (also counted in `record_bytes`).
    pub png_record_bytes: u64,
    /// Streams whose Up text was encoded too (a PNG image the re-encoding tries), kept or not.
    pub png_tried: u64,
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
                self.file_share,
                self.file_bound,
            ),
            (&self.outcomes, &self.kept_by_class),
            (
                self.stream_bytes,
                self.plain_bytes,
                self.model_bytes,
                self.correction_bytes,
                self.record_bytes,
                self.png_filtered,
                self.png_record_bytes,
                self.png_tried,
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
/// body and its BLAKE3 (public for `probe png-filter`, which counts side information with it).
pub fn record_len(record: &Record) -> u64 {
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

/// Which file gate a file's streams fail, if any: `gain` is the sum of their gains, `len` the
/// file's length, `kept` whether at least one of them would be written as a peeled block (the
/// share rule applies only then: a file whose streams are all already stored closes no block
/// and codes nothing alone, so it keeps its references).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileVerdict {
    /// Keep the streams.
    Peel,
    /// The total gain does not exceed [`FILE_GATE`].
    Gate,
    /// A stream is kept and the total gain is below `1 / FILE_SHARE` of the file's length.
    Share,
}

fn file_verdict(gain: u64, len: u64, kept: bool) -> FileVerdict {
    if gain <= FILE_GATE {
        FileVerdict::Gate
    } else if kept && gain.saturating_mul(FILE_SHARE) < len {
        FileVerdict::Share
    } else {
        FileVerdict::Peel
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
        png: Option<Box<(PngFilterRecord, u64)>>,
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

impl Context<'_> {
    /// The settings [`prepare`] reads, taken on the feeding thread.
    pub(crate) fn settings(&self) -> Settings {
        Settings::of(self.stage, self.block_size)
    }
}

impl Settings {
    /// The settings of `stage` with the tier's `block_size`, taken on the feeding thread.
    pub(crate) fn of(stage: &DeflatePeel, block_size: u64) -> Settings {
        Settings {
            limits: stage.limits.clone(),
            block_size,
            pregate: pregate(),
            skips: stage.exact_skips,
            share: true,
            png_filter: stage.png_filter,
        }
    }
}

/// Everything [`prepare`] reads besides the file's bytes (design
/// `docs/notes/e2-10-peel-pool-design.md`, section 1): no writer state.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub(crate) limits: Limits,
    pub(crate) block_size: u64,
    pub(crate) pregate: bool,
    /// The exact skips of design section 3 ([`DeflatePeel::exact_skips`]).
    pub(crate) skips: bool,
    /// With `skips`, the share bound too (off when a share-bound file with a stored stream is
    /// prepared again; the gate bound stays).
    pub(crate) share: bool,
    /// The PNG row-filter re-encoding ([`DeflatePeel::png_filter`]).
    pub(crate) png_filter: bool,
}

/// An error or a panic met while preparing, raised by the commit only where the sequential
/// run would have met it.
pub(crate) enum Failure {
    /// An error (of the part encoder, or of reading the file).
    Error(CoreError),
    /// A panic's payload, resumed on the feeding thread.
    Panic(Box<dyn std::any::Any + Send + 'static>),
}

impl Failure {
    /// Raise it: return the error, or resume the panic.
    pub(crate) fn raise<T>(self) -> Result<T, CoreError> {
        match self {
            Failure::Error(e) => Err(e),
            Failure::Panic(p) => std::panic::resume_unwind(p),
        }
    }
}

impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Error(e) => write!(f, "Error({e})"),
            Failure::Panic(_) => f.write_str("Panic"),
        }
    }
}

/// A stream's model encoding with what the commit needs to judge it.
#[derive(Debug)]
pub(crate) struct Coded {
    /// The `png-filter` record and its cost (record and step) when the Up text won.
    png: Option<(PngFilterRecord, u64)>,
    /// The Up text was encoded too.
    png_tried: bool,
    record: DeflateRecord,
    record_len: u64,
    class: Class,
    encoded: Encoded,
    model: PartModel,
    sample_bytes: u64,
    seconds: f64,
}

/// The pure verdict on one reconstructed stream (design section 1): every check that depends
/// only on the file's bytes and the settings, in the sequential order, without the dedup probe
/// and the records cap, which the commit applies.
#[derive(Debug)]
pub(crate) enum Verdict {
    /// No spans or no bytes: no gain, before the probe.
    Empty,
    /// Longer than the block size.
    BlockCap,
    /// The memory term plus the block size above the reader's default memory.
    MemoryCap,
    /// The record and the overhead alone reach the stream's length.
    PreNoGain,
    /// The oracle found the spans stored: nothing past it was computed.
    Skipped,
    /// Not encoded: the file's bound ([`Prepared::bound`]) decides the file without it.
    Bound,
    /// Encoded, and M's bytes, the record and the overhead reach the stream's length: only what
    /// the summary counts of the encode is kept (the stream gate is pure).
    NoGain {
        /// The trial's sample bytes.
        sample_bytes: u64,
        /// Seconds in the part encoder.
        seconds: f64,
        /// The Up text was encoded too.
        png_tried: bool,
    },
    /// Encoded, and below the stream's length.
    Encoded(Box<Coded>),
    /// The encode failed or panicked.
    Failed(Failure),
}

/// A bound on a file's gain that decides the file without encodes (design section 3): G, the
/// lengths of the streams reconstructed so far plus the span lengths of the candidates left,
/// bounds the gain of its streams (a stream gains at most its length).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bound {
    /// G ≤ [`FILE_GATE`]: the file gate fails whatever the encodes say.
    Gate,
    /// `FILE_SHARE × G` < the file's length and no stream was found stored: the share rule
    /// fails for any kept stream.
    Share,
}

/// One reconstructed stream, prepared.
#[derive(Debug)]
pub(crate) struct PreparedStream {
    spans: Vec<(usize, usize)>,
    len: u64,
    verdict: Verdict,
}

/// One file, prepared: the walker's kind, the probe's counts and its reconstructed streams in
/// file order.
#[derive(Debug)]
pub(crate) struct Prepared {
    kind: &'static str,
    counts: Counts,
    streams: Vec<PreparedStream>,
    /// The hooks stopped the examination (a cancelled run): the result is incomplete.
    pub(crate) stopped: bool,
    /// The bound that left streams unencoded ([`Verdict::Bound`]), if any.
    bound: Option<Bound>,
    /// Not examined: a hint says the file is stored whole already (the commit checks).
    pub(crate) whole_hint: bool,
}

impl Prepared {
    /// The result for a file a hint says is stored whole (nothing examined).
    pub(crate) fn hinted_whole() -> Prepared {
        Prepared {
            kind: "other",
            counts: Counts::default(),
            streams: Vec::new(),
            stopped: false,
            bound: None,
            whole_hint: true,
        }
    }
}

impl Prepared {
    /// The sample bytes of the encodes whose result the commit did not use (counted outside
    /// [`DeflateSummary`]).
    fn unused_encodes(&self) -> impl Iterator<Item = u64> + '_ {
        self.streams.iter().filter_map(|s| match &s.verdict {
            Verdict::Encoded(c) => Some(c.sample_bytes),
            Verdict::NoGain { sample_bytes, .. } => Some(*sample_bytes),
            _ => None,
        })
    }
}

/// What a pool asks of [`prepare`] around each stream (design section 2): the reservation of
/// the stream's analysis and encode, and the cancellation of a dropped run.
pub(crate) trait Hooks {
    /// The candidates are known (before any analysis).
    fn walked(&mut self, candidates: &[super::Candidate]) {
        let _ = candidates;
    }
    /// A candidate whose text is at most `plain` bytes is about to be analysed; false stops.
    fn analysing(&mut self, plain: u64) -> bool {
        let _ = plain;
        true
    }
    /// The analysis and the encode of the last candidate are over.
    fn finished(&mut self) {}
}

/// No hooks (the sequential path).
pub(crate) struct NoHooks;

impl Hooks for NoHooks {}

/// The skip-only oracle of [`prepare`]: whether the stream at these spans of the file is stored.
pub(crate) type Oracle<'a> = dyn FnMut(&[u8], &[(usize, usize)]) -> bool + 'a;

struct Prep<'a, H: Hooks> {
    data: &'a [u8],
    #[cfg_attr(not(test), allow(dead_code))]
    path: &'a str,
    set: &'a Settings,
    encoder: &'a mut PartEncoder,
    oracle: &'a mut Oracle<'a>,
    hooks: &'a mut H,
    streams: Vec<PreparedStream>,
    /// Span lengths of the candidates after each index (`left[i]`: candidates `i..`).
    left: Vec<u64>,
    /// The candidate being analysed.
    index: usize,
    /// Lengths of the streams reconstructed so far.
    reconstructed: u64,
    /// The file's bound, once it holds.
    bound: Option<Bound>,
    /// An encode failed: the encoder is replaced before the next encode.
    stale: bool,
    /// The hooks stopped the examination.
    stopped: bool,
}

impl<H: Hooks> super::analyse::Visitor for Prep<'_, H> {
    fn walked(&mut self, candidates: &[super::Candidate]) {
        let mut left = vec![0u64; candidates.len() + 1];
        for (i, c) in candidates.iter().enumerate().rev() {
            let len: u64 = c.spans.iter().map(|(a, b)| (b - a) as u64).sum();
            left[i] = left[i + 1].saturating_add(len);
        }
        self.left = left;
        self.hooks.walked(candidates);
    }

    fn analysing(&mut self, index: usize, plain: u64) -> bool {
        self.index = index;
        self.hooks.finished();
        let go = self.hooks.analysing(plain);
        self.stopped |= !go;
        go
    }

    fn done(&mut self, mut r: Reconstructed) {
        self.reconstructed = self.reconstructed.saturating_add(r.deflate_len as u64);
        // The file's bound (design section 3), a function of the file's bytes alone: updated
        // for every reconstructed stream, whatever the oracle says of it.
        if self.set.skips {
            let g = self
                .reconstructed
                .saturating_add(self.left.get(self.index + 1).copied().unwrap_or(0));
            if g <= FILE_GATE {
                self.bound = Some(Bound::Gate);
            } else if self.bound.is_none()
                && self.set.share
                && g.saturating_mul(FILE_SHARE) < self.data.len() as u64
            {
                self.bound = Some(Bound::Share);
            }
        }
        let verdict = self.verdict(&mut r);
        self.hooks.finished();
        self.streams.push(PreparedStream {
            spans: r.spans,
            len: r.deflate_len as u64,
            verdict,
        });
    }
}

impl<H: Hooks> Prep<'_, H> {
    fn verdict(&mut self, r: &mut Reconstructed) -> Verdict {
        let data = self.data;
        let len = r.deflate_len as u64;
        if r.spans.is_empty() || len == 0 {
            return Verdict::Empty;
        }
        // The skip-only oracle: a stream it finds stored is not encoded (the commit checks).
        if (self.oracle)(data, &r.spans) {
            return Verdict::Skipped;
        }
        if len > self.set.block_size {
            return Verdict::BlockCap;
        }
        let plain = r.plain.text();
        let plain_len = plain.len() as u64;
        let term = memory_term(plain_len, r.corrections.len() as u64);
        if term.saturating_add(self.set.block_size) > DEFAULT_MEMORY {
            return Verdict::MemoryCap;
        }
        let class = classify(&plain[..plain.len().min(CLASSIFY_LEN)]);
        let stream = contiguous(data, &r.spans);
        let record = DeflateRecord {
            original_len: len,
            plain_len,
            corrections: std::mem::take(&mut r.corrections),
            library: 0,
            original_hash: *blake3::hash(&stream).as_bytes(),
        };
        let record_len = record_len(&Record::new(RecordBody::Deflate(record.clone())));
        // The exact pre-encode gate: when the record and the overhead alone reach the stream's
        // length, no model output can pass the stream gate, so the stream is "no gain" without
        // the encode (the same outcome and the same bytes, only sooner).
        if self.set.pregate && record_len.saturating_add(STREAM_OVERHEAD) >= len {
            return Verdict::PreNoGain;
        }
        // The exact skips (design section 3): once the file's bound holds, no encode is needed.
        if self.bound.is_some() {
            return Verdict::Bound;
        }
        // No state of a failed encode is carried into the next one. A replacement that fails is
        // retried at the next encode; this stream is left unencoded and unknown (`Skipped`): the
        // commit, if it finds it not stored, prepares the file again on the feeding thread.
        if self.stale {
            match fresh_in(self.encoder, self.path) {
                Ok(e) => {
                    *self.encoder = e;
                    self.stale = false;
                }
                Err(_) => return Verdict::Skipped,
            }
        }
        let t = Instant::now();
        let (encoder, path) = (&mut *self.encoder, self.path);
        let r = catch_unwind(AssertUnwindSafe(|| encode_hook(encoder, path, plain)));
        let seconds = t.elapsed().as_secs_f64();
        let mut seconds = seconds;
        let failure = match r {
            Ok(Ok(mut p)) => {
                let mut cost = (p.encoded.bytes.len() as u64)
                    .saturating_add(record_len)
                    .saturating_add(STREAM_OVERHEAD);
                let mut sample_bytes = p.sample_bytes;
                let mut png = None;
                // The PNG row-filter re-encoding (E2-16): the Up text through the same encoder,
                // kept when it costs less with its record and step counted.
                let candidate = if self.set.png_filter {
                    png_candidate(data, plain)
                } else {
                    None
                };
                let png_tried = candidate.is_some();
                if let Some((rec, up)) = candidate {
                    let side = record_len_png(&rec).saturating_add(PNG_STEP_BYTES);
                    let t = Instant::now();
                    let (encoder, path) = (&mut *self.encoder, self.path);
                    let q = catch_unwind(AssertUnwindSafe(|| encode_hook(encoder, path, &up)));
                    seconds += t.elapsed().as_secs_f64();
                    match q {
                        Ok(Ok(q)) => {
                            sample_bytes += q.sample_bytes;
                            let cost_b = (q.encoded.bytes.len() as u64)
                                .saturating_add(record_len)
                                .saturating_add(side)
                                .saturating_add(STREAM_OVERHEAD);
                            if cost_b < cost {
                                cost = cost_b;
                                p = q;
                                png = Some((rec, side));
                            }
                        }
                        Ok(Err(e)) => {
                            self.stale = true;
                            return Verdict::Failed(Failure::Error(e.into()));
                        }
                        Err(e) => {
                            self.stale = true;
                            return Verdict::Failed(Failure::Panic(e));
                        }
                    }
                }
                if cost >= len {
                    return Verdict::NoGain {
                        sample_bytes,
                        seconds,
                        png_tried,
                    };
                }
                return Verdict::Encoded(Box::new(Coded {
                    png,
                    png_tried,
                    record,
                    record_len,
                    class,
                    encoded: p.encoded,
                    model: p.model,
                    sample_bytes,
                    seconds,
                }));
            }
            Ok(Err(e)) => Failure::Error(e.into()),
            Err(p) => Failure::Panic(p),
        };
        self.stale = true;
        Verdict::Failed(failure)
    }
}

/// The encoded length of a `png-filter` record in the `Records` frame.
fn record_len_png(rec: &PngFilterRecord) -> u64 {
    record_len(&Record::new(RecordBody::PngFilter(rec.clone())))
}

/// Whether a reconstructed stream of `data` is a PNG image the row-filter re-encoding tries,
/// and then its `png-filter` record and the Up text (every scanline filtered with Up): `data` is
/// a PNG whose IHDR gives compression and filter method 0, an allowed pair, a non-empty image
/// at depth 8 or 16 and not a palette (the PNG specification recommends filter None below
/// depth 8 and for palettes, and E2-16's probe found no gain there), the stream's inflated text
/// `plain` is exactly the image's filtered length with every filter byte at most 4 and not all
/// 0 (an image filtered with None throughout: no gain in the probe either), and the Up text
/// re-filters back to `plain` byte for byte (the decoder's own code, checked before it is kept).
pub fn png_candidate(data: &[u8], plain: &[u8]) -> Option<(PngFilterRecord, Vec<u8>)> {
    let l = Header::parse(data).ok()?.layout().ok()?;
    if l.bit_depth() < 8 || l.color_type() == 3 {
        return None;
    }
    if l.filtered_len()? != plain.len() as u64 {
        return None;
    }
    let filters = l.filter_bytes(plain).ok()?;
    if filters.iter().all(|&f| f == 0) || filters.iter().any(|&f| f > 4) {
        return None;
    }
    let up = l.transcode(plain, &vec![FILTER_UP; filters.len()]).ok()?;
    if l.transcode(&up, &filters).ok()? != plain {
        return None;
    }
    Some((
        PngFilterRecord {
            width: l.width(),
            height: l.height(),
            bit_depth: l.bit_depth(),
            color_type: l.color_type(),
            interlace: l.interlace(),
            filters,
            original_hash: *blake3::hash(plain).as_bytes(),
        },
        up,
    ))
}

/// Test hook, for every thread (pool workers too): the part encoder fails on the texts that
/// start with the prefix (empty: every text) of the file with this entry path (`false`: an
/// error, `true`: a panic). Tests use paths no other test uses.
#[cfg(test)]
pub(crate) static FAIL_ON: std::sync::Mutex<Vec<(String, Vec<u8>, bool)>> =
    std::sync::Mutex::new(Vec::new());

/// Test hook, for every thread: replacing the part encoder while preparing the file with this
/// entry path fails this many more times.
#[cfg(test)]
pub(crate) static FRESH_FAIL_ON: std::sync::Mutex<Vec<(String, usize)>> =
    std::sync::Mutex::new(Vec::new());

/// Test hook for the part encoder failing (see `FAIL_ON`); the plain encode outside tests.
fn encode_hook(
    encoder: &mut PartEncoder,
    path: &str,
    plain: &[u8],
) -> Result<super::PartEncoded, lpk_format::FormatError> {
    #[cfg(test)]
    {
        let fail = FAIL_ON
            .lock()
            .map(|f| {
                f.iter()
                    .find(|(p, x, _)| p == path && plain.starts_with(x))
                    .map(|(_, _, panic)| *panic)
            })
            .unwrap_or(None);
        match fail {
            Some(true) => panic!("injected part encoder panic"),
            Some(false) => {
                return Err(lpk_format::FormatError::Io(std::io::Error::other(
                    "injected part encoder error",
                )))
            }
            None => {}
        }
    }
    #[cfg(not(test))]
    let _ = path;
    encoder.encode(plain)
}

/// The replacement of a part encoder while preparing the file `path` (see `FRESH_FAIL_ON`).
fn fresh_in(encoder: &PartEncoder, path: &str) -> Result<PartEncoder, CoreError> {
    #[cfg(test)]
    {
        let mut f = FRESH_FAIL_ON.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, n)) = f.iter_mut().find(|(p, n)| p == path && *n > 0) {
            *n -= 1;
            return Err(CoreError::InvalidOption("injected fresh failure".into()));
        }
    }
    #[cfg(not(test))]
    let _ = path;
    fresh_hook(encoder)
}

// Test hook: with `Some(n)`, `PartEncoder::fresh` succeeds n more times on this thread, then
// fails every time.
#[cfg(test)]
thread_local! {
    pub(crate) static FRESH_FAILS_AFTER: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// `PartEncoder::fresh`, failing on the test hook (see `FRESH_FAILS_AFTER`; this thread only).
pub(crate) fn fresh_hook(encoder: &PartEncoder) -> Result<PartEncoder, CoreError> {
    #[cfg(test)]
    {
        let fail = FRESH_FAILS_AFTER.with(|f| match f.get() {
            Some(0) => true,
            Some(n) => {
                f.set(Some(n - 1));
                false
            }
            None => false,
        });
        if fail {
            return Err(CoreError::InvalidOption("injected fresh failure".into()));
        }
    }
    encoder.fresh()
}

/// The pure half of the peel of one file (design section 1): `examine` and, per reconstructed
/// stream, every check that depends only on the file's bytes and `set`, and the encode. `path`
/// is the entry path (its extension labels ZIP kinds). The `oracle` may only skip work: a
/// stream all of whose spans it calls stored is not encoded ([`Verdict::Skipped`]); the commit
/// re-checks every stream against the writer.
pub(crate) fn prepare(
    data: &[u8],
    path: &str,
    set: &Settings,
    encoder: &mut PartEncoder,
    oracle: &mut Oracle<'_>,
    hooks: &mut impl Hooks,
) -> Prepared {
    let ext = extension(path);
    let mut p = Prep {
        data,
        path,
        set,
        encoder,
        oracle,
        hooks,
        streams: Vec::new(),
        left: Vec::new(),
        index: 0,
        reconstructed: 0,
        bound: None,
        stale: false,
        stopped: false,
    };
    let examined = examine_with(data, &ext, &set.limits, &mut p);
    p.hooks.finished();
    Prepared {
        kind: examined.kind,
        counts: examined.counts,
        streams: p.streams,
        stopped: p.stopped,
        bound: p.bound,
        whole_hint: false,
    }
}

/// The writer's own probe as the oracle (exact at the commit point: the sequential path).
fn writer_oracle<O: Write>(w: &mut Writer<O>) -> impl FnMut(&[u8], &[(usize, usize)]) -> bool + '_ {
    move |data: &[u8], spans: &[(usize, usize)]| {
        spans
            .iter()
            .all(|&(a, b)| matches!(w.part_known(&data[a..b]), Ok(Some(_))))
    }
}

/// What the commit decided for one file's streams.
enum Decision {
    /// A stream the oracle skipped is not stored now, or a bound file's stored and unencoded
    /// streams mix: prepare the file again with no skips.
    Rerun,
    Ready {
        decided: Vec<Decided>,
        out: Outcomes,
        sample_bytes: u64,
        encode_seconds: f64,
        /// A stream left unencoded by the file's bound was not stored.
        bound_hit: bool,
        /// Streams whose Up text was encoded too.
        png_tried: u64,
    },
}

/// The sequential walk over one file's prepared streams (design section 1): empty, probe, then
/// the pure verdict, then the records cap against the run's record bytes.
fn decide<O: Write>(
    w: &mut Writer<O>,
    data: &[u8],
    prepared: &mut Prepared,
    record_bytes: u64,
    records_cap: u64,
) -> Result<Decision, CoreError> {
    let mut decided: Vec<Decided> = Vec::new();
    let mut out = Outcomes::default();
    let (mut sample_bytes, mut encode_seconds) = (0u64, 0f64);
    let mut pending = 0u64;
    let mut png_tried = 0u64;
    let mut bound_hit = false;
    for st in &mut prepared.streams {
        if matches!(st.verdict, Verdict::Empty) {
            out.no_gain += 1;
            continue;
        }
        // The dedup probe: span by span, the stream is referenced when every span's chunks are
        // stored already (a multi-span stream is all or nothing, as `add_parts_encoded` writes
        // it: a copy of a peeled PNG, whose file is not cut as a whole, is found here).
        let mut known = true;
        for &(a, b) in &st.spans {
            if w.part_known(&data[a..b])?.is_none() {
                known = false;
                break;
            }
        }
        if known {
            decided.push(Decided {
                spans: std::mem::take(&mut st.spans),
                gain: st.len,
                kind: Kind::Stored,
            });
            // Its verdict stays: an encode left here is work the sequential run skipped.
            continue;
        }
        // Taken: what is left in the file's verdicts afterwards is unused work.
        match std::mem::replace(&mut st.verdict, Verdict::Empty) {
            Verdict::Empty => {}
            Verdict::Skipped => return Ok(Decision::Rerun),
            Verdict::Bound => {
                out.file_bound += 1;
                bound_hit = true;
            }
            Verdict::BlockCap => out.block_cap += 1,
            Verdict::MemoryCap => out.memory_cap += 1,
            Verdict::PreNoGain => out.no_gain += 1,
            Verdict::Failed(f) => return f.raise(),
            Verdict::NoGain {
                sample_bytes: b,
                seconds,
                png_tried: tried,
            } => {
                encode_seconds += seconds;
                sample_bytes += b;
                png_tried += u64::from(tried);
                out.no_gain += 1;
            }
            Verdict::Encoded(c) => {
                let c = *c;
                encode_seconds += c.seconds;
                sample_bytes += c.sample_bytes;
                png_tried += u64::from(c.png_tried);
                let side = c.png.as_ref().map_or(0, |(_, side)| *side);
                let cost = (c.encoded.bytes.len() as u64)
                    .saturating_add(c.record_len)
                    .saturating_add(side)
                    .saturating_add(STREAM_OVERHEAD);
                if cost >= st.len {
                    out.no_gain += 1;
                    continue;
                }
                let records = c.record_len + side.saturating_sub(PNG_STEP_BYTES);
                if record_bytes + pending + records > records_cap {
                    out.records_cap += 1;
                    continue;
                }
                pending += records;
                decided.push(Decided {
                    spans: std::mem::take(&mut st.spans),
                    gain: st.len - cost,
                    kind: Kind::Kept {
                        png: c.png.map(Box::new),
                        record: c.record,
                        encoded: c.encoded,
                        class: c.class,
                        model: c.model,
                        record_len: c.record_len,
                    },
                });
            }
        }
    }
    // A file bound by the share rule goes whole only when none of its streams is stored (then
    // a stream that would be kept fails the rule, and with none kept nothing is written
    // either); with a stored one the unencoded streams decide, so it is prepared again.
    if bound_hit
        && prepared.bound == Some(Bound::Share)
        && decided.iter().any(|d| matches!(d.kind, Kind::Stored))
    {
        return Ok(Decision::Rerun);
    }
    Ok(Decision::Ready {
        decided,
        out,
        sample_bytes,
        encode_seconds,
        bound_hit,
        png_tried,
    })
}

/// Work the commit did not use, and work the hints saved (design section 1: counted outside
/// [`DeflateSummary`]'s equality, like the seconds).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Waste {
    /// Files prepared and then referenced whole (`add_file_if_known`).
    pub files: u64,
    /// Streams encoded and then found stored by the commit's probe.
    pub encodes: u64,
    /// The trial's sample bytes of those encodes.
    pub sample_bytes: u64,
    /// Files prepared again on the feeding thread (a skip the commit did not confirm, a bound
    /// file whose stored and unencoded streams mix, or a forced miss).
    pub reruns: u64,
}

/// Where the commit takes a file's prepared result from.
pub(crate) enum Source {
    /// Prepare it now, on the feeding thread, with the writer's probe as the oracle.
    Inline,
    /// Prepared by a pool worker (or its failure).
    Ready(Box<Result<Prepared, Failure>>),
}

/// What a commit wrote that later copies can be found by (design section 1, hints): the file
/// whole, or its streams as parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Published {
    /// The BLAKE3 and length of the file, when it was written or referenced whole.
    pub(crate) whole: Option<([u8; 32], u64)>,
    /// Per stream written as a part block or referenced: the BLAKE3 of its bytes and its span
    /// lengths.
    pub(crate) streams: Vec<StreamKey>,
}

/// A stream's hint key: the BLAKE3 of its bytes and its span lengths.
pub(crate) type StreamKey = ([u8; 32], Vec<u64>);

/// The hint key of the stream at `spans` of `data`.
pub(crate) fn stream_key(data: &[u8], spans: &[(usize, usize)]) -> StreamKey {
    (
        *blake3::hash(&contiguous(data, spans)).as_bytes(),
        spans.iter().map(|&(a, b)| (b - a) as u64).collect(),
    )
}

#[cfg(test)]
thread_local! {
    /// Test hook: the commit treats every pool result as a miss and prepares the file again.
    pub(crate) static FORCE_RERUN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn force_rerun() -> bool {
    #[cfg(test)]
    {
        FORCE_RERUN.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Peel one file read whole (at most the size cap) and add it to the open writer, on the
/// feeding thread: prepare and commit back to back.
pub(crate) fn peel_file<O: Write>(
    w: &mut Writer<O>,
    cx: &mut Context<'_>,
    input: &Input,
    data: &[u8],
    s: &mut DeflateSummary,
) -> Result<(), CoreError> {
    commit(
        w,
        cx,
        input,
        data,
        Source::Inline,
        s,
        &mut Waste::default(),
        None,
    )
}

/// The writer-dependent half of the peel of one file (design section 1), on the feeding thread
/// in input order: whole-file dedup, the probe of every stream, the records cap, the file gate
/// and the share rule, then the writes. With `publish`, what later copies can be found by is
/// recorded there (the pool's hints).
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit<O: Write>(
    w: &mut Writer<O>,
    cx: &mut Context<'_>,
    input: &Input,
    data: &[u8],
    source: Source,
    s: &mut DeflateSummary,
    waste: &mut Waste,
    mut publish: Option<&mut Published>,
) -> Result<(), CoreError> {
    let whole = |publish: &mut Option<&mut Published>| {
        if let Some(p) = publish.as_mut() {
            p.whole = Some((*blake3::hash(data).as_bytes(), data.len() as u64));
        }
    };
    // A whole duplicate is referenced and never walked.
    if w.add_file_if_known(&input.path, input.flags, input.mtime_ns, data)? {
        s.deduplicated.files += 1;
        s.deduplicated.bytes += data.len() as u64;
        // A file prepared for nothing; one a hint let the worker skip was never examined (it
        // counts under the pool's hinted files only, review N1).
        if matches!(&source, Source::Ready(r) if !matches!(&**r, Ok(p) if p.whole_hint)) {
            waste.files += 1;
        }
        whole(&mut publish);
        return Ok(());
    }
    let set = cx.settings();
    let records_cap = cx.stage.records_cap;
    let inline = |w: &mut Writer<O>, encoder: &mut PartEncoder, share: bool| {
        let set = Settings {
            share: set.share && share,
            ..set.clone()
        };
        prepare(
            data,
            &input.path,
            &set,
            encoder,
            &mut writer_oracle(w),
            &mut NoHooks,
        )
    };
    let mut prepared = match source {
        Source::Inline => inline(w, &mut *cx.encoder, true),
        Source::Ready(r) => match *r {
            // A file hinted whole that whole-file dedup did not find: prepare it here.
            Ok(p) if !force_rerun() && !p.whole_hint => p,
            Ok(_) => {
                waste.reruns += 1;
                inline(w, &mut *cx.encoder, true)
            }
            Err(f) => return f.raise(),
        },
    };
    let (decided, mut out, sample_bytes, encode_seconds, bound_hit, png_tried) =
        match decide(w, data, &mut prepared, s.record_bytes, records_cap)? {
            Decision::Ready {
                decided,
                out,
                sample_bytes,
                encode_seconds,
                bound_hit,
                png_tried,
            } => (
                decided,
                out,
                sample_bytes,
                encode_seconds,
                bound_hit,
                png_tried,
            ),
            Decision::Rerun => {
                waste.reruns += 1;
                // Without the share bound (the gate bound, a function of the bytes, stays): every
                // stream it would leave unencoded is judged as the sequential run judges it.
                prepared = inline(w, &mut *cx.encoder, false);
                match decide(w, data, &mut prepared, s.record_bytes, records_cap)? {
                    Decision::Ready {
                        decided,
                        out,
                        sample_bytes,
                        encode_seconds,
                        bound_hit,
                        png_tried,
                    } => (
                        decided,
                        out,
                        sample_bytes,
                        encode_seconds,
                        bound_hit,
                        png_tried,
                    ),
                    // Not reachable: the writer's own probe skipped only stored streams.
                    Decision::Rerun => {
                        return Err(CoreError::InvalidOption(
                            "the Deflate peel's probe changed its answer".into(),
                        ))
                    }
                }
            }
        };
    // Encodes the sequential run would not have made: streams found stored.
    for b in prepared.unused_encodes() {
        waste.encodes += 1;
        waste.sample_bytes += b;
    }
    // Only the kind and the counts are needed past here: the unused encodings go before the
    // writes.
    let Prepared {
        kind,
        counts,
        bound,
        ..
    } = prepared;
    s.sample_bytes += sample_bytes;
    s.encode_seconds += encode_seconds;
    s.png_tried += png_tried;
    let gain: u64 = decided.iter().map(|d| d.gain).sum();
    *s.walked_by_kind.entry(kind).or_default() += 1;
    let kept = decided.iter().any(|d| matches!(d.kind, Kind::Kept { .. }));
    let verdict = file_verdict(gain, data.len() as u64, kept);
    let mut kept_by_class = BTreeMap::new();
    if bound_hit {
        // The file's bound sent it whole without the encodes (design section 3): the gate
        // (gain at most the file gate) or the share rule (no stream stored, see `decide`).
        match bound {
            Some(Bound::Share) => out.file_share += decided.len() as u64,
            _ => out.file_gate += decided.len() as u64,
        }
        s.file_bound += 1;
        w.add_file(&input.path, input.flags, input.mtime_ns, &mut &data[..])?;
        whole(&mut publish);
    } else if decided.is_empty() || verdict != FileVerdict::Peel {
        if counts.reconstructed == 0 {
            s.no_streams += 1;
        } else if decided.is_empty() {
            s.none_kept += 1;
        } else if verdict == FileVerdict::Share {
            out.file_share += decided.len() as u64;
            s.file_share += 1;
        } else {
            out.file_gate += decided.len() as u64;
            s.file_gate += 1;
        }
        w.add_file(&input.path, input.flags, input.mtime_ns, &mut &data[..])?;
        whole(&mut publish);
    } else {
        if let Some(p) = publish.as_mut() {
            p.streams = decided
                .iter()
                .map(|d| match &d.kind {
                    Kind::Kept { record, .. } => (
                        record.original_hash,
                        d.spans.iter().map(|&(a, b)| (b - a) as u64).collect(),
                    ),
                    Kind::Stored => stream_key(data, &d.spans),
                })
                .collect();
        }
        write_peeled(w, input, data, decided, &mut out, &mut kept_by_class, s)?;
        s.peeled.files += 1;
        s.peeled.bytes += data.len() as u64;
    }
    s.streams.add(&counts);
    s.outcomes.add(&out);
    if let Some(files) = s.files.as_mut() {
        files.push(FileStreams {
            path: input.path.clone(),
            kind,
            counts,
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
                for &(a, b) in &d.spans {
                    if w.add_part_known(a as u64, &data[a..b])?.is_none() {
                        // Not reachable (stored chunks stay stored); written plainly if it were.
                        w.add_part(a as u64, &mut &data[a..b])?;
                    }
                }
            }
            Kind::Kept {
                png,
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
                // Revision 1.4: the deflate record, then the png-filter record; the block
                // `[M, png-filter, deflate-reconstruct]` (spec section 9).
                if let Some((rec, side)) = png.map(|b| *b) {
                    s.png_filtered += 1;
                    s.png_record_bytes += side - PNG_STEP_BYTES;
                    s.record_bytes += side - PNG_STEP_BYTES;
                    let pid = w.add_record(Record::new(RecordBody::PngFilter(rec)))?;
                    let mut pparams = Vec::new();
                    lpk_format::varint::write(&mut pparams, pid)?;
                    graph.steps.push(Step {
                        primitive: PrimitiveId::PngFilter,
                        params: pparams,
                    });
                }
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
                // The exact skips would leave the encodes the gate skips undone too.
                exact_skips: false,
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
    fn the_file_verdict_applies_the_gate_then_the_share() {
        for kept in [false, true] {
            assert_eq!(file_verdict(0, 0, kept), FileVerdict::Gate);
            assert_eq!(file_verdict(FILE_GATE, 1, kept), FileVerdict::Gate);
            assert_eq!(file_verdict(FILE_GATE, 1 << 30, kept), FileVerdict::Gate);
        }
        let g = FILE_GATE + 1;
        assert_eq!(file_verdict(g, g, true), FileVerdict::Peel);
        assert_eq!(file_verdict(g, g * FILE_SHARE, true), FileVerdict::Peel);
        assert_eq!(
            file_verdict(g, g * FILE_SHARE + 1, true),
            FileVerdict::Share
        );
        assert_eq!(file_verdict(u64::MAX, u64::MAX, true), FileVerdict::Peel);
        // Only already-stored streams: no block is closed, the share rule does not apply.
        assert_eq!(file_verdict(g, u64::MAX, false), FileVerdict::Peel);
    }

    /// A prepared file keeps no encoding of a stream the stream gate refuses (review B1): its
    /// streams of incompressible text are encoded and dropped, so what the file holds until its
    /// commit (the kept encodings and records) stays far within h = 2 × its length.
    #[test]
    fn a_prepared_file_keeps_no_encoding_of_a_no_gain_stream() {
        use super::super::testkit::{gzip, noise, text};
        let mut data = gzip(&noise(300_000, 5));
        data.extend_from_slice(&gzip(&noise(200_000, 6)));
        data.extend_from_slice(&gzip(&text(100_000, 7)));
        let o = crate::BalancedOptions {
            dict_size: 1 << 20,
            sample_len: 1 << 16,
            ..crate::BalancedOptions::default()
        };
        let mut encoder = PartEncoder::balanced(&o, 64 << 20).unwrap();
        let set = Settings {
            limits: Limits::standard(),
            block_size: 4 << 20,
            pregate: true,
            skips: false,
            share: true,
            png_filter: false,
        };
        let p = prepare(
            &data,
            "b1.gz",
            &set,
            &mut encoder,
            &mut |_, _| false,
            &mut NoHooks,
        );
        assert_eq!(p.streams.len(), 3, "{p:?}");
        let no_gain = p
            .streams
            .iter()
            .filter(
                |s| matches!(s.verdict, Verdict::NoGain { sample_bytes, .. } if sample_bytes > 0),
            )
            .count();
        assert_eq!(no_gain, 2, "{p:?}");
        let held: u64 = p
            .streams
            .iter()
            .map(|s| match &s.verdict {
                Verdict::Encoded(c) => {
                    c.encoded.bytes.len() as u64 + c.record.corrections.len() as u64
                }
                _ => 0,
            })
            .sum();
        assert!(held > 0 && held < 20_000, "{held}");
        assert!(held <= 2 * data.len() as u64);
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
