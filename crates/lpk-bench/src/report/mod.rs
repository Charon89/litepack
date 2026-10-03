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
    /// A baseline results directory written by `lpk-bench run` (`<date>-<host>[-<n>]`)
    #[arg(long, value_name = "DIR")]
    pub results: PathBuf,
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

fn load_baseline(dir: &Path, sources: &mut Sources) -> Result<Baseline> {
    check_dir(dir)?;
    let name = dir_name(dir);
    let src = |sources: &mut Sources, file: &str| sources.add(format!("{name}/{file}"));
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
    })
}

/// Add the probe files of `dir` to `probes`; `host` and the corpus must agree with the baseline.
fn load_probes_dir(
    dir: &Path,
    base: &Baseline,
    allow_unclean: bool,
    probes: &mut Probes,
    sources: &mut Sources,
) -> Result<()> {
    check_dir(dir)?;
    let name = dir_name(dir);
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
                    bail!("probe `{}` is given twice", $probe);
                }
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

/// Read and validate every input.
pub fn load(args: &ReportArgs) -> Result<Inputs> {
    let mut sources = Sources::default();
    let baseline = load_baseline(&args.results, &mut sources)?;
    if !args.allow_unclean
        && (baseline.host.dirty_build_allowed || !build_is_clean(&baseline.host.git_commit))
    {
        bail!(
            "{}: the baseline was run from build `{}` (dirty or unknown); use --allow-unclean to \
             accept and mark it",
            args.results.display(),
            baseline.host.git_commit
        );
    }
    let mut probes = Probes::default();
    for dir in &args.probes {
        load_probes_dir(
            dir,
            &baseline,
            args.allow_unclean,
            &mut probes,
            &mut sources,
        )?;
    }
    let mixes_text = read_text(&args.mixes)?;
    let mixes =
        mixes::Mixes::parse(&mixes_text).with_context(|| format!("in {}", args.mixes.display()))?;
    let label = if args.mixes.is_relative() {
        args.mixes.to_string_lossy().replace('\\', "/")
    } else {
        args.mixes
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mixes_src = sources.add(label);
    Ok(Inputs {
        baseline,
        probes,
        mixes,
        mixes_src,
        mixes_text,
        sources: sources.list,
        allow_unclean: args.allow_unclean,
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
    let line = model::verdict_line(&model::gates(&model, &inputs));
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
