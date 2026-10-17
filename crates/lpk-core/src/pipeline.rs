//! The pipeline: the fixed stage order `classify -> peel -> fold -> model -> seal` as a struct,
//! and the one code path that turns a directory tree into an archive.
//!
//! Peel (turn one input stream into a peeled stream plus a reconstruction record; the JPEG peel
//! is the first, [`crate::peel`]) runs in the Fast model on the inputs of the classes a stage
//! applies to; Fold (deduplicate and delta across streams; it runs on the peeled streams, never
//! on the raw inputs) installs its chunker and dedup in the writer ([`crate::fold`]; the Fast and
//! Balanced pipelines run [`Dedup`] by default).
//! [`archive_store`](crate::archive_store) and [`archive_fast`](crate::archive_fast) are thin
//! fronts over [`Pipeline::run`].
//!
//! Blocks are encoded one after the other; parallel encoding needs the writer to hold blocks in
//! flight and comes with a later task.

use std::collections::HashSet;
use std::io::{BufWriter, Read, Seek, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use lpk_format::prefetch::{NextFileError, OpenFn, Prefetch, PrefetchOptions};
use lpk_format::{
    Archive, Chunker, Credentials, Encoded, EntryKind, FormatError, Graph, GraphResources, Step,
    Writer, WriterOptions, WriterSummary,
};

use crate::balanced::{BalancedEncoder, BalancedHandle, BalancedOptions, BalancedSummary};
use crate::classify::Class;
use crate::cluster::{cluster, DictionaryKind};
use crate::error::CoreError;
use crate::fast::{resolve_threads, stored_by_class};
use crate::fast::{FastHandle, FastOptions, FastSummary, ZstdEncoder};
use crate::fold::ordering::order_cluster;
use crate::fold::{Dedup, FoldStage};
use crate::ingest::{file_identity, validate_input, walk, IngestOptions, Input};
use crate::peel::{JpegPeel, PeelPlan, PeelStage, PeelSummary};
use crate::source::Source;
use crate::store::{create_new_and_run, StoreOptions, SyncFn, OUT_BUF};

/// The classify stage: how the tree is walked; the classifier and the clustering run on what the
/// walk found (when the model stage wants them).
#[derive(Debug, Clone, Default)]
pub struct ClassifyStage {
    /// How the tree is walked.
    pub ingest: IngestOptions,
}

/// The model stage: how blocks are coded.
#[derive(Debug, Clone)]
pub enum ModelStage {
    /// Every block stored, entries in walk order (no classification).
    Store,
    /// The Fast tier: classes, clusters, zstd with a long window. (Balanced comes later.) The
    /// `ingest` field of the options is not used here; the classify stage's is.
    Fast(FastOptions),
    /// The Balanced tier: the Fast tier's classes and clusters, LZMA or zstd per block by a trial
    /// on a sample. The `ingest` field of the options is not used here either.
    Balanced(BalancedOptions),
}

/// The seal stage: the writer's settings (chunking, block size, recovery, encryption). The
/// writer's encoder is the model stage's business: for `Fast` it is replaced, and the block size
/// of the `Fast` options wins.
#[derive(Debug, Default)]
pub struct SealOptions {
    /// The writer's settings. `chunk_size` and `dedup` are set by the fold stage when there is
    /// one (see [`Pipeline::fold`]) and are used as given only without it.
    pub writer: WriterOptions,
}

/// Wall time per stage.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StageTimings {
    /// Walking the tree.
    pub walk: Duration,
    /// Classifying and clustering.
    pub classify: Duration,
    /// Peel stages (reading the inputs they apply to, peeling, verifying).
    pub peel: Duration,
    /// The fold stage (zero: its work is done inside the writer's calls, counted in `model`;
    /// the file ordering is a sort and is not timed).
    pub fold: Duration,
    /// Reading the files and encoding blocks (the writer's `add_*` calls).
    pub model: Duration,
    /// Closing: index, records, recovery, trailer, sync.
    pub seal: Duration,
}

/// What a pipeline run did.
#[derive(Debug, Clone, Copy)]
pub struct RunSummary {
    /// The writer's counts.
    pub writer: WriterSummary,
    /// The Fast encoder's counts (`None` for the store model).
    pub fast: Option<FastSummary>,
    /// The Balanced encoder's counts (`None` unless the Balanced model ran).
    pub balanced: Option<BalancedSummary>,
    /// What the peel stages did (empty for the store model, which does not peel).
    pub peel: PeelSummary,
    /// Wall time per stage.
    pub timings: StageTimings,
    /// The thread budget the run resolved (`threads` of the tier's options, the machine's
    /// logical cores when unset).
    pub threads: usize,
    /// Blocks the writer encoded at once: `threads`, capped by the tier (the Balanced tier's
    /// [`crate::balanced::BALANCED_MAX_IN_FLIGHT`]).
    pub blocks_in_flight: usize,
    /// I/O threads beside the encode threads: a reader thread reading files ahead and a write
    /// thread writing the archive file, each started only when more than one block is in
    /// flight (one block in flight is the sequential path: one thread does everything), the
    /// write thread only for an archive file (`run_file`, `append_file`). They mostly wait on
    /// I/O; 0, 1 or 2.
    pub io_threads: usize,
    /// The most threads that may hash the chunks of a read-ahead segment, the feeding thread
    /// included: 1 with one block in flight or without a reader thread, else the blocks in
    /// flight capped at [`lpk_format::writer::MAX_HASH_THREADS`]. An upper bound: a segment
    /// with few chunk bytes, or a run whose files are all read whole for a peel, uses fewer;
    /// the threads are scoped to one segment at a time.
    pub hash_threads: usize,
}

/// [`RunSummary::hash_threads`] for `blocks_in_flight`, with or without a reader thread.
fn hash_threads(blocks_in_flight: usize, reader: bool) -> usize {
    if !reader {
        return 1;
    }
    blocks_in_flight.clamp(1, lpk_format::writer::MAX_HASH_THREADS)
}

/// The stages in order.
pub struct Pipeline {
    /// Walk and classify.
    pub classify: ClassifyStage,
    /// Peel stages: the first that applies to an input's class peels it (Fast model only).
    pub peel: Vec<Box<dyn PeelStage>>,
    /// The fold stage (chunker and dedup); `None` keeps the writer's fixed chunker, no dedup. A
    /// fold stage sets the writer's `chunk_size` (the longest chunk its chunker makes), so the
    /// `chunk_size` of [`SealOptions`] is overridden whenever this is `Some`.
    pub fold: Option<Box<dyn FoldStage>>,
    /// How blocks are coded.
    pub model: ModelStage,
    /// The writer's settings.
    pub seal: SealOptions,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("classify", &self.classify)
            .field("peel", &self.peel)
            .field("fold", &self.fold)
            .field("model", &self.model)
            .field("seal", &self.seal)
            .finish()
    }
}

/// An encoder built (and validated) before any output exists.
pub(crate) enum Prepared {
    Store,
    Fast(ZstdEncoder, FastHandle),
    Balanced(BalancedEncoder, BalancedHandle),
}

impl Pipeline {
    /// The store pipeline with the writer settings of `options`.
    pub fn store(options: StoreOptions) -> Self {
        let mut writer = options.writer;
        writer.encode_threads = resolve_threads(options.threads);
        Pipeline {
            classify: ClassifyStage {
                ingest: options.ingest,
            },
            peel: Vec::new(),
            fold: None,
            model: ModelStage::Store,
            seal: SealOptions { writer },
        }
    }

    /// The Fast pipeline, with the JPEG peel (reading whole inputs up to
    /// [`FastOptions::jpeg_max_file`]).
    pub fn fast(options: FastOptions) -> Self {
        Pipeline {
            classify: ClassifyStage {
                ingest: options.ingest,
            },
            peel: vec![Box::new(JpegPeel {
                max_file: options.jpeg_max_file,
                ..JpegPeel::default()
            })],
            fold: Some(Box::new(Dedup::default())),
            model: ModelStage::Fast(options),
            seal: SealOptions::default(),
        }
    }

    /// The Balanced pipeline, with the JPEG peel.
    pub fn balanced(options: BalancedOptions) -> Self {
        Pipeline {
            classify: ClassifyStage {
                ingest: options.ingest,
            },
            peel: vec![Box::new(JpegPeel {
                max_file: options.jpeg_max_file,
                ..JpegPeel::default()
            })],
            fold: Some(Box::new(Dedup::default())),
            model: ModelStage::Balanced(options),
            seal: SealOptions::default(),
        }
    }

    /// Validate the options and build the encoder, before any output exists.
    fn prepare(&self) -> Result<Prepared, CoreError> {
        match &self.model {
            ModelStage::Store => Ok(Prepared::Store),
            ModelStage::Fast(o) => {
                let (e, h) = ZstdEncoder::new(o.level, o.window_log, &o.dictionaries, o.gate)?;
                Ok(Prepared::Fast(e, h))
            }
            ModelStage::Balanced(o) => {
                let (e, h) = BalancedEncoder::new(o)?;
                Ok(Prepared::Balanced(e, h))
            }
        }
    }

    /// Walk `root` and write the archive to `out`.
    pub fn run(self, root: &Path, out: impl Write) -> Result<RunSummary, CoreError> {
        let prepared = self.prepare()?;
        let t = Instant::now();
        let inputs = walk(root, &self.classify.ingest)?;
        self.run_inputs(inputs, out, None, prepared, t.elapsed(), new_archive())
    }

    /// Walk `root` and append its entries as the next generation of `existing` (spec section
    /// 15): `out` is the same archive opened for writing and positioned at its end
    /// ([`Writer::append`]'s contract; the caller syncs). A path that exists is replaced. The
    /// stages run as in [`run`](Self::run) (a peeled file's record joins the archive's records);
    /// `existing` must be open with `credentials` when it is encrypted, and its reader is read
    /// lazily between writes (old block graphs, [`Writer::append`]), so it must not share a file
    /// position with `out`: a separately opened handle on the same file is fine. The archive's
    /// own revision applies: a pipeline with a peel stage needs a header `version_minor` of at
    /// least 1 (`BadOptions`, `version_minor`, before anything is written).
    pub fn append<R: Read + Seek + 'static, W: Write>(
        self,
        root: &Path,
        existing: Archive<R>,
        out: W,
        credentials: Option<&Credentials>,
    ) -> Result<RunSummary, CoreError> {
        let prepared = self.prepare()?;
        let t = Instant::now();
        let inputs = walk(root, &self.classify.ingest)?;
        let opener: Opener<'_, W> = Box::new(move |out, wopts, minor, chunker| {
            if existing.header().version.minor < minor {
                return Err(FormatError::BadOptions {
                    reason: "version_minor",
                }
                .into());
            }
            Ok(match chunker {
                Some(c) => Writer::append_with_chunker(existing, out, wopts, credentials, c)?,
                None => Writer::append(existing, out, wopts, credentials)?,
            })
        });
        self.run_inputs(inputs, out, None, prepared, t.elapsed(), opener)
    }

    /// Like [`append`](Self::append), on the archive at `archive_path` in place, with the
    /// guarantees of [`run_file`](Self::run_file): the archive is left out of its own walk
    /// (by file identity, when `root` holds it), the data is synced before the new trailer,
    /// and a failed append (an error, or a panic in a stage, which is re-raised) cuts the file
    /// back to its original length, so the archive stays at the generation it had; when that
    /// cut itself fails the error is [`CoreError::AppendLeftCut`], naming both causes and the
    /// length a rollback restores. The new generation is a snapshot of `root` (spec section
    /// 15): every path of the old entry table that `root` no longer has is deleted from the
    /// entry table (its chunks stay), the rest is replaced or kept. An encrypted archive needs
    /// its `credentials` (a keyless open is refused with `AppendNeedsCredentials`, a wrong
    /// password with `WrongKey`, both before the file is opened for writing). Invalid options
    /// are refused before anything is touched.
    pub fn append_file(
        self,
        root: &Path,
        archive_path: &Path,
        credentials: Option<&Credentials>,
    ) -> Result<RunSummary, CoreError> {
        let prepared = self.prepare()?;
        let start = Instant::now();
        let io = |e| CoreError::io(archive_path, e);
        let mut existing = Archive::open_with(
            std::fs::File::open(archive_path).map_err(io)?,
            &lpk_format::Resources::default(),
            credentials,
        )?;
        if existing.is_keyless() {
            return Err(FormatError::AppendNeedsCredentials.into());
        }
        let old_paths: Vec<String> = existing
            .entry_table()?
            .table()?
            .iter()
            .map(|e| e.map(|e| e.path))
            .collect::<Result<_, _>>()?;
        // The handle that cuts the file back: a plain write handle (an append-mode handle
        // cannot truncate on Windows), opened before anything is written, and the length it
        // restores read from it.
        let mut restore = Restore::open(archive_path)?;
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(archive_path)
            .map_err(io)?;
        let own_id = file_identity(&file).map_err(io)?;
        let sync_file = file.try_clone().map_err(io)?;
        let r = (|| {
            let mut inputs = walk(root, &self.classify.ingest)?;
            inputs.retain(|i| i.identity != Some(own_id));
            let kept: HashSet<&str> = inputs.iter().map(|i| i.path.as_str()).collect();
            let deleted: Vec<String> = old_paths
                .iter()
                .filter(|p| !kept.contains(p.as_str()))
                .cloned()
                .collect();
            let opener: Opener<'_, std::fs::File> = Box::new(move |out, wopts, minor, chunker| {
                let pipelined = wopts.encode_threads > 1;
                if existing.header().version.minor < minor {
                    return Err(FormatError::BadOptions {
                        reason: "version_minor",
                    }
                    .into());
                }
                let mut w = match chunker {
                    Some(c) => Writer::append_with_chunker(existing, out, wopts, credentials, c)?,
                    None => Writer::append(existing, out, wopts, credentials)?,
                };
                for p in &deleted {
                    w.delete_path(p)?;
                }
                // Above one block in flight writing overlaps the cut: a write thread.
                Ok(if pipelined { w.with_write_thread() } else { w })
            });
            let sync: SyncFn = Box::new(move || sync_file.sync_data());
            self.run_inputs(inputs, file, Some(sync), prepared, start.elapsed(), opener)
        })();
        // The writer (and its buffer) is gone by now: whatever reached the file past the old
        // trailer is cut off. A panic inside the closure unwinds through `restore`'s Drop.
        match r {
            Ok(s) => {
                restore.disarm();
                Ok(s)
            }
            Err(cause) => match restore.truncate() {
                Ok(()) => Err(cause),
                Err(truncation) => Err(CoreError::AppendLeftCut {
                    path: archive_path.to_path_buf(),
                    cause: Box::new(cause),
                    kept: restore.len,
                    truncation,
                }),
            },
        }
    }

    /// Like [`run`](Self::run), writing to a new file at `archive_path` with the guarantees of
    /// `archive_store_file`: create-new, the output left out of its own archive, data synced
    /// before the trailer, a failed write removes the file. Invalid options are refused before
    /// the file exists. The walk time then includes creating the file.
    pub fn run_file(self, root: &Path, archive_path: &Path) -> Result<RunSummary, CoreError> {
        let prepared = self.prepare()?;
        let ingest = self.classify.ingest;
        let start = Instant::now();
        create_new_and_run(root, archive_path, &ingest, |inputs, file, sync| {
            self.run_inputs(
                inputs,
                file,
                Some(sync),
                prepared,
                start.elapsed(),
                new_archive_file(),
            )
        })
    }

    /// Run the stages over already-walked inputs; `opener` opens the writer (a new archive, or
    /// an append).
    pub(crate) fn run_inputs<W: Write>(
        self,
        inputs: Vec<Input>,
        out: W,
        sync: Option<SyncFn>,
        prepared: Prepared,
        walk_time: Duration,
        opener: Opener<'_, W>,
    ) -> Result<RunSummary, CoreError> {
        let mut timings = StageTimings {
            walk: walk_time,
            ..StageTimings::default()
        };
        let Pipeline {
            model,
            seal,
            peel,
            fold,
            ..
        } = self;
        let mut wopts = seal.writer;
        let source = Source::new();
        match (model, prepared) {
            (ModelStage::Store, Prepared::Store) => {
                let threads = wopts.encode_threads.max(1);
                let mut writer = open_writer(out, wopts, sync, 0, fold.as_deref(), opener)?;
                let t = Instant::now();
                let mut reads = ReadAhead::spawn(
                    inputs.iter().filter(|i| i.kind == EntryKind::File),
                    threads > 1,
                )?;
                let io_threads = reads.threads() + usize::from(writer.has_write_thread());
                for input in &inputs {
                    validate_input(input)?;
                    match input.kind {
                        EntryKind::Directory => {
                            writer.add_directory(&input.path, input.flags, input.mtime_ns)?;
                        }
                        EntryKind::Symlink => add_symlink(&mut writer, input)?,
                        EntryKind::File => reads.add_file(&mut writer, input)?,
                    }
                }
                timings.model = t.elapsed();
                let t = Instant::now();
                let writer = writer.finish()?;
                timings.seal = t.elapsed();
                Ok(RunSummary {
                    writer,
                    fast: None,
                    balanced: None,
                    peel: PeelSummary::default(),
                    timings,
                    threads,
                    blocks_in_flight: threads,
                    io_threads,
                    hash_threads: hash_threads(threads, reads.threads() > 0),
                })
            }
            (model, prepared) => {
                let (encoder, block_size, soft, steer, finish, threads, in_flight): (
                    Box<dyn lpk_format::BlockEncoder>,
                    u64,
                    Option<u64>,
                    Steer,
                    Finish,
                    usize,
                    usize,
                ) = match (model, prepared) {
                    (ModelStage::Fast(o), Prepared::Fast(e, h)) => {
                        let h2 = h.clone();
                        let threads = resolve_threads(o.threads);
                        (
                            Box::new(e),
                            o.block_size,
                            None,
                            Box::new(move |c, d| h.set_hint(c, d)),
                            Box::new(move |peel| {
                                let mut f = h2.summary();
                                f.peel = peel;
                                (Some(f), None)
                            }),
                            threads,
                            threads,
                        )
                    }
                    (ModelStage::Balanced(o), Prepared::Balanced(e, h)) => {
                        let h2 = h.clone();
                        (
                            Box::new(e),
                            o.block_size,
                            Some(
                                o.min_block_before_boundary
                                    .unwrap_or(u64::from(o.dict_size) / 2),
                            ),
                            Box::new(move |c, d| h.set_hint(c, d)),
                            Box::new(move |peel| {
                                let mut b = h2.summary();
                                b.peel = peel;
                                (None, Some(b))
                            }),
                            resolve_threads(o.threads),
                            o.blocks_in_flight(),
                        )
                    }
                    _ => {
                        return Err(CoreError::InvalidOption(
                            "the model needs a prepared encoder of its own tier".into(),
                        ))
                    }
                };
                for input in &inputs {
                    validate_input(input)?;
                }
                let t = Instant::now();
                let mut clusters = cluster(&inputs)?;
                if soft.is_some() {
                    // Compressible clusters first, stored-by-class last (stable), so an open
                    // block is never forced closed by a stored cluster.
                    clusters.sort_by_key(|c| stored_by_class(c.class));
                }
                timings.classify = t.elapsed();
                if let Some(f) = fold.as_deref() {
                    let mode = f.ordering();
                    for c in &mut clusters {
                        order_cluster(&mut c.inputs, mode);
                    }
                }
                wopts.block_size = block_size;
                wopts.encoder = encoder;
                wopts.encode_threads = in_flight;
                let max_part = wopts.block_size;
                // The header's version_minor is the revision this writer writes under (spec
                // section 2): 1 whenever a peel stage is enabled, whether or not one peels.
                let minor = if peel.is_empty() { 0 } else { 1 };
                let mut writer = open_writer(out, wopts, sync, minor, fold.as_deref(), opener)?;
                let mut summary = PeelSummary::default();
                let mut peel_time = Duration::ZERO;
                let t = Instant::now();
                let mut meta: Vec<&Input> = inputs
                    .iter()
                    .filter(|i| i.kind != EntryKind::File)
                    .collect();
                meta.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
                for input in meta {
                    if input.kind == EntryKind::Directory {
                        writer.add_directory(&input.path, input.flags, input.mtime_ns)?;
                    } else {
                        add_symlink(&mut writer, input)?;
                    }
                }
                // The files streamed whole, in the order the loop below adds them, read
                // ahead; the list and the loop take the choice from the same function.
                let mut reads = ReadAhead::spawn(
                    clusters.iter().flat_map(|c| {
                        let peel = &peel;
                        c.inputs.iter().filter(move |input| {
                            matches!(feed_of(peel, c.class, input.len), Feed::Stream(_))
                        })
                    }),
                    in_flight > 1,
                )?;
                let io_threads = reads.threads() + usize::from(writer.has_write_thread());
                // Plain bytes fed since the last block boundary (approximate across the
                // writer's own size splits).
                let mut in_block = 0u64;
                for (i, c) in clusters.iter().enumerate() {
                    if in_block == 0 {
                        // A block takes the hint of its first cluster.
                        steer(c.class, c.dictionary);
                    }
                    for input in &c.inputs {
                        in_block += input.len;
                        if in_block >= block_size {
                            in_block %= block_size;
                        }
                        let stage = match feed_of(&peel, c.class, input.len) {
                            Feed::Stream(cause) => {
                                if let Some(cause) = cause {
                                    // Too long to read whole: streamed like any file, never held.
                                    summary.note_fallback(cause, input.len);
                                }
                                reads.add_file(&mut writer, input)?;
                                continue;
                            }
                            Feed::Peel(stage) => stage,
                        };
                        // Each input is read once: the peel and the writer use the same bytes.
                        let tp = Instant::now();
                        let data = read_all(&source, input)?;
                        // Dedup: a file whose chunks are all stored already is never peeled.
                        if writer.add_file_if_known(
                            &input.path,
                            input.flags,
                            input.mtime_ns,
                            &data,
                        )? {
                            summary.note_deduplicated(input.len);
                            peel_time += tp.elapsed();
                            continue;
                        }
                        let r = stage.peel(&data, max_part);
                        peel_time += tp.elapsed();
                        match r {
                            Ok(plan) => {
                                let sizes = (
                                    plan.stream.len() as u64,
                                    plan.primary_len,
                                    plan.original_len,
                                );
                                if write_peeled(&mut writer, stage, input, &data, plan)? {
                                    summary.note_primary_deduplicated(sizes.2);
                                } else {
                                    summary.note_peeled(sizes.0, sizes.1, sizes.2);
                                }
                            }
                            Err(cause) => {
                                summary.note_fallback(cause, data.len() as u64);
                                writer.add_file(
                                    &input.path,
                                    input.flags,
                                    input.mtime_ns,
                                    &mut &data[..],
                                )?;
                            }
                        }
                    }
                    let next_stored = clusters.get(i + 1).is_none_or(|n| stored_by_class(n.class));
                    let close = match soft {
                        None => true,
                        Some(min) => in_block >= min || stored_by_class(c.class) || next_stored,
                    };
                    if close {
                        writer.close_block()?;
                        in_block = 0;
                    }
                }
                // Only peel time spent inside the model interval is taken out of it.
                timings.peel = peel_time;
                timings.model = t.elapsed().saturating_sub(peel_time);
                let t = Instant::now();
                let written = writer.finish()?;
                timings.seal = t.elapsed();
                let (fast, balanced) = finish(summary);
                Ok(RunSummary {
                    writer: written,
                    fast,
                    balanced,
                    peel: summary,
                    timings,
                    threads,
                    blocks_in_flight: in_flight,
                    io_threads,
                    hash_threads: hash_threads(in_flight, reads.threads() > 0),
                })
            }
        }
    }
}

/// Cuts an archive back to the length it had before an append, on the error path and on the
/// panic path alike (its `Drop`), unless disarmed by a completed append.
struct Restore {
    file: std::fs::File,
    len: u64,
    armed: bool,
}

impl Restore {
    fn open(path: &Path) -> Result<Self, CoreError> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| CoreError::io(path, e))?;
        let len = file.metadata().map_err(|e| CoreError::io(path, e))?.len();
        Ok(Restore {
            file,
            len,
            armed: true,
        })
    }

    /// The append completed: the file keeps its new generation.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Cut the file back (once).
    fn truncate(&mut self) -> std::io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        self.armed = false;
        self.file.set_len(self.len)
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        // The panic path: clean up, then the panic goes on. The error path reports a failed
        // cut itself, so nothing is lost by ignoring the result here.
        let _ = self.truncate();
    }
}

/// Tells the encoder the class and dictionary kind of the next cluster.
type Steer = Box<dyn Fn(Class, DictionaryKind)>;
/// Reads the encoder's counts once the peel summary is known.
type Finish = Box<dyn Fn(PeelSummary) -> (Option<FastSummary>, Option<BalancedSummary>)>;

type OutWriter<W> = Writer<BufWriter<W>>;

/// Opens the writer of a run from the output, the writer options (the fold stage's chunker
/// installed), the revision the pipeline writes under and the fold stage's chunker: a new
/// archive ([`new_archive`]) or the next generation of an existing one ([`Pipeline::append`]).
pub(crate) type Opener<'a, W> = Box<
    dyn FnOnce(
            BufWriter<W>,
            WriterOptions,
            u16,
            Option<Box<dyn Chunker>>,
        ) -> Result<OutWriter<W>, CoreError>
        + 'a,
>;

/// The opener of a new archive.
pub(crate) fn new_archive<W: Write>() -> Opener<'static, W> {
    Box::new(|out, wopts, minor, chunker| {
        Ok(match chunker {
            Some(c) => Writer::new_revision_with_chunker(out, wopts, minor, c)?,
            None => Writer::new_revision(out, wopts, minor)?,
        })
    })
}

/// The opener of a new archive file: [`new_archive`], the file written by a write thread so
/// writing overlaps the cut when more than one block is in flight.
pub(crate) fn new_archive_file() -> Opener<'static, std::fs::File> {
    Box::new(|out, wopts, minor, chunker| {
        let pipelined = wopts.encode_threads > 1;
        let w = match chunker {
            Some(c) => Writer::new_revision_with_chunker(out, wopts, minor, c)?,
            None => Writer::new_revision(out, wopts, minor)?,
        };
        Ok(if pipelined { w.with_write_thread() } else { w })
    })
}

fn open_writer<W: Write>(
    out: W,
    mut wopts: WriterOptions,
    sync: Option<SyncFn>,
    version_minor: u16,
    fold: Option<&dyn FoldStage>,
    opener: Opener<'_, W>,
) -> Result<OutWriter<W>, CoreError> {
    let out = BufWriter::with_capacity(OUT_BUF, out);
    let chunker = match fold {
        Some(f) => Some(f.install(&mut wopts)?),
        None => None,
    };
    let mut writer = opener(out, wopts, version_minor, chunker)?;
    if let Some(sync) = sync {
        writer = writer.with_sync(sync);
    }
    Ok(writer)
}

fn add_symlink<W: Write>(writer: &mut OutWriter<W>, input: &Input) -> Result<(), CoreError> {
    let target = input.symlink_target.as_deref().unwrap_or_default();
    writer.add_symlink(&input.path, input.flags, input.mtime_ns, target)?;
    Ok(())
}

/// Read a whole input (a file a peel stage applies to); a length that changed since the walk is
/// `ChangedWhileReading`.
fn read_all(source: &Source, input: &Input) -> Result<Vec<u8>, CoreError> {
    let mut data = Vec::with_capacity(usize::try_from(input.len).unwrap_or(0));
    source
        .open(input)?
        .read_to_end(&mut data)
        .map_err(|e| CoreError::io(&input.source, e))?;
    if data.len() as u64 != input.len {
        return Err(CoreError::ChangedWhileReading {
            path: input.source.clone(),
        });
    }
    Ok(data)
}

/// Write a peeled input; true when its primary part was stored already (dedup) and the entry
/// lists those chunks with no record and no block of its own.
///
/// (spec section 9, revision 1.1): the nested parts first, through the
/// model, so their chunks lie in lower blocks; then the record; then the peeled part as a block
/// of its own whose graph names the record.
fn write_peeled<W: Write>(
    writer: &mut OutWriter<W>,
    stage: &dyn PeelStage,
    input: &Input,
    data: &[u8],
    plan: PeelPlan,
) -> Result<bool, CoreError> {
    writer.begin_entry(&input.path, input.flags, input.mtime_ns)?;
    // Dedup: a primary part whose chunks are all stored already needs no block and no record
    // (the entry lists those chunks); the nested parts go through the normal path.
    if writer
        .add_part_known(0, &data[..plan.primary_len as usize])?
        .is_some()
    {
        for p in &plan.nested {
            let (a, b) = (p.offset as usize, (p.offset + p.len) as usize);
            writer.add_part(p.offset, &mut &data[a..b])?;
        }
        writer.end_entry()?;
        return Ok(true);
    }
    let mut lists = Vec::with_capacity(plan.nested.len());
    for p in &plan.nested {
        let (a, b) = (p.offset as usize, (p.offset + p.len) as usize);
        lists.push(writer.add_part(p.offset, &mut &data[a..b])?);
    }
    let id = writer.add_record(stage.record(&plan, &lists))?;
    let mut params = Vec::new();
    lpk_format::varint::write(&mut params, id)?;
    let encoded = Encoded {
        graph: Graph {
            steps: vec![Step {
                primitive: plan.primitive,
                params,
            }],
        },
        bytes: plan.stream,
        resources: GraphResources::default(),
        memory: 0,
    };
    writer.add_part_encoded(0, &data[..plan.primary_len as usize], encoded, plan.memory)?;
    writer.end_entry()?;
    Ok(false)
}

/// Add a file read on this thread as it is added (the sequential path); a length that changed
/// since the walk is `ChangedWhileReading`, after its bytes were added.
fn add_file_here<W: Write>(writer: &mut OutWriter<W>, input: &Input) -> Result<(), CoreError> {
    let mut r = Counting {
        inner: Source::new().open(input)?,
        n: 0,
    };
    writer.add_file(&input.path, input.flags, input.mtime_ns, &mut r)?;
    if r.n != input.len {
        return Err(CoreError::ChangedWhileReading {
            path: input.source.clone(),
        });
    }
    Ok(())
}

/// Counts what passes through a reader.
struct Counting<R> {
    inner: R,
    n: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.n += n as u64;
        Ok(n)
    }
}

/// How the run adds a file of `class` and length `len`.
enum Feed<'a> {
    /// Streamed whole: no peel stage applies, or the stage refuses it unread (the cause).
    Stream(Option<crate::peel::Fallback>),
    /// Read whole for this peel stage.
    Peel(&'a dyn PeelStage),
}

/// The one place the run decides how a file is added (the read-ahead's list and the loop both
/// ask it, so they cannot disagree).
fn feed_of(peel: &[Box<dyn PeelStage>], class: Class, len: u64) -> Feed<'_> {
    match peel.iter().find(|s| s.applies_to(class)) {
        None => Feed::Stream(None),
        Some(stage) => match stage.refuse_unread(len) {
            Some(cause) => Feed::Stream(Some(cause)),
            None => Feed::Peel(stage.as_ref()),
        },
    }
}

/// The files a run streams whole. Above one block in flight they are read ahead by a reader
/// thread in the order the run adds them ([`Prefetch`]; joined when this is dropped); with one
/// block in flight each is read on the run's thread as it is added.
struct ReadAhead {
    prefetch: Option<Prefetch<CoreError>>,
    /// Files handed to the writer so far: the id the next file must carry.
    next: usize,
}

impl ReadAhead {
    /// Start reading `files` (in this order) ahead when `pipelined`.
    fn spawn<'a>(
        files: impl Iterator<Item = &'a Input>,
        pipelined: bool,
    ) -> Result<Self, CoreError> {
        if !pipelined {
            return Ok(ReadAhead {
                prefetch: None,
                next: 0,
            });
        }
        let list: Vec<(usize, OpenFn<CoreError>)> = files
            .enumerate()
            .map(|(id, input)| {
                let input = input.clone();
                let open: OpenFn<CoreError> = Box::new(move || Source::new().open(&input));
                (id, open)
            })
            .collect();
        let prefetch = Prefetch::spawn(list, PrefetchOptions::default())
            .map_err(|e| CoreError::Format(FormatError::Io(e)))?;
        Ok(ReadAhead {
            prefetch: Some(prefetch),
            next: 0,
        })
    }

    /// Reader threads running (0 or 1).
    fn threads(&self) -> usize {
        usize::from(self.prefetch.is_some())
    }

    /// Add the next file read ahead, which must be `input`; a length that changed since the
    /// walk is `ChangedWhileReading`, after its bytes were added.
    fn add_file<W: Write>(
        &mut self,
        writer: &mut OutWriter<W>,
        input: &Input,
    ) -> Result<(), CoreError> {
        let Some(prefetch) = &mut self.prefetch else {
            return add_file_here(writer, input);
        };
        let mut file = match prefetch.next_file() {
            Ok(f) => f,
            Err(NextFileError::Open(e)) => return Err(e),
            Err(NextFileError::Panicked(p)) => std::panic::resume_unwind(p),
            Err(NextFileError::OutOfStep | NextFileError::Ended) => {
                return Err(CoreError::InvalidOption(
                    "read-ahead out of step with the run".into(),
                ))
            }
        };
        if file.id() != self.next {
            return Err(CoreError::InvalidOption(
                "read-ahead out of step with the run".into(),
            ));
        }
        self.next += 1;
        writer.add_file_segments(
            &input.path,
            input.flags,
            input.mtime_ns,
            &mut file,
            input.len,
        )?;
        if file.bytes_read() != input.len {
            return Err(CoreError::ChangedWhileReading {
                path: input.source.clone(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::{clear_readonly, make_tree};

    #[test]
    fn stages_run_in_order_and_timings_are_filled() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let mut out = Vec::new();
        let p = Pipeline::fast(FastOptions::default());
        assert!(p.peel.len() == 1 && p.fold.is_some());
        let s = p.run(dir.path(), &mut out).unwrap();
        assert_eq!(s.writer.archive_len, out.len() as u64);
        let fast = s.fast.unwrap();
        assert!(fast.zstd_blocks + fast.stored_by_class + fast.stored_by_gate > 0);
        let t = s.timings;
        assert!(t.walk > Duration::ZERO && t.model > Duration::ZERO);
        assert!(t.seal > Duration::ZERO);
        assert_eq!(t.fold, Duration::ZERO);
        assert_eq!(s.peel, PeelSummary::default());
        let mut out = Vec::new();
        let s = Pipeline::store(StoreOptions::default())
            .run(dir.path(), &mut out)
            .unwrap();
        assert!(s.fast.is_none());
        assert_eq!(s.timings.classify, Duration::ZERO);
        clear_readonly(&dir.path().join("ro.txt"));
    }

    #[test]
    fn run_file_refuses_bad_options_before_creating_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let arch = out.path().join("x.lpk");
        let opts = FastOptions {
            window_log: 40,
            ..FastOptions::default()
        };
        let e = Pipeline::fast(opts)
            .run_file(dir.path(), &arch)
            .unwrap_err();
        assert!(matches!(e, CoreError::InvalidOption(_)));
        assert!(!arch.exists());
    }
}
