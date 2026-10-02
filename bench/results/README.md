# Results

Committed JSON measurements land here (see docs/PLAN.md P0-3). Never edit by hand.

## Layout

```text
bench/results/
  schema.json                         JSON Schema for every file below
  <YYYY-MM-DD>-<host>[-<n>]/          one directory per run on one machine (n >= 2: further runs
                                      on the same UTC day); one corpus profile and manifest hash
    host.json                         machine, OS, CPU, RAM, lpk-bench version, git commit, rustc
    run.json                          written last: what the run covered, each combination's outcome
                                      (a directory without it is an aborted run and does not validate)
    tools.json                        every catalogue tool: found or skipped, reason, version used
    <tool>-<setting>-<class>.json     one file per tool x setting x corpus class
    probe-<name>.json                 component probes (PLAN P0-4)
```

The date is UTC and the host name is lower-cased and reduced to `[a-z0-9-]`. Each machine writes
only its own directory; nothing in these files may contain absolute paths, user names or
environment dumps.

## Rules

- Every number that appears in a report, in `docs/` or anywhere else must trace to one of these
  files. A result file records the tool version, the exact argument lists, the corpus profile and
  its manifest hash, the thread count, every repeat and the medians, and whether every extracted
  file verified.
- A combination that was skipped has a result file with the reason and no measurements.
- A combination that ran and failed (non-zero exit, timeout, killed descendants, missing or empty
  archive, or a missing, extra or different file after extraction) has a result file with a
  `failed` object (reason, step, repeat, `timed_out`, `descendants_killed`), no median, and the
  repeats completed before the failure. Every measure also records `timed_out` and
  `descendants_killed`; the validator rejects a median or `verified: true` next to a failed,
  timed-out or descendant-killing repeat.
- `measurement.env_stripped` lists the names of the environment variables removed from the tools'
  environment. Results from a private corpus carry `"private": true`.
- `run.json` also records `settle_ms_per_1000_files`, the pause after deleting extracted files, and records the antivirus products queried again after the last combination
  (`antivirus_end_source`, `antivirus_end`) and `antivirus_changed`, true when the names or decoded
  scanner states differ from the start-of-run values in `host.json`. It lists the classes, every tool x setting x class with its outcome, the thread count,
  the repeats requested, `--long-run-s` and the catalogue's BLAKE3; `--validate` checks each listed
  combination against its file. A combination whose first repeat reached `--long-run-s` is measured
  once and says so in `repeats_short`. `host.json` records the antivirus products Windows Security Center lists (name, raw `productState` as hex, decoded scanner state, and `antivirus_source`: `queried`, `query-failed` or `not-applicable`) and, independently, `defender_realtime` from the registry.
- Make a run with `lpk-bench run --tools all --profile small`; compare two runs of the same corpus
  with `lpk-bench run --compare <dirA> <dirB> [--max-diff-pct 3]`.
- Check a directory, or this whole folder, with
  `cargo run -p lpk-bench -- run --validate bench/results` (or one `<date>-<host>` directory).
