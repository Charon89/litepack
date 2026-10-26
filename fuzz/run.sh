#!/usr/bin/env bash
# Run every fuzz target (or the ones named) for a bounded time.
#   bash fuzz/run.sh [-t SECONDS] [-m] [target ...]
# Needs Linux (or WSL), a nightly toolchain and cargo-fuzz; see fuzz/README.md.
# The corpus grows under fuzz/corpus/<target> (git-ignored); fuzz/seeds/<target>, the committed
# test vectors and (for the foreign_* targets) the foreign readers' fixtures are read as extra
# seed directories. The full log of each target is fuzz/artifacts/logs/<target>.log and a crash
# input is saved under fuzz/artifacts/<target>/; a failing target does not stop the others, the
# script lists the failures and exits 1 at the end.
#   -m  after the run, minimise each target's corpus (libFuzzer's -merge=1) in place.
# With LPK_FUZZ_BIN_DIR set (CI), the prebuilt binaries in that directory are run directly
# instead of `cargo fuzz run`, with the same flags and seed directories.
# The list `all` below must equal fuzz/fuzz_targets/*.rs and fuzz/Cargo.toml's [[bin]] names:
# .github/workflows/fuzz.yml's `targets` job fails when it does not.
set -uo pipefail
cd "$(dirname "$0")"

secs=60
minimise=0
while [ $# -gt 0 ]; do
  case "$1" in
    -t) secs="$2"; shift 2 ;;
    -m) minimise=1; shift ;;
    *) break ;;
  esac
done

all="frame_read header_read varint_read entry_table chunk_table records_table index_parse \
trailer_read_tail block_header_and_decode zstd_decode lzma_decode recovery_frame key_slot \
archive_open archive_mutate deflate_walk deflate_recreate bwt_decode bwt_roundtrip delta_patch_decode png_filter_decode worker_proto \
foreign_zip foreign_7z foreign_tar jpeg_reconstruct jpeg_peel deflate_analyse png_filter_undo"
if [ "${1:-}" = "--list" ]; then
  echo "$all" | tr -s ' ' '\n'
  exit 0
fi
targets="${*:-$all}"
toolchain="${LPK_FUZZ_TOOLCHAIN:-nightly}"
bin_dir="${LPK_FUZZ_BIN_DIR:-}"
vectors="../crates/lpk-format/tests/vectors"
foreign_fixtures="../workers/lpk-foreign/tests/fixtures"
flags=(-max_total_time="$secs" -rss_limit_mb=2048 -timeout=30 -print_final_stats=1)
failed=""

# run TARGET ARGS...: libFuzzer with ARGS (flags first, then directories).
run() {
  local t="$1"
  shift
  if [ -n "$bin_dir" ]; then
    "$bin_dir/$t" -artifact_prefix="artifacts/$t/" "$@"
  else
    # cargo fuzz puts its own -artifact_prefix (fuzz/artifacts/<target>/) before these.
    local dirs=() opts=()
    for a in "$@"; do
      case "$a" in -*) opts+=("$a") ;; *) dirs+=("$a") ;; esac
    done
    cargo "+$toolchain" fuzz run "$t" "${dirs[@]}" -- "${opts[@]}"
  fi
}

mkdir -p artifacts/logs
for t in $targets; do
  mkdir -p "corpus/$t" "artifacts/$t"
  extra=()
  # A target's own seeds are optional (libFuzzer exits on a missing directory).
  if [ -d "seeds/$t" ]; then extra+=("seeds/$t"); fi
  # archive_mutate reads the vectors itself; its input is a mutation list, not an archive.
  if [ "$t" != "archive_mutate" ]; then extra+=("$vectors"); fi
  case "$t" in foreign_*) extra+=("$foreign_fixtures") ;; esac
  echo "== $t (${secs}s)"
  if run "$t" "${flags[@]}" "corpus/$t" "${extra[@]}" >"artifacts/logs/$t.log" 2>&1; then
    grep -E "stat::number_of_executed_units|Done [0-9]+ runs" "artifacts/logs/$t.log" | tail -n 2
  else
    echo "FAILED: $t (log: fuzz/artifacts/logs/$t.log)" >&2
    tail -n 30 "artifacts/logs/$t.log" >&2
    failed="$failed $t"
    continue
  fi
  if [ "$minimise" = 1 ]; then
    rm -rf "corpus/$t.min"
    mkdir -p "corpus/$t.min"
    if run "$t" -merge=1 -rss_limit_mb=2048 -timeout=30 "corpus/$t.min" "corpus/$t" \
      >"artifacts/logs/$t.merge.log" 2>&1; then
      echo "minimised: $(find "corpus/$t" -type f | wc -l) -> $(find "corpus/$t.min" -type f | wc -l) inputs"
      rm -rf "corpus/$t"
      mv "corpus/$t.min" "corpus/$t"
    else
      # A failed merge keeps the corpus as it was; the run's result stands.
      echo "minimise failed: $t (log: fuzz/artifacts/logs/$t.merge.log)" >&2
      rm -rf "corpus/$t.min"
    fi
  fi
done

if [ -n "$failed" ]; then
  echo "failing targets:$failed" >&2
  exit 1
fi
