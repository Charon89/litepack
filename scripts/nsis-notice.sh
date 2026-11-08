#!/usr/bin/env bash
# E7-8: the installer's licence texts in THIRD-PARTY-NOTICES, checked against what the bundler fetches
# (notices/nsis/pin.toml). CI only (it downloads); from the repository root:
#   scripts/nsis-notice.sh --check   download the pinned NSIS archive and nsis_tauri_utils.dll, check their SHA-1
#                                    (the bundler's pins) and SHA-256, compare the archive's COPYING with
#                                    notices/nsis/COPYING (line ends normalised to LF) and the plugin's LICENSE_MIT
#                                    at its release tag with notices/nsis/nsis_tauri_utils-LICENSE_MIT; first, the
#                                    compression check below
#   scripts/nsis-notice.sh --compression [<tauri.conf.json>]
#                                    offline: fail unless bundle.windows.nsis.compression is exactly "zlib"
#                                    (default crates/lpk-gui/tauri.conf.json). Without the key tauri-bundler uses
#                                    LZMA (CPL-1.0), and THIRD-PARTY-NOTICES says LZMA is not used.
set -euo pipefail

# An explicit tauri.conf.json is taken relative to the caller's directory, before the cd below.
conf_arg=""
if [[ "${1:-}" == "--compression" && -n "${2:-}" ]]; then
  conf_arg="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
fi
cd "$(dirname "$0")/.."

pin=notices/nsis/pin.toml
required_compression="$(awk '$1 == "compression" && $2 == "=" { v = $3; gsub(/"/, "", v); print v; exit }' "$pin")"
if [[ -z "$required_compression" ]]; then
  echo "$pin: no compression" >&2
  exit 1
fi

# Fails, with one line, unless the installer's compression in tauri.conf.json is the pinned one (zlib).
check_compression() {
  local conf="$1" found
  found="$(python3 -I -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    conf = json.load(f)
value = conf.get("bundle", {}).get("windows", {}).get("nsis", {}).get("compression")
print("(absent)" if value is None else json.dumps(value) if not isinstance(value, str) else value)
' "$conf")"
  if [[ "$found" != "$required_compression" ]]; then
    echo "$conf: bundle.windows.nsis.compression is $found, $required_compression is required ($pin)" >&2
    exit 1
  fi
  echo "$conf: bundle.windows.nsis.compression is $required_compression"
}

if [[ "${1:-}" == "--compression" ]]; then
  check_compression "${conf_arg:-crates/lpk-gui/tauri.conf.json}"
  exit 0
fi

# The value of `key = "value"` in section [$1] of the pin file.
pin_value() {
  local value
  value="$(awk -v section="[$1]" -v key="$2" '
    /^\[/ { in_section = ($0 == section); next }
    in_section && $1 == key && $2 == "=" { v = $3; gsub(/"/, "", v); print v; exit }
  ' "$pin")"
  if [[ -z "$value" ]]; then
    echo "$pin: no $2 in [$1]" >&2
    exit 1
  fi
  printf '%s' "$value"
}

# The SHA-1 and SHA-256 of a downloaded file against the pin's section.
check_hashes() {
  local file="$1" section="$2" status=0 got
  got="$(sha1sum "$file" | cut -d' ' -f1)"
  if [[ "$got" != "$(pin_value "$section" sha1)" ]]; then
    echo "[$section] SHA-1 $got differs from $pin" >&2
    status=1
  fi
  got="$(sha256sum "$file" | cut -d' ' -f1)"
  if [[ "$got" != "$(pin_value "$section" sha256)" ]]; then
    echo "[$section] SHA-256 $got differs from $pin" >&2
    status=1
  fi
  return "$status"
}

if [[ "${1:-}" != "--check" ]]; then
  echo "usage: $0 --check" >&2
  exit 2
fi

check_compression crates/lpk-gui/tauri.conf.json

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
status=0

curl -fsSL --retry 3 -o "$work/nsis.zip" "$(pin_value nsis url)"
check_hashes "$work/nsis.zip" nsis || status=1
unzip -p "$work/nsis.zip" "$(pin_value nsis licence)" | tr -d '\r' > "$work/COPYING"
if ! cmp "$work/COPYING" notices/nsis/COPYING; then
  echo "notices/nsis/COPYING differs from $(pin_value nsis licence) in the NSIS $(pin_value nsis version) archive" >&2
  status=1
fi

curl -fsSL --retry 3 -o "$work/nsis_tauri_utils.dll" "$(pin_value nsis_tauri_utils url)"
check_hashes "$work/nsis_tauri_utils.dll" nsis_tauri_utils || status=1
tag="nsis_tauri_utils-v$(pin_value nsis_tauri_utils version)"
curl -fsSL --retry 3 -o "$work/LICENSE_MIT" \
  "https://raw.githubusercontent.com/tauri-apps/nsis-tauri-utils/$tag/$(pin_value nsis_tauri_utils licence)"
if ! cmp "$work/LICENSE_MIT" notices/nsis/nsis_tauri_utils-LICENSE_MIT; then
  echo "notices/nsis/nsis_tauri_utils-LICENSE_MIT differs from LICENSE_MIT at $tag" >&2
  status=1
fi

if [[ "$status" -ne 0 ]]; then
  exit "$status"
fi
echo "NSIS notices pinned: NSIS $(pin_value nsis version) and nsis_tauri_utils $(pin_value nsis_tauri_utils version) match their archives and texts"
