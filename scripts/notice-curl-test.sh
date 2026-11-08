#!/usr/bin/env bash
# Keeps the curl options of the two notice pin scripts identical (R287): the `curl_limits=(...)` block of
# scripts/nsis-notice.sh and scripts/webview2-notice.sh must be the same text, must carry a stall limit and a
# per-attempt time limit, and each script must pass it to curl. No network. From the repository root:
#   bash scripts/notice-curl-test.sh
set -euo pipefail
cd "$(dirname "$0")/.."

block() {
  awk '/^curl_limits=\(/ { on = 1 } on { print } on && /\)$/ { exit }' "$1"
}

a="$(block scripts/nsis-notice.sh)"
b="$(block scripts/webview2-notice.sh)"
fail=0
if [ -z "$a" ] || [ -z "$b" ]; then
  echo "FAIL: a curl_limits=( block is missing (nsis: ${#a} bytes, webview2: ${#b} bytes)" >&2
  exit 1
fi
if [ "$a" != "$b" ]; then
  echo "FAIL: the curl_limits blocks differ" >&2
  diff <(printf '%s\n' "$a") <(printf '%s\n' "$b") >&2 || true
  fail=1
fi
for opt in --speed-limit --speed-time --max-time --connect-timeout --retry-max-time; do
  case "$a" in
    *"$opt "*) ;;
    *) echo "FAIL: curl_limits lacks $opt" >&2; fail=1 ;;
  esac
done
for s in scripts/nsis-notice.sh scripts/webview2-notice.sh; do
  if ! grep -q 'curl -fsSL "${curl_limits\[@\]}"' "$s"; then
    echo "FAIL: $s does not pass curl_limits to curl" >&2
    fail=1
  fi
  # No other curl call may bypass the limits.
  if [ -n "$(grep -E '^[^#]*\bcurl ' "$s" | grep -v 'curl_limits')" ]; then
    echo "FAIL: $s runs curl without curl_limits" >&2
    fail=1
  fi
done
[ "$fail" = 0 ] && echo "notice-curl-test: ok (both scripts use the same curl_limits: $(echo $a))"
exit "$fail"
