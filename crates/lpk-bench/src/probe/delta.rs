//! `probe delta` (E2-9 task A, design `docs/notes/e2-9-design.md` section 5): the measured
//! pre-gate of the delta stage on the versioned classes (`backup-versions`,
//! `backup-versions-large`).
//!
//! **Pairing** (section 1, the writer's own code: `lpk_core::fold::versions`): the version roots
//! are the class's top-level folders (the one input's children when the class folder is
//! archived); two are siblings when at least half of the smaller one's files have the same
//! relative path in the other; connected sibling sets are groups, ranked in natural order. Files
//! directly in the class folder belong to no root and are left out.
//!
//! **Walk.** Per group, the relative paths in byte order; per path, its versions in rank order
//! (the order `--ordering versions` writes). Each version of a file is cut by FastCDC as the
//! writer cuts it (the 2020 variant, 4 KiB / 64 KiB / 512 KiB, per file) and every chunk sorted:
//! *unchanged* (its BLAKE3 was seen earlier in the walk: the writer's dedup), *changed* (not
//! seen, and the file has a base file) or *new* (not seen, no base file, or an empty one). Rank 0
//! is the base and is not reported.
//!
//! **Rules.** Pairing P1 (the nearest lower rank that has the path; a candidate base chunk that
//! was itself derived under the same rule is replaced by its own base: depth 1) and P2 (the
//! lowest rank that has the path); base B1 (the base file's chunk whose span covers the changed
//! chunk's offset; past the base's end, its last chunk) and B2 (B1 and its two neighbours, the
//! smallest coded patch wins); format F1 (`lpk_core::delta`, the suffix-array patch, coded alone
//! by the tier's coder, stored when that is not smaller) and F0 (a zstd level-19 frame against
//! the base as a raw prefix, `ref_prefix`: format 0's estimate, not re-coded). Every chunk of
//! every rule is derived (an appended generation's case, D-59). Every patch is applied and
//! compared with the chunk: F1 by `lpk_core::delta::apply`, F0 by libzstd; F0 also through
//! `ruzstd` 0.9.1 (the in-tree decoder) with the base as its dictionary content, the outcome
//! recorded as the `ruzstd` fact (a probe failure only for F1 or libzstd).
//!
//! **The tier's coder** is the Balanced tier's LZMA (raw LZMA1, preset 9 extreme, BT4, nice 273,
//! lc 3 lp 0 pb 2, the dictionary min(64 MiB, the input's next power of two)), the sizes taken
//! here through liblzma directly (one stream per input).
//!
//! **Per version v** (formulas' inputs): Y_v = the sum of the coded patches, n_v the derived
//! chunks, the create and apply seconds (single-threaded, B2 counting all three candidates);
//! X_v = the coder over the changed chunks in walk order; X_v^adj = the coder over each changed
//! chunk right after its P1/B1 base (no substitution: the ordering alone) minus the coder over
//! those bases alone; the coder over v's new chunks alone (for A_v = Y_v + n_v * O_block + it);
//! and the sum of each changed chunk coded alone (for the ratio r of section 2).
//!
//! **O_block** = 86 bytes: a one-chunk derived block's frame (kind and flags 4, `payload_len` 3,
//! hash 32), header (graph `[lzma, delta]` 23, `plain_len` 3, `encoded_len` 3) and index row
//! (`frame_offset` 5, `frame_len` 3, `first_chunk` 3, `chunk_count` 1, `plain_len` 3, `sequence`
//! 3), the varints at the sizes of an archive under 32 GiB with chunks under 2 MiB.
//!
//! **The pre-gate** (with `--rows DIR`, repeatable: results directories of the same manifest; a
//! row of the class from another manifest is refused): S_bal = the `lpk/balanced` row, B = the
//! best row among the other tools without `dedup = true`; S_est(rule) = S_bal - sum X_v +
//! sum (Y_v + n_v O_block); S_ord = S_bal - sum X_v + sum X_v^adj; mu = 0.02 B; the decision: (i)
//! S_ord <= 0.5 B: the ordering alone meets G2; (ii) else the writer (task C) is built when the
//! smallest S_est <= S_ord - mu (D-91's amendment: 0.5 B is reported, not a build condition);
//! (iii) else both are reported to the owner.
//!
//! Memory: one path's versions at a time, a suffix array per base chunk (never per file), one
//! LZMA encoder at a time for the concatenations (they go through scratch files).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use fastcdc::v2020::FastCDC;
use liblzma::stream::{Filters, LzmaOptions, MatchFinder, Mode, Stream, PRESET_EXTREME};
use lpk_core::delta;
use lpk_core::fold::versions::Versions;
use lpk_core::fold::{AVG_CHUNK, MAX_CHUNK, MIN_CHUNK};
use serde::{Deserialize, Serialize};

use super::dedup::{VERSIONS_CLASS, VERSIONS_LARGE_CLASS};
use super::{mbps, md_header, md_table, par_map, timed, Ctx, Envelope, Output};
use crate::corpus::manifest::ManifestFile;
use crate::run::result::{ToolResult, ToolsFile};

pub const NAME: &str = "delta";
/// The Balanced tier's configured dictionary.
pub const DICT: u32 = 64 << 20;
/// The per-block constant (see the module documentation).
pub const O_BLOCK: u64 = 86;
/// The margin's fraction of B (D-NN (1), the owner's to change).
pub const MARGIN_FRACTION: f64 = 0.02;
/// zstd level of format 0's estimate.
pub const ZSTD_LEVEL: i32 = 19;
/// Tools the catalogue flags `dedup = true`, for a `tools.json` without the field.
const DEDUP_TOOLS: [&str; 2] = ["zpaqfranz", "tsaur"];
/// The rules, in the order of every `rules` list: (pairing, base, format).
pub const RULES: [(&str, &str, &str); 8] = [
    ("P1", "B1", "F1"),
    ("P1", "B1", "F0"),
    ("P1", "B2", "F1"),
    ("P1", "B2", "F0"),
    ("P2", "B1", "F1"),
    ("P2", "B1", "F0"),
    ("P2", "B2", "F1"),
    ("P2", "B2", "F0"),
];

/// The decision strings.
pub const DECISION_ORDERING: &str = "(i) the ordering alone meets G2: B, C, D are not built";
pub const DECISION_WRITER: &str =
    "(ii) the writer is built: the best estimate beats the ordering alone by the margin";
pub const DECISION_OWNER: &str = "(iii) both estimates go to the owner";
pub const DECISION_NONE: &str = "not evaluated (no --rows with S_bal and B for this class)";

/// What `ruzstd` did with frames written against a raw prefix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuzstdFact {
    pub version: String,
    /// How it was tried.
    pub method: String,
    /// The crafted pair (an edited text against its original) decoded bit-exactly.
    pub crafted_decoded: bool,
    /// Format-0 patches of the run tried and decoded bit-exactly.
    pub corpus_tried: u64,
    pub corpus_decoded: u64,
    /// `yes` when the crafted pair and every corpus patch decoded bit-exactly, else `no`.
    pub outcome: String,
    /// The first failure, when there was one.
    pub first_failure: Option<String>,
}

/// One version root of a group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    /// The folder name (`root-<n>` for a private corpus, n counting roots in name order).
    pub label: String,
    pub group: u64,
    pub rank: u64,
    pub files: u64,
    pub bytes: u64,
}

/// One rule's result on one version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleRow {
    pub pairing: String,
    pub base: String,
    pub format: String,
    /// n_v: derived chunks (every changed chunk).
    pub derived: u64,
    /// Plain bytes of those chunks.
    pub derived_bytes: u64,
    /// Patch bytes before the coder (F1) or the frames (F0).
    pub patch_bytes: u64,
    /// Y_v.
    pub coded_bytes: u64,
    /// Patches applied and equal to their chunk (must equal `derived`).
    pub verified: u64,
    /// Chunks whose coded patch plus O_block is not smaller than the chunk coded alone.
    pub not_smaller: u64,
    /// A P1 candidate replaced by its own base (depth 1).
    pub substituted: u64,
    pub create_seconds: f64,
    pub apply_seconds: f64,
    /// A_v = Y_v + n_v O_block + the coder over the version's new chunks alone.
    pub append_estimate: u64,
}

/// One later version (rank >= 1) of a group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionRow {
    pub group: u64,
    pub rank: u64,
    pub label: String,
    pub files: u64,
    /// Files with a base file (P1).
    pub paired_files: u64,
    pub chunks: u64,
    pub unchanged_chunks: u64,
    pub unchanged_bytes: u64,
    pub changed_chunks: u64,
    pub changed_bytes: u64,
    pub new_chunks: u64,
    pub new_bytes: u64,
    /// X_v.
    pub changed_coded: u64,
    /// The coder over (P1/B1 base, chunk) pairs, and over the bases alone.
    pub adjacent_coded: u64,
    pub bases_coded: u64,
    /// X_v^adj = adjacent_coded - bases_coded (0 when negative).
    pub changed_adjacent: u64,
    /// The coder over the new chunks alone.
    pub new_coded: u64,
    /// The sum of every changed chunk coded alone.
    pub changed_alone_coded: u64,
    pub rules: Vec<RuleRow>,
}

/// The rows the pre-gate reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateRows {
    /// The directories read (their last path component).
    pub dirs: Vec<String>,
    /// `lpk/balanced` on the class.
    pub s_bal: Option<u64>,
    /// The best row among the other tools without `dedup = true`, and which it is.
    pub b: Option<u64>,
    pub b_row: Option<String>,
}

/// One rule's estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Estimate {
    pub pairing: String,
    pub base: String,
    pub format: String,
    pub s_est: u64,
}

/// The formulas on one class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    pub s_bal: u64,
    pub b: u64,
    pub sum_changed_coded: u64,
    pub sum_changed_adjacent: u64,
    pub s_ord: u64,
    pub estimates: Vec<Estimate>,
    pub margin: u64,
    pub half_b: u64,
    pub decision: String,
}

/// One versioned class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRow {
    pub class: String,
    pub present: bool,
    pub roots: Vec<Root>,
    /// Files in no group root (left out).
    pub files_outside: u64,
    pub versions: Vec<VersionRow>,
    pub rows: Option<GateRows>,
    pub gate: Option<Gate>,
    pub decision: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub chunking: String,
    pub coder: String,
    pub o_block: u64,
    pub o_block_breakdown: String,
    pub margin_fraction: f64,
    pub zstd_level: i32,
    pub ruzstd: RuzstdFact,
    pub classes: Vec<ClassRow>,
}

// ---------------------------------------------------------------------------------------------
// The coders

fn dict_for(len: u64) -> u32 {
    len.next_power_of_two().clamp(4096, u64::from(DICT)) as u32
}

struct Count(u64);

impl Write for Count {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The Balanced tier's raw LZMA stream size for `len` bytes read from `r` (0 for none).
pub fn lzma_size_of(mut r: impl Read, len: u64) -> Result<u64> {
    if len == 0 {
        return Ok(0);
    }
    let mut o = LzmaOptions::new_preset(9 | PRESET_EXTREME)?;
    o.dict_size(dict_for(len))
        .literal_context_bits(3)
        .literal_position_bits(0)
        .position_bits(2)
        .nice_len(273)
        .match_finder(MatchFinder::BinaryTree4)
        .mode(Mode::Normal);
    let mut filters = Filters::new();
    filters.lzma1(&o);
    let stream = Stream::new_raw_encoder(&filters)?;
    let mut enc = liblzma::write::XzEncoder::new_stream(Count(0), stream);
    std::io::copy(&mut r, &mut enc)?;
    Ok(enc.finish()?.0)
}

/// [`lzma_size_of`] over bytes in memory.
pub fn lzma_size(b: &[u8]) -> Result<u64> {
    lzma_size_of(b, b.len() as u64)
}

/// A patch coded alone: the coder's output, or the patch stored when that is not smaller.
fn coded(patch: &[u8]) -> Result<u64> {
    Ok(lzma_size(patch)?.min(patch.len() as u64))
}

/// Format 0: one zstd frame of `new` against `base` as a raw prefix.
pub fn zstd_patch(base: &[u8], new: &[u8]) -> Result<Vec<u8>> {
    let mut e = zstd::stream::write::Encoder::with_ref_prefix(Vec::new(), ZSTD_LEVEL, base)?;
    e.write_all(new)?;
    Ok(e.finish()?)
}

/// Format 0 applied by libzstd.
pub fn zstd_apply(base: &[u8], frame: &[u8]) -> Result<Vec<u8>> {
    let mut d = zstd::stream::read::Decoder::with_ref_prefix(frame, base)?;
    let mut out = Vec::new();
    d.read_to_end(&mut out)?;
    Ok(out)
}

/// A formatted zstd dictionary from libzstd's trainer over deterministic text: `ruzstd` has
/// no raw-content constructor, so its `Dictionary` is parsed from this and its content and
/// repeat offsets replaced.
pub fn ruzstd_template() -> Result<Vec<u8>> {
    let words = [
        "alpha ", "bravo ", "charlie ", "delta ", "echo ", "foxtrot ", "golf ", "hotel\n",
    ];
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let samples: Vec<Vec<u8>> = (0..500)
        .map(|_| {
            let mut s = Vec::new();
            while s.len() < 600 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                s.extend_from_slice(words[(x % 8) as usize].as_bytes());
            }
            s
        })
        .collect();
    zstd::dict::from_samples(&samples, 2048).context("training the ruzstd template dictionary")
}

/// Format 0 decoded by `ruzstd` 0.9.1: a `Dictionary` whose content is the base and whose
/// repeat offsets are the frame defaults (1, 4, 8), forced onto a frame without a dictionary ID.
pub fn ruzstd_apply(template: &[u8], base: &[u8], frame: &[u8]) -> Result<Vec<u8>, String> {
    use ruzstd::decoding::{BlockDecodingStrategy, Dictionary, FrameDecoder};
    const ID: u32 = 0x4C50_4B31;
    let mut d = Dictionary::decode_dict(template).map_err(|e| e.to_string())?;
    d.id = ID;
    d.dict_content = base.to_vec();
    d.offset_hist = [1, 4, 8];
    let mut dec = FrameDecoder::new();
    dec.add_dict(d).map_err(|e| e.to_string())?;
    let mut src = frame;
    dec.reset(&mut src).map_err(|e| e.to_string())?;
    dec.force_dict(ID).map_err(|e| e.to_string())?;
    dec.decode_blocks(&mut src, BlockDecodingStrategy::All)
        .map_err(|e| e.to_string())?;
    dec.collect().ok_or_else(|| "no output".to_string())
}

/// The crafted pair of the `ruzstd` fact: decoded bit-exactly?
pub fn ruzstd_crafted(template: &[u8]) -> Result<bool> {
    let base: Vec<u8> = (0..4000u32)
        .flat_map(|i| format!("line {i}: the quick brown fox {}\n", i * 7 % 13).into_bytes())
        .collect();
    let mut new = base.clone();
    new.splice(
        1000..1000,
        b"an insertion that is not in the base".iter().copied(),
    );
    new.drain(20_000..20_500);
    for b in &mut new[40_000..40_100] {
        *b = b.wrapping_add(1);
    }
    let frame = zstd_patch(&base, &new)?;
    if zstd_apply(&base, &frame)? != new {
        bail!("libzstd does not decode its own ref_prefix frame");
    }
    Ok(ruzstd_apply(template, &base, &frame).is_ok_and(|o| o == new))
}

// ---------------------------------------------------------------------------------------------
// The rows of the pre-gate

/// Read the rows of `class` in `dirs`; a row of the class from another manifest is an error.
pub fn read_rows(dirs: &[PathBuf], class: &str, manifest: &str) -> Result<GateRows> {
    let mut rows = GateRows {
        dirs: Vec::new(),
        s_bal: None,
        b: None,
        b_row: None,
    };
    for dir in dirs {
        rows.dirs.push(
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        let dedup: HashMap<String, Option<bool>> =
            match std::fs::read_to_string(dir.join("tools.json")) {
                Ok(t) => serde_json::from_str::<ToolsFile>(&t)
                    .with_context(|| format!("{}: tools.json", dir.display()))?
                    .tools
                    .into_iter()
                    .map(|t| (t.id, t.dedup))
                    .collect(),
                Err(_) => HashMap::new(),
            };
        let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        names.sort();
        for p in names {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(r) = serde_json::from_str::<ToolResult>(&text) else {
                continue;
            };
            if r.class != class {
                continue;
            }
            if r.corpus.manifest_blake3 != manifest {
                bail!(
                    "{}: the row's manifest {} is not the probe's {manifest}; rows of another \
                     manifest are refused",
                    p.display(),
                    r.corpus.manifest_blake3
                );
            }
            let Some(m) = r.median else { continue };
            let bytes = m.archive_bytes;
            if r.tool.id == "lpk" {
                if r.setting.id == "balanced" {
                    rows.s_bal = Some(rows.s_bal.map_or(bytes, |s: u64| s.min(bytes)));
                }
                continue;
            }
            let is_dedup = match dedup.get(&r.tool.id).copied().flatten() {
                Some(d) => d,
                None => DEDUP_TOOLS.contains(&r.tool.id.as_str()),
            };
            if !is_dedup && rows.b.is_none_or(|b| bytes < b) {
                rows.b = Some(bytes);
                rows.b_row = Some(format!("{}/{}", r.tool.id, r.setting.id));
            }
        }
    }
    Ok(rows)
}

/// The formulas.
pub fn gate(s_bal: u64, b: u64, versions: &[VersionRow]) -> Gate {
    let sum_x: u64 = versions.iter().map(|v| v.changed_coded).sum();
    let sum_adj: u64 = versions.iter().map(|v| v.changed_adjacent).sum();
    let s_ord = s_bal.saturating_sub(sum_x).saturating_add(sum_adj);
    let estimates: Vec<Estimate> = RULES
        .iter()
        .enumerate()
        .map(|(i, (p, bb, f))| {
            let add: u64 = versions
                .iter()
                .map(|v| v.rules[i].coded_bytes + v.rules[i].derived * O_BLOCK)
                .sum();
            Estimate {
                pairing: p.to_string(),
                base: bb.to_string(),
                format: f.to_string(),
                s_est: s_bal.saturating_sub(sum_x).saturating_add(add),
            }
        })
        .collect();
    let margin = (MARGIN_FRACTION * b as f64).round() as u64;
    let half_b = b / 2;
    let best = estimates.iter().map(|e| e.s_est).min().unwrap_or(u64::MAX);
    let decision = decide(s_ord, best, margin, half_b);
    Gate {
        s_bal,
        b,
        sum_changed_coded: sum_x,
        sum_changed_adjacent: sum_adj,
        s_ord,
        estimates,
        margin,
        half_b,
        decision: decision.to_string(),
    }
}

/// The decision rule of section 5 with D-91's amendment.
pub fn decide(s_ord: u64, best_estimate: u64, margin: u64, half_b: u64) -> &'static str {
    if s_ord <= half_b {
        DECISION_ORDERING
    } else if best_estimate.saturating_add(margin) <= s_ord {
        DECISION_WRITER
    } else {
        DECISION_OWNER
    }
}

// ---------------------------------------------------------------------------------------------
// The walk

/// One version of one file in memory.
struct FileVer {
    data: Vec<u8>,
    /// (offset, length) of every chunk.
    chunks: Vec<(usize, usize)>,
    hashes: Vec<[u8; 32]>,
    /// Per rule (P1 entries only), the base each chunk was derived from: (rank, chunk).
    derived: Vec<Vec<Option<(usize, usize)>>>,
}

impl FileVer {
    fn chunk(&self, i: usize) -> &[u8] {
        let (o, l) = self.chunks[i];
        &self.data[o..o + l]
    }
    /// The chunk whose span covers `offset` (past the end: the last one).
    fn covering(&self, offset: usize) -> Option<usize> {
        if self.chunks.is_empty() {
            return None;
        }
        Some(
            self.chunks
                .iter()
                .position(|&(o, l)| offset < o + l)
                .unwrap_or(self.chunks.len() - 1),
        )
    }
}

fn cut(data: Vec<u8>) -> FileVer {
    let chunks: Vec<(usize, usize)> = FastCDC::new(&data, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK)
        .map(|c| (c.offset, c.length))
        .collect();
    let hashes = chunks
        .iter()
        .map(|&(o, l)| *blake3::hash(&data[o..o + l]).as_bytes())
        .collect();
    let n = chunks.len();
    FileVer {
        data,
        chunks,
        hashes,
        derived: vec![vec![None; n]; RULES.len()],
    }
}

/// A candidate base (rank, chunk) and whether it replaced a derived one.
type Pick = ((usize, usize), bool);

/// A patch against one candidate base chunk.
struct Cand {
    f1_raw: u64,
    f1_coded: u64,
    f1_create: f64,
    f1_apply: f64,
    f0_bytes: u64,
    f0_create: f64,
    f0_apply: f64,
}

/// Scratch files of one later version.
struct Sinks {
    changed: BufWriter<std::fs::File>,
    adjacent: BufWriter<std::fs::File>,
    bases: BufWriter<std::fs::File>,
    new: BufWriter<std::fs::File>,
    lens: [u64; 4],
    paths: [PathBuf; 4],
}

impl Sinks {
    fn open(dir: &Path, tag: &str) -> Result<Sinks> {
        let paths = ["changed", "adjacent", "bases", "new"].map(|k| dir.join(format!("{tag}-{k}")));
        let open = |p: &Path| -> Result<BufWriter<std::fs::File>> {
            Ok(BufWriter::new(
                std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?,
            ))
        };
        Ok(Sinks {
            changed: open(&paths[0])?,
            adjacent: open(&paths[1])?,
            bases: open(&paths[2])?,
            new: open(&paths[3])?,
            lens: [0; 4],
            paths,
        })
    }

    /// Flush and code the four files: (X_v, adjacent, bases, new).
    fn code(mut self) -> Result<[u64; 4]> {
        self.changed.flush()?;
        self.adjacent.flush()?;
        self.bases.flush()?;
        self.new.flush()?;
        drop((self.changed, self.adjacent, self.bases, self.new));
        let mut out = [0u64; 4];
        for (i, p) in self.paths.iter().enumerate() {
            let f = std::fs::File::open(p)?;
            out[i] = lzma_size_of(BufReader::new(f), self.lens[i])?;
            let _ = std::fs::remove_file(p);
        }
        Ok(out)
    }
}

/// The ruzstd tallies of a run.
struct Ruz {
    template: Vec<u8>,
    tried: u64,
    decoded: u64,
    first_failure: Option<String>,
}

/// One class's group walk: the version rows.
fn walk_class(
    ctx: &Ctx<'_>,
    class: &str,
    versions: &Versions,
    roots: &BTreeMap<String, Vec<(String, &ManifestFile)>>,
    ruz: &mut Ruz,
    labels: &HashMap<String, String>,
) -> Result<Vec<VersionRow>> {
    let scratch = ctx.scratch_dir(&format!("delta-{class}"))?;
    let mut rows: Vec<VersionRow> = Vec::new();
    for g in 0..versions.group_count() {
        let ranks: Vec<&str> = versions
            .roots
            .iter()
            .filter(|r| r.group == g)
            .map(|r| r.name.as_str())
            .collect();
        // Per relative path, its file in each rank.
        let mut paths: BTreeMap<&str, Vec<Option<&ManifestFile>>> = BTreeMap::new();
        for (k, name) in ranks.iter().enumerate() {
            for (rel, f) in roots.get(*name).map(Vec::as_slice).unwrap_or(&[]) {
                paths
                    .entry(rel.as_str())
                    .or_insert_with(|| vec![None; ranks.len()])[k] = Some(f);
            }
        }
        let mut seen: HashSet<[u8; 32]> = HashSet::new();
        let mut vrows: Vec<VersionRow> = (1..ranks.len())
            .map(|k| VersionRow {
                group: g as u64,
                rank: k as u64,
                label: labels.get(ranks[k]).cloned().unwrap_or_default(),
                files: 0,
                paired_files: 0,
                chunks: 0,
                unchanged_chunks: 0,
                unchanged_bytes: 0,
                changed_chunks: 0,
                changed_bytes: 0,
                new_chunks: 0,
                new_bytes: 0,
                changed_coded: 0,
                adjacent_coded: 0,
                bases_coded: 0,
                changed_adjacent: 0,
                new_coded: 0,
                changed_alone_coded: 0,
                rules: RULES
                    .iter()
                    .map(|(p, b, f)| RuleRow {
                        pairing: p.to_string(),
                        base: b.to_string(),
                        format: f.to_string(),
                        derived: 0,
                        derived_bytes: 0,
                        patch_bytes: 0,
                        coded_bytes: 0,
                        verified: 0,
                        not_smaller: 0,
                        substituted: 0,
                        create_seconds: 0.0,
                        apply_seconds: 0.0,
                        append_estimate: 0,
                    })
                    .collect(),
            })
            .collect();
        let mut sinks: Vec<Sinks> = (1..ranks.len())
            .map(|k| Sinks::open(&scratch, &format!("g{g}-r{k}")))
            .collect::<Result<_>>()?;
        for files in paths.values() {
            let mut vers: Vec<Option<FileVer>> = Vec::with_capacity(ranks.len());
            for f in files {
                vers.push(match f {
                    Some(f) => Some(cut(ctx.read_file(class, f)?)),
                    None => None,
                });
            }
            for k in 0..ranks.len() {
                if vers[k].is_none() {
                    continue;
                }
                let p1 = (0..k).rev().find(|&j| vers[j].is_some());
                let p2 = (0..k).find(|&j| vers[j].is_some());
                let has_base =
                    p1.is_some_and(|j| vers[j].as_ref().is_some_and(|v| !v.data.is_empty()));
                if k > 0 {
                    let row = &mut vrows[k - 1];
                    row.files += 1;
                    if has_base {
                        row.paired_files += 1;
                    }
                }
                let n = vers[k].as_ref().map_or(0, |v| v.chunks.len());
                for j in 0..n {
                    let (hash, len, offset) = {
                        let v = vers[k].as_ref().expect("present");
                        (v.hashes[j], v.chunks[j].1, v.chunks[j].0)
                    };
                    let fresh = seen.insert(hash);
                    if k == 0 {
                        continue;
                    }
                    let row = &mut vrows[k - 1];
                    let sink = &mut sinks[k - 1];
                    row.chunks += 1;
                    let chunk = vers[k].as_ref().expect("present").chunk(j).to_vec();
                    if !fresh {
                        row.unchanged_chunks += 1;
                        row.unchanged_bytes += len as u64;
                        continue;
                    }
                    if !has_base {
                        row.new_chunks += 1;
                        row.new_bytes += len as u64;
                        sink.new.write_all(&chunk)?;
                        sink.lens[3] += len as u64;
                        continue;
                    }
                    row.changed_chunks += 1;
                    row.changed_bytes += len as u64;
                    sink.changed.write_all(&chunk)?;
                    sink.lens[0] += len as u64;
                    let (r1, r2) = (p1.expect("paired"), p2.expect("paired"));
                    let b1_p1 = vers[r1].as_ref().and_then(|v| v.covering(offset));
                    if let Some(i) = b1_p1 {
                        let base = vers[r1].as_ref().expect("present").chunk(i);
                        sink.adjacent.write_all(base)?;
                        sink.adjacent.write_all(&chunk)?;
                        sink.bases.write_all(base)?;
                        sink.lens[1] += (base.len() + len) as u64;
                        sink.lens[2] += base.len() as u64;
                    }
                    // Candidates per rule: (rank, chunk) after substitution, and whether one
                    // was substituted.
                    let mut wanted: Vec<Vec<((usize, usize), bool)>> = Vec::with_capacity(8);
                    for (ri, (p, b, _)) in RULES.iter().enumerate() {
                        let r = if *p == "P1" { r1 } else { r2 };
                        let base_ver = vers[r].as_ref().expect("present");
                        let Some(i) = base_ver.covering(offset) else {
                            wanted.push(Vec::new());
                            continue;
                        };
                        let mut list: Vec<usize> = vec![i];
                        if *b == "B2" {
                            if i > 0 {
                                list.push(i - 1);
                            }
                            if i + 1 < base_ver.chunks.len() {
                                list.push(i + 1);
                            }
                        }
                        let c: Vec<((usize, usize), bool)> = list
                            .into_iter()
                            .map(|i| {
                                match (*p == "P1").then(|| base_ver.derived[ri][i]).flatten() {
                                    Some(sub) => (sub, true),
                                    None => ((r, i), false),
                                }
                            })
                            .collect();
                        wanted.push(c);
                    }
                    let distinct: Vec<(usize, usize)> = {
                        let mut d: Vec<(usize, usize)> =
                            wanted.iter().flatten().map(|(c, _)| *c).collect();
                        d.sort_unstable();
                        d.dedup();
                        d
                    };
                    // Patches, timed one at a time, each applied and compared.
                    let mut f1_patches: Vec<Vec<u8>> = Vec::with_capacity(distinct.len());
                    let mut cands: Vec<Cand> = Vec::with_capacity(distinct.len());
                    for &(r, i) in &distinct {
                        let base = vers[r].as_ref().expect("present").chunk(i);
                        let (p, f1_create) = timed(|| delta::encode(base, &chunk));
                        let p = p.map_err(|e| anyhow!("suffix array: {e}"))?;
                        let (back, f1_apply) = timed(|| delta::apply(base, &p));
                        if back.as_deref() != Ok(chunk.as_slice()) {
                            bail!("class `{class}`: a format-1 patch does not apply back");
                        }
                        let (frame, f0_create) = timed(|| zstd_patch(base, &chunk));
                        let frame = frame?;
                        let (back, f0_apply) = timed(|| zstd_apply(base, &frame));
                        if back? != chunk {
                            bail!("class `{class}`: a format-0 patch does not apply back");
                        }
                        ruz.tried += 1;
                        match ruzstd_apply(&ruz.template, base, &frame) {
                            Ok(o) if o == chunk => ruz.decoded += 1,
                            Ok(_) => {
                                ruz.first_failure.get_or_insert("different bytes".into());
                            }
                            Err(e) => {
                                ruz.first_failure.get_or_insert(e);
                            }
                        }
                        cands.push(Cand {
                            f1_raw: p.len() as u64,
                            f1_coded: 0,
                            f1_create,
                            f1_apply,
                            f0_bytes: frame.len() as u64,
                            f0_create,
                            f0_apply,
                        });
                        f1_patches.push(p);
                    }
                    // The coder's sizes (sizes only: in parallel), and the chunk alone.
                    let mut jobs: Vec<&[u8]> = f1_patches.iter().map(Vec::as_slice).collect();
                    jobs.push(&chunk);
                    let last = jobs.len() - 1;
                    let sizes = par_map(&jobs, ctx.threads as usize, |i, b| {
                        if i == last {
                            lzma_size(b)
                        } else {
                            coded(b)
                        }
                    });
                    let mut sizes = sizes.into_iter().collect::<Result<Vec<u64>>>()?;
                    let alone = sizes.pop().unwrap_or(0);
                    for (c, s) in cands.iter_mut().zip(sizes) {
                        c.f1_coded = s;
                    }
                    row.changed_alone_coded += alone;
                    for (ri, (_, _, f)) in RULES.iter().enumerate() {
                        let list = &wanted[ri];
                        if list.is_empty() {
                            continue;
                        }
                        let size = |c: &Cand| if *f == "F1" { c.f1_coded } else { c.f0_bytes };
                        let mut best: Option<(usize, &Pick)> = None;
                        let (mut create, mut apply) = (0.0, 0.0);
                        for item in list {
                            let ci = distinct.binary_search(&item.0).expect("listed");
                            let c = &cands[ci];
                            if *f == "F1" {
                                create += c.f1_create;
                                apply += c.f1_apply;
                            } else {
                                create += c.f0_create;
                                apply += c.f0_apply;
                            }
                            if best.is_none_or(|(b, _)| size(c) < size(&cands[b])) {
                                best = Some((ci, item));
                            }
                        }
                        let (ci, item) = best.expect("a candidate");
                        let c = &cands[ci];
                        let rr = &mut row.rules[ri];
                        rr.derived += 1;
                        rr.derived_bytes += len as u64;
                        rr.patch_bytes += if *f == "F1" { c.f1_raw } else { c.f0_bytes };
                        rr.coded_bytes += size(c);
                        rr.verified += 1;
                        if size(c) + O_BLOCK >= alone {
                            rr.not_smaller += 1;
                        }
                        if item.1 {
                            rr.substituted += 1;
                        }
                        rr.create_seconds += create;
                        rr.apply_seconds += apply;
                        vers[k].as_mut().expect("present").derived[ri][j] = Some(item.0);
                    }
                }
            }
        }
        for (row, sink) in vrows.iter_mut().zip(sinks) {
            let [x, adj, bases, new] = sink.code()?;
            row.changed_coded = x;
            row.adjacent_coded = adj;
            row.bases_coded = bases;
            row.changed_adjacent = adj.saturating_sub(bases);
            row.new_coded = new;
            for r in &mut row.rules {
                r.append_estimate = r.coded_bytes + r.derived * O_BLOCK + new;
            }
        }
        rows.extend(vrows);
    }
    Ok(rows)
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let template = ruzstd_template()?;
    let crafted = ruzstd_crafted(&template)?;
    let mut ruz = Ruz {
        template,
        tried: 0,
        decoded: 0,
        first_failure: None,
    };
    let mut classes = Vec::new();
    let mut notes = Vec::new();
    for class in [VERSIONS_CLASS, VERSIONS_LARGE_CLASS] {
        let Some(files) = ctx.class_files(class) else {
            classes.push(ClassRow {
                class: class.to_string(),
                present: false,
                roots: Vec::new(),
                files_outside: 0,
                versions: Vec::new(),
                rows: None,
                gate: None,
                decision: DECISION_NONE.to_string(),
            });
            continue;
        };
        let mut roots: BTreeMap<String, Vec<(String, &ManifestFile)>> = BTreeMap::new();
        let mut outside = 0u64;
        for f in files {
            let rel = ctx.rel_path(class, f);
            match rel.split_once('/') {
                Some((root, inner)) => roots
                    .entry(root.to_string())
                    .or_default()
                    .push((inner.to_string(), f)),
                None => outside += 1,
            }
        }
        let list: Vec<(String, Vec<String>)> = roots
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|(r, _)| r.clone()).collect()))
            .collect();
        let versions = Versions::detect(&list);
        let mut labels: HashMap<String, String> = HashMap::new();
        let mut names: Vec<&String> = roots.keys().collect();
        names.sort_by(|a, b| lpk_core::fold::versions::natural_cmp(a, b));
        for (n, name) in names.iter().enumerate() {
            labels.insert(
                (*name).clone(),
                if ctx.private() {
                    format!("root-{n}")
                } else {
                    (*name).clone()
                },
            );
        }
        let grouped: HashSet<&str> = versions.roots.iter().map(|r| r.name.as_str()).collect();
        outside += roots
            .iter()
            .filter(|(k, _)| !grouped.contains(k.as_str()))
            .map(|(_, v)| v.len() as u64)
            .sum::<u64>();
        let root_rows: Vec<Root> = versions
            .roots
            .iter()
            .map(|r| {
                let fs = roots.get(&r.name).map(Vec::as_slice).unwrap_or(&[]);
                Root {
                    label: labels.get(&r.name).cloned().unwrap_or_default(),
                    group: r.group as u64,
                    rank: r.rank as u64,
                    files: fs.len() as u64,
                    bytes: fs.iter().map(|(_, f)| f.bytes).sum(),
                }
            })
            .collect();
        if versions.is_empty() {
            notes.push(format!("class `{class}`: no version group"));
        }
        let vrows = walk_class(ctx, class, &versions, &roots, &mut ruz, &labels)?;
        let rows = if ctx.rows.is_empty() {
            None
        } else {
            Some(read_rows(&ctx.rows, class, &ctx.corpus.manifest_blake3)?)
        };
        let g = match &rows {
            Some(GateRows {
                s_bal: Some(s),
                b: Some(b),
                ..
            }) if !vrows.is_empty() => Some(gate(*s, *b, &vrows)),
            _ => None,
        };
        let decision = g
            .as_ref()
            .map_or(DECISION_NONE.to_string(), |g| g.decision.clone());
        classes.push(ClassRow {
            class: class.to_string(),
            present: true,
            roots: root_rows,
            files_outside: outside,
            versions: vrows,
            rows,
            gate: g,
            decision,
        });
    }
    let ok = crafted && ruz.decoded == ruz.tried;
    let ruzstd = RuzstdFact {
        version: "0.9.1".to_string(),
        method: "a zstd frame written against a raw prefix (libzstd ref_prefix, level 19), \
                 decoded by ruzstd::decoding::FrameDecoder with a Dictionary parsed from a \
                 trained template whose dict_content is replaced by the base and offset_hist by \
                 [1, 4, 8], forced onto the frame (force_dict; the frame has no Dictionary_ID)"
            .to_string(),
        crafted_decoded: crafted,
        corpus_tried: ruz.tried,
        corpus_decoded: ruz.decoded,
        outcome: if ok { "yes" } else { "no" }.to_string(),
        first_failure: ruz.first_failure,
    };
    let data = Data {
        chunking: format!(
            "fastcdc v2020, normalization level 1, per file, {MIN_CHUNK}/{AVG_CHUNK}/{MAX_CHUNK} \
             bytes, blake3"
        ),
        coder: "the Balanced tier's raw LZMA1 (liblzma preset 9 extreme, BT4, nice 273, lc 3 \
                lp 0 pb 2, dictionary min(64 MiB, next power of two of the input)); a patch is \
                stored when the coder is not smaller"
            .to_string(),
        o_block: O_BLOCK,
        o_block_breakdown: "frame 4 + 3 + 32, header 23 + 3 + 3, index row 5 + 3 + 3 + 1 + 3 + 3"
            .to_string(),
        margin_fraction: MARGIN_FRACTION,
        zstd_level: ZSTD_LEVEL,
        ruzstd,
        classes,
    };
    let mut out = Output::new(data, 1).with_zstd().with_xz();
    out.libraries
        .insert("ruzstd".to_string(), "0.9.1".to_string());
    out.libraries.insert(
        "libsais".to_string(),
        lpk_codecs_bwt::LIBSAIS_VERSION.to_string(),
    );
    out.notes = notes;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Rendering and checks

fn rule_name(r: &RuleRow) -> String {
    format!("{}/{}/{}", r.pairing, r.base, r.format)
}

pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    s.push_str(&format!(
        "- chunking: {}\n- coder: {}\n- O_block {} bytes ({}); margin {} of B; zstd level {}\n",
        d.chunking, d.coder, d.o_block, d.o_block_breakdown, d.margin_fraction, d.zstd_level
    ));
    let r = &d.ruzstd;
    s.push_str(&format!(
        "- ruzstd {} raw-prefix decode: **{}** (crafted pair {}, corpus frames {} of {} \
         bit-exact{}); method: {}\n\n",
        r.version,
        r.outcome,
        if r.crafted_decoded {
            "decoded"
        } else {
            "not decoded"
        },
        r.corpus_decoded,
        r.corpus_tried,
        r.first_failure
            .as_ref()
            .map(|f| format!("; first failure: {f}"))
            .unwrap_or_default(),
        r.method
    ));
    for c in &d.classes {
        s.push_str(&format!("## `{}`\n\n", c.class));
        if !c.present {
            s.push_str("Not in this corpus.\n\n");
            continue;
        }
        let rows: Vec<Vec<String>> = c
            .roots
            .iter()
            .map(|r| {
                vec![
                    r.label.clone(),
                    r.group.to_string(),
                    r.rank.to_string(),
                    r.files.to_string(),
                    r.bytes.to_string(),
                ]
            })
            .collect();
        s.push_str(&md_table(
            &["root", "group", "rank", "files", "bytes"],
            &rows,
        ));
        s.push_str(&format!(
            "\nFiles in no group root: {}.\n\n",
            c.files_outside
        ));
        let rows: Vec<Vec<String>> = c
            .versions
            .iter()
            .map(|v| {
                vec![
                    format!("{} (g{} r{})", v.label, v.group, v.rank),
                    format!("{} / {}", v.paired_files, v.files),
                    format!("{} / {}", v.unchanged_chunks, v.unchanged_bytes),
                    format!("{} / {}", v.changed_chunks, v.changed_bytes),
                    format!("{} / {}", v.new_chunks, v.new_bytes),
                    v.changed_coded.to_string(),
                    v.changed_adjacent.to_string(),
                    v.new_coded.to_string(),
                    v.changed_alone_coded.to_string(),
                ]
            })
            .collect();
        s.push_str(&md_table(
            &[
                "version",
                "paired / files",
                "unchanged chunks / bytes",
                "changed chunks / bytes",
                "new chunks / bytes",
                "X_v",
                "X_v^adj",
                "new coded",
                "changed coded alone",
            ],
            &rows,
        ));
        s.push('\n');
        let mut rows: Vec<Vec<String>> = Vec::new();
        for v in &c.versions {
            for r in &v.rules {
                rows.push(vec![
                    format!("{} (r{})", v.label, v.rank),
                    rule_name(r),
                    r.derived.to_string(),
                    r.patch_bytes.to_string(),
                    r.coded_bytes.to_string(),
                    (r.coded_bytes + r.derived * d.o_block).to_string(),
                    r.append_estimate.to_string(),
                    r.not_smaller.to_string(),
                    r.substituted.to_string(),
                    format!("{} / {}", r.verified, r.derived),
                    mbps(r.derived_bytes, r.create_seconds),
                    mbps(r.derived_bytes, r.apply_seconds),
                ]);
            }
        }
        s.push_str(&md_table(
            &[
                "version",
                "rule",
                "n_v",
                "patch bytes",
                "Y_v",
                "Y_v + n_v O_block",
                "A_v",
                "not smaller than alone",
                "substituted",
                "applied and equal",
                "create MB/s",
                "apply MB/s",
            ],
            &rows,
        ));
        s.push('\n');
        if let Some(rows) = &c.rows {
            s.push_str(&format!(
                "Rows read from: {}; S_bal {}, B {}{}.\n\n",
                rows.dirs.join(", "),
                rows.s_bal.map_or("absent".into(), |v| v.to_string()),
                rows.b.map_or("absent".into(), |v| v.to_string()),
                rows.b_row
                    .as_ref()
                    .map(|r| format!(" (`{r}`)"))
                    .unwrap_or_default()
            ));
        }
        if let Some(g) = &c.gate {
            let mut rows = vec![
                vec!["S_bal".to_string(), g.s_bal.to_string(), String::new()],
                vec!["B".to_string(), g.b.to_string(), String::new()],
                vec!["0.5 B".to_string(), g.half_b.to_string(), String::new()],
                vec![
                    "mu = 0.02 B".to_string(),
                    g.margin.to_string(),
                    String::new(),
                ],
                vec![
                    "S_ord = S_bal - sum X_v + sum X_v^adj".to_string(),
                    g.s_ord.to_string(),
                    super::pct(g.s_ord, g.b),
                ],
            ];
            for est in &g.estimates {
                rows.push(vec![
                    format!("S_est {}/{}/{}", est.pairing, est.base, est.format),
                    est.s_est.to_string(),
                    super::pct(est.s_est, g.b),
                ]);
            }
            s.push_str(&md_table(&["quantity", "bytes", "of B"], &rows));
            s.push('\n');
        }
        s.push_str(&format!("Decision: {}.\n\n", c.decision));
    }
    s
}

pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let mut p = Vec::new();
    let d = &e.data;
    let r = &d.ruzstd;
    let ok = r.crafted_decoded && r.corpus_decoded == r.corpus_tried;
    if r.outcome != if ok { "yes" } else { "no" } || r.corpus_decoded > r.corpus_tried {
        p.push("/data/ruzstd/outcome: does not follow from the counts".to_string());
    }
    if d.o_block != O_BLOCK {
        p.push(format!("/data/o_block: {} is not {O_BLOCK}", d.o_block));
    }
    for (ci, c) in d.classes.iter().enumerate() {
        let at = format!("/data/classes/{ci}");
        for (vi, v) in c.versions.iter().enumerate() {
            let at = format!("{at}/versions/{vi}");
            if v.unchanged_chunks + v.changed_chunks + v.new_chunks != v.chunks {
                p.push(format!("{at}: the chunk counts do not add up"));
            }
            if v.changed_adjacent != v.adjacent_coded.saturating_sub(v.bases_coded) {
                p.push(format!("{at}/changed_adjacent: not adjacent - bases"));
            }
            if v.rules.len() != RULES.len() {
                p.push(format!(
                    "{at}/rules: {} rules, not {}",
                    v.rules.len(),
                    RULES.len()
                ));
                continue;
            }
            for (ri, (rule, (pp, b, f))) in v.rules.iter().zip(RULES).enumerate() {
                let at = format!("{at}/rules/{ri}");
                if rule.pairing != pp || rule.base != b || rule.format != f {
                    p.push(format!("{at}: not the rule {pp}/{b}/{f}"));
                }
                if rule.verified != rule.derived || rule.derived != v.changed_chunks {
                    p.push(format!(
                        "{at}/verified: every changed chunk is derived and every patch applied"
                    ));
                }
                if rule.append_estimate != rule.coded_bytes + rule.derived * O_BLOCK + v.new_coded {
                    p.push(format!(
                        "{at}/append_estimate: not Y + n O_block + new coded"
                    ));
                }
            }
        }
        if let Some(g) = &c.gate {
            let want = gate(g.s_bal, g.b, &c.versions);
            if &want != g {
                p.push(format!("{at}/gate: does not follow from the version rows"));
            }
            if c.decision != g.decision {
                p.push(format!("{at}/decision: not the gate's"));
            }
        } else if c.decision != DECISION_NONE {
            p.push(format!("{at}/decision: no gate, so not evaluated"));
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruzstd_raw_prefix_fact() {
        // The one test the design asks for: can ruzstd 0.9.1 decode a frame written against a
        // raw prefix? The answer is asserted as found, so a change of behaviour shows here.
        let t = ruzstd_template().expect("template");
        assert!(ruzstd_crafted(&t).expect("crafted"));
    }

    fn salad(len: usize, seed: u64) -> Vec<u8> {
        let words = ["alpha ", "bravo ", "charlie ", "delta ", "echo ", "golf\n"];
        let mut x = seed | 1;
        let mut out = Vec::with_capacity(len + 8);
        while out.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(words[(x % 6) as usize].as_bytes());
        }
        out.truncate(len);
        out
    }

    /// `v` with a few bytes changed every `step` bytes, starting at `from`.
    fn edited(v: &[u8], from: usize, step: usize, tag: &[u8]) -> Vec<u8> {
        let mut out = v.to_vec();
        let mut at = from;
        while at + tag.len() < out.len() {
            out[at..at + tag.len()].copy_from_slice(tag);
            at += step;
        }
        out
    }

    fn manifest_file(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile {
            blake3: blake3::hash(data).to_hex().to_string(),
            bytes: data.len() as u64,
            licence: "CC0-1.0".to_string(),
            path: path.to_string(),
            source: "test".to_string(),
        }
    }

    /// `backup-versions` with v1, v2, v3: `a.txt` edited from version to version, `b.txt`
    /// the same in all three, `c.txt` only in v3, `d.txt` in v1 and v3, and a file outside.
    fn three_versions(dir: &Path) -> (PathBuf, usize) {
        let a1 = salad(400_000, 3);
        let a2 = edited(&a1, 1000, 30_000, b"EDIT-TWO");
        let a3 = edited(&a2, 16_000, 30_000, b"EDIT-THREE");
        let b = salad(150_000, 5);
        let c = salad(90_000, 7);
        let d1 = salad(100_000, 11);
        let d3 = edited(&d1, 500, 40_000, b"D3");
        let class = "backup-versions";
        let files: Vec<(&str, &[u8])> = vec![
            ("v1/a.txt", &a1),
            ("v2/a.txt", &a2),
            ("v3/a.txt", &a3),
            ("v1/b.txt", &b),
            ("v2/b.txt", &b),
            ("v3/b.txt", &b),
            ("v3/c.txt", &c),
            ("v1/d.txt", &d1),
            ("v3/d.txt", &d3),
            ("readme.txt", b"outside every root"),
        ];
        let mut list = Vec::new();
        for (rel, data) in files {
            let p = dir.join(class).join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(&p, data).expect("write");
            list.push((
                class.to_string(),
                manifest_file(&format!("{class}/{rel}"), data),
            ));
        }
        let m = crate::corpus::manifest::Manifest::with_profile_name("small", list);
        std::fs::write(dir.join("manifest.json"), m.render()).expect("manifest");
        (dir.to_path_buf(), c.len())
    }

    fn rows_dir(dir: &Path, manifest: &str) {
        std::fs::create_dir_all(dir).expect("mkdir");
        let tools = crate::run::result::samples::tools();
        std::fs::write(dir.join("tools.json"), crate::run::result::render(&tools)).expect("t");
        for (tool, setting, bytes) in [
            ("lpk", "balanced", 900_000u64),
            ("7z", "ultra", 1_000_000),
            ("zpaqfranz", "m5", 10),
        ] {
            let mut r = crate::run::result::samples::measured();
            r.tool.id = tool.into();
            r.setting.id = setting.into();
            r.class = "backup-versions".into();
            r.corpus.manifest_blake3 = manifest.into();
            if let Some(m) = r.median.as_mut() {
                m.archive_bytes = bytes;
            }
            std::fs::write(dir.join(r.file_name()), crate::run::result::render(&r)).expect("row");
        }
    }

    fn config(corpus: PathBuf, root: PathBuf, rows: Vec<PathBuf>) -> super::super::Config {
        super::super::Config {
            probes: vec![NAME.to_string()],
            corpus,
            into: None,
            tmp_root: root.join("tmp"),
            results_root: root,
            threads: 2,
            allow_dirty: true,
            allow_debug_build: true,
            local_tools: PathBuf::from("none.toml"),
            tool_timeout: std::time::Duration::from_secs(60),
            rows,
        }
    }

    #[test]
    fn a_three_version_tree_pairs_prices_and_gates() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (corpus, c_len) = three_versions(&tmp.path().join("corpus"));
        let manifest = crate::run::exec::load_corpus(&corpus)
            .expect("corpus")
            .manifest_blake3;
        let rows = tmp.path().join("rows");
        rows_dir(&rows, &manifest);
        let cfg = config(corpus.clone(), tmp.path().join("results"), vec![rows]);
        let out = super::super::execute(&cfg).expect("execute");
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(out.problems, 0);
        let json = std::fs::read_to_string(out.results_dir.join("probe-delta.json")).expect("json");
        let env: Envelope<Data> = serde_json::from_str(&json).expect("typed");
        assert_eq!(env.data.ruzstd.outcome, "yes");
        let c = &env.data.classes[0];
        assert!(c.present && !env.data.classes[1].present);
        let labels: Vec<(&str, u64)> = c.roots.iter().map(|r| (r.label.as_str(), r.rank)).collect();
        assert_eq!(labels, [("v1", 0), ("v2", 1), ("v3", 2)]);
        assert_eq!(c.files_outside, 1);
        let (v2, v3) = (&c.versions[0], &c.versions[1]);
        assert_eq!((v2.files, v2.paired_files, v2.new_chunks), (2, 2, 0));
        assert_eq!((v3.files, v3.paired_files), (4, 3));
        // c.txt is v3's only new file; b.txt is unchanged in v2 and v3.
        assert_eq!(v3.new_bytes, c_len as u64);
        assert!(v2.unchanged_bytes >= 150_000 && v3.unchanged_bytes >= 150_000);
        assert!(v2.changed_chunks > 0 && v3.changed_chunks > 0);
        for v in [v2, v3] {
            for r in &v.rules {
                assert_eq!(r.verified, v.changed_chunks, "{}", rule_name(r));
                assert!(r.coded_bytes > 0);
            }
            // The patches are far smaller than the changed chunks coded together.
            assert!(v.rules[0].coded_bytes < v.changed_coded, "{v:?}");
        }
        // v2: the previous version is the first one, so P1 and P2 price the same.
        assert_eq!(v2.rules[0].coded_bytes, v2.rules[4].coded_bytes);
        assert_eq!(v2.rules[0].substituted, 0);
        // v3: a.txt's candidates in v2 were derived, so P1 substitutes their own base.
        assert!(v3.rules[0].substituted > 0);
        // The gate: S_bal from lpk/balanced, B from 7z (zpaqfranz deduplicates).
        let g = c.gate.as_ref().expect("gate");
        assert_eq!((g.s_bal, g.b), (900_000, 1_000_000));
        let sum_x = v2.changed_coded + v3.changed_coded;
        let sum_adj = v2.changed_adjacent + v3.changed_adjacent;
        assert_eq!(g.s_ord, 900_000 - sum_x + sum_adj);
        let y: u64 = [v2, v3]
            .iter()
            .map(|v| v.rules[0].coded_bytes + v.rules[0].derived * O_BLOCK)
            .sum();
        assert_eq!(g.estimates[0].s_est, 900_000 - sum_x + y);
        assert_eq!(g.margin, 20_000);
        assert_eq!(c.decision, g.decision);
        // Rows of another manifest are refused.
        let other = tmp.path().join("other-rows");
        rows_dir(&other, &"cd".repeat(32));
        let cfg = config(corpus, tmp.path().join("results2"), vec![other]);
        let out = super::super::execute(&cfg).expect("execute");
        assert_eq!(out.failed.len(), 1, "{:?}", out.failed);
        assert!(
            out.failed[0].1.contains("another manifest"),
            "{:?}",
            out.failed
        );
    }

    #[test]
    fn the_decision_rule() {
        assert_eq!(decide(50, 10, 5, 50), DECISION_ORDERING);
        assert_eq!(decide(100, 95, 5, 50), DECISION_WRITER);
        assert_eq!(decide(100, 96, 5, 50), DECISION_OWNER);
        // D-91: an estimate above 0.5 B still builds the writer when it beats S_ord by mu.
        assert_eq!(decide(100, 60, 2, 10), DECISION_WRITER);
    }
}
