//! The LitePack pipeline. This first slice holds ingest (a directory tree to the format's
//! inputs, in the entry table's order) and the store path (those inputs written with the
//! writer's default store graph). The classifier, Fold, Peel, Model and Seal build on these
//! names later.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod error;
pub mod ingest;
pub mod source;
pub mod store;

pub use error::CoreError;
pub use ingest::{walk, IngestOptions, Input};
pub use source::Source;
pub use store::{archive_store, archive_store_file, StoreOptions};
