#!/usr/bin/env bash
# Run every fuzz target (or the ones named) for a bounded time.
#   fuzz/run.sh [-t SECONDS] [target ...]
# Needs Linux (or WSL), a nightly toolchain and cargo-fuzz; see fuzz/README.md.
# The corpus grows under fuzz/corpus/<target> (git-ignored); fuzz/seeds/<target> and the
# committed test vectors are read as extra seed directories.
set -euo pipefail
cd "$(dirname "$0")"

secs=60
if [ "${1:-}" = "-t" ]; then
  secs="$2"
  shift 2
fi

all="frame_read header_read varint_read entry_table chunk_table records_table index_parse \
trailer_read_tail block_header_and_decode zstd_decode lzma_decode recovery_frame key_slot \
archive_open archive_mutate"
targets="${*:-$all}"
toolchain="${LPK_FUZZ_TOOLCHAIN:-nightly}"
vectors="../crates/lpk-format/tests/vectors"

for t in $targets; do
  mkdir -p "corpus/$t"
  extra=""
  # archive_mutate reads the vectors itself; its input is a mutation list, not an archive.
  if [ "$t" != "archive_mutate" ]; then extra="$vectors"; fi
  echo "== $t (${secs}s)"
  cargo "+$toolchain" fuzz run "$t" "corpus/$t" "seeds/$t" $extra -- \
    -max_total_time="$secs" -rss_limit_mb=2048 -timeout=30 -print_final_stats=1 2>&1 | tail -n 12
  # tail hides the exit code; libFuzzer leaves a crash file under artifacts/<target>.
  if ls "artifacts/$t"/crash-* "artifacts/$t"/oom-* "artifacts/$t"/timeout-* >/dev/null 2>&1; then
    echo "CRASH in $t: see fuzz/artifacts/$t" >&2
    exit 1
  fi
done
