//! Memory budgets (E2-20): the ceiling a tier's whole process may use while compressing or
//! extracting, and the blocks in flight the writer derives from it.
//!
//! **Budgets.** A tier's budget is `min(ceiling, half of the machine's physical memory)` by
//! default, with the ceilings [`STORE_CEILING`], [`FAST_CEILING`] and [`BALANCED_CEILING`].
//! `--memory` replaces the default.
//!
//! **Extraction** (ruling R19, amending D-67 (4)): the budget defaults to the ceiling of the tier
//! the archive's envelope shows ([`EnvelopeTier::of`]), resolved as above: a store-only envelope
//! (`max_window` 0, `max_bwt_block` 0, `decode_memory` equal to `max_block_plain`) is the store
//! tier's; an envelope with a BWT block, a block above [`FAST_MAX_BLOCK_PLAIN`] or a window above
//! [`FAST_MAX_WINDOW`] (more than the Fast tier's default settings, the CLI's, produce) is the
//! Balanced tier's; any other the Fast tier's. The thresholds come from the tiers' defaults: an
//! archive built through the library with other settings may be classed otherwise, which is
//! harmless because what the engine counts comes from the envelope, not from the class (only
//! the default budget does). A peeled archive follows the same rule on its envelope. The reader
//! opened for `lpk x` and `lpk t` is limited before the envelope is known, by `--memory` or by
//! [`EXTRACT_CEILING`] resolved likewise. What extraction counts against the budget is in
//! [`crate::extract`].
//!
//! **The raised extraction default** (E2-25). An archive whose largest declared decoder term
//! (the envelope's `decode_memory` beyond `max_block_plain`) cuts the decode workers below the
//! thread budget's decoders (D-95) at the default above gets a larger default instead
//! ([`BudgetSource::Raised`]): `fixed + decoders * (per_block + per_worker) + held + 2 *
//! declared`, at most [`EXTRACT_CEILING`] and at most half the physical memory (no raise when
//! the physical memory is unknown), never below the tier's default. That is what every decoder
//! counts at its block terms, one decoded block held, the largest need in the decode pool, and
//! as much again so that the need shares the pool with the decodes beside it instead of
//! waiting for it to empty (D-131 (2)). Only a cut the declared need causes raises (R122): when
//! the same sizing with the declared term at the decoder floor would still cut the workers (the
//! block terms or the threads do), the tier's default stays. Within the caps the pool's cut applies as before; an
//! archive whose workers are not cut keeps the tier's default; a given `--memory` is never
//! changed. The terms come from the envelope, the index and the plan's needed blocks (no block
//! is read): [`crate::extract`]'s `Sizing::default_budget`, the one function behind
//! extraction, `lpk-api` and the GUI. An export keeps the tier's default (R123): it counts
//! every block it holds at the declared term, so the raise would mostly pay for held blocks.
//!
//! **The model.** Compressing holds:
//!
//! - per block in flight (`per_block`): the block's plain bytes (`block_size`), its encoding
//!   (the largest buffer the encoder may allocate for it: none for `store`, whose encoding is the
//!   plain buffer moved; `ZSTD_compressBound` for zstd; for the Balanced tier the larger of that
//!   and the LZMA output buffer, which is reserved once at `block + block / 64 + 64 KiB`) and
//!   one encoder working set: none for `store`; for zstd the compression context at the level
//!   and window used ([`zstd_cctx_memory`], from libzstd's parameter table and table sizes, as
//!   `ZSTD_estimateCCtxSize` computes it), one per provided dictionary more, and each provided
//!   dictionary twice (the context's copy and the prepared dictionary's); for the Balanced tier
//!   the larger of the two ways a block ends: LZMA wins the trial (liblzma's own encoder memory
//!   query for the dictionary and match finder used, plus the zstd candidate's context sized by
//!   the trial's stripe — the job drops the zstd context's tables before the full LZMA encode)
//!   or zstd wins (the zstd context at the level and window used, plus the LZMA encoder of the
//!   stripe, dropped after the trial), or, when the BWT stage is on and a `Text` cluster is
//!   present ([`BwtMemory`], revision 1.3), BWT wins (the transform phase — the suffix array of
//!   a BWT block, libsais's allowance and the transformed buffer — or the transformed buffer
//!   beside the LZMA encoder, whichever is larger; the trial's stripe likewise); the BWT path
//!   reserves its LZMA output for the transformed input;
//! - `fixed` with one block in flight (the sequential path): the writer's spare block buffer and
//!   read buffer (D-61: `block_size` and `block_size + chunk_size`), the output buffer
//!   ([`crate::store`]'s `OUT_BUF`), the peel's working set when a peel stage runs (the file it
//!   reads whole and the peeled stream, each up to its file cap, plus `lepton_jpeg`'s model
//!   allowance [`crate::peel::MODEL_ALLOWANCE`]), and the input-dependent terms: per chunk
//!   [`CHUNK_COST`] bytes (the chunk table's entry, the dedup map's entry and its overhead, the
//!   index's encoding), per entry [`ENTRY_COST`] bytes plus three times its path (the walk's
//!   input, the writer's entry and its seen-path set), with an append's old chunks and entries
//!   counted too (its hash map, records and entries are held until `finish`); and, with
//!   recovery records, the group encoder's open group, work buffer and recovery shards
//!   ([`recovery_memory`]);
//! - `fixed` with two or more blocks in flight counts the read buffer as `max(segment, the
//!   peel's file cap when a peel stage runs) + chunk_size` (the reader thread hands segments
//!   over; only a peeled file's fallback goes through the writer's own read buffer) and adds the
//!   pipeline of D-63: the pending block being filled (`block_size`), the frames queued for the write thread and the one it writes
//!   (`WRITE_DEPTH + 1` times the larger of the block and its encoding), the reader thread's read
//!   buffers (`buffers * segment` of the prefetch options) and the write thread's staging buffer.
//!   The chunk-hash threads of D-65 and the frame-hash threads of D-61 hold nothing beyond the
//!   segment and the block they hash.
//!
//! Blocks in flight: `max(1, min(threads, floor((budget - fixed_pipelined) / per_block)))` when
//! that is two or more, else one, which is the sequential path (no reader or write thread, D-63).
//! A budget below `fixed_sequential + per_block` (what one block in flight needs) is refused
//! when it was given with `--memory`; a default budget that small runs one block in flight.

use lpk_format::prefetch::PrefetchOptions;

use crate::error::CoreError;

/// 1 GiB.
const GIB: u64 = 1 << 30;
/// The store tier's ceiling.
pub const STORE_CEILING: u64 = GIB;
/// The Fast tier's ceiling.
pub const FAST_CEILING: u64 = 2 * GIB;
/// The Balanced tier's ceiling.
pub const BALANCED_CEILING: u64 = 4 * GIB;
/// The ceiling the reader of `lpk x` and `lpk t` is limited by when `--memory` is not given:
/// it is opened before its envelope is known, so this is the largest tier ceiling. The
/// extraction's own budget is the envelope's tier's ([`EnvelopeTier::of`]), raised up to this
/// figure for a large declared record need (module documentation).
pub const EXTRACT_CEILING: u64 = 4 * GIB;
/// The largest block the Fast tier writes with its default block size, `FastOptions::block_size`
/// (the format writer's `DEFAULT_BLOCK_SIZE`), the one the CLI uses: it cannot change it. A
/// threshold from the tiers' defaults, not a limit the code enforces: a library caller may set a
/// larger Fast block, and its archive is then classed Balanced; a Balanced archive with blocks
/// and dictionary within these thresholds is classed Fast. Either way is harmless: every term
/// the engine counts comes from the envelope and the index, so the class only sets the default
/// budget (and with it the workers), never what a block is counted at.
pub const FAST_MAX_BLOCK_PLAIN: u64 = lpk_format::DEFAULT_BLOCK_SIZE;
/// The largest window the Fast tier declares with its default block size: its zstd step
/// declares the smaller of the window log and the block's own log2 rounded up, so never more
/// than [`FAST_MAX_BLOCK_PLAIN`] (also with `--window-log 28`). A threshold from the defaults,
/// as [`FAST_MAX_BLOCK_PLAIN`].
pub const FAST_MAX_WINDOW: u64 = FAST_MAX_BLOCK_PLAIN;

/// The tier an archive's envelope shows, whose ceiling is the extraction's default budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvelopeTier {
    /// Every block stored: `max_window` 0, `max_bwt_block` 0, `decode_memory` equal to
    /// `max_block_plain`.
    #[default]
    Store,
    /// Blocks and windows within what the Fast tier's settings produce.
    Fast,
    /// A BWT block, a block above [`FAST_MAX_BLOCK_PLAIN`] or a window above
    /// [`FAST_MAX_WINDOW`].
    Balanced,
}

impl EnvelopeTier {
    /// The tier `env` shows.
    pub fn of(env: &lpk_format::Envelope) -> EnvelopeTier {
        if env.max_window == 0 && env.max_bwt_block == 0 && env.decode_memory == env.max_block_plain
        {
            EnvelopeTier::Store
        } else if env.max_bwt_block > 0
            || env.max_block_plain > FAST_MAX_BLOCK_PLAIN
            || env.max_window > FAST_MAX_WINDOW
        {
            EnvelopeTier::Balanced
        } else {
            EnvelopeTier::Fast
        }
    }

    /// The tier's ceiling.
    pub fn ceiling(self) -> u64 {
        match self {
            EnvelopeTier::Store => STORE_CEILING,
            EnvelopeTier::Fast => FAST_CEILING,
            EnvelopeTier::Balanced => BALANCED_CEILING,
        }
    }

    /// `store`, `fast` or `balanced`, for `-v`.
    pub fn name(self) -> &'static str {
        match self {
            EnvelopeTier::Store => "store",
            EnvelopeTier::Fast => "fast",
            EnvelopeTier::Balanced => "balanced",
        }
    }
}

/// Bytes counted per chunk: the chunk table's 40, the dedup map's 40 with the hash map's
/// control bytes, load factor and growth headroom, and the index's encoding of the entry. An
/// allowance.
pub const CHUNK_COST: u64 = 192;
/// Bytes counted per entry besides three copies of its path: the walk's `Input`, the writer's
/// `Entry` and the seen-path set's node. An allowance.
pub const ENTRY_COST: u64 = 256;
/// The zstd decoder's context beyond its window (ruzstd's block scratch, its entropy tables and
/// a prior's content), and the LZMA decoder's state beyond its dictionary (lzma-rs's probability
/// tables): an allowance for the envelope's `decode_memory`.
pub const DECODER_ALLOWANCE: u64 = 1 << 20;
/// The zstd compression context's buffers outside its match tables (the sequence store, the
/// literal buffer and the entropy workspaces for 128 KiB blocks): an allowance.
const ZSTD_SEQ_ALLOWANCE: u64 = 1 << 20;
/// The optimal parser's state of zstd's `btopt` strategies and above: an allowance.
const ZSTD_OPT_ALLOWANCE: u64 = 1 << 20;
/// The write thread's staging buffer (`lpk_format::sink`).
const STAGE: u64 = 1 << 20;

/// Where a budget came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    /// `min(ceiling, half of physical)`; `physical` is `None` when the query failed (the
    /// ceiling is then used).
    Default {
        /// The tier's ceiling.
        ceiling: u64,
        /// The machine's physical memory.
        physical: Option<u64>,
    },
    /// Raised for the largest declared record need (E2-25, module documentation): the tier's
    /// default cut the decode workers below the thread budget's decoders.
    Raised {
        /// The tier's ceiling (the default before the raise).
        ceiling: u64,
        /// The machine's physical memory.
        physical: u64,
        /// The archive's largest declared decoder term.
        need: u64,
    },
    /// Given (`--memory`).
    Given,
}

impl BudgetSource {
    /// `default` or `--memory`, for `-v`.
    pub fn name(&self) -> &'static str {
        match self {
            BudgetSource::Default { .. } | BudgetSource::Raised { .. } => "default",
            BudgetSource::Given => "--memory",
        }
    }
}

/// A memory budget in bytes and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Bytes.
    pub bytes: u64,
    /// Where it came from.
    pub source: BudgetSource,
}

impl Budget {
    /// `given` when set, else the default for `ceiling` on this machine.
    pub fn resolve(given: Option<u64>, ceiling: u64) -> Budget {
        match given {
            Some(bytes) => Budget {
                bytes,
                source: BudgetSource::Given,
            },
            None => Budget::default_for(ceiling, physical_memory()),
        }
    }

    /// `min(ceiling, physical / 2)`; the ceiling when `physical` is unknown.
    pub fn default_for(ceiling: u64, physical: Option<u64>) -> Budget {
        Budget {
            bytes: physical.map_or(ceiling, |p| ceiling.min(p / 2)),
            source: BudgetSource::Default { ceiling, physical },
        }
    }
}

/// The machine's physical memory (on Linux, capped by the cgroup's `memory.max`); `None` where
/// the query is not available (targets other than Windows and Linux) or fails.
pub fn physical_memory() -> Option<u64> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        lpk_procstat_sys::physical_memory()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

/// Parse a size: plain bytes, or a number with `K`, `M`, `G` or `T` (binary: `512M` is
/// 512 MiB), optionally followed by `B` or `iB`, in any case.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let body = lower
        .strip_suffix("ib")
        .or_else(|| lower.strip_suffix('b'))
        .unwrap_or(&lower);
    let (digits, shift) = match body.chars().last() {
        Some('k') => (&body[..body.len() - 1], 10),
        Some('m') => (&body[..body.len() - 1], 20),
        Some('g') => (&body[..body.len() - 1], 30),
        Some('t') => (&body[..body.len() - 1], 40),
        _ => (body, 0),
    };
    let n: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("{t:?} is not a size (plain bytes, or a number with K, M, G or T)"))?;
    n.checked_mul(1u64 << shift)
        .filter(|&b| b > 0)
        .ok_or_else(|| format!("{t:?} is not a size above zero that fits in 64 bits"))
}

/// The encoder working set of a tier, per block in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderMemory {
    /// Every block stored: no encoder memory; the encoding is the plain buffer.
    Store,
    /// zstd at `level` with `window_log` (the window the block declares), long-distance
    /// matching on, with `dictionaries` provided dictionaries of `dictionary_bytes` in total.
    Zstd {
        /// The level.
        level: i32,
        /// The effective window log for a full block.
        window_log: u32,
        /// Provided dictionaries (each its own compression context).
        dictionaries: u64,
        /// Their bytes.
        dictionary_bytes: u64,
    },
    /// The Balanced tier: liblzma's encoder memory query (`lzma`) or the zstd candidate, each
    /// with the other's trial-sized working set.
    Balanced {
        /// liblzma's figure for the dictionary and match finder used.
        lzma: u64,
        /// liblzma's figure for the trial's stripe (its dictionary sized to the stripe).
        lzma_sample: u64,
        /// The zstd candidate's effective window log for one trial stripe.
        zstd_sample_window_log: u32,
        /// The zstd candidate's level.
        zstd_level: i32,
        /// Its effective window log for a full block.
        zstd_window_log: u32,
        /// The BWT stage's terms (revision 1.3), `None` when the stage is off or no `Text`
        /// cluster is present (the pipeline sets it as D-69 (4) does for the Deflate peel).
        bwt: Option<BwtMemory>,
    },
}

/// The BWT stage's working set per block in flight (design `e2-15-revision-1.3-design.md`,
/// section 4, "Memory"): on a `Text` block the transform phase holds libsais's `i32` suffix
/// array (4 bytes per byte of a BWT block) beside libsais's output of it (one more) plus its allowance and the transformed buffer of
/// I(`block_size`, B) bytes; the LZMA phase holds the transformed buffer and the encoder. Every
/// BWT block is transformed before the encoder exists, so the two phases are a `max`, not a
/// sum. The trial's stripe adds the same terms at the stripe's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BwtMemory {
    /// The stage's BWT block (`BalancedOptions::bwt_block`); a block uses
    /// min(`block`, its `plain_len`).
    pub block: u64,
    /// The trial's stripe length.
    pub stripe: u64,
    /// liblzma's figure for the encoder of a transformed stripe (its dictionary sized to
    /// I(`stripe`, `stripe`), one power of two above the plain stripe's).
    pub lzma_sample: u64,
}

impl BwtMemory {
    /// The transformed input of a text of `len` bytes: I(len, min(block, len)).
    pub fn input(&self, len: u64) -> u64 {
        lpk_codecs_bwt::layout::input_len(len, self.block.min(len).max(1))
    }

    /// The transform phase of a text of `len` bytes: the suffix array of its largest BWT block
    /// (4 bytes per byte) beside libsais's own output of that block (one more byte per byte,
    /// copied into the transformed buffer afterwards), libsais's allowance and the transformed
    /// buffer: 5·min(block, len) + 1 MiB + I(len, B).
    pub fn transform(&self, len: u64) -> u64 {
        self.block
            .min(len)
            .saturating_mul(5)
            .saturating_add(lpk_codecs_bwt::layout::MEMORY_ALLOWANCE)
            .saturating_add(self.input(len))
    }
}

/// The Deflate peel's working set (design 3.2; it runs on the feeding thread, one file at a
/// time, so the whole set is a `fixed` term, never per block). The pipeline counts it only when
/// some input file is of a class the stage examines, with `max_file` the largest such file
/// within the stage's cap; a run without such files plans as without the stage. It holds the file
/// read whole (up to `max_file`) and, until the file gate decides, the kept streams' encodings, each smaller
/// than its stream, together with one multi-span stream's contiguous copy, all within one more
/// `max_file`; then the larger of the analysis and recreation of one stream (bounded by the
/// decoder's memory term for a `plain_limit` text with no corrections: the recreation the
/// analysis verifies with is the decoder's) and the encoding of one inflated text (the text
/// plus the part encoder's encoders and output, `encoder`). The records the run keeps are held by
/// the writer until `finish` and encoded into one frame there: twice `records` more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeflateMemory {
    /// The stage's file cap.
    pub max_file: u64,
    /// The largest inflated text of one stream.
    pub plain_limit: u64,
    /// The part encoder's working set for a `plain_limit` text.
    pub encoder: u64,
    /// The records cap (the stage's `RECORDS_CAP`).
    pub records: u64,
}

impl DeflateMemory {
    /// `2 * max_file + max(memory_term(plain_limit, 0), plain_limit + encoder) + 2 * records`.
    pub fn working_set(&self) -> u64 {
        let analysis = crate::peel::deflate::memory_term(self.plain_limit, 0);
        let encode = self.plain_limit.saturating_add(self.encoder);
        self.max_file
            .saturating_mul(2)
            .saturating_add(analysis.max(encode))
            .saturating_add(self.records.saturating_mul(2))
    }

    /// R of the peel pool: one stream's analysis or encode for the run's largest text,
    /// `max(memory_term(plain_limit, 0), plain_limit + encoder)`.
    pub fn reserve(&self) -> u64 {
        let analysis = crate::peel::deflate::memory_term(self.plain_limit, 0);
        analysis.max(self.plain_limit.saturating_add(self.encoder))
    }
}

/// What the model needs to know about a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInputs {
    /// Plain bytes per block.
    pub block_size: u64,
    /// The longest chunk.
    pub chunk_size: u64,
    /// The average chunk the chunker aims at (for the chunk count).
    pub avg_chunk: u64,
    /// The encoder.
    pub encoder: EncoderMemory,
    /// The peel stage's file cap (`None`: no peel).
    pub peel_max_file: Option<u64>,
    /// The Deflate peel's working set (`None`: the stage is off).
    pub deflate: Option<DeflateMemory>,
    /// Entries of the run.
    pub entries: u64,
    /// Their path bytes.
    pub path_bytes: u64,
    /// Their file bytes.
    pub input_bytes: u64,
    /// The reader thread's read-ahead.
    pub prefetch: PrefetchOptions,
    /// The recovery group encoder's memory ([`recovery_memory`]; 0 without recovery).
    pub recovery: u64,
    /// Chunks of the archive an append extends (0 for a new archive).
    pub base_chunks: u64,
}

/// The recovery group encoder's memory with `o` (0 when `o.percent` is 0): the open group's
/// data shards (`group_shards * shard_len`), the encoder's work buffer
/// ([`lpk_format::recovery::encoder_work_bytes`]) and the group's recovery shards.
pub fn recovery_memory(o: &lpk_format::RecoveryOptions) -> u64 {
    if o.percent == 0 {
        return 0;
    }
    let r = lpk_format::recovery::group_recovery_shards(o.group_shards, o.percent);
    let shard = u64::from(o.shard_len);
    (u64::from(o.group_shards) * shard)
        .saturating_add(lpk_format::recovery::encoder_work_bytes(
            o.group_shards,
            r,
            o.shard_len,
        ))
        .saturating_add(u64::from(r) * shard)
}

/// The blocks in flight a budget allows, with the figures that decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPlan {
    /// The budget.
    pub budget: Budget,
    /// One block in flight's cost.
    pub per_block: u64,
    /// The fixed cost of the sequential path.
    pub fixed_sequential: u64,
    /// The fixed cost with two or more blocks in flight.
    pub fixed_pipelined: u64,
    /// Blocks in flight: 1 is the sequential path.
    pub blocks_in_flight: usize,
    /// The bytes the Deflate peel pool may hold, P (design
    /// `docs/notes/e2-10-peel-pool-design.md`, section 2; 0: no pool).
    pub peel_pool: u64,
    /// The Deflate peel pool's workers, W = threads − max(blocks in flight, 1) (0: no pool).
    pub peel_workers: usize,
}

impl MemoryPlan {
    /// The fixed cost of the path chosen.
    pub fn fixed(&self) -> u64 {
        if self.blocks_in_flight > 1 {
            self.fixed_pipelined
        } else {
            self.fixed_sequential
        }
    }

    /// What one block in flight needs: `fixed_sequential + per_block`.
    pub fn minimum(&self) -> u64 {
        self.fixed_sequential.saturating_add(self.per_block)
    }
}

/// `ceil(log2(n))`, at least 10 (the smallest window a `zstd` step declares).
pub fn window_for(window_log: u32, block_size: u64) -> u32 {
    let need = 64 - block_size.saturating_sub(1).leading_zeros();
    window_log.min(need.max(10))
}

/// The capacity the Balanced tier's LZMA encoder reserves for a block's output, once:
/// `n + n / 64 + 64 KiB`.
pub fn lzma_out_reserve(n: u64) -> u64 {
    n + n / 64 + (64 << 10)
}

/// `ZSTD_compressBound`.
pub fn zstd_bound(n: u64) -> u64 {
    let margin = if n < (128 << 10) {
        ((128 << 10) - n) >> 11
    } else {
        0
    };
    n + (n >> 8) + margin
}

/// libzstd's compression parameters for inputs above 256 KiB (`clevels.h`, zstd 1.5.7):
/// `(window_log, chain_log, hash_log, strategy)` per level 0 to 22 (row 0 is the base of the
/// negative levels; level 0 is the default, 3). Strategies: 1 fast, 2 dfast, 3 greedy, 4 lazy,
/// 5 lazy2, 6 btlazy2, 7 btopt, 8 btultra, 9 btultra2.
const ZSTD_PARAMS: [(u32, u32, u32, u8); 23] = [
    (19, 12, 13, 1),
    (19, 13, 14, 1),
    (20, 15, 16, 1),
    (21, 16, 17, 2),
    (21, 18, 18, 2),
    (21, 18, 19, 3),
    (21, 18, 19, 4),
    (21, 19, 20, 4),
    (21, 19, 20, 5),
    (22, 20, 21, 5),
    (22, 21, 22, 5),
    (22, 21, 22, 5),
    (22, 22, 23, 5),
    (22, 22, 22, 6),
    (22, 22, 23, 6),
    (22, 23, 23, 6),
    (22, 22, 22, 7),
    (23, 23, 22, 7),
    (23, 23, 22, 8),
    (23, 24, 22, 9),
    (25, 25, 23, 9),
    (26, 26, 24, 9),
    (27, 27, 25, 9),
];

/// A zstd compression context's memory for `level` with the window `window_log` set
/// explicitly and long-distance matching on (`ldm`), as libzstd sizes it for a one-shot
/// compression: the hash table (`4 << hash_log`), the chain or binary-tree table (`4 <<
/// chain_log`, none for `fast`), the row match finder's tag table for `greedy` to `lazy2`
/// (counted as `2 << hash_log`), with `hash_log` capped at `window_log + 1` and `chain_log`
/// reduced so its cycle does not exceed the window (`ZSTD_adjustCParams`); long-distance
/// matching's hash table (`8 << (window_log - 7 + strategy / 3)`) and bucket offsets; plus the sequence store
/// and, for `btopt` and above, the optimal parser's state (allowances).
pub fn zstd_cctx_memory(level: i32, window_log: u32, ldm: bool) -> u64 {
    let row = match level {
        i32::MIN..=-1 => 0,
        0 => 3,
        l => usize::try_from(l).unwrap_or(22).min(22),
    };
    let (_, mut chain_log, mut hash_log, strategy) = ZSTD_PARAMS[row];
    hash_log = hash_log.min(window_log + 1);
    let cycle = if strategy >= 6 {
        chain_log.saturating_sub(1)
    } else {
        chain_log
    };
    if cycle > window_log {
        chain_log -= cycle - window_log;
    }
    let mut m = (4u64 << hash_log) + ZSTD_SEQ_ALLOWANCE;
    if strategy > 1 {
        m += 4u64 << chain_log;
    }
    if (3..=5).contains(&strategy) {
        m += 2u64 << hash_log;
    }
    if strategy >= 7 {
        m += ZSTD_OPT_ALLOWANCE;
    }
    if ldm {
        // `ZSTD_ldm_adjustParameters` (zstd 1.5.7): hashRateLog = 7 - strategy / 3, hashLog =
        // max(6, windowLog - hashRateLog), bucketSizeLog = clamp(strategy, 4, 8) capped at
        // hashLog; `ZSTD_ldm_getTableSize`: 8-byte entries, one bucket offset byte per
        // `2^bucketSizeLog` entries; plus the sequences of one block (an allowance).
        let rate = 7 - u32::from(strategy) / 3;
        let h = window_log.saturating_sub(rate).max(6);
        let bucket = u32::from(strategy).clamp(4, 8).min(h);
        m += (8u64 << h) + (1u64 << (h - bucket)) + (64 << 10);
    }
    m
}

/// The largest zstd block (`MAX_BLOCK_SIZE` of RFC 8878 and ruzstd).
const ZSTD_MAX_BLOCK: u64 = 128 << 10;

/// The decoder memory a zstd block declares for the envelope beyond its plain buffer: ruzstd's
/// ring buffer, which holds the window (`min(2^window_log, plain_len)`; a single-segment
/// frame's window is its content) plus what one decode step produces
/// ([`lpk_format::zstd::DECODE_STEP`] and up to one more block), its capacity rounded up to a
/// power of two plus ruzstd's slack (it grows to `next_power_of_two(required - 256 KiB) +
/// 256 KiB + 1`, which `next_power_of_two(required) + 256 KiB + 1` bounds), plus
/// [`DECODER_ALLOWANCE`]. The decoded bytes go from the ring straight
/// into the block's buffer, without an intermediate copy.
pub fn zstd_decode_memory(window_log: u32, plain_len: u64) -> u64 {
    let window = (1u64 << window_log.min(62)).min(plain_len);
    let required = window + lpk_format::zstd::DECODE_STEP as u64 + ZSTD_MAX_BLOCK;
    required.next_power_of_two() + 2 * ZSTD_MAX_BLOCK + 1 + DECODER_ALLOWANCE
}

/// The decoder memory an LZMA block declares beyond its plain buffer: the dictionary buffer
/// (`min(dict_size, plain_len)`) plus [`DECODER_ALLOWANCE`].
pub fn lzma_decode_memory(dict_size: u32, plain_len: u64) -> u64 {
    u64::from(dict_size).min(plain_len) + DECODER_ALLOWANCE
}

impl ModelInputs {
    /// The largest encoding buffer of a block.
    pub fn encoded(&self) -> u64 {
        let b = self.block_size;
        match self.encoder {
            EncoderMemory::Store => 0,
            EncoderMemory::Zstd { .. } => zstd_bound(b),
            EncoderMemory::Balanced { bwt, .. } => {
                // The BWT path's LZMA output is reserved for the transformed input.
                let lzma = lzma_out_reserve(bwt.map_or(b, |w| w.input(b)));
                zstd_bound(b).max(lzma)
            }
        }
    }

    /// One encoder working set.
    pub fn encoder_memory(&self) -> u64 {
        match self.encoder {
            EncoderMemory::Store => 0,
            EncoderMemory::Zstd {
                level,
                window_log,
                dictionaries,
                dictionary_bytes,
            } => zstd_cctx_memory(level, window_log, true)
                .saturating_mul(1 + dictionaries)
                .saturating_add(dictionary_bytes.saturating_mul(2)),
            EncoderMemory::Balanced {
                lzma,
                lzma_sample,
                zstd_sample_window_log,
                zstd_level,
                zstd_window_log,
                bwt,
            } => {
                let zstd_small = zstd_cctx_memory(zstd_level, zstd_sample_window_log, true);
                // The trial's BWT candidate on a stripe: its transform, then LZMA on the
                // transformed stripe (the stripe's suffix array is freed before).
                let sample = bwt.map_or(lzma_sample, |w| {
                    let s = w.stripe;
                    w.transform(s)
                        .max(w.input(s).saturating_add(w.lzma_sample))
                        .max(lzma_sample)
                });
                let lzma_wins = lzma.saturating_add(zstd_small);
                let zstd_wins =
                    zstd_cctx_memory(zstd_level, zstd_window_log, true).saturating_add(sample);
                // The BWT path on a `Text` block: the transform phase, then the transformed
                // buffer beside the LZMA encoder (the zstd context rebuilt small before).
                let bwt_wins = bwt.map_or(0, |w| {
                    let b = self.block_size;
                    w.transform(b)
                        .max(w.input(b).saturating_add(lzma).saturating_add(zstd_small))
                });
                lzma_wins.max(zstd_wins).max(bwt_wins)
            }
        }
    }

    /// `block_size + encoded + encoder`.
    pub fn per_block(&self) -> u64 {
        self.block_size
            .saturating_add(self.encoded())
            .saturating_add(self.encoder_memory())
    }

    /// The input-dependent terms: chunks and entries.
    pub fn inventory(&self) -> u64 {
        let chunks = self
            .input_bytes
            .div_ceil(self.avg_chunk.max(1))
            .saturating_add(self.entries)
            .saturating_add(self.base_chunks);
        chunks
            .saturating_mul(CHUNK_COST)
            .saturating_add(self.entries.saturating_mul(ENTRY_COST))
            .saturating_add(self.path_bytes.saturating_mul(3))
    }

    /// The fixed cost of the sequential path.
    pub fn fixed_sequential(&self) -> u64 {
        // The JPEG and Deflate stages handle one file at a time on the feeding thread: the
        // larger of their working sets.
        let jpeg = self.peel_max_file.map_or(0, |f| {
            f.saturating_mul(2)
                .saturating_add(crate::peel::MODEL_ALLOWANCE)
        });
        let peel = jpeg.max(self.deflate.map_or(0, |d| d.working_set()));
        let b = self.block_size;
        b.saturating_add(b.saturating_add(self.chunk_size))
            .saturating_add(crate::store::OUT_BUF as u64)
            .saturating_add(peel)
            .saturating_add(self.inventory())
            .saturating_add(self.recovery)
    }

    /// The fixed cost with two or more blocks in flight.
    pub fn fixed_pipelined(&self) -> u64 {
        let frame = self.block_size.max(self.encoded());
        let queue = frame.saturating_mul(lpk_format::writer::WRITE_DEPTH as u64 + 1);
        let reads = (self.prefetch.segment as u64).saturating_mul(self.prefetch.buffers as u64);
        // The read buffer: what the sequential path counts, replaced.
        let read_seq = self.block_size.saturating_add(self.chunk_size);
        let read_pipe = (self.prefetch.segment as u64)
            .max(self.peel_max_file.unwrap_or(0))
            .max(self.deflate.map_or(0, |d| d.max_file))
            .saturating_add(self.chunk_size);
        self.fixed_sequential()
            .saturating_sub(read_seq)
            .saturating_add(read_pipe)
            .saturating_add(self.block_size)
            .saturating_add(queue)
            .saturating_add(reads)
            .saturating_add(STAGE)
    }

    /// The blocks in flight `budget` allows with at most `threads` (see the module
    /// documentation). Refused (`InvalidOption`, naming the figure) when a given budget is below
    /// what one block in flight needs.
    pub fn plan(&self, budget: Budget, threads: usize) -> Result<MemoryPlan, CoreError> {
        let mut plan = MemoryPlan {
            budget,
            per_block: self.per_block(),
            fixed_sequential: self.fixed_sequential(),
            fixed_pipelined: self.fixed_pipelined(),
            blocks_in_flight: 1,
            peel_pool: 0,
            peel_workers: 0,
        };
        if budget.bytes < plan.minimum() && budget.source == BudgetSource::Given {
            return Err(CoreError::InvalidOption(format!(
                "--memory {} is below what one block in flight needs: {} bytes ({} per block, {} fixed)",
                budget.bytes,
                plan.minimum(),
                plan.per_block,
                plan.fixed_sequential
            )));
        }
        let by_memory = budget.bytes.saturating_sub(plan.fixed_pipelined) / plan.per_block.max(1);
        let n = usize::try_from(by_memory)
            .unwrap_or(usize::MAX)
            .min(threads.max(1));
        plan.blocks_in_flight = if n >= 2 { n } else { 1 };
        Ok(plan)
    }

    /// The peel pool's bytes, P (design `docs/notes/e2-10-peel-pool-design.md`, sections 2 and
    /// 4), with `blocks` blocks in flight: `max(P_min, budget − fixed(blocks) without the
    /// pooled peels' sets − blocks × per_block)`, where the Deflate records' term stays in
    /// `fixed`; `P_min` is the larger one-file set plus R ([`DeflateMemory::reserve`], 0
    /// without the Deflate stage): `2 × max_file` for the Deflate peel, `2 × peel_max_file +`
    /// [`crate::peel::MODEL_ALLOWANCE`] for the JPEG peel when `jpeg` (its files go through
    /// the pool).
    pub fn peel_pool(&self, budget: u64, blocks: usize, jpeg: bool) -> u64 {
        let jpeg_set = self.peel_max_file.filter(|_| jpeg).map(|f| {
            f.saturating_mul(2)
                .saturating_add(crate::peel::MODEL_ALLOWANCE)
        });
        let without = ModelInputs {
            deflate: None,
            peel_max_file: if jpeg { None } else { self.peel_max_file },
            ..*self
        };
        let fixed = if blocks > 1 {
            without.fixed_pipelined()
        } else {
            without.fixed_sequential()
        }
        .saturating_add(self.deflate.map_or(0, |d| d.records.saturating_mul(2)));
        let reserve = self.deflate.map_or(0, |d| d.reserve());
        let p_min = self
            .deflate
            .map_or(0, |d| d.max_file.saturating_mul(2))
            .max(jpeg_set.unwrap_or(0))
            .saturating_add(reserve);
        budget
            .saturating_sub(fixed)
            .saturating_sub(self.per_block().saturating_mul(blocks as u64))
            .max(p_min)
    }

    /// The envelope's `threads_hint` cap for this run: the blocks in flight the tier's
    /// `ceiling` allows for these settings with the input-dependent terms left out, so it
    /// depends neither on `--threads`, `--memory`, the machine nor the inputs' count.
    pub fn threads_hint(&self, ceiling: u64) -> u32 {
        let settings = ModelInputs {
            entries: 0,
            path_bytes: 0,
            input_bytes: 0,
            ..*self
        };
        let budget = Budget {
            bytes: ceiling,
            source: BudgetSource::Default {
                ceiling,
                physical: None,
            },
        };
        settings
            .plan(budget, usize::MAX)
            .map_or(1, |p| u32::try_from(p.blocks_in_flight).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    /// R19 (1): the envelope's tier at and around each threshold; the Fast thresholds are the
    /// Fast tier's own block size.
    #[test]
    fn the_envelope_shows_its_tier() {
        assert_eq!(
            crate::FastOptions::default().block_size,
            FAST_MAX_BLOCK_PLAIN
        );
        let env = |window: u64, bwt: u64, plain: u64, decode: u64| lpk_format::Envelope {
            max_window: window,
            max_bwt_block: bwt,
            max_block_plain: plain,
            decode_memory: decode,
            max_frame_payload: plain,
            threads_hint: 1,
        };
        let f = FAST_MAX_BLOCK_PLAIN;
        for (e, tier) in [
            (env(0, 0, f, f), EnvelopeTier::Store),
            (env(0, 0, 4 * f, 4 * f), EnvelopeTier::Store),
            (env(0, 0, f, f + 1), EnvelopeTier::Fast),
            (env(FAST_MAX_WINDOW, 0, f, 2 * f), EnvelopeTier::Fast),
            (
                env(FAST_MAX_WINDOW + 1, 0, f, 3 * f),
                EnvelopeTier::Balanced,
            ),
            (env(MIB, 0, f + 1, 2 * f), EnvelopeTier::Balanced),
            (env(0, MIB, f, 2 * f), EnvelopeTier::Balanced),
        ] {
            assert_eq!(EnvelopeTier::of(&e), tier, "{e:?}");
        }
        assert_eq!(EnvelopeTier::Store.ceiling(), STORE_CEILING);
        assert_eq!(EnvelopeTier::Fast.ceiling(), FAST_CEILING);
        assert_eq!(EnvelopeTier::Balanced.ceiling(), BALANCED_CEILING);
    }

    fn given(bytes: u64) -> Budget {
        Budget {
            bytes,
            source: BudgetSource::Given,
        }
    }

    fn inputs(block: u64, encoder: EncoderMemory) -> ModelInputs {
        ModelInputs {
            block_size: block,
            chunk_size: 512 << 10,
            avg_chunk: 64 << 10,
            encoder,
            peel_max_file: None,
            deflate: None,
            entries: 0,
            path_bytes: 0,
            input_bytes: 0,
            prefetch: PrefetchOptions::default(),
            recovery: 0,
            base_chunks: 0,
        }
    }

    #[test]
    fn recovery_append_and_dictionaries_count() {
        let mut m = inputs(
            64 * MIB,
            EncoderMemory::Zstd {
                level: 3,
                window_log: 26,
                dictionaries: 2,
                dictionary_bytes: 1000,
            },
        );
        assert_eq!(m.encoder_memory(), 3 * zstd_cctx_memory(3, 26, true) + 2000);
        let base = (m.fixed_sequential(), m.fixed_pipelined());
        let o = lpk_format::RecoveryOptions {
            percent: 5,
            ..lpk_format::RecoveryOptions::default()
        };
        let r = lpk_format::recovery::group_recovery_shards(o.group_shards, o.percent);
        let want = u64::from(o.group_shards) * u64::from(o.shard_len)
            + lpk_format::recovery::encoder_work_bytes(o.group_shards, r, o.shard_len)
            + u64::from(r) * u64::from(o.shard_len);
        assert_eq!(recovery_memory(&o), want);
        assert_eq!(recovery_memory(&lpk_format::RecoveryOptions::default()), 0);
        m.recovery = want;
        m.base_chunks = 100;
        let add = want + 100 * CHUNK_COST;
        assert_eq!(
            (m.fixed_sequential(), m.fixed_pipelined()),
            (base.0 + add, base.1 + add)
        );
    }

    #[test]
    fn the_pipelined_read_buffer_is_the_segment_or_the_peel_cap() {
        let mut m = inputs(256 * MIB, EncoderMemory::Store);
        let seq_read = 256 * MIB + (512 << 10);
        let pipe_extra = |m: &ModelInputs| m.fixed_pipelined() - m.fixed_sequential();
        let common = 256 * MIB + 2 * 256 * MIB + 32 * MIB + MIB;
        assert_eq!(pipe_extra(&m) + seq_read, common + 8 * MIB + (512 << 10));
        m.peel_max_file = Some(128 * MIB);
        assert_eq!(pipe_extra(&m) + seq_read, common + 128 * MIB + (512 << 10));
    }

    #[test]
    fn the_deflate_stage_is_fixed_and_the_larger_peel_set() {
        let mut m = inputs(256 * MIB, EncoderMemory::Store);
        m.peel_max_file = Some(128 * MIB);
        let jpeg = m.fixed_sequential();
        let d = DeflateMemory {
            max_file: 256 * MIB,
            plain_limit: 64 * MIB,
            encoder: 800 * MIB,
            records: 0,
        };
        assert_eq!(d.working_set(), 512 * MIB + 64 * MIB + 800 * MIB);
        let small = DeflateMemory { encoder: 0, ..d };
        assert_eq!(
            small.working_set(),
            512 * MIB + crate::peel::deflate::memory_term(64 * MIB, 0)
        );
        let per = m.per_block();
        m.deflate = Some(d);
        assert_eq!(m.per_block(), per, "never per block");
        let jpeg_set = 2 * 128 * MIB + crate::peel::MODEL_ALLOWANCE;
        assert_eq!(m.fixed_sequential(), jpeg - jpeg_set + d.working_set());
        let r = DeflateMemory {
            records: 64 * MIB,
            ..d
        };
        assert_eq!(r.working_set(), d.working_set() + 128 * MIB);
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("512M"), Ok(512 * MIB));
        assert_eq!(parse_size("2G"), Ok(2 << 30));
        assert_eq!(parse_size("2GiB"), Ok(2 << 30));
        assert_eq!(parse_size("128mb"), Ok(128 * MIB));
        assert_eq!(parse_size("4k"), Ok(4096));
        assert_eq!(parse_size("1000"), Ok(1000));
        assert!(parse_size("0").is_err());
        assert!(parse_size("x").is_err());
        assert!(parse_size("-1G").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn the_default_budget_is_the_ceiling_or_half_the_memory() {
        assert_eq!(
            Budget::default_for(FAST_CEILING, Some(32 << 30)).bytes,
            FAST_CEILING
        );
        assert_eq!(
            Budget::default_for(BALANCED_CEILING, Some(4 << 30)).bytes,
            2 << 30
        );
        assert_eq!(
            Budget::default_for(STORE_CEILING, None).bytes,
            STORE_CEILING
        );
        let b = Budget::resolve(None, FAST_CEILING);
        assert!(b.bytes > 0 && b.bytes <= FAST_CEILING);
        assert_eq!(Budget::resolve(Some(7), FAST_CEILING), given(7));
    }

    #[test]
    fn zstd_context_follows_libzstds_table() {
        // Level 3 at a 64 MiB window: dfast, hash 2^17, chain 2^16, LDM hash 2^19 (rate 7),
        // buckets of 2^4.
        let l3 = zstd_cctx_memory(3, 26, true);
        assert_eq!(
            l3,
            (4 << 17) + (4 << 16) + MIB + (8 << 19) + (1 << 15) + (64 << 10)
        );
        // Level 22 at 2^26 with LDM: btultra2 (9), LDM rate 7 - 3 = 4, hash 2^22 (32 MiB),
        // buckets of 2^8.
        let l22_ldm = zstd_cctx_memory(22, 26, true);
        assert_eq!(
            l22_ldm,
            (4 << 25) + (4 << 27) + 2 * MIB + (8 << 22) + (1 << 14) + (64 << 10)
        );
        // Level 22 at 2^26: btultra2, hash 2^25, chain 2^27 (cycle 26 = window), opt state.
        let l22 = zstd_cctx_memory(22, 26, false);
        assert_eq!(l22, (4 << 25) + (4 << 27) + 2 * MIB);
        // A small window caps the tables.
        assert!(zstd_cctx_memory(22, 20, false) < l22 / 32);
        assert_eq!(zstd_bound(1 << 26), (1 << 26) + (1 << 18));
        assert_eq!(window_for(27, 64 * MIB), 26);
        assert_eq!(window_for(27, 3000), 12);
        assert_eq!(window_for(27, 10), 10);
    }

    #[test]
    fn blocks_in_flight_follow_the_budget() {
        let m = inputs(
            64 * MIB,
            EncoderMemory::Zstd {
                level: 3,
                window_log: 26,
                dictionaries: 0,
                dictionary_bytes: 0,
            },
        );
        let per = m.per_block();
        let fixed = m.fixed_pipelined();
        assert_eq!(
            per,
            64 * MIB + zstd_bound(64 * MIB) + zstd_cctx_memory(3, 26, true)
        );
        // Exactly five fit.
        let p = m.plan(given(fixed + 5 * per), 24).unwrap();
        assert_eq!(p.blocks_in_flight, 5);
        assert_eq!(p.fixed(), fixed);
        assert_eq!(
            m.plan(given(fixed + 6 * per - 1), 24)
                .unwrap()
                .blocks_in_flight,
            5
        );
        // Threads cap it.
        assert_eq!(m.plan(given(u64::MAX), 3).unwrap().blocks_in_flight, 3);
        assert_eq!(m.plan(given(u64::MAX), 1).unwrap().blocks_in_flight, 1);
        // Room for one pipelined block only: the sequential path.
        let p = m.plan(given(fixed + 2 * per - 1), 24).unwrap();
        assert_eq!(p.blocks_in_flight, 1);
        assert_eq!(p.fixed(), m.fixed_sequential());
        // Exactly one block in flight's need is accepted, one byte less is refused.
        let one = m.fixed_sequential() + per;
        assert_eq!(m.plan(given(one), 24).unwrap().blocks_in_flight, 1);
        let e = m.plan(given(one - 1), 24).unwrap_err().to_string();
        assert!(e.contains(&one.to_string()), "{e}");
        // A default budget that small still runs, sequentially.
        let small = Budget::default_for(1, Some(2));
        assert_eq!(m.plan(small, 24).unwrap().blocks_in_flight, 1);
    }

    #[test]
    fn balanced_counts_lzma_and_the_zstd_candidate() {
        let m = inputs(
            256 * MIB,
            EncoderMemory::Balanced {
                lzma: 700 * MIB,
                lzma_sample: 12 * MIB,
                zstd_sample_window_log: 20,
                zstd_level: 22,
                zstd_window_log: 26,
                bwt: None,
            },
        );
        // LZMA wins: the full LZMA encoder plus the stripe-sized zstd context.
        let lzma_wins = 700 * MIB + zstd_cctx_memory(22, 20, true);
        let zstd_wins = zstd_cctx_memory(22, 26, true) + 12 * MIB;
        assert!(lzma_wins > zstd_wins);
        assert_eq!(
            m.per_block(),
            256 * MIB + (256 * MIB + 256 * MIB / 64 + (64 << 10)) + lzma_wins
        );
        // A bigger zstd context decides instead.
        let z = inputs(
            256 * MIB,
            EncoderMemory::Balanced {
                lzma: 10 * MIB,
                lzma_sample: 12 * MIB,
                zstd_sample_window_log: 20,
                zstd_level: 22,
                zstd_window_log: 26,
                bwt: None,
            },
        );
        assert_eq!(z.encoder_memory(), zstd_wins);
        let p = m
            .plan(given(m.fixed_pipelined() + 2 * m.per_block()), 24)
            .unwrap();
        assert_eq!(p.blocks_in_flight, 2);
    }

    /// The BWT stage's terms (revision 1.3): with a `Text` cluster a block in flight holds the
    /// larger of the transform phase (5 bytes per byte of a BWT block, the allowance, the
    /// transformed buffer) and the LZMA phase (the transformed buffer beside the encoder);
    /// the LZMA output is reserved for the transformed input; without `Text` nothing changes.
    #[test]
    fn balanced_counts_the_bwt_stage_on_text_blocks() {
        use lpk_codecs_bwt::layout::{input_len, MEMORY_ALLOWANCE};
        let bal = |lzma: u64, lzma_sample: u64, zwl: u32, bwt: Option<BwtMemory>| {
            EncoderMemory::Balanced {
                lzma,
                lzma_sample,
                zstd_sample_window_log: 20,
                zstd_level: 22,
                zstd_window_log: zwl,
                bwt,
            }
        };
        let w = BwtMemory {
            block: 64 * MIB,
            stripe: MIB,
            lzma_sample: 24 * MIB,
        };
        let (m, t) = (
            inputs(256 * MIB, bal(700 * MIB, 12 * MIB, 26, None)),
            inputs(256 * MIB, bal(700 * MIB, 12 * MIB, 26, Some(w))),
        );
        let i = input_len(256 * MIB, 64 * MIB);
        assert_eq!(w.input(256 * MIB), i);
        assert_eq!(w.transform(256 * MIB), 5 * 64 * MIB + MEMORY_ALLOWANCE + i);
        let lzma_phase = i + 700 * MIB + zstd_cctx_memory(22, 20, true);
        assert!(lzma_phase > w.transform(256 * MIB));
        assert_eq!(t.encoder_memory(), lzma_phase);
        assert_eq!(t.encoded(), lzma_out_reserve(i));
        assert!(t.per_block() > m.per_block());
        // A small dictionary and a small zstd window: the transform phase decides.
        let small = inputs(
            256 * MIB,
            bal(
                10 * MIB,
                MIB,
                16,
                Some(BwtMemory {
                    lzma_sample: MIB,
                    ..w
                }),
            ),
        );
        assert_eq!(small.encoder_memory(), w.transform(256 * MIB));
        // A block shorter than the BWT block is one BWT block.
        let short = BwtMemory {
            block: 64 * MIB,
            stripe: MIB,
            lzma_sample: MIB,
        };
        assert_eq!(short.input(1000), input_len(1000, 1000));
        assert_eq!(
            short.transform(1000),
            5000 + MEMORY_ALLOWANCE + input_len(1000, 1000)
        );
        // The plan at the ceiling on a machine of 8 GiB or more, with and without the stage
        // (the integration test `memory_budget` prints the real figures).
        let budget = || Budget::default_for(BALANCED_CEILING, Some(8 << 30));
        let (p, q) = (t.plan(budget(), 24).unwrap(), m.plan(budget(), 24).unwrap());
        assert!(p.blocks_in_flight >= 1 && p.blocks_in_flight <= q.blocks_in_flight);
        assert!(p.fixed() + p.per_block * p.blocks_in_flight as u64 <= BALANCED_CEILING);
    }

    #[test]
    fn the_inventory_counts_chunks_and_entries() {
        let mut m = inputs(64 * MIB, EncoderMemory::Store);
        let base = m.fixed_sequential();
        m.entries = 10;
        m.path_bytes = 100;
        m.input_bytes = 640 << 10; // ten average chunks
        assert_eq!(
            m.fixed_sequential() - base,
            20 * CHUNK_COST + 10 * ENTRY_COST + 300
        );
        // The hint ignores the inventory and the threads.
        let mut n = m;
        n.input_bytes = 1 << 40;
        assert_eq!(m.threads_hint(STORE_CEILING), n.threads_hint(STORE_CEILING));
    }

    /// ruzstd 0.9.1's ring buffer growth (`RingBuffer::reserve_amortized`,
    /// `src/decoding/ringbuffer.rs`), restated from its source: the capacity `reserve(amount)`
    /// leaves when `free < amount` (one slot is the sentinel).
    fn ruzstd_reserve(cap: usize, len: usize, amount: usize) -> usize {
        const SLACK: usize = 2 * 128 * 1024;
        let free = cap.saturating_sub(len + 1);
        if free >= amount {
            return cap;
        }
        let required = cap + (amount - free);
        if required <= SLACK {
            cap.next_power_of_two().max(required.next_power_of_two()) + 1
        } else {
            (required - SLACK).next_power_of_two() + SLACK + 1
        }
    }

    /// The ring's capacity over one frame as `DecodeBuffer` drives it: `reset` reserves the
    /// window, then each step pushes up to `DECODE_STEP` plus one block before the caller
    /// drains down to the window.
    fn ruzstd_peak(window: usize) -> usize {
        let step = lpk_format::zstd::DECODE_STEP + 128 * 1024;
        let mut cap = ruzstd_reserve(0, 0, window);
        let mut len = 0;
        for _ in 0..(window / step + 8) {
            cap = ruzstd_reserve(cap, len, step);
            len = (len + step).min(window);
        }
        cap
    }

    #[test]
    fn the_zstd_figure_covers_ruzstds_ring_growth() {
        for (w, plain) in [
            (20u32, 1u64 << 20),
            (20, (1 << 20) + 1),
            (26, 64 * MIB),
            (26, 64 * MIB - 4096),
            (27, 64 * MIB),
            (28, 129 * MIB),
            (17, 3000),
            (12, 3000),
            (22, 1 << 30),
        ] {
            let window = usize::try_from((1u64 << w).min(plain)).unwrap();
            let ring = ruzstd_peak(window) as u64;
            let declared = zstd_decode_memory(w, plain) - DECODER_ALLOWANCE;
            assert!(
                ring <= declared,
                "w {w}, plain {plain}: ring {ring} > {declared}"
            );
        }
    }

    #[test]
    fn decoder_memory_figures() {
        let r: u64 = (1 << 20) + (64 << 10) + (128 << 10);
        let slack = (256 << 10) + 1 + DECODER_ALLOWANCE;
        assert_eq!(
            zstd_decode_memory(20, 1 << 26),
            r.next_power_of_two() + slack
        );
        assert_eq!(
            zstd_decode_memory(26, 1000),
            (1000u64 + (192 << 10)).next_power_of_two() + slack
        );
        assert_eq!(lzma_decode_memory(1 << 26, 5000), 5000 + DECODER_ALLOWANCE);
    }
}
