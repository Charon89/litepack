#!/usr/bin/env bash
# E7-2 (release.yml's build-x64 and build-arm64 jobs): the build jobs' shared body.
#   bash scripts/e7-release-build.sh <x64|arm64> <out dir>
# Builds the six shipped PEs through scripts/repro-build.sh (--auditable --gui, so what ships is what
# release-checks.yml's reproducible job compares), runs scripts/check-worker-symbols.sh for x64 as ci.yml's
# `workers` job runs it (its self-test, then the check: it links an x64 release worker with /MAP, so it has
# nothing to say about ARM64), and writes <out dir>/unsigned.sha256: the six PEs in `sha256sum` format, the
# list the package job checks its downloads against. Nothing here signs anything (E7-2 part 2).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
usage() { echo "usage: $0 <x64|arm64> <out dir>" >&2; exit 2; }
[ $# -eq 2 ] || usage
case "$1" in
  x64) triple=x86_64-pc-windows-msvc ;;
  arm64) triple=aarch64-pc-windows-msvc ;;
  *) usage ;;
esac
out="$2"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

bash "$root/scripts/repro-build.sh" --target "$triple" --auditable --gui --out "$out"

if [ "$1" = x64 ]; then
  bash "$root/scripts/check-worker-symbols.sh" --self-test
  bash "$root/scripts/check-worker-symbols.sh"
fi

pes=(lpk.exe lpk-check.exe lpk-worker.exe lpk-worker-libarchive.exe lpk_shell.dll lpk-gui.exe)
for p in "${pes[@]}"; do
  [ -f "$out/$p" ] || { echo "error: $out/$p was not built" >&2; exit 1; }
done
(cd "$out" && sha256sum "${pes[@]}" > unsigned.sha256)
n=$(wc -l < "$out/unsigned.sha256")
[ "$n" = 6 ] || { echo "error: expected 6 lines in unsigned.sha256, got $n" >&2; exit 1; }
echo "unsigned.sha256 ($1, $triple):"
cat "$out/unsigned.sha256"
