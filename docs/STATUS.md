# Where the work stands

Hand-off between sessions and machines (D-13). Read this first; update it whenever work stops mid-task;
delete an entry when its branch is merged. `docs/PLAN.md` stays the source of truth for what is done.

Last updated: 2026-10-04, about 05:00 UTC (01:00 local on the measuring machine). No measurement is running.

**Waiting on the owner: the Phase 0 verdict (D-08).** The `full` measurement ran on 2026-10-03 (owner's
go-ahead), the official report is committed, a read-only reviewer recomputed every figure in it from the
JSON (no mismatch) and drafted the verdict text with the alternative readings of G2 and G3
(`docs/notes/phase0-verdict-review.md`). The owner records D-08 — or says the wording — and P0-5's last
box is ticked; Phase 0 is then complete.

## Summary
- **P0-1, P0-2, P0-3, P0-4, P0-6 done** and on `main` (PRs #2, #3, #4, #5 merged; every box ticked with evidence in PLAN; decisions D-19 to D-26).
  - P0-3's second acceptance pair (`bench/results/2026-10-03-megatron`, `-megatron-2`) met the round-trip and schema clauses twice over; the owner took the 3% clause as met in substance (D-24: 23 of 28 totals within 3%, the five others caused by the machine's clock and by single stalled sub-second repeats, not by the loop).
  - P0-4's official `small` probe run is `bench/results/2026-10-03-megatron-3`.
  - P0-6 cut `deny.toml` to what the tree needs (D-25) and fixed the LGPL boundary (D-26).
- **P0-5** (report, verdict): boxes 1 and 2 ticked. The official report `bench/reports/phase0-2026-10-03.md` (full profile: baseline `bench/results/2026-10-03-megatron-4`, 7 h 31 min, "238 measured, 0 failed, 34 skipped; 0 validation problem(s)"; probes `-megatron-5`, 2 h 15 min; build `78bc960`) computes **G1 PASS** (estimate 83.6% of the best incumbent, 81.0% of 7-Zip Ultra on the three photo/document classes combined), **G2 FAIL** (110.2% of zpaqfranz `-m5` on `backup-versions`), **G3 FAIL** (the `store` control writes `video` at 32.1% of the uncached raw read), **G4 PASS** (zstd/3 extraction above 7-Zip `-mx5` on all three mixes). Under D-07 as written that is a NO-GO proposal; the reviewer's draft D-08 and the alternative readings are in `docs/notes/phase0-verdict-review.md`. Left: the owner's D-08; then the third box.
- Two things the trial reports on `small` (both pairs) showed need the owner's reading before D-08: gate G2 (versioned backup, 2× smaller) fails because the incumbents already remove the cross-version redundancy inside solid archives (the estimate sits around 110% of zpaqfranz m5 on `small`), and gate G3 (video store speed) as proxied compares a cache-warm store pass with an uncached raw read (23% on `small`'s 66 MB video class). Neither is a measurement error; both are questions of what the gate should mean, to be answered in D-08 or by amending D-07 (a gate change is the owner's).

## Branches in flight
None. Everything is on `main` (`78bc960` or later); the main checkout of the measuring machine is on `main` with a clean release build of its head in `target/release/`.

## How to resume

### P0-5 — the verdict (what the owner decides)
The report applies D-07 literally. The two failing gates are definitional, not measurement errors — the reviewer's numbers (`docs/notes/phase0-verdict-review.md`, section 4):
- **G2** (versioned backup ≤ 50% of the best incumbent): the estimate is 110.2% of zpaqfranz `-m5` and 99.4% of 7-Zip Ultra, because the whole `backup-versions` class (25.5 MB, three versions) fits inside a solid incumbent's dictionary, so every solid setting already removes the cross-version redundancy; 2× against zpaqfranz would need version 1 alone compressed to 10% of its bytes. The estimate passes only against settings whose window is smaller than the class (rar/best non-solid 24.3%, zstd/19 33.9%, xz/6 34.2%) and is 7.3% of the raw bytes. Readings: (a) keep D-07 → NO-GO on G2; (b) measure G2 against a non-solid incumbent → PASS, but a weak claim; (c) the honest test of Fold is a versioned set larger than the incumbents' windows (gigabytes per version), which the corpus does not have — a corpus addition (P0-2 spec change, owner's) measured in Phase 1 with real LitePack output (draft task E2-9).
- **G3** (video stored at ≥ 80% of raw read): 32.1% as proxied, because the proxy reads *and writes* on one disk; the store control's own write rate is the bound any archiver shares. Without the write, a gate on the same thread would reach 50.0% (entropy), 55.1% (zstd level 1) or 93.8% (sampled entropy) of the raw read — arithmetic bounds from the probe, not measurements. Readings: (a) keep → NO-GO on G3; (b) define G3 as "LitePack's store path on video no slower than the `store` control" (at copy speed), measured in Phase 1; (c) define it as the gate's cost alone (read plus gate ≥ 80% of raw read).
- **G1** holds on the combined bytes only: `office-pdf` alone is 91.0% of the best incumbent and 89.1% of 7-Zip Ultra, outside both thresholds; the photo classes carry the pass. WinZip and PowerArchiver (both recompress JPEG) were not measured, so the fallback wording of D-07 applies.
1. The owner reads `docs/notes/phase0-verdict-review.md` section 3 (the draft D-08) and 4, and either records D-08 as drafted (NO-GO under D-07 as written) or amends D-07 for G2/G3 (a gate change is the owner's; it goes into `docs/DECISIONS.md` as its own entry) and records GO with the numbers.
2. Tick the third P0-5 box with the decision number; Phase 0 is complete. Then the Phase 1 breakdown (draft PR #8) is re-cut from the `full` report and adopted into PLAN.
- Parked from the measurement, low severity: the G3 "gate cost ... share of the raw read rate" lines use the probe's whole-corpus gate rates (on `video`'s own blocks zstd level 1 is 122.5%, not 30.9%) — information lines only, not the verdict; two `preflate-rs` 0.7.6 panics (`tree_predictor.rs:169`, index out of bounds) on PDF streams of `office-pdf`, caught and counted by the probe — a containment and fuzzing item for the Phase 1 Deflate peel and an upstream report; the validator's absolute-path false positive is fixed (PR #9).

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
