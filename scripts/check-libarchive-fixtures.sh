#!/usr/bin/env bash
# E4-4: the libarchive worker's fixtures folder is documented and its manifest is true.
#   1. every file of crates/lpk-worker-libarchive/tests/fixtures/ is named in its README.md (verbatim);
#   2. every path in manifest.b3 is a path the tests expect (checked by the Rust test
#      `the_manifest_covers_every_expected_file`; here only that the file exists and has three fields).
# Usage, from the repository root: bash scripts/check-libarchive-fixtures.sh
set -euo pipefail
cd "$(dirname "$0")/.."
dir=crates/lpk-worker-libarchive/tests/fixtures
status=0
for f in "$dir"/*; do
  name="$(basename "$f")"
  [[ -f "$f" ]] || continue
  [[ "$name" == README.md ]] && continue
  if ! grep -qF -- "$name" "$dir/README.md"; then
    echo "README.md does not name $name" >&2
    status=1
  fi
done
if awk -F'  ' '!/^#/ && NF != 3 { bad = 1 } END { exit bad }' "$dir/manifest.b3"; then :; else
  echo "manifest.b3 has a malformed line" >&2
  status=1
fi
[[ $status -eq 0 ]] && echo "libarchive fixtures: every file is in the README, manifest well-formed"
exit $status
