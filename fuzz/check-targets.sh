#!/usr/bin/env bash
# Check that fuzz/fuzz_targets/*.rs, fuzz/Cargo.toml's [[bin]] entries (names and paths) and
# fuzz/run.sh's target list name the same targets; print them one per line, sorted.
#   bash fuzz/check-targets.sh            # exit 1 (with a diff) when they differ
# .github/workflows/fuzz.yml builds its target matrix from this output.
set -euo pipefail
cd "$(dirname "$0")"

files="$(find fuzz_targets -maxdepth 1 -name '*.rs' | sed 's|^fuzz_targets/||; s|\.rs$||' | sort)"
bins="$(awk '/^\[\[bin\]\]/{b=1; next} /^\[/{b=0} b && /^name *=/{gsub(/[" ]/,""); sub(/^name=/,""); print}' Cargo.toml | sort)"
paths="$(awk '/^\[\[bin\]\]/{b=1; next} /^\[/{b=0} b && /^path *=/{gsub(/[" ]/,""); sub(/^path=fuzz_targets\//,""); sub(/\.rs$/,""); print}' Cargo.toml | sort)"
script="$(bash run.sh --list | sort)"

ok=1
check() {
  if [ "$2" != "$files" ]; then
    echo "error: fuzz/$1 differs from fuzz/fuzz_targets/*.rs (< files, > $1):" >&2
    diff <(echo "$files") <(echo "$2") >&2 || true
    ok=0
  fi
}
check "Cargo.toml [[bin]] names" "$bins"
check "Cargo.toml [[bin]] paths" "$paths"
check "run.sh" "$script"
[ "$ok" = 1 ] || exit 1
echo "$files"
