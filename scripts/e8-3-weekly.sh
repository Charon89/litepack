#!/usr/bin/env bash
# E8-3: the weekly speed-regression run on the measuring machine (docs/notes/e8-3-design.md).
#
#   bash scripts/e8-3-weekly.sh            refuse or run this week's measurement
#   bash scripts/e8-3-weekly.sh --dry-run  run every refusal check and print each command it would
#                                          run, without fetching, building, measuring or writing
#                                          any state file
#
# Bash for Git for Windows, started by the scheduled task `\LitePack\E8-3 weekly speed`
# (scripts/e8-3-task.ps1). Every path derives from `git rev-parse --git-common-dir`, i.e. the main
# checkout, whatever copy of this script runs:
#   <main>/.claude/worktrees/e8-3-weekly   the detached weekly worktree (created on first use)
#   <main>/bench/corpus/small              the corpus (`--corpus`)
#   <main>/target/acceptance/              the status log, the per-step logs and the ledger
# The script never changes a system setting (power plan, antivirus), never commits and never runs
# `git clean`. The whole body is a function called on the last line, so a checkout that rewrites
# this file cannot change what bash runs mid-run.

main() {
  set -u
  local dry=0
  case "${1:-}" in
    "") ;;
    --dry-run) dry=1 ;;
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
  local busy_images="cargo rustc link cargo-nextest lpk-bench lpk lpk-worker 7z zstd xz rar zpaqfranz"
  local settings=fast,balanced,store,mx5

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
  local week
  week="$(date +%G-W%V)"
  E83_HELD=0
  local build_sha=- dir=-

  # --- helpers --------------------------------------------------------------------------------
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
  release_lock() {
    if [ "${E83_HELD:-0}" = 1 ]; then
      rm -rf "$E83_LOCK"
      E83_HELD=0
    fi
  }
  # The Monday (UTC midnight, epoch seconds) of ISO week `YYYY-Www`.
  week_monday() {
    local y="${1%-W*}" w="${1#*-W}" jan4 dow
    jan4="$(date -u -d "$y-01-04" +%s)"
    dow="$(date -u -d "$y-01-04" +%u)"
    echo $((jan4 - (dow - 1) * 86400 + (10#$w - 1) * 7 * 86400))
  }
  # One ledger line, after a MISSED line for each ISO week skipped since the ledger's last week.
  ledger_line() {
    local last t end
    last="$(awk '$1 == "WEEK" { w = $2 } END { print w }' "$ledger" 2> /dev/null)"
    if [ -n "$last" ] && [ "$last" != "$week" ]; then
      t=$(($(week_monday "$last") + 7 * 86400))
      end="$(week_monday "$week")"
      while [ "$t" -lt "$end" ]; do
        echo "WEEK $(date -u -d "@$t" +%G-W%V) - - MISSED" >> "$ledger"
        t=$((t + 7 * 86400))
      done
    fi
    echo "WEEK $week $*" >> "$ledger"
  }
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
  # Is a Windows process with this pid running?
  win_pid_alive() {
    MSYS2_ARG_CONV_EXCL='*' tasklist /FI "PID eq $1" /FO CSV /NH 2> /dev/null | grep -q "\"$1\""
  }

  # --- refusals ---------------------------------------------------------------------------------
  [ "$dry" = 1 ] || mkdir -p "$acc" || return 2
  if [ "$dry" = 1 ]; then
    echo "[dry-run] main checkout: $root"
    echo "[dry-run] weekly worktree: $wt"
    echo "[dry-run] corpus: $corpus"
    echo "[dry-run] state: $acc"
    echo "[dry-run] ISO week: $week"
  fi

  # 1. A lock held by a live run. A lock whose run is gone is taken over.
  local mypid
  mypid="$(cat /proc/$$/winpid 2> /dev/null || echo $$)"
  if [ -d "$lock" ]; then
    local other
    other="$(cat "$lock/pid" 2> /dev/null || true)"
    if [ -n "$other" ] && win_pid_alive "$other"; then
      refuse "another weekly run holds $lock (pid $other)"
    fi
    say "stale lock of pid ${other:-unknown} (not running): taken over"
    if [ "$dry" = 0 ]; then
      echo "$mypid" > "$lock/pid" || return 2
      E83_HELD=1
    fi
  elif [ "$dry" = 0 ]; then
    mkdir "$lock" 2> /dev/null || refuse "another weekly run took $lock"
    E83_HELD=1
    echo "$mypid" > "$lock/pid"
  fi
  trap release_lock EXIT

  # 2. This ISO week already has a complete run.
  if [ -f "$ledger" ] && grep -Eq "^WEEK $week [^ ]+ [^ ]+ (GREEN|OVER|BASELINE)( |$)" "$ledger"; then
    refuse "ISO week $week already has a complete run in $ledger"
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

  # 6. Tracked changes in the weekly worktree.
  if [ -e "$wt" ]; then
    if [ -n "$(git -C "$wt" status --porcelain --untracked-files=no 2>&1)" ]; then
      refuse "the weekly worktree $wt has tracked changes"
    fi
  fi
  [ -d "$corpus" ] || refuse "no corpus at $corpus"

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
    would git -C "$root" fetch origin
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

  say "start commit $(git -C "$root" rev-parse --short=12 HEAD 2> /dev/null || echo unknown) pid $mypid week $week"
  rm -f "$acc"/e8-3-current-*.log

  git -C "$root" fetch origin > "$acc/e8-3-current-fetch.log" 2>&1
  local rc=$?
  say "fetch exit $rc"
  [ "$rc" = 0 ] || fail fetch

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
  local clog="$acc/e8-3-$dir-compare.log"
  if [ "$rc" = 0 ]; then
    ledger_line "$dir $build_sha GREEN against $prev"
  else
    # The verdict comes from the compare's own `error:` lines.
    local problems base other
    problems="$(grep '^error: ' "$clog" | sed 's/^error: //')"
    base="$(printf '%s\n' "$problems" | grep -E 'different corpora|different hosts|different catalogue files' | head -1)"
    other="$(printf '%s\n' "$problems" | grep -v -E 'time differs by|different corpora|different hosts|different catalogue files' | grep -v '^$' | head -1)"
    if [ -n "$base" ]; then
      ledger_line "$dir $build_sha BASELINE $base"
    elif [ -n "$other" ]; then
      fail compare "$other"
    elif printf '%s\n' "$problems" | grep -q 'time differs by'; then
      ledger_line "$dir $build_sha OVER against $prev"
    else
      fail compare "exit $rc"
    fi
  fi
  say "DONE"
  return 0
}

main "$@"; exit $?
