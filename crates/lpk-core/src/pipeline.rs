//! The pipeline: the fixed stage order `classify -> peel -> fold -> model -> seal` as a struct,
//! and the one code path that turns a directory tree into an archive.
//!
//! Only `classify`, `model` and `seal` do work today. Peel (turn one input stream into a peeled
//! stream plus a reconstruction record) and Fold (deduplicate and delta across streams; it runs
//! on the peeled streams, never on the raw inputs) are empty traits the later tasks fill.
//! [`archive_store`](crate::archive_store) and [`archive_fast`](crate::archive_fast) are thin
//! fronts over [`Pipeline::run`].
//!
//! Blocks are encoded one after the other; parallel encoding needs the writer to hold blocks in
//! flight and comes with a later task.

use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use lpk_format::{EntryKind, Writer, WriterOptions, WriterSummary};

use crate::cluster::cluster;
use crate::error::CoreError;
use crate::fast::{FastHandle, FastOptions, FastSummary, ZstdEncoder};
use crate::ingest::{validate_input, walk, IngestOptions, Input};
use crate::source::Source;
use crate::store::{create_new_and_run, Counting, StoreOptions, SyncFn, OUT_BUF};

/// A peel stage turns one input stream (a file whose bytes are an already-compressed container,
/// for example a Deflate stream inside a PDF) into a peeled stream plus a reconstruction record
/// that restores the original bit for bit. No implementation exists yet.
pub trait PeelStage: std::fmt::Debug + Send + Sync {}

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
    /// Peel stages (none yet).
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
    /// Wall time per stage.
    pub timings: StageTimings,
}

/// The stages in order.
pub struct Pipeline {
    /// Walk and classify.
    pub classify: ClassifyStage,
    /// Peel stages, applied in order (none yet).
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

    /// The Fast pipeline.
    pub fn fast(options: FastOptions) -> Self {
        Pipeline {
            classify: ClassifyStage {
                ingest: options.ingest.clone(),
            },
            peel: Vec::new(),
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
        let ingest = self.classify.ingest.clone();
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
        let Pipeline { model, seal, .. } = self;
        let mut wopts = seal.writer;
        let source = Source::new();
        match (model, prepared) {
            (ModelStage::Store, _) => {
                let mut writer = open_writer(out, wopts, sync)?;
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
                let mut writer = open_writer(out, wopts, sync)?;
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
                    for input in &c.inputs {
                        add_file(&mut writer, &source, input)?;
                    }
                    writer.close_block()?;
                }
                timings.model = t.elapsed();
                let t = Instant::now();
                let summary = writer.finish()?;
                timings.seal = t.elapsed();
                Ok(RunSummary {
                    writer: summary,
                    fast: Some(handle.summary()),
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
) -> Result<OutWriter<W>, CoreError> {
    let mut writer = Writer::new(BufWriter::with_capacity(OUT_BUF, out), wopts)?;
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
        assert!(p.peel.is_empty() && p.fold.is_none());
        let s = p.run(dir.path(), &mut out).unwrap();
        assert_eq!(s.writer.archive_len, out.len() as u64);
        let fast = s.fast.unwrap();
        assert!(fast.zstd_blocks + fast.stored_by_class + fast.stored_by_gate > 0);
        let t = s.timings;
        assert!(t.walk > Duration::ZERO && t.model > Duration::ZERO);
        assert!(t.seal > Duration::ZERO);
        assert_eq!((t.peel, t.fold), (Duration::ZERO, Duration::ZERO));
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
        let e = Pipeline::fast(opts).run_file(dir.path(), &arch).unwrap_err();
        assert!(matches!(e, CoreError::InvalidOption(_)));
        assert!(!arch.exists());
    }
}
