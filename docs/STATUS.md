# Where the work stands

Hand-off between sessions and machines (D-13). Read this first; update it whenever work stops mid-task;
delete an entry when its branch is merged. `docs/PLAN.md` stays the source of truth for what is done.

Last updated: 2026-10-02, about 12:10 UTC.

## Summary
- **P0-1** done. **P0-2** done except the `full` profile pin (box 1 is `[~]`).
- **P0-3** (baseline runner): code complete, reviewed and approved, CI green. A first pair of acceptance runs passed the round-trip and schema clauses and agreed within 3% on every compress total, but not on four extract totals. The cause was a bias of the run loop on classes with thousands of files; it is fixed (decision D-21 on the branch) and the fix was checked. **Next step: a new pair of acceptance runs.**
- **P0-6** (licensing): boxes 2 and 3 written, reviewed and approved on their branch; box 1 waits for P0-4.
- **P0-4** (probes) and **P0-5** (report, verdict): not started; pre-flight notes are in `docs/notes/p0-4-preflight.md`.

## Branches in flight
| Branch | Pull request | State | What is left |
|---|---|---|---|
| `task/p0-3-runner` | #2 (draft) | Run loop, catalogue, result format, `--compare`; decisions D-19, D-20, D-21; four result directories (see below) | New acceptance pair, PLAN ticks, merge (see "P0-3") |
| `task/p0-2-full` | #3 (draft) | Resumable pinning and the `full` registry limits; review verdict APPROVE | Merge together with the completed pins |
| `task/p0-2-full-pins` | none (sits on top of #3) | Partial lock for `full`; its last commit message gives the counts | Finish the pin, prove it, merge (see "P0-2") |
| `task/p0-6-licensing` | #4 (draft) | `docs/LICENSING.md` rewritten; review verdict APPROVE; 21 factual claims checked against the web sources | Merge after P0-3 (it points to `docs/BASELINES.md`), tick boxes 2 and 3, add the decision named below |

## How to resume

### P0-3 — the new acceptance pair
The acceptance line reads: all installed tools complete with 100% round-trip verification; a second run differs by < 3% in time; results validate.
1. Make the machine idle (no builds, no agents, no corpus download). On `task/p0-3-runner` with a clean tree: `cargo build --release -p lpk-bench --locked`.
2. Run twice, one after the other: `target/release/lpk-bench run --tools all --profile small --repeats 3`. Each run takes several hours here (`zpaqfranz -m5` dominates; the settle pause adds some minutes).
3. `target/release/lpk-bench run --compare bench/results/<first> bench/results/<second>`. It checks, per tool and setting, the sums over all classes of the median compress and extract times against 3%, and prints notes when the antivirus state differed or a combination was repeated a different number of times.
4. Reviewer's condition for D-21: in both new runs the per-repeat extract times of `store` and zstd level 3 on the many-file classes (`small-files`, `game-assets`, `backup-versions`) must sit in one tight group at the value zstd level 19 shows. Summarise them from the result files with a few lines of script; do not paste result JSON into a session.
5. Outcome:
   - Comparison passes and the condition holds: commit both directories, tick the P0-3 boxes in `docs/PLAN.md` with the evidence, list the parked items below in the PLAN note, merge `main` into the branch, fast-forward `main`, push.
   - Two groups remain: apply the fallback named in D-21 (each repeat extracts into its own directory; delete them together at the end of the combination), review, rerun.
   - Only totals of a second or less still miss 3%: that is the owner's decision (see "Open points") — do not change the criterion silently.
- Result directories on the branch: `2026-10-02-megatron` and `-megatron-2` are the first pair, measured without the settle pause — evidence for D-21, not to be used for the report. `-megatron-3` (pause off) and `-megatron-4` (pause on) are the short check of the remedy on `small-files`.
- Parked, low severity: a failed end-of-run antivirus query is reported as "changed" instead of "unknown" (`run/exec.rs`, `run/host.rs`); the validator does not recompute `antivirus_changed` in `run.json`; `run.json` has no schema-version step for the settle field, so a hand-edited new file can pass as an old one (D-21).

### P0-2 — finish the `full` pin
On `task/p0-2-full-pins` (build `lpk-bench` in release mode from that branch):
1. `lpk-bench corpus build --profile full --update-lock` (add `--cache` and `--out` pointing at the main checkout's `bench/corpus/.cache` and `bench/corpus/full` when running from a worktree). It is resumable: pinned entries whose files are in the cache are reused. If an earlier run was killed, delete `bench/corpus.lock.run` first. Do not run it during a measured benchmark run.
2. Then a plain `lpk-bench corpus build --profile full` to prove the lock is complete.
3. Check the total against the range in `docs/CORPUS.md`; if it is over, lower the GovDocs `max_bytes` caps (extraction-time only). Confirm the `small` manifest hash recorded in `docs/PLAN.md` is unchanged.
4. Commit the lock, tick P0-2 box 1 with the evidence, rebase onto `main`, merge #3.

### P0-6 — finish
1. After P0-3 is on `main`: rebase `task/p0-6-licensing`, tick boxes 2 and 3 (evidence: the review verdict and the web check), and add a decision (the next free number after D-21): *closed-source GUI code reaches LGPL components only through the open engine as a separate process or DLL; LGPL C sources only through a separately published crate with its own named `deny.toml` exception*.
2. Box 1, when P0-4 has added its dependencies: add the named exception for `cabac` (`LGPL-3.0-or-later`), then reconcile `deny.toml` with the allow-list in PLAN's box 1 — `deny.toml` also allows `Apache-2.0 WITH LLVM-exception`, `MIT-0` (needed by a crate in the tree), `Unicode-DFS-2016` and `BSL-1.0`.
3. Open architecture point noted in the licensing table: the method document runs the pure-Rust ZIP/7z parsers in-process, PLAN E4 says foreign-format parsers run only in the sandboxed worker. E4 decides.

### P0-4 — start
After P0-3 is merged. Plan and verified facts: `docs/notes/p0-4-preflight.md`. Order: one framework sub-task (probe command, result envelope, validation, all dependencies, `probe weights`), then the other probes in parallel (at most four at a time), each reviewed. P0-5 note from the P0-3 review: rows that rest on a single measurement are marked in the report.

## Open points for the owner
- **3% clause.** Only if the new acceptance pair still misses 3% on totals of a second or less: choose a minimum-time floor, more repeats, or keep the clause as written.
- **Antivirus.** Windows Security Center on the measuring machine reports Defender off and a third-party product "snoozed". Each run records the state at start and end. Whether to run with the scanner fully on (with exclusions for `bench/tmp` and `bench/corpus`) or fully off is the owner's setting to change.
- **Optional probe baselines.** `bsc` and `hdiffz` (free, from their authors' GitHub releases) are not installed; the text and dedup probes skip them unless the owner wants them installed. Kanzi publishes no binaries.
- **WinZip and PowerArchiver.** Without their paid command-line tools the D-07 photo/document gate uses its fallback wording (best measured incumbent).
- **Method document.** It names "xEnc3" as a component; no public source or licence exists. `docs/LICENSING.md` marks it as not usable; the method document itself is unchanged (it needs the owner's say-so).
- **Licence option.** The corpus builder carries its own small JPEG encoder because the `jpeg-encoder` crate includes the IJG licence, which is not on the allow-list (D-18). Adding IJG to the list would allow the crate instead.
- **Corpus gaps carried forward** (see the P0-2 notes in PLAN): `office-versions` is not built; the `small` model-weights file is fp32; the private scan was never tried on a OneDrive folder.

## Working notes
- A measured benchmark run needs the machine to itself; plan other work around it.
- Subagent worktrees start from `main`: tell an implementer to fast-forward to the task branch first.
- Implementers and reviewers run out of turns on large tasks: ask for a commit after each step and a notes file, then resume them.
- A subagent that reports a refused command is not to be worked around; the refusal goes to the owner.
- Result directories are per machine (`bench/results/<date>-<host>`); never read their JSON into a session — use `run --validate`, `run --compare` and, later, the report command.
