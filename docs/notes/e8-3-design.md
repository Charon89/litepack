# E8-3: the weekly speed-regression run on the measuring machine (design as built)

PLAN task E8-3: time is gated only on the measuring machine (Megatron), never on shared CI, with
each delta read against D-24's noise; a gate change stays the owner's. This note is the design as
built from the planning draft of 2026-11-04, with the owner's answers and the controller's rulings
of 2026-11-07. The weekly runs themselves, and the acceptance evidence, come after the merge.

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
- `scripts/e8-3-weekly.sh`: the run (below).
- `scripts/e8-3-task.ps1 -Register | -Remove [-Day Sunday] [-At 03:00] [-FallbackDay Monday]`: the
  scheduled task.

## The schedule

The task `\LitePack\E8-3 weekly speed` runs for the owner's account, only while the owner is
logged on (an interactive logon, no stored password, RunLevel Limited; a locked screen counts as
logged on). Two weekly triggers: **Sunday 03:00 local, and the fallback Monday 03:00**. It may wake
the PC from sleep (the owner's answer of 2026-11-07), starts only after 15 idle minutes and waits
up to 2 hours for them, does not stop when the machine stops being idle, ignores a second start
while one runs (`IgnoreNew`), stops after 8 hours, and is hidden. The Task Scheduler service starts
it, outside the Claude app's process tree (STATUS's working note: a `Start-Process` run died with
the app). The action is Git for Windows' `bash.exe`, found from git's own location (never the WSL
`bash.exe` on PATH), on the main checkout's `scripts/e8-3-weekly.sh`.

**At most one complete run per ISO week**: the fallback only runs when the week has no complete run
yet (a GREEN, OVER or BASELINE ledger line; a FAILED week may run again on Monday). There is no
automatic re-measure (R6) and no catch-up at logon, which would measure a busy machine. The first,
baseline run is started by hand through the scheduler after the merge
(`Start-ScheduledTask -TaskPath '\LitePack\' -TaskName 'E8-3 weekly speed'`).

## Where things live

Every path derives from `git rev-parse --git-common-dir`, i.e. the main checkout, whatever copy of
the script runs; nothing machine-specific is in git.

- The weekly worktree: `<main>/.claude/worktrees/e8-3-weekly`, a detached worktree created on first
  use (ignored by `.gitignore`'s `/.claude/worktrees/`; not under `target/`, which `cargo clean`
  deletes). A session's `cargo build` elsewhere never replaces its `target/release/lpk.exe`
  mid-run, and the owner's checkout is never switched.
- The corpus: `<main>/bench/corpus/small`, passed with `--corpus` (the runner's `--corpus` names the
  directory holding `manifest.json`, so the profile's directory, not `bench/corpus` itself).
- State, under `<main>/target/acceptance/`:
  - `e8-3-weekly-status.log`: the measurement nights' `== <step> exit <code> <UTC>` lines, a start
    line with the main checkout's commit, the Windows pid and the ISO week, and `REFUSED <reason>`
    lines.
  - The per-step logs `e8-3-<dir>-fetch.log`, `-checkout.log`, `-build.log`, `-run.log`,
    `-validate.log` and `-compare.log`. They are written as `e8-3-current-<step>.log` and renamed
    once the run has named its results directory; a run that fails before that names them
    `e8-3-<YYYY-Www>-<step>.log`.
  - `e8-3-weekly-ledger.log`, one line per ISO week:
    `WEEK <YYYY-Www> <dir> <build sha7> GREEN|OVER|BASELINE|FAILED|MISSED [reason]`
    (`-` for a directory or build that does not exist).
  - `e8-3-weekly.lock`: the lock directory, holding the running script's Windows pid.

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

1. The lock directory is held by a live run (its recorded pid is a running Windows process). A lock
   whose pid is no longer running is logged as stale and taken over.
2. This ISO week already has a GREEN, OVER or BASELINE ledger line.
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
6. The weekly worktree has tracked changes (`git status --porcelain --untracked-files=no`).

Then the steps, each exit code checked and logged:

1. `git fetch origin` in the main checkout.
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
8. The ledger line, after a MISSED line for each ISO week skipped since the ledger's last line.

A failed step logs it, writes a FAILED ledger line naming the step, removes the lock and exits
nonzero. `--dry-run` runs every refusal check and prints each command it would run, without
fetching, building, measuring or writing any state file.

## The ledger categories (R5)

The script gives no RED or NOISE verdict; it only sorts the compare's outcome:

- `GREEN`: the compare exits 0, every row within the limit.
- `OVER`: the compare ran and some row is beyond the limit (its only `error:` lines are
  `time differs by`).
- `BASELINE`: there is no previous weekly directory, or the compare refused for a reason that
  starts a new baseline: a different corpus (a re-pin), catalogue file (`bench/tools-weekly.toml`
  edited) or host. The reason goes on the line. The next week compares with this directory.
- `FAILED`: a step failed; the step goes on the line. This includes a compare that refused for any
  other reason (for example a new 7-Zip version: the control changed, which a session resolves by
  hand).
- `MISSED`: written by the next run for each ISO week that had no complete run.

### Reading an OVER row

A session reads each OVER row when it commits the week, against D-24's noise: on this machine 23
of 28 totals of a same-build pair agreed within 3%, the rest because of a clock that swings by
about 10% over half-hour windows and single slow repeats of sub-second extractions. The compare's
limit is two-sided, so the direction comes from the A and B columns. Per `lpk` row:

- slower by more than 3% while `7z/mx5` stays within 3%: a possible regression of the code between
  the two builds (`git log <build A>..<build B>`); first rule out a single slow repeat in the
  per-repeat values of the row's result files;
- slower by more than 3% with the control slower too: the clock, i.e. noise;
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

Acceptance (R4): two consecutive weekly directories and the one compare between them, cited with
each delta's reading against D-24's 3%.

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
