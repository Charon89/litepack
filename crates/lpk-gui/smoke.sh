#!/usr/bin/env bash
# E5-2's check of the built GUI (CI job `gui` on windows-2025; also by hand):
#   bash crates/lpk-gui/smoke.sh <lpk.exe> <lpk-gui.exe> <scratch dir>
# Creates an archive of three files with lpk, starts lpk-gui.exe with the archive on its command
# line and LPK_GUI_SMOKE_REPORT set, and waits for it to exit. The page reports through the
# `listing_shown` command once the rows are in the window's DOM (the count of [role=row] elements
# it rendered, the first name, and every error, unhandled rejection or CSP violation it saw); the
# host writes that report and exits. The check fails unless the window rendered all four rows (the folder and its three files),
# the first is the archive's first entry and nothing went wrong on the page.
set -euo pipefail
lpk="$1"
gui="$2"
work="$3"
rm -rf "$work"
mkdir -p "$work/in/sub" "$work/local"
for i in 1 2 3; do
  echo "file $i" > "$work/in/sub/f$i.txt"
done
"$lpk" a "$work/smoke.lpk" "$work/in"
# The GUI's state (journal, WebView2's data folder) goes to the scratch dir, not the user's.
export LOCALAPPDATA="$work/local"
export LPK_GUI_SMOKE_REPORT="$work/report.json"
timeout 180 "$gui" "$work/smoke.lpk"
report="$(cat "$work/report.json")"
echo "report: $report"
grep -q '"archive":"smoke.lpk"' <<<"$report"
grep -q '"total":4' <<<"$report"
grep -q '"rendered":4' <<<"$report"
grep -q '"first":"sub"' <<<"$report"
grep -q '"problems":\[\]' <<<"$report"
echo "the listing of smoke.lpk reached the window: 4 rows (the folder and its three files), no page error"
