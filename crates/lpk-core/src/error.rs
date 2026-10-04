//! Errors of the pipeline.

use std::path::PathBuf;

/// What can go wrong between a directory tree and an archive.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The format crate refused something (a path, an order, an output error).
    #[error(transparent)]
    Format(#[from] lpk_format::FormatError),
    /// An I/O error on a named path.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path being read or created.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A name that cannot be an archive path; the walk fails rather than skip it.
    #[error("unportable name {path:?}: {reason}")]
    UnportableName {
        /// The path, lossily decoded when it is not UTF-8.
        path: String,
        /// Why (the format's reason string, or "not utf-8", or "special file").
        reason: String,
    },
    /// A FIFO, socket, device or other entry that is not a file, directory or symlink.
    #[error("{}: special file", path.display())]
    SpecialFile {
        /// The entry.
        path: PathBuf,
    },
    /// Two inputs with the same archive path.
    #[error("duplicate archive path {path:?}")]
    DuplicatePath {
        /// The path.
        path: String,
    },
    /// A file's length (or identity, or kind) differed from what was read while archiving it.
    #[error("{}: changed while reading", path.display())]
    ChangedWhileReading {
        /// The file.
        path: PathBuf,
    },
    /// An option outside the range the format or the encoder accepts.
    #[error("invalid option: {0}")]
    InvalidOption(String),
    /// A block an extraction needs failed to decode or to check (`ChunkMismatch`, a damaged
    /// frame, ...); names the block and the first file with a chunk in it.
    #[error("block {block} (first file {path:?}): {source}")]
    Decode {
        /// The block's index in the block table.
        block: usize,
        /// The archive path of the first file placed in the block.
        path: String,
        /// The reader's error.
        source: lpk_format::FormatError,
    },
    /// An extraction's internal step failed (a decode worker stopped without a result).
    #[error("extraction: {0}")]
    Extract(&'static str),
    /// The root given to a walk is not a directory.
    #[error("{}: not a directory", path.display())]
    NotADirectory {
        /// The root.
        path: PathBuf,
    },
}

impl CoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        CoreError::Io {
            path: path.into(),
            source,
        }
    }
}
