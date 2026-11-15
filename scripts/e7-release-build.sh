#!/usr/bin/env bash
# E7-2 (release.yml's build-x64 and build-arm64 jobs): the build jobs' shared body.
#   bash scripts/e7-release-build.sh <x64|arm64> <out dir>
# Builds the six shipped PEs through scripts/repro-build.sh (--auditable --gui, so what ships is what
# release-checks.yml's reproducible job compares), runs scripts/check-worker-symbols.sh for x64 as ci.yml's
# `workers` job runs it (its self-test, then the check: it links an x64 release worker with /MAP, so it has
# nothing to say about ARM64), and writes <out dir>/unsigned.sha256: the six PEs in `sha256sum` format, the
# list the package job checks its downloads against. Then (E7-2 part 1d) one CycloneDX SBOM per shipped binary in
# <out dir>/sboms/, listed in <out dir>/sboms.sha256 (below). Nothing here signs anything (E7-2 part 2).
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

# E7-2 part 1d: one CycloneDX SBOM per shipped binary (cargo-cyclonedx at the version of scripts/release-tools.env),
# for this target, of the dependency graphs the build above used: the root workspace with its default features (as
# repro-build.sh's `cargo build -p ...`), lpk-gui with tauri-cli's features, and workers/ with its default features
# (never `faults`). cargo-cyclonedx writes `<binary>_bin.cdx.json` (or `_cdylib`) beside each member's Cargo.toml,
# every member's in every run, so each run's files are moved out of the tree at once. scripts/e7-sboms.py collect
# keeps the six shipped binaries' as <out>/sboms/<binary>.cdx.json; <out>/sboms.sha256 lists them (the job's second
# output; unsigned.sha256 stays the list of the six PEs every later job reads).
cargo cyclonedx --version > /dev/null 2>&1 ||
  { echo "error: the SBOMs need cargo-cyclonedx (\`cargo install --locked --version =\$CARGO_CYCLONEDX_VERSION cargo-cyclonedx\`, the version in scripts/release-tools.env)" >&2; exit 2; }
cargo cyclonedx --version
sbom=(cargo cyclonedx -q --describe binaries --format json --spec-version 1.5 --target "$triple")
# No serial number and the source commit's time as the timestamp: the same commit gives the same SBOMs.
SOURCE_DATE_EPOCH="$(git -C "$root" log -1 --format=%ct)"
export SOURCE_DATE_EPOCH
tree_sboms() { find "$root/crates" "$root/workers" -name target -prune -o -name '*.cdx.json' -print; }
move_sboms() {
  local dest="$1" f rel
  while IFS= read -r f; do
    rel="${f#"$root"/}"
    mkdir -p "$dest/$(dirname "$rel")"
    mv "$f" "$dest/$rel"
  done < <(tree_sboms)
}
stale="$(tree_sboms)"
[ -z "$stale" ] || { echo "error: SBOMs already in the tree: $stale" >&2; exit 1; }
raw="$(mktemp -d)"
(cd "$root" && "${sbom[@]}")
move_sboms "$raw/workspace"
# tauri-cli's build (2.12.4, interface/rust.rs) runs cargo with --no-default-features and the default features (minus
# any that names tauri/custom-protocol) plus tauri/custom-protocol: for lpk-gui's `default = ["updater"]`, these.
grep -qx 'default = \["updater"\]' "$root/crates/lpk-gui/Cargo.toml" ||
  { echo "error: lpk-gui's default features changed; update the GUI's SBOM features here" >&2; exit 1; }
(cd "$root/crates/lpk-gui" && "${sbom[@]}" --no-default-features --features "updater tauri/custom-protocol")
move_sboms "$raw/gui-run"
mkdir -p "$raw/gui"
mv "$raw/gui-run/crates/lpk-gui/lpk-gui_bin.cdx.json" "$raw/gui/"
rm -rf "$raw/gui-run"
# The root workspace run's lpk-gui SBOM has the default features, not the built ones: the GUI run's replaces it.
rm "$raw/workspace/crates/lpk-gui/lpk-gui_bin.cdx.json"
(cd "$root/workers" && "${sbom[@]}")
move_sboms "$raw/workers"
# cargo-cyclonedx has no --locked: its `cargo metadata` would rewrite a stale lockfile. The build ran --locked with
# these very lockfiles, and they are unchanged, so the SBOMs describe the locked graphs.
git -C "$root" diff --exit-code -- Cargo.lock workers/Cargo.lock ||
  { echo "error: cargo cyclonedx changed a lockfile" >&2; exit 1; }
helper="$(cygpath -m "$root")/scripts/e7-sboms.py"
python -I "$helper" collect --target "$triple" --out "$(cygpath -m "$out")/sboms" "$(cygpath -m "$raw")/workspace" \
  "$(cygpath -m "$raw")/gui" "$(cygpath -m "$raw")/workers"
rm -rf "$raw"
(cd "$out" && sha256sum sboms/*.cdx.json > sboms.sha256)
n=$(wc -l < "$out/sboms.sha256")
[ "$n" = 6 ] || { echo "error: expected 6 lines in sboms.sha256, got $n" >&2; exit 1; }
echo "sboms.sha256 ($1, $triple):"
cat "$out/sboms.sha256"
