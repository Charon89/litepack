//! LitePack `.lpk` container format: header, varints, the hashed frame grammar, the entry
//! table, the chunk table and the Merkle tree with file and range verification.
//!
//! This is the reference reader side of the format; see `docs/spec/lpk-v1.md`.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod chunk;
pub mod entry;
pub mod error;
pub mod frame;
pub mod header;
pub mod magic;
pub mod merkle;
pub mod varint;

pub use chunk::{
    chunk_record_table, verify_file, verify_range, ChunkIter, ChunkRecord, ChunkSource, ChunkTable,
    ChunkTableWriter, MIN_CHUNK_RECORD_LEN,
};
pub use merkle::{
    merkle_root, verify_proof, MerkleTree, MERKLE_EMPTY_CONTEXT, MERKLE_NODE_CONTEXT,
};

pub use entry::{
    entry_byte_table, entry_flag_table, entry_kind_table, entry_payload_table, validate_path,
    Entry, EntryFlags, EntryIter, EntryKind, EntryTable, EntryTableWriter, MIN_ENTRY_LEN,
};
pub use error::FormatError;
pub use frame::{
    frame_flag_table, frame_kind_table, frame_layout_table, Frame, FrameFlags, FrameKind,
    ReadFrame, ReadLimits,
};
pub use header::{header_byte_table, header_flag_table, FormatVersion, Header, HeaderFlags};
pub use magic::MAGIC;
