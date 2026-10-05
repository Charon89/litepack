# Where the work stands

Hand-off between sessions and machines (D-13). Read this first; update it whenever work stops mid-task;
delete an entry when its branch is merged. `docs/PLAN.md` stays the source of truth for what is done.

Last updated: 2026-10-05, about 01:42 UTC (2026-10-04, 21:42 local on the measuring machine). No measurement is running.
The owner stopped on the evening of 2026-10-04 and continues on Wednesday 2026-10-07.

**Where Phase 1 stands.** Epic E1 is complete (the format frozen at v1, revision 1.1 added for the JPEG peel). Of
E2's twenty-two tasks, done and on `main`: E2-1 ingest, E2-2 classifier and gate, E2-3 Fast tier, E2-4 pipeline
and the `lpk` tool with the pooled report, E2-5 JPEG peel (with the independent decoder passing all seventeen
vectors), E2-6 Balanced tier, E2-7 chunk dedup, E2-8 file ordering (negative result), E2-19 the extraction engine
(decisions D-44 to D-55). E0-1 is measured on `nodejs/node` and waits for the owner (below). Every `lpk` number
lives in `bench/reports/phase1-2026-10-04-*.md` with its results directory; the latest overall `small` picture is
`phase1-2026-10-04-small-extract.md`: the JPEG classes about 77% of their bytes against 94–97% for every incumbent;
Balanced at or within a few tenths of a percent of 7-Zip Ultra on the text classes, behind on installed software
and source trees; `lpk/fast` extracting faster than 7-Zip `-mx5` on every class but the JPEG ones; compression
still single-threaded.

**How to resume (in this order).**
1. `git pull --ff-only`; read this file and `docs/PLAN.md`'s Phase 1 list; the next task is **E2-12/13/14 (seal
   through the pipeline: encryption, recovery records, journal)** — its brief is `docs/notes/e2-12-14-seal-brief.md`
   and an implementer's design notes with one blocker are `docs/notes/e2-12-14-seal-resume.md` (the format's
   writer refuses `add_record` in an append, so a peeled JPEG cannot join an appended generation without a small
   writer addition — decide option 1 of that note (allow records after the old ones; a reference change, no byte
   change) unless the owner prefers unpeeled appends). No code of those tasks exists yet.
2. Then E2-4b (parallel block encoding: a writer extension holding blocks in flight) and E2-18 (the video target:
   `lpk/fast` writes `video` at about a third of the `store` control's rate; G3 as restated in D-27 needs 80%),
   E2-20 (memory and the envelope), then the revision-gated peels E2-10/11 (Deflate and containers — the largest
   remaining size win on office, PDF and installed software), E2-15, E2-16, E2-17 (D-49 process), then E2-21/22.
3. E2-9 (deltas) waits for two things from the owner (below).
4. The method: implementer in a worktree from a brief with exact values → read-only review → fix rounds with
   recorded rulings → a `D-NN` → PLAN tick with evidence → PR → CI green → `main` fast-forwarded (merge `main`
   into the branch first; squash only when a branch commit message carries figures).

**Open for the owner (asked on 2026-10-04, unanswered):**
- **The versioned set (E0-1, G2).** On node v22/v23/v24 the Phase 0 estimate is 100.3% of 7-Zip Ultra
  (`phase1-2026-10-04-full-versioned.md`): releases six months apart share little at chunk level. Recommended:
  consecutive patch releases of one line (`v22.0.0`, `v22.1.0`, `v22.2.0`), a backup-like series, each still above
  the 256 MiB window. "patch releases" → re-pin (sources, lock, CORPUS row, D-NN), rebuild `full` twice, re-run the
  baseline on the class and `probe dedup` (about three hours of machine time), report; "keep" → tick E0-1 as is.
- **Nine file names.** The Windows `tar` crashes on node's emoji and CJK test-file names, so the `store` control and
  the tar-stream tools (zstd, xz) have no rows on the class. Recommended: exclude names outside ASCII from this
  class's export (nine of 127153 files), re-run the three tools.
- **Counsel on US 9,798,731** (`docs/LICENSING.md`, sketch-based similarity clustering with deltas): gates E2-9
  (deltas) and any return of the similarity sketch (E2-8, D-55).

## Summary
- **P0-1, P0-2, P0-3, P0-4, P0-6 done** and on `main` (PRs #2, #3, #4, #5 merged; every box ticked with evidence in PLAN; decisions D-19 to D-26).
  - P0-3's second acceptance pair (`bench/results/2026-10-03-megatron`, `-megatron-2`) met the round-trip and schema clauses twice over; the owner took the 3% clause as met in substance (D-24: 23 of 28 totals within 3%, the five others caused by the machine's clock and by single stalled sub-second repeats, not by the loop).
  - P0-4's official `small` probe run is `bench/results/2026-10-03-megatron-3`.
  - P0-6 cut `deny.toml` to what the tree needs (D-25) and fixed the LGPL boundary (D-26).
- **P0-5** (report, verdict): done. Official report `bench/reports/phase0-2026-10-03.md` (full profile; baseline `bench/results/2026-10-03-megatron-4`, probes `-megatron-5`, build `78bc960`): G1 PASS (83.6% of the best incumbent, 81.0% of 7-Zip Ultra on the photo/document classes combined), G4 PASS (zstd/3 extraction above 7-Zip `-mx5` on all three mixes), G2 and G3 FAIL as written — on their definitions, not the method (the versioned class fits inside one dictionary window; a read-plus-write proxy against a read-only rate). The owner recorded **D-08 GO** and **D-27** (G2 on a versioned set larger than the incumbents' windows, to be added to the corpus; G3 as the store path at ≥ 80% of the `store` control). The arithmetic of the report was recomputed in full by a read-only review (`docs/notes/phase0-verdict-review.md`, no mismatch).
- Two things the trial reports on `small` (both pairs) showed need the owner's reading before D-08: gate G2 (versioned backup, 2× smaller) fails because the incumbents already remove the cross-version redundancy inside solid archives (the estimate sits around 110% of zpaqfranz m5 on `small`), and gate G3 (video store speed) as proxied compares a cache-warm store pass with an uncached raw read (23% on `small`'s 66 MB video class). Neither is a measurement error; both are questions of what the gate should mean, to be answered in D-08 or by amending D-07 (a gate change is the owner's).

## Branches in flight
None. Every task branch is merged and deleted; the E2-8 branch's unmerged sketch commits (3be4d83, b599566) are
referenced from D-55 and remain in the repository's history through the merged branch.

## How to resume

### Phase 1 — how it runs
See "How to resume" above. Parked items, to pick up in the tasks named:
- From Phase 0: the G3 "gate cost … share of the raw read rate" lines of the report use the probe's whole-corpus gate rates (information lines only); two `preflate-rs` 0.7.6 panics on PDF streams, caught and counted by the probe — containment and fuzzing item for E2-10; WinZip and PowerArchiver unmeasured (the D-07 fallback wording stands); `bsc`/`hdiffz` not installed.
- From E2-1 (low): on Unix a non-UTF-8 dot-name excluded by `include_hidden: false` still fails the walk; the per-file identity open on Windows costs on many-small-files trees; the Unix-only ingest tests run on CI only.
- From E2-5: the `decode_memory` fixed term of `jpeg-reconstruct` is an allowance, not measured (E2-20); the Lepton stream bytes depend on the zlib backend (vector checked structurally under another backend, byte-identically in CI).
- From E2-6: `text-prose` and `office-pdf` a tenth or two of a percent above 7-Zip Ultra (the coder); `source-git` and `software-installed` behind (ordering gave nothing on `small`; BCJ waits for a format revision).
- From E2-19: opening relative to a verified directory handle is the stronger parent-chain rule (E3/E6); flags and directory times are not restored (as the format tool); the writer thread count is unmeasured on `full` and on HDDs.
- The report takes one baseline directory per corpus per run; E2-22 pools directories (D-48) — a combination measured twice is refused, so each `lpk` run on a class supersedes by a new directory, never by overwriting.

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
