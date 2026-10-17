//! `probe bwt` (PLAN E2-15, the BWT measured gate; brief `docs/notes/e2-15-probe-brief.md`).
//!
//! Per text class (`text-prose`, `logs-text`) the files are laid out in the order the Balanced
//! tier feeds them (`lpk_core::walk` then `lpk_core::cluster` on the class folder: clusters in
//! the tier's fixed order, files in path order) and cut into blocks the way the Balanced
//! pipeline closes them: at the block size, and at a cluster change once the block holds
//! `min_block_before_boundary` bytes. Two block sizes are used: the BWT cap (the envelope's
//! default `max_bwt_block`, 64 MiB, smaller than the Balanced block) and the Balanced block
//! (256 MiB). Fold (dedup) and the zstd trial of the tier are not applied: the reference row
//! is checked against the runner's `lpk/balanced` size in the report instead.
//!
//! Candidates, every one round-tripped and compared with the block:
//! * `bwt+lzma` (a): libsais BWT, then the Balanced tier's own LZMA encoder (`lpk_core`
//!   `LzmaEncoder::with_dict(64 MiB)`) on the BWT, decoded by the format's `lzma` decoder; the
//!   graph `[lzma, bwt]` the format can express today.
//! * `bwt+zstd` (a): libsais BWT, then zstd level 22 with long-distance matching and the window
//!   of the LZMA dictionary rule, decoded by the format's `zstd` decoder.
//! * `bwt+ari` (b): libsais BWT, then `lpk_codecs_bwt::entropy` (MTF, zero-run coding, an
//!   adaptive binary arithmetic coder with order-1 contexts).
//! * `lzma` (c): the Balanced tier's LZMA on the same 64 MiB blocks.
//! * `lzma-balanced-blocks` (c'): the same on the Balanced tier's 256 MiB blocks.
//!
//! The BWT candidates carry the index header of `lpk_codecs_bwt::Transformed::header` (the
//! sampling rate and up to 32 auxiliary indices) in their bytes. Times: one thread, blocks one
//! after another, each step timed alone on data in memory. A BWT candidate's compress time is
//! the transform plus its stage; its decompress time is its stage's decode plus the inverse
//! transform (the inverse is run once per block on the verified BWT, and its time is added to
//! each BWT candidate).
//!
//! Gate verdict (the PLAN's acceptance): the smallest BWT candidate is smaller than the smaller
//! of the two LZMA rows, on both classes.

use anyhow::{anyhow, bail, Context, Result};
use lpk_core::{cluster, walk, IngestOptions, LzmaEncoder};
use lpk_format::{BlockEncoder, LzmaDecoder, PrimitiveDecoder, Resources, ZstdDecoder};
use serde::{Deserialize, Serialize};
use zstd::zstd_safe::CParameter;

use super::{mbps, md_header, md_table, pct, timed, Ctx, Envelope, Output};

pub const NAME: &str = "bwt";

/// The classes, in table order.
pub const CLASSES: [&str; 2] = ["text-prose", "logs-text"];

/// The Balanced tier's defaults (`BalancedOptions::default()`).
pub const BALANCED_BLOCK: u64 = 256 << 20;
pub const DICT: u32 = 64 << 20;
/// Half the dictionary (the Balanced default of `min_block_before_boundary`).
pub const MIN_BLOCK_BEFORE_BOUNDARY: u64 = 32 << 20;
/// The envelope's default `max_bwt_block`.
pub const BWT_BLOCK: u64 = lpk_format::DEFAULT_MAX_BWT_BLOCK;
pub const ZSTD_LEVEL: i32 = 22;

/// The candidates, in table order: id and what it is.
pub const CANDIDATES: [(&str, &str); 5] = [
    (
        "bwt+lzma",
        "(a) libsais BWT, then the Balanced LZMA (64 MiB dict, lc3 lp0 pb2, preset 9e, nice 273)",
    ),
    (
        "bwt+zstd",
        "(a) libsais BWT, then zstd level 22, long matching, window = LZMA dictionary rule",
    ),
    (
        "bwt+ari",
        "(b) libsais BWT, then MTF + zero-run + order-1 adaptive binary arithmetic coder",
    ),
    ("lzma", "(c) the Balanced LZMA on the same (BWT-cap) blocks"),
    (
        "lzma-balanced-blocks",
        "(c') the Balanced LZMA on the Balanced tier's own blocks",
    ),
];

const BWT_IDS: [&str; 3] = ["bwt+lzma", "bwt+zstd", "bwt+ari"];
const REF_IDS: [&str; 2] = ["lzma", "lzma-balanced-blocks"];

/// What one candidate did with one class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub description: String,
    pub blocks: u64,
    pub compressed_bytes: u64,
    pub compress_seconds: f64,
    pub decompress_seconds: f64,
    /// Of the seconds above, the forward and inverse transforms (BWT candidates only).
    pub bwt_seconds: Option<f64>,
    pub unbwt_seconds: Option<f64>,
    /// Every block decoded back to its bytes.
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassRun {
    pub class: String,
    pub present: bool,
    pub files: u64,
    pub plain_bytes: u64,
    pub candidates: Vec<Candidate>,
    /// The smallest BWT candidate and the smaller LZMA row (ids), when present.
    pub best_bwt: Option<String>,
    pub reference: Option<String>,
    /// The smallest BWT candidate is smaller than the reference.
    pub passes: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub bwt_block: u64,
    pub balanced_block: u64,
    pub min_block_before_boundary: u64,
    pub classes: Vec<ClassRun>,
    /// Both classes present and passing: the gate opens.
    pub gate_open: bool,
}

/// Cut the files (in order) into blocks: a block closes when it reaches `block` bytes, and at
/// the end of a cluster once it holds `min_close` bytes (always at the end of the last).
pub fn blocks_of(clusters: &[Vec<&[u8]>], block: usize, min_close: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for (i, files) in clusters.iter().enumerate() {
        for f in files {
            let mut rest: &[u8] = f;
            while !rest.is_empty() {
                let take = (block - cur.len()).min(rest.len());
                cur.extend_from_slice(&rest[..take]);
                rest = &rest[take..];
                if cur.len() == block {
                    out.push(std::mem::take(&mut cur));
                }
            }
        }
        let last = i + 1 == clusters.len();
        if !cur.is_empty() && (last || cur.len() >= min_close) {
            out.push(std::mem::take(&mut cur));
        }
    }
    out
}

/// The class's file contents grouped in the Balanced tier's cluster order, each verified
/// against the manifest; the number of files.
fn class_layout(ctx: &Ctx<'_>, class: &str) -> Result<(Vec<Vec<Vec<u8>>>, u64)> {
    let files = ctx.class_files(class).unwrap_or(&[]);
    let root = ctx.corpus.files_root().join(class);
    let by_rel = |rel: &str| files.iter().find(|f| ctx.rel_path(class, f) == rel);
    if ctx.private() || !root.is_dir() {
        // No class folder (a private corpus): manifest order, one cluster.
        let mut one = Vec::new();
        for f in files {
            one.push(ctx.read_file(class, f)?);
        }
        return Ok((vec![one], files.len() as u64));
    }
    let inputs = walk(&root, &IngestOptions::default())
        .map_err(|e| anyhow!("walking class `{class}`: {e}"))?;
    let clusters = cluster(&inputs).map_err(|e| anyhow!("clustering class `{class}`: {e}"))?;
    let mut out = Vec::new();
    let mut count = 0u64;
    for c in &clusters {
        let mut group = Vec::new();
        for input in &c.inputs {
            let f = by_rel(&input.path).ok_or_else(|| {
                anyhow!(
                    "class `{class}`: `{}` is not in the manifest (corpus changed?)",
                    input.path
                )
            })?;
            group.push(ctx.read_file(class, f)?);
            count += 1;
        }
        out.push(group);
    }
    if count != files.len() as u64 {
        bail!(
            "class `{class}`: the folder holds {count} files, the manifest {}",
            files.len()
        );
    }
    Ok((out, count))
}

fn lzma_encoder() -> Result<LzmaEncoder> {
    LzmaEncoder::with_dict(DICT).map_err(|e| anyhow!("{e}"))
}

/// The LZMA dictionary rule: the dictionary, or the next power of two above `len` when smaller
/// (at least 4 KiB); as a log2 for zstd's window.
fn window_log(len: usize) -> u32 {
    let need = (len as u64)
        .next_power_of_two()
        .clamp(1 << 12, u64::from(DICT));
    need.trailing_zeros().max(10)
}

/// LZMA (the Balanced encoder) and its decode with the format's decoder.
fn lzma_round(enc: &mut LzmaEncoder, data: &[u8]) -> Result<(u64, f64, f64, bool)> {
    let (e, cs) = timed(|| enc.encode(data));
    let e = e.map_err(|e| anyhow!("lzma encode: {e}"))?;
    let params = e
        .graph
        .steps
        .first()
        .map(|s| s.params.clone())
        .ok_or_else(|| anyhow!("lzma graph has no step"))?;
    let limits = Resources::default();
    let (d, ds) = timed(|| LzmaDecoder.decode(&params, &e.bytes, data.len() as u64, &limits));
    let d = d.map_err(|e| anyhow!("lzma decode: {e}"))?;
    Ok((e.bytes.len() as u64, cs, ds, d == data))
}

fn zstd_round(data: &[u8]) -> Result<(u64, f64, f64, bool)> {
    let wlog = window_log(data.len());
    let mut c = zstd::bulk::Compressor::new(ZSTD_LEVEL)?;
    c.set_parameter(CParameter::WindowLog(wlog))?;
    c.set_parameter(CParameter::EnableLongDistanceMatching(true))?;
    let (bytes, cs) = timed(|| c.compress(data));
    let bytes = bytes?;
    let mut params = vec![wlog as u8];
    params.extend_from_slice(&[0u8; 32]);
    let limits = Resources::default();
    let dec = ZstdDecoder::default();
    let (d, ds) = timed(|| dec.decode(&params, &bytes, data.len() as u64, &limits));
    let d = d.map_err(|e| anyhow!("zstd decode: {e}"))?;
    Ok((bytes.len() as u64, cs, ds, d == data))
}

#[derive(Default)]
struct Acc {
    blocks: u64,
    bytes: u64,
    cs: f64,
    ds: f64,
    bwt: f64,
    unbwt: f64,
    ok: bool,
}

impl Acc {
    fn new() -> Self {
        Acc {
            ok: true,
            ..Acc::default()
        }
    }

    fn add(&mut self, (bytes, cs, ds, ok): (u64, f64, f64, bool)) {
        self.blocks += 1;
        self.bytes += bytes;
        self.cs += cs;
        self.ds += ds;
        self.ok &= ok;
    }

    fn row(&self, id: &str, bwt: bool) -> Candidate {
        let description = CANDIDATES
            .iter()
            .find(|(i, _)| *i == id)
            .map_or("", |(_, d)| *d);
        Candidate {
            id: id.to_string(),
            description: description.to_string(),
            blocks: self.blocks,
            compressed_bytes: self.bytes,
            compress_seconds: self.cs,
            decompress_seconds: self.ds,
            bwt_seconds: bwt.then_some(self.bwt),
            unbwt_seconds: bwt.then_some(self.unbwt),
            verified: self.ok,
        }
    }
}

/// All candidates on one class's blocks.
fn measure(clusters: &[Vec<&[u8]>]) -> Result<Vec<Candidate>> {
    let small = blocks_of(
        clusters,
        BWT_BLOCK as usize,
        MIN_BLOCK_BEFORE_BOUNDARY as usize,
    );
    let mut enc = lzma_encoder()?;
    let (mut a_lzma, mut a_zstd, mut b_ari, mut c_lzma, mut c_bal) =
        (Acc::new(), Acc::new(), Acc::new(), Acc::new(), Acc::new());
    for block in &small {
        let (t, fwd) = timed(|| lpk_codecs_bwt::forward(block));
        let t = t.map_err(|e| anyhow!("bwt: {e}"))?;
        let header = t.header().len() as u64;
        let (inv, inv_s) = timed(|| lpk_codecs_bwt::inverse(&t));
        let inv_ok = inv.map_err(|e| anyhow!("unbwt: {e}"))? == *block;
        for (acc, r) in [
            (&mut a_lzma, lzma_round(&mut enc, &t.bwt)?),
            (&mut a_zstd, zstd_round(&t.bwt)?),
        ] {
            acc.add((r.0 + header, r.1 + fwd, r.2 + inv_s, r.3 && inv_ok));
            acc.bwt += fwd;
            acc.unbwt += inv_s;
        }
        let (bytes, es) = timed(|| lpk_codecs_bwt::encode_transformed(&t));
        let (back, ds) = timed(|| lpk_codecs_bwt::decode_to_transformed(&bytes, block.len()));
        let back_ok = back.map(|b| b == t).unwrap_or(false);
        b_ari.add((bytes.len() as u64, fwd + es, ds + inv_s, back_ok && inv_ok));
        b_ari.bwt += fwd;
        b_ari.unbwt += inv_s;
        drop(t);
        c_lzma.add(lzma_round(&mut enc, block)?);
    }
    drop(small);
    for block in blocks_of(
        clusters,
        BALANCED_BLOCK as usize,
        MIN_BLOCK_BEFORE_BOUNDARY as usize,
    ) {
        c_bal.add(lzma_round(&mut enc, &block)?);
    }
    Ok(vec![
        a_lzma.row("bwt+lzma", true),
        a_zstd.row("bwt+zstd", true),
        b_ari.row("bwt+ari", true),
        c_lzma.row("lzma", false),
        c_bal.row("lzma-balanced-blocks", false),
    ])
}

/// The smallest candidate among `ids` (first on a tie).
fn smallest<'a>(cands: &'a [Candidate], ids: &[&str]) -> Option<&'a Candidate> {
    cands
        .iter()
        .filter(|c| ids.contains(&c.id.as_str()))
        .min_by_key(|c| c.compressed_bytes)
}

fn verdict(c: &mut ClassRun) {
    let best = smallest(&c.candidates, &BWT_IDS).map(|x| (x.id.clone(), x.compressed_bytes));
    let reference = smallest(&c.candidates, &REF_IDS).map(|x| (x.id.clone(), x.compressed_bytes));
    c.passes = matches!((&best, &reference), (Some(b), Some(r)) if b.1 < r.1)
        && c.candidates.iter().all(|x| x.verified);
    c.best_bwt = best.map(|b| b.0);
    c.reference = reference.map(|r| r.0);
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let mut classes = Vec::new();
    let mut notes = Vec::new();
    for class in CLASSES {
        let Some(files) = ctx.class_files(class).filter(|f| !f.is_empty()) else {
            notes.push(format!("class `{class}` is not in this corpus"));
            classes.push(ClassRun {
                class: class.to_string(),
                present: false,
                files: 0,
                plain_bytes: 0,
                candidates: Vec::new(),
                best_bwt: None,
                reference: None,
                passes: false,
            });
            continue;
        };
        let _ = files;
        let (layout, count) = class_layout(ctx, class)?;
        let refs: Vec<Vec<&[u8]>> = layout
            .iter()
            .map(|g| g.iter().map(Vec::as_slice).collect())
            .collect();
        let plain_bytes = refs.iter().flatten().map(|f| f.len() as u64).sum();
        let candidates = measure(&refs).with_context(|| format!("class `{class}`"))?;
        let mut c = ClassRun {
            class: class.to_string(),
            present: true,
            files: count,
            plain_bytes,
            candidates,
            best_bwt: None,
            reference: None,
            passes: false,
        };
        verdict(&mut c);
        classes.push(c);
    }
    let gate_open = classes.iter().all(|c| c.present && c.passes);
    let mut out = Output::new(
        Data {
            bwt_block: BWT_BLOCK,
            balanced_block: BALANCED_BLOCK,
            min_block_before_boundary: MIN_BLOCK_BEFORE_BOUNDARY,
            classes,
            gate_open,
        },
        1,
    )
    .with_zstd()
    .with_xz();
    out.libraries.insert(
        "libsais".to_string(),
        lpk_codecs_bwt::LIBSAIS_VERSION.to_string(),
    );
    out.libraries
        .insert("lzma-rs (decode)".to_string(), "0.3.0".to_string());
    out.libraries
        .insert("ruzstd (decode)".to_string(), "0.9.0".to_string());
    for n in notes {
        out = out.note(n);
    }
    Ok(out)
}

fn seconds_ok(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    if d.bwt_block == 0 || d.bwt_block > d.balanced_block {
        p.push("/data/bwt_block: must be 1..=balanced_block".to_string());
    }
    let names: Vec<&str> = d.classes.iter().map(|c| c.class.as_str()).collect();
    if names != CLASSES {
        p.push(format!(
            "/data/classes: must list exactly {CLASSES:?}, found {names:?}"
        ));
    }
    for (i, c) in d.classes.iter().enumerate() {
        let at = format!("/data/classes/{i}");
        if !c.present {
            if !c.candidates.is_empty() || c.passes {
                p.push(format!(
                    "{at}: an absent class has no candidates and no pass"
                ));
            }
            continue;
        }
        let ids: Vec<&str> = c.candidates.iter().map(|x| x.id.as_str()).collect();
        let want: Vec<&str> = CANDIDATES.iter().map(|(i, _)| *i).collect();
        if ids != want {
            p.push(format!("{at}/candidates: must be {want:?}, found {ids:?}"));
        }
        for (j, x) in c.candidates.iter().enumerate() {
            let at = format!("{at}/candidates/{j}");
            if !seconds_ok(x.compress_seconds) || !seconds_ok(x.decompress_seconds) {
                p.push(format!("{at}: seconds must be non-negative numbers"));
            }
            let bwt = BWT_IDS.contains(&x.id.as_str());
            if bwt != (x.bwt_seconds.is_some() && x.unbwt_seconds.is_some()) {
                p.push(format!(
                    "{at}: transform seconds are given exactly for the BWT candidates"
                ));
            }
            if x.blocks == 0 && c.plain_bytes > 0 {
                p.push(format!("{at}/blocks: a class with bytes has blocks"));
            }
            if !x.verified {
                p.push(format!("{at}/verified: the round trip failed"));
            }
        }
        let mut again = c.clone();
        verdict(&mut again);
        if (again.passes, &again.best_bwt, &again.reference)
            != (c.passes, &c.best_bwt, &c.reference)
        {
            p.push(format!(
                "{at}: best_bwt, reference and passes do not follow from the candidates"
            ));
        }
    }
    let open = d.classes.iter().all(|c| c.present && c.passes);
    if open != d.gate_open {
        p.push(
            "/data/gate_open: must be true exactly when every class is present and passes"
                .to_string(),
        );
    }
    p
}

fn secs(v: Option<f64>) -> String {
    v.map_or("-".to_string(), |s| format!("{s:.2}"))
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    s.push_str(&format!(
        "The E2-15 measured gate: BWT plus entropy coding against the Balanced tier's LZMA on the \
         text classes. Files in the Balanced tier's order (walk, then clusters), cut into blocks \
         the way the Balanced pipeline closes them: BWT candidates and `lzma` on {} byte blocks \
         (the envelope's default BWT cap), `lzma-balanced-blocks` on {} byte blocks (the Balanced \
         block); a block also closes at a cluster change once it holds {} bytes. One thread, \
         blocks one after another, each step timed alone; MB/s are of the plain bytes (10^6 bytes \
         per second). BWT rows include the transform in compress and the inverse in decompress. \
         Every block is decoded back and compared.\n\n",
        d.bwt_block, d.balanced_block, d.min_block_before_boundary
    ));
    let mut rows = Vec::new();
    for c in &d.classes {
        if !c.present {
            rows.push(vec![
                c.class.clone(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "class not in this corpus".into(),
            ]);
            continue;
        }
        for x in &c.candidates {
            rows.push(vec![
                c.class.clone(),
                x.id.clone(),
                x.blocks.to_string(),
                c.plain_bytes.to_string(),
                x.compressed_bytes.to_string(),
                pct(x.compressed_bytes, c.plain_bytes),
                format!(
                    "{:.2} ({} MB/s)",
                    x.compress_seconds,
                    mbps(c.plain_bytes, x.compress_seconds)
                ),
                format!(
                    "{:.2} ({} MB/s)",
                    x.decompress_seconds,
                    mbps(c.plain_bytes, x.decompress_seconds)
                ),
                format!("{} / {}", secs(x.bwt_seconds), secs(x.unbwt_seconds)),
                if x.verified {
                    "verified"
                } else {
                    "NOT verified"
                }
                .to_string(),
            ]);
        }
    }
    s.push_str(&md_table(
        &[
            "class",
            "candidate",
            "blocks",
            "plain bytes",
            "compressed bytes",
            "of plain",
            "compress s",
            "decompress s",
            "bwt / unbwt s",
            "status",
        ],
        &rows,
    ));
    s.push_str("\nCandidates:\n\n");
    for (id, what) in CANDIDATES {
        s.push_str(&format!("- `{id}`: {what}\n"));
    }
    s.push_str("\nVerdict per class (smallest BWT candidate against the smaller LZMA row):\n\n");
    for c in &d.classes {
        if !c.present {
            s.push_str(&format!("- `{}`: not in this corpus\n", c.class));
            continue;
        }
        let size = |id: &Option<String>| {
            c.candidates
                .iter()
                .find(|x| Some(&x.id) == id.as_ref())
                .map_or(0, |x| x.compressed_bytes)
        };
        s.push_str(&format!(
            "- `{}`: `{}` {} bytes against `{}` {} bytes: {}\n",
            c.class,
            c.best_bwt.as_deref().unwrap_or("-"),
            size(&c.best_bwt),
            c.reference.as_deref().unwrap_or("-"),
            size(&c.reference),
            if c.passes { "smaller" } else { "not smaller" }
        ));
    }
    s.push_str(&format!(
        "\nGate: {}\n",
        if d.gate_open {
            "OPEN (a BWT candidate is smaller than the LZMA row on both classes)"
        } else {
            "CLOSED (no BWT candidate is smaller than the LZMA row on both classes; the route stays LZMA)"
        }
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_close_at_the_size_and_at_cluster_changes_past_the_minimum() {
        let a = vec![1u8; 10];
        let b = vec![2u8; 7];
        let c = vec![3u8; 3];
        let clusters: Vec<Vec<&[u8]>> = vec![vec![&a, &b], vec![&c], vec![&c]];
        let bl = blocks_of(&clusters, 8, 4);
        let lens: Vec<usize> = bl.iter().map(Vec::len).collect();
        // 17 bytes of cluster 0: 8, 8, then 1 left (below the minimum, carried on);
        // cluster 1 makes it 4 (closes); cluster 2 is the last.
        assert_eq!(lens, vec![8, 8, 4, 3]);
        let joined: Vec<u8> = bl.concat();
        let want: Vec<u8> = [&a[..], &b, &c, &c].concat();
        assert_eq!(joined, want);
        assert!(blocks_of(&[], 8, 4).is_empty());
        assert_eq!(blocks_of(&[vec![&a[..]]], 64, 32).len(), 1);
    }

    #[test]
    fn rounds_verify_on_small_text() {
        let text: Vec<u8> = b"the quick brown fox jumps over the lazy dog\n"
            .iter()
            .cycle()
            .take(50_000)
            .copied()
            .collect();
        let clusters: Vec<Vec<&[u8]>> = vec![vec![&text]];
        let cands = measure(&clusters).expect("measure");
        let ids: Vec<&str> = cands.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, CANDIDATES.iter().map(|c| c.0).collect::<Vec<_>>());
        assert!(cands.iter().all(|c| c.verified && c.blocks == 1));
        assert!(
            cands.iter().all(|c| c.compressed_bytes < 2_000),
            "{cands:?}"
        );
        let mut c = ClassRun {
            class: "text-prose".into(),
            present: true,
            files: 1,
            plain_bytes: text.len() as u64,
            candidates: cands,
            best_bwt: None,
            reference: None,
            passes: false,
        };
        verdict(&mut c);
        assert!(c.best_bwt.is_some() && c.reference.is_some());
    }

    #[test]
    fn verdict_needs_a_strictly_smaller_bwt_row() {
        let cand = |id: &str, bytes: u64| Candidate {
            id: id.into(),
            description: String::new(),
            blocks: 1,
            compressed_bytes: bytes,
            compress_seconds: 0.0,
            decompress_seconds: 0.0,
            bwt_seconds: BWT_IDS.contains(&id).then_some(0.0),
            unbwt_seconds: BWT_IDS.contains(&id).then_some(0.0),
            verified: true,
        };
        let mut c = ClassRun {
            class: "logs-text".into(),
            present: true,
            files: 1,
            plain_bytes: 1000,
            candidates: vec![
                cand("bwt+lzma", 120),
                cand("bwt+zstd", 130),
                cand("bwt+ari", 100),
                cand("lzma", 110),
                cand("lzma-balanced-blocks", 100),
            ],
            best_bwt: None,
            reference: None,
            passes: false,
        };
        verdict(&mut c);
        assert_eq!(c.best_bwt.as_deref(), Some("bwt+ari"));
        assert_eq!(c.reference.as_deref(), Some("lzma-balanced-blocks"));
        assert!(!c.passes, "a tie is not smaller");
        c.candidates[2].compressed_bytes = 99;
        verdict(&mut c);
        assert!(c.passes);
        c.candidates[0].verified = false;
        verdict(&mut c);
        assert!(!c.passes, "an unverified row fails the class");
    }

    #[test]
    fn window_log_follows_the_dictionary_rule() {
        assert_eq!(window_log(100), 12);
        assert_eq!(window_log(5000), 13);
        assert_eq!(window_log(64 << 20), 26);
        assert_eq!(window_log(256 << 20), 26);
    }
}
