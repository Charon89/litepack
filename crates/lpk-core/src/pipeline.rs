//! The pipeline: the fixed stage order `classify -> peel -> fold -> model -> seal` as a struct,
//! and the one code path that turns a directory tree into an archive.
//!
//! Peel (turn one input stream into a peeled stream plus a reconstruction record; the JPEG peel
//! is the first, [`crate::peel`]) runs in the Fast model on the inputs of the classes a stage
//! applies to; Fold (deduplicate and delta across streams; it runs on the peeled streams, never
//! on the raw inputs) is an empty trait a later task fills.
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

use crate::cluster::cluster;
use crate::error::CoreError;
use crate::fast::{FastHandle, FastOptions, FastSummary, ZstdEncoder};
use crate::ingest::{validate_input, walk, IngestOptions, Input};
use crate::peel::{JpegPeel, PeelPlan, PeelStage, PeelSummary};
use crate::source::Source;
use crate::store::{create_new_and_run, Counting, StoreOptions, SyncFn, OUT_BUF};

/// A fold stage deduplicates and deltas across streams. It runs on the peeled streams (the
/// output of the peel stages), so duplicates hidden inside containers are found. No
/// implementation exists yet.
pub trait FoldStage: std::fmt::Debug + Send + Sync {}

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
}

/// The seal stage: the writer's settings (chunking, block size, recovery, encryption). The
/// writer's encoder is the model stage's business: for `Fast` it is replaced, and the block size
/// of the `Fast` options wins.
#[derive(Debug, Default)]
pub struct SealOptions {
    /// The writer's settings.
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
    /// The fold stage (none yet).
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
    /// The fold stage (none yet).
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
type Prepared = Option<(ZstdEncoder, FastHandle)>;

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
            fold: None,
            model: ModelStage::Fast(options),
            seal: SealOptions::default(),
        }
    }

    /// Validate the options and build the encoder, before any output exists.
    fn prepare(&self) -> Result<Prepared, CoreError> {
        match &self.model {
            ModelStage::Store => Ok(None),
            ModelStage::Fast(o) => Ok(Some(ZstdEncoder::new(
                o.level,
                o.window_log,
                &o.dictionaries,
                o.gate,
            )?)),
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
            model, seal, peel, ..
        } = self;
        let mut wopts = seal.writer;
        let source = Source::new();
        match (model, prepared) {
            (ModelStage::Store, _) => {
                let mut writer = open_writer(out, wopts, sync, 0)?;
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
                    peel: PeelSummary::default(),
                    timings,
                })
            }
            (ModelStage::Fast(options), Some((encoder, handle))) => {
                for input in &inputs {
                    validate_input(input)?;
                }
                let t = Instant::now();
                let clusters = cluster(&inputs)?;
                timings.classify = t.elapsed();
                wopts.block_size = options.block_size;
                wopts.encoder = Box::new(encoder);
                let max_part = wopts.block_size;
                // The header's version_minor is the revision this writer writes under (spec
                // section 2): 1 whenever a peel stage is enabled, whether or not one peels.
                let minor = if peel.is_empty() { 0 } else { 1 };
                let mut writer = open_writer(out, wopts, sync, minor)?;
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
                for c in &clusters {
                    handle.set_hint(c.class, c.dictionary);
                    let stage = peel.iter().find(|s| s.applies_to(c.class));
                    for input in &c.inputs {
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
                        let r = stage.peel(&data, max_part);
                        peel_time += tp.elapsed();
                        match r {
                            Ok(plan) => {
                                summary.note_peeled(&plan);
                                write_peeled(&mut writer, stage.as_ref(), input, &data, plan)?;
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
                    writer.close_block()?;
                }
                // Only peel time spent inside the model interval is taken out of it.
                timings.peel = peel_time;
                timings.model = t.elapsed().saturating_sub(peel_time);
                let t = Instant::now();
                let written = writer.finish()?;
                timings.seal = t.elapsed();
                let mut fast = handle.summary();
                fast.peel = summary;
                Ok(RunSummary {
                    writer: written,
                    fast: Some(fast),
                    peel: summary,
                    timings,
                })
            }
            (ModelStage::Fast(_), None) => Err(CoreError::InvalidOption(
                "the Fast model needs a prepared encoder".into(),
            )),
        }
    }
}

type OutWriter<W> = Writer<BufWriter<W>>;

fn open_writer<W: Write>(
    out: W,
    wopts: WriterOptions,
    sync: Option<SyncFn>,
    version_minor: u16,
) -> Result<OutWriter<W>, CoreError> {
    let mut writer =
        Writer::new_revision(BufWriter::with_capacity(OUT_BUF, out), wopts, version_minor)?;
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

/// Write a peeled input (spec section 9, revision 1.1): the nested parts first, through the
/// model, so their chunks lie in lower blocks; then the record; then the peeled part as a block
/// of its own whose graph names the record.
fn write_peeled<W: Write>(
    writer: &mut OutWriter<W>,
    stage: &dyn PeelStage,
    input: &Input,
    data: &[u8],
    plan: PeelPlan,
) -> Result<(), CoreError> {
    writer.begin_entry(&input.path, input.flags, input.mtime_ns)?;
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
    Ok(())
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
        assert!(p.peel.len() == 1 && p.fold.is_none());
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
