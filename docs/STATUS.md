# Where the work stands

Hand-off between sessions and machines (D-13). Read this first; update it whenever work stops mid-task;
delete an entry when its branch is merged. `docs/PLAN.md` stays the source of truth for what is done.

Last updated: 2026-10-05, about 00:05 UTC (2026-10-04, 20:05 local on the measuring machine). No measurement is running.

**Epic E1 is complete and on `main`; E2 has started.** The `.lpk` v1 format is frozen (D-41). The first CI fuzz run
(37204202840, 120 s per target, fifteen targets) was clean. **E2-1 (ingest and the store path of `lpk-core`) is
done (D-44)**: buffered reads for every size (no memory map), opens that never follow a link and verify the identity
the walk recorded, bounded reads, a synced commit, the archive never its own input. **E0-1 (the larger versioned set) is waiting for the owner:** the harness side is on `main` (the `backup-versions-large`
spec row and lock entries, the `dedup` catalogue flag, the G2 rule against the non-deduplicating incumbents with
zpaqfranz printed as reference, the 256 MiB premise checked per version). The class was built (the `full` rebuild
kept the seventeen old classes byte-identical; a second build of the class was identical), **but the godot working
trees are 173, 196 and 245 MiB — all under WinRAR's 256 MiB window**, so D-43's premise fails and the report would
print G2 as not evaluable. Measured alternatives (shallow clones, deleted afterwards): `nodejs/node` at v22.0.0,
v23.0.0, v24.0.0 is about 535–543 MiB per version (MIT; the recommendation); `llvm/llvm-project` at llvmorg-19.1.7
is about 1.6 GiB (would need the runner's per-step timeout raised for zpaqfranz). Once the owner picks the source:
re-pin the three sources in `bench/corpus-sources.toml` and `bench/corpus.lock` (`corpus build --profile full
--update-lock --only backup-versions-large`, then a plain `full` build twice — the last rebuild took seven
minutes from the cache), amend the CORPUS.md row (owner-approved), then `run --tools all --profile full --classes
backup-versions-large` and `probe dedup --profile full --into <that directory>` as detached processes, `report`
for the G2 row, commit the directory and tick E0-1.
**E2-2 (classifier and entropy gate) is done (D-45); E2-3 (the Fast tier) is done (D-46 writer API, D-47 the tier —
no bundled dictionaries, BCJ waits for a format revision); E2-4 (pipeline struct, the `lpk` tool, the catalogue row,
the pooled report) is done (D-48).** The first real `lpk` rows exist: `bench/results/2026-10-04-megatron` (`small`,
`lpk/fast` and `lpk/store`, every extraction verified) and the pooled report
`bench/reports/phase1-2026-10-04-small-lpk.md` next to the committed incumbents. What they say, for the speed tasks:
`lpk/fast` reaches `zstd/3`'s sizes but runs single-threaded; extraction of many-small-file classes is far below the
store path's because files are extracted in entry order across blocks (decode each block once — E2-19); the Fast
tier's store throughput on `video` is about a third of the `store` control's (E2-18, G3 as restated in D-27). Parallel
block encoding (E2-4b, a writer extension) comes before or with E2-19.
**E2-5 (JPEG peel) is nearly done:** the first format revision (1.1, `jpeg-reconstruct`; D-49 the revision process,
D-50 the peel), the peel in `lpk-core`, the independent decoder updated from the text alone (all 17 vectors) and
its fifteen clarifications written back. Measured on `small` (`bench/results/2026-10-04-megatron-2`, report
`bench/reports/phase1-2026-10-04-small-jpeg.md`): the real `lpk/fast` rows on the two JPEG classes reproduce the Phase 0
estimate within the container overhead and sit below every incumbent; extraction is single-threaded Lepton, far
below the incumbents' rate, so the acceptance's "parallel decode" clause stays open until E2-19. **E2-6 (Balanced tier) is done (D-51)**: on `small` it is below 7-Zip Ultra on `logs-text` and a tenth or two of a
percentage point above on `text-prose` and `office-pdf` (near-parity, recorded as such); behind on `source-git` and
`software-installed` (file ordering, BCJ). **E0-1 is measured on `nodejs/node` v22/v23/v24 (D-52) and waits for two owner decisions:** the G2 estimate on that set
is 100.3% of 7-Zip Ultra (`bench/reports/phase1-2026-10-04-full-versioned.md`) because major releases six months
apart share little at chunk level — a backup-like series (consecutive patch releases of one line, weeks apart) is the
honest model for the gate; and the Windows `tar` crashes on nine node file names outside the BMP or in CJK, so the
`store`, zstd and xz rows are missing until those names are excluded from the export (or another tar is used).
**E2-7 (Fold: CDC chunking and dedup) is done (D-53)** — the acceptance came out equal to the probe's figure.
**E2-19 (extraction performance) is done (D-54), pulled forward:** block-ordered, every block decoded once by a
parallel pool under a memory bound, any failure cleaned up; on `small` the classes that used to re-decode blocks
extract tens of times faster; the G4 reading by the runner is the next measurement. E2-5's last clause (parallel JPEG
decode) is met by it. **Next: E2-8 (file ordering), then E2-12/13/14 (seal), E2-18 (video target), E2-20 (memory);
the revision process (D-49) serves E2-9, E2-10/11, E2-15, E2-16, E2-17.** All three `lpk` tiers are measured on every class
of `small` (`bench/reports/phase1-2026-10-04-small-tiers.md`); the report never counts `lpk` as an incumbent.
Then E2-7/E2-8 (Fold), E2-12/13/14 (seal), E2-18/19/20 (speed, memory); the revision process (D-49)
serves E2-9 (delta), E2-10/11 (deflate, container), E2-15 (bwt), E2-16 (png-filter), E2-17 (base64, utf16).

## Summary
- **P0-1, P0-2, P0-3, P0-4, P0-6 done** and on `main` (PRs #2, #3, #4, #5 merged; every box ticked with evidence in PLAN; decisions D-19 to D-26).
  - P0-3's second acceptance pair (`bench/results/2026-10-03-megatron`, `-megatron-2`) met the round-trip and schema clauses twice over; the owner took the 3% clause as met in substance (D-24: 23 of 28 totals within 3%, the five others caused by the machine's clock and by single stalled sub-second repeats, not by the loop).
  - P0-4's official `small` probe run is `bench/results/2026-10-03-megatron-3`.
  - P0-6 cut `deny.toml` to what the tree needs (D-25) and fixed the LGPL boundary (D-26).
- **P0-5** (report, verdict): done. Official report `bench/reports/phase0-2026-10-03.md` (full profile; baseline `bench/results/2026-10-03-megatron-4`, probes `-megatron-5`, build `78bc960`): G1 PASS (83.6% of the best incumbent, 81.0% of 7-Zip Ultra on the photo/document classes combined), G4 PASS (zstd/3 extraction above 7-Zip `-mx5` on all three mixes), G2 and G3 FAIL as written — on their definitions, not the method (the versioned class fits inside one dictionary window; a read-plus-write proxy against a read-only rate). The owner recorded **D-08 GO** and **D-27** (G2 on a versioned set larger than the incumbents' windows, to be added to the corpus; G3 as the store path at ≥ 80% of the `store` control). The arithmetic of the report was recomputed in full by a read-only review (`docs/notes/phase0-verdict-review.md`, no mismatch).
- Two things the trial reports on `small` (both pairs) showed need the owner's reading before D-08: gate G2 (versioned backup, 2× smaller) fails because the incumbents already remove the cross-version redundancy inside solid archives (the estimate sits around 110% of zpaqfranz m5 on `small`), and gate G3 (video store speed) as proxied compares a cache-warm store pass with an uncached raw read (23% on `small`'s 66 MB video class). Neither is a measurement error; both are questions of what the gate should mean, to be answered in D-08 or by amending D-07 (a gate change is the owner's).

## Branches in flight
None at the moment.

## How to resume

### Phase 1 — how it runs
1. One task at a time from `docs/PLAN.md` (E2-8 next; E0-1's re-measurement once the owner answers): implementer in a worktree from a brief
   with the exact values → read-only review (spec compliance and quality, benchmark honesty for harness tasks) →
   fix rounds with rulings → a decision in `docs/DECISIONS.md` → PLAN tick with evidence → PR → CI green →
   `main` fast-forwarded (merge `main` into the branch first).
2. E0-1's measurement half (above) is the next long machine job; it needs the machine to itself for the baseline
   run (zpaqfranz `-m5` on three gigabyte-sized trees is the slow part).
3. The report takes one baseline directory. E2-22 needs either a full `--tools all` re-run that includes `lpk`, or
   a report that merges result directories by class — decide when E2-4 gives the runner an `lpk` tool.
- Parked from Phase 0, to pick up in the tasks named: the G3 "gate cost … share of the raw read rate" lines of the report use the probe's whole-corpus gate rates rather than the video class's own (information lines only; a report fix round); two `preflate-rs` 0.7.6 panics (`tree_predictor.rs:169`, index out of bounds) on PDF streams, caught and counted by the probe — containment and fuzzing item for E2-10 and an upstream report; WinZip and PowerArchiver still unmeasured (the D-07 fallback wording stands until the owner installs them); `bsc`/`hdiffz` not installed (D-05's BWT claim is measured in E2-15 with libsais instead).
- Parked from E2-1 (low): on Unix a non-UTF-8 dot-name excluded by `include_hidden: false` still fails the walk; the per-file identity open on Windows costs on many-small-files trees (the speed task); the Unix-only ingest tests run on CI only.

### P0-5 — Linux CI smoke test (done)
The manual workflow `corpus-smoke.yml` has an input `run_smoke` (default on): after the corpus build it installs `zstd`, `xz-utils` and `7zip`, runs the baseline runner on four classes with one repeat, validates and uploads the results as an artifact (CI results are never committed). The green run and its summary lines are cited under the P0-5 box in PLAN. Nothing else is needed from CI for Phase 0.

## Open points for the owner
- **Local fuzzing through WSL**: optional; the CI job runs the fuzzers on Linux (manual and weekly). A long local run means installing rustup and `cargo-fuzz` for the WSL user (both would go into the removal inventory).
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
