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
    /// The sink of an export (the ZIP or 7z writer) failed on an entry.
    #[error("{}: export: {entry:?}: {message}", path.display())]
    Export {
        /// The archive being written.
        path: PathBuf,
        /// The archive path of the entry being written (empty for the archive as a whole).
        entry: String,
        /// The writer's error.
        message: String,
    },
    /// Something of another kind exists where an extracted entry goes (a directory where a
    /// file goes, a file or a link where a directory goes); refused before any output in every
    /// overwrite mode but `skip`.
    #[error("{}: a {found} exists where a {wanted} goes", path.display())]
    InTheWay {
        /// The target on disk.
        path: PathBuf,
        /// What is there: "file", "directory" or "symlink".
        found: &'static str,
        /// What the entry is: "file", "directory" or "symlink".
        wanted: &'static str,
    },
    /// Two entries whose paths differ only by case, refused before any output because the
    /// target is case-insensitive.
    #[error(
        "entries {first:?} and {second:?} differ only by case; the target is case-insensitive"
    )]
    CaseCollision {
        /// The first of the two in entry order.
        first: String,
        /// The second.
        second: String,
    },
    /// The run was cancelled through its [`CancelToken`](crate::CancelToken); what it had
    /// written is cleaned up as after a failure (a new archive removed, an append cut back,
    /// an extraction's unfinished files removed).
    #[error("cancelled")]
    Cancelled,
    /// A selection pattern (`lpk x`, `lpk l`, `lpk export`) that matches no entry of the
    /// archive; refused before any output.
    #[error("pattern {pattern:?} matches no entry")]
    NoMatch {
        /// The pattern as given.
        pattern: String,
    },
    /// The root given to a walk is not a directory.
    #[error("{}: not a directory", path.display())]
    NotADirectory {
        /// The root.
        path: PathBuf,
    },
    /// An append failed and the archive could not be cut back to its previous generation: the
    /// file holds a cut generation past `kept` bytes, which a rollback recovers.
    #[error(
        "{}: append failed ({cause}); the file could not be cut back to {kept} bytes ({truncation}) and holds a cut generation that rollback recovers",
        path.display()
    )]
    AppendLeftCut {
        /// The archive.
        path: PathBuf,
        /// Why the append failed.
        cause: Box<CoreError>,
        /// The length of the previous generation, where the file should have been cut.
        kept: u64,
        /// Why the truncation failed.
        truncation: std::io::Error,
    },
    /// A foreign archive (read by a sandboxed worker, [`crate::foreign`]) was refused or is
    /// damaged: its listing, its sizes or a limit.
    #[error(transparent)]
    Foreign(#[from] ForeignError),
    /// The sandboxed worker of a foreign archive failed: it could not start, broke the
    /// protocol, went silent, crashed or hit a limit.
    #[error(transparent)]
    Worker(#[from] WorkerError),
}

/// What is wrong with a foreign archive (D-77: each a named kind, exit 2).
#[derive(Debug, thiserror::Error)]
pub enum ForeignError {
    /// The file is not an archive of a format this build reads.
    #[error("not an archive of a format this build reads")]
    UnknownFormat,
    /// A method, feature or format version the reader does not implement.
    #[error("unsupported: {message}")]
    Unsupported {
        /// The reader's or the broker's details (escaped).
        message: String,
    },
    /// The archive is damaged (the reader's report, or a listing the broker refuses).
    #[error("damaged archive: {message}")]
    Corrupt {
        /// Details (escaped).
        message: String,
    },
    /// A member's data did not match its declared size; the file was removed.
    #[error("{path:?}: declared {declared} bytes, the worker sent {received} or more")]
    SizeMismatch {
        /// The member's name.
        path: String,
        /// Its declared size.
        declared: u64,
        /// The bytes received when the mismatch was seen.
        received: u64,
    },
    /// Two members whose packed ranges in different blocks overlap.
    #[error("members {first:?} and {second:?} overlap in the archive")]
    OverlappingEntries {
        /// The member whose range starts first.
        first: String,
        /// The other member.
        second: String,
    },
    /// A limit of the broker (raise it with the flag named in the message).
    #[error("{limit}: {value} exceeds the limit of {max} (see {flag})")]
    LimitExceeded {
        /// What was limited.
        limit: &'static str,
        /// The value reached (at least).
        value: u64,
        /// The limit.
        max: u64,
        /// The option that changes it.
        flag: &'static str,
    },
    /// Not enough free space at the destination for what the archive declares.
    #[error("not enough free space: {needed} bytes needed, {available} available")]
    NoSpace {
        /// Bytes still to write.
        needed: u64,
        /// Bytes free.
        available: u64,
    },
}

/// How the sandboxed worker failed (D-77: each a named kind, exit 2).
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// The worker could not be started (its executable missing, the OS refused).
    #[error("worker could not start: {message}")]
    Spawn {
        /// Details.
        message: String,
    },
    /// The sandbox could not be set up (the AppContainer profile, a mitigation).
    #[error("worker sandbox: {message}")]
    Sandbox {
        /// Details.
        message: String,
    },
    /// The worker reported that the sandbox refused something it tried.
    #[error("worker was denied: {message}")]
    Denied {
        /// The worker's report (escaped).
        message: String,
    },
    /// The worker died (exit status or exception code), not by a limit.
    #[error("worker crashed (exit status {code:#x}){}", stderr_suffix(stderr))]
    Crashed {
        /// The exit status (Windows: the exception code when one killed it; Linux: 128 + the
        /// signal number).
        code: u32,
        /// Its diagnostics, escaped (the first and last 32 KiB).
        stderr: String,
    },
    /// The worker's reader panicked.
    #[error("worker panicked: {message}")]
    Panicked {
        /// The panic message (escaped).
        message: String,
    },
    /// The worker was stopped at its memory limit.
    #[error("worker reached its memory limit of {limit} bytes (see --memory)")]
    MemoryLimit {
        /// The limit.
        limit: u64,
    },
    /// The worker was stopped at its CPU-time limit.
    #[error("worker reached its CPU-time limit of {seconds} s (see --max-cpu)")]
    CpuLimit {
        /// The limit in whole seconds.
        seconds: u64,
    },
    /// The worker made no observable progress for too long and was stopped.
    #[error(
        "worker made no progress for {seconds:.1} s while the broker waited for {waiting_for}"
    )]
    Timeout {
        /// How long it was silent.
        seconds: f64,
        /// What the broker was waiting for.
        waiting_for: &'static str,
    },
    /// The worker sent something that is not the protocol.
    #[error("worker broke the protocol: {message}")]
    Protocol {
        /// What was wrong.
        message: String,
    },
    /// The worker closed its pipe without an answer.
    #[error("worker closed its pipe{}", stderr_suffix(stderr))]
    Pipe {
        /// Its diagnostics, escaped.
        stderr: String,
    },
    /// The worker is from another build than the host.
    #[error("worker build {worker:?} does not match the host's {host:?}")]
    BuildMismatch {
        /// The host's build.
        host: String,
        /// The worker's (escaped).
        worker: String,
    },
}

fn stderr_suffix(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        format!("; its diagnostics: {s}")
    }
}

impl CoreError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        CoreError::Io {
            path: path.into(),
            source,
        }
    }
}
