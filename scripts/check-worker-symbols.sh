#!/usr/bin/env bash
# E4-2: the shipped worker has no test faults and links no C codec.
#   bash scripts/check-worker-symbols.sh             build and check the release lpk-worker
#   bash scripts/check-worker-symbols.sh --self-test check that the checks fail on planted names
# The release artefact itself is checked: on Windows `target/release/lpk-worker.exe` is linked
# with a linker map (/MAP) in the same link, and the map is searched; on Linux the same binary's
# symbol table (`nm`). It fails on an unmangled `inflate*`, `ZSTD_*` or `BZ2_*` symbol, and when
# a default build's feature tree (`cargo tree -e features`) shows `faults`.
# Every command's output goes to a file first and is searched there, so no pipeline can turn
# a failure into "not found" (pipefail with an early-exiting grep).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

pattern='(^|[[:space:]])_?(inflate[A-Za-z0-9_]*|ZSTD_[A-Za-z0-9_]+|BZ2_[A-Za-z0-9_]+)([[:space:]]|$)'
faults_pattern='(^|[^A-Za-z0-9_-])faults([^A-Za-z0-9_-]|$)'

# 0 when the feature tree in file $1 is clean, 1 when it shows `faults`.
tree_clean() {
  if grep -qE "$faults_pattern" "$1"; then return 1; fi
  return 0
}

# 0 when the symbol listing in file $1 is clean, 1 when it holds a C codec symbol.
symbols_clean() {
  if grep -qE "$pattern" "$1"; then return 1; fi
  return 0
}

if [ "${1:-}" = "--self-test" ]; then
  t="$(mktemp -d)"
  trap 'rm -rf "$t"' EXIT
  printf 'lpk-worker v0.0.1\n|-- lpk-proto feature "default"\n' > "$t/tree-clean"
  printf 'lpk-worker v0.0.1\n|-- lpk-proto feature "faults"\n' > "$t/tree-faults"
  printf ' 0001:00001000       _ZN4core3fmt5write17h0123456789abcdefE 0000000140001000 f\n' > "$t/map-clean"
  printf ' 0001:00012340       inflateInit2_              0000000140013340 f   z:inflate.obj\n' > "$t/map-zlib"
  printf '0000000000401000 T ZSTD_decompressDCtx\n' > "$t/nm-zstd"
  printf '0000000000401000 T BZ2_bzDecompress\n' > "$t/nm-bz2"
  fail=0
  tree_clean "$t/tree-clean" || { echo "self-test: a clean tree was refused" >&2; fail=1; }
  if tree_clean "$t/tree-faults"; then echo "self-test: faults not found" >&2; fail=1; fi
  symbols_clean "$t/map-clean" || { echo "self-test: a clean map was refused" >&2; fail=1; }
  for f in map-zlib nm-zstd nm-bz2; do
    if symbols_clean "$t/$f"; then echo "self-test: $f not found" >&2; fail=1; fi
  done
  [ "$fail" = 0 ] && echo "self-test: the checks fail on planted names and pass clean input"
  exit "$fail"
fi

cd "$root/workers"
out="$root/target/worker-check"
mkdir -p "$out"

cargo tree --locked -e features -p lpk-worker > "$out/tree.txt"
if ! tree_clean "$out/tree.txt"; then
  echo "error: the faults feature is in lpk-worker's default feature tree" >&2
  exit 1
fi
echo "faults: absent from lpk-worker's default build"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    map="$out/lpk-worker.map"
    winmap="$(cygpath -w "$map" 2>/dev/null || echo "$map")"
    # The release artefact, with its map from the same link (`cargo rustc` passes the flag to
    # the worker's own link only, as one argument: the path may hold spaces; the flag is part
    # of the fingerprint, so an artefact linked without it is linked again here).
    cargo rustc --locked --release -p lpk-worker --bin lpk-worker -- -C "link-arg=/MAP:$winmap"
    bin="$root/target/release/lpk-worker.exe"
    [ -s "$map" ] || { echo "error: no linker map at $map" >&2; exit 1; }
    # The map belongs to this artefact: written by the same link, not older than it.
    if [ $(( $(stat -c %Y "$bin") - $(stat -c %Y "$map") )) -gt 60 ]; then
      echo "error: $map is older than $bin" >&2
      exit 1
    fi
    syms="$map"
    ;;
  *)
    cargo build --locked --release -p lpk-worker
    bin="$root/target/release/lpk-worker"
    syms="$out/symbols.txt"
    nm "$bin" > "$syms"
    ;;
esac
[ -f "$bin" ] || { echo "error: no release worker at $bin" >&2; exit 1; }
if ! symbols_clean "$syms"; then
  echo "error: $bin links a C codec symbol:" >&2
  grep -E "$pattern" "$syms" > "$out/found.txt" || true
  head -n 20 "$out/found.txt" >&2
  exit 1
fi
echo "symbols: no inflate*, ZSTD_* or BZ2_* in $(basename "$bin") ($(wc -l < "$syms") lines checked)"
