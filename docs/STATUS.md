# Where the work stands

Hand-off between sessions and machines (D-13). Read this first; update it whenever work stops mid-task;
delete an entry when its branch is merged. `docs/PLAN.md` stays the source of truth for what is done.

Last updated: 2026-10-04, about 08:15 UTC (04:15 local on the measuring machine). No measurement is running.

**Phase 1 is under way.** Phase 0 closed with D-08 (GO) and D-27; the Phase 1 task list is in PLAN (37 tasks).
E1-1 (frame grammar, D-28) is on `main`; E1-2 (entry table) is being implemented on `task/e1-2-entry-table`.
Each task runs as in Phase 0: implementer in a worktree from a brief with exact values → read-only review →
fix round → the format values recorded as a decision → PLAN tick with evidence → PR → `main` fast-forwarded.

## Summary
- **P0-1, P0-2, P0-3, P0-4, P0-6 done** and on `main` (PRs #2, #3, #4, #5 merged; every box ticked with evidence in PLAN; decisions D-19 to D-26).
  - P0-3's second acceptance pair (`bench/results/2026-10-03-megatron`, `-megatron-2`) met the round-trip and schema clauses twice over; the owner took the 3% clause as met in substance (D-24: 23 of 28 totals within 3%, the five others caused by the machine's clock and by single stalled sub-second repeats, not by the loop).
  - P0-4's official `small` probe run is `bench/results/2026-10-03-megatron-3`.
  - P0-6 cut `deny.toml` to what the tree needs (D-25) and fixed the LGPL boundary (D-26).
- **P0-5** (report, verdict): done. Official report `bench/reports/phase0-2026-10-03.md` (full profile; baseline `bench/results/2026-10-03-megatron-4`, probes `-megatron-5`, build `78bc960`): G1 PASS (83.6% of the best incumbent, 81.0% of 7-Zip Ultra on the photo/document classes combined), G4 PASS (zstd/3 extraction above 7-Zip `-mx5` on all three mixes), G2 and G3 FAIL as written — on their definitions, not the method (the versioned class fits inside one dictionary window; a read-plus-write proxy against a read-only rate). The owner recorded **D-08 GO** and **D-27** (G2 on a versioned set larger than the incumbents' windows, to be added to the corpus; G3 as the store path at ≥ 80% of the `store` control). The arithmetic of the report was recomputed in full by a read-only review (`docs/notes/phase0-verdict-review.md`, no mismatch).
- Two things the trial reports on `small` (both pairs) showed need the owner's reading before D-08: gate G2 (versioned backup, 2× smaller) fails because the incumbents already remove the cross-version redundancy inside solid archives (the estimate sits around 110% of zpaqfranz m5 on `small`), and gate G3 (video store speed) as proxied compares a cache-warm store pass with an uncached raw read (23% on `small`'s 66 MB video class). Neither is a measurement error; both are questions of what the gate should mean, to be answered in D-08 or by amending D-07 (a gate change is the owner's).

## Branches in flight
| Branch | State | What is left |
|---|---|---|
| `task/e1-2-entry-table` | implementer running (brief in the orchestrator's scratchpad: sorted unique paths, kinds file/directory/symlink, flags, `mtime_ns`, chunk indices, streaming reader) | review, fix round, D-29, tick, PR, fast-forward `main` |

## How to resume

### Phase 1 — how it starts
1. The owner reads the draft breakdown (PR #8, `docs/notes/phase1-e1-e2-draft.md`): section 1 is what the `full` numbers say about priorities; section 2 lists the twelve format details the method document leaves open, each with a proposal; sections 4–5 are the 14 E1 and 22 E2 tasks (plus E0-1, the versioned-set corpus addition D-27 needs); the last paragraph lists what the owner must settle before adoption (the spec of the versioned set; MP3 and the structured engine staying in Phase 2; one pipeline crate or several; Argon2id and recovery defaults; the pure-Rust LZMA decoder's licence).
2. On adoption: the tasks move into `docs/PLAN.md` under Phase 1 with checkboxes (one commit), PR #8 merges, and work proceeds one task at a time as in Phase 0 (implementer in a worktree → read-only review → ticks with evidence → `main` fast-forwarded). E1-1 (frame grammar and header) is first; E2-1/E2-2 (ingest, classifier) can run alongside once E1-1's types exist.
3. Format details become decisions as their tasks land (`D-28` onwards); the format freezes at E1-14 (the independent-decoder gate), not before.
- Parked from Phase 0, to pick up in the tasks named: the G3 "gate cost … share of the raw read rate" lines of the report use the probe's whole-corpus gate rates rather than the video class's own (information lines only; a report fix round); two `preflate-rs` 0.7.6 panics (`tree_predictor.rs:169`, index out of bounds) on PDF streams, caught and counted by the probe — containment and fuzzing item for E2-10 and an upstream report; WinZip and PowerArchiver still unmeasured (the D-07 fallback wording stands until the owner installs them); `bsc`/`hdiffz` not installed (D-05's BWT claim is measured in E2-15 with libsais instead).

### P0-5 — Linux CI smoke test (done)
The manual workflow `corpus-smoke.yml` has an input `run_smoke` (default on): after the corpus build it installs `zstd`, `xz-utils` and `7zip`, runs the baseline runner on four classes with one repeat, validates and uploads the results as an artifact (CI results are never committed). The green run and its summary lines are cited under the P0-5 box in PLAN. Nothing else is needed from CI for Phase 0.

## Open points for the owner
- **G2 and G3 before D-08** (see the summary): decide what "2× smaller on versioned backups" means against incumbents that already deduplicate inside solid archives, and what "store at ≥ 80% of raw read" should be measured against (the current proxy is a real program reading and writing the files against an uncached raw read).
- **Antivirus.** Windows Security Center on the measuring machine reports Defender off and a third-party product "snoozed". Each run records the state at start and end. Whether to run with the scanner fully on (with exclusions for `bench/tmp` and `bench/corpus`) or fully off is the owner's setting to change.
- **Optional probe baselines.** `bsc` and `hdiffz` (free, from their authors' GitHub releases) are not installed; the text and dedup probes skip them unless the owner wants them installed. Without `bsc`, D-05's claim about BWT on text cannot be checked. Kanzi publishes no binaries. When `bsc` is first installed, its strongest flags must be checked against its usage text and recorded.
- **WinZip and PowerArchiver.** Without their paid command-line tools the D-07 photo/document gate uses its fallback wording (best measured incumbent).
- **Method document.** It names "xEnc3" as a component; no public source or licence exists. `docs/LICENSING.md` marks it as not usable; the method document itself is unchanged (it needs the owner's say-so).
- **Licence option.** The corpus builder carries its own small JPEG encoder because the `jpeg-encoder` crate includes the IJG licence, which is not on the allow-list (D-18). Adding IJG to the list would allow the crate instead.
- **Optional build decision** (affects the product too): build the bundled xz with its fast paths enabled (`CFLAGS` through `.cargo/config.toml`), so in-process xz matches upstream; until then the report's cross-tool speed comparisons use the process-wall rows.
- **Corpus gaps carried forward** (see the P0-2 notes in PLAN): `office-versions` is not built; the `small` model-weights file is fp32 and not safetensors; the private scan was never tried on a OneDrive folder; in `full`, 43 of the 50 photo conversions fit their budget and the Ubuntu image is just under the size range the spec names.

## Working notes
- A measured benchmark run needs the machine to itself; plan other work around it.
- A command run in the background from a Claude session is killed after two hours whatever timeout it was
  given, and its child processes die with it (that ended a first attempt at the acceptance pair after 155 of
  238 combinations). Launch anything longer as a detached process (on Windows: PowerShell `Start-Process`
  on a script with a hidden window and redirected output; the git-ignored `target/acceptance/pair2.sh` and
  `full.sh` are the ones in use) and follow it through a status file.
- A probe or baseline run made in a worktree names its directory `<date>-<host>[-n]` from what that worktree's `bench/results` holds; if another branch already holds a directory of that name, rename before committing (one build per directory) or use `--into` a directory prepared for it.
- Subagent worktrees start from `main`: before dispatching, make sure `main` is an ancestor of the task branch (merge `main` in), and give the implementer the expected head hash; their sandbox refuses `git reset --hard`.
- Implementers and reviewers run out of turns on large tasks: ask for a commit after each step, then resume them. Agents from an earlier session cannot be resumed; start a fresh one with the context it needs.
- A subagent that reports a refused command is not to be worked around; the refusal goes to the owner.
- Result directories are per machine (`bench/results/<date>-<host>`); never read their JSON into a session — use `run --validate`, `run --compare` and the report command. The git-ignored helpers `target/acceptance/per-repeat.py` and `class-diff.py` on the measuring machine print per-repeat times and per-class differences without dumping JSON.
- The CLAUDE.md `report` command line: `cargo run --release -p lpk-bench -- report --results <baseline dir> --probes <probe dir>`; the probe `--results` option names the root under which a new directory is created, while `run --validate` and `report --results` name the directory itself.
