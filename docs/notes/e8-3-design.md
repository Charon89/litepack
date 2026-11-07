# E8-3: the weekly speed-regression run on the measuring machine (design as built)

PLAN task E8-3: time is gated only on the measuring machine (Megatron), never on shared CI, with
each delta read against D-24's noise; a gate change stays the owner's. This note is the design as
built from the planning draft of 2026-11-04, with the owner's answers and the controller's rulings
of 2026-11-07 (R1-R17) and the read-only review's findings. The weekly runs themselves, and the
acceptance evidence, come after the merge.

## The pieces

- `lpk-bench run --compare A B --across-builds` (`crates/lpk-bench/src/run/compare.rs`). `lpk`'s
  version carries the build hash (`lpk 0.x.y+<commit>`), so two weeks of different builds never
  compare clean without the flag. With it, a different version of the tool this workspace builds
  (`discover::WORKSPACE_TOOL`, the catalogue's `lpk` row) is a `note:` naming both versions. Every
  other refusal stays a problem: corpus, host, requested repeats, `--long-run-s`, the D-21 settle
  pause, catalogue file, thread count, classes, a combination measured on one side only, and any
  other tool's version: the `7z` control must be the same binary in both weeks. Without the flag
  nothing changes.
- `bench/tools-weekly.toml`: `bench/tools.toml`'s `7z` and `lpk` rows copied verbatim (a unit test
  checks they equal the main catalogue's rows). Directory mode only, so there is no `store` row.
  `bench/tools.toml` changes often, and `run --compare` refuses two runs whose catalogue files
  differ (`catalogue_blake3` in `run.json`); the weekly file changes only on purpose, and **editing
  it starts a new baseline**.
- `scripts/e8-3-weekly.sh`: the run (below). `scripts/e8-3-weekly-test.sh` tests its pure parts
  (the compare verdicts on canned compare output, the measurement week, the MISSED lines across a
  year end) through the script's own test modes.
- `scripts/e8-3-launch.ps1`: the launcher the task runs (below).
- `scripts/e8-3-task.ps1 -Register | -Remove [-Day Sunday] [-At 03:00] [-FallbackDay Monday]`: the
  scheduled task.

`lpk-bench` is rebuilt from `origin/main` every week too, not only `lpk`: a harness change moves
the `lpk` rows and the 7z control alike, and the control is what tells the two apart.

## The schedule

The task `\LitePack\E8-3 weekly speed` runs for the owner's account, only while the owner is
logged on (an interactive logon, no stored password, RunLevel Limited; a locked screen counts as
logged on). Two weekly triggers: **Sunday 03:00 local, and the fallback Monday 03:00**. It may wake
the PC from sleep (the owner's answer of 2026-11-07), ignores a second start while one runs
(`IgnoreNew`), stops after 8 hours, and runs at priority 4 (`NORMAL_PRIORITY_CLASS`, like every
earlier measurement; a task's default, 7, is below normal). It has no idle condition and no
`StartWhenAvailable`, so there is no catch-up start at logon, which would measure a busy machine.
It is not hidden: the owner sees it under `\LitePack\` in Task Scheduler. The Task Scheduler
service starts it, outside the Claude app's process tree (STATUS's working note: a `Start-Process`
run died with the app).

Waking works only where the power plan's "Allow wake timers" is on; no script changes it (R1).

### The launcher

The task's action is
`pwsh.exe -NoProfile -NonInteractive -WindowStyle Hidden -File <main>\scripts\e8-3-launch.ps1`
(`pwsh.exe` found with `Get-Command pwsh` when the task is registered; no `-ExecutionPolicy`,
since PowerShell 7's default allows local scripts). A console window may flash for a moment when
it starts; after that the run has no window. The launcher replaces the task's idle condition:
after an unattended wake Windows goes back to sleep after its unattended-sleep timeout (about two
minutes), so a task waiting for 15 idle minutes would never start. In order, it:

1. finds the main checkout from its own location;
2. if `scripts/e8-3-weekly.sh` is missing there (the checkout is on an older branch), appends
   `REFUSED the weekly script is missing from the main checkout (HEAD <branch or sha>)` to the
   status log and exits 0;
3. reads the time since the last keyboard or mouse input (`GetLastInputInfo`); under 15 minutes it
   appends `REFUSED user input <n> min ago` and exits 0;
4. calls `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`, so the PC stays awake while
   the run lasts: a transient request that ends with the process, not a setting;
5. starts Git for Windows' `bash.exe` (found from git's own location, never the WSL `bash.exe` on
   PATH) on `scripts/e8-3-weekly.sh` with any passed-through arguments, waits, and exits with
   bash's code.

`-DryRun` passes `--dry-run` through and prints the launcher's own refusals instead of logging
them.

**At most one complete run per measurement week.** The measurement week is the ISO week of the
day before the run (`week_key_at`: the run's time minus one day, `+%G-W%V`), so the Monday fallback
counts with the Sunday before it: Sunday 2027-01-03 and Monday 2027-01-04 are both 2026-W53. The
fallback only does something when the week has no complete run yet (a GREEN, OVER or BASELINE
ledger line; a FAILED week runs again on Monday). There is no automatic re-measure (R6).

### Stopping a run

End the task in Task Scheduler (right-click, End), then end any `bash.exe`, `lpk-bench.exe`,
`lpk.exe` or `7z.exe` still running (Task Manager, by process, not by image name across the
machine if other work runs). The next run finds the lock's pid gone and takes it over.

## Where things live

Every path derives from `git rev-parse --git-common-dir`, i.e. the main checkout, whatever copy of
the scripts runs; nothing machine-specific is in git.

- The weekly worktree: `<main>/.claude/worktrees/e8-3-weekly`, a detached worktree created on first
  use (ignored by `.gitignore`'s `/.claude/worktrees/`; not under `target/`, which `cargo clean`
  deletes). A session's `cargo build` elsewhere never replaces its `target/release/lpk.exe`
  mid-run, and the owner's checkout is never switched. Its `target/` grows slowly with each week's
  incremental build; a session may `cargo clean` it between runs (the next build is then a full
  one).
- The corpus: `<main>/bench/corpus/small`, passed with `--corpus` (the runner's `--corpus` names the
  directory holding `manifest.json`, so the profile's directory, not `bench/corpus` itself).
- State, under `<main>/target/acceptance/`:
  - `e8-3-weekly-status.log`: the measurement nights' `== <step> exit <code> <UTC>` lines, a start
    line with the main checkout's commit, the Windows pid and the measurement week, and
    `REFUSED <reason> <UTC>` lines (from the script and the launcher).
  - The per-step logs `e8-3-<dir>-fetch.log`, `-checkout.log`, `-build.log`, `-run.log`,
    `-validate.log` and `-compare.log`. They are written as `e8-3-current-<step>.log` and renamed
    once the run has named its results directory; a run that fails before that names them
    `e8-3-<YYYY-Www>-<step>.log`.
  - `e8-3-weekly-ledger.log`, one line per measurement week:
    `WEEK <YYYY-Www> <dir> <build sha7> GREEN|OVER|BASELINE|FAILED|MISSED [reason]`
    (`-` for a directory or build that does not exist).
  - `e8-3-weekly.lock`: the lock directory, holding the running bash's Windows pid
    (`/proc/$$/winpid`; MSYS's `$$` is not a Windows pid) and its start time.

### Untracked results directories and `check_build`

`check_build` (`run/host.rs`) refuses a binary whose stamp ends in `-dirty`. The stamp comes from
the build scripts (`crates/lpk-bench/build.rs`, `crates/lpk-cli/build.rs`), which run
`git status --porcelain -- crates Cargo.toml Cargo.lock bench/tools.toml`: untracked files count,
but only under those paths. `bench/results/` is outside them, so an earlier week's untracked
results directory does not make the build dirty. The simplest correct choice is therefore to
leave every weekly directory where the runner wrote it, in the weekly worktree's `bench/results/`,
and to compare from there; no copy elsewhere is needed. The script never runs `git clean`, and
`git checkout --detach --force origin/main` keeps untracked files (once a session has committed a
week's directory to `main`, the checkout writes the same files over it).

## One run, step by step

Refusals come first. Each is logged as `REFUSED <reason>` in the status log and the script exits 0
without touching anything else:

1. The lock directory is held by a live run: its pid is a running `bash.exe`
   (`tasklist /FI "PID eq <pid>" /FI "IMAGENAME eq bash.exe"`) and it is younger than 9 hours (the
   task stops after 8). Otherwise the lock is stale and taken over, logged with the old pid and
   age.
2. This measurement week already has a GREEN, OVER or BASELINE ledger line.
3. A `cargo`, `rustc`, `link`, `cargo-nextest`, `lpk-bench`, `lpk`, `lpk-worker`, `7z`, `zstd`,
   `xz`, `rar` or `zpaqfranz` process is running (from `tasklist`).
4. The CPU averaged above 10% over two minutes
   (`typeperf "\Processor(_Total)\% Processor Time" -si 5 -sc 24`, the mean of its CSV samples).
5. Free space on the volume below 16,384 MiB. Derivation: the weekly worktree's release build
   (`target/` and `workers/target/`; the main checkout's `target/release` alone held 5,699 MiB on
   2026-11-07), so about 8 GiB; the run's scratch under `bench/tmp`, one class at a time (an
   archive and an extracted tree; the largest class of `small`, `photo-jpeg`, is 313 MiB of a
   1,789 MiB profile), and at worst the failed-repeat evidence it keeps under `bench/tmp/failed`
   for every class (2 x 1,789 MiB); the results directory itself is small. About 12 GiB, rounded
   up to 16 GiB for margin.
6. The weekly worktree folder exists but is not a valid worktree of its own
   (`git -C <wt> rev-parse --is-inside-work-tree --show-toplevel` must name that folder; a plain
   folder would resolve to the main checkout around it). Remedy, in the reason: remove the folder
   and run `git worktree prune`; the next run recreates it.
7. The weekly worktree has tracked changes (`git status --porcelain --untracked-files=no`).
8. `/usr/bin/timeout` is not coreutils' (Windows' `timeout.exe` only waits).

Then housekeeping: `--keep-failed` evidence older than 28 days under the weekly worktree's
`bench/tmp/failed` is deleted, each removal logged.

Then the steps, each exit code checked and logged:

1. `git fetch origin` in the main checkout, with `GIT_TERMINAL_PROMPT=0 GCM_INTERACTIVE=never`
   under `/usr/bin/timeout 600`, so it never waits for a prompt; a timeout or a failure is FAILED
   for `fetch`.
2. In the weekly worktree, `git checkout --detach --force origin/main` (on first use,
   `git worktree add --detach`). Never `git clean`.
3. A release build as the measurement nights do: `cargo build --release --locked -p lpk-cli
   -p lpk-bench`, the `workers/` workspace (`cargo build --release --locked`), and
   `lpk-worker.exe` copied beside `lpk.exe`.
4. A five-minute pause.
5. `lpk-bench run --catalogue bench/tools-weekly.toml --tools lpk,7z --settings
   fast,balanced,store,mx5 --profile small --repeats 3 --threads <logical cores, written out>
   --corpus <main>/bench/corpus/small`. `small` covers every class; three repeats absorb one slow
   repeat (D-24's second cause); `lpk`'s settings are E8-2's set (D-109); 7-Zip `-mx5` is the
   control, whose binary does not change, which separates clock drift from code change.
6. `lpk-bench run --validate <new dir> --corpus <main>/bench/corpus/small`.
7. `lpk-bench run --compare <previous> <new dir> --across-builds` at the default 3%
   (`DEFAULT_MAX_DIFF_PCT`). "Previous" is the directory of the last GREEN, OVER or BASELINE ledger
   line.
8. The ledger line, after a MISSED line for each measurement week skipped since the ledger's last
   line.

A failed step logs it, writes a FAILED ledger line naming the step, removes the lock and exits
nonzero. `--dry-run` runs every refusal check and prints each command it would run, without
fetching, building, measuring, deleting or writing any state file.

## The ledger categories (R5, R9)

The script gives no RED or NOISE verdict; it only sorts the compare's outcome by the compare's own
`error:` lines (`classify_compare`, tested by `scripts/e8-3-weekly-test.sh`):

- `GREEN`: the compare exits 0, every row within the limit.
- `OVER`: the compare ran and its only problems are rows beyond the limit (`time differs by`).
- `BASELINE`: there is no previous weekly directory, or the compare refused only because the
  environment or the run's configuration changed: a different version of a tool not built here
  (the 7z control), thread count, host, corpus (a re-pin) or catalogue file
  (`bench/tools-weekly.toml` edited), requested repeats, `--long-run-s` or the settle value. Every
  reason goes on the line. The next week compares with this directory.
- `FAILED`: a step failed; the step goes on the line. This includes a compare refused for any other
  reason (different classes, an unreadable directory, a different `lpk` version, an unrecognised
  message), which waits for a session; a FAILED reason wins over BASELINE reasons beside it.
- `MISSED`: written by the next run for each measurement week that had no complete run.

### Reading an OVER row

A session reads each OVER row when it commits the week, against D-24's noise: on this machine 23
of 28 totals of a same-build pair agreed within 3%, the rest because of a clock that swings by
about 10% over half-hour windows and single slow repeats of sub-second extractions. The compare's
limit is two-sided, so the direction comes from the A and B columns. Per `lpk` row:

- slower by more than 3% while `7z/mx5` stays within 3%: a possible regression of the code between
  the two builds (`git log <build A>..<build B>`), of `lpk` or of the harness; first rule out a
  single slow repeat in the per-repeat values of the row's result files;
- slower by more than 3% with the control slower too: the clock (or a harness change), i.e. not a
  code regression of `lpk`;
- faster by more than 3%: noted.

A red reading is only reported (R3): in the week's commit note and STATUS's open points. No CI job,
no revert and no gate change; a time threshold or a blocking rule stays the owner's call.

## Committing a finished week (R2)

The task never commits. A session commits each weekly directory to `main` in a `docs(...)` commit,
as the measurement nights were committed: the commit carries `bench/results/<dir>` (from the
weekly worktree) and the verbatim compare log (`e8-3-<dir>-compare.log`), and its note names the
directory, the build and each OVER row's reading; no figure is typed by hand.
`scripts/check-cited-results.sh` checks that every cited directory exists in the tree. A directory
whose name collides with another branch's same-day directory is renamed (STATUS's working notes),
and the ledger line is corrected to the new name so the next week finds it. No `supersedes.toml`
(D-74): weekly directories feed `--compare`, not a pooled report.

## Acceptance (R4, R17)

Two consecutive weekly directories and the one compare between them, cited with each delta's
reading against D-24's 3%. There is no manual baseline: the first scheduled run, Sunday 2026-11-08
03:00 (measurement week 2026-W45), becomes the BASELINE automatically, and that week's Monday run
is refused as already done. The first compare is on Sunday 2026-11-15, week 2026-W46.

## Removal

1. Unregister the task: `scripts/e8-3-task.ps1 -Remove` (or
   `Unregister-ScheduledTask -TaskPath '\LitePack\' -TaskName 'E8-3 weekly speed' -Confirm:$false`);
   it prints the next command without running it. An empty `\LitePack\` folder may stay in Task
   Scheduler and can be deleted there.
2. Remove the weekly worktree with its `target/` and `bench/tmp/`:
   `git -C <main> worktree remove --force <main>/.claude/worktrees/e8-3-weekly`, then
   `git -C <main> worktree prune` (commit any results directory you want to keep first).
3. Delete the state under `<main>/target/acceptance/`: `e8-3-weekly-status.log`,
   `e8-3-weekly-ledger.log`, the `e8-3-weekly.lock` directory if present, and the `e8-3-*.log`
   step logs.
