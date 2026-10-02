# Results

Committed JSON measurements land here (see docs/PLAN.md P0-3). Never edit by hand.

## Layout

```text
bench/results/
  schema.json                         JSON Schema for every file below
  <YYYY-MM-DD>-<host>/                one directory per run on one machine
    host.json                         machine, OS, CPU, RAM, lpk-bench version, git commit, rustc
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
- Check a directory, or this whole folder, with
  `cargo run -p lpk-bench -- run --validate bench/results` (or one `<date>-<host>` directory).
