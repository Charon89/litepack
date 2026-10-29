//! Baseline tool runner (PLAN P0-3).
//!
//! ```text
//! lpk-bench run --list-tools [--tools a,b] [--catalogue FILE] [--local FILE]
//! lpk-bench run --validate <results-dir>
//! lpk-bench run --tools <list|all> --profile <small|full> [--corpus DIR] [--classes a,b]
//!               [--repeats N] [--threads N] [--timeout-s N] [--results DIR] [--tmp DIR]
//!               [--allow-dirty-build] [--keep-failed on|off] [--tar auto|system|lpk-bench]
//!               [--settings a,b]
//!               (the measuring loop, [`exec`])
//! lpk-bench run --compare <dirA> <dirB> [--max-diff-pct 3]   ([`compare`])
//! lpk-bench run --compare-sizes <golden-dir|bench/ci-golden.toml> <dir> [--max-growth-pct 1]
//!               (the size-regression gate of E8-2, [`size`])
//! lpk-bench tar -cf A -C DIR . | -cf A -T LIST | -xf A -C DIR   (hidden; [`harness_tar`])
//! ```
//!
//! * [`catalogue`]: `bench/tools.toml` and the untracked `bench/tools.local.toml`.
//! * [`discover`]: finding each tool and reading its version.
//! * [`result`]: the result-file types; [`validate`]: checking a results directory against
//!   `bench/results/schema.json`; [`host`]: `host.json` contents and results-directory names.
//! * [`harness_tar`]: the runner's own tar stream for a class whose names the system tar
//!   rejects (E2-23).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Args;

pub mod catalogue;
pub mod compare;
pub mod discover;
pub mod exec;
pub mod harness_tar;
pub mod host;
pub mod result;
pub mod size;
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
    /// Only these setting ids of the selected tools, comma-separated (default: every setting);
    /// an id no selected tool has is an error
    #[arg(long, value_delimiter = ',')]
    pub settings: Vec<String>,
    /// Corpus profile, `small`, `full` or `ci`: selects bench/corpus/<profile> (default: small)
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
    /// After the loop deletes a tree of extracted files, pause this many milliseconds per 1000
    /// files removed (minimum 250 ms, maximum 30 s) before the next timed step, so the file
    /// system settles; 0 disables the pause
    #[arg(
        long,
        value_name = "MS",
        default_value_t = 500,
        overrides_with = "settle_ms_per_1000_files"
    )]
    pub settle_ms_per_1000_files: u64,
    /// Where result directories are created
    #[arg(long, value_name = "DIR", default_value = "bench/results")]
    pub results: PathBuf,
    /// Where temporary files go (on the same volume as the corpus)
    #[arg(long, value_name = "DIR", default_value = "bench/tmp")]
    pub tmp: PathBuf,
    /// Write results from a dirty or unknown build (recorded in host.json)
    #[arg(long)]
    pub allow_dirty_build: bool,
    /// When a repeat fails verification, keep its archive and extracted tree (moved, outside any
    /// timing) under <tmp>/failed/<results dir>/<tool>-<setting>-<class>-r<repeat>/ and name that
    /// directory in the result (failed.evidence_dir); `off` deletes them like every other repeat
    #[arg(
        long,
        value_name = "on|off",
        default_value = "on",
        value_parser = ["on", "off"],
        overrides_with = "keep_failed"
    )]
    pub keep_failed: String,
    /// Which tar writes the `store` control and the stream of the tar-stream tools: `auto`
    /// (per class: the system tar, unless a name of the class is one it is known to reject or
    /// a dry run rejects it, then the runner's own PAX writer), `system` (always the system
    /// tar) or `lpk-bench` (always the runner's own writer). The row records which
    /// (measurement.tar.tool)
    #[arg(
        long,
        value_name = "auto|system|lpk-bench",
        default_value = "auto",
        value_parser = ["auto", "system", "lpk-bench"]
    )]
    pub tar: String,
    /// Compare two result directories of the same corpus and exit
    #[arg(long, num_args = 2, value_names = ["DIR_A", "DIR_B"])]
    pub compare: Vec<PathBuf>,
    /// Largest accepted time difference for --compare, in percent
    #[arg(long, value_name = "PERCENT", default_value_t = compare::DEFAULT_MAX_DIFF_PCT)]
    pub max_diff_pct: f64,
    /// Size-regression gate (E8-2): compare the median archive bytes of every row of a golden
    /// results directory (or of the directory a pointer file such as bench/ci-golden.toml names)
    /// with a new results directory; fails when an archive grew by more than --max-growth-pct or
    /// a row is missing on either side, and exits
    #[arg(long, num_args = 2, value_names = ["GOLDEN", "NEW_DIR"])]
    pub compare_sizes: Vec<PathBuf>,
    /// Largest accepted archive growth for --compare-sizes, in percent
    #[arg(long, value_name = "PERCENT", default_value_t = size::DEFAULT_MAX_GROWTH_PCT)]
    pub max_growth_pct: f64,
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
    // `all` is every tool but the foreign-extraction ones (E4-8): their information rows are
    // asked for by name, so a baseline run of `all` stays what it was.
    let named = |t: &&catalogue::Tool| t.foreign.is_none() || wanted.contains(&t.id);
    if wanted.is_empty() || wanted.iter().any(|w| w == "all") {
        return Ok(cat
            .tools
            .iter()
            .filter(named)
            .map(|t| t.id.clone())
            .collect());
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

/// Keep only the `wanted` settings of the tools in `ids` (every setting when `wanted` is empty);
/// an id that none of those tools has is an error.
pub fn filter_settings(
    discovered: &mut [discover::Discovered],
    ids: &[String],
    wanted: &[String],
) -> Result<()> {
    if wanted.is_empty() {
        return Ok(());
    }
    let mut found = std::collections::BTreeSet::new();
    for d in discovered.iter_mut().filter(|d| ids.contains(&d.tool.id)) {
        d.tool.settings.retain(|s| wanted.contains(&s.id));
        found.extend(d.tool.settings.iter().map(|s| s.id.clone()));
    }
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|w| !found.contains(*w))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            "--settings: no selected tool has setting(s) {}",
            missing.join(", ")
        );
    }
    Ok(())
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

fn validate_cmd(dir: &Path, corpus: Option<&Path>) -> Result<ExitCode> {
    let mut report = validate::validate_dir(dir)?;
    // E8-10: each row's class digest against the corpus manifest, when that corpus is present.
    let digests = validate::check_class_digests(dir, corpus)?;
    report.notes.extend(digests.notes);
    report.problems.extend(digests.problems);
    if digests.results > 0 {
        report.notes.push(format!(
            "{} row(s)' class digests match their corpus manifest",
            digests.results
        ));
    }
    for n in &report.notes {
        println!("note: {n}");
    }
    for p in &report.problems {
        eprintln!("error: {p}");
    }
    if report.problems.is_empty() {
        println!(
            "{}: {} result file(s) and {} probe file(s) validate (result files against \
             bench/results/schema.json)",
            dir.display(),
            report.results,
            report.probes
        );
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{} problem(s) in {}", report.problems.len(), dir.display());
        Ok(ExitCode::FAILURE)
    }
}

/// `lpk-bench tar ...`: the runner's own tar as a child process (hidden; [`harness_tar`]).
#[derive(Debug, Args)]
pub struct TarArgs {
    /// `-cf ARCHIVE -C DIR .`, `-cf ARCHIVE -T LIST` or `-xf ARCHIVE -C DIR`
    #[arg(allow_hyphen_values = true, num_args = 1.., trailing_var_arg = true)]
    pub args: Vec<String>,
}

pub fn tar(args: TarArgs) -> ExitCode {
    match harness_tar::command(&args.args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

pub fn run(args: RunArgs) -> ExitCode {
    if let Some(dir) = &args.validate {
        return match validate_cmd(dir, args.corpus.as_deref()) {
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
    } else if let [golden, new] = args.compare_sizes.as_slice() {
        size::compare_sizes_command(golden, new, args.max_growth_pct)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn discovered() -> Vec<discover::Discovered> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/tools.toml");
        let cat = Catalogue::load(&path).expect("catalogue");
        cat.tools
            .iter()
            .map(|t| discover::Discovered {
                tool: t.clone(),
                status: discover::Status::Skipped {
                    reason: "test".into(),
                },
                local_override: false,
            })
            .collect()
    }

    fn lpk_settings(d: &[discover::Discovered]) -> Vec<String> {
        d.iter()
            .find(|x| x.tool.id == "lpk")
            .expect("lpk")
            .tool
            .settings
            .iter()
            .map(|s| s.id.clone())
            .collect()
    }

    #[test]
    fn settings_filter_keeps_the_named_settings_of_the_selected_tools() {
        let ids = vec!["lpk".to_string()];
        let mut d = discovered();
        let wanted: Vec<String> = ["store", "fast", "balanced"].map(String::from).to_vec();
        filter_settings(&mut d, &ids, &wanted).expect("filter");
        // Catalogue order is kept.
        assert_eq!(lpk_settings(&d), ["fast", "balanced", "store"]);
        // Tools that are not selected are untouched.
        let zstd = d.iter().find(|x| x.tool.id == "zstd").expect("zstd");
        assert_eq!(zstd.tool.settings.len(), 3);
        // An id no selected tool has is an error.
        let mut d = discovered();
        assert!(filter_settings(&mut d, &ids, &["mx5".to_string()]).is_err());
        // No filter keeps every setting.
        let mut d = discovered();
        let before = lpk_settings(&d);
        filter_settings(&mut d, &ids, &[]).expect("none");
        assert_eq!(lpk_settings(&d), before);
    }
}
