#!/usr/bin/env bash
# Fails when a chapter listed in book/src/SUMMARY.md lacks the licence-footer include (E9-1).
set -euo pipefail
src="$(dirname "$0")/../book/src"
fail=0
count=0
while IFS= read -r path; do
  count=$((count + 1))
  if [ ! -f "$src/$path" ]; then echo "missing chapter file: $path"; fail=1; continue; fi
  last="$(grep -v '^[[:space:]]*$' "$src/$path" | tail -n 1 | tr -d '\r')"
  if ! printf '%s\n' "$last" | grep -Eq '^\{\{#include (\.\./)*footer\.md\}\}$'; then
    echo "last non-blank line is not the footer include: $path"; fail=1
  fi
done < <(sed -nE 's/.*\]\(([^)#]+\.md)\).*/\1/p' "$src/SUMMARY.md")
[ "$count" -gt 0 ] || { echo "no chapters found in SUMMARY.md"; exit 1; }
[ "$fail" -eq 0 ] && echo "footer check: $count chapters ok"
exit "$fail"
