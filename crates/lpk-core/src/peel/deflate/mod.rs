//! The Deflate peel's front half: finding the Deflate streams of a file ([`walk`]) and
//! reconstructing them bit for bit with `preflate-rs` ([`analyse`]). Both are moved unchanged
//! from Phase 0's `probe deflate` (design `docs/notes/e2-10-11-design.md`, section 3.1);
//! [`examine`] is the probe's per-file loop, calling back once per reconstructed stream.
//!
//! The walker inflates with `flate2`, whose backend is C zlib in the workspace build and
//! `miniz_oxide` when this crate is built alone: a different backend can change which damaged
//! streams become candidates, never what a reconstructed stream recreates.

pub mod analyse;
pub mod walk;

pub use analyse::{
    analyse_stream, code_name, examine, preflate_config, Counts, Examined, Limits, Reconstructed,
    StreamOutcome, FAILURE_NAMES, FILE_PLAIN_BUDGET, MAX_CHAIN_LENGTH, PANIC, PLAIN_TEXT_LIMIT,
};
/// The `preflate-rs` types that appear in this module's public items.
pub use preflate_rs::{PlainText, PreflateConfig, TokenPredictorParameters};
pub use walk::{find_streams, trim_spans, Candidate, Found, INFLATE_CAP, KINDS, MIN_SCANNED_PLAIN};
/// The pinned `preflate-rs` version (Cargo.toml pins it exactly).
pub const PREFLATE_VERSION: &str = "0.7.6";

#[cfg(test)]
mod testkit;
