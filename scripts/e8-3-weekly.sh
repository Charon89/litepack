#!/usr/bin/env bash
# E8-3: the weekly speed-regression run on the measuring machine (docs/notes/e8-3-design.md).
#
#   bash scripts/e8-3-weekly.sh            refuse or run this week's measurement
#   bash scripts/e8-3-weekly.sh --dry-run  run every refusal check and print each command it would
#                                          run, without fetching, building, measuring, deleting or
#                                          writing any state file
# Test modes (scripts/e8-3-weekly-test.sh; they touch nothing but the files they are given):
#   --classify <compare log> <exit code>   print the ledger verdict of a compare's output
#   --week-key <date>                      print the measurement week of a run started at <date>
#   --ledger-line <ledger> <date> <text>   append <text> as a run started at <date> would
#
# Bash for Git for Windows, started by scripts/e8-3-launch.ps1, which the scheduled task
# `\LitePack\E8-3 weekly speed` runs (scripts/e8-3-task.ps1). Every path derives from
# `git rev-parse --git-common-dir`, i.e. the main checkout, whatever copy of this script runs:
#   <main>/.claude/worktrees/e8-3-weekly   the detached weekly worktree (created on first use)
#   <main>/bench/corpus/small              the corpus (`--corpus`)
#   <main>/target/acceptance/              the status log, the per-step logs and the ledger
# The script never changes a system setting (power plan, antivirus), never commits and never runs
# `git clean`. Every function is defined before the last line and only the last line runs
# anything, so a checkout that rewrites this file cannot change what bash runs mid-run.

# The id of the tool this workspace builds (`discover::WORKSPACE_TOOL` in lpk-bench).
E83_WORKSPACE_TOOL=lpk

# The measurement week of a run started at epoch second $1: the ISO week of the day before, so
# the Monday fallback counts with the Sunday before it.
week_key_at() {
  date -d "@$(($1 - 86400))" +%G-W%V
}

# The Monday (UTC midnight, epoch seconds) of ISO week `YYYY-Www`.
week_monday() {
  local y="${1%-W*}" w="${1#*-W}" jan4 dow
  jan4="$(date -u -d "$y-01-04" +%s)"
  dow="$(date -u -d "$y-01-04" +%u)"
  echo $((jan4 - (dow - 1) * 86400 + (10#$w - 1) * 7 * 86400))
}

# Append one ledger line for week $2 to ledger $1, after a MISSED line for each ISO week skipped
# since the ledger's last week.
ledger_append() {
  local ledger="$1" week="$2" text="$3" last t end
  last="$(awk '$1 == "WEEK" { w = $2 } END { print w }' "$ledger" 2> /dev/null)"
  if [ -n "$last" ] && [ "$last" != "$week" ]; then
    t=$(($(week_monday "$last") + 7 * 86400))
    end="$(week_monday "$week")"
    while [ "$t" -lt "$end" ]; do
      echo "WEEK $(date -u -d "@$t" +%G-W%V) - - MISSED" >> "$ledger"
      t=$((t + 7 * 86400))
    done
  fi
  echo "WEEK $week $text" >> "$ledger"
}

# The ledger verdict of a compare log $1 whose command exited $2 (R5, R9): `GREEN`, `OVER`,
# `BASELINE <reasons>` (a changed environment or configuration) or `FAILED compare: <reason>`.
classify_compare() {
  local log="$1" rc="$2" line p base="" bad="" over=0
  if [ "$rc" = 0 ]; then
    echo GREEN
    return
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%$'\r'}"
    case "$line" in
      "error: "*) p="${line#error: }" ;;
      *) continue ;;
    esac
    case "$p" in
      *" time differs by "*"% (limit "*) over=1 ;;
      "the two directories were measured on different corpora"* | \
        "different hosts: "* | \
        "different thread counts: "* | \
        "different requested repeats: "* | \
        "different --long-run-s: "* | \
        "different --settle-ms-per-1000-files: "* | \
        "the two runs used different catalogue files"*)
        base="${base:+$base; }$p"
        ;;
      "tool \`$E83_WORKSPACE_TOOL\` has different versions"*) bad="${bad:-$p}" ;;
      "tool \`"*"\` has different versions: "*) base="${base:+$base; }$p" ;;
      *) bad="${bad:-$p}" ;;
    esac
  done < "$log"
  if [ -n "$bad" ]; then
    echo "FAILED compare: $bad"
  elif [ -n "$base" ]; then
    echo "BASELINE $base"
  elif [ "$over" = 1 ]; then
    echo OVER
  else
    echo "FAILED compare: exit $rc without a recognised error line"
  fi
}

# Is Windows pid $1 a running bash.exe?
win_bash_alive() {
  MSYS2_ARG_CONV_EXCL='*' tasklist /FI "PID eq $1" /FI "IMAGENAME eq bash.exe" /FO CSV /NH \
    2> /dev/null | grep -q "\"$1\""
}

# Release the lock if this run holds it (globals: the EXIT trap runs after main returns).
E83_HELD=0
E83_LOCK=
release_lock() {
  if [ "${E83_HELD:-0}" = 1 ] && [ -n "$E83_LOCK" ]; then
    rm -rf "$E83_LOCK"
    E83_HELD=0
  fi
}

main() {
  set -u
  local dry=0
  case "${1:-}" in
    "") ;;
    --dry-run) dry=1 ;;
    --classify)
      [ $# = 3 ] || { echo "usage: --classify <compare log> <exit code>" >&2; return 2; }
      classify_compare "$2" "$3"
      return 0
      ;;
    --week-key)
      [ $# = 2 ] || { echo "usage: --week-key <date>" >&2; return 2; }
      week_key_at "$(date -d "$2" +%s)"
      return 0
      ;;
    --ledger-line)
      [ $# = 4 ] || { echo "usage: --ledger-line <ledger> <date> <text>" >&2; return 2; }
      ledger_append "$2" "$(week_key_at "$(date -d "$3" +%s)")" "$4"
      return 0
      ;;
    *)
      echo "usage: bash scripts/e8-3-weekly.sh [--dry-run]" >&2
      return 2
      ;;
  esac

  # Free space the run needs on the corpus's volume, in MiB (docs/notes/e8-3-design.md, "Free
  # space"): the release build of the weekly worktree (target/ and workers/target/, about 8 GiB),
  # the run's scratch for one class at a time plus failed-repeat evidence kept under bench/tmp/failed
  # (at worst an archive and an extracted tree of every class of `small`: 2 x 1.8 GiB), rounded up
  # with a margin.
  local need_mb=16384
  # CPU limit in percent, averaged over 24 samples 5 s apart (two minutes).
  local cpu_limit=10
  # A lock older than this is stale whatever its pid (the task stops after 8 hours).
  local lock_max_s=$((9 * 3600))
  # Failed-repeat evidence older than this many days is deleted from the weekly worktree.
  local evidence_days=28
  local busy_images="cargo rustc link cargo-nextest lpk-bench lpk lpk-worker 7z zstd xz rar zpaqfranz"
  local settings=fast,balanced,store,mx5
  local timeout_bin=/usr/bin/timeout

  # Cargo comes from the user's toolchain when it is not on PATH.
  if ! command -v cargo > /dev/null 2>&1 && [ -n "${USERPROFILE:-}" ]; then
    PATH="$(cygpath -u "$USERPROFILE" 2> /dev/null || echo "$USERPROFILE")/.cargo/bin:$PATH"
    export PATH
  fi

  local here common root
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || return 2
  common="$(git -C "$here" rev-parse --path-format=absolute --git-common-dir)" || {
    echo "error: $here is not inside a git checkout" >&2
    return 2
  }
  root="$(cd "$common/.." && pwd)" || return 2
  local acc="$root/target/acceptance"
  local wt="$root/.claude/worktrees/e8-3-weekly"
  local corpus="$root/bench/corpus/small"
  local status="$acc/e8-3-weekly-status.log"
  local ledger="$acc/e8-3-weekly-ledger.log"
  E83_LOCK="$acc/e8-3-weekly.lock"
  local lock="$E83_LOCK"
  local now week
  now="$(date +%s)"
  week="$(week_key_at "$now")"
  local build_sha=- dir=-

  # --- helpers that use this run's state ------------------------------------------------------
  utc() { date -u +%FT%TZ; }
  say() {
    if [ "$dry" = 1 ]; then
      echo "[dry-run] status: == $*"
    else
      echo "== $* $(utc)" >> "$status"
    fi
  }
  refuse() {
    if [ "$dry" = 1 ]; then
      echo "[dry-run] REFUSED $*"
    else
      echo "REFUSED $* $(utc)" >> "$status"
    fi
    release_lock
    exit 0
  }
  would() { echo "[dry-run] would run: $*"; }
  ledger_line() { ledger_append "$ledger" "$week" "$*"; }
  # Rename the step logs written so far from their stem `e8-3-current-` to `e8-3-<name>-`.
  name_logs() {
    local f
    for f in "$acc"/e8-3-current-*.log; do
      [ -e "$f" ] || continue
      mv -f "$f" "$acc/e8-3-$1-${f##*/e8-3-current-}"
    done
  }
  fail() {
    local step="$1"
    shift
    say "$step FAILED${*:+ ($*)}"
    if [ "$dir" != - ]; then name_logs "$dir"; else name_logs "$week"; fi
    ledger_line "$dir $build_sha FAILED $step${*:+: $*}"
    release_lock
    exit 1
  }

  # --- refusals ---------------------------------------------------------------------------------
  [ "$dry" = 1 ] || mkdir -p "$acc" || return 2
  if [ "$dry" = 1 ]; then
    echo "[dry-run] main checkout: $root"
    echo "[dry-run] weekly worktree: $wt"
    echo "[dry-run] corpus: $corpus"
    echo "[dry-run] state: $acc"
    echo "[dry-run] measurement week (ISO week of the day before): $week"
  fi

  # 1. A lock held by a live run. The lock holds this run's Windows pid (MSYS's $$ is not one)
  #    and its start time; it is stale when that pid is not a running bash.exe or when it is
  #    older than the task's time limit allows.
  local mypid
  mypid="$(cat "/proc/$$/winpid" 2> /dev/null)"
  [ -n "$mypid" ] || refuse "cannot read this bash's Windows pid from /proc/$$/winpid"
  if [ -d "$lock" ]; then
    local other started age
    other="$(sed -n 1p "$lock/pid" 2> /dev/null)"
    started="$(sed -n 1p "$lock/started" 2> /dev/null)"
    case "$started" in '' | *[!0-9]*) started=0 ;; esac
    age=$((now - started))
    if [ -n "$other" ] && [ "$age" -lt "$lock_max_s" ] && win_bash_alive "$other"; then
      refuse "another weekly run holds $lock (Windows pid $other, started $((age / 60)) min ago)"
    fi
    say "stale lock taken over (Windows pid ${other:-unknown}, age $((age / 60)) min)"
    if [ "$dry" = 0 ]; then
      echo "$mypid" > "$lock/pid" && echo "$now" > "$lock/started" || return 2
      E83_HELD=1
    fi
  elif [ "$dry" = 0 ]; then
    mkdir "$lock" 2> /dev/null || refuse "another weekly run took $lock"
    E83_HELD=1
    echo "$mypid" > "$lock/pid"
    echo "$now" > "$lock/started"
  fi
  trap release_lock EXIT

  # 2. This measurement week already has a complete run.
  if [ -f "$ledger" ] && grep -Eq "^WEEK $week [^ ]+ [^ ]+ (GREEN|OVER|BASELINE)( |$)" "$ledger"; then
    refuse "week $week already has a complete run in $ledger"
  fi

  # 3. A build, test or measured tool running.
  local running img
  running="$(MSYS2_ARG_CONV_EXCL='*' tasklist /FO CSV /NH 2> /dev/null |
    awk -F'","' '{ n = tolower($1); sub(/^"/, "", n); sub(/\.exe$/, "", n); print n }' | sort -u)"
  [ -n "$running" ] || refuse "tasklist printed no processes"
  for img in $busy_images; do
    if printf '%s\n' "$running" | grep -qx "$img"; then
      refuse "$img.exe is running"
    fi
  done

  # 4. CPU above the limit over two minutes.
  local cpu
  cpu="$(MSYS2_ARG_CONV_EXCL='*' typeperf '\Processor(_Total)\% Processor Time' -si 5 -sc 24 2> /dev/null |
    awk -F'","' '/^"[0-9]/ { v = $2; gsub(/"/, "", v); gsub(/,/, ".", v); if (v ~ /[0-9]/) { s += v; n++ } }
      END { if (n > 0) printf "%.1f", s / n }')"
  [ -n "$cpu" ] || refuse "typeperf gave no CPU samples"
  if awk -v c="$cpu" -v l="$cpu_limit" 'BEGIN { exit !(c > l) }'; then
    refuse "CPU averaged $cpu% over two minutes (limit $cpu_limit%)"
  fi
  [ "$dry" = 1 ] && echo "[dry-run] CPU averaged $cpu% over two minutes (limit $cpu_limit%)"

  # 5. Free space on the corpus's volume.
  local free_mb
  free_mb="$(df -Pm "$root" | awk 'NR == 2 { print $4 }')"
  [ -n "$free_mb" ] || refuse "df gave no free space for $root"
  if [ "$free_mb" -lt "$need_mb" ]; then
    refuse "only $free_mb MiB free on the volume of $root (the run needs $need_mb MiB)"
  fi
  [ "$dry" = 1 ] && echo "[dry-run] free space $free_mb MiB (needs $need_mb MiB)"

  # 6. The weekly worktree: a valid worktree of its own, without tracked changes. (A folder that
  #    is not a worktree would otherwise resolve to the main checkout around it.)
  if [ -e "$wt" ]; then
    local top want
    top="$(git -C "$wt" rev-parse --is-inside-work-tree --show-toplevel 2> /dev/null | sed -n 2p)"
    want="$(cd "$wt" && pwd -W 2> /dev/null || pwd)"
    if [ -z "$top" ] || [ "$(echo "$top" | tr 'A-Z\\' 'a-z/')" != "$(echo "$want" | tr 'A-Z\\' 'a-z/')" ]; then
      refuse "$wt exists but is not a valid git worktree: remove the folder and run \`git -C $root worktree prune\`; the next run recreates it"
    fi
    if [ -n "$(git -C "$wt" status --porcelain --untracked-files=no 2>&1)" ]; then
      refuse "the weekly worktree $wt has tracked changes"
    fi
  fi
  [ -d "$corpus" ] || refuse "no corpus at $corpus"

  # 7. The fetch's timeout must be coreutils' (Windows has a timeout.exe that only waits).
  "$timeout_bin" --version 2> /dev/null | grep -q coreutils ||
    refuse "$timeout_bin is not coreutils' timeout"

  # --- housekeeping: old failed-repeat evidence (R15) -------------------------------------------
  local old
  if [ -d "$wt/bench/tmp/failed" ]; then
    while IFS= read -r old; do
      [ -n "$old" ] || continue
      if [ "$dry" = 1 ]; then
        would rm -rf "$old"
      else
        rm -rf "$old" && say "removed failed-repeat evidence older than $evidence_days days: $old"
      fi
    done < <(find "$wt/bench/tmp/failed" -mindepth 1 -maxdepth 1 -mtime +"$evidence_days" 2> /dev/null)
  fi

  # --- the steps --------------------------------------------------------------------------------
  local threads="${NUMBER_OF_PROCESSORS:-$(nproc)}"
  local bench=./target/release/lpk-bench.exe
  local run_args=(run --catalogue bench/tools-weekly.toml --tools lpk,7z --settings "$settings"
    --profile small --repeats 3 --threads "$threads" --corpus "$corpus")

  # The directory of the last complete week: what this week compares with.
  local prev
  prev="$(awk '$1 == "WEEK" && ($5 == "GREEN" || $5 == "OVER" || $5 == "BASELINE") { d = $3 }
    END { print d }' "$ledger" 2> /dev/null)"

  if [ "$dry" = 1 ]; then
    would GIT_TERMINAL_PROMPT=0 GCM_INTERACTIVE=never "$timeout_bin" 600 git -C "$root" fetch origin
    if [ -e "$wt" ]; then
      would git -C "$wt" checkout --detach --force origin/main
    else
      would git -C "$root" worktree add --detach "$wt" origin/main
    fi
    would "(cd $wt && cargo build --release --locked -p lpk-cli -p lpk-bench)"
    would "(cd $wt/workers && cargo build --release --locked)"
    would cp "$wt/workers/target/release/lpk-worker.exe" "$wt/target/release/"
    would sleep 300
    would "(cd $wt && $bench ${run_args[*]})"
    would "(cd $wt && $bench run --validate bench/results/<new dir> --corpus $corpus)"
    if [ -n "$prev" ]; then
      would "(cd $wt && $bench run --compare bench/results/$prev bench/results/<new dir> --across-builds)"
    else
      echo "[dry-run] no previous complete week in the ledger: the run would be the BASELINE"
    fi
    echo "[dry-run] would append to $ledger: WEEK $week <new dir> <build sha7> GREEN|OVER|BASELINE|FAILED"
    return 0
  fi

  say "start commit $(git -C "$root" rev-parse --short=12 HEAD 2> /dev/null || echo unknown) Windows pid $mypid week $week"
  rm -f "$acc"/e8-3-current-*.log

  # Never wait for a credential prompt; give up after ten minutes.
  GIT_TERMINAL_PROMPT=0 GCM_INTERACTIVE=never "$timeout_bin" 600 git -C "$root" fetch origin \
    > "$acc/e8-3-current-fetch.log" 2>&1 < /dev/null
  local rc=$?
  say "fetch exit $rc"
  if [ "$rc" = 124 ]; then fail fetch "timed out after 600 s"; fi
  [ "$rc" = 0 ] || fail fetch "exit $rc"

  if [ -e "$wt" ]; then
    git -C "$wt" checkout --detach --force origin/main > "$acc/e8-3-current-checkout.log" 2>&1
  else
    mkdir -p "$(dirname "$wt")" && git -C "$root" worktree add --detach "$wt" origin/main \
      > "$acc/e8-3-current-checkout.log" 2>&1
  fi
  rc=$?
  say "checkout exit $rc"
  [ "$rc" = 0 ] || fail checkout
  build_sha="$(git -C "$wt" rev-parse --short=7 HEAD)" || fail checkout "no HEAD"
  cd "$wt" || fail checkout "cannot enter $wt"

  {
    cargo build --release --locked -p lpk-cli -p lpk-bench &&
      (cd workers && cargo build --release --locked) &&
      cp workers/target/release/lpk-worker.exe target/release/
  } > "$acc/e8-3-current-build.log" 2>&1
  rc=$?
  say "build exit $rc (build $build_sha)"
  [ "$rc" = 0 ] || fail build
  [ -x "$bench" ] || fail build "no $bench"

  say "pause 300 s"
  sleep 300

  local before after
  before="$(ls -1 bench/results 2> /dev/null | sort)"
  "$bench" "${run_args[@]}" > "$acc/e8-3-current-run.log" 2>&1
  rc=$?
  after="$(ls -1 bench/results 2> /dev/null | sort)"
  local new
  new="$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | head -1)"
  [ -n "$new" ] && dir="$new"
  say "run exit $rc dir $dir"
  [ "$rc" = 0 ] || fail run "exit $rc"
  [ "$dir" != - ] || fail run "no new results directory"

  "$bench" run --validate "bench/results/$dir" --corpus "$corpus" > "$acc/e8-3-current-validate.log" 2>&1
  rc=$?
  say "validate exit $rc"
  [ "$rc" = 0 ] || fail validate "exit $rc"

  if [ -z "$prev" ]; then
    name_logs "$dir"
    say "no previous complete week: BASELINE"
    ledger_line "$dir $build_sha BASELINE no previous weekly directory"
    say "DONE"
    return 0
  fi
  if [ ! -f "bench/results/$prev/run.json" ]; then
    name_logs "$dir"
    say "previous weekly directory $prev not found: BASELINE"
    ledger_line "$dir $build_sha BASELINE previous weekly directory $prev not found"
    say "DONE"
    return 0
  fi

  "$bench" run --compare "bench/results/$prev" "bench/results/$dir" --across-builds \
    > "$acc/e8-3-current-compare.log" 2>&1
  rc=$?
  say "compare exit $rc ($prev against $dir)"
  name_logs "$dir"
  local verdict
  verdict="$(classify_compare "$acc/e8-3-$dir-compare.log" "$rc")"
  case "$verdict" in
    GREEN | OVER) ledger_line "$dir $build_sha $verdict against $prev" ;;
    BASELINE*) ledger_line "$dir $build_sha $verdict" ;;
    *) fail compare "${verdict#FAILED compare: }" ;;
  esac
  say "DONE ($verdict)"
  return 0
}

main "$@"; exit $?
