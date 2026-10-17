//! The Deflate peel's front half: finding the Deflate streams of a file ([`walk`]) and
//! reconstructing them bit for bit with `preflate-rs` ([`analyse`]). Both are moved unchanged
//! from Phase 0's `probe deflate` (design `docs/notes/e2-10-11-design.md`, section 3.1);
//! [`examine`] is the probe's per-file loop, calling back once per reconstructed stream.
//!
//! The walker inflates with `flate2` on its C zlib backend, enabled in this crate's manifest, so
//! every build (the workspace's, or `lpk-cli`'s alone) uses the inflater the probe used.

pub mod analyse;
pub mod decode;
pub mod part;
pub mod pool;
pub mod stage;
pub mod walk;

pub use analyse::{
    analyse_stream, analysis_budget, examine, examine_with, work_budget, Counts, Estimate,
    Examined, Limits, Plain, Reconstructed, StreamOutcome, Visitor, ANALYSIS_BUDGET_FACTOR,
    FAILURE_NAMES, FILE_PLAIN_BUDGET, MAX_CHAIN_LENGTH, PANIC, PLAIN_TEXT_LIMIT, WORK_FLOOR,
    WORK_PER_BYTE,
};
pub use decode::{
    memory_term, DeflateDecoder, MEMORY_ALLOWANCE, MEMORY_PER_CORRECTION_BYTE,
    MEMORY_PER_PLAIN_BYTE,
};
pub use part::{PartEncoded, PartEncoder, PartModel};
pub use pool::PoolSummary;
pub use stage::{DeflatePeel, DeflateSummary, FileStreams, Outcomes, Waste, MAX_FILE};
pub use walk::{find_streams, trim_spans, Candidate, Found, INFLATE_CAP, KINDS, MIN_SCANNED_PLAIN};
/// The pinned `preflate-rs` version (Cargo.toml pins it exactly).
pub const PREFLATE_VERSION: &str = "0.7.6";

#[cfg(test)]
mod testkit;

#[cfg(test)]
mod tests {
    #[test]
    fn the_preflate_version_is_the_manifest_pin() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let manifest = std::fs::read_to_string(path).expect("manifest");
        let pin = format!("preflate-rs = \"={}\"", super::PREFLATE_VERSION);
        assert!(
            manifest.lines().any(|l| l.trim() == pin),
            "Cargo.toml must pin `{pin}`"
        );
    }
}
