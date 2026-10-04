//! One error type: the spec's error class (its name) and a human message.

use std::fmt;

/// An error of the format, named by the class the specification gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The class name as the specification writes it (`Truncated`, `HashMismatch`, ...).
    pub class: &'static str,
    /// A human-readable message.
    pub detail: String,
}

impl Error {
    /// Builds an error of a class.
    pub fn new(class: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class,
            detail: detail.into(),
        }
    }

    /// `Truncated { what }`.
    pub fn truncated(what: &str) -> Self {
        Self::new("Truncated", format!("input truncated in {what}"))
    }

    /// `TrailingBytes { what }`.
    pub fn trailing(what: &str) -> Self {
        Self::new("TrailingBytes", format!("trailing bytes after {what}"))
    }

    /// `HashMismatch` of a frame kind.
    pub fn hash_mismatch(kind: u16) -> Self {
        Self::new(
            "HashMismatch",
            format!("payload hash mismatch in frame kind {kind}"),
        )
    }

    /// `BadFrameLocation { what }`.
    pub fn bad_location(what: &str) -> Self {
        Self::new("BadFrameLocation", format!("bad frame location: {what}"))
    }

    /// An I/O error of the tool (not a format error).
    pub fn io(context: &str, e: &std::io::Error) -> Self {
        Self::new("Io", format!("{context}: {e}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for Error {}

/// Result with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
