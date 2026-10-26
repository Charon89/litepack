#!/usr/bin/env bash
# E8-7: the memory-safety statement of a release, as Markdown on stdout (docs/UNSAFE.md).
#   bash scripts/memory-safety-statement.sh [--since <previous-tag>]
# Prints the inventory of the crates allowed `unsafe` (from docs/UNSAFE.md), the runs of the fuzz
# workflow on main since the previous release (every run when no tag is given) and the count of
# open issues labelled `memory-safety`. Needs `gh`, authenticated for this repository; run by
# E7-2's release workflow. Exit 1 when a fuzz run since the tag failed or a finding is open, so the
# release workflow cannot state "0 findings" by mistake (the text is printed either way).
set -euo pipefail
cd "$(dirname "$0")/.."

since_tag=""
case "${1:-}" in
  --since) since_tag="${2:?--since needs a tag}" ;;
  "") ;;
  *) echo "usage: $0 [--since <previous-tag>]" >&2; exit 2 ;;
esac

created=()
since_text="since the first run"
if [ -n "$since_tag" ]; then
  since_date="$(git log -1 --format=%cI "$since_tag")"
  created=(--created ">=$since_date")
  since_text="since \`$since_tag\` ($since_date)"
fi

runs="$(gh run list --workflow fuzz.yml --branch main "${created[@]}" --limit 1000 \
  --json databaseId,conclusion,status,createdAt,event \
  --jq '.[] | "\(.databaseId)\t\(.createdAt)\t\(.event)\t\(.status)\t\(.conclusion)"')"
open="$(gh issue list --label memory-safety --state open --limit 1000 --json number --jq 'length' 2>/dev/null || echo "unknown")"

total=0
failed=0
echo "## Memory safety"
echo
echo "Phase 1 criterion: zero memory-safety findings. Evidence:"
echo
echo "### Crates allowed \`unsafe\` (docs/UNSAFE.md)"
echo
sed -n '/<!-- inventory:begin -->/,/<!-- inventory:end -->/p' docs/UNSAFE.md | grep -v '^<!--' | cut -d'|' -f1-5 | sed 's/$/|/'
echo
echo "Every other crate forbids \`unsafe_code\`, enforced by \`scripts/check-unsafe.sh\` on CI."
echo
echo "### Fuzz runs on main $since_text"
echo
echo "| run | created | trigger | conclusion |"
echo "|---|---|---|---|"
while IFS=$'\t' read -r id at ev st co; do
  [ -n "$id" ] || continue
  total=$((total + 1))
  [ "$st" = "completed" ] || co="$st"
  case "$co" in success|in_progress|queued|cancelled|skipped) ;; *) failed=$((failed + 1)) ;; esac
  echo "| $id | $at | $ev | $co |"
done <<< "$runs"
echo
echo "$total runs, $failed failed."
echo
echo "### Open findings"
echo
echo "Open issues labelled \`memory-safety\`: $open."
if [ "$failed" != 0 ] || [ "$open" != 0 ]; then
  echo "error: $failed failed fuzz runs and $open open findings: the criterion is not met" >&2
  exit 1
fi
