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
    /// Entry paths are not strictly ascending.
    #[error("entry {index} is out of order or a duplicate")]
    UnsortedEntries {
        /// Index of the offending entry.
        index: u64,
    },
    /// Entry kind byte not known to this reader.
    #[error("entry {index}: unsupported kind {kind}")]
    UnsupportedEntryKind {
        /// The raw kind.
        kind: u8,
        /// Index of the entry.
        index: u64,
    },
    /// Reserved entry flag bits are set.
    #[error("entry {index}: reserved flag bits {bits:#x}")]
    ReservedEntryBits {
        /// The offending bits.
        bits: u16,
        /// Index of the entry.
        index: u64,
    },
    /// An entry path (or symlink target) is invalid.
    #[error("entry {index}: invalid path ({reason})")]
    InvalidPath {
        /// Index of the entry.
        index: u64,
        /// Short reason.
        reason: &'static str,
    },
    /// An entry's fields contradict each other.
    #[error("entry {index}: inconsistent ({reason})")]
    InconsistentEntry {
        /// Index of the entry.
        index: u64,
        /// Short reason.
        reason: &'static str,
    },
    /// Bytes remain after the last item of a payload.
    #[error("trailing bytes after {what}")]
    TrailingBytes {
        /// Which payload.
        what: &'static str,
    },
    /// A chunk's bytes do not match its table hash (or length).
    #[error("chunk {chunk} does not match its hash")]
    ChunkMismatch {
        /// Index of the chunk in the chunk table.
        chunk: u64,
    },
    /// A chunk index is not in the chunk table.
    #[error("chunk index {chunk} out of range (table has {len})")]
    ChunkIndexOutOfRange {
        /// The offending index.
        chunk: u64,
        /// Number of records in the table.
        len: u64,
    },
    /// A file's length differs from the sum of its chunk lengths.
    #[error("file size {expected} does not match chunk total {found}")]
    FileSizeMismatch {
        /// The file length claimed.
        expected: u64,
        /// The sum of the chunks' `plain_len`.
        found: u64,
    },
    /// A requested byte range extends past the end of the file.
    #[error("range {offset}+{len} is outside a file of {file_len} bytes")]
    RangeOutOfFile {
        /// First byte of the range.
        offset: u64,
        /// Length of the range.
        len: u64,
        /// Length of the file.
        file_len: u64,
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
