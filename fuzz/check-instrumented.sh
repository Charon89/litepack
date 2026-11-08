#!/usr/bin/env bash
# Fails when a C library linked into a fuzz binary was compiled without AddressSanitizer / libFuzzer
# instrumentation (R213, D-149 (9)).
#   bash fuzz/check-instrumented.sh [--each-library-somewhere] [--probe LIB=sym,sym ...] BINARY...
#   bash fuzz/check-instrumented.sh --self-test
#
# Method. The verdict comes from the linked binary alone, never from CC/CFLAGS or the workflow's flags:
#  1. `nm` lists the functions the binary defines. A library is in the binary when a name of its family (archive_*, lzma_*, ZSTD_*/HUF_*/FSE_*, libsais*, zlib's own spellings) is defined; the job log prints the count.
#     Per library a few probe functions are named below (big functions with loops, loads, stores and comparisons,
#     from different source files). A library present by its family with no probe defined is a FAIL (probes stale).
#  2.`objdump -d` disassembles the defined probes. Code compiled with -fsanitize=address,fuzzer-no-link calls
#     __asan_report_* / __asan_stack_malloc_* (every checked load and store) and __sanitizer_cov_trace_*
#     (comparisons) inside the function. Plain C has no such reference.
#  3. The library is instrumented when at least one defined probe has such a reference; it FAILS when probes
#     are defined and none has one. ("At least one", not "all": a tiny probe without memory access or
#     comparison is clean even when instrumented, so "all" would give false failures.)
# A library with no family symbol and no probe defined is "not linked": printed as such, and not a pass. With
# --each-library-somewhere the run fails when a library is not linked into ANY of the binaries given, so a
# probe list that has gone stale cannot turn the whole check into a no-op.
# --probe replaces the built-in probe list (first use) or adds to it (further uses); the self-test uses it.
# --self-test compiles a tiny C sample twice with clang (with and without the instrumentation), and asserts
# that this script passes the first and fails the second.
# Output: one line per binary and library, "OK" / "FAIL" / "--" (not linked); exit 1 on any FAIL.
set -uo pipefail

# LIB=function,function,... (C names; gc-sections may drop the ones a target does not reach).
probes=(
  "libarchive=archive_read_next_header,archive_read_new,__archive_read_ahead,archive_string_append,archive_read_data_block"
  "zlib=inflate,deflate,adler32,crc32,inflate_table"
  "liblzma=lzma_block_header_decode,lzma_index_append,lzma_lz_decoder_init,lzma_crc32,lzma_stream_decoder"
  "libzstd=ZSTD_decompressStream,ZSTD_compressStream2,ZSTD_decompress,ZSTD_compress,FSE_readNCount,HUF_readStats,ZSTD_decompressFrame"
  "libsais=libsais_bwt,libsais_bwt_aux,libsais,libsais_unbwt"
)

# Presence, independent of the probes: LIB=ERE over the defined function names. A library is in the binary when
# at least one name matches; then its probes must be defined too (else the probe list is stale: FAIL, not a pass).
# Families are the libraries' own prefixes; Rust symbols are mangled (_ZN..., _R...) and never match them. zlib has
# no prefix, so its family is its exported names that carry zlib's spelling (an underscore suffix such as
# inflateInit2_, or _z / zError / zlibVersion), which no other library in the graph defines.
families=(
  "libarchive=^(__)?archive_"
  "zlib=^(zlibVersion|zError|inflateInit_|inflateInit2_|deflateInit_|deflateInit2_|inflateEnd|deflateEnd|adler32_z|crc32_z)$"
  "liblzma=^lzma_"
  "libzstd=^(ZSTD_|HUF_|FSE_)"
  "libsais=^libsais"
)

# The grep pattern for instrumentation references in a disassembly.
ref_re='__asan_|__sanitizer_cov'

self_test() {
  local tmp rc=0 out
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  cat >"$tmp/sample.c" <<'EOF'
#include <stddef.h>
__attribute__((noinline)) int aaa_sample_probe(const unsigned char *p, int n) {
  int s = 0;
  for (int i = 0; i < n; i++) {
    if (p[i] > 3) s += p[i];
  }
  return s;
}
int main(int argc, char **argv) { return aaa_sample_probe((const unsigned char *)argv[0], argc); }
EOF
  # About 200 KB of other function names, sorted after the probe: the probe is the first line of the symbol list,
  # so a reader that exits at its first match (grep -q) while the writer still has data to write would send the
  # writer SIGPIPE. The check must still find the probe every time (the earlier pipe-based lookup did not).
  for i in $(seq 1 12000); do echo "int pad_function_$i(void) { return $i; }"; done >>"$tmp/sample.c"
  clang -g -O1 -fsanitize=address,fuzzer-no-link -c "$tmp/sample.c" -o "$tmp/good.o" || return 2
  clang -g -O1 -c "$tmp/sample.c" -o "$tmp/bad.o" || return 2
  # Both link the AddressSanitizer runtime: only the compiled function bodies differ.
  clang -fsanitize=address,fuzzer-no-link "$tmp/good.o" -o "$tmp/good" || return 2
  clang -fsanitize=address "$tmp/bad.o" -o "$tmp/bad" || return 2

  echo "self-test: instrumented sample must pass"
  if out="$(bash "$0" --probe sample=aaa_sample_probe --family "sample=^aaa_sample_probe$" "$tmp/good" 2>&1)"; then
    echo "$out"
    case "$out" in
      *"OK"*"sample instrumented"*) ;;
      *) echo "self-test FAILED: the instrumented sample passed without an 'instrumented' verdict" >&2; rc=1 ;;
    esac
  else
    echo "$out"
    echo "self-test FAILED: the instrumented sample was rejected" >&2
    rc=1
  fi
  echo "self-test: uninstrumented sample must fail"
  if out="$(bash "$0" --probe sample=aaa_sample_probe --family "sample=^aaa_sample_probe$" "$tmp/bad" 2>&1)"; then
    echo "$out"
    echo "self-test FAILED: the uninstrumented sample passed" >&2
    rc=1
  else
    echo "$out"
    case "$out" in
      *"FAIL"*"sample is NOT instrumented"*) ;;
      *) echo "self-test FAILED: the uninstrumented sample failed for another reason" >&2; rc=1 ;;
    esac
  fi
  [ "$rc" = 0 ] && echo "self-test: ok (the check passes instrumented C and fails plain C)"
  return "$rc"
}

each=0
custom=0
bins=()
while [ $# -gt 0 ]; do
  case "$1" in
    --self-test) self_test; exit $? ;;
    --each-library-somewhere) each=1; shift ;;
    --probe)
      [ $# -ge 2 ] || { echo "--probe needs LIB=sym,sym" >&2; exit 2; }
      if [ "$custom" = 0 ]; then probes=(); families=(); custom=1; fi
      probes+=("$2")
      shift 2
      ;;
    --family)
      [ $# -ge 2 ] || { echo "--family needs LIB=ERE" >&2; exit 2; }
      if [ "$custom" = 0 ]; then probes=(); families=(); custom=1; fi
      families+=("$2")
      shift 2
      ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) bins+=("$1"); shift ;;
  esac
done
if [ "${#bins[@]}" = 0 ]; then
  echo "usage: check-instrumented.sh [--each-library-somewhere] [--probe LIB=sym,sym] BINARY... | --self-test" >&2
  exit 2
fi

failed=0
declare -A seen
declare -A family
for p in "${probes[@]}"; do seen["${p%%=*}"]=0; done
for p in "${families[@]}"; do family["${p%%=*}"]="${p#*=}"; done
for p in "${probes[@]}"; do
  if [ -z "${family[${p%%=*}]:-}" ]; then
    echo "no --family for ${p%%=*}" >&2
    exit 2
  fi
done

for bin in "${bins[@]}"; do
  name="$(basename "$bin")"
  if [ ! -f "$bin" ]; then
    echo "FAIL $name: $bin does not exist"
    failed=1
    continue
  fi
  # Defined functions: text symbols (T, t, W, w, i for ifunc).
  if ! defined="$(nm --defined-only "$bin" 2>/dev/null | awk '$2 ~ /^[TtWwi]$/ { print $3 }' | sort -u)"; then
    defined=""
  fi
  if [ -z "$defined" ]; then
    echo "FAIL $name: no function symbols in the binary (stripped?); the check cannot decide"
    failed=1
    continue
  fi
  unset def
  declare -A def
  while IFS= read -r s; do def["$s"]=1; done <<<"$defined"
  for p in "${probes[@]}"; do
    lib="${p%%=*}"
    # Lookups in the associative array `def` and a here-string for the family count: no pipe whose reader
    # can exit early, so no SIGPIPE under pipefail (an earlier `printf | grep -q` lost verdicts that way).
    fam_n="$(grep -cE "${family[$lib]}" <<<"$defined" || true)"
    echo "     $name: $lib: ${fam_n:-0} defined function symbols match ${family[$lib]}"
    present=()
    for f in $(echo "${p#*=}" | tr ',' ' '); do
      if [ -n "${def[$f]:-}" ]; then present+=("$f"); fi
    done
    if [ "${fam_n:-0}" = 0 ] && [ "${#present[@]}" = 0 ]; then
      echo "--   $name: $lib not linked (no symbol of its family is defined)"
      continue
    fi
    if [ "${#present[@]}" = 0 ]; then
      echo "FAIL $name: $lib is linked (${fam_n} family symbols) but none of its probes ${p#*=} is defined: probe list stale, nothing was checked"
      failed=1
      continue
    fi
    seen["$lib"]=$((${seen["$lib"]} + 1))
    args=()
    for f in "${present[@]}"; do args+=("--disassemble=$f"); done
    # Per function: the number of lines that reference the sanitizer runtime.
    counts="$(objdump -d --no-show-raw-insn -w "${args[@]}" "$bin" 2>/dev/null | awk -v re="$ref_re" '
      /^[0-9a-f]+ <.*>:$/ { cur = $2; gsub(/[<>:]/, "", cur); if (!(cur in n)) n[cur] = 0; next }
      cur != "" && $0 ~ re { n[cur]++ }
      END { for (k in n) print k, n[k] }' | sort)"
    total=0
    detail=""
    while read -r fn cnt; do
      [ -n "$fn" ] || continue
      total=$((total + cnt))
      detail="$detail $fn=$cnt"
    done <<<"$counts"
    if [ -z "$detail" ]; then
      echo "FAIL $name: $lib: ${present[*]} defined but objdump could not disassemble them"
      failed=1
    elif [ "$total" -gt 0 ]; then
      echo "OK   $name: $lib instrumented (sanitizer references per probe:$detail)"
    else
      echo "FAIL $name: $lib is NOT instrumented: probes${detail} have no __asan_/__sanitizer_cov references (the C code was compiled without -fsanitize=address,fuzzer-no-link)"
      failed=1
    fi
  done
done

if [ "$each" = 1 ]; then
  for p in "${probes[@]}"; do
    lib="${p%%=*}"
    if [ "${seen[$lib]}" = 0 ]; then
      echo "FAIL $lib is linked into none of the ${#bins[@]} binaries (probe list stale, or the library is gone); nothing was checked for it"
      failed=1
    else
      echo "OK   $lib was checked in ${seen[$lib]} of ${#bins[@]} binaries"
    fi
  done
fi

exit "$failed"
