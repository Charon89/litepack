#!/usr/bin/env bash
# E4-2: the shipped worker links no C codec. Builds `lpk-worker` for release (default features,
# so without `faults`) and fails when its symbols hold an unmangled `inflate*`, `ZSTD_*` or
# `BZ2_*` name: on Windows from the linker map (/MAP), on Linux from `nm`. Also fails when a
# default build's feature tree shows `faults`. Run from the repository root:
#   bash scripts/check-worker-symbols.sh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/workers"

if cargo tree --locked -e features -p lpk-worker | grep -q 'faults'; then
  echo "error: the faults feature is in lpk-worker's default feature tree" >&2
  exit 1
fi
echo "faults: absent from lpk-worker's default build"

# A target directory of its own: the map flag changes RUSTFLAGS, which would rebuild the shared one.
export CARGO_TARGET_DIR="$root/target/worker-symbols"
pattern='(^|[[:space:]])_?(inflate[A-Za-z0-9_]*|ZSTD_[A-Za-z0-9_]+|BZ2_[A-Za-z0-9_]+)([[:space:]]|$)'
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    map="$CARGO_TARGET_DIR/lpk-worker.map"
    mkdir -p "$CARGO_TARGET_DIR"
    # `cargo rustc` passes the flag to the worker's own link only (not to proc macros), as
    # one argument (the path may hold spaces).
    winmap="$(cygpath -w "$map" 2>/dev/null || echo "$map")"
    cargo rustc --locked --release -p lpk-worker --bin lpk-worker -- -C "link-arg=/MAP:$winmap"
    [ -s "$map" ] || { echo "error: no linker map at $map" >&2; exit 1; }
    syms="$map"
    ;;
  *)
    cargo build --locked --release -p lpk-worker
    syms="$CARGO_TARGET_DIR/symbols.txt"
    nm "$CARGO_TARGET_DIR/release/lpk-worker" > "$syms"
    ;;
esac
if grep -E "$pattern" "$syms" | head -n 20 | grep -q .; then
  echo "error: lpk-worker links a C codec symbol:" >&2
  grep -E "$pattern" "$syms" | head -n 20 >&2
  exit 1
fi
echo "symbols: no inflate*, ZSTD_* or BZ2_* in lpk-worker ($(wc -l < "$syms") lines checked)"
