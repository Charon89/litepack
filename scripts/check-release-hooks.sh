#!/usr/bin/env bash
# E6-6: the release `lpk` has no extraction test hook (D-99 (8)).
#   bash scripts/check-release-hooks.sh             check lpk-cli's release feature tree
#   bash scripts/check-release-hooks.sh --self-test check that the check fails on a planted tree
# The TOCTOU hooks of lpk_core::outtree are `cfg(test)` (compiled into lpk-core's own unit tests
# only); the share-mode switch is lpk-fs-sys's `test-hooks` feature, which lpk-core enables
# from its dev-dependencies alone. This fails when a normal (non-dev) build of lpk-cli for
# Windows shows that feature, which would put the switch in the shipped binary.
# Output goes to a file first and is searched there (no pipeline can hide a failure).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

pattern='lpk-fs-sys feature "test-hooks"'

tree_clean() {
  if grep -qF "$pattern" "$1"; then return 1; fi
  return 0
}

if [ "${1:-}" = "--self-test" ]; then
  t="$(mktemp -d)"
  trap 'rm -rf "$t"' EXIT
  printf 'lpk-cli v0.0.1\n|-- lpk-fs-sys feature "default"\n' > "$t/clean"
  printf 'lpk-cli v0.0.1\n|-- lpk-fs-sys feature "test-hooks"\n' > "$t/hooks"
  fail=0
  tree_clean "$t/clean" || { echo "self-test: a clean tree was refused" >&2; fail=1; }
  if tree_clean "$t/hooks"; then echo "self-test: test-hooks not found" >&2; fail=1; fi
  [ "$fail" = 0 ] && echo "self-test: the check fails on a planted feature and passes a clean tree"
  exit "$fail"
fi

out="$root/target/release-hooks-check"
mkdir -p "$out"
cd "$root"
# -e normal: the release build's edges only (dev-dependencies are not part of it).
cargo tree --locked -e normal,features -p lpk-cli --target x86_64-pc-windows-msvc > "$out/tree.txt"
grep -qF 'lpk-fs-sys' "$out/tree.txt" || { echo "error: lpk-fs-sys is not in lpk-cli's Windows tree" >&2; exit 1; }
if ! tree_clean "$out/tree.txt"; then
  echo "error: lpk-fs-sys's test-hooks feature is in lpk-cli's release build" >&2
  exit 1
fi
echo "test-hooks: absent from lpk-cli's release build (the outtree hooks are cfg(test))"
