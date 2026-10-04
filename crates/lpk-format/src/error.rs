//! Error type of the format crate.

/// Every way reading or writing the container grammar can fail.
#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    /// The first eight bytes are not the `.lpk` magic.
    #[error("bad magic")]
    BadMagic,
    /// The header names a major version this reader does not support.
    #[error("unsupported major version {found}")]
    UnsupportedMajor {
        /// The major version found in the header.
        found: u16,
    },
    /// Reserved header flag bits are set.
    #[error("reserved header flag bits set: {bits:#x}")]
    ReservedHeaderBits {
        /// The offending bits.
        bits: u32,
    },
    /// Reserved frame flag bits are set.
    #[error("reserved frame flag bits set: {bits:#x}")]
    ReservedFrameBits {
        /// The offending bits.
        bits: u16,
    },
    /// Frame kind 0 is invalid.
    #[error("invalid frame kind")]
    InvalidKind,
    /// A varint was not in its shortest form.
    #[error("non-canonical varint")]
    NonCanonicalVarint,
    /// A varint ran past ten bytes.
    #[error("varint too long")]
    VarintTooLong,
    /// The input ended inside the named part.
    #[error("input truncated in {what}")]
    Truncated {
        /// Which part was cut short.
        what: &'static str,
    },
    /// The payload does not match its BLAKE3 hash.
    #[error("payload hash mismatch in frame kind {kind}")]
    HashMismatch {
        /// Raw kind of the frame.
        kind: u16,
    },
    /// A frame of unknown kind demands to be understood.
    #[error("unknown frame kind {kind} must be understood")]
    UnknownMustUnderstand {
        /// Raw kind of the frame.
        kind: u16,
    },
    /// The declared payload length exceeds the reader limit.
    #[error("payload length {len} exceeds limit {max}")]
    PayloadTooLarge {
        /// Declared length.
        len: u64,
        /// The limit in force.
        max: u64,
    },
    /// An underlying I/O error.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Fill `buf` exactly, mapping a short read to `Truncated { what }`.
pub(crate) fn read_exact_or(
    r: &mut impl std::io::Read,
    buf: &mut [u8],
    what: &'static str,
) -> Result<(), FormatError> {
    r.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            FormatError::Truncated { what }
        } else {
            FormatError::Io(e)
        }
    })
}
