//! Peel stages: turn one input stream into a peeled stream plus a reconstruction record that
//! restores the original bit for bit (D-09: verified at compression time, fallback-first).

pub mod jpeg;

pub use jpeg::{Cause, JpegDecoder, JpegPeel, LEPTON_VERSION, MODEL_ALLOWANCE};

use lpk_format::{PrimitiveId, Record};

use crate::classify::Class;

/// A range of the input that is stored as a nested part (written plainly or through the model
/// before the peeled part, so its chunks lie in lower blocks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestedPart {
    /// Offset in the input.
    pub offset: u64,
    /// Length in bytes (not 0).
    pub len: u64,
    /// True for a secondary image (a JPEG gain map or MPF picture), false for trailing data.
    pub secondary: bool,
}

/// What a successful peel produced. The peeled part is the input's bytes `[0, primary_len)`;
/// its block holds `stream` under the graph `[primitive {record_id}]`.
#[derive(Debug, Clone)]
pub struct PeelPlan {
    /// The primitive that rebuilds the peeled part.
    pub primitive: PrimitiveId,
    /// Length of the peeled part (it starts at offset 0).
    pub primary_len: u64,
    /// The peeled stream (the block's encoded bytes).
    pub stream: Vec<u8>,
    /// The ranges after the peeled part, in file order, covering the rest of the input.
    pub nested: Vec<NestedPart>,
    /// Decoder working memory the block needs beyond its plain buffer, bytes.
    pub memory: u64,
    /// BLAKE3 of the whole input.
    pub original_hash: [u8; 32],
    /// Length of the whole input.
    pub original_len: u64,
}

/// A peel stage. `peel` either returns a verified plan or the fallback cause; the pipeline then
/// writes the input through the normal path and counts the cause.
pub trait PeelStage: std::fmt::Debug + Send + Sync {
    /// Short name, for reports.
    fn name(&self) -> &'static str;
    /// True when the stage applies to inputs of `class`.
    fn applies_to(&self, class: Class) -> bool;
    /// Peel `data`; `max_part` is the largest peeled part the writer accepts (one block).
    fn peel(&self, data: &[u8], max_part: u64) -> Result<PeelPlan, Cause>;
    /// The record of `plan`, given the chunk lists the writer gave its nested parts (same order
    /// as `plan.nested`).
    fn record(&self, plan: &PeelPlan, nested_chunks: &[Vec<u64>]) -> Record;
}

/// Files and input bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Count {
    /// Files.
    pub files: u64,
    /// Input bytes of those files.
    pub bytes: u64,
}

/// What the peel stages did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeelSummary {
    /// Inputs peeled.
    pub peeled: Count,
    /// Bytes of the peeled streams plus the nested parts of the peeled inputs (before the model).
    pub peeled_output_bytes: u64,
    /// Inputs stored through the normal path, by cause (indexed like [`Cause::ALL`]).
    pub fallbacks: [Count; Cause::ALL.len()],
}

impl PeelSummary {
    /// The count of fallbacks with `cause`.
    pub fn fallback(&self, cause: Cause) -> Count {
        self.fallbacks[cause.index()]
    }

    /// All fallbacks.
    pub fn fallback_total(&self) -> Count {
        self.fallbacks.iter().fold(Count::default(), |a, c| Count {
            files: a.files + c.files,
            bytes: a.bytes + c.bytes,
        })
    }

    pub(crate) fn note_fallback(&mut self, cause: Cause, bytes: u64) {
        let c = &mut self.fallbacks[cause.index()];
        c.files += 1;
        c.bytes += bytes;
    }

    pub(crate) fn note_peeled(&mut self, plan: &PeelPlan) {
        self.peeled.files += 1;
        self.peeled.bytes += plan.original_len;
        self.peeled_output_bytes +=
            plan.stream.len() as u64 + (plan.original_len - plan.primary_len);
    }
}
