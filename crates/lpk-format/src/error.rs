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
    /// The Merkle root stored in the index differs from the one recomputed.
    #[error("index Merkle root does not match the chunk table")]
    MerkleRootMismatch,
    /// The index frame's payload hash differs from the trailer's `index_hash`.
    #[error("index hash does not match the trailer")]
    IndexHashMismatch,
    /// The trailer's `archive_id` differs from the header's.
    #[error("trailer archive id differs from the header's")]
    ArchiveIdMismatch,
    /// A block's `plain_len` differs from the sum over its chunks.
    #[error("block {block}: plain length does not match its chunks")]
    BlockLengthMismatch {
        /// Index into the block table.
        block: usize,
    },
    /// The blocks do not partition the chunk table.
    #[error("block {block}: chunk range does not continue the previous block")]
    BlockCoverage {
        /// Index into the block table (one past the end when blocks run short).
        block: usize,
    },
    /// A block's frame lies outside the area between the header and the index.
    #[error("block {block}: frame location out of range")]
    BlockOutOfRange {
        /// Index into the block table.
        block: usize,
    },
    /// A located frame has another kind than expected.
    #[error("expected frame kind {expected}, found {found}")]
    WrongFrameKind {
        /// The kind asked for.
        expected: u16,
        /// The kind found.
        found: u16,
    },
    /// The last bytes of the archive are not a trailer frame.
    #[error("no trailer at the end of the archive")]
    NoTrailer,
    /// The index's generation table breaks its rules (section 15).
    #[error("bad generation table")]
    BadGenerationTable,
    /// A trailer field is out of range.
    #[error("bad trailer ({reason})")]
    BadTrailer {
        /// The field at fault.
        reason: &'static str,
    },
    /// Rollback named a generation the archive does not have.
    #[error("no generation {requested} in the archive (latest is {latest})")]
    NoSuchGeneration {
        /// The generation asked for.
        requested: u64,
        /// The archive's latest generation.
        latest: u64,
    },
    /// Appending to an encrypted archive needs the credentials.
    #[error("appending to an encrypted archive needs the password")]
    AppendNeedsCredentials,
    /// The trailer chain's generation numbers do not fall by one.
    #[error("trailer chain: expected generation {expected}, found {found}")]
    GenerationMismatch {
        /// The number the chain should have had.
        expected: u64,
        /// The number the trailer carries.
        found: u64,
    },
    /// A recorded frame location is out of range or its length is wrong.
    #[error("bad frame location for {what}")]
    BadFrameLocation {
        /// Which frame.
        what: &'static str,
    },
    /// The archive declares more than this reader allows (spec section 7).
    #[error("{0}")]
    Refused(crate::envelope::Refusal),
    /// An envelope field contradicts the block table or the index length.
    #[error("decode envelope field {field} does not match the archive")]
    EnvelopeMismatch {
        /// The envelope field.
        field: &'static str,
    },
    /// A decode graph names a primitive ID that is not in the registry.
    #[error("unknown primitive {id:#06x}")]
    UnknownPrimitive {
        /// The primitive ID.
        id: u16,
    },
    /// The primitive is in the registry but this reader cannot run it.
    #[error("primitive {id:#06x} is not implemented by this reader")]
    UnimplementedPrimitive {
        /// The primitive ID.
        id: u16,
    },
    /// A decode graph is malformed.
    #[error("bad decode graph ({reason})")]
    BadGraph {
        /// Short reason.
        reason: &'static str,
    },
    /// A primitive's parameters do not match its layout.
    #[error("primitive {id:#06x}: bad parameters ({reason})")]
    BadParams {
        /// The primitive ID.
        id: u16,
        /// Short reason.
        reason: &'static str,
    },
    /// A reconstruction record has a kind this reader does not know.
    #[error("record {record}: unknown record kind {kind}")]
    UnknownRecordKind {
        /// The raw kind.
        kind: u16,
        /// Id of the record (its position in the frame).
        record: u64,
    },
    /// Reserved record flag bits are set.
    #[error("record {record}: reserved flag bits {bits:#x}")]
    ReservedRecordBits {
        /// The offending bits.
        bits: u16,
        /// Id of the record.
        record: u64,
    },
    /// A record's body does not match its `body_hash`.
    #[error("record {record} does not match its hash")]
    RecordHashMismatch {
        /// Id of the record.
        record: u64,
    },
    /// A block names a record id the `Records` frame does not have.
    #[error("record id {record} out of range (the archive has {count})")]
    RecordOutOfRange {
        /// The id asked for.
        record: u64,
        /// Number of records in the archive.
        count: u64,
    },
    /// A record's body breaks its layout.
    #[error("record {record}: bad record ({reason})")]
    BadRecord {
        /// Id of the record.
        record: u64,
        /// The field at fault.
        reason: &'static str,
    },
    /// A block names a prior the caller's store does not have.
    #[error("prior {} is not available", hex32(.id))]
    MissingPrior {
        /// The prior's ID (BLAKE3 of its bytes).
        id: [u8; 32],
    },
    /// A block names a prior the index does not list.
    #[error("block names prior {} which the index does not list", hex32(.id))]
    UnlistedPrior {
        /// The prior's ID.
        id: [u8; 32],
    },
    /// The index's prior list is not ascending, unique and non-zero.
    #[error("bad prior list ({reason})")]
    BadPriorList {
        /// Short reason.
        reason: &'static str,
    },
    /// A decoder needs a larger window than the one declared or allowed.
    #[error("decoder window of {needed} bytes exceeds the {allowed} allowed")]
    WindowTooLarge {
        /// The window asked for, in bytes.
        needed: u64,
        /// The window allowed, in bytes.
        allowed: u64,
    },
    /// The zstd decoder rejected its input.
    #[error("zstd: {reason}")]
    ZstdError {
        /// The decoder's own text.
        reason: String,
    },
    /// The LZMA decoder rejected its input.
    #[error("lzma: {reason}")]
    LzmaError {
        /// Short reason (`truncated`, `trailing input`, or the decoder's text).
        reason: String,
    },
    /// The writer was given options it cannot honour.
    #[error("bad writer options ({reason})")]
    BadOptions {
        /// Short reason.
        reason: &'static str,
    },
    /// A chunker broke the cutting rules.
    #[error("bad chunker output ({reason})")]
    BadChunk {
        /// Short reason.
        reason: &'static str,
    },
    /// A recovery frame's fields contradict each other or the archive.
    #[error("bad recovery frame ({reason})")]
    BadRecovery {
        /// The field at fault.
        reason: &'static str,
    },
    /// More shards are damaged than a recovery frame can rebuild.
    #[error("recovery frame {frame}: {damaged} shards damaged, it can rebuild {capacity}")]
    Unrepairable {
        /// Position of the frame in the index's list.
        frame: usize,
        /// Damaged data shards in its coverage.
        damaged: u64,
        /// Shards the frame can rebuild.
        capacity: u64,
    },
    /// `lpk-decode check` found damage (not a library error).
    #[error("damage found: {damaged} shards damaged, {unusable} recovery frames unusable")]
    DamageFound {
        /// Damaged data shards.
        damaged: u64,
        /// Unusable recovery frames.
        unusable: u64,
    },
    /// The Reed-Solomon library refused its input.
    #[error("recovery: {reason}")]
    RecoveryError {
        /// The library's own text.
        reason: String,
    },
    /// An entry path is not safe to extract on this platform.
    #[error("unsafe path {path:?} ({reason})")]
    UnsafePath {
        /// The archive path.
        path: String,
        /// Short reason.
        reason: &'static str,
    },
    /// The extraction tool does not create symbolic links.
    #[error("symlink entry {path:?} refused")]
    SymlinkRefused {
        /// The archive path.
        path: String,
    },
    /// The archive is encrypted and no credentials were given.
    #[error("the archive is encrypted: a password is required")]
    PasswordRequired,
    /// The key slot did not open with the given credentials.
    #[error("wrong password or keyfile")]
    WrongKey,
    /// A sealed frame failed its authentication tag.
    #[error("frame kind {kind} (sequence {sequence}) failed authentication")]
    AuthenticationFailed {
        /// Raw kind of the frame.
        kind: u16,
        /// Position of the frame in the archive.
        sequence: u64,
    },
    /// A frame is sealed where the archive does not seal it.
    #[error("frame kind {kind} is sealed but must not be")]
    UnexpectedSealedFrame {
        /// Raw kind of the frame.
        kind: u16,
    },
    /// A frame is not sealed where the archive seals it.
    #[error("frame kind {kind} must be sealed but is not")]
    UnsealedFrame {
        /// Raw kind of the frame.
        kind: u16,
    },
    /// A key slot frame in an archive that is not encrypted, or a second one.
    #[error("unexpected key slot frame")]
    UnexpectedKeySlot,
    /// An encrypted archive whose first frame is not the key slot.
    #[error("the first frame of an encrypted archive must be the key slot")]
    MissingKeySlot,
    /// The key slot's fields are invalid.
    #[error("bad key slot ({reason})")]
    BadKeySlot {
        /// The field at fault.
        reason: &'static str,
    },
    /// Argon2 parameters outside the bounds.
    #[error("bad argon2 parameters ({reason})")]
    BadArgon2 {
        /// The field at fault.
        reason: &'static str,
    },
    /// The entry table frame does not match the hash the index records.
    #[error("entry table does not match the index")]
    EntryTableMismatch,
    /// The cipher refused to seal a payload.
    #[error("sealing frame kind {kind} (sequence {sequence}) failed")]
    SealFailed {
        /// Raw kind of the frame.
        kind: u16,
        /// Position of the frame in the archive.
        sequence: u64,
    },
    /// An underlying I/O error.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

fn hex32(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
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
