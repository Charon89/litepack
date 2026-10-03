# Where the work stands

Hand-off between sessions and machines (D-13). Read this first; update it whenever work stops mid-task;
delete an entry when its branch is merged. `docs/PLAN.md` stays the source of truth for what is done.

Last updated: 2026-10-03, about 11:00 UTC (07:00 local on the measuring machine). No measurement is running.

**Waiting on the owner:** the P0-3 acceptance clause "a second run differs by < 3% in time" (see the first
open point below). Everything else in P0-3 is done and evidenced; the merge order below starts when that
is settled.

## Summary
- **P0-1** and **P0-2** done. The `full` corpus profile is pinned and built (PR #3 merged).
- **P0-3** (baseline runner): code complete, reviewed and approved, CI green. The second acceptance pair (with the settle pause of D-21) ran on the night of 2026-10-02/03 and is committed (`bench/results/2026-10-03-megatron` and `-megatron-2`): both runs complete with 100% round-trip verification and 0 validation problems; 23 of the 28 compare totals agree within 3%, five do not, for two causes that are not the run loop — a processor that ran slower for part of a run, and single slow repeats of sub-second extractions on two many-file classes (details in the PLAN note and the open point). **The boxes stay `[~]` until the owner rules on the 3% clause.**
- **P0-4** (probes): **code complete** on `task/p0-4-probes` — the framework and all six probes, each reviewed to approval (D-22). What remains is the official run of every probe on `small`, on an idle machine, committed under `bench/results/`, and the PLAN ticks.
- **P0-6** (licensing): boxes 2 and 3 written, reviewed and approved on their branch; box 1 waits for P0-4 to merge.
- **P0-5** (report, verdict): the report generator `lpk-bench report` is implemented, reviewed and approved on `task/p0-5-report` (PR #6, stacked on #5; rules in D-23). The official report comes from the official `full` runs; the verdict (D-08) is the owner's after that. Two things the trial report on `small` already showed need the owner's reading before D-08: gate G2 (versioned backup, 2× smaller) fails because the incumbents already remove the cross-version redundancy inside solid archives, and gate G3 (video store speed) as proxied compares a cache-warm store pass with an uncached raw read.

## Branches in flight
| Branch | Pull request | State | What is left |
|---|---|---|---|
| `task/p0-3-runner` | #2 (draft) | Run loop, catalogue, result format, `--compare`; D-19, D-20, D-21; four result directories; `main` merged in | New acceptance pair, PLAN ticks, fast-forward `main` (see "P0-3") |
| `task/p0-4-probes` | #5 (draft, stacked on #2) | Probe framework and six probes, all reviewed; D-22; PLAN status note | Official probe runs on `small`, PLAN ticks, then retarget #5 to `main` and merge after #2 (see "P0-4") |
| `task/p0-5-report` | #6 (draft, stacked on #5) | Report generator, reviewed; D-23; CLAUDE.md command lines | Official report from the official `full` runs; retarget #6 to `main` after #5 |
| `task/p0-6-licensing` | #4 (draft) | `docs/LICENSING.md` rewritten; review verdict APPROVE; 21 factual claims checked against the web sources | Merge after P0-3 (it points to `docs/BASELINES.md`), tick boxes 2 and 3, add the decision named below |

## How to resume

### P0-3 — after the second acceptance pair
The acceptance line reads: all installed tools complete with 100% round-trip verification; a second run differs by < 3% in time; results validate. The second pair (`2026-10-03-megatron`, `-megatron-2`; build `feddbfb`; settle pause on; each run 3 h 8 min) meets the first and third clauses twice over; the 3% clause holds for 23 of 28 totals. The PLAN note under P0-3 has the numbers and the two causes.
1. The owner rules on the 3% clause (open point below). Then:
   - *Clause kept as written and a steady processor clock arranged (a machine setting: turbo boost off or a fixed frequency):* repeat the pair exactly as before — `task/p0-3-runner`, clean tree, `cargo build --release -p lpk-bench --locked`, then on the measuring machine the git-ignored `target/acceptance/pair2.sh` detached (it runs both runs and `run --compare` and writes `target/acceptance/pair2-status.log`; see "Working notes" for why detached). Check the result with `run --compare` and the git-ignored `target/acceptance/per-repeat.py <dirA> <dirB>` (per-repeat extract times of `store`, zstd/3, zstd/19 on the many-file classes) and `class-diff.py <dirA> <dirB> <tool/setting> <compress|extract>` (which class a miss comes from).
   - *Clause amended (a time floor below which totals are not compared, or more repeats):* that is a change to a decided acceptance criterion — record it in `docs/DECISIONS.md` (next free number) with the owner's wording, implement it in `run --compare` (a floor) or rerun with `--repeats 5` (more repeats), then tick.
   - *Clause accepted as met in substance with the two causes recorded:* tick the four boxes with the evidence already in the PLAN note, list the parked items below there, fast-forward `main`, push.
2. Either way the per-class check for D-21 is settled: on `small-files` every repeat of `store`, zstd/3 and zstd/19 sits in one group in both runs and the runs agree within 2.5%; no two-group pattern remains, so the D-21 fallback (per-repeat extraction directories) is not needed.
- Result directories on the branch: `2026-10-02-megatron` and `-megatron-2` are the first pair, measured without the settle pause — evidence for D-21, not to be used for the report. `-megatron-3` (pause off) and `-megatron-4` (pause on) are the short check of the remedy on `small-files`. `2026-10-03-megatron` and `-megatron-2` are the second pair, the candidate baseline for the `small` report.
- Parked, low severity: a failed end-of-run antivirus query is reported as "changed" instead of "unknown" (`run/exec.rs`, `run/host.rs`); the validator does not recompute `antivirus_changed` in `run.json`; `run.json` has no schema-version step for the settle field, so a hand-edited new file can pass as an old one (D-21).

### P0-4 — official probe runs and ticks
1. On an idle machine, from a clean release build of `task/p0-4-probes`: `cargo run --release -p lpk-bench -- probe all --profile small` (results go to a fresh `bench/results/<date>-<host>[-n]`; probes refuse debug builds). Expect about twenty minutes on the measuring machine; `probe text` is the slowest (about ten minutes). `probe weights` produces no dtype rows on `small` (no safetensors file there) — that is expected and recorded.
2. `cargo run --release -p lpk-bench -- run --validate bench/results/<that directory>`; commit the directory.
3. Tick the P0-4 boxes in `docs/PLAN.md` with the evidence (elapsed time per probe from the files, validation line, CI run), note that `bsc`, `kanzi` and `hdiffz` were not installed (skipped rows) and that `weights` needs `full`.
4. Retarget PR #5 to `main` once PR #2 is merged, fast-forward `main`, push. Then P0-6 box 1 (see below).
- What P0-5 must know when reading probe files: in-process speed rows measure the libraries this crate links (the bundled xz is a generic C build without SIMD paths) — use the process-wall rows (`xz-cli`, `zstd-cli`, `bsc`, `kanzi`) for speed comparisons across tools; `probe deflate` groups streams by the library's encoder estimate and by correction-overhead bucket (the library reports no "recognised" flag); the D-07 video gate uses the video class's own raw-read rate from the `full` profile (median of three passes), and the store-throughput numerator must include file opens over the same files; rows that rest on a single measurement are marked.
- Optional decision for later (affects the product too): build the bundled xz with its fast paths enabled (`CFLAGS` through `.cargo/config.toml`), so in-process xz matches upstream.

### P0-6 — finish
1. After P0-3 is on `main`: rebase `task/p0-6-licensing`, tick boxes 2 and 3 (evidence: the review verdict and the web check), and add a decision (next free number after D-22): *closed-source GUI code reaches LGPL components only through the open engine as a separate process or DLL; LGPL C sources only through a separately published crate with its own named `deny.toml` exception*.
2. Box 1, when P0-4 is merged: reconcile `deny.toml` with the allow-list in PLAN's box 1 — `deny.toml` also allows `Apache-2.0 WITH LLVM-exception`, `MIT-0` (needed by a crate in the tree), `Unicode-DFS-2016` and `BSL-1.0`; the `cabac` exception is already in place.
3. Open architecture point noted in the licensing table: the method document runs the pure-Rust ZIP/7z parsers in-process, PLAN E4 says foreign-format parsers run only in the sandboxed worker. E4 decides.

## Open points for the owner
- **3% clause (decision needed now).** The second pair misses it on 5 of 28 totals: 7z/mx5 compress 3.34%, 7z/ultra compress 4.47%, store compress 3.06%, zpaqfranz/m1 extract 3.47%, zstd/ultra22-long27 extract 8.39%. Two causes, both outside the run loop: (1) the processor ran slower for part of run 1 (02:00–02:50 local): on `text-prose` the CPU seconds of 7z, zstd/19 and xz/9 grew in step with the wall time from one repeat to the next (7z/mx5: 61.9, 67.3, 73.9 s; run 2: 62–63 s), which is a lower clock, not another process taking cores; run 2 had the same once (xz/9 on `text-prose`, 10% slower than run 1). The automatic-maintenance tasks did not run in the window; another Claude session was running on the machine all night. Minute-long CPU-bound totals cannot agree within 3% without a steady clock — that is a BIOS or power-plan setting (turbo boost off, or a fixed frequency) and the owner's to change. (2) Single slow repeats of sub-second extractions on `game-assets` (4,660 files) and `backup-versions` (1,974 files), in both runs, at random positions (store on `game-assets`, run 1: 0.565, 1.198, 0.564 s); three repeats absorb one such repeat, not two (zstd/ultra22-long27 on `backup-versions`, run 2: 0.254, 0.660, 0.704 s). Options: keep the clause and rerun with the clock fixed (another ~7 h); amend it (a floor of a few seconds below which totals are not compared, and/or `--repeats 5`); or accept this pair as meeting the clause in substance with the causes recorded. Whichever it is goes into `docs/DECISIONS.md`.
- **Antivirus.** Windows Security Center on the measuring machine reports Defender off and a third-party product "snoozed". Each run records the state at start and end. Whether to run with the scanner fully on (with exclusions for `bench/tmp` and `bench/corpus`) or fully off is the owner's setting to change.
- **Optional probe baselines.** `bsc` and `hdiffz` (free, from their authors' GitHub releases) are not installed; the text and dedup probes skip them unless the owner wants them installed. Without `bsc`, D-05's claim about BWT on text cannot be checked. Kanzi publishes no binaries. When `bsc` is first installed, its strongest flags must be checked against its usage text and recorded.
- **WinZip and PowerArchiver.** Without their paid command-line tools the D-07 photo/document gate uses its fallback wording (best measured incumbent).
- **Method document.** It names "xEnc3" as a component; no public source or licence exists. `docs/LICENSING.md` marks it as not usable; the method document itself is unchanged (it needs the owner's say-so).
- **Licence option.** The corpus builder carries its own small JPEG encoder because the `jpeg-encoder` crate includes the IJG licence, which is not on the allow-list (D-18). Adding IJG to the list would allow the crate instead.
- **Corpus gaps carried forward** (see the P0-2 notes in PLAN): `office-versions` is not built; the `small` model-weights file is fp32 and not safetensors; the private scan was never tried on a OneDrive folder; in `full`, 43 of the 50 photo conversions fit their budget and the Ubuntu image is just under the size range the spec names.

## Merge order
`main` ← `task/p0-3-runner` (#2) ← `task/p0-4-probes` (#5) ← `task/p0-5-report` (#6). After the owner rules on the 3% clause: fast-forward `main` to #2; retarget #5 to `main` and fast-forward; retarget #6 and fast-forward; then P0-6 (#4): rebase, ticks, the LGPL-boundary decision, and box 1 (reconcile `deny.toml`). Then the official probe run on `small` (P0-4 ticks), the `full` baseline run and `probe all --profile full` (long, idle machine), the official report, and D-08.

## Working notes
- A measured benchmark run needs the machine to itself; plan other work around it.
- A command run in the background from a Claude session is killed after two hours whatever timeout it was
  given, and its child processes die with it (that ended a first attempt at the acceptance pair after 155 of
  238 combinations). Launch anything longer as a detached process (on Windows: PowerShell `Start-Process`
  on a script with a hidden window and redirected output; the git-ignored `target/acceptance/pair2.sh` is
  the one in use) and follow it through a status file.
- Subagent worktrees start from `main`: before dispatching, make sure `main` is an ancestor of the task branch (merge `main` in), and give the implementer the expected head hash; their sandbox refuses `git reset --hard`.
- Implementers and reviewers run out of turns on large tasks: ask for a commit after each step, then resume them. Agents from an earlier session cannot be resumed; start a fresh one with the context it needs.
- A subagent that reports a refused command is not to be worked around; the refusal goes to the owner.
- Result directories are per machine (`bench/results/<date>-<host>`); never read their JSON into a session — use `run --validate`, `run --compare` and, later, the report command.
