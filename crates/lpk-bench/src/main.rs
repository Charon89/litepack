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

use clap::{Parser, Subcommand};

mod corpus;
mod probe;
mod report;
mod run;

#[derive(Debug, Parser)]
#[command(name = "lpk-bench", version, about = "LitePack Phase 0 benchmark tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
    Report(report::ReportArgs),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Run(args) => run::run(args),
        Command::Probe(args) => probe::command(&args),
        Command::Corpus(args) => corpus::run(args),
        Command::Report(args) => report::command(&args),
    }
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
    fn documented_invocations_parse() {
        let documented: [&[&str]; 3] = [
            &["corpus", "build", "--profile", "small"],
            &["run", "--tools", "all"],
            &["report", "--results", "bench/results/d"],
        ];
        for args in documented {
            let argv = std::iter::once("lpk-bench").chain(args.iter().copied());
            assert!(Cli::try_parse_from(argv).is_ok(), "{args:?}");
        }
    }

    #[test]
    fn report_subcommand_parses_its_options() {
        let argv = [
            "lpk-bench",
            "report",
            "--results",
            "r",
            "--probes",
            "p1",
            "--probes",
            "p2",
            "--mixes",
            "m.toml",
            "--out",
            "o.md",
            "--allow-unclean",
        ];
        let cli = Cli::try_parse_from(argv).expect("parse");
        match cli.command {
            Command::Report(a) => assert_eq!(a.probes.len(), 2),
            other => panic!("not a report: {other:?}"),
        }
        assert!(Cli::try_parse_from(["lpk-bench", "report"]).is_err());
        assert!(Cli::try_parse_from(["lpk-bench", "run"]).is_ok());
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
            assert!(matches!(cli.command, Command::Probe(_)));
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
