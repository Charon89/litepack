//! The LitePack pipeline. It holds ingest (a directory tree to the format's inputs, in the entry
//! table's order), the store path (those inputs written with the writer's default store graph),
//! the classifier and the entropy gate, clustering by class, and the Fast tier (zstd with a long
//! window, optional caller-supplied dictionaries, one block per cluster) and the Balanced tier
//! (LZMA or zstd per block by a trial on a sample). Fold (chunking and
//! global dedup), Peel, Model and Seal build on these names later.
//!
//! Files are read through a plain `File` for every size, so every read failure is a
//! [`CoreError::Io`] and never a process fault; a memory-mapped path may return when a measured
//! need appears.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod balanced;
pub mod classify;
pub mod cluster;
pub mod error;
pub mod extract;
pub mod fast;
pub mod fold;
pub mod gate;
pub mod ingest;
pub mod peel;
pub mod pipeline;
pub mod priors;
pub mod source;
pub mod store;

pub use balanced::{
    archive_balanced, archive_balanced_file, BalancedEncoder, BalancedHandle, BalancedOptions,
    BalancedSummary, LzmaEncoder,
};
pub use classify::{classify, Class, Features};
pub use cluster::{cluster, Cluster, DictionaryKind};
pub use error::CoreError;
pub use extract::{
    extract_archive, extract_file, DefaultPolicy, ExtractOptions, ExtractPolicy, ExtractSummary,
};
pub use fast::{
    archive_fast, archive_fast_file, DictionaryPolicy, FastHandle, FastOptions, FastSummary,
    ZstdEncoder,
};
pub use fold::{
    ChunkerKind, Dedup, FastCdcChunker, FoldOptions, FoldStage, Ordering, OrderingSummary,
};
pub use gate::{entropy, is_incompressible, sampled_entropy, Gate, GATE_BLOCK};
pub use ingest::{file_identity, validate_input, walk, IngestOptions, Input};
pub use peel::jpeg::register_full_reader;
pub use peel::{
    Cause, Count, Fallback, JpegDecoder, JpegPeel, NestedPart, PeelPlan, PeelStage, PeelSummary,
};
pub use pipeline::{ClassifyStage, ModelStage, Pipeline, RunSummary, SealOptions, StageTimings};
pub use priors::ProvidedDictionaries;
pub use source::Source;
pub use store::{archive_store, archive_store_file, write_inputs, StoreOptions};
