#!/usr/bin/env bash
# E8-5: build the shipped Windows binaries the reproducible way and write their SHA-256.
#   bash scripts/repro-build.sh [--target TRIPLE] [--out DIR]
# TRIPLE is x86_64-pc-windows-msvc (default) or aarch64-pc-windows-msvc; DIR defaults to
# target/repro/TRIPLE. The shipped binaries: `lpk` (lpk-cli), `lpk-check` (lpk-check, shipped with
# its own THIRD-PARTY-NOTICES section, LIC-2) and `lpk-worker` (built in workers/, E4-2).
# `lpk-bench`, `lpk-test-worker` and the test helpers are not shipped. The GUI comes later (E7).
#
# The flags: `/Brepro` and `/PDBALTPATH:%_PDB%` come from .cargo/config.toml; this script adds
# `--remap-path-prefix` for the checkout (to `/lpk`) and the cargo home (to `/cargo`) with
# `--config`, which cargo appends to the file's list. The verbose cargo log is kept in DIR, and the
# script fails unless every rustc invocation of a shipped crate carries all four flags (the proof
# that they reached rustc; the lines are printed).
# Release CI (E7-2) builds through this script, so what is compared is what ships.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

target="x86_64-pc-windows-msvc"
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="${2:?--target needs a triple}"; shift 2 ;;
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    *) echo "usage: $0 [--target TRIPLE] [--out DIR]" >&2; exit 2 ;;
  esac
done
case "$target" in
  x86_64-pc-windows-msvc|aarch64-pc-windows-msvc) ;;
  *) echo "error: $target is not a shipped target" >&2; exit 2 ;;
esac
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) ;;
  *) echo "error: the shipped binaries are built on Windows (MSVC linker)" >&2; exit 2 ;;
esac
[ -n "$out" ] || out="$root/target/repro/$target"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

# An environment RUSTFLAGS (or its encoded or per-target form) would replace the config's flags.
target_env="CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')_RUSTFLAGS"
for v in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS "$target_env"; do
  if [ -n "${!v+x}" ]; then
    echo "note: $v was set (${!v}); unset for this build so that the configured flags apply"
    unset "$v"
  fi
done

ws="$(cygpath -w "$root")"
cargo_home="$(cygpath -w "${CARGO_HOME:-$HOME/.cargo}")"
# TOML literal strings: the Windows paths keep their backslashes. The workspace goes last (rustc
# applies the last matching prefix), so a target directory inside the checkout maps to /lpk.
remap="target.$target.rustflags=['--remap-path-prefix=$cargo_home=/cargo', '--remap-path-prefix=$ws=/lpk']"

log="$out/cargo-build.log"
rustc --version | tee "$out/rustc-version.txt"
echo "building lpk, lpk-check and lpk-worker for $target in $ws"
status=0
{
  (cd "$root" && cargo build --locked --release -v --target "$target" --config "$remap" -p lpk-cli -p lpk-check) &&
    (cd "$root/workers" && cargo build --locked --release -v --target "$target" --config "$remap" -p lpk-worker)
} > "$log" 2>&1 || status=$?
if [ "$status" != 0 ]; then
  echo "error: the build failed (exit $status); the end of $log:" >&2
  tail -n 60 "$log" >&2
  exit "$status"
fi

# The proof that the flags reached rustc: every invocation of a shipped crate carries all four.
grep -E -- "--crate-name (lpk|lpk_check|lpk_worker) " "$log" > "$out/rustc-shipped.txt" || true
n="$(wc -l < "$out/rustc-shipped.txt")"
[ "$n" -ge 3 ] || { echo "error: expected the rustc lines of lpk, lpk_check and lpk_worker in $log, found $n" >&2; exit 1; }
bad=0
while IFS= read -r line; do
  name="$(echo "$line" | grep -oE -- "--crate-name [a-z_]+" | head -n 1)"
  for flag in "link-arg=/Brepro" "link-arg=/PDBALTPATH:%_PDB%" "--remap-path-prefix=$cargo_home=/cargo" "--remap-path-prefix=$ws=/lpk"; do
    if ! grep -qF -- "$flag" <<< "$line"; then
      echo "error: $name: rustc ran without $flag" >&2
      bad=1
    fi
  done
  echo "flags reached rustc ($name): $(grep -oE -- "-C ?link-arg=/(Brepro|PDBALTPATH:%_PDB%)|--remap-path-prefix=[^ ]*" <<< "$line" | tr '\n' ' ')"
done < "$out/rustc-shipped.txt"
[ "$bad" = 0 ] || exit 1

rel="$root/target/$target/release"
for b in lpk lpk-check lpk-worker; do
  cp "$rel/$b.exe" "$out/$b.exe"
done
(cd "$out" && sha256sum lpk.exe lpk-check.exe lpk-worker.exe > SHA256SUMS)
cat "$out/SHA256SUMS"
