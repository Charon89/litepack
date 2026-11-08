#!/usr/bin/env bash
# Fails when a C library linked into a fuzz binary was compiled without AddressSanitizer / libFuzzer
# instrumentation (R213, D-149 (9)).
#   bash fuzz/check-instrumented.sh [--each-library-somewhere] [--verbose] [--family LIB=ERE ...] BINARY...
#   bash fuzz/check-instrumented.sh --self-test
#
# Method. The verdict comes from the linked binary alone, never from CC/CFLAGS or the workflow's flags:
#  1. `nm -S` lists the sized functions the binary defines. A library is in the binary when a defined function
#     name matches its family (below); the family IS the set of functions judged, there is no probe list to go
#     stale: every defined function of the family is judged.
#  2. `objdump -d` disassembles them (by name, but each result is attributed by ADDRESS, so same-named static
#     functions of other translation units neither merge nor hide). A function counts as instrumented when its
#     body references the sanitizer runtime (__asan_report_*, __asan_stack_*, __sanitizer_cov_trace_*: checked
#     loads and stores, comparisons) or libFuzzer's inline 8-bit counters. The counters are plain increments of
#     bytes in the section __sancov_cntrs, not calls, so the script reads that section's address range from
#     `objdump -h` and counts any operand address ("# <hex> <sym>") that falls inside it, as well as any symbol
#     name containing __sancov. Even a function that is only a tail jump carries a counter increment.
#  3. For each library: k of n family functions are instrumented. FAIL when k = 0 ("NOT instrumented") or when
#     k/n is below min_pct. Plain C has k = 0; code built with -fsanitize=address,fuzzer-no-link has nearly every
#     function instrumented (the exceptions are assembly-written functions, which no compiler instruments).
# A library with no family function defined is "not linked": printed as such, and not a pass. With
# --each-library-somewhere the run fails when a library is not linked into ANY of the binaries given, so a
# stale family cannot turn the whole check into a no-op.
# --family LIB=ERE replaces the built-in families (first use) or adds to them (further uses); the self-test uses it.
# --verbose lists every judged function with its reference count.
# --self-test compiles a tiny C sample twice with clang (with and without the instrumentation), and asserts
# that this script passes the first and fails the second, including for a function that is a bare tail call.
# Output: one line per binary and library, "OK" / "FAIL" / "--" (not linked); exit 1 on any FAIL.
# No pipe in this script has a reader that exits early (grep -q, head): under pipefail that gives the writer SIGPIPE
# and flips a verdict (run 42481555463 lost probes that way).
set -uo pipefail

# LIB=ERE over the defined function names. Prefixes are the libraries' own; Rust symbols are mangled (_ZN..., _R...)
# and never match. zlib has no prefix, so its family is its exported names in zlib's own spelling (an underscore
# suffix such as inflateInit2_, or _z / zError / zlibVersion), which no other library in the graph defines; the
# log names the ones found.
families=(
  "libarchive=^(__)?archive_"
  "zlib=^(zlibVersion|zError|inflateInit_|inflateInit2_|deflateInit_|deflateInit2_|inflateEnd|deflateEnd|adler32_z|crc32_z)$"
  "liblzma=^lzma_"
  "libzstd=^(ZSTD_|HUF_|FSE_)"
  "libsais=^libsais"
)

# The share of a library's family functions that must be instrumented (percent). Code built with the flags is
# instrumented in nearly every function (a counter per function entry at least), plain C in none, so any value
# between is safe; 50 is far from both. The job log prints the observed share of every library and binary.
min_pct=50

# Instrumentation references in a disassembly line.
ref_re='__asan_|__sanitizer_cov|__sancov'

self_test() {
  local tmp rc=0 out i
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
/* Nothing but a tail call: no load, no store, no comparison. */
__attribute__((noinline)) int aaa_wrapper(const unsigned char *p, int n) { return aaa_sample_probe(p, n); }
int main(int argc, char **argv) { return aaa_wrapper((const unsigned char *)argv[0], argc); }
EOF
  # About 200 KB of other function names: a reader that exits at its first match while the writer still has
  # data would send the writer SIGPIPE under pipefail; every function must still be found.
  for i in $(seq 1 12000); do echo "int pad_function_$i(void) { return $i; }"; done >>"$tmp/sample.c"
  clang -g -O1 -fsanitize=address,fuzzer-no-link -c "$tmp/sample.c" -o "$tmp/good.o" || return 2
  clang -g -O1 -c "$tmp/sample.c" -o "$tmp/bad.o" || return 2
  # Both link the AddressSanitizer runtime: only the compiled function bodies differ.
  clang -fsanitize=address,fuzzer-no-link "$tmp/good.o" -o "$tmp/good" || return 2
  clang -fsanitize=address "$tmp/bad.o" -o "$tmp/bad" || return 2

  echo "self-test: what objdump prints for the tail-call wrapper (instrumented, then plain)"
  objdump -d --no-show-raw-insn --disassemble=aaa_wrapper "$tmp/good" | tail -n 8
  objdump -d --no-show-raw-insn --disassemble=aaa_wrapper "$tmp/bad" | tail -n 6

  echo "self-test: instrumented sample must pass, the wrapper through its counter"
  if out="$(bash "$0" --verbose --family 'sample=^aaa_' "$tmp/good" 2>&1)"; then
    echo "$out"
    case "$out" in
      *"OK"*"sample instrumented: 2/2"*) ;;
      *) echo "self-test FAILED: the instrumented sample passed without a 2/2 'instrumented' verdict" >&2; rc=1 ;;
    esac
    if ! grep -Eq 'aaa_wrapper refs=[1-9]' <<<"$out"; then
      echo "self-test FAILED: the instrumented tail-call wrapper shows no counter reference" >&2
      rc=1
    fi
  else
    echo "$out"
    echo "self-test FAILED: the instrumented sample was rejected" >&2
    rc=1
  fi
  echo "self-test: uninstrumented sample must fail, the wrapper included"
  if out="$(bash "$0" --verbose --family 'sample=^aaa_' "$tmp/bad" 2>&1)"; then
    echo "$out"
    echo "self-test FAILED: the uninstrumented sample passed" >&2
    rc=1
  else
    echo "$out"
    case "$out" in
      *"FAIL"*"sample is NOT instrumented: 0/2"*) ;;
      *) echo "self-test FAILED: the uninstrumented sample failed for another reason" >&2; rc=1 ;;
    esac
    if ! grep -Eq 'aaa_wrapper refs=0' <<<"$out"; then
      echo "self-test FAILED: the plain tail-call wrapper was not reported with 0 references" >&2
      rc=1
    fi
  fi
  [ "$rc" = 0 ] && echo "self-test: ok (the check passes instrumented C and fails plain C, tail calls included)"
  return "$rc"
}

each=0
custom=0
verbose=0
bins=()
while [ $# -gt 0 ]; do
  case "$1" in
    --self-test) self_test; exit $? ;;
    --each-library-somewhere) each=1; shift ;;
    --verbose) verbose=1; shift ;;
    --family)
      [ $# -ge 2 ] || { echo "--family needs LIB=ERE" >&2; exit 2; }
      if [ "$custom" = 0 ]; then families=(); custom=1; fi
      families+=("$2")
      shift 2
      ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) bins+=("$1"); shift ;;
  esac
done
if [ "${#bins[@]}" = 0 ]; then
  echo "usage: check-instrumented.sh [--each-library-somewhere] [--verbose] [--family LIB=ERE ...] BINARY... | --self-test" >&2
  exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Awk, run over the disassembly: lo/hi are the address range of the counters section (decimal; 0 0 when none).
# Input 1: lines "address name" of the family; input 2 (stdin): the disassembly. Output: "name seen refs" per address.
cat >"$work/judge.awk" <<'EOF'
function hexv(s,   i, c, v) {
  v = 0
  s = tolower(s)
  for (i = 1; i <= length(s); i++) {
    c = index("0123456789abcdef", substr(s, i, 1)) - 1
    if (c < 0) return -1
    v = v * 16 + c
  }
  return v
}
FNR == NR { if (!($1 in nm)) { nm[$1] = $2; order[++cnt] = $1 } next }
/^[0-9a-f]+ <.*>:$/ { cur = $1; if (cur in nm) { seen[cur] = 1 } else { cur = "" } next }
cur != "" {
  if ($0 ~ re) { r[cur]++; next }
  if (hi > lo && match($0, /# [0-9a-f]+ </)) {
    v = hexv(substr($0, RSTART + 2, RLENGTH - 4))
    if (v >= lo && v < hi) r[cur]++
  }
}
END { for (i = 1; i <= cnt; i++) { a = order[i]; print nm[a], ((a in seen) ? 1 : 0), r[a] + 0 } }
EOF

failed=0
declare -A seen_lib family
for p in "${families[@]}"; do
  seen_lib["${p%%=*}"]=0
  family["${p%%=*}"]="${p#*=}"
done

for bin in "${bins[@]}"; do
  name="$(basename "$bin")"
  if [ ! -f "$bin" ]; then
    echo "FAIL $name: $bin does not exist"
    failed=1
    continue
  fi
  syms="$(nm -S --defined-only "$bin" 2>/dev/null)" || true
  ntext="$(awk '$3 ~ /^[TtWwi]$/ { c++ } END { print c + 0 }' <<<"$syms")"
  if [ "$ntext" = 0 ]; then
    echo "FAIL $name: no function symbols in the binary (stripped?); the check cannot decide"
    failed=1
    continue
  fi
  # The counters section's address range (decimal), from the section headers.
  read -r lo hi < <(objdump -h "$bin" 2>/dev/null | awk '
    function hexv(s,   i, c, v) { v = 0; s = tolower(s); for (i = 1; i <= length(s); i++) { c = index("0123456789abcdef", substr(s, i, 1)) - 1; v = v * 16 + c } return v }
    $2 == "__sancov_cntrs" { printf "%.0f %.0f\n", hexv($4), hexv($4) + hexv($3); found = 1 }
    END { if (!found) print "0 0" }')
  for p in "${families[@]}"; do
    lib="${p%%=*}"
    # Sized, defined functions of the family: "address name".
    awk -v re="${family[$lib]}" 'NF == 4 && $3 ~ /^[TtWwi]$/ && $4 ~ re { print $1, $4 }' <<<"$syms" >"$work/fam.txt"
    if [ ! -s "$work/fam.txt" ]; then
      echo "--   $name: $lib not linked (no defined function matches ${family[$lib]})"
      continue
    fi
    mapfile -t names < <(awk '{ print $2 }' "$work/fam.txt" | sort -u)
    args=("${names[@]/#/--disassemble=}")
    objdump -d --no-show-raw-insn -w "${args[@]}" "$bin" 2>/dev/null \
      | awk -v re="$ref_re" -v lo="$lo" -v hi="$hi" -f "$work/judge.awk" "$work/fam.txt" - >"$work/res.txt"
    judged=0 k=0 unseen=0 zero=0 zero_list="" all_list=""
    while read -r fn sn rf; do
      [ -n "$fn" ] || continue
      if [ "$sn" != 1 ]; then unseen=$((unseen + 1)); continue; fi
      judged=$((judged + 1))
      all_list="$all_list $fn"
      if [ "$verbose" = 1 ]; then echo "       $fn refs=$rf"; fi
      if [ "$rf" -gt 0 ]; then
        k=$((k + 1))
      else
        zero=$((zero + 1))
        if [ "$zero" -le 5 ]; then zero_list="$zero_list $fn"; fi
      fi
    done <"$work/res.txt"
    if [ "$judged" = 0 ]; then
      echo "FAIL $name: $lib has $unseen defined functions but objdump disassembled none of them"
      failed=1
      continue
    fi
    seen_lib["$lib"]=$((${seen_lib["$lib"]} + 1))
    pct=$((k * 100 / judged))
    note=""
    [ "$unseen" = 0 ] || note=" ($unseen not disassembled)"
    [ "$zero" = 0 ] || note="$note; without references:$zero_list"
    [ "$judged" -gt 12 ] || note="$note; functions:$all_list"
    if [ "$k" = 0 ]; then
      echo "FAIL $name: $lib is NOT instrumented: $k/$judged functions reference ASan/coverage code${note} (the C code was compiled without -fsanitize=address,fuzzer-no-link)"
      failed=1
    elif [ "$pct" -lt "$min_pct" ]; then
      echo "FAIL $name: $lib is only partly instrumented: $k/$judged ($pct%, minimum $min_pct%)${note}"
      failed=1
    else
      echo "OK   $name: $lib instrumented: $k/$judged functions ($pct%) reference ASan/coverage code${note}"
    fi
  done
done

if [ "$each" = 1 ]; then
  for p in "${families[@]}"; do
    lib="${p%%=*}"
    if [ "${seen_lib[$lib]}" = 0 ]; then
      echo "FAIL $lib is linked into none of the ${#bins[@]} binaries (family stale, or the library is gone); nothing was checked for it"
      failed=1
    else
      echo "OK   $lib was checked in ${seen_lib[$lib]} of ${#bins[@]} binaries"
    fi
  done
fi

exit "$failed"
