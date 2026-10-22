//! How a failed command is reported: one exit code per failure class (D-48's three codes: 0,
//! 1 usage, 2 refused or failed), one line on stderr — `error: <message>` as before, or with
//! the global `--json` flag exactly one JSON object line:
//! `{"error":{"code":1|2,"kind":"<variant>","message":"...","path":"..."?,"entries":[...]?}}`.
//!
//! `kind` is the error's variant name (`Usage` for a command-line error; for a
//! [`CoreError::Format`] wrapper the inner [`FormatError`]'s name, since the wrapper adds
//! nothing; `Reference` for a failure the reference tool's code reported through its own
//! error line, whose variant does not reach this crate yet). `path` is the path the error
//! names when it names one, else the archive the command was working on. `entries` lists the
//! per-entry errors of a partial extraction (each `{"path","kind","message"}`); it is
//! present only when there are some.
//!
//! The classification is an exhaustive match over every variant of [`CoreError`] and
//! [`FormatError`], so a variant added to either enum does not compile until it is placed.

use std::io::Write;
use std::path::Path;

use lpk_core::CoreError;
use lpk_format::FormatError;

use crate::{EXIT_FAILED, EXIT_USAGE};

/// A command that did not succeed: what to print and how to exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// 1 (usage) or 2 (refused or failed).
    pub code: i32,
    /// The variant name, `Usage`, or `Reference`.
    pub kind: String,
    /// The text after `error: ` (the error's own Display, possibly prefixed by the path).
    pub message: String,
    /// The path the error names, or the archive the command worked on.
    pub path: Option<String>,
    /// Per-entry errors of a partial extraction (empty otherwise).
    pub entries: Vec<EntryFailure>,
}

/// One entry of a partial extraction that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryFailure {
    /// The archive path of the entry.
    pub path: String,
    /// The error's variant name.
    pub kind: String,
    /// The error's Display.
    pub message: String,
}

impl Failure {
    /// A usage error (exit 1): a wrong command line, refused before anything was done.
    pub fn usage(message: impl Into<String>) -> Self {
        Failure {
            code: EXIT_USAGE,
            kind: "Usage".to_string(),
            message: message.into(),
            path: None,
            entries: Vec::new(),
        }
    }

    /// A failure of a named kind with exit 2.
    pub fn failed(kind: &str, message: impl Into<String>, path: Option<&Path>) -> Self {
        Failure {
            code: EXIT_FAILED,
            kind: kind.to_string(),
            message: message.into(),
            path: path.map(|p| p.display().to_string()),
            entries: Vec::new(),
        }
    }

    /// A pipeline error; `archive` names the archive when the error names no path itself.
    pub fn core(e: &CoreError, archive: Option<&Path>) -> Self {
        Failure {
            code: core_exit(e),
            kind: core_kind(e).to_string(),
            message: e.to_string(),
            path: core_path(e).or_else(|| archive.map(|p| p.display().to_string())),
            entries: Vec::new(),
        }
    }

    /// A format error; `archive` names the archive when the error names no path itself.
    pub fn format(e: &FormatError, archive: Option<&Path>) -> Self {
        Failure {
            code: format_exit(e),
            kind: format_kind(e).to_string(),
            message: e.to_string(),
            path: format_path(e).or_else(|| archive.map(|p| p.display().to_string())),
            entries: Vec::new(),
        }
    }

    /// The text form: `error: <message>` and one `  <path>: <message>` line per entry.
    pub fn write_text(&self, err: &mut dyn Write) {
        let _ = writeln!(err, "error: {}", self.message);
        for e in &self.entries {
            let _ = writeln!(err, "  {}: {}", e.path, e.message);
        }
    }

    /// The `--json` form: exactly one object line.
    pub fn write_json(&self, err: &mut dyn Write) {
        let mut object = serde_json::Map::new();
        object.insert("code".into(), self.code.into());
        object.insert("kind".into(), self.kind.as_str().into());
        object.insert("message".into(), self.message.as_str().into());
        if let Some(p) = &self.path {
            object.insert("path".into(), p.as_str().into());
        }
        if !self.entries.is_empty() {
            let entries: Vec<serde_json::Value> = self
                .entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "path": e.path,
                        "kind": e.kind,
                        "message": e.message,
                    })
                })
                .collect();
            object.insert("entries".into(), entries.into());
        }
        let line = serde_json::json!({ "error": object });
        let _ = writeln!(err, "{line}");
    }

    /// Print in the form the global flag selects.
    pub fn write(&self, json: bool, err: &mut dyn Write) {
        if json {
            self.write_json(err);
        } else {
            self.write_text(err);
        }
    }
}

/// The exit code of a pipeline error: 1 for an option the engine refused (a usage error
/// found after parsing), 2 for everything else (D-48). Every variant placed by name.
pub fn core_exit(e: &CoreError) -> i32 {
    match e {
        CoreError::InvalidOption(_) => EXIT_USAGE,
        CoreError::Format(inner) => format_exit(inner),
        CoreError::Io { .. }
        | CoreError::UnportableName { .. }
        | CoreError::SpecialFile { .. }
        | CoreError::DuplicatePath { .. }
        | CoreError::ChangedWhileReading { .. }
        | CoreError::Decode { .. }
        | CoreError::Extract(_)
        | CoreError::Export { .. }
        | CoreError::InTheWay { .. }
        | CoreError::CaseCollision { .. }
        | CoreError::NotADirectory { .. }
        | CoreError::Cancelled
        | CoreError::AppendLeftCut { .. } => EXIT_FAILED,
    }
}

/// The exit code of a format error: always 2 (an archive, a key or an I/O failure, never
/// the command line). Every variant placed by name.
pub fn format_exit(e: &FormatError) -> i32 {
    match e {
        FormatError::BadMagic
        | FormatError::UnsupportedMajor { .. }
        | FormatError::ReservedHeaderBits { .. }
        | FormatError::BadHeaderFlags { .. }
        | FormatError::ReservedFrameBits { .. }
        | FormatError::InvalidKind
        | FormatError::NonCanonicalVarint
        | FormatError::VarintTooLong
        | FormatError::Truncated { .. }
        | FormatError::HashMismatch { .. }
        | FormatError::UnknownMustUnderstand { .. }
        | FormatError::PayloadTooLarge { .. }
        | FormatError::UnsortedEntries { .. }
        | FormatError::DuplicateEntry { .. }
        | FormatError::UnsupportedEntryKind { .. }
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
        | FormatError::NoSuchGeneration { .. }
        | FormatError::AppendNeedsCredentials
        | FormatError::GenerationMismatch { .. }
        | FormatError::BadFrameLocation { .. }
        | FormatError::Refused(_)
        | FormatError::EnvelopeMismatch { .. }
        | FormatError::UnknownPrimitive { .. }
        | FormatError::UnimplementedPrimitive { .. }
        | FormatError::BadGraph { .. }
        | FormatError::BadParams { .. }
        | FormatError::UnknownRecordKind { .. }
        | FormatError::ReservedRecordBits { .. }
        | FormatError::RecordHashMismatch { .. }
        | FormatError::RecordOutOfRange { .. }
        | FormatError::BadRecord { .. }
        | FormatError::MissingPrior { .. }
        | FormatError::UnlistedPrior { .. }
        | FormatError::BadPriorList { .. }
        | FormatError::WindowTooLarge { .. }
        | FormatError::ZstdError { .. }
        | FormatError::LzmaError { .. }
        | FormatError::BwtError { .. }
        | FormatError::BadOptions { .. }
        | FormatError::BadChunk { .. }
        | FormatError::BadRecovery { .. }
        | FormatError::Unrepairable { .. }
        | FormatError::DamageFound { .. }
        | FormatError::RecoveryError { .. }
        | FormatError::UnsafePath { .. }
        | FormatError::SymlinkRefused { .. }
        | FormatError::PasswordRequired
        | FormatError::WrongKey
        | FormatError::AuthenticationFailed { .. }
        | FormatError::UnexpectedSealedFrame { .. }
        | FormatError::UnsealedFrame { .. }
        | FormatError::UnexpectedKeySlot
        | FormatError::MissingKeySlot
        | FormatError::BadKeySlot { .. }
        | FormatError::BadArgon2 { .. }
        | FormatError::EntryTableMismatch
        | FormatError::SealFailed { .. }
        | FormatError::Io(_) => EXIT_FAILED,
    }
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
        CoreError::NotADirectory { .. } => "NotADirectory",
        CoreError::Cancelled => "Cancelled",
        CoreError::AppendLeftCut { .. } => "AppendLeftCut",
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
        CoreError::InvalidOption(_) | CoreError::Extract(_) | CoreError::Cancelled => None,
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_json_line_has_the_documented_shape() {
        let f = Failure {
            code: 2,
            kind: "Decode".to_string(),
            message: "block 3: bad".to_string(),
            path: Some("a/b".to_string()),
            entries: vec![EntryFailure {
                path: "a/b".to_string(),
                kind: "ChunkMismatch".to_string(),
                message: "chunk 7 does not match its hash".to_string(),
            }],
        };
        let mut err = Vec::new();
        f.write_json(&mut err);
        let text = String::from_utf8(err).unwrap();
        assert_eq!(text.matches('\n').count(), 1);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], 2);
        assert_eq!(v["error"]["kind"], "Decode");
        assert_eq!(v["error"]["message"], "block 3: bad");
        assert_eq!(v["error"]["path"], "a/b");
        assert_eq!(v["error"]["entries"][0]["kind"], "ChunkMismatch");
        assert_eq!(v["error"].as_object().unwrap().len(), 5);

        let u = Failure::usage("bad flag");
        let mut err = Vec::new();
        u.write_json(&mut err);
        let v: serde_json::Value = serde_json::from_slice(&err).unwrap();
        assert_eq!(v["error"]["code"], 1);
        assert_eq!(v["error"]["kind"], "Usage");
        assert_eq!(v["error"].as_object().unwrap().len(), 3, "{v}");

        let mut err = Vec::new();
        f.write_text(&mut err);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "error: block 3: bad\n  a/b: chunk 7 does not match its hash\n"
        );
    }
}
