//! What the report computes: baseline rows, blends over the disk mixes, the per-class estimates
//! from the probes and the gate evaluation. Everything is a [`Traced`] value.

use std::collections::BTreeMap;

use super::mixes::{Mix, Mixes};
use super::traced::{SourceId, Traced};
use crate::probe::{dedup, deflate, entropy_gate, jpeg, text, weights, Envelope};
use crate::run::result::{HostFile, RunFile, ToolResult, ToolsFile};

// ---------------------------------------------------------------------------------------------
// Inputs

/// A probe file: the typed envelope, its JSON text (for the probe's own renderer) and its source.
#[derive(Debug)]
pub struct ProbeFile<D> {
    pub env: Envelope<D>,
    pub json: String,
    pub src: SourceId,
}

#[derive(Debug, Default)]
pub struct Probes {
    pub jpeg: Option<ProbeFile<jpeg::Data>>,
    pub deflate: Option<ProbeFile<deflate::Data>>,
    pub dedup: Option<ProbeFile<dedup::Data>>,
    pub text: Option<ProbeFile<text::Data>>,
    pub weights: Option<ProbeFile<weights::Data>>,
    pub entropy_gate: Option<ProbeFile<entropy_gate::Data>>,
}

/// Name, build and source of every probe present, in the order of `probe all`.
pub struct ProbeInfo {
    pub name: &'static str,
    pub build: String,
    pub release: bool,
    pub date: String,
    pub host: String,
    pub src: SourceId,
    pub json: String,
    pub libraries: BTreeMap<String, String>,
}

impl Probes {
    pub fn present(&self) -> Vec<ProbeInfo> {
        let mut out = Vec::new();
        macro_rules! add {
            ($f:ident, $n:expr) => {
                if let Some(p) = &self.$f {
                    out.push(ProbeInfo {
                        name: $n,
                        build: p.env.build.clone(),
                        release: p.env.build_profile.is_release()
                            && !p.env.build_profile.allow_debug_build,
                        date: p.env.date.clone(),
                        host: p.env.host.clone(),
                        src: p.src,
                        json: p.json.clone(),
                        libraries: p.env.libraries.clone(),
                    });
                }
            };
        }
        add!(jpeg, "jpeg");
        add!(deflate, "deflate");
        add!(dedup, "dedup");
        add!(text, "text");
        add!(weights, "weights");
        add!(entropy_gate, "entropy-gate");
        out
    }

    pub fn missing(&self) -> Vec<&'static str> {
        let have: Vec<&str> = self.present().iter().map(|p| p.name).collect();
        crate::probe::NAMES
            .iter()
            .copied()
            .filter(|n| !have.contains(n))
            .collect()
    }
}

#[derive(Debug)]
pub struct Baseline {
    /// Name of the results directory (`<date>-<host>[-<n>]`).
    pub dir: String,
    pub host: HostFile,
    pub host_src: SourceId,
    pub tools: ToolsFile,
    pub tools_src: SourceId,
    pub run: RunFile,
    pub run_src: SourceId,
    pub results: Vec<(ToolResult, SourceId)>,
}

#[derive(Debug)]
pub struct Inputs {
    pub baseline: Baseline,
    pub probes: Probes,
    pub mixes: Mixes,
    pub mixes_src: SourceId,
    /// The mixes file as read, printed by the report.
    pub mixes_text: String,
    /// Source labels; the id of a source is its position + 1.
    pub sources: Vec<String>,
    /// Labels of input directories that are not under `bench/results`.
    pub outside: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Baseline rows

pub const STORE: (&str, &str) = ("store", "store");
pub const ULTRA: (&str, &str) = ("7z", "ultra");
pub const FAST: (&str, &str) = ("zstd", "3");
pub const MX5: (&str, &str) = ("7z", "mx5");

#[derive(Debug, Clone)]
pub struct Measured {
    pub size_bytes: Traced,
    /// Archive bytes as a percentage of the class bytes.
    pub size_pct: Traced,
    /// MB/s = class bytes / 10^6 / median wall seconds.
    pub compress_mbps: Traced,
    pub extract_mbps: Traced,
    /// The larger of the compress and extract peak memory medians, in MiB.
    pub rss_mib: Traced,
    pub repeats: Traced,
    /// Median compress wall seconds.
    pub compress_seconds: Traced,
    /// Class bytes over the first repeat's compress wall time, MB/s.
    pub first_compress_mbps: Traced,
    /// Set when the row rests on one measurement: why.
    pub single: Option<String>,
}

#[derive(Debug, Clone)]
pub enum RowState {
    Measured(Box<Measured>),
    Failed {
        step: String,
        reason: String,
        src: SourceId,
    },
    Skipped {
        reason: String,
        src: SourceId,
    },
}

#[derive(Debug, Clone)]
pub struct Row {
    pub tool: String,
    pub setting: String,
    pub class: String,
    pub class_bytes: Traced,
    pub class_files: u64,
    pub state: RowState,
}

impl Row {
    pub fn label(&self) -> String {
        format!("{}/{}", self.tool, self.setting)
    }

    pub fn measured(&self) -> Option<&Measured> {
        match &self.state {
            RowState::Measured(m) => Some(m),
            _ => None,
        }
    }
}

fn row_of(r: &ToolResult, src: SourceId) -> Row {
    let class_bytes = Traced::from_u64(r.corpus.class_bytes, src);
    let state = if let Some(s) = &r.skipped {
        RowState::Skipped {
            reason: s.clone(),
            src,
        }
    } else if let Some(f) = &r.failed {
        RowState::Failed {
            step: f.step.clone(),
            reason: f.reason.clone(),
            src,
        }
    } else if let (Some(med), Some(reps)) = (&r.median, &r.repeats) {
        let (cw, ew) = (med.compress.wall_seconds, med.extract.wall_seconds);
        if !(cw > 0.0 && ew > 0.0 && cw.is_finite() && ew.is_finite()) || r.corpus.class_bytes == 0
        {
            RowState::Failed {
                step: "report".into(),
                reason: "the result records no usable time or class size".into(),
                src,
            }
        } else {
            let size_bytes = Traced::from_u64(med.archive_bytes, src);
            let size_pct = (&size_bytes / &class_bytes).map(|x| x * 100.0);
            let peak = med
                .compress
                .peak_memory_bytes
                .max(med.extract.peak_memory_bytes);
            RowState::Measured(Box::new(Measured {
                size_pct,
                size_bytes,
                compress_mbps: class_bytes.map(|b| b / 1e6 / cw),
                extract_mbps: class_bytes.map(|b| b / 1e6 / ew),
                rss_mib: Traced::from_u64(peak, src).map(|b| b / 1_048_576.0),
                repeats: Traced::from_u64(reps.len() as u64, src),
                compress_seconds: Traced::new(cw, src),
                first_compress_mbps: class_bytes
                    .map(|b| b / 1e6 / reps.first().map_or(cw, |s| s.compress.wall_seconds)),
                single: (reps.len() == 1).then(|| {
                    r.repeats_short
                        .clone()
                        .unwrap_or_else(|| "one repeat recorded".to_string())
                }),
            }))
        }
    } else {
        RowState::Failed {
            step: "report".into(),
            reason: "the result has neither a median nor a failure".into(),
            src,
        }
    };
    Row {
        tool: r.tool.id.clone(),
        setting: r.setting.id.clone(),
        class: r.class.clone(),
        class_bytes,
        class_files: r.corpus.class_files,
        state,
    }
}

/// One row per planned combination, in the order of `run.json`.
pub fn rows(b: &Baseline) -> Vec<Row> {
    b.run
        .combinations
        .iter()
        .filter_map(|c| {
            b.results
                .iter()
                .find(|(r, _)| {
                    r.tool.id == c.tool && r.setting.id == c.setting && r.class == c.class
                })
                .map(|(r, src)| row_of(r, *src))
        })
        .collect()
}

/// Tool/setting pairs in order of first appearance.
pub fn settings(rows: &[Row]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for r in rows {
        let k = (r.tool.clone(), r.setting.clone());
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

pub fn find<'a>(rows: &'a [Row], key: (&str, &str), class: &str) -> Option<&'a Row> {
    rows.iter()
        .find(|r| r.tool == key.0 && r.setting == key.1 && r.class == class)
}

/// Class bytes by class, each with the source of the result file it came from.
pub fn class_bytes(rows: &[Row]) -> BTreeMap<String, Traced> {
    let mut m = BTreeMap::new();
    for r in rows {
        m.entry(r.class.clone())
            .or_insert_with(|| r.class_bytes.clone());
    }
    m
}

// ---------------------------------------------------------------------------------------------
// Blending

#[derive(Debug, Clone)]
pub struct Blend {
    pub size_pct: Traced,
    pub compress_mbps: Traced,
    pub extract_mbps: Traced,
    /// Some class of the mix rests on one measurement for this tool.
    pub single: bool,
}

/// Blend one tool × setting over a mix: the size ratio is `Σ w·ratio / Σ w`, a speed the
/// bytes-weighted harmonic mean `Σ w / Σ (w / speed)`.
pub fn blend(
    rows: &[Row],
    key: (&str, &str),
    mix: &Mix,
    mixes_src: SourceId,
) -> Result<Blend, String> {
    let mut size = Vec::new();
    let mut cmp = Vec::new();
    let mut ext = Vec::new();
    let mut weights = Vec::new();
    let mut single = false;
    for (class, w) in &mix.weights {
        let row = find(rows, key, class).ok_or_else(|| format!("no result for class `{class}`"))?;
        let m = row
            .measured()
            .ok_or_else(|| format!("class `{class}` has no measured result"))?;
        single |= m.single.is_some();
        let w = Traced::new(f64::from(*w), mixes_src);
        size.push(&w * &m.size_pct);
        cmp.push(&w / &m.compress_mbps);
        ext.push(&w / &m.extract_mbps);
        weights.push(w);
    }
    let total = Traced::sum(&weights).ok_or("the mix has no class")?;
    let sum = |v: &[Traced]| Traced::sum(v).ok_or_else(|| "the mix has no class".to_string());
    Ok(Blend {
        size_pct: &sum(&size)? / &total,
        compress_mbps: &total / &sum(&cmp)?,
        extract_mbps: &total / &sum(&ext)?,
        single,
    })
}

/// Classes of the mix that the corpus (the baseline) does not have.
pub fn missing_classes(mix: &Mix, corpus: &BTreeMap<String, Traced>) -> Vec<String> {
    mix.weights
        .keys()
        .filter(|c| !corpus.contains_key(*c))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Incumbents and estimates

#[derive(Debug, Clone)]
pub struct Incumbent {
    pub name: String,
    pub bytes: Traced,
}

/// The smallest archive among the measured rows of the class; the value carries the sources of
/// every row compared.
pub fn best_incumbent(rows: &[Row], class: &str) -> Option<Incumbent> {
    let mut best: Option<(&Row, &Measured)> = None;
    let mut all: Vec<&Traced> = Vec::new();
    for r in rows.iter().filter(|r| r.class == class) {
        if let Some(m) = r.measured() {
            all.push(&m.size_bytes);
            if best.is_none_or(|(_, b)| m.size_bytes.value < b.size_bytes.value) {
                best = Some((r, m));
            }
        }
    }
    let (row, m) = best?;
    let mut bytes = m.size_bytes.clone();
    for t in all {
        bytes.sources.extend(t.sources.iter().copied());
    }
    Some(Incumbent {
        name: format!(
            "{}{}",
            row.label(),
            if m.single.is_some() { " *" } else { "" }
        ),
        bytes,
    })
}

#[derive(Debug, Clone)]
pub struct Est {
    pub bytes: Traced,
    /// How it was derived (names the rows used); printed under the table.
    pub basis: String,
}

/// Probe-covered bytes plus the class bytes the probe did not measure, counted as stored.
fn cover(est: Traced, probe_bytes: Traced, class_bytes: &Traced) -> Result<(Traced, bool), String> {
    if probe_bytes.value > class_bytes.value {
        return Err("the probe covers more bytes than the baseline's class".into());
    }
    let rest = class_bytes - &probe_bytes;
    let partial = rest.value > 0.0;
    Ok((&est + &rest, partial))
}

fn partial_note(partial: bool) -> &'static str {
    if partial {
        "; files the probe did not measure are counted as stored"
    } else {
        ""
    }
}

fn jpeg_est(p: &Probes, class: &str, cb: &Traced) -> Result<Est, String> {
    let pf = p.jpeg.as_ref().ok_or("probe jpeg is not available")?;
    let c = pf
        .env
        .data
        .classes
        .iter()
        .find(|c| c.class == class)
        .filter(|c| !c.files.is_empty())
        .ok_or("probe jpeg measured no file of this class")?;
    let total: u64 = c.files.iter().map(|f| f.bytes).sum();
    let after: u64 = c
        .files
        .iter()
        .map(|f| f.lepton_bytes.unwrap_or(f.bytes))
        .sum();
    let (bytes, partial) = cover(
        Traced::from_u64(after, pf.src),
        Traced::from_u64(total, pf.src),
        cb,
    )?;
    Ok(Est {
        bytes,
        basis: format!(
            "bytes after Lepton with failed files stored as-is (probe jpeg, `after fallback`){}",
            partial_note(partial)
        ),
    })
}

fn deflate_est(p: &Probes, class: &str, cb: &Traced) -> Result<Est, String> {
    let pf = p.deflate.as_ref().ok_or("probe deflate is not available")?;
    let d = &pf.env.data;
    if d.settings.xz.preset != 9 {
        return Err("probe deflate used an xz preset other than 9".into());
    }
    let c = d
        .classes
        .iter()
        .find(|c| c.class == class)
        .filter(|c| !c.files.is_empty())
        .ok_or("probe deflate measured no file of this class")?;
    let total: u64 = c.files.iter().map(|f| f.bytes).sum();
    let b: u64 = c
        .files
        .iter()
        .map(|f| f.b.xz_bytes + f.streams.correction_bytes)
        .sum();
    let (bytes, partial) = cover(
        Traced::from_u64(b, pf.src),
        Traced::from_u64(total, pf.src),
        cb,
    )?;
    Ok(Est {
        bytes,
        basis: format!(
            "probe deflate figure B with xz preset 9 (reconstructed streams replaced by plain data, \
             corrections added, whole file compressed){}",
            partial_note(partial)
        ),
    })
}

const PATCH_TOOL: &str = "zstd --patch-from";

fn backup_est(p: &Probes, cb: &Traced) -> Result<Est, String> {
    let pf = p.dedup.as_ref().ok_or("probe dedup is not available")?;
    let versions = &pf.env.data.versions;
    if versions.is_empty() {
        return Err("probe dedup found no version folder".into());
    }
    let total: u64 = versions.iter().map(|v| v.bytes).sum();
    let a: u64 = versions
        .iter()
        .map(|v| v.new_unique_chunks_zstd19_bytes)
        .sum();
    let mut b: Option<u64> = Some(versions[0].new_unique_chunks_zstd19_bytes);
    for v in &versions[1..] {
        let patch = v
            .delta
            .as_ref()
            .and_then(|d| d.tools.iter().find(|t| t.tool == PATCH_TOOL))
            .filter(|t| t.verified)
            .and_then(|t| t.patch_bytes);
        b = match (b, patch) {
            (Some(acc), Some(x)) => Some(acc + x),
            _ => None,
        };
    }
    let (used, name) = match b {
        Some(b) if b < a => (
            b,
            "(b) version 1 at zstd level 19 plus the zstd --patch-from patches",
        ),
        _ => (
            a,
            if b.is_some() {
                "(a) the new unique chunks of every version at zstd level 19"
            } else {
                "(a) the new unique chunks of every version at zstd level 19 (the patches are \
                 incomplete, so (b) could not be formed)"
            },
        ),
    };
    let (bytes, partial) = cover(
        Traced::from_u64(used, pf.src),
        Traced::from_u64(total, pf.src),
        cb,
    )?;
    Ok(Est {
        bytes,
        basis: format!(
            "the smaller of two derivations, used: {name}{}",
            partial_note(partial)
        ),
    })
}

pub(super) fn text_est(p: &Probes, class: &str, cb: &Traced) -> Result<Est, String> {
    let pf = p.text.as_ref().ok_or("probe text is not available")?;
    let c = pf
        .env
        .data
        .classes
        .iter()
        .find(|c| c.class == class && c.present)
        .ok_or("probe text has no entry for this class")?;
    let best = c
        .runs
        .iter()
        .filter_map(|r| r.measured.as_ref().filter(|m| m.verified).map(|m| (r, m)))
        .min_by_key(|(_, m)| m.compressed_bytes)
        .ok_or("probe text has no verified result for this class")?;
    let (bytes, partial) = cover(
        Traced::from_u64(best.1.compressed_bytes, pf.src),
        Traced::from_u64(c.content_bytes, pf.src),
        cb,
    )?;
    Ok(Est {
        bytes,
        basis: format!(
            "smallest verified row of probe text: {} {} ({} timing); the stream is a tar of the class{}",
            best.0.compressor,
            best.0.setting,
            best.0.timing,
            partial_note(partial)
        ),
    })
}

pub(super) fn weights_est(p: &Probes, class: &str, cb: &Traced) -> Result<Est, String> {
    let pf = p.weights.as_ref().ok_or("probe weights is not available")?;
    let d = &pf.env.data;
    if d.class != class {
        return Err("probe weights measured another class".into());
    }
    if d.files.is_empty() {
        return Err("probe weights parsed no safetensors file (this profile has none)".into());
    }
    let (mut byte, mut rot, mut covered) = (0u64, 0u64, 0u64);
    for f in &d.files {
        let fixed = f.header_bytes + f.other_bytes;
        covered += f.bytes;
        byte += fixed
            + f.dtypes
                .iter()
                .map(|t| t.byte_planes.compressed_bytes)
                .sum::<u64>();
        rot += fixed
            + f.dtypes
                .iter()
                .map(|t| t.rotated_planes.compressed_bytes)
                .sum::<u64>();
    }
    for n in &d.not_parsed {
        covered += n.bytes;
        byte += n.bytes;
        rot += n.bytes;
    }
    let (used, name) = if rot < byte {
        (rot, "rotated planes")
    } else {
        (byte, "byte planes")
    };
    let (bytes, partial) = cover(
        Traced::from_u64(used, pf.src),
        Traced::from_u64(covered, pf.src),
        cb,
    )?;
    Ok(Est {
        bytes,
        basis: format!(
            "probe weights, the smaller total of the two plane variants: {name}; headers, \
             non-float tensors and unparsed files counted as stored{}",
            partial_note(partial)
        ),
    })
}

/// The rule used for each class, as printed under the estimate table.
pub const RULES: [(&str, &str); 6] = [
    (
        "photo-jpeg, photo-jpeg-edited",
        "bytes after Lepton with failed files stored as-is (probe jpeg, `after fallback`)",
    ),
    (
        "office-pdf, archives-nested, software-installed, game-assets, photo-raw-png",
        "probe deflate figure B with xz preset 9: reconstructed streams replaced by plain data, \
         corrections added, whole file compressed",
    ),
    (
        "backup-versions",
        "the smaller of (a) the sum over versions of the new unique chunks compressed with zstd \
         level 19 (version 1 whole) and (b) version 1 at zstd level 19 plus the zstd --patch-from \
         patches of the later versions (probe dedup); the report names which was used",
    ),
    (
        "text-prose, logs-text, small-files",
        "the smallest verified row of probe text for the class (xz, zstd, bsc or kanzi, whichever \
         ran); the report names the row",
    ),
    (
        "model-weights",
        "the smaller total of byte planes and rotated planes (probe weights) when the probe parsed \
         at least one file, otherwise not available",
    ),
    (
        "video, encrypted-random",
        "stored as-is (the class bytes); every other class: the best measured incumbent, no gain \
         claimed",
    ),
];

/// Classes with a probe-based or stored rule; the rest claim no gain.
pub const RULED: [&str; 14] = [
    "photo-jpeg",
    "photo-jpeg-edited",
    "office-pdf",
    "archives-nested",
    "software-installed",
    "game-assets",
    "photo-raw-png",
    "backup-versions",
    "text-prose",
    "logs-text",
    "small-files",
    "model-weights",
    "video",
    "encrypted-random",
];

pub fn estimate(
    class: &str,
    probes: &Probes,
    cb: &Traced,
    best: Option<&Incumbent>,
) -> Result<Est, String> {
    match class {
        "photo-jpeg" | "photo-jpeg-edited" => jpeg_est(probes, class, cb),
        "office-pdf" | "archives-nested" | "software-installed" | "game-assets"
        | "photo-raw-png" => deflate_est(probes, class, cb),
        "backup-versions" => backup_est(probes, cb),
        "text-prose" | "logs-text" | "small-files" => text_est(probes, class, cb),
        "model-weights" => weights_est(probes, class, cb),
        "video" | "encrypted-random" => Ok(Est {
            bytes: cb.clone(),
            basis: "stored as-is".into(),
        }),
        _ => best
            .map(|b| Est {
                bytes: b.bytes.clone(),
                basis: format!(
                    "no rule: the best measured incumbent ({}); no gain is claimed",
                    b.name
                ),
            })
            .ok_or_else(|| "no measured incumbent".to_string()),
    }
}

/// How a class's estimate came about, for the table cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstKind {
    Probe,
    Stored,
    NoRule,
}

pub fn est_kind(class: &str) -> EstKind {
    match class {
        "video" | "encrypted-random" => EstKind::Stored,
        c if RULED.contains(&c) => EstKind::Probe,
        _ => EstKind::NoRule,
    }
}

/// The best single tool × setting over the combined bytes of `classes`: the smallest sum of
/// archive bytes among the tool × settings measured on every class. The value carries the
/// sources of every candidate compared.
pub fn best_single(rows: &[Row], classes: &[&str]) -> Option<Incumbent> {
    let mut best: Option<(String, Traced, bool)> = None;
    let mut seen = std::collections::BTreeSet::new();
    for (t, s) in settings(rows) {
        let mut parts = Vec::new();
        let mut single = false;
        for c in classes {
            let Some(m) = find(rows, (&t, &s), c).and_then(Row::measured) else {
                break;
            };
            single |= m.single.is_some();
            parts.push(m.size_bytes.clone());
        }
        if parts.len() != classes.len() {
            continue;
        }
        let Some(sum) = Traced::sum(&parts) else {
            continue;
        };
        seen.extend(sum.sources.iter().copied());
        if best.as_ref().is_none_or(|b| sum.value < b.1.value) {
            best = Some((format!("{t}/{s}"), sum, single));
        }
    }
    let (name, mut bytes, single) = best?;
    bytes.sources = seen;
    Some(Incumbent {
        name: format!("{name}{}", if single { " *" } else { "" }),
        bytes,
    })
}

/// The mix's blended size (percent of the mix's bytes) of the best single tool × setting.
fn mix_best_single(rows: &[Row], mix: &Mix, src: SourceId) -> Result<Incumbent, String> {
    let mut best: Option<(String, Traced, bool)> = None;
    let mut seen = std::collections::BTreeSet::new();
    for (t, s) in settings(rows) {
        let (mut terms, mut ws, mut single) = (Vec::new(), Vec::new(), false);
        for (class, w) in &mix.weights {
            let Some(row) = find(rows, (&t, &s), class) else {
                break;
            };
            let Some(m) = row.measured() else { break };
            single |= m.single.is_some();
            let w = Traced::new(f64::from(*w), src);
            terms.push(&w * &(&m.size_bytes / &row.class_bytes));
            ws.push(w);
        }
        if terms.len() != mix.weights.len() {
            continue;
        }
        let (Some(num), Some(den)) = (Traced::sum(&terms), Traced::sum(&ws)) else {
            continue;
        };
        let ratio = (&num / &den).map(|x| x * 100.0);
        seen.extend(ratio.sources.iter().copied());
        if best.as_ref().is_none_or(|b| ratio.value < b.1.value) {
            best = Some((format!("{t}/{s}"), ratio, single));
        }
    }
    let (name, mut bytes, single) =
        best.ok_or("no tool has a measured result for every class of the mix")?;
    bytes.sources = seen;
    Ok(Incumbent {
        name: format!("{name}{}", if single { " *" } else { "" }),
        bytes,
    })
}

/// Failed or skipped rows of the classes, as one sentence (none: `None`).
pub fn problem_rows(rows: &[Row], classes: &[&str]) -> Option<String> {
    let mut failed = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for r in rows.iter().filter(|r| classes.contains(&r.class.as_str())) {
        match &r.state {
            RowState::Failed { reason, .. } => {
                failed.push(format!("{} on {} ({reason})", r.label(), r.class))
            }
            RowState::Skipped { reason, .. } => {
                let s = format!("{} ({reason})", r.label());
                if !skipped.contains(&s) {
                    skipped.push(s);
                }
            }
            RowState::Measured(_) => {}
        }
    }
    if failed.is_empty() && skipped.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !failed.is_empty() {
        parts.push(format!("failed: {}", failed.join("; ")));
    }
    if !skipped.is_empty() {
        parts.push(format!("skipped: {}", skipped.join("; ")));
    }
    Some(parts.join(". "))
}

#[derive(Debug, Clone)]
pub struct ClassView {
    pub class: String,
    pub class_bytes: Traced,
    pub best: Option<Incumbent>,
    pub ultra: Option<Traced>,
    pub est: Result<Est, String>,
}

#[derive(Debug, Clone)]
pub struct MixView {
    pub name: String,
    /// Blended ratios in percent of the mix's bytes; `Err` carries why not. `best` is the best
    /// single tool × setting (named); `best_per_class` the stricter sum of per-class bests.
    pub best: Result<Incumbent, String>,
    pub best_per_class: Result<Traced, String>,
    pub ultra: Result<Traced, String>,
    pub est: Result<Traced, String>,
}

/// `Σ w · ratio_c / Σ w` in percent, over the classes of the mix.
fn mix_ratio(
    mix: &Mix,
    mixes_src: SourceId,
    classes: &BTreeMap<&str, &ClassView>,
    pick: impl Fn(&ClassView) -> Result<Traced, String>,
) -> Result<Traced, String> {
    let mut terms = Vec::new();
    let mut ws = Vec::new();
    for (class, w) in &mix.weights {
        let v = classes
            .get(class.as_str())
            .ok_or_else(|| format!("class `{class}` is not in the corpus"))?;
        let bytes = pick(v).map_err(|e| format!("class `{class}`: {e}"))?;
        let w = Traced::new(f64::from(*w), mixes_src);
        terms.push(&w * &(&bytes / &v.class_bytes));
        ws.push(w);
    }
    let total = Traced::sum(&ws).ok_or("the mix has no class")?;
    let sum = Traced::sum(&terms).ok_or("the mix has no class")?;
    Ok((&sum / &total).map(|x| x * 100.0))
}

#[derive(Debug)]
pub struct Model {
    pub rows: Vec<Row>,
    pub classes: Vec<ClassView>,
    pub mixes: Vec<MixView>,
}

impl Model {
    pub fn build(inputs: &Inputs) -> Model {
        let rows = rows(&inputs.baseline);
        let cb = class_bytes(&rows);
        let mut classes = Vec::new();
        for class in &inputs.baseline.run.classes {
            let Some(bytes) = cb.get(class) else { continue };
            let best = best_incumbent(&rows, class);
            let ultra = find(&rows, ULTRA, class)
                .and_then(Row::measured)
                .map(|m| m.size_bytes.clone());
            let est = estimate(class, &inputs.probes, bytes, best.as_ref());
            classes.push(ClassView {
                class: class.clone(),
                class_bytes: bytes.clone(),
                best,
                ultra,
                est,
            });
        }
        let by_class: BTreeMap<&str, &ClassView> =
            classes.iter().map(|c| (c.class.as_str(), c)).collect();
        let mixes = inputs
            .mixes
            .mix
            .iter()
            .map(|mix| {
                let missing = missing_classes(mix, &cb);
                let src = inputs.mixes_src;
                let guard = |r: Result<Traced, String>| {
                    if missing.is_empty() {
                        r
                    } else {
                        Err(format!("not in the corpus: {}", missing.join(", ")))
                    }
                };
                MixView {
                    name: mix.name.clone(),
                    best: if missing.is_empty() {
                        mix_best_single(&rows, mix, src)
                    } else {
                        Err(format!("not in the corpus: {}", missing.join(", ")))
                    },
                    best_per_class: guard(mix_ratio(mix, src, &by_class, |v| {
                        v.best
                            .as_ref()
                            .map(|b| b.bytes.clone())
                            .ok_or("no measured incumbent".into())
                    })),
                    ultra: guard(mix_ratio(mix, src, &by_class, |v| {
                        v.ultra.clone().ok_or("no 7z/ultra result".into())
                    })),
                    est: guard(mix_ratio(mix, src, &by_class, |v| {
                        v.est
                            .as_ref()
                            .map(|e| e.bytes.clone())
                            .map_err(|e| e.clone())
                    })),
                }
            })
            .collect();
        Model {
            rows,
            classes,
            mixes,
        }
    }

    pub fn class(&self, name: &str) -> Option<&ClassView> {
        self.classes.iter().find(|c| c.class == name)
    }
}

// ---------------------------------------------------------------------------------------------
// Gates (D-07)

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Pass,
    Fail,
    NotEvaluable(String),
}

impl Verdict {
    pub fn word(&self) -> String {
        match self {
            Verdict::Pass => "PASS".into(),
            Verdict::Fail => "FAIL".into(),
            Verdict::NotEvaluable(why) => format!("not evaluable: {why}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GateRow {
    pub id: &'static str,
    /// The rule, in words (thresholds included).
    pub title: String,
    /// One traced number per line.
    pub numbers: Vec<String>,
    /// Sentences printed under the table: failed, skipped or one-measurement inputs, reasons.
    pub notes: Vec<String>,
    pub verdict: Verdict,
}

pub const G1_VS_BEST_PCT: f64 = 90.0;
pub const G1_VS_ULTRA_PCT: f64 = 85.0;
pub const G2_VS_BEST_PCT: f64 = 50.0;
pub const G3_MIN_PCT: f64 = 80.0;

/// `a <= limit% of b`, exactly for byte counts.
pub fn within(a: f64, b: f64, limit_pct: f64) -> bool {
    a * 100.0 <= limit_pct * b
}

/// `a >= pct% of b`.
pub fn at_least_pct(a: f64, b: f64, pct: f64) -> bool {
    a * 100.0 >= pct * b
}

pub const G1_CLASSES: [&str; 3] = ["photo-jpeg", "photo-jpeg-edited", "office-pdf"];

fn pct1(v: f64) -> String {
    format!("{v:.1}%")
}
fn bytes_s(v: f64) -> String {
    format!("{v:.0} bytes")
}
fn mbps_s(v: f64) -> String {
    format!("{v:.1} MB/s")
}

fn ratio_pct(a: &Traced, b: &Traced) -> Traced {
    (a / b).map(|x| x * 100.0)
}

fn gate1(m: &Model) -> GateRow {
    let title = format!(
        "G1 photo/document size: on photo-jpeg, photo-jpeg-edited and office-pdf together, \
         estimate at most {G1_VS_BEST_PCT:.0}% of the best measured incumbent (the best single \
         tool x setting over the combined bytes; D-07's fallback wording: WinZip and PowerArchiver \
         were not measured) and at most {G1_VS_ULTRA_PCT:.0}% of 7-Zip Ultra; the sum of the \
         per-class bests (stricter) and the three classes alone are shown for information"
    );
    let mut numbers = Vec::new();
    let notes: Vec<String> = problem_rows(&m.rows, &G1_CLASSES)
        .map(|p| format!("G1 inputs, {p}"))
        .into_iter()
        .collect();
    let mut parts: Vec<(&str, Traced, Traced, Traced)> = Vec::new();
    let mut why: Option<String> = None;
    for c in G1_CLASSES {
        let Some(v) = m.class(c) else {
            why.get_or_insert(format!("class {c} is not in the corpus"));
            continue;
        };
        match (&v.est, &v.best, &v.ultra) {
            (Ok(e), Some(b), Some(u)) => {
                parts.push((c, e.bytes.clone(), b.bytes.clone(), u.clone()))
            }
            (Err(e), _, _) => {
                why.get_or_insert(format!("estimate for {c}: {e}"));
            }
            (_, None, _) => {
                why.get_or_insert(format!("no measured incumbent for {c}"));
            }
            (_, _, None) => {
                why.get_or_insert(format!("no 7z/ultra result for {c}"));
            }
        }
    }
    if let Some(w) = why {
        return GateRow {
            id: "G1",
            title,
            numbers,
            notes,
            verdict: Verdict::NotEvaluable(w),
        };
    }
    let (Some(e), Some(sum_b), Some(u), Some(single)) = (
        Traced::sum(parts.iter().map(|p| &p.1)),
        Traced::sum(parts.iter().map(|p| &p.2)),
        Traced::sum(parts.iter().map(|p| &p.3)),
        best_single(&m.rows, &G1_CLASSES),
    ) else {
        return GateRow {
            id: "G1",
            title,
            numbers,
            notes,
            verdict: Verdict::NotEvaluable("no tool has a measured result on every class".into()),
        };
    };
    let b = single.bytes.clone();
    let vs_best = ratio_pct(&e, &b);
    let vs_ultra = ratio_pct(&e, &u);
    numbers.push(format!("combined estimate: {}", e.show(bytes_s)));
    numbers.push(format!(
        "combined best single tool ({}): {}",
        single.name,
        b.show(bytes_s)
    ));
    numbers.push(format!("combined 7-Zip Ultra: {}", u.show(bytes_s)));
    numbers.push(format!(
        "combined estimate / best single tool: {}",
        vs_best.show(pct1)
    ));
    numbers.push(format!(
        "combined estimate / 7-Zip Ultra: {}",
        vs_ultra.show(pct1)
    ));
    numbers.push(format!(
        "stricter, sum of the per-class bests (no single tool): {}",
        sum_b.show(bytes_s)
    ));
    numbers.push(format!(
        "combined estimate / sum of the per-class bests: {}",
        ratio_pct(&e, &sum_b).show(pct1)
    ));
    for (c, e, b, u) in &parts {
        numbers.push(format!(
            "{c} alone, estimate / best incumbent: {}",
            ratio_pct(e, b).show(pct1)
        ));
        numbers.push(format!(
            "{c} alone, estimate / 7-Zip Ultra: {}",
            ratio_pct(e, u).show(pct1)
        ));
    }
    let pass =
        within(e.value, b.value, G1_VS_BEST_PCT) && within(e.value, u.value, G1_VS_ULTRA_PCT);
    GateRow {
        id: "G1",
        title,
        numbers,
        notes,
        verdict: if pass { Verdict::Pass } else { Verdict::Fail },
    }
}

fn gate2(m: &Model) -> GateRow {
    let title = format!(
        "G2 versioned backup: on backup-versions, estimate at most {G2_VS_BEST_PCT:.0}% of the best \
         measured incumbent"
    );
    let mut numbers = Vec::new();
    let verdict = match m.class("backup-versions") {
        None => Verdict::NotEvaluable("class backup-versions is not in the corpus".into()),
        Some(v) => match (&v.est, &v.best) {
            (Err(e), _) => Verdict::NotEvaluable(format!("estimate: {e}")),
            (_, None) => Verdict::NotEvaluable("no measured incumbent".into()),
            (Ok(e), Some(b)) => {
                numbers.push(format!("estimate: {}", e.bytes.show(bytes_s)));
                numbers.push(format!(
                    "best incumbent ({}): {}",
                    b.name,
                    b.bytes.show(bytes_s)
                ));
                numbers.push(format!(
                    "estimate / best incumbent: {}",
                    ratio_pct(&e.bytes, &b.bytes).show(pct1)
                ));
                if within(e.bytes.value, b.bytes.value, G2_VS_BEST_PCT) {
                    Verdict::Pass
                } else {
                    Verdict::Fail
                }
            }
        },
    };
    let notes = problem_rows(&m.rows, &["backup-versions"])
        .map(|p| format!("G2 inputs, {p}"))
        .into_iter()
        .collect();
    GateRow {
        id: "G2",
        title,
        numbers,
        notes,
        verdict,
    }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n - 1 - n / 2] + v[n / 2]) / 2.0
    })
}

/// The class's own raw read rate: median over the passes of bytes / (read + open seconds), or of
/// bytes / read seconds when `with_opens` is false.
pub fn raw_rate(p: &Probes, class: &str, with_opens: bool) -> Option<Traced> {
    let pf = p.entropy_gate.as_ref()?;
    let secs = |c: &entropy_gate::RawClass| {
        if with_opens {
            c.seconds + c.open_seconds
        } else {
            c.seconds
        }
    };
    let rates: Vec<f64> = pf
        .env
        .data
        .raw_read
        .passes
        .iter()
        .filter_map(|pass| pass.classes.iter().find(|c| c.class == class))
        .filter(|c| c.bytes > 0 && secs(c) > 0.0)
        .map(|c| c.bytes as f64 / 1e6 / secs(c))
        .collect();
    median(rates).map(|v| Traced::new(v, pf.src))
}

/// The video class's raw read rate including opens (the figure of the gate).
pub fn raw_video_rate(p: &Probes, class: &str) -> Option<Traced> {
    raw_rate(p, class, true)
}

fn gate3(m: &Model, p: &Probes) -> GateRow {
    let title = format!(
        "G3 video store speed: the store tool's compress MB/s on video (a real program reading and \
         writing the files) is at least {G3_MIN_PCT:.0}% of the video class's raw read rate"
    );
    let mut numbers = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let store_row = find(&m.rows, STORE, "video");
    let store = store_row.and_then(Row::measured);
    if let Some(r) = store_row {
        match &r.state {
            RowState::Failed { step, reason, .. } => notes.push(format!(
                "G3 input: store on video failed ({step}): {reason}"
            )),
            RowState::Skipped { reason, .. } => {
                notes.push(format!("G3 input: store on video was skipped: {reason}"))
            }
            RowState::Measured(x) => {
                if let Some(why) = &x.single {
                    notes.push(format!(
                        "G3 input: store on video rests on one measurement ({why})"
                    ));
                }
            }
        }
    }
    let star = if store.is_some_and(|s| s.single.is_some()) {
        "*"
    } else {
        ""
    };
    let raw = raw_video_rate(p, "video");
    let verdict = match (store, &raw) {
        (None, _) => Verdict::NotEvaluable("no measured store result on class video".into()),
        (_, None) => {
            Verdict::NotEvaluable("probe entropy-gate has no raw read rate for video".into())
        }
        (Some(s), Some(r)) => {
            let cb = store_row.map(|x| x.class_bytes.clone());
            numbers.push(format!(
                "store{star} compress on video: {}",
                s.compress_mbps.show(mbps_s)
            ));
            if let Some(cb) = &cb {
                numbers.push(format!("video class bytes: {}", cb.show(bytes_s)));
            }
            numbers.push(format!(
                "store{star} median compress wall seconds on video: {}",
                s.compress_seconds.show(|v| format!("{v:.3} s"))
            ));
            numbers.push(format!(
                "store{star} first repeat compress on video: {}",
                s.first_compress_mbps.show(mbps_s)
            ));
            if let Some(e) = find(&m.rows, STORE, "encrypted-random").and_then(Row::measured) {
                numbers.push(format!(
                    "store{} compress on encrypted-random: {}",
                    if e.single.is_some() { "*" } else { "" },
                    e.compress_mbps.show(mbps_s)
                ));
            }
            numbers.push(format!(
                "raw read of video, median of the passes, including opens: {}",
                r.show(mbps_s)
            ));
            if let Some(ro) = raw_rate(p, "video", false) {
                numbers.push(format!(
                    "raw read of video, median of the passes, read only: {}",
                    ro.show(mbps_s)
                ));
            }
            let ratio = ratio_pct(&s.compress_mbps, r);
            numbers.push(format!("store / raw read: {}", ratio.show(pct1)));
            if at_least_pct(s.compress_mbps.value, r.value, G3_MIN_PCT) {
                Verdict::Pass
            } else {
                Verdict::Fail
            }
        }
    };
    if let Some(pf) = &p.entropy_gate {
        for c in &pf.env.data.gate_cost {
            if c.seconds > 0.0 {
                let rate = Traced::new(c.bytes as f64 / 1e6 / c.seconds, pf.src);
                numbers.push(format!(
                    "gate cost, {} (single thread, information only): {}",
                    c.gate,
                    rate.show(mbps_s)
                ));
                if let Some(r) = &raw {
                    numbers.push(format!(
                        "gate cost, {}, gate rate as a share of the raw read rate: {}",
                        c.gate,
                        ratio_pct(&rate, r).show(pct1)
                    ));
                }
            }
        }
    }
    GateRow {
        id: "G3",
        title,
        numbers,
        notes,
        verdict,
    }
}

fn gate4(m: &Model, inputs: &Inputs) -> GateRow {
    let title = "G4 fast-tier extraction (proxy): the baseline zstd/3 extraction MB/s is at least \
                 the 7z/mx5 extraction MB/s, blended over each disk mix"
        .to_string();
    let mut numbers = Vec::new();
    let mut notes = Vec::new();
    let (mut any_fail, mut why) = (false, None::<String>);
    for mix in &inputs.mixes.mix {
        let z = blend(&m.rows, FAST, mix, inputs.mixes_src);
        let s = blend(&m.rows, MX5, mix, inputs.mixes_src);
        match (z, s) {
            (Ok(z), Ok(s)) => {
                numbers.push(format!(
                    "{}: zstd/3{} extract: {}",
                    mix.name,
                    if z.single { "*" } else { "" },
                    z.extract_mbps.show(mbps_s)
                ));
                numbers.push(format!(
                    "{}: 7z/mx5{} extract: {}",
                    mix.name,
                    if s.single { "*" } else { "" },
                    s.extract_mbps.show(mbps_s)
                ));
                if z.extract_mbps.value < s.extract_mbps.value {
                    any_fail = true;
                }
            }
            (z, s) => {
                let e = z.err().or(s.err()).unwrap_or_default();
                let line = format!("mix {}: {e}", mix.name);
                notes.push(format!("G4: not evaluable on {line}"));
                why.get_or_insert(line);
            }
        }
    }
    let verdict = if any_fail {
        Verdict::Fail
    } else if let Some(w) = why {
        Verdict::NotEvaluable(w)
    } else {
        Verdict::Pass
    };
    GateRow {
        id: "G4",
        title,
        numbers,
        notes,
        verdict,
    }
}

pub fn gates(m: &Model, inputs: &Inputs) -> Vec<GateRow> {
    vec![
        gate1(m),
        gate2(m),
        gate3(m, &inputs.probes),
        gate4(m, inputs),
    ]
}

/// Why the inputs are unclean (empty: all clean): a baseline without the settle pause (D-21), an
/// input directory outside `bench/results`, a dirty or unknown build, an unoptimised probe build.
pub fn unclean_reasons(i: &Inputs) -> Vec<String> {
    let b = &i.baseline;
    let mut v = Vec::new();
    if b.run.settle_ms_per_1000_files == 0 {
        v.push("the baseline has no settle pause (D-21)".to_string());
    }
    for d in &i.outside {
        v.push(format!("`{d}` is not under bench/results"));
    }
    if b.host.dirty_build_allowed || !crate::run::host::build_is_clean(&b.host.git_commit) {
        v.push("the baseline was run from a dirty or unknown build".to_string());
    }
    for p in i.probes.present() {
        if !p.release || !crate::run::host::build_is_clean(&p.build) {
            v.push(format!(
                "probe {} is from a dirty, unknown or unoptimised build",
                p.name
            ));
        }
    }
    v
}

/// The proposal line printed under the gate table.
pub fn verdict_line(gates: &[GateRow]) -> String {
    let ids = |f: &dyn Fn(&Verdict) -> bool| -> Vec<&str> {
        gates
            .iter()
            .filter(|g| f(&g.verdict))
            .map(|g| g.id)
            .collect()
    };
    let failing = ids(&|v| *v == Verdict::Fail);
    let open = ids(&|v| matches!(v, Verdict::NotEvaluable(_)));
    if failing.is_empty() && open.is_empty() {
        return "Verdict proposal: GO".into();
    }
    let mut s = String::from("NO-GO proposal");
    if !failing.is_empty() {
        s.push_str(&format!(": failing gates: {}", failing.join(", ")));
    }
    if !open.is_empty() {
        s.push_str(&format!(
            "{} not evaluable (the data is incomplete, not a failure): {}",
            if failing.is_empty() { ":" } else { ";" },
            open.join(", ")
        ));
    }
    s
}
