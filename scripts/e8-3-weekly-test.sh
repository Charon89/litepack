#!/usr/bin/env bash
# Tests of scripts/e8-3-weekly.sh's pure parts (E8-3): the compare verdicts on canned compare
# output (R5, R9), the measurement week (R10) and the MISSED lines across a year end.
#   bash scripts/e8-3-weekly-test.sh
# Writes only to a temporary directory it removes; never fetches, builds or measures.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
weekly="$here/e8-3-weekly.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fails=0
n=0

check() {
  local what="$1" want="$2" got="$3"
  n=$((n + 1))
  if [ "$got" = "$want" ]; then
    echo "ok   $what"
  else
    echo "FAIL $what"
    echo "     want: $want"
    echo "     got:  $got"
    fails=$((fails + 1))
  fi
}

# classify <name> <exit code> <log lines...>
classify() {
  local name="$1" rc="$2"
  shift 2
  printf '%s\n' "$@" > "$tmp/$name.log"
  bash "$weekly" --classify "$tmp/$name.log" "$rc"
}

table='tool/setting           cls compress A s compress B s   diff %  extract A s  extract B s   diff %'

check "exit 0 is GREEN" "GREEN" "$(classify green 0 "$table" \
  "note: tool \`lpk\` is a different build (--across-builds): 0.1.0+aaaa against 0.1.0+bbbb" \
  "4 combination(s) compared; every difference is within 3%")"

check "only time differences are OVER" "OVER" "$(classify over 1 "$table" \
  "error: lpk/fast: compress time differs by 4.10% (limit 3%)" \
  "error: lpk/balanced: extract time differs by 3.20% (limit 3%)" \
  "2 problem(s)")"

check "a new 7z version starts a BASELINE" \
  "BASELINE tool \`7z\` has different versions: 26.03 against 26.04" \
  "$(classify seven 1 "error: tool \`7z\` has different versions: 26.03 against 26.04" "1 problem(s)")"

check "corpus and threads start a BASELINE with both reasons, even with time differences" \
  "BASELINE the two directories were measured on different corpora (profile, manifest hash or private flag differ); different thread counts: {24} against {16}" \
  "$(classify corpus 1 \
    "error: the two directories were measured on different corpora (profile, manifest hash or private flag differ)" \
    "error: different thread counts: {24} against {16}" \
    "error: lpk/fast: compress time differs by 9.00% (limit 3%)" \
    "3 problem(s)")"

for p in 'different hosts: "megatron" against "other"' \
  'different requested repeats: 3 against 5' \
  'different --long-run-s: 120 against 60' \
  'different --settle-ms-per-1000-files: 500 against 0' \
  'the two runs used different catalogue files (bench/tools.toml)'; do
  check "BASELINE: ${p%%:*}" "BASELINE $p" "$(classify cfg 1 "error: $p" "1 problem(s)")"
done

check "different classes stay FAILED, even beside a BASELINE reason" \
  "FAILED compare: lpk/fast: the two directories measured different classes" \
  "$(classify classes 1 \
    "error: different hosts: \"a\" against \"b\"" \
    "error: lpk/fast: the two directories measured different classes" \
    "2 problem(s)")"

check "a different lpk version (the flag missing) is FAILED" \
  "FAILED compare: tool \`lpk\` has different versions: 0.1.0+a against 0.1.0+b" \
  "$(classify lpk 1 "error: tool \`lpk\` has different versions: 0.1.0+a against 0.1.0+b")"

check "an unreadable directory is FAILED" \
  "FAILED compare: reading bench/results/x/run.json (a directory without run.json is an aborted run)" \
  "$(classify unreadable 1 "error: reading bench/results/x/run.json (a directory without run.json is an aborted run)")"

check "a nonzero exit without an error line is FAILED" \
  "FAILED compare: exit 2 without a recognised error line" "$(classify none 2 "something else")"

check "CRLF line ends are read" "OVER" \
  "$(classify crlf 1 $'error: 7z/mx5: compress time differs by 5.00% (limit 3%)\r')"

# R10: the measurement week is the ISO week of the day before the run.
check "Sunday 2026-11-08 03:00 is 2026-W45" "2026-W45" "$(bash "$weekly" --week-key '2026-11-08 03:00')"
check "Monday 2026-11-09 03:00 is 2026-W45" "2026-W45" "$(bash "$weekly" --week-key '2026-11-09 03:00')"
check "Sunday 2026-11-15 03:00 is 2026-W46" "2026-W46" "$(bash "$weekly" --week-key '2026-11-15 03:00')"
check "Sunday 2027-01-03 03:00 is 2026-W53" "2026-W53" "$(bash "$weekly" --week-key '2027-01-03 03:00')"
check "Monday 2027-01-04 03:00 is 2026-W53" "2026-W53" "$(bash "$weekly" --week-key '2027-01-04 03:00')"
check "Sunday 2027-01-10 03:00 is 2027-W01" "2027-W01" "$(bash "$weekly" --week-key '2027-01-10 03:00')"

# MISSED lines across a year end: the last line is 2026-W51 (Sunday 2026-12-20); the next run is
# Sunday 2027-01-10 (2027-W01), so 2026-W52 and 2026-W53 were missed.
ledger="$tmp/ledger.log"
bash "$weekly" --ledger-line "$ledger" '2026-12-20 03:00' 'd1 aaaaaaa BASELINE no previous weekly directory'
bash "$weekly" --ledger-line "$ledger" '2027-01-10 03:00' 'd2 bbbbbbb GREEN against d1'
bash "$weekly" --ledger-line "$ledger" '2027-01-11 03:00' 'd3 bbbbbbb FAILED run: exit 1'
check "MISSED lines across a year end" \
  "WEEK 2026-W51 d1 aaaaaaa BASELINE no previous weekly directory
WEEK 2026-W52 - - MISSED
WEEK 2026-W53 - - MISSED
WEEK 2027-W01 d2 bbbbbbb GREEN against d1
WEEK 2027-W01 d3 bbbbbbb FAILED run: exit 1" "$(cat "$ledger")"

ledger="$tmp/ledger2.log"
bash "$weekly" --ledger-line "$ledger" '2026-11-08 03:00' 'd1 aaaaaaa BASELINE no previous weekly directory'
bash "$weekly" --ledger-line "$ledger" '2026-11-16 03:00' 'd2 bbbbbbb OVER against d1'
check "a Monday fallback the next week misses nothing" \
  "WEEK 2026-W45 d1 aaaaaaa BASELINE no previous weekly directory
WEEK 2026-W46 d2 bbbbbbb OVER against d1" "$(cat "$ledger")"

echo "$((n - fails)) of $n passed"
[ "$fails" = 0 ]
