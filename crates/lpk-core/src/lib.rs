//! The LitePack pipeline. This first slice holds ingest (a directory tree to the format's
//! inputs, in the entry table's order) and the store path (those inputs written with the
//! writer's default store graph). The classifier, Fold, Peel, Model and Seal build on these
//! names later.
//!
//! Files are read through a plain `File` for every size, so every read failure is a
//! [`CoreError::Io`] and never a process fault; a memory-mapped path may return when a measured
//! need appears.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod classify;
pub mod cluster;
pub mod error;
pub mod fast;
pub mod gate;
pub mod ingest;
pub mod priors;
pub mod source;
pub mod store;

pub use classify::{classify, Class, Features};
pub use cluster::{cluster, Cluster, DictionaryKind};
pub use error::CoreError;
pub use fast::{
    archive_fast, archive_fast_file, DictionaryPolicy, FastHandle, FastOptions, FastSummary,
    ZstdEncoder,
};
pub use gate::{entropy, is_incompressible, sampled_entropy, Gate, GATE_BLOCK};
pub use ingest::{file_identity, validate_input, walk, IngestOptions, Input};
pub use priors::BundledPriors;
pub use source::Source;
pub use store::{archive_store, archive_store_file, write_inputs, StoreOptions};
