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

#[derive(Debug, Parser)]
#[command(name = "lpk-bench", version, about = "LitePack Phase 0 benchmark tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build the public corpus or scan a private folder (PLAN P0-2)
    Corpus,
    /// Run baseline tools over the corpus with round-trip verification (PLAN P0-3)
    Run,
    /// Run a component probe: jpeg, deflate, dedup, text, weights, entropy-gate (PLAN P0-4)
    Probe,
    /// Generate the Phase 0 report from bench/results (PLAN P0-5)
    Report,
}

impl Command {
    /// Subcommand name and the PLAN task that will implement it.
    fn task(&self) -> (&'static str, &'static str) {
        match self {
            Command::Corpus => ("corpus", "P0-2"),
            Command::Run => ("run", "P0-3"),
            Command::Probe => ("probe", "P0-4"),
            Command::Report => ("report", "P0-5"),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
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
    fn every_subcommand_parses_and_names_a_task() {
        for (arg, task) in [
            ("corpus", "P0-2"),
            ("run", "P0-3"),
            ("probe", "P0-4"),
            ("report", "P0-5"),
        ] {
            let cli = Cli::try_parse_from(["lpk-bench", arg]).expect("parse");
            assert_eq!(cli.command.task(), (arg, task));
        }
    }
}
