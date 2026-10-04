//! Peel stages: turn one input stream into a peeled stream plus a reconstruction record that
//! restores the original bit for bit (D-09: verified at compression time, fallback-first).

pub mod jpeg;

pub use jpeg::{Cause, JpegDecoder, JpegPeel, LEPTON_MAX_FILE, LEPTON_VERSION, MODEL_ALLOWANCE};

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

/// Why a peel stage left an input as it is. One type for every stage: each stage's own causes
/// are mapped into it (the JPEG peel's are [`Cause`]), so the pipeline and [`PeelSummary`] do
/// not depend on any one stage's taxonomy. A later stage adds a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fallback {
    /// A cause of the JPEG peel.
    Jpeg(Cause),
}

impl Fallback {
    /// Every fallback, in report order.
    pub const ALL: [Fallback; Cause::ALL.len()] = {
        let mut all = [Fallback::Jpeg(Cause::Other); Cause::ALL.len()];
        let mut i = 0;
        while i < all.len() {
            all[i] = Fallback::Jpeg(Cause::ALL[i]);
            i += 1;
        }
        all
    };

    /// Position in [`Fallback::ALL`].
    pub fn index(self) -> usize {
        match self {
            Fallback::Jpeg(c) => c.index(),
        }
    }

    /// The name of the stage the fallback belongs to.
    pub fn stage(self) -> &'static str {
        match self {
            Fallback::Jpeg(_) => "jpeg",
        }
    }

    /// The label, for reports.
    pub fn label(self) -> &'static str {
        match self {
            Fallback::Jpeg(c) => c.label(),
        }
    }
}

impl From<Cause> for Fallback {
    fn from(c: Cause) -> Self {
        Fallback::Jpeg(c)
    }
}

impl PartialEq<Cause> for Fallback {
    fn eq(&self, other: &Cause) -> bool {
        *self == Fallback::Jpeg(*other)
    }
}

/// A peel stage. `peel` either returns a verified plan or the fallback cause; the pipeline then
/// writes the input through the normal path and counts the cause.
pub trait PeelStage: std::fmt::Debug + Send + Sync {
    /// Short name, for reports.
    fn name(&self) -> &'static str;
    /// True when the stage applies to inputs of `class`.
    fn applies_to(&self, class: Class) -> bool;
    /// The fallback for an input of `len` bytes that the stage refuses before any byte of it is
    /// read (the pipeline then streams it through the normal path); `None` to read it.
    fn refuse_unread(&self, len: u64) -> Option<Fallback> {
        let _ = len;
        None
    }
    /// Peel `data`; `max_part` is the largest peeled part the writer accepts (one block).
    fn peel(&self, data: &[u8], max_part: u64) -> Result<PeelPlan, Fallback>;
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
    /// Inputs stored through the normal path, by cause (indexed like [`Fallback::ALL`]).
    pub fallbacks: [Count; Fallback::ALL.len()],
}

impl PeelSummary {
    /// The count of fallbacks with `cause`.
    pub fn fallback(&self, cause: impl Into<Fallback>) -> Count {
        self.fallbacks[cause.into().index()]
    }

    /// All fallbacks.
    pub fn fallback_total(&self) -> Count {
        self.fallbacks.iter().fold(Count::default(), |a, c| Count {
            files: a.files + c.files,
            bytes: a.bytes + c.bytes,
        })
    }

    pub(crate) fn note_fallback(&mut self, cause: Fallback, bytes: u64) {
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
