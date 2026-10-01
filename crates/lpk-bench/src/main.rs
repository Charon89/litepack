//! lpk-bench — LitePack Phase 0 measurement tool.
//!
//! Subcommands (implemented by PLAN tasks P0-1 … P0-5):
//!   corpus build|scan   build the public corpus or scan a private folder (docs/CORPUS.md)
//!   run                 run baseline tools over the corpus with round-trip verification
//!   probe <name>        jpeg | deflate | dedup | text | weights | entropy-gate
//!   report              generate bench/reports/phase0-<date>.md from bench/results
//!
//! Every number LitePack ever quotes must come from this tool's committed JSON output.

fn main() {
    println!("lpk-bench 0.0.1 — see docs/PLAN.md task P0-1 to implement the CLI (corpus, run, probe, report)");
}
