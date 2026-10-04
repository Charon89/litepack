//! The Phase 0 report (PLAN P0-5): `lpk-bench report`.
//!
//! ```text
//! lpk-bench report --results <DIR> [--probes <DIR>]... [--mixes FILE] [--out FILE] [--allow-unclean]
//! ```
//!
//! One Markdown file from committed JSON: the baseline directory written by `lpk-bench run` and
//! the directories with probe files written by `lpk-bench probe`. Every input directory is
//! validated first ([`crate::run::validate::validate_dir`]); any problem refuses the report.
//!
//! * [`model`]: inputs, the baseline rows, blends over the disk mixes, the estimates and the
//!   gates, all as [`traced::Traced`] values (a number with the source files it came from).
//! * [`render`]: the Markdown; [`mixes`]: `bench/report-mixes.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::de::DeserializeOwned;

use crate::probe::{self, Envelope};
use crate::run::host::build_is_clean;
use crate::run::result::{HostFile, RunFile, ToolResult, ToolsFile};
use crate::run::validate::validate_dir;

pub mod mixes;
pub mod model;
pub mod render;
pub mod traced;

#[cfg(test)]
mod tests;

use model::{Baseline, Inputs, ProbeFile, Probes};
use traced::SourceId;

pub const DEFAULT_MIXES: &str = "bench/report-mixes.toml";

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// A baseline results directory written by `lpk-bench run` (`<date>-<host>[-<n>]`); repeatable: rows of several
    /// directories of the same corpus on the same host are pooled
    #[arg(long, value_name = "DIR", required = true)]
    pub results: Vec<PathBuf>,
    /// A directory with probe files written by `lpk-bench probe` (repeatable; optional: a missing
    /// probe leaves its sections and estimates marked not available)
    #[arg(long, value_name = "DIR")]
    pub probes: Vec<PathBuf>,
    /// The disk-mix definitions
    #[arg(long, value_name = "FILE", default_value = DEFAULT_MIXES)]
    pub mixes: PathBuf,
    /// Where to write the report (default: bench/reports/phase0-<date of the baseline run>.md)
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
    /// Accept results from a dirty, unknown or unoptimised build (the report marks them)
    #[arg(long)]
    pub allow_unclean: bool,
}

/// The list of source files; an id is the position + 1.
#[derive(Debug, Default)]
struct Sources {
    list: Vec<String>,
}

impl Sources {
    fn add(&mut self, label: String) -> SourceId {
        match self.list.iter().position(|l| *l == label) {
            Some(i) => i + 1,
            None => {
                self.list.push(label);
                self.list.len()
            }
        }
    }
}

fn read_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = read_text(path)?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn dir_name(dir: &Path) -> String {
    std::fs::canonicalize(dir)
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .or_else(|| dir.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The canonical path of `path` relative to the repository root (the nearest ancestor holding
/// `.git`), as `/`-separated components; `None` when there is no repository above it.
fn repo_components(path: &Path) -> (PathBuf, Option<Vec<String>>) {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let rel = canon
        .ancestors()
        .skip(1)
        .find(|a| a.join(".git").exists())
        .and_then(|root| canon.strip_prefix(root).ok())
        .map(|rel| {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect()
        });
    (canon, rel)
}

/// How an input directory is labelled in the source list, and whether it lies inside
/// `bench/results`: only when its path relative to the repository root is exactly
/// `bench/results/<dir>`. The label is that relative path; with no repository above the
/// directory it is the last two components and the directory counts as outside.
fn dir_label(dir: &Path) -> (String, bool) {
    match repo_components(dir) {
        (_, Some(c)) => {
            let under = c.len() == 3 && c[0] == "bench" && c[1] == "results";
            (c.join("/"), under)
        }
        (canon, None) => {
            let leaf = |p: Option<&Path>| {
                p.and_then(Path::file_name)
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            let (name, up) = (leaf(Some(&canon)), leaf(canon.parent()));
            (
                if up.is_empty() {
                    name
                } else {
                    format!("{up}/{name}")
                },
                false,
            )
        }
    }
}

/// The label of a file inside the repository (relative to its root), or its bare file name
/// when it lies outside any repository; the flag says whether it is inside one.
fn file_label(file: &Path) -> (String, bool) {
    match repo_components(file) {
        (_, Some(c)) => (c.join("/"), true),
        (canon, None) => (
            canon
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            false,
        ),
    }
}

/// Refuse a directory the validator has any problem with, or that is not one results directory.
fn check_dir(dir: &Path) -> Result<()> {
    if !dir.join("host.json").is_file() {
        bail!(
            "{}: no host.json; give one `<date>-<host>` results directory, not its parent",
            dir.display()
        );
    }
    let report = validate_dir(dir)?;
    if !report.problems.is_empty() {
        let shown: Vec<&str> = report
            .problems
            .iter()
            .take(20)
            .map(String::as_str)
            .collect();
        bail!(
            "{} does not validate ({} problem(s)); run `lpk-bench run --validate`. First: {}",
            dir.display(),
            report.problems.len(),
            shown.join("; ")
        );
    }
    Ok(())
}

fn load_baseline(dir: &Path, sources: &mut Sources, outside: &mut Vec<String>) -> Result<Baseline> {
    check_dir(dir)?;
    let name = dir_name(dir);
    let (label, under) = dir_label(dir);
    if !under {
        outside.push(label.clone());
    }
    let src = |sources: &mut Sources, file: &str| sources.add(format!("{label}/{file}"));
    if !dir.join("run.json").is_file() {
        bail!(
            "{}: no run.json: this is not a baseline directory",
            dir.display()
        );
    }
    let host: HostFile = read_json(&dir.join("host.json"))?;
    let tools: ToolsFile = read_json(&dir.join("tools.json"))?;
    let run: RunFile = read_json(&dir.join("run.json"))?;
    let host_src = src(sources, "host.json");
    let tools_src = src(sources, "tools.json");
    let run_src = src(sources, "run.json");
    let mut results = Vec::new();
    for c in &run.combinations {
        let file = format!("{}-{}-{}.json", c.tool, c.setting, c.class);
        let r: ToolResult = read_json(&dir.join(&file))?;
        results.push((r, src(sources, &file)));
    }
    if results.is_empty() {
        bail!("{}: the run lists no combination", dir.display());
    }
    Ok(Baseline {
        dir: name,
        host,
        host_src,
        tools,
        tools_src,
        run,
        run_src,
        results,
        pooled: vec![],
        tool_srcs: BTreeMap::new(),
    })
}

/// Add the probe files of `dir` to `probes`; `host` and the corpus must agree with the baseline.
fn load_probes_dir(
    dir: &Path,
    base: &Baseline,
    allow_unclean: bool,
    probes: &mut Probes,
    sources: &mut Sources,
    seen: &mut BTreeMap<String, String>,
    outside: &mut Vec<String>,
) -> Result<()> {
    check_dir(dir)?;
    let (name, under) = dir_label(dir);
    if !under {
        if !allow_unclean {
            bail!(
                "{}: not under bench/results; use --allow-unclean to accept and mark it",
                dir.display()
            );
        }
        outside.push(name.clone());
    }
    let host: HostFile = read_json(&dir.join("host.json"))?;
    if host.host != base.host.host {
        bail!(
            "{}: taken on host `{}`, the baseline on `{}`; a report needs one machine",
            dir.display(),
            host.host,
            base.host.host
        );
    }
    let first = &base.results[0].0;
    macro_rules! load {
        ($field:ident, $probe:expr, $module:ident) => {{
            let file = format!("probe-{}.json", $probe);
            let path = dir.join(&file);
            if path.is_file() {
                let json = read_text(&path)?;
                let env: Envelope<probe::$module::Data> = serde_json::from_str(&json)
                    .with_context(|| format!("parsing {}", path.display()))?;
                if probes.$field.is_some() {
                    bail!(
                        "probe `{}` is given twice: in `{}` and in `{name}`",
                        $probe,
                        seen.get($probe).map_or("?", String::as_str)
                    );
                }
                seen.insert($probe.to_string(), name.clone());
                if env.host != base.host.host {
                    bail!("{file}: taken on host `{}`, not `{}`", env.host, base.host.host);
                }
                if env.corpus.manifest_blake3 != first.corpus.manifest_blake3
                    || env.corpus.profile != first.corpus.profile
                {
                    bail!(
                        "{name}/{file}: another corpus than the baseline (profile `{}`, manifest {}; \
                         the baseline: `{}`, {})",
                        env.corpus.profile,
                        env.corpus.manifest_blake3,
                        first.corpus.profile,
                        first.corpus.manifest_blake3
                    );
                }
                let optimised = env.build_profile.is_release() && !env.build_profile.allow_debug_build;
                if (!optimised || !build_is_clean(&env.build)) && !allow_unclean {
                    bail!(
                        "{name}/{file}: built as `{}` ({} profile{}): dirty, unknown or unoptimised \
                         builds are refused; use --allow-unclean to accept and mark them",
                        env.build,
                        env.build_profile.profile,
                        if env.build_profile.allow_debug_build { ", --allow-debug-build" } else { "" }
                    );
                }
                let src = sources.add(format!("{name}/{file}"));
                probes.$field = Some(ProbeFile { env, json, src });
            }
        }};
    }
    load!(jpeg, "jpeg", jpeg);
    load!(deflate, "deflate", deflate);
    load!(dedup, "dedup", dedup);
    load!(text, "text", text);
    load!(weights, "weights", weights);
    load!(entropy_gate, "entropy-gate", entropy_gate);
    Ok(())
}

/// Load one results directory and apply the cleanliness rules of a baseline (settle pause,
/// location, a clean build) unless `allow_unclean`.
fn load_baseline_checked(
    dir: &Path,
    allow_unclean: bool,
    sources: &mut Sources,
    outside: &mut Vec<String>,
) -> Result<Baseline> {
    let before = outside.len();
    let b = load_baseline(dir, sources, outside)?;
    if !allow_unclean && b.run.settle_ms_per_1000_files == 0 {
        bail!(
            "{}: run.json has no settle pause (settle_ms_per_1000_files is 0 or absent; D-21 rules \
             such baselines out for the report); use --allow-unclean to accept and mark it",
            dir.display()
        );
    }
    if !allow_unclean && outside.len() > before {
        bail!(
            "{}: not under bench/results; use --allow-unclean to accept and mark it",
            dir.display()
        );
    }
    if !allow_unclean && (b.host.dirty_build_allowed || !build_is_clean(&b.host.git_commit)) {
        bail!(
            "{}: the baseline was run from build `{}` (dirty or unknown); use --allow-unclean to \
             accept and mark it",
            dir.display(),
            b.host.git_commit
        );
    }
    Ok(b)
}

fn pool_dir(label: String, b: &Baseline) -> model::PoolDir {
    model::PoolDir {
        label,
        build: b.host.git_commit.clone(),
        tools_src: b.tools_src,
        run_src: b.run_src,
        catalogue_blake3: b.run.catalogue_blake3.clone(),
        repeats: b.run.repeats_requested,
        long_run_s: b.run.long_run_s,
        threads: b.run.threads,
        settle_ms: b.run.settle_ms_per_1000_files,
        antivirus_changed: b.run.antivirus_changed,
    }
}

/// Does `b` hold a result row of tool `id`?
fn has_rows(b: &[(ToolResult, SourceId)], id: &str) -> bool {
    b.iter().any(|(r, _)| r.tool.id == id)
}

/// Pool `next` into `base`: same corpus manifest and host required; rows keep their own source
/// ids. `tools.json` entries are merged: a tool's entry (and version) comes from the directory
/// that holds its rows, two directories with rows of one tool must agree on the version, a
/// `dedup` of `None` is unknown and takes the other side's flag, and `Some(a)` against
/// `Some(b)` is refused. Every merged entry keeps the source files it came from.
fn pool(base: &mut Baseline, next: Baseline, dir: &Path, allow_unclean: bool) -> Result<()> {
    let name = dir.display();
    let (Some((first, _)), Some((other, _))) = (base.results.first(), next.results.first()) else {
        bail!("{name}: no results to pool");
    };
    if other.corpus.manifest_blake3 != first.corpus.manifest_blake3 {
        bail!(
            "{name}: corpus manifest BLAKE3 {} differs from the first directory's {}; pooled \
             directories must measure the same corpus",
            other.corpus.manifest_blake3,
            first.corpus.manifest_blake3
        );
    }
    if next.host.host != base.host.host {
        bail!(
            "{name}: taken on host `{}`, the first directory on `{}`; a report needs one machine",
            next.host.host,
            base.host.host
        );
    }
    if !allow_unclean && next.run.threads != base.pooled[0].threads {
        bail!(
            "{name}: ran at {} threads, the first directory at {}; use --allow-unclean to accept \
             and mark it",
            next.run.threads,
            base.pooled[0].threads
        );
    }
    let pd = pool_dir(dir_label(dir).0, &next);
    let (base_rows, next_rows): (Vec<String>, Vec<String>) = (
        base.tools
            .tools
            .iter()
            .filter(|t| has_rows(&base.results, &t.id))
            .map(|t| t.id.clone())
            .collect(),
        next.tools
            .tools
            .iter()
            .filter(|t| has_rows(&next.results, &t.id))
            .map(|t| t.id.clone())
            .collect(),
    );
    for t in next.tools.tools {
        let (in_base, in_next) = (base_rows.contains(&t.id), next_rows.contains(&t.id));
        let Some(slot) = base.tools.tools.iter().position(|b| b.id == t.id) else {
            base.tool_srcs
                .entry(t.id.clone())
                .or_default()
                .insert(next.tools_src);
            base.tools.tools.push(t);
            continue;
        };
        let have = base.tools.tools[slot].clone();
        if in_base && in_next && have.version != t.version {
            bail!(
                "{name}: tool `{}` has rows at version {:?} here and {:?} in an earlier directory",
                t.id,
                t.version,
                have.version
            );
        }
        if have.dedup.is_some() && t.dedup.is_some() && have.dedup != t.dedup {
            bail!(
                "{name}: tool `{}` has dedup {:?} here and {:?} in another directory",
                t.id,
                t.dedup,
                have.dedup
            );
        }
        let id = t.id.clone();
        let srcs = base.tool_srcs.entry(id).or_default();
        let (mut merged, take_next) = if in_next && !in_base {
            (t.clone(), true)
        } else {
            (have.clone(), false)
        };
        let earlier = srcs.clone();
        if take_next {
            srcs.clear();
            srcs.insert(next.tools_src);
        }
        if merged.dedup.is_none() {
            let (flag, from) = if take_next {
                (have.dedup, earlier)
            } else {
                (t.dedup, BTreeSet::from([next.tools_src]))
            };
            if flag.is_some() {
                merged.dedup = flag;
                srcs.extend(from);
            }
        }
        base.tools.tools[slot] = merged;
    }
    for c in next.run.combinations {
        if base
            .run
            .combinations
            .iter()
            .any(|b| (&b.tool, &b.setting, &b.class) == (&c.tool, &c.setting, &c.class))
        {
            bail!(
                "{name}: combination {}/{}/{} is also in an earlier directory",
                c.tool,
                c.setting,
                c.class
            );
        }
        base.run.combinations.push(c);
    }
    for class in next.run.classes {
        if !base.run.classes.contains(&class) {
            base.run.classes.push(class);
        }
    }
    base.run.settle_ms_per_1000_files = base
        .run
        .settle_ms_per_1000_files
        .min(next.run.settle_ms_per_1000_files);
    base.run.antivirus_changed |= next.run.antivirus_changed;
    base.host.dirty_build_allowed |= next.host.dirty_build_allowed;
    base.pooled.push(pd);
    base.results.extend(next.results);
    Ok(())
}

/// Read and validate every input.
pub fn load(args: &ReportArgs) -> Result<Inputs> {
    let mut sources = Sources::default();
    let mut outside = Vec::new();
    let Some((first_dir, more)) = args.results.split_first() else {
        bail!("no --results directory given");
    };
    let mut baseline =
        load_baseline_checked(first_dir, args.allow_unclean, &mut sources, &mut outside)?;
    if !more.is_empty() {
        let first = pool_dir(dir_label(first_dir).0, &baseline);
        baseline.pooled.push(first);
        for t in &baseline.tools.tools {
            baseline
                .tool_srcs
                .entry(t.id.clone())
                .or_default()
                .insert(baseline.tools_src);
        }
        for dir in more {
            let next = load_baseline_checked(dir, args.allow_unclean, &mut sources, &mut outside)?;
            pool(&mut baseline, next, dir, args.allow_unclean)?;
        }
    }
    let mut probes = Probes::default();
    let mut seen = BTreeMap::new();
    for dir in &args.probes {
        load_probes_dir(
            dir,
            &baseline,
            args.allow_unclean,
            &mut probes,
            &mut sources,
            &mut seen,
            &mut outside,
        )?;
    }
    if !args.allow_unclean {
        let reasons = model::pool_reasons(&baseline);
        if !reasons.is_empty() {
            bail!(
                "{}; use --allow-unclean to accept and mark it",
                reasons.join("; ")
            );
        }
    }
    let mixes_text = read_text(&args.mixes)?;
    let mixes =
        mixes::Mixes::parse(&mixes_text).with_context(|| format!("in {}", args.mixes.display()))?;
    let (label, in_repo) = file_label(&args.mixes);
    let mixes_src = sources.add(label);
    Ok(Inputs {
        baseline,
        probes,
        mixes,
        mixes_src,
        mixes_text,
        sources: sources.list,
        outside,
        mixes_outside: !in_repo,
    })
}

/// The report text for validated inputs.
pub fn report_text(inputs: &Inputs) -> String {
    render::render(inputs)
}

fn run_inner(args: &ReportArgs) -> Result<()> {
    let inputs = load(args)?;
    let text = report_text(&inputs);
    let out = args.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "bench/reports/phase0-{}.md",
            render::baseline_date(&inputs.baseline)
        ))
    });
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&out, text).with_context(|| format!("writing {}", out.display()))?;
    let model = model::Model::build(&inputs);
    let line = model::marked_verdict(&inputs, &model::gates(&model, &inputs));
    println!("wrote {}", out.display());
    println!("{line}");
    Ok(())
}

pub fn command(args: &ReportArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
