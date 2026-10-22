# Where the work stands

Hand-off between sessions and machines (D-13). Read this first; update it whenever work stops mid-task;
delete an entry when its branch is merged. `docs/PLAN.md` stays the source of truth for what is done.

Last updated: 2026-10-22, about 01:45 UTC. No measurement is running. **E3-3 is done (D-87, PR #92), the three stale corpus pins are re-pinned (D-88, PR #93), and the owner answered the gate and product questions on 2026-10-22: D-89.** E2-22's verdict input is D-85 (`bench/reports/phase1-2026-10-21-full-gates.md`): G1 PASS; G2, G3, G4 FAIL, each now with its work ruled (D-89). E3-4 is next; the D-89 work is planned in PLAN after it.
The owner asked on 2026-10-16 for the work to continue; this file is kept current as each task lands.

**Where Phase 1 stands.** Epic E1 is complete (the format frozen at v1, revision 1.1 added for the JPEG peel; on
2026-10-16 E1-15 wrote the Reed-Solomon code out in section 13, proven by an independent repair in `lpk-check`, and
a forgotten second-reading commit of 2026-10-04 was landed — both text only, D-62). Of
E2's twenty-two tasks, done and on `main`: E2-1 ingest, E2-2 classifier and gate, E2-3 Fast tier, E2-4 pipeline
and the `lpk` tool with the pooled report, E2-5 JPEG peel (with the independent decoder passing all eighteen
vectors), E2-6 Balanced tier, E2-7 chunk dedup, E2-8 file ordering (negative result), E2-19 the extraction engine
(decisions D-44 to D-55), and from 2026-10-13: E2-13 recovery records and E2-14 the journal (a negative result on
its size clause, D-59) with the format's records-in-appends change they needed (D-56, PR #46), and E2-12
encryption (D-57; the sealed row measured on `small`, `bench/reports/phase1-2026-10-13-small-seal.md`: the same
size as the plain row on every class, a fixed key-derivation cost per archive, a higher working set noted for
E2-20). `cargo deny` is green again after the `ruzstd` advisory bump (PR #45). E2-4b (parallel block encoding,
D-60) is done: the Fast tier compresses `small`'s multi-block classes near the store path's rate
(`phase1-2026-10-14-small-threads.md`) and the Balanced tier about three times faster on `vm-image` of `full`
with four blocks in flight (`phase1-2026-10-14-full-vm-image-threads.md`), archive sizes equal, at a working set
of several GiB for Balanced — the first input of E2-20.
E0-1 is measured on `nodejs/node` and waits for the owner (below). Every `lpk` number
lives in `bench/reports/phase1-2026-10-04-*.md` with its results directory; the latest overall `small` picture is
`phase1-2026-10-04-small-extract.md`: the JPEG classes about 77% of their bytes against 94–97% for every incumbent;
Balanced at or within a few tenths of a percent of 7-Zip Ultra on the text classes, behind on installed software
and source trees; `lpk/fast` extracting faster than 7-Zip `-mx5` on every class but the JPEG ones; compression
was single-threaded then and is parallel since E2-4b. Since 2026-10-17: E2-20 (memory budgets on both sides,
D-67), E2-11 and E2-10 (the Deflate peel through format revision 1.2, D-68 to D-71) and E2-21 (the full corpus
round-trips with 0 mismatches, D-72) are done; E2-15's gate opened (D-73) and BWT ships through a revision 1.3.
Open in E2: E2-15's shipping, E2-16, E2-17, E2-22, and E2-9 (the owner's). E3 is adopted into PLAN from
`docs/notes/phase1-e3-e9-breakdown.md`, which also drafts E4 to E9 and lists the decisions each epic needs;
E3-8 is done (D-75, PR #77).

**How to resume (in this order).**
1. `git pull --ff-only`; read this file and `docs/PLAN.md`'s Phase 1 list.
2. Revision 1.3 is final (D-78 (8)), the Balanced tier writes it (D-81) and E2-15 is done (D-82). **Open:** the
   one failed round trip of 2026-10-20 (`lpk/balanced/office-pdf`, two files different; 52 later round trips
   clean, 40 of them a stress loop at 24 threads). The runner now keeps a failed verification's archive and
   tree and records each difference's kind (D-86, PR #90), so the next occurrence is diagnosable; no cause yet.
   `png-filter` would be revision 1.4 if the owner accepts E2-16's margin (below). E2-17 closed (D-76).
3. E2-22 is done (D-85). The `lpk` rows (`bench/results/2026-10-20-megatron-2`) and the single-run G3 row
   (`2026-10-21-megatron`) are committed with the report. The report's gate table comes from `lpk-bench report`'s
   Phase 1 form (PR #89: measured `lpk` rows; G3 names each row's run). The owner's rulings are D-89 (G2: E2-9 and the ordering; G3 and G4: engine
   rounds, then the class re-measured); a re-measurement after any engine change is one detached run of `lpk` on `full` plus the
   same-run G3 script (`target/acceptance/e2-22-lpk-full.sh`, `e2-22-g3.sh`), pooled with the committed
   incumbents' baseline. A measurement needs the machine idle: no agent builds or tests while it runs.
4. E3: E3-1, E3-2, E3-3, E3-5, E3-6, E3-7 and E3-8 done (D-75, D-77, D-79, D-80, D-84, D-87). **Next E3-4**
   (several inputs, a list file, selective extraction); its rulings, so a new machine can brief it: `lpk a`
   takes several inputs, each entered under its own name (a directory `D:\x\photos` gives `photos/...`, a
   file its name; two inputs with the same entry name, or one a prefix path of another, refused before any
   output, exit 1), the single-directory form unchanged (so every archive and pin stays byte-identical);
   `--list <file>` one path per line (UTF-8, CRLF tolerated, `#` lines ignored, relative to the current
   directory), and the catalogue row gains the runner's private-corpus template (D-20 (6)); `--exclude <glob>`
   repeatable over entry paths with `/`, a directory match prunes its subtree (an allow-listed glob crate);
   `lpk x|l|export <archive> ... [<pattern>...]` globs over entry paths (a directory pattern selects its
   subtree), fed to the engine's plan so only the blocks the selection needs are decoded (asserted in a test),
   a pattern matching nothing refused (exit 2). E3-9 re-runs the harness after the G3 and G4 rounds (D-89).
   E2-9 (deltas) is unblocked by D-89 (1): E0-1's re-pin first, then the delta stage with versions paired by
   path and name (no similarity sketch), then the class re-measured.
   Housekeeping found on the way: the ignored corpus tests (`fast_corpus`, `store_path_corpus`,
   `deflate_pinned_corpus`) were stale on `main` for decided reasons and are re-pinned (D-88); run them by
   hand with an absolute `LPK_CORPUS` (a test's working directory is its crate's). E3-6's export tests
   compared mtimes with the live tree and flaked on NTFS (a directory's last-write time moves after the
   walk); they compare with the archive's entries now (PR #92, `a424845`). E3-3's progress line deadlocked
   when stderr was a terminal (`main` held the stderr lock across the run while the engine's threads wrote
   the line; found by the Linux pseudo-terminal test): `main` passes the unlocked handles now, and a pty test
   pins it.
5. The method: implementer in a worktree from a brief with exact values → read-only review → fix rounds with
   recorded rulings → a `D-NN` → PLAN tick with evidence → PR → CI green → `main` fast-forwarded (merge `main`
   into the branch first; squash only when a branch commit message carries figures). Learned on 2026-10-13: the
   harness gives every subagent its own worktree `.claude/worktrees/agent-<id>` on a branch `worktree-agent-<id>`
   cut from `main`, whatever worktree the dispatch names — an implementer continuing a task branch must first
   `git merge --ff-only <task branch>` (so keep the task branch a descendant of `main`), and the orchestrator
   merges the agent branch into the task branch afterwards; implementers and reviewers still stop at their turn
   limits on tasks of this size and are resumed with a short step list.
   While a measurement runs on the measuring machine, no agent builds, tests or runs anything there: the Agent
   tool's `isolation: remote` ran as a local worktree agent on 2026-10-17 without an error, so it is no
   protection. Read-only agents (designs, static reviews told not to run cargo) are fine; GitHub CI builds and
   tests pushed branches.

**Open for the owner (the 2026-10-22 answers are D-89; what remains):**
- **The publisher's legal name and the product name** for the code-signing certificate (E7-1: a certificate
  in the publisher's name, Azure Artifact Signing or an OV certificate, since a later version may be closed,
  D-89 (6)). Identity validation takes weeks; the earlier the better.
- **US 9,798,731** (`docs/LICENSING.md`): E2-9's deltas pair a file's versions by path and name, with no
  similarity sketch, so as far as this session can read it the claimed method is not practised; counsel's
  reading is still the owner's to get if sketch-based clustering (E2-8, D-55) is ever wanted back.
- **Idle time for the measurements:** E0-1's rebuild and re-measurement (about three hours), E3-9, and every
  re-measurement after an engine change need the machine to itself; say when it can sit idle, or they run
  detached overnight as before, announced in this file.
- Optional, nothing waits: E2-17's extra corpus class (D-76); the `bsc`/`hdiffz` probe baselines; the antivirus
  setting during measurements; the corpus gaps in the P0-2 notes; WinZip and PowerArchiver (unmeasured, the D-07
  fallback wording stands); the method document's "xEnc3" (marked unusable in `docs/LICENSING.md`); the IJG
  licence option (D-18); the bundled xz fast paths. Revision 1.3's coder fold is closed by 1.3 being final.

## Summary
- **P0-1, P0-2, P0-3, P0-4, P0-6 done** and on `main` (PRs #2, #3, #4, #5 merged; every box ticked with evidence in PLAN; decisions D-19 to D-26).
  - P0-3's second acceptance pair (`bench/results/2026-10-03-megatron`, `-megatron-2`) met the round-trip and schema clauses twice over; the owner took the 3% clause as met in substance (D-24: 23 of 28 totals within 3%, the five others caused by the machine's clock and by single stalled sub-second repeats, not by the loop).
  - P0-4's official `small` probe run is `bench/results/2026-10-03-megatron-3`.
  - P0-6 cut `deny.toml` to what the tree needs (D-25) and fixed the LGPL boundary (D-26).
- **P0-5** (report, verdict): done. Official report `bench/reports/phase0-2026-10-03.md` (full profile; baseline `bench/results/2026-10-03-megatron-4`, probes `-megatron-5`, build `78bc960`): G1 PASS (83.6% of the best incumbent, 81.0% of 7-Zip Ultra on the photo/document classes combined), G4 PASS (zstd/3 extraction above 7-Zip `-mx5` on all three mixes), G2 and G3 FAIL as written — on their definitions, not the method (the versioned class fits inside one dictionary window; a read-plus-write proxy against a read-only rate). The owner recorded **D-08 GO** and **D-27** (G2 on a versioned set larger than the incumbents' windows, to be added to the corpus; G3 as the store path at ≥ 80% of the `store` control). The arithmetic of the report was recomputed in full by a read-only review (`docs/notes/phase0-verdict-review.md`, no mismatch).
- Two things the trial reports on `small` (both pairs) showed need the owner's reading before D-08: gate G2 (versioned backup, 2× smaller) fails because the incumbents already remove the cross-version redundancy inside solid archives (the estimate sits around 110% of zpaqfranz m5 on `small`), and gate G3 (video store speed) as proxied compares a cache-warm store pass with an uncached raw read (23% on `small`'s 66 MB video class). Neither is a measurement error; both are questions of what the gate should mean, to be answered in D-08 or by amending D-07 (a gate change is the owner's).

## Branches in flight
- `task/e3-4` (E3-4) once it is dispatched; nothing else of this session's (PRs #92 and #93 merged).
- Not this session's: PRs #57 and #59 (`claude/*` branches of another session, 2026-10-17), left as they are.

## How to resume

### Phase 1 — how it runs
See "How to resume" above. Parked items, to pick up in the tasks named:
- From Phase 0: the G3 "gate cost … share of the raw read rate" lines of the report use the probe's whole-corpus gate rates (information lines only); two `preflate-rs` 0.7.6 panics on PDF streams, caught and counted by the probe — containment and fuzzing item for E2-10; WinZip and PowerArchiver unmeasured (the D-07 fallback wording stands); `bsc`/`hdiffz` not installed.
- From E2-1 (low): on Unix a non-UTF-8 dot-name excluded by `include_hidden: false` still fails the walk; the per-file identity open on Windows costs on many-small-files trees; the Unix-only ingest tests run on CI only.
- From E2-5: the `decode_memory` fixed term of `jpeg-reconstruct` is an allowance, not measured (E2-20); the Lepton stream bytes depend on the zlib backend (vector checked structurally under another backend, byte-identically in CI).
- From E2-6: `text-prose` and `office-pdf` a tenth or two of a percent above 7-Zip Ultra (the coder); `source-git` and `software-installed` behind (ordering gave nothing on `small`; BCJ waits for a format revision).
- From E2-19: opening relative to a verified directory handle is the stronger parent-chain rule (E3/E6); flags and directory times are not restored (as the format tool); the writer thread count is unmeasured on `full` and on HDDs.
- From E2-12: `lpk t|check|repair|rollback|info` hand the password to the format tool as an argument string that is not wiped, and the clap `String` lives until exit (no `zeroize` in lpk-cli; the format tool wipes its own copy) — a task that touches `lpk-format` should re-export `Zeroizing` or add a `run_with_credentials` entry point; the format tool accepts an unneeded `--keyfile` on a password-only archive (E3-5 takes both).
- From the records change (D-56): the append constructors require `R: 'static` (the old archive is boxed into the writer); an append over an archive with an unparsable `Records` frame is refused at open; the tests' with/without pre-loading comparison is against the committed vector; `Pipeline::append` (opener-based) is the shared target `append_file` builds on.
- From E2-13: the product's default recovery percentage is ruled for E3: the CLI keeps 0, the GUI proposes 5 (PLAN, the E3 header).
- From E2-14: the size cost of a changed chunk in an appended generation (D-59) is E2-9's; an append reads the old entry table's paths into memory for the snapshot deletion.
- From E2-4b: the JPEG peel still runs on the calling thread (the photo classes gain nothing from the pool; a peel pool is its own task); the pool is one thread per block in flight, not a persistent worker set; the Balanced cap of four blocks in flight is provisional (E2-20); `StoreOptions::threads` (None = the cores) overwrites `encode_threads`; the core and CLI identity tests use flat trees because NTFS moved a directory's mtime between two walks of a fresh temp tree.
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
