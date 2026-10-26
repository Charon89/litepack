#!/usr/bin/env bash
# E8 honesty rule: every results directory the documents cite exists in the tree.
#   bash scripts/check-cited-results.sh              check this repository
#   bash scripts/check-cited-results.sh --self-test  check that a missing directory fails
#   bash scripts/check-cited-results.sh --root DIR   check another tree
# Looks in docs/, bench/reports/, README.md and CLAUDE.md for a `bench/results/<date>-<host>[-n]`
# path (with or without a file after it) and for a bare `<date>-<host>[-n]` name in backticks, and
# fails when bench/results/<that name> is not a directory. Placeholders (`<date>-<host>`) are not
# names and are ignored. (On 2026-10-25 a cited directory was found only on a task branch.)
set -euo pipefail

# Prints `file: name` for every cited name under root $1 that has no directory; returns 1 if any.
check_tree() {
  local top="$1" fail=0 n=0 srcs=() s
  for s in docs bench/reports README.md CLAUDE.md; do
    [ -e "$top/$s" ] && srcs+=("$s")
  done
  [ "${#srcs[@]}" -gt 0 ] || { echo "error: nothing to check under $top" >&2; return 1; }
  local hits
  # Host names may hold `_` and `.`; the path may use backslashes; a bare name in backticks may
  # be followed by `/` and a file.
  local host='[A-Za-z][A-Za-z0-9_.]*(-[A-Za-z0-9_.]+)*'
  hits="$(cd "$top" && grep -rIoE \
    "bench[/\\\\]results[/\\\\][0-9]{4}-[0-9]{2}-[0-9]{2}-$host|\`[0-9]{4}-[0-9]{2}-[0-9]{2}-$host(/[^\`]*)?\`" \
    "${srcs[@]}" || true)"
  local line file name
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    file="${line%%:*}"
    name="${line#*:}"
    name="${name//\`/}"
    name="${name#bench[/\\]results[/\\]}"
    name="${name%%/*}"
    while [ "${name%.}" != "$name" ]; do name="${name%.}"; done # a sentence's full stop
    n=$((n + 1))
    if [ ! -d "$top/bench/results/$name" ]; then
      echo "error: $file cites bench/results/$name, which is not in the tree"
      fail=1
    fi
  done <<< "$hits"
  [ "$fail" = 0 ] && echo "cited results: $n citations, every directory present"
  return "$fail"
}

self_test() {
  st_tmp="$(mktemp -d)"
  trap 'rm -rf "$st_tmp"' EXIT
  local base="$st_tmp/base" fail=0
  mkdir -p "$base/docs" "$base/bench/reports" "$base/bench/results/2026-10-22-megatron-4" "$base/bench/results/2026-10-03-megatron"
  cat > "$base/docs/a.md" <<'EOF'
Measured in `bench/results/2026-10-22-megatron-4` (see bench/results/2026-10-03-megatron/probe.json).
Placeholders are not names: `bench/results/<date>-<host>/`, `<date>-<host>[-n]`.
Also `2026-10-03-megatron/probe.json`, bench\results\2026-10-22-megatron-4\x.json and `2026-10-22-megatron-4`.
EOF
  printf 'The run `2026-10-03-megatron` again.\n' > "$base/bench/reports/r.md"
  cat > "$st_tmp/backslash.txt" <<'EOF'
On Windows: bench\results\2026-10-24-megatron\x.json
EOF
  printf '# readme\n' > "$base/README.md"
  if ! check_tree "$base" > "$st_tmp/clean.out" 2>&1; then
    echo "self-test: the clean fixture was refused:" >&2
    cat "$st_tmp/clean.out" >&2
    fail=1
  fi
  local cases=(
    "missing-path|printf 'See bench/results/2026-10-25-megatron-2/x.json.\n' >> docs/a.md"
    "missing-bare-name|printf 'Run \`2026-10-24-otherhost\`.\n' >> bench/reports/r.md"
    "missing-in-readme|printf 'bench/results/2026-01-01-ci\n' >> README.md"
    "missing-bare-name-with-file|printf 'See \`2026-10-24-megatron-9/run.json\`.\n' >> docs/a.md"
    "missing-host-with-underscore|printf 'In bench/results/2026-10-24-build_host.\n' >> docs/a.md"
    "missing-host-with-dot|printf 'In \`2026-10-24-ci.example-2\`.\n' >> docs/a.md"
    "missing-backslash-path|cat \"\$st_tmp/backslash.txt\" >> docs/a.md"
  )
  local c name cmd d
  for c in "${cases[@]}"; do
    name="${c%%|*}"
    cmd="${c#*|}"
    d="$st_tmp/$name"
    cp -R "$base" "$d"
    (cd "$d" && eval "$cmd")
    if check_tree "$d" > "$st_tmp/$name.out" 2>&1; then
      echo "self-test: $name was not found" >&2
      fail=1
    else
      echo "self-test: $name refused: $(head -n 1 "$st_tmp/$name.out")"
    fi
  done
  [ "$fail" = 0 ] && echo "self-test: a missing directory fails (${#cases[@]} cases), the clean fixture passes"
  return "$fail"
}

case "${1:-}" in
  --self-test) self_test ;;
  --root) check_tree "$(cd "${2:?--root needs a directory}" && pwd)" ;;
  "") check_tree "$(cd "$(dirname "$0")/.." && pwd)" ;;
  *) echo "usage: $0 [--self-test | --root DIR]" >&2; exit 2 ;;
esac
