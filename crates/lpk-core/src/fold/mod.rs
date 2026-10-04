//! Fold stages: deduplicate across streams. The first is [`Dedup`]: content-defined chunking and
//! global chunk dedup within one archive. The writer does the work (a chunk whose BLAKE3 and
//! length match a chunk already stored is referenced by its index, nothing is written); the
//! stage chooses the chunker and switches the writer's dedup on.

pub mod chunker;

pub use chunker::{FastCdcChunker, AVG_CHUNK, MAX_CHUNK, MIN_CHUNK};

use lpk_format::{Chunker, FixedChunker, WriterOptions};

use crate::error::CoreError;

/// How a fold stage cuts files into chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkerKind {
    /// FastCDC with these sizes in bytes; the writer's `chunk_size` becomes `max`.
    Cdc {
        /// Smallest chunk.
        min: usize,
        /// Average chunk.
        avg: usize,
        /// Largest chunk.
        max: usize,
    },
    /// Chunks of exactly this many bytes (the last of a file shorter); at least 4 KiB.
    Fixed(usize),
}

impl Default for ChunkerKind {
    /// FastCDC, 4 KiB minimum, 64 KiB average, 512 KiB maximum.
    fn default() -> Self {
        ChunkerKind::Cdc {
            min: MIN_CHUNK,
            avg: AVG_CHUNK,
            max: MAX_CHUNK,
        }
    }
}

/// Settings of the fold stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldOptions {
    /// Deduplicate identical chunks within the archive.
    pub dedup: bool,
    /// How files are cut into chunks.
    pub chunker: ChunkerKind,
}

impl Default for FoldOptions {
    fn default() -> Self {
        FoldOptions {
            dedup: true,
            chunker: ChunkerKind::default(),
        }
    }
}

/// A fold stage deduplicates across streams. It runs on the peeled streams (the output of the
/// peel stages) and installs its chunker and dedup settings in the writer before any output
/// exists.
pub trait FoldStage: std::fmt::Debug + Send + Sync {
    /// Short name, for reports.
    fn name(&self) -> &'static str;
    /// Adjust the writer's settings (`chunk_size`, `dedup`) and return the chunker it uses.
    fn install(&self, options: &mut WriterOptions) -> Result<Box<dyn Chunker>, CoreError>;
}

/// The first fold stage: chunking and global dedup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dedup {
    /// The settings.
    pub options: FoldOptions,
}

impl Dedup {
    /// A stage with `options`.
    pub fn new(options: FoldOptions) -> Self {
        Dedup { options }
    }
}

impl FoldStage for Dedup {
    fn name(&self) -> &'static str {
        "dedup"
    }

    fn install(&self, options: &mut WriterOptions) -> Result<Box<dyn Chunker>, CoreError> {
        options.dedup = self.options.dedup;
        match self.options.chunker {
            ChunkerKind::Cdc { min, avg, max } => {
                let c = FastCdcChunker::new(min, avg, max)?;
                options.chunk_size = max as u64;
                Ok(Box::new(c))
            }
            ChunkerKind::Fixed(size) => {
                if (size as u64) < lpk_format::MIN_CHUNK_SIZE {
                    return Err(CoreError::InvalidOption(
                        "fixed chunk size below 4 KiB".into(),
                    ));
                }
                options.chunk_size = size as u64;
                Ok(Box::new(FixedChunker::new(size)))
            }
        }
    }
}
