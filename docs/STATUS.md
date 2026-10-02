# Where the work stands

Hand-off between sessions and machines (D-13). Read this first; update it whenever work stops mid-task;
delete an entry when its branch is merged. `docs/PLAN.md` stays the source of truth for what is done.

Last updated: 2026-10-02, about 11:15 UTC.

## Summary
- **P0-1** done. **P0-2** done except the `full` profile pin (box 1 is `[~]`).
- **P0-3** (baseline runner): code complete, reviewed and approved, CI green. The acceptance measurement is under way: run 1 is committed; run 2 and the comparison are outstanding.
- **P0-6** (licensing): boxes 2 and 3 written, reviewed and approved on their branch; box 1 waits for P0-4.
- **P0-4** (probes) and **P0-5** (report, verdict): not started; pre-flight notes are in `docs/notes/p0-4-preflight.md`.

## Branches in flight
| Branch | Pull request | State | What is left |
|---|---|---|---|
| `task/p0-3-runner` | #2 (draft) | Run loop, catalogue, result format, `--compare`; decisions D-19 and D-20; results of run 1 in `bench/results/2026-10-02-megatron` | Second run, comparison, PLAN ticks, merge (see "P0-3" below) |
| `task/p0-2-full` | #3 (draft) | Resumable pinning and the `full` registry limits; review verdict APPROVE | Merge together with the completed pins |
| `task/p0-2-full-pins` | none (sits on top of #3) | Partial lock for `full`: 2,672 entries pinned of 3,303 listed so far | Finish the pin, prove it, merge (see "P0-2" below) |
| `task/p0-6-licensing` | #4 (draft) | `docs/LICENSING.md` rewritten; review verdict APPROVE; 21 factual claims checked against the web sources | Merge after P0-3 (it points to `docs/BASELINES.md`), tick boxes 2 and 3, add the decision named below |

## How to resume

### P0-3 — finish the acceptance
The acceptance line reads: all installed tools complete with 100% round-trip verification; a second run differs by < 3% in time; results validate.
1. The second run writes `bench/results/2026-10-02-megatron-2`. It is complete only if it contains `run.json`.
   - Complete: `cargo run --release -p lpk-bench -- run --validate bench/results/2026-10-02-megatron-2`
   - Incomplete (the session ended first): delete that directory and `bench/tmp`, then run again on an otherwise idle machine:
     `cargo run --release -p lpk-bench -- run --tools all --profile small --repeats 3` (a few hours; `zpaqfranz -m5` dominates).
2. Compare: `cargo run --release -p lpk-bench -- run --compare bench/results/2026-10-02-megatron bench/results/<second run>`.
   It checks, per tool and setting, the sums over all classes of the median compress and extract times against the 3% clause, and prints notes when the antivirus state differed.
   If only small totals miss 3%, that is the owner's decision (see "Open points") — do not change the criterion silently.
3. Commit the second results directory, tick the P0-3 boxes in `docs/PLAN.md` with the evidence (both runs' closing lines, the `--compare` outcome, CI run), record the two parked items below, merge `main` into the branch, fast-forward `main`, push.
- The machine must be idle during a measured run: no builds, no agents, no corpus download. Pause other work first.
- Parked, low severity (from the last review): a failed end-of-run antivirus query is reported as "changed" instead of "unknown" (`run/exec.rs`, `run/host.rs`); the validator does not recompute `antivirus_changed` in `run.json`.

### P0-2 — finish the `full` pin
On `task/p0-2-full-pins` (build `lpk-bench` in release mode from that branch):
1. `lpk-bench corpus build --profile full --update-lock` (add `--cache` and `--out` pointing at the main checkout's `bench/corpus/.cache` and `bench/corpus/full` when running from a worktree). It is resumable: pinned entries whose files are in the cache are reused. If an earlier run was killed, delete `bench/corpus.lock.run` first.
2. Then a plain `lpk-bench corpus build --profile full` to prove the lock is complete.
3. Check the total against the range in `docs/CORPUS.md`; if it is over, lower the GovDocs `max_bytes` caps (extraction-time only). Confirm the `small` manifest hash recorded in `docs/PLAN.md` is unchanged.
4. Commit the lock, tick P0-2 box 1 with the evidence, rebase onto `main`, merge #3.

### P0-6 — finish
1. After P0-3 is on `main`: rebase `task/p0-6-licensing`, tick boxes 2 and 3 (evidence: the review verdict and the web check), and add a decision: *closed-source GUI code reaches LGPL components only through the open engine as a separate process or DLL; LGPL C sources only through a separately published crate with its own named `deny.toml` exception*.
2. Box 1, when P0-4 has added its dependencies: add the named exception for `cabac` (`LGPL-3.0-or-later`), then reconcile `deny.toml` with the allow-list in PLAN's box 1 — `deny.toml` also allows `Apache-2.0 WITH LLVM-exception`, `MIT-0` (needed by a crate in the tree), `Unicode-DFS-2016` and `BSL-1.0`.
3. Open architecture point noted in the licensing table: the method document runs the pure-Rust ZIP/7z parsers in-process, PLAN E4 says foreign-format parsers run only in the sandboxed worker. E4 decides.

### P0-4 — start
After P0-3 is merged. Plan and verified facts: `docs/notes/p0-4-preflight.md`. Order: one framework sub-task (probe command, result envelope, validation, all dependencies, `probe weights`), then the other probes in parallel (at most four at a time), each reviewed.

## Open points for the owner
- **3% clause.** If the comparison of the two acceptance runs misses 3% only on totals of a second or less, choose: a minimum-time floor, more repeats, or keep the clause as written.
- **Antivirus.** Windows Security Center on the measuring machine reports Defender off and a third-party product "snoozed". Each run records the state at start and end. Whether to run with the scanner fully on (with exclusions for `bench/tmp` and `bench/corpus`) or fully off is the owner's setting to change.
- **Optional probe baselines.** `bsc` and `hdiffz` (free, from their authors' GitHub releases) are not installed; the text and dedup probes skip them unless the owner wants them installed. Kanzi publishes no binaries.
- **WinZip and PowerArchiver.** Without their paid command-line tools the D-07 photo/document gate uses its fallback wording (best measured incumbent).
- **Method document.** It names "xEnc3" as a component; no public source or licence exists. `docs/LICENSING.md` marks it as not usable; the method document itself is unchanged (it needs the owner's say-so).
- **Licence option.** The corpus builder carries its own small JPEG encoder because the `jpeg-encoder` crate includes the IJG licence, which is not on the allow-list (D-18). Adding IJG to the list would allow the crate instead.
- **Corpus gaps carried forward** (see the P0-2 notes in PLAN): `office-versions` is not built; the `small` model-weights file is fp32; the private scan was never tried on a OneDrive folder.

## Working notes
- Subagent worktrees start from `main`: tell an implementer to fast-forward to the task branch first.
- Implementers and reviewers run out of turns on large tasks: ask for a commit after each step and a notes file, then resume them.
- A subagent that reports a refused command is not to be worked around; the refusal goes to the owner.
- Result directories are per machine (`bench/results/<date>-<host>`); never read their JSON into a session — use `run --validate`, `run --compare` and, later, the report command.
