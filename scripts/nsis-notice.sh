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
  conf_arg="$(cd -- "$(dirname -- "$2")" && pwd)/$(basename -- "$2")"
fi
cd "$(dirname "$0")/.."

pin=notices/nsis/pin.toml
required_compression="$(awk '$1 == "compression" && $2 == "=" { v = $3; gsub(/"/, "", v); print v; exit }' "$pin")"
if [[ -z "$required_compression" ]]; then
  echo "$pin: no compression" >&2
  exit 1
fi

# Fails, with one line, unless the installer's compression in tauri.conf.json is the pinned one (zlib). An
# unreadable file, invalid JSON or a level that is not an object reads as "(unreadable: ...)" or "(absent)"; a value
# that is not a plain word is printed as JSON, so it can never compare equal by accident.
check_compression() {
  local conf="$1" found
  found="$(python3 -I -c '
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as f:
        value = json.load(f)
except (OSError, ValueError) as e:
    print("(unreadable: %s)" % type(e).__name__)
    sys.exit(0)
for key in ("bundle", "windows", "nsis", "compression"):
    value = value.get(key) if isinstance(value, dict) else None
if value is None:
    print("(absent)")
elif isinstance(value, str) and value.isalnum() and value.isascii():
    print(value)
else:
    print(json.dumps(value))
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

# GitHub's release downloads can answer 504 for minutes at a time (three of four CI runs on 2026-11-08 failed on
# nsis.zip after curl's default --retry 3 backoff of about 7 s). Retry each download for up to three minutes, 20 s
# apart (the limits below bound each attempt).
#
# One bound for every attempt of every download, identical in nsis-notice.sh and webview2-notice.sh
# (scripts/notice-curl-test.sh compares the two `curl_limits=` blocks, so edit both together).
# Sizes of the files (2026-11-08): nsis-3.13.zip 2,363,925 bytes, nsis_tauri_utils.dll 34,304, LICENSE_MIT 1,110,
# the WebView2 package 8,973,762. Assumed rate of a normal download: 100 kB/s or better (GitHub and nuget.org
# answer a runner in MB/s); the largest file then takes about 90 s, inside --max-time 120.
#   --speed-limit 10240 --speed-time 30   abort a transfer that stays under 10 kB/s for 30 s (a stall)
#   --max-time 120                        abort any single attempt after 120 s, however it is going
# curl counts a timeout as transient and retries it, so --retry-max-time 180 only stops NEW attempts; with the
# per-attempt bound a download ends within 180 s + 120 s = 300 s at worst.
# Worst case of the job: its four downloads (nsis-notice.sh: 3, webview2-notice.sh: 1) at 300 s each is 20 min,
# more than the "notices pins" job's 15-minute timeout (.github/workflows/ci.yml): then the job fails closed at the
# timeout, as before, but only when every download fails slowly; one stalled download now costs 5 min, not 15.
curl_limits=(--connect-timeout 30 --speed-limit 10240 --speed-time 30 --max-time 120
  --retry 6 --retry-delay 20 --retry-max-time 180)
fetch() { curl -fsSL "${curl_limits[@]}" "$@"; }

fetch -o "$work/nsis.zip" "$(pin_value nsis url)"
# The archive's hashes first: nothing is unpacked from an archive that is not the pinned one.
if ! check_hashes "$work/nsis.zip" nsis; then
  exit 1
fi
unzip -p "$work/nsis.zip" "$(pin_value nsis licence)" | tr -d '\r' > "$work/COPYING"
if ! cmp "$work/COPYING" notices/nsis/COPYING; then
  echo "notices/nsis/COPYING differs from $(pin_value nsis licence) in the NSIS $(pin_value nsis version) archive" >&2
  status=1
fi

fetch -o "$work/nsis_tauri_utils.dll" "$(pin_value nsis_tauri_utils url)"
# The plugin DLL and its LICENSE_MIT are only hashed and compared, never unpacked or run, so their checks collect
# into status and report together.
check_hashes "$work/nsis_tauri_utils.dll" nsis_tauri_utils || status=1
tag="nsis_tauri_utils-v$(pin_value nsis_tauri_utils version)"
fetch -o "$work/LICENSE_MIT" \
  "https://raw.githubusercontent.com/tauri-apps/nsis-tauri-utils/$tag/$(pin_value nsis_tauri_utils licence)"
if ! cmp "$work/LICENSE_MIT" notices/nsis/nsis_tauri_utils-LICENSE_MIT; then
  echo "notices/nsis/nsis_tauri_utils-LICENSE_MIT differs from LICENSE_MIT at $tag" >&2
  status=1
fi

if [[ "$status" -ne 0 ]]; then
  exit "$status"
fi
echo "NSIS notices pinned: NSIS $(pin_value nsis version) and nsis_tauri_utils $(pin_value nsis_tauri_utils version) match their archives and texts"
