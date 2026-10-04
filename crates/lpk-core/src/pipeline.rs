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

use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use lpk_format::{
    Encoded, EntryKind, Graph, GraphResources, Step, Writer, WriterOptions, WriterSummary,
};

use crate::balanced::{BalancedEncoder, BalancedHandle, BalancedOptions, BalancedSummary};
use crate::classify::Class;
use crate::cluster::{cluster, DictionaryKind};
use crate::error::CoreError;
use crate::fast::stored_by_class;
use crate::fast::{FastHandle, FastOptions, FastSummary, ZstdEncoder};
use crate::fold::{Dedup, FoldStage};
use crate::ingest::{validate_input, walk, IngestOptions, Input};
use crate::peel::{JpegPeel, PeelPlan, PeelStage, PeelSummary};
use crate::source::Source;
use crate::store::{create_new_and_run, Counting, StoreOptions, SyncFn, OUT_BUF};

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
    /// The fold stage (zero: its work is done inside the writer's calls, counted in `model`).
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
        Pipeline {
            classify: ClassifyStage {
                ingest: options.ingest,
            },
            peel: Vec::new(),
            fold: None,
            model: ModelStage::Store,
            seal: SealOptions {
                writer: options.writer,
            },
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
        self.run_inputs(inputs, out, None, prepared, t.elapsed())
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
            self.run_inputs(inputs, file, Some(sync), prepared, start.elapsed())
        })
    }

    /// Run the stages over already-walked inputs.
    pub(crate) fn run_inputs(
        self,
        inputs: Vec<Input>,
        out: impl Write,
        sync: Option<SyncFn>,
        prepared: Prepared,
        walk_time: Duration,
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
                let mut writer = open_writer(out, wopts, sync, 0, fold.as_deref())?;
                let t = Instant::now();
                for input in &inputs {
                    validate_input(input)?;
                    match input.kind {
                        EntryKind::Directory => {
                            writer.add_directory(&input.path, input.flags, input.mtime_ns)?;
                        }
                        EntryKind::Symlink => add_symlink(&mut writer, input)?,
                        EntryKind::File => add_file(&mut writer, &source, input)?,
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
                })
            }
            (model, prepared) => {
                let (encoder, block_size, soft, steer, finish): (
                    Box<dyn lpk_format::BlockEncoder>,
                    u64,
                    Option<u64>,
                    Steer,
                    Finish,
                ) = match (model, prepared) {
                    (ModelStage::Fast(o), Prepared::Fast(e, h)) => {
                        let h2 = h.clone();
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
                wopts.block_size = block_size;
                wopts.encoder = encoder;
                let max_part = wopts.block_size;
                // The header's version_minor is the revision this writer writes under (spec
                // section 2): 1 whenever a peel stage is enabled, whether or not one peels.
                let minor = if peel.is_empty() { 0 } else { 1 };
                let mut writer = open_writer(out, wopts, sync, minor, fold.as_deref())?;
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
                // Plain bytes fed since the last block boundary (approximate across the
                // writer's own size splits).
                let mut in_block = 0u64;
                for (i, c) in clusters.iter().enumerate() {
                    if in_block == 0 {
                        // A block takes the hint of its first cluster.
                        steer(c.class, c.dictionary);
                    }
                    let stage = peel.iter().find(|s| s.applies_to(c.class));
                    for input in &c.inputs {
                        in_block += input.len;
                        if in_block >= block_size {
                            in_block %= block_size;
                        }
                        let Some(stage) = stage else {
                            add_file(&mut writer, &source, input)?;
                            continue;
                        };
                        if let Some(cause) = stage.refuse_unread(input.len) {
                            // Too long to read whole: streamed like any file, never held.
                            summary.note_fallback(cause, input.len);
                            add_file(&mut writer, &source, input)?;
                            continue;
                        }
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
                                if write_peeled(&mut writer, stage.as_ref(), input, &data, plan)? {
                                    summary.note_deduplicated(sizes.2);
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
                })
            }
        }
    }
}

/// Tells the encoder the class and dictionary kind of the next cluster.
type Steer = Box<dyn Fn(Class, DictionaryKind)>;
/// Reads the encoder's counts once the peel summary is known.
type Finish = Box<dyn Fn(PeelSummary) -> (Option<FastSummary>, Option<BalancedSummary>)>;

type OutWriter<W> = Writer<BufWriter<W>>;

fn open_writer<W: Write>(
    out: W,
    mut wopts: WriterOptions,
    sync: Option<SyncFn>,
    version_minor: u16,
    fold: Option<&dyn FoldStage>,
) -> Result<OutWriter<W>, CoreError> {
    let out = BufWriter::with_capacity(OUT_BUF, out);
    let mut writer = match fold {
        Some(f) => {
            let chunker = f.install(&mut wopts)?;
            Writer::new_revision_with_chunker(out, wopts, version_minor, chunker)?
        }
        None => Writer::new_revision(out, wopts, version_minor)?,
    };
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
    };
    writer.add_part_encoded(0, &data[..plan.primary_len as usize], encoded, plan.memory)?;
    writer.end_entry()?;
    Ok(false)
}

fn add_file<W: Write>(
    writer: &mut OutWriter<W>,
    source: &Source,
    input: &Input,
) -> Result<(), CoreError> {
    let mut r = Counting {
        inner: source.open(input)?,
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
