//! LitePack `.lpk` container format: header, varints, the hashed frame grammar, the entry
//! table, the chunk table and the Merkle tree with file and range verification, and the
//! index, trailer and archive opener, and encryption (key slot, sealed frames).
//!
//! This is the reference reader side of the format; see `docs/spec/lpk-v1.md`.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod archive;
pub mod chunk;
pub mod cli;
pub mod crypto;
pub mod decode;
pub mod entry;
pub mod envelope;
pub mod error;
pub mod frame;
pub mod graph;
pub mod header;
pub mod index;
pub mod lzma;
pub mod magic;
pub mod merkle;
pub mod primitive;
pub mod priors;
pub mod reader;
pub mod record;
pub mod recovery;
pub mod trailer;
pub mod varint;
pub mod writer;
pub mod zstd;

pub use archive::{generation_rules_table, rollback, Archive, Diagnosis, Generation};
pub use chunk::{
    chunk_record_table, verify_file, verify_range, ChunkIndex, ChunkIter, ChunkLookup, ChunkPlace,
    ChunkRecord, ChunkSource, ChunkTable, ChunkTableWriter, MIN_CHUNK_RECORD_LEN,
};
pub use index::{index_layout_table, BlockLocation, FrameLocation, GenerationInfo, Index};
pub use merkle::{
    merkle_root, verify_proof, MerkleTree, MERKLE_EMPTY_CONTEXT, MERKLE_NODE_CONTEXT,
};
pub use priors::{prior_id, prior_list_table, MemoryPriors, NoPriors, PriorStore};
pub use trailer::{trailer_layout_table, Trailer, TRAILER_FRAME_LEN, TRAILER_PAYLOAD_LEN};

pub use crypto::index_sequence;
pub use crypto::{
    associated_data, derive_nonce, key_slot_table, sealing_rule, sealing_rules_table, ArchiveKey,
    Argon2Params, Credentials, KeySlot, Sealer, Suite, INDEX_SEQUENCE, KEY_SLOT_LEN,
};
pub use decode::{
    decode_block, decode_block_in, DecodeContext, NoContext, PrimitiveDecoder, Registry,
};
pub use entry::{
    entry_byte_table, entry_flag_table, entry_kind_table, entry_payload_table, validate_path,
    Entry, EntryFlags, EntryIter, EntryKind, EntryTable, EntryTableWriter, MIN_ENTRY_LEN,
};
pub use envelope::{
    envelope_layout_table, resources_default_table, ArchiveSizes, Envelope, Refusal, Resources,
    DEFAULT_MAX_BWT_BLOCK, DEFAULT_MAX_WINDOW,
};
pub use error::FormatError;
pub use frame::{
    frame_flag_table, frame_kind_table, frame_layout_table, Frame, FrameFlags, FrameKind,
    ReadFrame, ReadLimits,
};
pub use graph::{
    block_header_table, graph_layout_table, BlockHeader, Graph, Step, MAX_PARAMS, MAX_STEPS,
};
pub use header::{header_byte_table, header_flag_table, FormatVersion, Header, HeaderFlags};
pub use lzma::LzmaDecoder;
pub use magic::MAGIC;
pub use primitive::{primitive_table, GraphResources, PrimitiveId};
pub use reader::{ArchiveChunks, OwnedEntryTable, OwnedRecordsTable, VerifySummary};
pub use record::{
    record_kind_table, record_layout_tables, Base64Record, ContainerMember, ContainerRecord,
    DeflateRecord, JpegRecord, PngFilterRecord, Record, RecordBody, RecordKind, RecordsIter,
    RecordsTable, RecordsWriter, SecondaryImage, Utf16Record, RECORD_COUNT_BOUND,
};
pub use recovery::{
    decoder_work_bytes, encoder_work_bytes, group_recovery_shards, recovery_layout_table, repair,
    repair_with_credentials, repair_with_report, RecoveryFrame, RecoveryOptions, RepairReport,
    DEFAULT_GROUP_SHARDS, DEFAULT_SHARD_LEN, MAX_GROUP_BYTES, MAX_GROUP_SHARDS, MAX_PERCENT,
    MAX_SHARD_LEN, MAX_TOTAL_SHARDS, SHARD_ALIGN,
};
pub use writer::{
    BlockEncoder, Chunker, Encoded, FixedChunker, SealOptions, StoreEncoder, Writer, WriterOptions,
    WriterSummary, DEFAULT_BLOCK_SIZE, DEFAULT_CHUNK_SIZE, MIN_CHUNK_SIZE,
};
pub use zstd::ZstdDecoder;
