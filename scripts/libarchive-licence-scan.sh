#!/usr/bin/env bash
# Scans the licence and copyright headers of every vendored libarchive .c and .h file (E4-4, R207) and prints one
# line per file: its name, then the notice classes it carries. Usage, from the repository root:
#   scripts/libarchive-licence-scan.sh            print the table
#   scripts/libarchive-licence-scan.sh --check    fail when it differs from vendor/libarchive-*/licence-scan.txt
# The classes: bsd2 (libarchive's two-clause BSD text), regents (University of California, with clause 3 dropped),
# openbsd-isc (arc4random's ISC notice), postgresql (the PostgreSQL / UC Regents 1994 notice), cc0-openssl-apache
# (BLAKE2's triple licence), public-domain (Igor Pavlov's PPMd). A file with none of them is "unclassified" and
# fails --check, so a new notice cannot enter unseen. docs/LICENSING.md names each class with its files, and
# THIRD-PARTY-NOTICES reproduces each text.
set -euo pipefail
cd "$(dirname "$0")/.."
dir="$(ls -d vendor/libarchive-*/ | head -n 1)"
dir="${dir%/}"

scan() {
  for f in "$dir"/libarchive/*.c "$dir"/libarchive/*.h; do
    classes=()
    grep -qE 'Redistribution and use in source and binary forms|SPDX-License-Identifier: BSD-2-Clause' "$f" && classes+=(bsd2)
    grep -q 'The Regents of the University of California' "$f" && classes+=(regents)
    grep -q 'OpenBSD: arc4random' "$f" && classes+=(openbsd-isc)
    grep -q 'PostgreSQL Database Management System' "$f" && classes+=(postgresql)
    grep -q 'CC0 1.0 Universal' "$f" && classes+=(cc0-openssl-apache)
    grep -qi 'public domain' "$f" && classes+=(public-domain)
    if [[ ${#classes[@]} -eq 0 ]]; then classes+=(unclassified); fi
    echo "$(basename "$f") ${classes[*]}"
  done
}

if [[ "${1:-}" == "--check" ]]; then
  tmp="$(mktemp)"
  trap 'rm -f "$tmp"' EXIT
  scan > "$tmp"
  if grep -n unclassified "$tmp"; then
    echo "a vendored libarchive file carries no known notice: read it, then extend this script, docs/LICENSING.md and about.hbs" >&2
    exit 1
  fi
  if ! diff -u "$dir/licence-scan.txt" "$tmp"; then
    echo "the licence scan changed: reconcile docs/LICENSING.md and about.hbs, then regenerate $dir/licence-scan.txt" >&2
    exit 1
  fi
  echo "libarchive licence scan: every file classified, unchanged"
else
  scan
fi
