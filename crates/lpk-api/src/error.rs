//! The API's errors (D-77's contract, E5-1): every failure is an [`Error`] carrying a
//! [`Class`] that keys the user-facing message ([`Class::message`]), D-77's `kind` (the
//! engine error's variant name, what `lpk --json` prints), the engine's own message, the path
//! the error names (else the archive the operation worked on), and, for an envelope refusal,
//! its figures ([`Error::refusal`]).
//!
//! The classification is an exhaustive match over every variant of [`CoreError`],
//! [`FormatError`], [`ForeignError`] and [`WorkerError`] (moved here from `lpk-cli`'s
//! `failure.rs`, which now calls it), so a variant added to any of them does not compile until
//! it is placed. Exit codes (D-48): [`Class::Usage`] is 1, every other class 2.

use std::path::Path;

use lpk_core::{CoreError, ForeignError, WorkerError};
use lpk_format::{FormatError, Refusal};

/// Exit code of a usage error.
pub const EXIT_USAGE: i32 = 1;
/// Exit code of a refused or failed operation.
pub const EXIT_FAILED: i32 = 2;

/// What kind of failure an [`Error`] is, for a front end's message and choices. New classes
/// may be added (the enum is `#[non_exhaustive]`).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// The request was wrong (an option, a pattern, a combination): exit 1.
    Usage,
    /// A file, an archive, a prior or a selected entry does not exist.
    NotFound,
    /// The operating system or the sandbox denied access.
    AccessDenied,
    /// The disk is full.
    DiskFull,
    /// The archive is encrypted and no password was given.
    PasswordRequired,
    /// The password (or keyfile) is wrong.
    WrongPassword,
    /// The archive (or a foreign archive) is damaged or not an archive.
    Damaged,
    /// The archive needs something this build does not have.
    Unsupported,
    /// Over a limit: memory, size, entries, expansion, a preview cap.
    TooLarge,
    /// A path or entry was refused as unsafe or unportable.
    Unsafe,
    /// Something is in the way: an existing file, a duplicate name.
    Exists,
    /// The operation was cancelled.
    Cancelled,
    /// The sandboxed worker of a foreign archive failed.
    Sandbox,
    /// Another input/output failure.
    Io,
    /// A failure inside the engine.
    Internal,
}

impl Class {
    /// Every class, in declaration order (the table `docs/API.md` lists).
    pub const ALL: [Class; 15] = [
        Class::Usage,
        Class::NotFound,
        Class::AccessDenied,
        Class::DiskFull,
        Class::PasswordRequired,
        Class::WrongPassword,
        Class::Damaged,
        Class::Unsupported,
        Class::TooLarge,
        Class::Unsafe,
        Class::Exists,
        Class::Cancelled,
        Class::Sandbox,
        Class::Io,
        Class::Internal,
    ];

    /// The class's name as `docs/API.md` writes it.
    pub fn name(self) -> &'static str {
        match self {
            Class::Usage => "Usage",
            Class::NotFound => "NotFound",
            Class::AccessDenied => "AccessDenied",
            Class::DiskFull => "DiskFull",
            Class::PasswordRequired => "PasswordRequired",
            Class::WrongPassword => "WrongPassword",
            Class::Damaged => "Damaged",
            Class::Unsupported => "Unsupported",
            Class::TooLarge => "TooLarge",
            Class::Unsafe => "Unsafe",
            Class::Exists => "Exists",
            Class::Cancelled => "Cancelled",
            Class::Sandbox => "Sandbox",
            Class::Io => "Io",
            Class::Internal => "Internal",
        }
    }

    /// The user-facing message of the class (a front end shows it with the error's own
    /// message as the detail).
    pub fn message(self) -> &'static str {
        match self {
            Class::Usage => "The request is not valid.",
            Class::NotFound => "A file or entry was not found.",
            Class::AccessDenied => "Access was denied.",
            Class::DiskFull => "The disk is full.",
            Class::PasswordRequired => "This archive needs a password.",
            Class::WrongPassword => "The password is wrong.",
            Class::Damaged => "The archive is damaged.",
            Class::Unsupported => "This is not supported (see the detail): an archive feature this version does not have, or an operation this archive does not allow.",
            Class::TooLarge => "This exceeds a limit.",
            Class::Unsafe => "A path was refused as unsafe.",
            Class::Exists => "Something is in the way.",
            Class::Cancelled => "The operation was cancelled.",
            Class::Sandbox => "The sandboxed reader failed.",
            Class::Io => "A read or write failed.",
            Class::Internal => "Something went wrong inside LitePack.",
        }
    }

    /// The exit code of the command line: 1 for [`Class::Usage`], 2 for every other class.
    pub fn exit_code(self) -> i32 {
        if self == Class::Usage {
            EXIT_USAGE
        } else {
            EXIT_FAILED
        }
    }
}

/// One entry of a partial operation that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryError {
    /// The archive path of the entry.
    pub path: String,
    /// The error's variant name.
    pub kind: String,
    /// The error's message.
    pub message: String,
}

/// The engine error an [`Error`] came from, kept for callers that need the typed value.
#[derive(Debug)]
enum Source {
    Core(CoreError),
    Format(FormatError),
    None,
}

/// A failed API call.
#[derive(Debug)]
pub struct Error {
    /// The class that keys the user-facing message.
    pub class: Class,
    /// D-77's `kind`: the engine error's variant name (`Usage` for a request error).
    pub kind: String,
    /// The engine's message (its Display).
    pub message: String,
    /// The path the error names, else the archive the operation worked on.
    pub path: Option<String>,
    /// Per-entry errors of a partial operation (empty otherwise).
    pub entries: Vec<EntryError>,
    source: Box<Source>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl Error {
    /// A request error (exit 1).
    pub fn usage(message: impl Into<String>) -> Self {
        Error {
            class: Class::Usage,
            kind: "Usage".to_string(),
            message: message.into(),
            path: None,
            entries: Vec::new(),
            source: Box::new(Source::None),
        }
    }

    /// An error of a class and kind of the caller's.
    pub fn new(class: Class, kind: &str, message: impl Into<String>, path: Option<&Path>) -> Self {
        Error {
            class,
            kind: kind.to_string(),
            message: message.into(),
            path: path.map(|p| p.display().to_string()),
            entries: Vec::new(),
            source: Box::new(Source::None),
        }
    }

    /// A pipeline error; `archive` is the path when the error names none itself.
    pub fn core(e: CoreError, archive: Option<&Path>) -> Self {
        Error {
            class: core_class(&e),
            kind: core_kind(&e).to_string(),
            message: e.to_string(),
            path: core_path(&e).or_else(|| archive.map(|p| p.display().to_string())),
            entries: Vec::new(),
            source: Box::new(Source::Core(e)),
        }
    }

    /// A format error; `archive` is the path when the error names none itself.
    pub fn format(e: FormatError, archive: Option<&Path>) -> Self {
        Error {
            class: format_class(&e),
            kind: format_kind(&e).to_string(),
            message: e.to_string(),
            path: format_path(&e).or_else(|| archive.map(|p| p.display().to_string())),
            entries: Vec::new(),
            source: Box::new(Source::Format(e)),
        }
    }

    /// The operation was cancelled (`CoreError::Cancelled`).
    pub fn cancelled() -> Self {
        Error::core(CoreError::Cancelled, None)
    }

    /// The exit code of the command line ([`Class::exit_code`]).
    pub fn exit_code(&self) -> i32 {
        self.class.exit_code()
    }

    /// True when the error came from the format crate (a `FormatError`, bare or inside a
    /// `CoreError::Format`).
    pub fn is_format(&self) -> bool {
        matches!(
            *self.source,
            Source::Format(_) | Source::Core(CoreError::Format(_))
        )
    }

    /// The figures of an envelope refusal (what is needed, what this machine allows).
    pub fn refusal(&self) -> Option<Refusal> {
        match &*self.source {
            Source::Format(FormatError::Refused(r))
            | Source::Core(CoreError::Format(FormatError::Refused(r))) => Some(r.clone()),
            // A foreign archive whose listing outgrew the memory budget carries both figures
            // (`value` is what the listing needed so far, a lower bound: "Allow" may refuse
            // again with a larger figure).
            Source::Core(CoreError::Foreign(ForeignError::LimitExceeded {
                flag: "--memory",
                value,
                max,
                ..
            })) => Some(Refusal {
                field: "--memory",
                needed: *value,
                allowed: *max,
            }),
            _ => None,
        }
    }

    /// The archive path of the entry a failure names (a block that failed to decode, a name that
    /// cannot be extracted), not the archive's file: what a front end lists as the failing
    /// entry when the operation reports no entry list of its own.
    pub fn entry_path(&self) -> Option<&str> {
        match &*self.source {
            Source::Core(
                CoreError::Decode { path, .. }
                | CoreError::DuplicatePath { path }
                | CoreError::UnportableName { path, .. },
            ) => Some(path),
            _ => None,
        }
    }

    /// The format error this came from, if any.
    pub fn format_error(&self) -> Option<&FormatError> {
        match &*self.source {
            Source::Format(e) | Source::Core(CoreError::Format(e)) => Some(e),
            _ => None,
        }
    }

    /// The pipeline error this came from, if any.
    pub fn core_error(&self) -> Option<&CoreError> {
        match &*self.source {
            Source::Core(e) => Some(e),
            _ => None,
        }
    }
}

/// The class of an I/O error by its kind.
fn io_class(e: &std::io::Error) -> Class {
    match e.kind() {
        std::io::ErrorKind::NotFound => Class::NotFound,
        std::io::ErrorKind::PermissionDenied => Class::AccessDenied,
        std::io::ErrorKind::AlreadyExists => Class::Exists,
        std::io::ErrorKind::StorageFull => Class::DiskFull,
        _ => Class::Io,
    }
}

/// The class of a pipeline error. Every variant placed by name.
pub fn core_class(e: &CoreError) -> Class {
    match e {
        CoreError::InvalidOption(_) => Class::Usage,
        CoreError::Format(inner) => format_class(inner),
        CoreError::Io { source, .. } => io_class(source),
        CoreError::UnportableName { .. }
        | CoreError::SpecialFile { .. }
        | CoreError::CaseCollision { .. } => Class::Unsafe,
        CoreError::DuplicatePath { .. }
        | CoreError::InTheWay { .. }
        | CoreError::NotADirectory { .. } => Class::Exists,
        CoreError::ChangedWhileReading { .. } => Class::Io,
        // An append cut back after a failure: the archive is left at its previous generation
        // only if the cut held, so the outcome is reported as damage to look at.
        CoreError::AppendLeftCut { .. } | CoreError::Decode { .. } => Class::Damaged,
        // `Extract` carries no inner cause (a decode worker stopped without a result: its
        // own error, disk full or damage, is reported instead when there is one).
        CoreError::Extract(_) => Class::Internal,
        CoreError::Export { .. } => Class::Io,
        CoreError::NoMatch { .. } => Class::NotFound,
        CoreError::Cancelled => Class::Cancelled,
        CoreError::Foreign(inner) => foreign_class(inner),
        CoreError::Worker(inner) => worker_class(inner),
    }
}

/// The class of a foreign-archive error. Every variant placed by name.
pub fn foreign_class(e: &ForeignError) -> Class {
    match e {
        ForeignError::UnknownFormat | ForeignError::Unsupported { .. } => Class::Unsupported,
        ForeignError::Corrupt { .. } | ForeignError::SizeMismatch { .. } => Class::Damaged,
        ForeignError::OverlappingEntries { .. } => Class::Unsafe,
        ForeignError::LimitExceeded { .. } => Class::TooLarge,
        ForeignError::NoSpace { .. } => Class::DiskFull,
        ForeignError::ArchiveChanged => Class::Io,
    }
}

/// The class of a worker error. Every variant placed by name.
pub fn worker_class(e: &WorkerError) -> Class {
    match e {
        WorkerError::Denied { .. } => Class::AccessDenied,
        WorkerError::MemoryLimit { .. } | WorkerError::CpuLimit { .. } => Class::TooLarge,
        WorkerError::Spawn { .. }
        | WorkerError::Sandbox { .. }
        | WorkerError::Crashed { .. }
        | WorkerError::Panicked { .. }
        | WorkerError::Timeout { .. }
        | WorkerError::Protocol { .. }
        | WorkerError::Pipe { .. }
        | WorkerError::BuildMismatch { .. } => Class::Sandbox,
    }
}

/// The class of a format error. Every variant placed by name.
pub fn format_class(e: &FormatError) -> Class {
    match e {
        FormatError::UnsupportedMajor { .. }
        | FormatError::UnknownMustUnderstand { .. }
        | FormatError::UnsupportedEntryKind { .. }
        | FormatError::UnknownPrimitive { .. }
        | FormatError::UnimplementedPrimitive { .. }
        | FormatError::UnknownRecordKind { .. } => Class::Unsupported,
        FormatError::BadMagic
        | FormatError::ReservedHeaderBits { .. }
        | FormatError::BadHeaderFlags { .. }
        | FormatError::ReservedFrameBits { .. }
        | FormatError::InvalidKind
        | FormatError::NonCanonicalVarint
        | FormatError::VarintTooLong
        | FormatError::Truncated { .. }
        | FormatError::HashMismatch { .. }
        | FormatError::UnsortedEntries { .. }
        | FormatError::DuplicateEntry { .. }
        | FormatError::ReservedEntryBits { .. }
        | FormatError::InvalidPath { .. }
        | FormatError::InconsistentEntry { .. }
        | FormatError::TrailingBytes { .. }
        | FormatError::ChunkMismatch { .. }
        | FormatError::ChunkIndexOutOfRange { .. }
        | FormatError::FileSizeMismatch { .. }
        | FormatError::RangeOutOfFile { .. }
        | FormatError::MerkleRootMismatch
        | FormatError::IndexHashMismatch
        | FormatError::ArchiveIdMismatch
        | FormatError::BlockLengthMismatch { .. }
        | FormatError::BlockCoverage { .. }
        | FormatError::BlockOutOfRange { .. }
        | FormatError::WrongFrameKind { .. }
        | FormatError::NoTrailer
        | FormatError::BadGenerationTable { .. }
        | FormatError::BadTrailer { .. }
        | FormatError::GenerationMismatch { .. }
        | FormatError::BadFrameLocation { .. }
        | FormatError::EnvelopeMismatch { .. }
        | FormatError::BadGraph { .. }
        | FormatError::BadParams { .. }
        | FormatError::ReservedRecordBits { .. }
        | FormatError::RecordHashMismatch { .. }
        | FormatError::RecordOutOfRange { .. }
        | FormatError::BadRecord { .. }
        | FormatError::BadPriorList { .. }
        | FormatError::ZstdError { .. }
        | FormatError::LzmaError { .. }
        | FormatError::BwtError { .. }
        | FormatError::BadChunk { .. }
        | FormatError::BadRecovery { .. }
        | FormatError::Unrepairable { .. }
        | FormatError::DamageFound { .. }
        | FormatError::RecoveryError { .. }
        | FormatError::AuthenticationFailed { .. }
        | FormatError::UnexpectedSealedFrame { .. }
        | FormatError::UnsealedFrame { .. }
        | FormatError::UnexpectedKeySlot
        | FormatError::MissingKeySlot
        | FormatError::BadKeySlot { .. }
        | FormatError::BadArgon2 { .. }
        | FormatError::EntryTableMismatch => Class::Damaged,
        FormatError::NoSuchGeneration { .. }
        | FormatError::MissingPrior { .. }
        | FormatError::UnlistedPrior { .. } => Class::NotFound,
        FormatError::PayloadTooLarge { .. }
        | FormatError::Refused(_)
        | FormatError::WindowTooLarge { .. } => Class::TooLarge,
        FormatError::BadOptions { .. } | FormatError::SealFailed { .. } => Class::Internal,
        FormatError::UnsafePath { .. } | FormatError::SymlinkRefused { .. } => Class::Unsafe,
        FormatError::PasswordRequired | FormatError::AppendNeedsCredentials => {
            Class::PasswordRequired
        }
        FormatError::WrongKey => Class::WrongPassword,
        FormatError::Io(e) => io_class(e),
    }
}

/// The exit code of a pipeline error: 1 for an option the engine refused (a usage error
/// found after parsing), 2 for everything else (D-48), by its [`Class`].
pub fn core_exit(e: &CoreError) -> i32 {
    core_class(e).exit_code()
}

/// The exit code of a format error: always 2 (an archive, a key or an I/O failure, never
/// the command line), by its [`Class`].
pub fn format_exit(e: &FormatError) -> i32 {
    format_class(e).exit_code()
}

/// The variant name of a pipeline error (the inner name for the transparent `Format`).
pub fn core_kind(e: &CoreError) -> &'static str {
    match e {
        CoreError::Format(inner) => format_kind(inner),
        CoreError::Io { .. } => "Io",
        CoreError::UnportableName { .. } => "UnportableName",
        CoreError::SpecialFile { .. } => "SpecialFile",
        CoreError::DuplicatePath { .. } => "DuplicatePath",
        CoreError::ChangedWhileReading { .. } => "ChangedWhileReading",
        CoreError::InvalidOption(_) => "InvalidOption",
        CoreError::Decode { .. } => "Decode",
        CoreError::Extract(_) => "Extract",
        CoreError::Export { .. } => "Export",
        CoreError::InTheWay { .. } => "InTheWay",
        CoreError::CaseCollision { .. } => "CaseCollision",
        CoreError::NoMatch { .. } => "NoMatch",
        CoreError::NotADirectory { .. } => "NotADirectory",
        CoreError::Cancelled => "Cancelled",
        CoreError::AppendLeftCut { .. } => "AppendLeftCut",
        CoreError::Foreign(inner) => foreign_kind(inner),
        CoreError::Worker(inner) => worker_kind(inner),
    }
}

/// The variant name of a foreign-archive error (the inner name, as for `Format`).
pub fn foreign_kind(e: &ForeignError) -> &'static str {
    match e {
        ForeignError::UnknownFormat => "UnknownFormat",
        ForeignError::Unsupported { .. } => "Unsupported",
        ForeignError::Corrupt { .. } => "Corrupt",
        ForeignError::SizeMismatch { .. } => "SizeMismatch",
        ForeignError::OverlappingEntries { .. } => "OverlappingEntries",
        ForeignError::LimitExceeded { .. } => "LimitExceeded",
        ForeignError::NoSpace { .. } => "NoSpace",
        ForeignError::ArchiveChanged => "ArchiveChanged",
    }
}

/// The variant name of a worker error (the inner name, as for `Format`).
pub fn worker_kind(e: &WorkerError) -> &'static str {
    match e {
        WorkerError::Spawn { .. } => "Spawn",
        WorkerError::Sandbox { .. } => "Sandbox",
        WorkerError::Denied { .. } => "Denied",
        WorkerError::Crashed { .. } => "Crashed",
        WorkerError::Panicked { .. } => "Panicked",
        WorkerError::MemoryLimit { .. } => "MemoryLimit",
        WorkerError::CpuLimit { .. } => "CpuLimit",
        WorkerError::Timeout { .. } => "Timeout",
        WorkerError::Protocol { .. } => "Protocol",
        WorkerError::Pipe { .. } => "Pipe",
        WorkerError::BuildMismatch { .. } => "BuildMismatch",
    }
}

/// The path a pipeline error names, when it names one.
pub fn core_path(e: &CoreError) -> Option<String> {
    match e {
        CoreError::Format(inner) => format_path(inner),
        CoreError::Io { path, .. }
        | CoreError::SpecialFile { path }
        | CoreError::ChangedWhileReading { path }
        | CoreError::NotADirectory { path }
        | CoreError::InTheWay { path, .. }
        | CoreError::Export { path, .. }
        | CoreError::AppendLeftCut { path, .. } => Some(path.display().to_string()),
        CoreError::UnportableName { path, .. }
        | CoreError::DuplicatePath { path }
        | CoreError::CaseCollision { first: path, .. }
        | CoreError::Decode { path, .. } => Some(path.clone()),
        CoreError::InvalidOption(_)
        | CoreError::Extract(_)
        | CoreError::NoMatch { .. }
        | CoreError::Cancelled
        | CoreError::Foreign(_)
        | CoreError::Worker(_) => None,
    }
}

/// The variant name of a format error.
pub fn format_kind(e: &FormatError) -> &'static str {
    match e {
        FormatError::BadMagic => "BadMagic",
        FormatError::UnsupportedMajor { .. } => "UnsupportedMajor",
        FormatError::ReservedHeaderBits { .. } => "ReservedHeaderBits",
        FormatError::BadHeaderFlags { .. } => "BadHeaderFlags",
        FormatError::ReservedFrameBits { .. } => "ReservedFrameBits",
        FormatError::InvalidKind => "InvalidKind",
        FormatError::NonCanonicalVarint => "NonCanonicalVarint",
        FormatError::VarintTooLong => "VarintTooLong",
        FormatError::Truncated { .. } => "Truncated",
        FormatError::HashMismatch { .. } => "HashMismatch",
        FormatError::UnknownMustUnderstand { .. } => "UnknownMustUnderstand",
        FormatError::PayloadTooLarge { .. } => "PayloadTooLarge",
        FormatError::UnsortedEntries { .. } => "UnsortedEntries",
        FormatError::DuplicateEntry { .. } => "DuplicateEntry",
        FormatError::UnsupportedEntryKind { .. } => "UnsupportedEntryKind",
        FormatError::ReservedEntryBits { .. } => "ReservedEntryBits",
        FormatError::InvalidPath { .. } => "InvalidPath",
        FormatError::InconsistentEntry { .. } => "InconsistentEntry",
        FormatError::TrailingBytes { .. } => "TrailingBytes",
        FormatError::ChunkMismatch { .. } => "ChunkMismatch",
        FormatError::ChunkIndexOutOfRange { .. } => "ChunkIndexOutOfRange",
        FormatError::FileSizeMismatch { .. } => "FileSizeMismatch",
        FormatError::RangeOutOfFile { .. } => "RangeOutOfFile",
        FormatError::MerkleRootMismatch => "MerkleRootMismatch",
        FormatError::IndexHashMismatch => "IndexHashMismatch",
        FormatError::ArchiveIdMismatch => "ArchiveIdMismatch",
        FormatError::BlockLengthMismatch { .. } => "BlockLengthMismatch",
        FormatError::BlockCoverage { .. } => "BlockCoverage",
        FormatError::BlockOutOfRange { .. } => "BlockOutOfRange",
        FormatError::WrongFrameKind { .. } => "WrongFrameKind",
        FormatError::NoTrailer => "NoTrailer",
        FormatError::BadGenerationTable { .. } => "BadGenerationTable",
        FormatError::BadTrailer { .. } => "BadTrailer",
        FormatError::NoSuchGeneration { .. } => "NoSuchGeneration",
        FormatError::AppendNeedsCredentials => "AppendNeedsCredentials",
        FormatError::GenerationMismatch { .. } => "GenerationMismatch",
        FormatError::BadFrameLocation { .. } => "BadFrameLocation",
        FormatError::Refused(_) => "Refused",
        FormatError::EnvelopeMismatch { .. } => "EnvelopeMismatch",
        FormatError::UnknownPrimitive { .. } => "UnknownPrimitive",
        FormatError::UnimplementedPrimitive { .. } => "UnimplementedPrimitive",
        FormatError::BadGraph { .. } => "BadGraph",
        FormatError::BadParams { .. } => "BadParams",
        FormatError::UnknownRecordKind { .. } => "UnknownRecordKind",
        FormatError::ReservedRecordBits { .. } => "ReservedRecordBits",
        FormatError::RecordHashMismatch { .. } => "RecordHashMismatch",
        FormatError::RecordOutOfRange { .. } => "RecordOutOfRange",
        FormatError::BadRecord { .. } => "BadRecord",
        FormatError::MissingPrior { .. } => "MissingPrior",
        FormatError::UnlistedPrior { .. } => "UnlistedPrior",
        FormatError::BadPriorList { .. } => "BadPriorList",
        FormatError::WindowTooLarge { .. } => "WindowTooLarge",
        FormatError::ZstdError { .. } => "ZstdError",
        FormatError::LzmaError { .. } => "LzmaError",
        FormatError::BwtError { .. } => "BwtError",
        FormatError::BadOptions { .. } => "BadOptions",
        FormatError::BadChunk { .. } => "BadChunk",
        FormatError::BadRecovery { .. } => "BadRecovery",
        FormatError::Unrepairable { .. } => "Unrepairable",
        FormatError::DamageFound { .. } => "DamageFound",
        FormatError::RecoveryError { .. } => "RecoveryError",
        FormatError::UnsafePath { .. } => "UnsafePath",
        FormatError::SymlinkRefused { .. } => "SymlinkRefused",
        FormatError::PasswordRequired => "PasswordRequired",
        FormatError::WrongKey => "WrongKey",
        FormatError::AuthenticationFailed { .. } => "AuthenticationFailed",
        FormatError::UnexpectedSealedFrame { .. } => "UnexpectedSealedFrame",
        FormatError::UnsealedFrame { .. } => "UnsealedFrame",
        FormatError::UnexpectedKeySlot => "UnexpectedKeySlot",
        FormatError::MissingKeySlot => "MissingKeySlot",
        FormatError::BadKeySlot { .. } => "BadKeySlot",
        FormatError::BadArgon2 { .. } => "BadArgon2",
        FormatError::EntryTableMismatch => "EntryTableMismatch",
        FormatError::SealFailed { .. } => "SealFailed",
        FormatError::Io(_) => "Io",
    }
}

/// The archive path a format error names, when it names one.
pub fn format_path(e: &FormatError) -> Option<String> {
    match e {
        FormatError::DuplicateEntry { path }
        | FormatError::UnsafePath { path, .. }
        | FormatError::SymlinkRefused { path } => Some(path.clone()),
        _ => None,
    }
}
