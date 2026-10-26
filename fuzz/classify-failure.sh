#!/usr/bin/env bash
# Label a failing target's log against fuzz/KNOWN.md.
#   bash fuzz/classify-failure.sh TARGET LOG
# Prints "known: <finding>" when a KNOWN.md row of TARGET matches (the log's libFuzzer SUMMARY
# line names the row's kind and the log contains the row's signature), else "new, or a known
# target with a different signature". Exit 0 when known, 1 otherwise.
set -uo pipefail
target="$1"
log="$2"
known="$(dirname "$0")/KNOWN.md"

summary="$(grep -m1 'SUMMARY: libFuzzer:' "$log" 2>/dev/null || true)"
while IFS='|' read -r _ t kind sig finding _; do
  t="$(echo "$t" | sed 's/^ *`//; s/` *$//')"
  kind="$(echo "$kind" | sed 's/^ *`//; s/` *$//')"
  sig="$(echo "$sig" | sed 's/^ *`//; s/` *$//')"
  [ "$t" = "$target" ] || continue
  [ -n "$kind" ] && [ -n "$sig" ] || continue
  if [[ "$summary" == *"$kind"* ]] && grep -qF -- "$sig" "$log"; then
    echo "known: $(echo "$finding" | sed 's/^ *//; s/ *$//')"
    exit 0
  fi
done < <(grep '^| `' "$known")
echo "new, or a known target with a different signature (${summary:-no libFuzzer SUMMARY line})"
exit 1
