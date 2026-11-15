#!/usr/bin/env bash
# E8-5, E7-2: build the shipped Windows binaries the reproducible way and write their SHA-256.
#   bash scripts/repro-build.sh [--target TRIPLE] [--out DIR] [--auditable] [--gui]
# TRIPLE is x86_64-pc-windows-msvc (default) or aarch64-pc-windows-msvc; DIR defaults to
# target/repro/TRIPLE. The shipped binaries: `lpk.exe` (lpk-cli), `lpk-check.exe` (lpk-check, shipped
# with its own THIRD-PARTY-NOTICES section, LIC-2), `lpk-worker.exe` (built in workers/, E4-2),
# `lpk-worker-libarchive.exe` (lpk-worker-libarchive, libarchive's C through cc, E4-4), `lpk_shell.dll`
# (lpk-shell, the Explorer handler, E6) and, with --gui, `lpk-gui.exe` (crates/lpk-gui through
# `cargo tauri build --no-bundle`, so the frontend is embedded; tauri-cli at the version of
# scripts/release-tools.env). `lpk-bench`, `lpk-test-worker` and the test helpers are not shipped.
#   --auditable  every cargo build of a shipped crate runs as `cargo auditable build` (the GUI's through
#                tauri-cli's `--runner scripts/cargo-auditable.cmd`), and each copied binary, the
#                cdylib lpk_shell.dll included, must carry cargo-auditable's `.dep-v0` section
#                (scripts/e7-pe-sections.py).
#   --gui        also builds lpk-gui.exe. Never a bundle: NSIS is never downloaded by this script.
# Release CI (E7-2) builds with both flags; a developer machine without cargo-auditable uses --gui alone.
#
# The flags: `/Brepro` and `/PDBALTPATH:%_PDB%` come from .cargo/config.toml; this script adds
# `--remap-path-prefix` for the checkout (to `/lpk`) and the cargo home (to `/cargo`) with
# `--config`, which cargo appends to the file's list. The verbose cargo log is kept in DIR, and the
# script fails unless every rustc invocation of every shipped crate carries all four flags (the proof
# that they reached rustc; the lines are printed).
# Release CI (E7-2) builds through this script, so what is compared is what ships.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

target="x86_64-pc-windows-msvc"
out=""
auditable=0
gui=0
while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="${2:?--target needs a triple}"; shift 2 ;;
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --auditable) auditable=1; shift ;;
    --gui) gui=1; shift ;;
    *) echo "usage: $0 [--target TRIPLE] [--out DIR] [--auditable] [--gui]" >&2; exit 2 ;;
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

build=(cargo build)
runner=()
if [ "$auditable" = 1 ]; then
  cargo auditable --version > /dev/null 2>&1 ||
    { echo "error: --auditable needs cargo-auditable (\`cargo install --locked --version =\$CARGO_AUDITABLE_VERSION cargo-auditable\`, the version in scripts/release-tools.env)" >&2; exit 2; }
  build=(cargo auditable build)
  # tauri-cli runs `<runner> build ...` in place of `cargo build ...`; the shim turns it into `cargo auditable build ...`.
  runner=(--runner "$(cygpath -w "$root/scripts/cargo-auditable.cmd")")
fi
if [ "$gui" = 1 ]; then
  cargo tauri --version > /dev/null 2>&1 ||
    { echo "error: --gui needs tauri-cli (\`cargo install --locked --version =\$TAURI_CLI_VERSION tauri-cli\`, the version in scripts/release-tools.env)" >&2; exit 2; }
fi

ws="$(cygpath -w "$root")"
cargo_home="$(cygpath -w "${CARGO_HOME:-$HOME/.cargo}")"
# TOML literal strings: the Windows paths keep their backslashes. The workspace goes last (rustc
# applies the last matching prefix), so a target directory inside the checkout maps to /lpk.
remap="target.$target.rustflags=['--remap-path-prefix=$cargo_home=/cargo', '--remap-path-prefix=$ws=/lpk']"

# The worker is built from a fixed drive letter mapped onto the checkout (`subst`). Its workspace
# (workers/) reaches lpk-proto and lpk-sandbox-sys through `../crates`, outside its own root, and
# cargo hashes such a path dependency's absolute path into its `-C metadata` (symbol names, and so
# the binary): measured, two checkouts gave two different lpk-worker.exe with every flag above,
# while lpk.exe and lpk-check.exe (all their path dependencies inside the root workspace, hashed
# by relative path) were equal. From the fixed drive the absolute paths are the same everywhere.
drive="${LPK_REPRO_DRIVE:-W:}"
remap_w="target.$target.rustflags=['--remap-path-prefix=$cargo_home=/cargo', '--remap-path-prefix=$drive=/lpk']"

# The shipped crates (rustc's crate names, for the proof below) and the files they ship as.
crates=(lpk lpk_check lpk_worker_libarchive lpk_shell lpk_worker)
bins=(lpk.exe lpk-check.exe lpk-worker-libarchive.exe lpk_shell.dll lpk-worker.exe)
if [ "$gui" = 1 ]; then
  crates+=(lpk_gui)
  bins+=(lpk-gui.exe)
fi

log="$out/cargo-build.log"
rustc --version | tee "$out/rustc-version.txt"
echo "building with \`${build[*]}\`: lpk, lpk-check, lpk-worker-libarchive and lpk-shell in $ws, lpk-worker in $drive\\workers ($drive mapped onto the checkout)$([ "$gui" = 1 ] && echo ", lpk-gui through cargo tauri build in $ws\\crates\\lpk-gui"), for $target"
status=0
(cd "$root" && "${build[@]}" --locked --release -v --target "$target" --config "$remap" \
  -p lpk-cli -p lpk-check -p lpk-worker-libarchive -p lpk-shell) > "$log" 2>&1 || status=$?
if [ "$status" = 0 ]; then
  if ! MSYS_NO_PATHCONV=1 subst "$drive" "$ws" >> "$log" 2>&1; then
    echo "error: cannot map $drive onto the checkout (in use? \`subst\` lists the mappings; LPK_REPRO_DRIVE picks another letter, which changes lpk-worker.exe)" >&2
    exit 1
  fi
  trap 'MSYS_NO_PATHCONV=1 subst "$drive" /D > /dev/null 2>&1 || true' EXIT
  (cd "$drive/workers" && "${build[@]}" --locked --release -v --target "$target" --config "$remap_w" -p lpk-worker) >> "$log" 2>&1 || status=$?
fi
if [ "$status" = 0 ] && [ "$gui" = 1 ]; then
  # `--no-bundle` and no `--config` overlay: tauri.conf.json's `bundle.active` is false, and the NSIS
  # overlay (tauri.nsis.conf.json) is never named here. After `--`, the arguments cargo gets.
  (cd "$root/crates/lpk-gui" && cargo tauri build --no-bundle --ci --target "$target" "${runner[@]}" \
    -- --locked -v --config "$remap") >> "$log" 2>&1 || status=$?
fi
if [ "$status" != 0 ]; then
  echo "error: the build failed (exit $status); the end of $log:" >&2
  tail -n 60 "$log" >&2
  exit "$status"
fi

# The proof that the flags reached rustc: every invocation of a shipped crate carries all four,
# and every shipped crate has at least one.
pattern="--crate-name ($(IFS='|'; echo "${crates[*]}")) "
grep -E -- "$pattern" "$log" > "$out/rustc-shipped.txt" || true
bad=0
for c in "${crates[@]}"; do
  grep -qE -- "--crate-name $c " "$out/rustc-shipped.txt" ||
    { echo "error: no rustc invocation of $c in $log" >&2; bad=1; }
done
while IFS= read -r line; do
  name="$(echo "$line" | grep -oE -- "--crate-name [a-z_]+" | head -n 1)"
  from="$ws"
  [ "$name" = "--crate-name lpk_worker" ] && from="$drive"
  for flag in "link-arg=/Brepro" "link-arg=/PDBALTPATH:%_PDB%" "--remap-path-prefix=$cargo_home=/cargo" "--remap-path-prefix=$from=/lpk"; do
    if grep -qF -- "$flag" <<< "$line"; then
      echo "flags reached rustc ($name): $flag"
    else
      echo "error: $name: rustc ran without $flag" >&2
      bad=1
    fi
  done
done < "$out/rustc-shipped.txt"
[ "$bad" = 0 ] || exit 1

rel="$root/target/$target/release"
for b in "${bins[@]}"; do
  cp "$rel/$b" "$out/$b"
done
if [ "$auditable" = 1 ]; then
  # cargo-auditable's dependency list, embedded in every shipped binary (fatal if absent). The DLL is
  # held to it too: CI showed cargo-auditable embeds it in the cdylib on both targets (R383).
  # The helper's path goes to the native python as a Windows path (no reliance on argument conversion).
  helper="$(cygpath -m "$root")/scripts/e7-pe-sections.py"
  (cd "$out" && python -I "$helper" --require .dep-v0 "${bins[@]}")
fi
(cd "$out" && sha256sum "${bins[@]}" > SHA256SUMS)
cat "$out/SHA256SUMS"
