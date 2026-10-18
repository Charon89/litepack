//! E3-7's exit-code table: every variant of `CoreError` and `FormatError`, constructed here
//! (neither enum is `#[non_exhaustive]`; the classification in `lpk_cli::failure` is an
//! exhaustive match, so a variant added to either enum fails to compile there, and this table
//! names each one once so its code and its `kind` are asserted). D-48's three codes only:
//! 1 for a usage error (`InvalidOption`: an option the engine refused), 2 for everything else.
#![allow(clippy::unwrap_used)]

use std::io::Error;
use std::path::PathBuf;

use lpk_cli::{core_exit, format_exit, Failure, EXIT_FAILED, EXIT_USAGE};
use lpk_core::CoreError;
use lpk_format::{FormatError, Refusal};

fn io() -> Error {
    Error::other("disk")
}

/// Every `FormatError` variant with the exit code it maps to and its kind name.
fn format_table() -> Vec<(FormatError, i32, &'static str)> {
    use FormatError::*;
    let f = EXIT_FAILED;
    vec![
        (BadMagic, f, "BadMagic"),
        (UnsupportedMajor { found: 2 }, f, "UnsupportedMajor"),
        (ReservedHeaderBits { bits: 8 }, f, "ReservedHeaderBits"),
        (BadHeaderFlags { bits: 2 }, f, "BadHeaderFlags"),
        (ReservedFrameBits { bits: 8 }, f, "ReservedFrameBits"),
        (InvalidKind, f, "InvalidKind"),
        (NonCanonicalVarint, f, "NonCanonicalVarint"),
        (VarintTooLong, f, "VarintTooLong"),
        (Truncated { what: "header" }, f, "Truncated"),
        (HashMismatch { kind: 2 }, f, "HashMismatch"),
        (
            UnknownMustUnderstand { kind: 9 },
            f,
            "UnknownMustUnderstand",
        ),
        (PayloadTooLarge { len: 2, max: 1 }, f, "PayloadTooLarge"),
        (UnsortedEntries { index: 1 }, f, "UnsortedEntries"),
        (DuplicateEntry { path: "a".into() }, f, "DuplicateEntry"),
        (
            UnsupportedEntryKind { kind: 3, index: 0 },
            f,
            "UnsupportedEntryKind",
        ),
        (
            ReservedEntryBits { bits: 16, index: 0 },
            f,
            "ReservedEntryBits",
        ),
        (
            InvalidPath {
                index: 0,
                reason: "empty",
            },
            f,
            "InvalidPath",
        ),
        (
            InconsistentEntry {
                index: 0,
                reason: "x",
            },
            f,
            "InconsistentEntry",
        ),
        (TrailingBytes { what: "index" }, f, "TrailingBytes"),
        (ChunkMismatch { chunk: 0 }, f, "ChunkMismatch"),
        (
            ChunkIndexOutOfRange { chunk: 1, len: 1 },
            f,
            "ChunkIndexOutOfRange",
        ),
        (
            FileSizeMismatch {
                expected: 1,
                found: 2,
            },
            f,
            "FileSizeMismatch",
        ),
        (
            RangeOutOfFile {
                offset: 1,
                len: 1,
                file_len: 1,
            },
            f,
            "RangeOutOfFile",
        ),
        (MerkleRootMismatch, f, "MerkleRootMismatch"),
        (IndexHashMismatch, f, "IndexHashMismatch"),
        (ArchiveIdMismatch, f, "ArchiveIdMismatch"),
        (BlockLengthMismatch { block: 0 }, f, "BlockLengthMismatch"),
        (BlockCoverage { block: 0 }, f, "BlockCoverage"),
        (BlockOutOfRange { block: 0 }, f, "BlockOutOfRange"),
        (
            WrongFrameKind {
                expected: 1,
                found: 2,
            },
            f,
            "WrongFrameKind",
        ),
        (NoTrailer, f, "NoTrailer"),
        (BadGenerationTable { reason: "x" }, f, "BadGenerationTable"),
        (BadTrailer { reason: "x" }, f, "BadTrailer"),
        (
            NoSuchGeneration {
                requested: 3,
                latest: 1,
            },
            f,
            "NoSuchGeneration",
        ),
        (AppendNeedsCredentials, f, "AppendNeedsCredentials"),
        (
            GenerationMismatch {
                expected: 1,
                found: 0,
            },
            f,
            "GenerationMismatch",
        ),
        (BadFrameLocation { what: "index" }, f, "BadFrameLocation"),
        (
            Refused(Refusal {
                field: "max_window",
                needed: 2,
                allowed: 1,
            }),
            f,
            "Refused",
        ),
        (
            EnvelopeMismatch {
                field: "max_window",
            },
            f,
            "EnvelopeMismatch",
        ),
        (UnknownPrimitive { id: 0xffff }, f, "UnknownPrimitive"),
        (
            UnimplementedPrimitive { id: 3 },
            f,
            "UnimplementedPrimitive",
        ),
        (BadGraph { reason: "x" }, f, "BadGraph"),
        (BadParams { id: 1, reason: "x" }, f, "BadParams"),
        (
            UnknownRecordKind { kind: 9, record: 0 },
            f,
            "UnknownRecordKind",
        ),
        (
            ReservedRecordBits { bits: 8, record: 0 },
            f,
            "ReservedRecordBits",
        ),
        (RecordHashMismatch { record: 0 }, f, "RecordHashMismatch"),
        (
            RecordOutOfRange {
                record: 1,
                count: 1,
            },
            f,
            "RecordOutOfRange",
        ),
        (
            BadRecord {
                record: 0,
                reason: "x",
            },
            f,
            "BadRecord",
        ),
        (MissingPrior { id: [0; 32] }, f, "MissingPrior"),
        (UnlistedPrior { id: [0; 32] }, f, "UnlistedPrior"),
        (BadPriorList { reason: "x" }, f, "BadPriorList"),
        (
            WindowTooLarge {
                needed: 2,
                allowed: 1,
            },
            f,
            "WindowTooLarge",
        ),
        (ZstdError { reason: "x".into() }, f, "ZstdError"),
        (LzmaError { reason: "x".into() }, f, "LzmaError"),
        (BadOptions { reason: "x" }, f, "BadOptions"),
        (BadChunk { reason: "x" }, f, "BadChunk"),
        (BadRecovery { reason: "x" }, f, "BadRecovery"),
        (
            Unrepairable {
                frame: 0,
                damaged: 2,
                capacity: 1,
            },
            f,
            "Unrepairable",
        ),
        (
            DamageFound {
                damaged: 1,
                unusable: 0,
            },
            f,
            "DamageFound",
        ),
        (RecoveryError { reason: "x".into() }, f, "RecoveryError"),
        (
            UnsafePath {
                path: "..".into(),
                reason: "dot-dot",
            },
            f,
            "UnsafePath",
        ),
        (SymlinkRefused { path: "l".into() }, f, "SymlinkRefused"),
        (PasswordRequired, f, "PasswordRequired"),
        (WrongKey, f, "WrongKey"),
        (
            AuthenticationFailed {
                kind: 2,
                sequence: 1,
            },
            f,
            "AuthenticationFailed",
        ),
        (
            UnexpectedSealedFrame { kind: 1 },
            f,
            "UnexpectedSealedFrame",
        ),
        (UnsealedFrame { kind: 2 }, f, "UnsealedFrame"),
        (UnexpectedKeySlot, f, "UnexpectedKeySlot"),
        (MissingKeySlot, f, "MissingKeySlot"),
        (BadKeySlot { reason: "x" }, f, "BadKeySlot"),
        (BadArgon2 { reason: "x" }, f, "BadArgon2"),
        (EntryTableMismatch, f, "EntryTableMismatch"),
        (
            SealFailed {
                kind: 2,
                sequence: 1,
            },
            f,
            "SealFailed",
        ),
        (Io(io()), f, "Io"),
    ]
}

/// Every `CoreError` variant with its exit code and kind name (`Format` reports the inner
/// variant's name, since the wrapper adds nothing).
fn core_table() -> Vec<(CoreError, i32, &'static str)> {
    use CoreError::*;
    let f = EXIT_FAILED;
    vec![
        (Format(FormatError::BadMagic), f, "BadMagic"),
        (
            Io {
                path: PathBuf::from("a"),
                source: io(),
            },
            f,
            "Io",
        ),
        (
            UnportableName {
                path: "a:b".into(),
                reason: "colon".into(),
            },
            f,
            "UnportableName",
        ),
        (
            SpecialFile {
                path: PathBuf::from("fifo"),
            },
            f,
            "SpecialFile",
        ),
        (DuplicatePath { path: "a".into() }, f, "DuplicatePath"),
        (
            ChangedWhileReading {
                path: PathBuf::from("a"),
            },
            f,
            "ChangedWhileReading",
        ),
        (
            InvalidOption("level 99".into()),
            EXIT_USAGE,
            "InvalidOption",
        ),
        (
            Decode {
                block: 3,
                path: "a".into(),
                source: FormatError::ChunkMismatch { chunk: 0 },
            },
            f,
            "Decode",
        ),
        (Extract("worker stopped"), f, "Extract"),
        (
            NotADirectory {
                path: PathBuf::from("a"),
            },
            f,
            "NotADirectory",
        ),
        (
            AppendLeftCut {
                path: PathBuf::from("a.lpk"),
                cause: Box::new(Extract("x")),
                kept: 10,
                truncation: io(),
            },
            f,
            "AppendLeftCut",
        ),
    ]
}

#[test]
fn every_format_error_variant_has_a_code_and_a_kind() {
    let table = format_table();
    assert_eq!(table.len(), 74, "one row per FormatError variant");
    for (e, code, kind) in table {
        assert_eq!(format_exit(&e), code, "{e}");
        let failure = Failure::format(&e, None);
        assert_eq!(failure.code, code, "{e}");
        assert_eq!(failure.kind, kind, "{e}");
        assert_eq!(failure.message, e.to_string());
        assert!(
            code == EXIT_USAGE || code == EXIT_FAILED,
            "{e}: D-48 has no other code"
        );
    }
}

#[test]
fn every_core_error_variant_has_a_code_and_a_kind() {
    let table = core_table();
    assert_eq!(table.len(), 11, "one row per CoreError variant");
    for (e, code, kind) in table {
        assert_eq!(core_exit(&e), code, "{e}");
        let failure = Failure::core(&e, Some(std::path::Path::new("x.lpk")));
        assert_eq!(failure.code, code, "{e}");
        assert_eq!(failure.kind, kind, "{e}");
        assert!(
            code == EXIT_USAGE || code == EXIT_FAILED,
            "{e}: D-48 has no other code"
        );
        // The path is the error's own when it names one, else the archive.
        let own = matches!(
            e,
            CoreError::Io { .. }
                | CoreError::UnportableName { .. }
                | CoreError::SpecialFile { .. }
                | CoreError::DuplicatePath { .. }
                | CoreError::ChangedWhileReading { .. }
                | CoreError::Decode { .. }
                | CoreError::NotADirectory { .. }
                | CoreError::AppendLeftCut { .. }
        );
        if own {
            assert_ne!(failure.path.as_deref(), Some("x.lpk"), "{e}");
        } else {
            assert_eq!(failure.path.as_deref(), Some("x.lpk"), "{e}");
        }
    }
}

#[test]
fn the_usage_failure_is_exit_1_and_a_failed_one_exit_2() {
    assert_eq!(Failure::usage("x").code, EXIT_USAGE);
    assert_eq!(Failure::failed("Io", "x", None).code, EXIT_FAILED);
}
