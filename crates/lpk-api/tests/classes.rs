//! E5-1: the exhaustive classification table. One value per variant of `FormatError`,
//! `CoreError`, `ForeignError` and `WorkerError` (the rows mirror `lpk-cli`'s `exit_codes.rs`)
//! with the `Class` it must map to, so a new variant is classified on purpose; the class also
//! decides the exit code (D-48: `Usage` 1, every other class 2) and D-77's kind is kept.
#![allow(clippy::unwrap_used)]

use std::io::Error;
use std::path::PathBuf;

use lpk_api::error::{core_class, core_exit, format_class, format_exit};
use lpk_api::Class;
use lpk_core::{CoreError, ForeignError, WorkerError};
use lpk_format::{FormatError, Refusal};

fn io() -> Error {
    Error::other("disk")
}

/// Every `FormatError` variant with the exit code it maps to and its kind name.
fn format_table() -> Vec<(FormatError, Class, &'static str)> {
    use FormatError::*;
    vec![
        (BadMagic, Class::Damaged, "BadMagic"),
        (
            UnsupportedMajor { found: 2 },
            Class::Unsupported,
            "UnsupportedMajor",
        ),
        (
            ReservedHeaderBits { bits: 8 },
            Class::Damaged,
            "ReservedHeaderBits",
        ),
        (BadHeaderFlags { bits: 2 }, Class::Damaged, "BadHeaderFlags"),
        (
            ReservedFrameBits { bits: 8 },
            Class::Damaged,
            "ReservedFrameBits",
        ),
        (InvalidKind, Class::Damaged, "InvalidKind"),
        (NonCanonicalVarint, Class::Damaged, "NonCanonicalVarint"),
        (VarintTooLong, Class::Damaged, "VarintTooLong"),
        (Truncated { what: "header" }, Class::Damaged, "Truncated"),
        (HashMismatch { kind: 2 }, Class::Damaged, "HashMismatch"),
        (
            UnknownMustUnderstand { kind: 9 },
            Class::Unsupported,
            "UnknownMustUnderstand",
        ),
        (
            PayloadTooLarge { len: 2, max: 1 },
            Class::TooLarge,
            "PayloadTooLarge",
        ),
        (
            UnsortedEntries { index: 1 },
            Class::Damaged,
            "UnsortedEntries",
        ),
        (
            DuplicateEntry { path: "a".into() },
            Class::Damaged,
            "DuplicateEntry",
        ),
        (
            UnsupportedEntryKind { kind: 3, index: 0 },
            Class::Unsupported,
            "UnsupportedEntryKind",
        ),
        (
            ReservedEntryBits { bits: 16, index: 0 },
            Class::Damaged,
            "ReservedEntryBits",
        ),
        (
            InvalidPath {
                index: 0,
                reason: "empty",
            },
            Class::Damaged,
            "InvalidPath",
        ),
        (
            InconsistentEntry {
                index: 0,
                reason: "x",
            },
            Class::Damaged,
            "InconsistentEntry",
        ),
        (
            TrailingBytes { what: "index" },
            Class::Damaged,
            "TrailingBytes",
        ),
        (ChunkMismatch { chunk: 0 }, Class::Damaged, "ChunkMismatch"),
        (
            ChunkIndexOutOfRange { chunk: 1, len: 1 },
            Class::Damaged,
            "ChunkIndexOutOfRange",
        ),
        (
            FileSizeMismatch {
                expected: 1,
                found: 2,
            },
            Class::Damaged,
            "FileSizeMismatch",
        ),
        (
            RangeOutOfFile {
                offset: 1,
                len: 1,
                file_len: 1,
            },
            Class::Damaged,
            "RangeOutOfFile",
        ),
        (MerkleRootMismatch, Class::Damaged, "MerkleRootMismatch"),
        (IndexHashMismatch, Class::Damaged, "IndexHashMismatch"),
        (ArchiveIdMismatch, Class::Damaged, "ArchiveIdMismatch"),
        (
            BlockLengthMismatch { block: 0 },
            Class::Damaged,
            "BlockLengthMismatch",
        ),
        (BlockCoverage { block: 0 }, Class::Damaged, "BlockCoverage"),
        (
            BlockOutOfRange { block: 0 },
            Class::Damaged,
            "BlockOutOfRange",
        ),
        (
            WrongFrameKind {
                expected: 1,
                found: 2,
            },
            Class::Damaged,
            "WrongFrameKind",
        ),
        (NoTrailer, Class::Damaged, "NoTrailer"),
        (
            BadGenerationTable { reason: "x" },
            Class::Damaged,
            "BadGenerationTable",
        ),
        (BadTrailer { reason: "x" }, Class::Damaged, "BadTrailer"),
        (
            NoSuchGeneration {
                requested: 3,
                latest: 1,
            },
            Class::NotFound,
            "NoSuchGeneration",
        ),
        (
            AppendNeedsCredentials,
            Class::PasswordRequired,
            "AppendNeedsCredentials",
        ),
        (
            GenerationMismatch {
                expected: 1,
                found: 0,
            },
            Class::Damaged,
            "GenerationMismatch",
        ),
        (
            BadFrameLocation { what: "index" },
            Class::Damaged,
            "BadFrameLocation",
        ),
        (
            Refused(Refusal {
                field: "max_window",
                needed: 2,
                allowed: 1,
            }),
            Class::TooLarge,
            "Refused",
        ),
        (
            EnvelopeMismatch {
                field: "max_window",
            },
            Class::Damaged,
            "EnvelopeMismatch",
        ),
        (
            UnknownPrimitive { id: 0xffff },
            Class::Unsupported,
            "UnknownPrimitive",
        ),
        (
            UnimplementedPrimitive { id: 3 },
            Class::Unsupported,
            "UnimplementedPrimitive",
        ),
        (BadGraph { reason: "x" }, Class::Damaged, "BadGraph"),
        (
            BadParams { id: 1, reason: "x" },
            Class::Damaged,
            "BadParams",
        ),
        (
            UnknownRecordKind { kind: 9, record: 0 },
            Class::Unsupported,
            "UnknownRecordKind",
        ),
        (
            ReservedRecordBits { bits: 8, record: 0 },
            Class::Damaged,
            "ReservedRecordBits",
        ),
        (
            RecordHashMismatch { record: 0 },
            Class::Damaged,
            "RecordHashMismatch",
        ),
        (
            RecordOutOfRange {
                record: 1,
                count: 1,
            },
            Class::Damaged,
            "RecordOutOfRange",
        ),
        (
            BadRecord {
                record: 0,
                reason: "x",
            },
            Class::Damaged,
            "BadRecord",
        ),
        (
            MissingPrior { id: [0; 32] },
            Class::NotFound,
            "MissingPrior",
        ),
        (
            UnlistedPrior { id: [0; 32] },
            Class::NotFound,
            "UnlistedPrior",
        ),
        (BadPriorList { reason: "x" }, Class::Damaged, "BadPriorList"),
        (
            WindowTooLarge {
                needed: 2,
                allowed: 1,
            },
            Class::TooLarge,
            "WindowTooLarge",
        ),
        (
            ZstdError { reason: "x".into() },
            Class::Damaged,
            "ZstdError",
        ),
        (
            LzmaError { reason: "x".into() },
            Class::Damaged,
            "LzmaError",
        ),
        (BwtError { reason: "inverse" }, Class::Damaged, "BwtError"),
        (BadOptions { reason: "x" }, Class::Internal, "BadOptions"),
        (BadChunk { reason: "x" }, Class::Damaged, "BadChunk"),
        (BadRecovery { reason: "x" }, Class::Damaged, "BadRecovery"),
        (
            Unrepairable {
                frame: 0,
                damaged: 2,
                capacity: 1,
            },
            Class::Damaged,
            "Unrepairable",
        ),
        (
            DamageFound {
                damaged: 1,
                unusable: 0,
            },
            Class::Damaged,
            "DamageFound",
        ),
        (
            RecoveryError { reason: "x".into() },
            Class::Damaged,
            "RecoveryError",
        ),
        (
            UnsafePath {
                path: "..".into(),
                reason: "dot-dot",
            },
            Class::Unsafe,
            "UnsafePath",
        ),
        (
            SymlinkRefused { path: "l".into() },
            Class::Unsafe,
            "SymlinkRefused",
        ),
        (
            PasswordRequired,
            Class::PasswordRequired,
            "PasswordRequired",
        ),
        (WrongKey, Class::WrongPassword, "WrongKey"),
        (
            AuthenticationFailed {
                kind: 2,
                sequence: 1,
            },
            Class::Damaged,
            "AuthenticationFailed",
        ),
        (
            UnexpectedSealedFrame { kind: 1 },
            Class::Damaged,
            "UnexpectedSealedFrame",
        ),
        (UnsealedFrame { kind: 2 }, Class::Damaged, "UnsealedFrame"),
        (UnexpectedKeySlot, Class::Damaged, "UnexpectedKeySlot"),
        (MissingKeySlot, Class::Damaged, "MissingKeySlot"),
        (BadKeySlot { reason: "x" }, Class::Damaged, "BadKeySlot"),
        (BadArgon2 { reason: "x" }, Class::Damaged, "BadArgon2"),
        (EntryTableMismatch, Class::Damaged, "EntryTableMismatch"),
        (
            SealFailed {
                kind: 2,
                sequence: 1,
            },
            Class::Internal,
            "SealFailed",
        ),
        (Io(io()), Class::Io, "Io"),
    ]
}

/// Every `CoreError` variant with its exit code and kind name (`Format` reports the inner
/// variant's name, since the wrapper adds nothing).
fn core_table() -> Vec<(CoreError, Class, &'static str)> {
    use CoreError::*;
    vec![
        (Format(FormatError::BadMagic), Class::Damaged, "BadMagic"),
        (
            Io {
                path: PathBuf::from("a"),
                source: io(),
            },
            Class::Io,
            "Io",
        ),
        (
            UnportableName {
                path: "a:b".into(),
                reason: "colon".into(),
            },
            Class::Unsafe,
            "UnportableName",
        ),
        (
            SpecialFile {
                path: PathBuf::from("fifo"),
            },
            Class::Unsafe,
            "SpecialFile",
        ),
        (
            DuplicatePath { path: "a".into() },
            Class::Exists,
            "DuplicatePath",
        ),
        (
            ChangedWhileReading {
                path: PathBuf::from("a"),
            },
            Class::Io,
            "ChangedWhileReading",
        ),
        (
            InvalidOption("level 99".into()),
            Class::Usage,
            "InvalidOption",
        ),
        (
            Decode {
                block: 3,
                path: "a".into(),
                source: FormatError::ChunkMismatch { chunk: 0 },
            },
            Class::Damaged,
            "Decode",
        ),
        (Extract("worker stopped"), Class::Internal, "Extract"),
        // E3-3: a run cancelled through its token (Ctrl-C) is a failed operation, `cancelled`.
        (Cancelled, Class::Cancelled, "Cancelled"),
        (
            Export {
                path: PathBuf::from("out.zip"),
                entry: "a/b".into(),
                message: "zip error".into(),
            },
            Class::Io,
            "Export",
        ),
        (
            InTheWay {
                path: "out/a".into(),
                found: "directory",
                wanted: "file",
            },
            Class::Exists,
            "InTheWay",
        ),
        (
            CaseCollision {
                first: "A".into(),
                second: "a".into(),
            },
            Class::Unsafe,
            "CaseCollision",
        ),
        (
            NotADirectory {
                path: PathBuf::from("a"),
            },
            Class::Exists,
            "NotADirectory",
        ),
        // E3-4: a selection pattern matching no entry is a refused operation, naming the
        // pattern in its message (no path of its own: the archive is reported).
        (
            NoMatch {
                pattern: "docs/*".into(),
            },
            Class::NotFound,
            "NoMatch",
        ),
        (
            AppendLeftCut {
                path: PathBuf::from("a.lpk"),
                cause: Box::new(Extract("x")),
                kept: 10,
                truncation: io(),
            },
            Class::Damaged,
            "AppendLeftCut",
        ),
        (
            Foreign(ForeignError::UnknownFormat),
            Class::Unsupported,
            "UnknownFormat",
        ),
        (
            Worker(WorkerError::Pipe {
                stderr: String::new(),
            }),
            Class::Sandbox,
            "Pipe",
        ),
    ]
}

#[test]
fn every_format_error_variant_has_its_class() {
    let table = format_table();
    assert_eq!(table.len(), 75, "one row per FormatError variant");
    for (e, class, kind) in table {
        assert_eq!(format_class(&e), class, "{e}");
        assert_eq!(format_exit(&e), class.exit_code(), "{e}");
        let a = lpk_api::Error::format(e, None);
        assert_eq!((a.class, a.kind.as_str()), (class, kind));
    }
}

#[test]
fn every_core_error_variant_has_its_class() {
    let table = core_table();
    assert_eq!(table.len(), 18, "one row per CoreError variant");
    for (e, class, kind) in table {
        assert_eq!(core_class(&e), class, "{e}");
        assert_eq!(core_exit(&e), class.exit_code(), "{e}");
        let a = lpk_api::Error::core(e, None);
        assert_eq!((a.class, a.kind.as_str()), (class, kind));
    }
}

fn foreign_and_worker() -> Vec<(CoreError, Class, &'static str)> {
    let s = || String::from("x");
    let foreign = [
        (
            ForeignError::UnknownFormat,
            Class::Unsupported,
            "UnknownFormat",
        ),
        (
            ForeignError::Unsupported { message: s() },
            Class::Unsupported,
            "Unsupported",
        ),
        (
            ForeignError::Corrupt { message: s() },
            Class::Damaged,
            "Corrupt",
        ),
        (
            ForeignError::SizeMismatch {
                path: s(),
                declared: 1,
                received: 2,
            },
            Class::Damaged,
            "SizeMismatch",
        ),
        (
            ForeignError::OverlappingEntries {
                first: s(),
                second: s(),
            },
            Class::Unsafe,
            "OverlappingEntries",
        ),
        (
            ForeignError::LimitExceeded {
                limit: "entries",
                value: 2,
                max: 1,
                flag: "--max-entries",
            },
            Class::TooLarge,
            "LimitExceeded",
        ),
        (
            ForeignError::NoSpace {
                needed: 2,
                available: 1,
            },
            Class::DiskFull,
            "NoSpace",
        ),
        (ForeignError::ArchiveChanged, Class::Io, "ArchiveChanged"),
    ];
    let worker = [
        (WorkerError::Spawn { message: s() }, Class::Sandbox, "Spawn"),
        (
            WorkerError::Sandbox { message: s() },
            Class::Sandbox,
            "Sandbox",
        ),
        (
            WorkerError::Denied { message: s() },
            Class::AccessDenied,
            "Denied",
        ),
        (
            WorkerError::Crashed {
                code: 1,
                stderr: s(),
            },
            Class::Sandbox,
            "Crashed",
        ),
        (
            WorkerError::Panicked { message: s() },
            Class::Sandbox,
            "Panicked",
        ),
        (
            WorkerError::MemoryLimit { limit: 1 },
            Class::TooLarge,
            "MemoryLimit",
        ),
        (
            WorkerError::CpuLimit { seconds: 1 },
            Class::TooLarge,
            "CpuLimit",
        ),
        (
            WorkerError::Timeout {
                seconds: 1.0,
                waiting_for: "x",
            },
            Class::Sandbox,
            "Timeout",
        ),
        (
            WorkerError::Protocol { message: s() },
            Class::Sandbox,
            "Protocol",
        ),
        (WorkerError::Pipe { stderr: s() }, Class::Sandbox, "Pipe"),
        (
            WorkerError::BuildMismatch {
                host: s(),
                worker: s(),
            },
            Class::Sandbox,
            "BuildMismatch",
        ),
    ];
    foreign
        .into_iter()
        .map(|(e, c, k)| (CoreError::Foreign(e), c, k))
        .chain(
            worker
                .into_iter()
                .map(|(e, c, k)| (CoreError::Worker(e), c, k)),
        )
        .collect()
}

#[test]
fn every_foreign_and_worker_variant_has_its_class() {
    let all = foreign_and_worker();
    assert_eq!(all.len(), 19);
    for (e, class, kind) in all {
        assert_eq!(core_class(&e), class, "{e}");
        let a = lpk_api::Error::core(e, None);
        assert_eq!((a.class, a.kind.as_str()), (class, kind));
    }
}

#[test]
fn io_errors_follow_their_kind() {
    use std::io::ErrorKind::*;
    for (k, class) in [
        (NotFound, Class::NotFound),
        (PermissionDenied, Class::AccessDenied),
        (AlreadyExists, Class::Exists),
        (StorageFull, Class::DiskFull),
        (Interrupted, Class::Io),
        (Other, Class::Io),
    ] {
        assert_eq!(
            format_class(&FormatError::Io(Error::from(k))),
            class,
            "{k:?}"
        );
    }
}

// One arm per variant and no wildcard: a variant added to any of the four enums breaks this
// test's build until it is named here, and the tests below then need its row.

fn format_variant(e: &FormatError) -> &'static str {
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

fn core_variant(e: &CoreError) -> &'static str {
    match e {
        CoreError::Format(_) => "Format",
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
        CoreError::Foreign(_) => "Foreign",
        CoreError::Worker(_) => "Worker",
    }
}

fn foreign_variant(e: &ForeignError) -> &'static str {
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

fn worker_variant(e: &WorkerError) -> &'static str {
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

/// Every variant has a row: the rows name as many distinct variants as the matches have arms.
#[test]
fn every_variant_has_a_row() {
    use std::collections::HashSet;
    let f: HashSet<&str> = format_table()
        .iter()
        .map(|(e, _, _)| format_variant(e))
        .collect();
    assert_eq!(f.len(), 75, "FormatError rows");
    let c: HashSet<&str> = core_table()
        .iter()
        .map(|(e, _, _)| core_variant(e))
        .collect();
    assert_eq!(c.len(), 18, "CoreError rows");
    let mut fo = HashSet::new();
    let mut wo = HashSet::new();
    for (e, _, _) in foreign_and_worker() {
        match e {
            CoreError::Foreign(x) => {
                fo.insert(foreign_variant(&x));
            }
            CoreError::Worker(x) => {
                wo.insert(worker_variant(&x));
            }
            _ => {}
        }
    }
    assert_eq!(fo.len(), 8, "ForeignError rows");
    assert_eq!(wo.len(), 11, "WorkerError rows");
}
