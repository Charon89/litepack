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
    /// A file's length differed from what was read while archiving it.
    #[error("{}: changed while reading", path.display())]
    ChangedWhileReading {
        /// The file.
        path: PathBuf,
    },
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
