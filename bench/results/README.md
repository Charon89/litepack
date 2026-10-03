# Results

Committed JSON measurements land here (see docs/PLAN.md P0-3). Never edit by hand.

## Layout

```text
bench/results/
  schema.json                         JSON Schema for host.json, tools.json, run.json and the result files (not for probe files)
  <YYYY-MM-DD>-<host>[-<n>]/          one directory per run on one machine (n >= 2: further runs
                                      on the same UTC day); one corpus profile and manifest hash
    host.json                         machine, OS, CPU, RAM, lpk-bench version, git commit, rustc
    run.json                          written last: what the run covered, each combination's outcome
                                      (a directory without it is an aborted run and does not validate)
    tools.json                        every catalogue tool: found or skipped, reason, version used
    <tool>-<setting>-<class>.json     one file per tool x setting x corpus class
    probe-<name>.json                 component probes (PLAN P0-4), see "Probe files" below
    probe-<name>.md                   the probe's table, rendered from the JSON
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
## Probe files

Run probes from a release build, because they time code of this crate:

```text
cargo run --release -p lpk-bench -- probe <jpeg|deflate|dedup|text|weights|entropy-gate|all>
    [--profile small|full] [--corpus DIR] [--results ROOT | --into DIR] [--tmp DIR] [--threads N]
    [--allow-dirty-build] [--allow-debug-build]
```

A debug build is refused unless `--allow-debug-build` is given; every probe file records the cargo
profile, the opt-level, debug assertions and that flag (`build_profile`), and `run --validate`
reports a file from an unoptimised build whose flag is not recorded.

The probe writes `probe-<name>.json` and `probe-<name>.md`. `--results ROOT` (default
`bench/results`, as for `run`) is the root under which a new `<date>-<host>[-<n>]` directory is
created, with a `host.json` written by the same code as the baseline runner. `--into DIR` adds to
an existing results directory instead: it must contain `host.json` and match this host, this build
(one build per directory) and this corpus; its `host.json` is left alone. An earlier result of the
same probe in that directory is removed before the probe runs, so a failing re-run leaves no stale
file. Scratch files go to a per-run directory under `--tmp` (default `bench/tmp`), removed at the
end. The clean-build rule is the same as for `run`. `probe all` runs every probe, goes on after a
failure and exits non-zero if any failed.

Every `probe-<name>.json` has the same envelope and a probe-specific `data` object:

| field | meaning |
|---|---|
| `probe`, `format_version` | probe name (as in the file name); version of this format |
| `corpus` | `profile`, `manifest_blake3`, `private` (true for a private corpus: per-file records carry an index and no name or path) |
| `build`, `build_profile` | lpk-bench build stamp; `profile`, `opt_level`, `debug_assertions`, `allow_debug_build` |
| `host`, `date` | host name; UTC time of the probe's start |
| `threads`, `library_threads` | threads used for size-only work; threads the compression libraries used inside timed sections |
| `libraries` | name to version of every compression library the probe used, linked C libraries included |
| `elapsed_seconds`, `notes` | wall time of the probe; parts that were skipped, and why |
| `data` | the probe's own typed structure |

Rules: quantities are raw (bytes, seconds, counts); percentages and MB/s (10^6 bytes per second)
are computed only when rendering the table, which is a pure function of the parsed JSON. A timed
section runs alone, on data already in memory, and the JSON records how many threads the library
used. Output is deterministic apart from timings, date and host.

Private corpora: inside `data` the keys `path`, `name` and `folder` are forbidden when
`corpus.private` is true, and the validator rejects them anywhere in `data`. A probe must therefore
use exactly those names for anything that could carry a file or folder name, so the check can see
it. Per-file records carry an index instead, and folder groups are labelled `group-<n>`, numbered
in manifest order; these labels are not stable across scans of a changing folder tree. Reasons and
other text in `data` are fixed categories and never quote file content.

`run --validate` checks each probe file: typed parse with unknown fields rejected, the envelope
rules, the consistency rules each probe declares (for example, counts add up), host, build and
corpus against `host.json` and the directory's other files, and `probe-<name>.md` equal to the
table rendered from the JSON. A directory with only probe files and `host.json` needs neither
`run.json` nor `tools.json`; a directory with any baseline file must satisfy the baseline rules too.

- Check a directory, or this whole folder, with
  `cargo run -p lpk-bench -- run --validate bench/results` (or one `<date>-<host>` directory).
