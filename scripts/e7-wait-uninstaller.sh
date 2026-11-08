#!/usr/bin/env bash
# E7-5 (CI's `installer` job only): wait until a silent NSIS uninstall has really finished. `uninstall.exe /S` copies
# itself to %TEMP% (as Un.exe, seen on CI, or Un_A.exe), starts that copy and exits at once; the copy does the work. This waits
# up to five minutes until no Un.exe or Un_<letter>.exe process is left and <install>\uninstall.exe is gone, then lists what remains.
#   scripts/e7-wait-uninstaller.sh <install folder>
set -euo pipefail
inst="$1"
running() { tasklist /FI "IMAGENAME eq Un*" /FO CSV /NH 2>/dev/null | grep -qiE '^"Un(_[A-Z])?.exe"'; }
for _ in $(seq 1 300); do
  if ! running && [[ ! -e "$inst/uninstall.exe" ]]; then
    echo "the uninstaller has finished"
    ls -la "$inst" 2>/dev/null || echo "install folder gone: $inst"
    exit 0
  fi
  sleep 1
done
echo "the uninstaller did not finish in five minutes" >&2
tasklist /FI "IMAGENAME eq Un*" >&2 || true
ls -la "$inst" >&2 || true
exit 1
