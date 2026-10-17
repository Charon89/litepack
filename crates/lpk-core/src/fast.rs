//! The Fast tier: one zstd frame per block, a long match window, optional caller-supplied
//! dictionaries, and blocks that never span two clusters.
//!
//! Files are grouped by [`cluster`] (class and dictionary kind) and fed to the writer cluster by
//! cluster with `Writer::close_block` between them; before each cluster the pipeline tells the
//! encoder, through a [`FastHandle`], the class and dictionary kind of what follows. The encoder
//! stores a block without trying zstd when its class is already compressed, or when the entropy
//! gate calls the block incompressible, and falls back to `store` when zstd does not shrink it.
//!
//! No dictionary is bundled; by default (`DictionaryPolicy::None`) none is used. Extracting an
//! archive written with `DictionaryPolicy::Provided` needs the same dictionaries: through
//! `lpk_format::Archive` pass the [`ProvidedDictionaries`] to `set_priors`; with the format tool,
//! give one `--prior <dictionary file>` per dictionary.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lpk_format::{
    BlockEncoder, EncodeJob, Encoded, FormatError, Graph, JobResult, JobState, PrimitiveId, Step,
    StoreEncoder, WriterSummary, DEFAULT_BLOCK_SIZE,
};
use std::io::Write;
use zstd::zstd_safe::CParameter;

use crate::classify::Class;
use crate::cluster::DictionaryKind;
use crate::error::CoreError;
use crate::gate::Gate;
use crate::ingest::IngestOptions;
use crate::pipeline::Pipeline;
use crate::priors::ProvidedDictionaries;

/// Smallest and largest `window_log` a `zstd` step may declare (the format's range; libzstd's
/// own bounds on this target are checked when the encoder is built).
const WINDOW_LOG_RANGE: std::ops::RangeInclusive<u32> = 10..=31;

/// Which dictionaries blocks may use.
#[derive(Debug, Clone, Default)]
pub enum DictionaryPolicy {
    /// Never use a dictionary.
    #[default]
    None,
    /// Use the dictionary the caller names for the cluster's kind (a kind without one gets
    /// none). The archive then needs the same dictionaries to be read.
    Provided(Arc<ProvidedDictionaries>),
}

/// Settings of [`archive_fast`].
#[derive(Debug, Clone)]
pub struct FastOptions {
    /// How the tree is walked.
    pub ingest: IngestOptions,
    /// zstd compression level. The default, 3, is the setting the Phase 0 G4 proxy measured.
    pub level: i32,
    /// log2 of the match window in bytes, 10 to 28 (the reader's default `max_window` caps it): how far back the encoder may match. The
    /// default, 27, is the window of the catalogue's zstd row (`--long=27`). It is the maximum
    /// of the window each block declares (a block shorter than the window declares a smaller
    /// one), not a memory figure the decoder reserves up front.
    pub window_log: u32,
    /// Plain bytes per block (the format's default).
    pub block_size: u64,
    /// Caller-supplied dictionaries (default: none).
    pub dictionaries: DictionaryPolicy,
    /// The entropy gate that sends incompressible blocks to `store`.
    pub gate: Gate,
    /// Longest `Jpeg`-classed input the JPEG peel reads whole; a longer one takes the streaming
    /// path and is counted as a `SizeCap` fallback before any byte is read. Default
    /// [`crate::peel::LEPTON_MAX_FILE`], `lepton_jpeg`'s own file-size cap.
    pub jpeg_max_file: u64,
    /// Blocks encoded at once (`WriterOptions::encode_threads`); `None` (the default) is the
    /// machine's logical cores, 1 the sequential path. The archive is the same whatever the
    /// count. Memory: that many blocks in flight, each its plain bytes plus its encoding, plus
    /// one zstd compressor per block in flight.
    pub threads: Option<usize>,
}

impl Default for FastOptions {
    fn default() -> Self {
        FastOptions {
            ingest: IngestOptions::default(),
            level: 3,
            window_log: 27,
            block_size: DEFAULT_BLOCK_SIZE,
            dictionaries: DictionaryPolicy::None,
            gate: Gate::DEFAULT,
            jpeg_max_file: crate::peel::LEPTON_MAX_FILE,
            threads: None,
        }
    }
}

/// `threads` resolved: the machine's logical cores for `None`, at least 1.
pub(crate) fn resolve_threads(threads: Option<usize>) -> usize {
    threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1)
}

/// What the Fast encoder did with the blocks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FastSummary {
    /// Blocks coded with zstd.
    pub zstd_blocks: u64,
    /// Blocks stored because the gate called them incompressible.
    pub stored_by_gate: u64,
    /// Blocks stored without the gate because their class is already compressed.
    pub stored_by_class: u64,
    /// Blocks stored because the zstd frame was not smaller than the plain bytes.
    pub stored_no_gain: u64,
    /// What the peel stages did before the encoder saw the blocks (set by the pipeline).
    pub peel: crate::peel::PeelSummary,
}

#[derive(Debug)]
struct Shared {
    class: Class,
    dictionary: DictionaryKind,
    summary: FastSummary,
}

/// The pipeline's side of the encoder: sets what the next blocks hold and reads the counts.
#[derive(Debug, Clone)]
pub struct FastHandle(Arc<Mutex<Shared>>);

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FastHandle {
    /// The class and dictionary kind of the blocks encoded from now on.
    pub fn set_hint(&self, class: Class, dictionary: DictionaryKind) {
        let mut s = lock(&self.0);
        s.class = class;
        s.dictionary = dictionary;
    }

    /// The counts so far.
    pub fn summary(&self) -> FastSummary {
        lock(&self.0).summary
    }
}

/// The compression contexts an encoder made and used, counted for the tests that show where
/// a block's store decision is made (a stored-by-class block reaches none of the counts).
#[derive(Debug, Default)]
pub(crate) struct ContextCounts {
    /// zstd cores built (one compressor per dictionary each), the construction's included.
    pub(crate) zstd_cores: AtomicU64,
    /// zstd compressions run (a trial or a block).
    pub(crate) zstd_runs: AtomicU64,
    /// liblzma encoder streams created.
    pub(crate) lzma_streams: AtomicU64,
}

impl ContextCounts {
    pub(crate) fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

/// Classes stored without trying to compress them: the class rule (D-47). It is decided
/// before any compression context exists for the block: the jobs and the sequential paths of
/// both tiers test it first and neither build nor borrow a compressor for such a block.
pub(crate) fn stored_by_class(class: Class) -> bool {
    matches!(
        class,
        Class::HighEntropy | Class::Video | Class::Compressed | Class::Jpeg | Class::Png
    )
}

/// The Fast tier's block encoder (zstd, gate, dictionaries).
///
/// Parallel encoding (`WriterOptions::encode_threads`): each [`BlockEncoder::job`] captures
/// the steering of the moment (class, dictionary kind) and borrows a [`ZstdCore`] — the
/// encoder's own when it is free, a spare given back by an earlier job, or a new one built
/// on the pool thread — so every job compresses with the same parameters and the output is
/// the one `encode` makes; the writer gives the core and the block's outcome back through
/// [`BlockEncoder::absorb`], in block order, where the counts are added.
pub struct ZstdEncoder {
    config: ZstdConfig,
    shared: Arc<Mutex<Shared>>,
    /// The compressors, when no job holds them.
    core: Option<ZstdCore>,
    /// Cores given back by jobs, for the next jobs (at most one per block in flight).
    spare: Vec<ZstdCore>,
}

/// What builds a [`ZstdCore`]: the checked parameters.
#[derive(Debug, Clone)]
pub(crate) struct ZstdConfig {
    level: i32,
    window_log: u32,
    gate: Gate,
    policy: DictionaryPolicy,
    counts: Arc<ContextCounts>,
}

/// The compressors of the Fast tier, one plain and one per provided dictionary.
pub(crate) struct ZstdCore {
    level: i32,
    window_log: u32,
    gate: Gate,
    /// The compressor without a dictionary.
    plain: zstd::bulk::Compressor<'static>,
    /// One compressor per provided dictionary: kind, prior id, compressor.
    with_dict: Vec<(DictionaryKind, [u8; 32], zstd::bulk::Compressor<'static>)>,
    counts: Arc<ContextCounts>,
}

/// What a block became, for the summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Zstd,
    StoredByGate,
    StoredByClass,
    StoredNoGain,
}

impl Outcome {
    fn count(self, s: &mut FastSummary) {
        match self {
            Outcome::Zstd => s.zstd_blocks += 1,
            Outcome::StoredByGate => s.stored_by_gate += 1,
            Outcome::StoredByClass => s.stored_by_class += 1,
            Outcome::StoredNoGain => s.stored_no_gain += 1,
        }
    }
}

impl std::fmt::Debug for ZstdEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZstdEncoder")
            .field("level", &self.config.level)
            .field("window_log", &self.config.window_log)
            .field("spare", &self.spare.len())
            .finish_non_exhaustive()
    }
}

/// `ceil(log2(n))`, 0 for `n <= 1`.
fn ceil_log2(n: usize) -> u32 {
    if n <= 1 {
        0
    } else {
        usize::BITS - (n - 1).leading_zeros()
    }
}

/// A compressor with the Fast tier's fixed parameters; every parameter is checked against
/// libzstd's bounds here, so a bad option fails at construction and names the parameter.
fn build_compressor(
    level: i32,
    window_log: u32,
    dictionary: Option<&[u8]>,
) -> Result<zstd::bulk::Compressor<'static>, CoreError> {
    let bad = |name: &str, e: std::io::Error| CoreError::InvalidOption(format!("{name}: {e}"));
    if let Some(d) = dictionary {
        // Raw bytes without zstd's dictionary magic would load as raw content and the frame
        // would then not name a dictionary the reader accepts.
        if d.get(..4) != Some(&[0x37, 0xA4, 0x30, 0xEC]) {
            return Err(CoreError::InvalidOption(
                "dictionary: missing the zstd dictionary magic".into(),
            ));
        }
    }
    let mut c = match dictionary {
        Some(d) => zstd::bulk::Compressor::with_dictionary(level, d),
        None => zstd::bulk::Compressor::new(level),
    }
    .map_err(|e| bad("dictionary", e))?;
    c.set_parameter(CParameter::WindowLog(window_log))
        .map_err(|e| bad("window_log", e))?;
    c.set_parameter(CParameter::EnableLongDistanceMatching(true))
        .map_err(|e| bad("long_distance_matching", e))?;
    // The format hashes every frame and chunk; the content size is written.
    c.set_parameter(CParameter::ChecksumFlag(false))
        .map_err(|e| bad("checksum", e))?;
    c.set_parameter(CParameter::ContentSizeFlag(true))
        .map_err(|e| bad("content_size", e))?;
    if dictionary.is_some() {
        // A malformed dictionary with the magic fails here, before any output exists.
        c.compress(b"").map_err(|e| bad("dictionary", e))?;
    }
    Ok(c)
}

impl ZstdCore {
    pub(crate) fn build(c: &ZstdConfig) -> Result<Self, CoreError> {
        ContextCounts::bump(&c.counts.zstd_cores);
        let plain = build_compressor(c.level, c.window_log, None)?;
        let mut with_dict = Vec::new();
        if let DictionaryPolicy::Provided(set) = &c.policy {
            for (kind, id, bytes) in set.iter() {
                with_dict.push((
                    kind,
                    id,
                    build_compressor(c.level, c.window_log, Some(bytes))?,
                ));
            }
        }
        Ok(ZstdCore {
            level: c.level,
            window_log: c.window_log,
            gate: c.gate,
            plain,
            with_dict,
            counts: Arc::clone(&c.counts),
        })
    }

    /// A core of its own (no dictionary, no gate, its own counts) at `level` and `window_log`,
    /// for the Deflate peel's part encoder; the parameters are checked as for
    /// [`ZstdEncoder::new`].
    pub(crate) fn standalone(level: i32, window_log: u32) -> Result<Self, CoreError> {
        let (enc, _) = ZstdEncoder::new(level, window_log, &DictionaryPolicy::None, Gate::OFF)?;
        ZstdCore::build(enc.config())
    }

    /// zstd on `plain` with the dictionary-free compressor and nothing else: no class rule, no
    /// gate, no store when the frame does not shrink `plain` (the caller's gate decides).
    pub(crate) fn encode_raw(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let declared = self.window_log.min(ceil_log2(plain.len()).max(10));
        self.plain.set_parameter(CParameter::WindowLog(declared))?;
        ContextCounts::bump(&self.counts.zstd_runs);
        let bytes = self.plain.compress(plain)?;
        let mut params = vec![declared as u8];
        params.extend_from_slice(&[0u8; 32]);
        let graph = Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Zstd,
                params,
            }],
        };
        Ok(Encoded {
            resources: graph.resources(),
            memory: crate::memory::zstd_decode_memory(declared, plain.len() as u64),
            graph,
            bytes,
        })
    }

    /// Drop the dictionary-free compression context's tables: it is rebuilt empty with the
    /// same parameters (libzstd's output does not depend on a context's history), so the core
    /// holds no full-size tables while another encoder runs (E2-20). Not counted as a new core.
    pub(crate) fn release(&mut self) -> Result<(), CoreError> {
        self.plain = build_compressor(self.level, self.window_log, None)?;
        Ok(())
    }

    /// Encode one block under the steering `class` and `kind`. The decisions come in this
    /// order, the first that applies: the class rule (stored, no trial), the entropy gate on
    /// its sample (stored), zstd, and `store` when zstd does not shrink the block.
    pub(crate) fn encode(
        &mut self,
        class: Class,
        kind: DictionaryKind,
        plain: &[u8],
    ) -> Result<(Encoded, Outcome), FormatError> {
        if stored_by_class(class) {
            return Ok((StoreEncoder.encode(plain)?, Outcome::StoredByClass));
        }
        if self.gate.is_incompressible(plain) {
            return Ok((StoreEncoder.encode(plain)?, Outcome::StoredByGate));
        }
        // RFC 8878: a frame whose content size is known and fits its window is written
        // single-segment, and its Window_Size is then its content size. So a block shorter than
        // 2^w declares w (at least 10, the smallest the step allows) and the declaration holds;
        // a longer block declares the configured window, which libzstd honours as a maximum.
        let declared = self.window_log.min(ceil_log2(plain.len()).max(10));
        let (id, compressor) = match self.with_dict.iter_mut().find(|(k, _, _)| *k == kind) {
            Some((_, id, c)) => (*id, c),
            None => ([0u8; 32], &mut self.plain),
        };
        compressor.set_parameter(CParameter::WindowLog(declared))?;
        ContextCounts::bump(&self.counts.zstd_runs);
        let bytes = compressor.compress(plain)?;
        if bytes.len() >= plain.len() {
            return Ok((StoreEncoder.encode(plain)?, Outcome::StoredNoGain));
        }
        let mut params = vec![declared as u8];
        params.extend_from_slice(&id);
        let graph = Graph {
            steps: vec![Step {
                primitive: PrimitiveId::Zstd,
                params,
            }],
        };
        Ok((
            Encoded {
                resources: graph.resources(),
                memory: crate::memory::zstd_decode_memory(declared, plain.len() as u64),
                graph,
                bytes,
            },
            Outcome::Zstd,
        ))
    }
}

/// One block's Fast encoding on a pool thread.
struct ZstdJob {
    class: Class,
    kind: DictionaryKind,
    config: ZstdConfig,
    core: Option<ZstdCore>,
}

/// What a [`ZstdJob`] gives back: the core it used (none for a stored-by-class block, which
/// never takes one) and the block's outcome.
struct ZstdReturn {
    core: Option<ZstdCore>,
    outcome: Outcome,
}

/// A build error on the pool thread (the options were accepted at construction, so this is a
/// resource failure).
fn build_failed(e: CoreError) -> FormatError {
    FormatError::Io(std::io::Error::other(e.to_string()))
}

impl EncodeJob for ZstdJob {
    fn run(self: Box<Self>, plain: &[u8]) -> Result<(Encoded, Option<JobState>), FormatError> {
        if stored_by_class(self.class) {
            // The class rule, before any compressor exists for this block.
            let back = ZstdReturn {
                core: self.core,
                outcome: Outcome::StoredByClass,
            };
            return Ok((StoreEncoder.encode(plain)?, Some(Box::new(back))));
        }
        let mut core = match self.core {
            Some(c) => c,
            None => ZstdCore::build(&self.config).map_err(build_failed)?,
        };
        let (encoded, outcome) = core.encode(self.class, self.kind, plain)?;
        let back = ZstdReturn {
            core: Some(core),
            outcome,
        };
        Ok((encoded, Some(Box::new(back))))
    }

    fn run_owned(self: Box<Self>, plain: Vec<u8>) -> (JobResult, Option<Vec<u8>>) {
        // The class rule, then the gate on its sample: a block either one
        // stores takes the plain buffer as its encoding, uncopied.
        let stored = stored_outcome(self.class, &self.config.gate, &plain);
        if let Some(outcome) = stored {
            let back = ZstdReturn {
                core: self.core,
                outcome,
            };
            return (Ok((StoreEncoder::wrap(plain), Some(Box::new(back)))), None);
        }
        let r = self.run(&plain);
        (r, Some(plain))
    }
}

/// The store decision taken before any compression: the class rule (D-47), then the entropy
/// gate on the block's sample. `None` means the block goes to zstd.
fn stored_outcome(class: Class, gate: &Gate, plain: &[u8]) -> Option<Outcome> {
    if stored_by_class(class) {
        Some(Outcome::StoredByClass)
    } else if gate.is_incompressible(plain) {
        Some(Outcome::StoredByGate)
    } else {
        None
    }
}

impl ZstdEncoder {
    /// A new encoder and the handle that steers it. Errors (`InvalidOption`, naming the
    /// parameter) when `level`, `window_log` or a dictionary is not accepted.
    pub fn new(
        level: i32,
        window_log: u32,
        policy: &DictionaryPolicy,
        gate: Gate,
    ) -> Result<(Self, FastHandle), CoreError> {
        if !WINDOW_LOG_RANGE.contains(&window_log) {
            return Err(CoreError::InvalidOption(format!(
                "window_log {window_log} is outside 10..=31"
            )));
        }
        if 1u64 << window_log > lpk_format::DEFAULT_MAX_WINDOW {
            return Err(CoreError::InvalidOption(format!(
                "window_log {window_log} is above the reader's default max_window (256 MiB, log2 28)"
            )));
        }
        let range = zstd::compression_level_range();
        if !range.contains(&level) {
            return Err(CoreError::InvalidOption(format!(
                "level {level} is outside {}..={}",
                range.start(),
                range.end()
            )));
        }
        let config = ZstdConfig {
            level,
            window_log,
            gate,
            policy: policy.clone(),
            counts: Arc::new(ContextCounts::default()),
        };
        let core = ZstdCore::build(&config)?;
        let shared = Arc::new(Mutex::new(Shared {
            class: Class::Other,
            dictionary: DictionaryKind::None,
            summary: FastSummary::default(),
        }));
        let enc = ZstdEncoder {
            config,
            shared: Arc::clone(&shared),
            core: Some(core),
            spare: Vec::new(),
        };
        Ok((enc, FastHandle(shared)))
    }

    fn bump(&self, outcome: Outcome) {
        outcome.count(&mut lock(&self.shared).summary);
    }

    /// The steering of the moment.
    fn hint(&self) -> (Class, DictionaryKind) {
        let s = lock(&self.shared);
        (s.class, s.dictionary)
    }

    /// A core for a job: the encoder's own, a spare, or none (the job builds one).
    pub(crate) fn lend_core(&mut self) -> Option<ZstdCore> {
        self.core.take().or_else(|| self.spare.pop())
    }

    /// Take a core back from a job.
    pub(crate) fn return_core(&mut self, core: ZstdCore) {
        if self.core.is_none() {
            self.core = Some(core);
        } else {
            self.spare.push(core);
        }
    }

    /// The config a job builds a core from.
    pub(crate) fn config(&self) -> &ZstdConfig {
        &self.config
    }

    /// The context counts of this encoder and of every job it made.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> Arc<ContextCounts> {
        Arc::clone(&self.config.counts)
    }

    /// Encode on this thread with the encoder's own core (built anew if a job holds it); a
    /// stored-by-class block takes no core.
    pub(crate) fn encode_with(
        &mut self,
        class: Class,
        kind: DictionaryKind,
        plain: &[u8],
    ) -> Result<(Encoded, Outcome), FormatError> {
        if stored_by_class(class) {
            return Ok((StoreEncoder.encode(plain)?, Outcome::StoredByClass));
        }
        let mut core = match self.lend_core() {
            Some(c) => c,
            None => ZstdCore::build(&self.config).map_err(build_failed)?,
        };
        let r = core.encode(class, kind, plain);
        self.return_core(core);
        r
    }
}

impl BlockEncoder for ZstdEncoder {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let (class, kind) = self.hint();
        let (encoded, outcome) = self.encode_with(class, kind, plain)?;
        self.bump(outcome);
        Ok(encoded)
    }

    fn encode_owned(&mut self, plain: Vec<u8>) -> (Result<Encoded, FormatError>, Option<Vec<u8>>) {
        let (class, _) = self.hint();
        if let Some(outcome) = stored_outcome(class, &self.config.gate, &plain) {
            self.bump(outcome);
            return (Ok(StoreEncoder::wrap(plain)), None);
        }
        let r = self.encode(&plain);
        (r, Some(plain))
    }

    fn job(&mut self) -> Option<Box<dyn EncodeJob>> {
        let (class, kind) = self.hint();
        // A stored-by-class block borrows no core: the job stores it without one.
        let core = if stored_by_class(class) {
            None
        } else {
            self.lend_core()
        };
        Some(Box::new(ZstdJob {
            class,
            kind,
            config: self.config.clone(),
            core,
        }))
    }

    fn absorb(&mut self, state: JobState) {
        if let Ok(r) = state.downcast::<ZstdReturn>() {
            self.bump(r.outcome);
            if let Some(core) = r.core {
                self.return_core(core);
            }
        }
    }
}

/// Walk `root` and write it with the Fast tier to `out` (a front over
/// [`Pipeline::run`]).
pub fn archive_fast(
    root: &Path,
    out: impl Write,
    options: FastOptions,
) -> Result<(WriterSummary, FastSummary), CoreError> {
    let s = Pipeline::fast(options).run(root, out)?;
    Ok((s.writer, s.fast.unwrap_or_default()))
}

/// Like [`archive_fast`], writing to a new file at `archive_path` with the guarantees of
/// `archive_store_file`: create-new, the output left out of its own archive, data synced before
/// the trailer, a failed write removes the file. Options (including provided dictionaries) are
/// validated before the output file exists.
pub fn archive_fast_file(
    root: &Path,
    archive_path: &Path,
    options: FastOptions,
) -> Result<(WriterSummary, FastSummary), CoreError> {
    let s = Pipeline::fast(options).run_file(root, archive_path)?;
    Ok((s.writer, s.fast.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::{clear_readonly, make_tree};
    use lpk_format::{Archive, EntryKind, Resources, Writer, WriterOptions};
    use std::io::Cursor;

    /// Compressible English-like text of exactly `len` bytes, varied by `seed`.
    fn prose_seeded(seed: u64, len: usize) -> Vec<u8> {
        const WORDS: [&str; 16] = [
            "the", "king", "of", "France", "and", "his", "army", "marched", "over", "a", "long",
            "road", "toward", "the", "sea", "while",
        ];
        let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
        let mut out = Vec::with_capacity(len + 16);
        while out.len() < len {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            out.extend_from_slice(WORDS[(s % 16) as usize].as_bytes());
            out.push(if s & 0x700 == 0 { b'\n' } else { b' ' });
        }
        out.truncate(len);
        out
    }

    fn prose(len: usize) -> Vec<u8> {
        prose_seeded(0, len)
    }

    fn random(len: usize) -> Vec<u8> {
        let mut s = 0x1234_5678_9ABC_DEF1u64;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    /// A prose dictionary trained at test time on synthetic samples.
    fn trained() -> Vec<u8> {
        let samples: Vec<Vec<u8>> = (1..400).map(|i| prose_seeded(i, 900)).collect();
        zstd::dict::from_samples(&samples, 8 * 1024).unwrap()
    }

    fn provided() -> (DictionaryPolicy, Arc<ProvidedDictionaries>) {
        let set = Arc::new(ProvidedDictionaries::new().with(DictionaryKind::Prose, trained()));
        (DictionaryPolicy::Provided(Arc::clone(&set)), set)
    }

    fn encoder(policy: &DictionaryPolicy) -> (ZstdEncoder, FastHandle) {
        ZstdEncoder::new(3, 27, policy, Gate::DEFAULT).unwrap()
    }

    type Extracted = (Archive<Cursor<Vec<u8>>>, Vec<(String, Vec<u8>)>);

    /// Read every file of an archive back; `priors` are the dictionaries it needs, if any.
    fn extract_with(
        bytes: Vec<u8>,
        resources: &Resources,
        priors: Option<Arc<ProvidedDictionaries>>,
    ) -> Extracted {
        let mut a = Archive::open(Cursor::new(bytes), resources).unwrap();
        if let Some(p) = priors {
            struct Shared(Arc<ProvidedDictionaries>);
            impl lpk_format::PriorStore for Shared {
                fn get(&self, id: &[u8; 32]) -> Option<&[u8]> {
                    self.0.get(id)
                }
            }
            a.set_priors(Box::new(Shared(p)));
        }
        let table = a.entry_table().unwrap();
        let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
        let mut files = Vec::new();
        for e in &entries {
            if e.kind == EntryKind::File {
                let mut out = Vec::new();
                a.extract(e, &mut out).unwrap();
                files.push((e.path.clone(), out));
            }
        }
        (a, files)
    }

    fn extract_all(bytes: Vec<u8>) -> Extracted {
        extract_with(bytes, &Resources::default(), None)
    }

    /// Write one file with a hand-steered encoder; returns the archive.
    fn one_file(enc: ZstdEncoder, data: &[u8]) -> Vec<u8> {
        let wopts = WriterOptions {
            encoder: Box::new(enc),
            ..WriterOptions::default()
        };
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, wopts).unwrap();
        w.add_file("p.txt", Default::default(), 0, &mut &data[..])
            .unwrap();
        w.finish().unwrap();
        out
    }

    #[test]
    fn a_block_with_a_provided_dictionary_lists_its_id_once_and_decodes() {
        let data = prose(1 << 20);
        let (policy, set) = provided();
        let (enc, handle) = encoder(&policy);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let out = one_file(enc, &data);
        let s = handle.summary();
        assert_eq!(
            (s.zstd_blocks, s.stored_by_gate, s.stored_by_class),
            (1, 0, 0)
        );
        assert!(out.len() < data.len() / 2);
        let (a, files) = extract_with(out, &Resources::default(), Some(Arc::clone(&set)));
        let want = set.dictionary(DictionaryKind::Prose).unwrap().0;
        assert_eq!(a.priors(), &[want]);
        assert_eq!(files[0].1, data);
        // A kind without a provided dictionary gets none.
        let (mut enc, handle) = encoder(&policy);
        handle.set_hint(Class::Text, DictionaryKind::Source);
        let e = enc.encode(&data).unwrap();
        assert!(e.graph.prior_ids().is_empty());
    }

    #[test]
    fn without_dictionaries_the_step_id_is_zero_and_the_frame_names_none() {
        let data = prose(1 << 20);
        let (mut enc, handle) = encoder(&DictionaryPolicy::None);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(&data).unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Zstd);
        assert_eq!(e.graph.steps[0].params[0], 20);
        assert_eq!(&e.graph.steps[0].params[1..], &[0u8; 32]);
        assert!(e.graph.prior_ids().is_empty());
        // Frame_Header_Descriptor bits 0..1 are Dictionary_ID_flag.
        assert_eq!(e.bytes[4] & 0b11, 0);
        assert_eq!(e.resources.window, 1 << 20);
        // With a dictionary the frame names it and the step carries its id.
        let (policy, set) = provided();
        let (mut enc, handle) = encoder(&policy);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(&data).unwrap();
        assert_ne!(e.bytes[4] & 0b11, 0);
        assert_eq!(e.graph.prior_ids().len(), 1);
        assert_eq!(
            e.graph.prior_ids()[0],
            lpk_format::prior_id(set.dictionary(DictionaryKind::Prose).unwrap().1)
        );
    }

    #[test]
    fn random_blocks_are_stored_by_the_gate_and_video_by_class() {
        let data = random(1 << 20);
        let (mut enc, handle) = encoder(&DictionaryPolicy::None);
        handle.set_hint(Class::Other, DictionaryKind::None);
        let e = enc.encode(&data).unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Store);
        assert_eq!(e.bytes, data);
        assert_eq!(handle.summary().stored_by_gate, 1);
        // Compressible bytes under a Video hint skip the gate and are stored.
        handle.set_hint(Class::Video, DictionaryKind::None);
        let e = enc.encode(&prose(1 << 16)).unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Store);
        let s = handle.summary();
        assert_eq!(
            (s.stored_by_gate, s.stored_by_class, s.zstd_blocks),
            (1, 1, 0)
        );
    }

    /// The class rule is decided before any compression context exists: a stored-by-class
    /// cluster builds no zstd core (beyond the construction's) and runs no compression, at one
    /// block in flight and at four (no job borrows the encoder's core for it either); a
    /// compressible cluster written the same way does.
    #[test]
    fn a_stored_by_class_cluster_creates_no_compression_context() {
        let data = prose(1 << 20);
        for threads in [1usize, 4] {
            for (class, compressed) in [(Class::Video, false), (Class::Text, true)] {
                let (enc, handle) = encoder(&DictionaryPolicy::None);
                let counts = enc.counts();
                let cores = ContextCounts::get(&counts.zstd_cores);
                assert_eq!(cores, 1, "the construction's core");
                handle.set_hint(class, DictionaryKind::None);
                let wopts = WriterOptions {
                    encoder: Box::new(enc),
                    chunk_size: 1 << 16,
                    block_size: 1 << 18,
                    encode_threads: threads,
                    ..WriterOptions::default()
                };
                let mut out = Vec::new();
                let mut w = Writer::new(&mut out, wopts).unwrap();
                w.add_file("p.txt", Default::default(), 0, &mut &data[..])
                    .unwrap();
                let blocks = w.finish().unwrap().blocks;
                let made = (
                    ContextCounts::get(&counts.zstd_cores) - cores,
                    ContextCounts::get(&counts.zstd_runs),
                );
                let s = handle.summary();
                if compressed {
                    assert!(made.1 > 0, "{threads} {made:?}");
                    assert_eq!(s.stored_by_class, 0);
                } else {
                    assert_eq!(made, (0, 0), "threads {threads}");
                    assert_eq!((s.stored_by_class, s.zstd_blocks), (blocks, 0));
                    assert_eq!(blocks, 4);
                }
                assert_eq!(extract_all(out).1[0].1, data);
            }
        }
    }

    #[test]
    fn a_tiny_block_that_zstd_cannot_shrink_is_stored_and_counted() {
        let (mut enc, handle) = encoder(&DictionaryPolicy::None);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(b"hi there").unwrap();
        assert_eq!(e.graph.steps[0].primitive, PrimitiveId::Store);
        assert_eq!(e.bytes, b"hi there");
        let s = handle.summary();
        assert_eq!(
            (s.stored_no_gain, s.zstd_blocks, s.stored_by_gate),
            (1, 0, 0)
        );
    }

    #[test]
    fn window_and_level_reach_the_frame() {
        let data = prose(1 << 20);
        let (mut enc, handle) =
            ZstdEncoder::new(3, 12, &DictionaryPolicy::None, Gate::DEFAULT).unwrap();
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let e = enc.encode(&data).unwrap();
        let fhd = e.bytes[4];
        assert_eq!(fhd & 0x20, 0, "not single-segment");
        // Window_Descriptor: exponent in the high five bits, base 2^10.
        assert_eq!(10 + u32::from(e.bytes[5] >> 3), 12);
        assert_eq!(e.resources.window, 1 << 12);
        assert_eq!(e.graph.steps[0].params[0], 12);
        for level in [1, 3, 19] {
            let (mut enc, handle) =
                ZstdEncoder::new(level, 20, &DictionaryPolicy::None, Gate::DEFAULT).unwrap();
            handle.set_hint(Class::Text, DictionaryKind::Prose);
            assert!(enc.encode(&data).is_ok(), "level {level}");
        }
        let none = DictionaryPolicy::None;
        for (level, wl, name) in [
            (3, 9, "window_log"),
            (3, 32, "window_log"),
            (99, 20, "level"),
        ] {
            match ZstdEncoder::new(level, wl, &none, Gate::DEFAULT) {
                Err(CoreError::InvalidOption(m)) => assert!(m.contains(name), "{m}"),
                other => panic!("{other:?}"),
            }
        }
        // A dictionary that is not a dictionary at all still loads as raw content in libzstd,
        // so only the parameters are rejected here.
    }

    #[test]
    fn a_provided_dictionary_is_validated_before_any_output_exists() {
        let raw = DictionaryPolicy::Provided(Arc::new(
            ProvidedDictionaries::new().with(DictionaryKind::Prose, b"just some bytes".to_vec()),
        ));
        match ZstdEncoder::new(3, 20, &raw, Gate::DEFAULT) {
            Err(CoreError::InvalidOption(m)) => assert!(m.contains("dictionary"), "{m}"),
            other => panic!("{other:?}"),
        }
        // Magic present but nothing after it.
        let hollow = DictionaryPolicy::Provided(Arc::new(ProvidedDictionaries::new().with(
            DictionaryKind::Prose,
            vec![0x37, 0xA4, 0x30, 0xEC, 1, 2, 3, 4],
        )));
        assert!(ZstdEncoder::new(3, 20, &hollow, Gate::DEFAULT).is_err());
        let (good, _) = provided();
        assert!(ZstdEncoder::new(3, 20, &good, Gate::DEFAULT).is_ok());
        // archive_fast_file refuses before it creates the file.
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("out.lpk");
        let options = FastOptions {
            dictionaries: raw,
            ..FastOptions::default()
        };
        assert!(archive_fast_file(dir.path(), &arch, options).is_err());
        assert!(!arch.exists());
    }

    #[test]
    fn a_small_block_declares_a_small_window_and_decodes_under_a_small_limit() {
        let data = prose(3000);
        let (enc, handle) = encoder(&DictionaryPolicy::None);
        handle.set_hint(Class::Text, DictionaryKind::Prose);
        let out = one_file(enc, &data);
        let mut probe = encoder(&DictionaryPolicy::None);
        probe.1.set_hint(Class::Text, DictionaryKind::Prose);
        let e = probe.0.encode(&data).unwrap();
        // ceil(log2(3000)) = 12; the content fits the window, so the frame is single-segment.
        assert_eq!(e.graph.steps[0].params[0], 12);
        assert_eq!(e.resources.window, 1 << 12);
        assert_ne!(e.bytes[4] & 0x20, 0, "single-segment");
        let small = Resources {
            max_window: 1 << 13,
            ..Resources::default()
        };
        let (_, files) = extract_with(out, &small, None);
        assert_eq!(files[0].1, data);
        // Tiny blocks never declare less than the step's minimum of 10.
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1025), 11);
    }

    /// A tree with two tiny clusters (prose and source), a directory and a random file.
    fn small_tree(p: &Path) {
        std::fs::create_dir(p.join("d")).unwrap();
        std::fs::write(p.join("a.txt"), prose(5000)).unwrap();
        std::fs::write(p.join("b.c"), "int main(void) { return 0; }\n".repeat(100)).unwrap();
        std::fs::write(p.join("d").join("r.bin"), random(100_000)).unwrap();
    }

    #[test]
    fn each_cluster_gets_its_own_block_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        small_tree(dir.path());
        let mut out = Vec::new();
        let (ws, fs) = archive_fast(dir.path(), &mut out, FastOptions::default()).unwrap();
        assert_eq!(ws.entries, 4);
        // Prose, source, and the random file's own block: three blocks, never merged.
        let (a, files) = extract_all(out);
        assert_eq!(a.index().blocks.len(), 3);
        assert_eq!(fs.zstd_blocks, 2);
        assert_eq!(fs.stored_by_gate + fs.stored_by_class, 1);
        assert_eq!(files.len(), 3);
        for (path, bytes) in files {
            assert_eq!(
                bytes,
                std::fs::read(dir.path().join(&path)).unwrap(),
                "{path}"
            );
        }
        // The default policy uses no dictionary.
        assert!(a.priors().is_empty());
    }

    #[test]
    fn archive_fast_file_refuses_to_overwrite_and_excludes_its_output() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let arch = dir.path().join("out.lpk");
        let (ws, _) = archive_fast_file(dir.path(), &arch, FastOptions::default()).unwrap();
        assert_eq!(ws.entries, 17);
        let (_, files) = extract_all(std::fs::read(&arch).unwrap());
        assert!(!files.iter().any(|(p, _)| p == "out.lpk"));
        assert_eq!(files.len(), 12);
        let again = archive_fast_file(dir.path(), &arch, FastOptions::default());
        assert!(matches!(again, Err(CoreError::Io { .. })));
        assert!(arch.exists(), "the first archive survives a refused rerun");
        clear_readonly(&dir.path().join("ro.txt"));
    }

    #[test]
    fn a_failed_fast_file_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("out.lpk");
        let r = archive_fast_file(
            &dir.path().join("missing-root"),
            &arch,
            FastOptions::default(),
        );
        assert!(r.is_err());
        assert!(!arch.exists());
    }
}
