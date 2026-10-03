//! lpk-bench — LitePack Phase 0 measurement tool.
//!
//! Subcommands (implemented by PLAN tasks P0-2 … P0-5):
//!   corpus   build the public corpus or scan a private folder (docs/CORPUS.md)
//!   run      run baseline tools over the corpus with round-trip verification
//!   probe    component probes: jpeg | deflate | dedup | text | weights | entropy-gate
//!   report   generate bench/reports/phase0-<date>.md from bench/results
//!
//! Every number LitePack ever quotes must come from this tool's committed JSON output.

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

mod corpus;
mod probe;
mod run;

#[derive(Debug, Parser)]
#[command(name = "lpk-bench", version, about = "LitePack Phase 0 benchmark tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Stub arguments: accepted and ignored until the implementing task defines the real ones.
#[derive(Debug, Args)]
struct StubArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    _ignored: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build the public corpus or scan a private folder (PLAN P0-2)
    Corpus(corpus::CorpusArgs),
    /// Run baseline tools over the corpus with round-trip verification (PLAN P0-3)
    Run(run::RunArgs),
    /// Run a component probe: jpeg, deflate, dedup, text, weights, entropy-gate (PLAN P0-4)
    Probe(probe::ProbeArgs),
    /// Generate the Phase 0 report from bench/results (PLAN P0-5)
    Report(StubArgs),
}

impl Command {
    /// Subcommand name and the PLAN task that will implement it.
    fn task(&self) -> (&'static str, &'static str) {
        match self {
            Command::Corpus(_) => ("corpus", "P0-2"),
            Command::Run(_) => ("run", "P0-3"),
            Command::Probe(_) => ("probe", "P0-4"),
            Command::Report(_) => ("report", "P0-5"),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Run(args) = cli.command {
        return run::run(args);
    }
    if let Command::Probe(args) = &cli.command {
        return probe::command(args);
    }
    if let Command::Corpus(args) = cli.command {
        return corpus::run(args);
    }
    let (name, task) = cli.command.task();
    eprintln!(
        "error: `lpk-bench {name}` is not implemented yet (PLAN task {task}); no measurement was made"
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_lists_all_subcommands() {
        let help = Cli::command().render_help().to_string();
        for name in ["corpus", "run", "probe", "report"] {
            assert!(help.contains(name), "help is missing `{name}`:\n{help}");
        }
    }

    #[test]
    fn documented_invocations_reach_the_stub() {
        let documented: [&[&str]; 3] = [
            &["corpus", "build", "--profile", "small"],
            &["run", "--tools", "all"],
            &["report"],
        ];
        for args in documented {
            let argv = std::iter::once("lpk-bench").chain(args.iter().copied());
            assert!(Cli::try_parse_from(argv).is_ok(), "{args:?}");
        }
    }

    #[test]
    fn every_stub_subcommand_parses_and_names_a_task() {
        for (arg, task) in [("run", "P0-3"), ("report", "P0-5")] {
            let cli = Cli::try_parse_from(["lpk-bench", arg]).expect("parse");
            assert_eq!(cli.command.task(), (arg, task));
        }
    }

    #[test]
    fn probe_subcommand_parses_names_and_options() {
        for name in [
            "jpeg",
            "deflate",
            "dedup",
            "text",
            "weights",
            "entropy-gate",
            "all",
        ] {
            let cli = Cli::try_parse_from(["lpk-bench", "probe", name]).expect("parse");
            assert_eq!(cli.command.task().0, "probe");
        }
        let argv = [
            "lpk-bench",
            "probe",
            "weights",
            "--profile",
            "small",
            "--corpus",
            "c",
            "--results",
            "r",
            "--threads",
            "3",
            "--allow-dirty-build",
        ];
        assert!(Cli::try_parse_from(argv).is_ok());
        assert!(Cli::try_parse_from(["lpk-bench", "probe"]).is_err());
        assert!(Cli::try_parse_from(["lpk-bench", "probe", "nope"]).is_err());
    }

    #[test]
    fn corpus_subcommands_parse() {
        for args in [
            &["corpus", "build", "--profile", "small"][..],
            &[
                "corpus",
                "build",
                "--profile",
                "full",
                "--out",
                "o",
                "--cache",
                "c",
                "--only",
                "a,b",
                "--update-lock",
            ],
            &["corpus", "scan", "--private", "p", "--out", "o"],
        ] {
            let argv = std::iter::once("lpk-bench").chain(args.iter().copied());
            assert!(Cli::try_parse_from(argv).is_ok(), "{args:?}");
        }
        assert!(Cli::try_parse_from(["lpk-bench", "corpus", "build"]).is_err());
        assert!(Cli::try_parse_from(["lpk-bench", "corpus", "scan", "--out", "o"]).is_err());
    }
}
