//! Baseline tool runner (PLAN P0-3).
//!
//! ```text
//! lpk-bench run --list-tools [--tools a,b] [--catalogue FILE] [--local FILE]
//! lpk-bench run --validate <results-dir>
//! lpk-bench run --tools <list|all> --profile <small|full> [--corpus DIR] [--classes a,b]
//!               [--repeats N] [--threads N] [--timeout-s N] [--results DIR] [--tmp DIR]
//!               [--allow-dirty-build]                    (the measuring loop, [`exec`])
//! lpk-bench run --compare <dirA> <dirB> [--max-diff-pct 3]   ([`compare`])
//! ```
//!
//! * [`catalogue`]: `bench/tools.toml` and the untracked `bench/tools.local.toml`.
//! * [`discover`]: finding each tool and reading its version.
//! * [`result`]: the result-file types; [`validate`]: checking a results directory against
//!   `bench/results/schema.json`; [`host`]: `host.json` contents and results-directory names.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Args;

pub mod catalogue;
pub mod compare;
pub mod discover;
pub mod exec;
pub mod host;
pub mod result;
pub mod validate;

use catalogue::{Catalogue, Local};

pub const DEFAULT_CATALOGUE: &str = "bench/tools.toml";
pub const DEFAULT_LOCAL: &str = "bench/tools.local.toml";

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Tools to use: comma-separated ids or `all` (default: all)
    #[arg(long, value_delimiter = ',')]
    pub tools: Vec<String>,
    /// Print one line per catalogue tool (found or skipped, version, path) and exit
    #[arg(long)]
    pub list_tools: bool,
    /// Validate a results directory against bench/results/schema.json and exit
    #[arg(long, value_name = "DIR")]
    pub validate: Option<PathBuf>,
    /// Corpus profile, `small` or `full`: selects bench/corpus/<profile> (default: small)
    #[arg(long)]
    pub profile: Option<String>,
    /// Corpus directory holding manifest.json (default: bench/corpus/<profile>)
    #[arg(long, value_name = "DIR")]
    pub corpus: Option<PathBuf>,
    /// Only these corpus classes, comma-separated (default: every class of the manifest)
    #[arg(long, value_delimiter = ',')]
    pub classes: Vec<String>,
    /// Repeats per combination (default: 3)
    #[arg(long)]
    pub repeats: Option<u32>,
    /// Thread count given to every tool (default: the machine's logical cores)
    #[arg(long)]
    pub threads: Option<u32>,
    /// Timeout of every compress or extract step, in seconds
    #[arg(long, value_name = "SECONDS", default_value_t = 3600)]
    pub timeout_s: u64,
    /// A combination whose first repeat (compress plus extract wall time) takes at least this many
    /// seconds is measured once and not repeated; the result records why
    #[arg(long, value_name = "SECONDS", default_value_t = 120)]
    pub long_run_s: u64,
    /// Where result directories are created
    #[arg(long, value_name = "DIR", default_value = "bench/results")]
    pub results: PathBuf,
    /// Where temporary files go (on the same volume as the corpus)
    #[arg(long, value_name = "DIR", default_value = "bench/tmp")]
    pub tmp: PathBuf,
    /// Write results from a dirty or unknown build (recorded in host.json)
    #[arg(long)]
    pub allow_dirty_build: bool,
    /// Compare two result directories of the same corpus and exit
    #[arg(long, num_args = 2, value_names = ["DIR_A", "DIR_B"])]
    pub compare: Vec<PathBuf>,
    /// Largest accepted time difference for --compare, in percent
    #[arg(long, value_name = "PERCENT", default_value_t = compare::DEFAULT_MAX_DIFF_PCT)]
    pub max_diff_pct: f64,
    /// Tool catalogue
    #[arg(long, value_name = "FILE", default_value = DEFAULT_CATALOGUE)]
    pub catalogue: PathBuf,
    /// Untracked per-machine overrides (optional file)
    #[arg(long, value_name = "FILE", default_value = DEFAULT_LOCAL)]
    pub local: PathBuf,
}

/// Load the catalogue and merge the local override file when it exists.
pub fn load_catalogue(catalogue: &Path, local: &Path) -> Result<(Catalogue, Local)> {
    let cat = Catalogue::load(catalogue)?;
    match std::fs::read_to_string(local) {
        Ok(text) => cat
            .with_local(&text)
            .with_context(|| format!("in {}", local.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((cat, Local::default())),
        Err(e) => Err(e).with_context(|| format!("reading {}", local.display())),
    }
}

/// Keep the requested tools, in catalogue order; unknown ids are errors.
pub fn select(cat: &Catalogue, wanted: &[String]) -> Result<Vec<String>> {
    if wanted.is_empty() || wanted.iter().any(|w| w == "all") {
        return Ok(cat.tools.iter().map(|t| t.id.clone()).collect());
    }
    for w in wanted {
        if cat.get(w).is_none() {
            let known: Vec<&str> = cat.tools.iter().map(|t| t.id.as_str()).collect();
            bail!("unknown tool `{w}` (catalogue has: {})", known.join(", "));
        }
    }
    Ok(cat
        .tools
        .iter()
        .filter(|t| wanted.contains(&t.id))
        .map(|t| t.id.clone())
        .collect())
}

fn list_tools(args: &RunArgs) -> Result<()> {
    let (cat, local) = load_catalogue(&args.catalogue, &args.local)?;
    let ids = select(&cat, &args.tools)?;
    let env = discover::Env::current();
    for id in ids {
        if let Some(tool) = cat.get(&id) {
            let status = discover::discover_tool(tool, &local, &env);
            let d = discover::Discovered {
                tool: tool.clone(),
                status,
                local_override: local.overridden.contains(&id),
            };
            println!("{}", discover::list_line(&d));
        }
    }
    Ok(())
}

fn validate_cmd(dir: &Path) -> Result<ExitCode> {
    let report = validate::validate_dir(dir)?;
    for n in &report.notes {
        println!("note: {n}");
    }
    for p in &report.problems {
        eprintln!("error: {p}");
    }
    if report.problems.is_empty() {
        println!(
            "{}: {} result file(s) plus host.json and tools.json validate against bench/results/schema.json",
            dir.display(),
            report.results
        );
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{} problem(s) in {}", report.problems.len(), dir.display());
        Ok(ExitCode::FAILURE)
    }
}

pub fn run(args: RunArgs) -> ExitCode {
    if let Some(dir) = &args.validate {
        return match validate_cmd(dir) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    let result = if args.list_tools {
        list_tools(&args).map(|()| ExitCode::SUCCESS)
    } else if let [a, b] = args.compare.as_slice() {
        compare::compare_command(a, b, args.max_diff_pct)
    } else {
        exec::measure_command(&args)
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
