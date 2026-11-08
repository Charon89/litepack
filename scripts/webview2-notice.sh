#!/usr/bin/env bash
# E7-8: the WebView2 loader's licence text, pinned (docs/notes/e7-design.md section 7). lpk-gui.exe links the
# WebView2LoaderStatic.lib that webview2-com-sys vendors per architecture; notices/webview2.toml names the
# Microsoft.Web.WebView2 NuGet package whose x64 and arm64 libraries equal those files byte for byte, and
# notices/webview2/LICENSE.txt is that package's LICENSE.txt. CI only (it downloads); from the repository root:
#   scripts/webview2-notice.sh --check            download the pinned package, check its SHA-256, compare its
#                                                 LICENSE.txt with the committed copy, check both libraries'
#                                                 hashes against the pin and against the webview2-com-sys that
#                                                 Cargo.lock resolves (run `cargo fetch --locked` first)
#   scripts/webview2-notice.sh --find <version>... print each candidate's library hashes and say which matches
#                                                 (for moving the pin after a webview2-com-sys bump; no CI step
#                                                 runs it)
set -euo pipefail
cd "$(dirname "$0")/.."

pin=notices/webview2.toml
committed=notices/webview2/LICENSE.txt
nuget=https://api.nuget.org/v3-flatcontainer/microsoft.web.webview2

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The value of `key = "value"` in the pin file.
pin_value() {
  local value
  value="$(sed -n "s/^$1 = \"\\([^\"]*\\)\"\$/\\1/p" "$pin")"
  if [[ -z "$value" ]]; then
    echo "$pin: no value for $1" >&2
    exit 1
  fi
  printf '%s' "$value"
}

sha256() { sha256sum "$1" | cut -d' ' -f1; }

# The webview2-com-sys version Cargo.lock resolves, and its unpacked source in the cargo registry.
crate_version() {
  awk '$0 == "name = \"webview2-com-sys\"" { getline; gsub(/version = |"/, ""); print; exit }' Cargo.lock
}
crate_dir() {
  local version dir
  version="$(crate_version)"
  dir="$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "webview2-com-sys-$version" | head -n 1)"
  if [[ -z "$dir" ]]; then
    echo "webview2-com-sys $version is not in the cargo registry: run cargo fetch --locked first" >&2
    exit 1
  fi
  printf '%s' "$dir"
}

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

# Downloads one package version to $work/<version>.nupkg. Retries for up to three minutes, 20 s apart, as
# nsis-notice.sh does: a 504 from the server lasting longer than curl's default backoff is not a failure of the pin.
download() {
  local version="$1"
  curl -fsSL "${curl_limits[@]}" \
    -o "$work/$version.nupkg" "$nuget/$version/microsoft.web.webview2.$version.nupkg"
}

# Unpacks a downloaded package into $work/<version>/.
unpack() {
  local version="$1"
  mkdir -p "$work/$version"
  unzip -q "$work/$version.nupkg" -d "$work/$version"
}

case "${1:-}" in
  --find)
    shift
    dir="$(crate_dir)"
    want_x64="$(sha256 "$dir/x64/WebView2LoaderStatic.lib")"
    want_arm64="$(sha256 "$dir/arm64/WebView2LoaderStatic.lib")"
    echo "webview2-com-sys $(crate_version): x64 $want_x64 arm64 $want_arm64"
    for version in "$@"; do
      # Unpinned candidates: no hash to check before unpacking; each is unpacked only into the temporary $work.
      download "$version"
      unpack "$version"
      (cd "$work/$version" && find . -name 'WebView2LoaderStatic.lib' -o -iname 'license*')
      x64="$(sha256 "$work/$version/build/native/x64/WebView2LoaderStatic.lib")"
      arm64="$(sha256 "$work/$version/build/native/arm64/WebView2LoaderStatic.lib")"
      echo "$version: package $(sha256 "$work/$version.nupkg") x64 $x64 arm64 $arm64"
      if [[ "$x64" == "$want_x64" && "$arm64" == "$want_arm64" ]]; then
        echo "$version MATCHES; LICENSE.txt sha256 $(sha256 "$work/$version/LICENSE.txt"), base64:"
        base64 -w 0 "$work/$version/LICENSE.txt"
        echo
      fi
    done
    ;;
  --check)
    version="$(pin_value version)"
    download "$version"
    # The package's hash first: nothing is unpacked from a package that is not the pinned one.
    package="$(sha256 "$work/$version.nupkg")"
    if [[ "$package" != "$(pin_value package_sha256)" ]]; then
      echo "Microsoft.Web.WebView2 $version: package SHA-256 $package differs from $pin" >&2
      exit 1
    fi
    unpack "$version"
    status=0
    if [[ "$(sha256 "$committed")" != "$(pin_value licence_sha256)" ]]; then
      echo "$committed: SHA-256 $(sha256 "$committed") differs from $pin" >&2
      status=1
    fi
    if ! cmp "$work/$version/LICENSE.txt" "$committed"; then
      echo "$committed differs from LICENSE.txt in Microsoft.Web.WebView2 $version" >&2
      status=1
    fi
    if [[ "$(crate_version)" != "$(pin_value crate_version)" ]]; then
      echo "Cargo.lock resolves webview2-com-sys $(crate_version), $pin pins $(pin_value crate_version)" >&2
      status=1
    fi
    dir="$(crate_dir)"
    for arch in x64 arm64; do
      want="$(pin_value "${arch}_sha256")"
      in_package="$(sha256 "$work/$version/build/native/$arch/WebView2LoaderStatic.lib")"
      in_crate="$(sha256 "$dir/$arch/WebView2LoaderStatic.lib")"
      if [[ "$in_package" != "$want" ]]; then
        echo "$arch WebView2LoaderStatic.lib in the package is $in_package, $pin says $want" >&2
        status=1
      fi
      if [[ "$in_crate" != "$want" ]]; then
        echo "$arch WebView2LoaderStatic.lib in webview2-com-sys $(crate_version) is $in_crate, $pin says $want" >&2
        status=1
      fi
    done
    if [[ "$status" -ne 0 ]]; then
      exit "$status"
    fi
    echo "WebView2 notice pinned: Microsoft.Web.WebView2 $version, LICENSE.txt and both loader libraries match"
    ;;
  *)
    echo "usage: $0 --check | --find <version>..." >&2
    exit 2
    ;;
esac
