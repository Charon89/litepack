//! Baseline tool runner (PLAN P0-3).
//!
//! ```text
//! lpk-bench run --list-tools [--tools a,b] [--catalogue FILE] [--local FILE]
//! lpk-bench run --validate <results-dir>
//! lpk-bench run --tools <list|all> ...        (the measuring loop: not implemented yet)
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
pub mod discover;
#[allow(dead_code)] // consumed by the run loop (next sub-task)
pub mod host;
#[allow(dead_code)] // consumed by the run loop (next sub-task)
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
    /// Corpus profile (the measuring loop is not implemented yet)
    #[arg(long)]
    pub profile: Option<String>,
    /// Repeats per measurement (the measuring loop is not implemented yet)
    #[arg(long)]
    pub repeats: Option<u32>,
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
        list_tools(&args)
    } else {
        eprintln!(
            "error: `lpk-bench run` (measuring) is not implemented yet (PLAN task P0-3); \
             no measurement was made. Available: --list-tools, --validate <dir>"
        );
        return ExitCode::FAILURE;
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
