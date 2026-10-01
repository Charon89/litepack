---
name: bench
description: Run a benchmark or probe through the bench-runner subagent and return only the summary tables. Usage: /bench <corpus build|run|probe <name>|report> [extra flags]
disable-model-invocation: true
---
Delegate to the **bench-runner** subagent: "Run `cargo run --release -p lpk-bench -- $ARGUMENTS` following your rules, then run the report command and return the summary tables and anomalies."

When it returns, print the tables verbatim and nothing else. Do not read `bench/results/*.json` into this conversation. If the run produced a gate evaluation, remind the user that the verdict must be recorded in `docs/DECISIONS.md` as D-08 (ask before writing it).
