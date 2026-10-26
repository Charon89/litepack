#!/usr/bin/env bash
# E8-7: `unsafe` only where docs/UNSAFE.md allows it.
#   bash scripts/check-unsafe.sh              check this repository
#   bash scripts/check-unsafe.sh --self-test  check that the checks fail on planted cases
#   bash scripts/check-unsafe.sh --root DIR   check another tree (DIR/docs/UNSAFE.md is its inventory)
# Every package of the tree (both workspaces, fuzz/ and any later one; vendor/, target/ and the
# corpus are skipped) is either in the inventory or:
#   - forbids `unsafe_code`: `[lints] workspace = true` under a workspace whose
#     `[workspace.lints.rust]` sets `unsafe_code = "forbid"`, or its own `[lints.rust]`
#     `unsafe_code = "forbid"`, or `#![forbid(unsafe_code)]` in every crate root under src/;
#   - holds no `unsafe` keyword (block, fn, impl, trait, extern, attribute) and no
#     `allow(unsafe_code)` (nor warn/expect) in any .rs file, and no `unsafe_code = "allow"` (nor
#     warn/deny/expect) in its Cargo.toml.
# An inventoried crate may use `unsafe` only in the modules its row lists; a "reserved" row names a
# crate not built yet (it fails once the crate exists, so that its row is filled in with it).
# The lexical check blanks comments (nested block comments included), string literals (raw,
# byte, C) and char literals before looking for the keyword, so `unsafe` in prose, in a message
# or in an identifier (`unsafe_path`) is not a finding.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

# Prints `file:line: text` for each `unsafe` keyword or allow(unsafe_code) attribute in the
# given .rs files, after blanking comments and literals (newlines kept, so line numbers hold).
scan_rs() {
  [ "$#" -gt 0 ] || return 0
  perl - "$@" <<'PERL'
use strict;
use warnings;
binmode STDOUT, ':encoding(UTF-8)';
for my $f (@ARGV) {
  open my $fh, '<:raw:encoding(UTF-8)', $f or die "$f: $!";
  local $/;
  my $s = <$fh>;
  close $fh;
  $s =~ s{
      //[^\n]*
    | (?<bc> /\* (?: [^/*]++ | /(?!\*) | \*(?!/) | (?&bc) )*+ \*/ )
    | (?<![A-Za-z0-9_]) [bc]? r (?<h>\#*) " .*? " \k<h>
    | (?<![A-Za-z0-9_]) [bc]? " (?: [^"\\]++ | \\. )*+ "
    | (?<![A-Za-z0-9_]) b? ' (?: \\ (?: u\{[0-9A-Fa-f]{1,6}\} | x[0-9A-Fa-f]{2} | . ) | [^'\\\n] ) '
  }{ (my $t = $&) =~ s/[^\n]/ /g; $t }gsex;
  my $n = 0;
  for my $line (split /\n/, $s, -1) {
    $n++;
    (my $show = $line) =~ s/^\s+|\s+$//g;
    if ($line =~ /\bunsafe\b/) {
      print "$f:$n: unsafe keyword: $show\n";
    } elsif ($line =~ /#\s*\[\s*(?:no_mangle|export_name|link_section)\b/) {
      print "$f:$n: unsafe attribute: $show\n";
    } elsif ($line =~ /\b(?:allow|warn|expect)\s*\([^)]*\bunsafe_code\b/) {
      print "$f:$n: unsafe_code allowed: $show\n";
    }
  }
}
PERL
}

# True when the crate root $1 carries #![forbid(unsafe_code)] outside comments and literals.
root_forbids() {
  perl - "$1" <<'PERL'
use strict;
use warnings;
open my $fh, '<:raw:encoding(UTF-8)', $ARGV[0] or die "$ARGV[0]: $!";
local $/;
my $s = <$fh>;
$s =~ s{ //[^\n]* | (?<bc> /\* (?: [^/*]++ | /(?!\*) | \*(?!/) | (?&bc) )*+ \*/ ) }{ }gsx;
exit($s =~ /#!\s*\[\s*forbid\s*\([^)]*\bunsafe_code\b/ ? 0 : 1);
PERL
}

# Prints the lines of TOML table $2 (exact header) in file $1, comments stripped.
toml_table() {
  awk -v want="$2" '
    { sub(/[[:space:]]*#.*$/, "") }
    /^[[:space:]]*\[/ { h = $0; gsub(/[[:space:]]/, "", h); cur = h; next }
    cur == "[" want "]" { print }
  ' "$1"
}

# True when file $1 has the TOML table header [$2] (comments and spaces ignored, so
# `[package] # note` counts); false, with an error, when the file cannot be read.
has_table() {
  [ -r "$1" ] || { echo "error: cannot read $1" >&2; return 1; }
  awk -v want="[$2]" '
    { sub(/[[:space:]]*#.*$/, ""); h = $0; gsub(/[[:space:]]/, "", h) }
    h == want { found = 1 }
    END { exit found ? 0 : 1 }
  ' "$1"
}

# True when TOML table $2 of file $1 sets unsafe_code to forbid (plain or `{ level = "forbid" }`).
table_forbids() {
  toml_table "$1" "$2" | grep -qE '^[[:space:]]*unsafe_code[[:space:]]*=[[:space:]]*("forbid"|\{[^}]*level[[:space:]]*=[[:space:]]*"forbid")'
}

# The Cargo.toml of the workspace governing the package in directory $1 (itself, or the nearest
# ancestor manifest with a [workspace] table, up to $2).
workspace_of() {
  local d="$1" top="$2"
  while :; do
    if [ -f "$d/Cargo.toml" ] && has_table "$d/Cargo.toml" workspace; then
      echo "$d/Cargo.toml"
      return 0
    fi
    [ "$d" = "$top" ] && return 1
    d="$(dirname "$d")"
  done
}

# The inventory rows of $1 (the table between the inventory markers) as `crate<TAB>dir<TAB>state<TAB>module module ...`.
inventory_rows() {
  [ -f "$1" ] || { echo "error: no inventory at $1" >&2; return 1; }
  awk -F'|' '
    /<!-- inventory:begin -->/ { on = 1; next }
    /<!-- inventory:end -->/ { on = 0 }
    on && /^\|[[:space:]]*`[a-z0-9_-]+`[[:space:]]*\|/ {
      crate = $2; dir = $3; state = $4; mods = $5
      gsub(/[[:space:]`]/, "", crate); gsub(/[[:space:]`]/, "", dir); gsub(/[[:space:]`]/, "", state)
      out = ""
      while (match(mods, /`[^`]+`/)) {
        out = out " " substr(mods, RSTART + 1, RLENGTH - 2)
        mods = substr(mods, RSTART + RLENGTH)
      }
      print crate "\t" dir "\t" state "\t" substr(out, 2)
    }
  ' "$1"
}

# Checks the tree at $1; prints every finding and returns 1 if there is one.
check_tree() {
  local top="$1" fail=0 inv="$1/docs/UNSAFE.md"
  local rows
  rows="$(inventory_rows "$inv")" || return 1
  [ -n "$rows" ] || { echo "error: $inv lists no crate" >&2; return 1; }

  # Every Cargo.toml of the tree (paths may hold spaces: one per line, never word-split).
  local all_manifests
  all_manifests="$(cd "$top" && find . \( -name target -o -name vendor -o -name .git -o -name .claude \
    -o -name node_modules -o -path ./bench/corpus -o -path ./bench/tmp \) -prune -o \
    -name Cargo.toml -print | sed 's#^\./##' | LC_ALL=C sort)"

  # The packages, as cargo itself sees them: `cargo metadata` of every workspace manifest (the
  # root's, and each manifest with a [workspace] table: workers/, fuzz/), name<TAB>relative path.
  local top_m="$top" m packages="" meta
  if command -v cygpath > /dev/null 2>&1; then top_m="$(cygpath -m "$top")"; fi
  while IFS= read -r m; do
    [ -n "$m" ] || continue
    [ "$m" = "Cargo.toml" ] || has_table "$top/$m" workspace || continue
    if ! meta="$(cargo metadata --no-deps --format-version 1 --offline --manifest-path "$top/$m" 2>&1)"; then
      echo "error: cargo metadata failed for $m: $(printf '%s' "$meta" | tail -n 1)"
      return 1
    fi
    packages="$packages$(printf '%s' "$meta" | perl -MJSON::PP -e '
      local $/; my $j = decode_json(<STDIN>);
      for my $p (@{$j->{packages}}) { (my $mp = $p->{manifest_path}) =~ s{\\}{/}g; print "$p->{name}\t$mp\n" }
    ')"$'\n'
  done <<< "$all_manifests"

  local pkg_list="" name abs rel
  while IFS=$'\t' read -r name abs; do
    [ -n "$name" ] || continue
    # Case-insensitive prefix (Windows drive letters); a package outside the tree is not ours.
    local lt="${top_m,,}" la="${abs,,}"
    [ "${la:0:$((${#lt} + 1))}" = "$lt/" ] || continue
    rel="${abs:$((${#lt} + 1))}"
    case "$rel" in vendor/*|*/vendor/*) continue ;; esac
    pkg_list="$pkg_list$name"$'\t'"$rel"$'\n'
  done <<< "$(printf '%s' "$packages" | LC_ALL=C sort -u)"

  # A manifest that is neither a package cargo reports nor a workspace-only manifest is a
  # package hidden from cargo metadata (or unreadable): refused, never skipped.
  while IFS= read -r m; do
    [ -n "$m" ] || continue
    if printf '%s' "$pkg_list" | cut -f2 | grep -qxF -- "$m"; then continue; fi
    if [ -r "$top/$m" ] && has_table "$top/$m" workspace && ! has_table "$top/$m" package; then continue; fi
    echo "error: $m is not a package of any workspace cargo metadata reports, nor a workspace-only manifest: cannot classify it"
    fail=1
  done <<< "$all_manifests"

  local seen_inv="" pkg_dir row state mods ws hits roots r
  while IFS=$'\t' read -r name m; do
    [ -n "$name" ] || continue
    pkg_dir="$(dirname "$m")"
    row="$(printf '%s\n' "$rows" | awk -F'\t' -v n="$name" '$1 == n' | head -n 1)"
    local files=()
    mapfile -t files < <(cd "$top" && find "$pkg_dir" -name target -prune -o -name '*.rs' -print | LC_ALL=C sort)
    if [ -n "$row" ]; then
      state="$(printf '%s' "$row" | cut -f3)"
      mods="$(printf '%s' "$row" | cut -f4)"
      seen_inv="$seen_inv $name"
      if [ "$(printf '%s' "$row" | cut -f2)" != "$pkg_dir" ]; then
        echo "error: $name is at $pkg_dir, docs/UNSAFE.md says $(printf '%s' "$row" | cut -f2)"
        fail=1
      fi
      if [ "$state" != "built" ]; then
        echo "error: $name exists but its docs/UNSAFE.md row is '$state': list its modules and mark it built"
        fail=1
        continue
      fi
      # Confinement: findings only in the listed modules.
      hits="$(cd "$top" && scan_rs "${files[@]}")" || return 1
      while IFS= read -r h; do
        [ -n "$h" ] || continue
        local f="${h%%:*}" allowed=0 mod
        for mod in $mods; do
          [ "$f" = "$pkg_dir/$mod" ] && allowed=1
        done
        if [ "$allowed" = 0 ]; then
          echo "error: $h (not a module docs/UNSAFE.md lists for $name)"
          fail=1
        fi
      done <<< "$hits"
      continue
    fi

    # Not inventoried: the lint must forbid unsafe_code ...
    local lint_ok=0
    if toml_table "$top/$m" lints | grep -qE '^[[:space:]]*workspace[[:space:]]*=[[:space:]]*true'; then
      ws="$(workspace_of "$top/$pkg_dir" "$top")" || ws=""
      if [ -n "$ws" ] && table_forbids "$ws" workspace.lints.rust; then
        lint_ok=1
      else
        echo "error: $m inherits the workspace lints, but ${ws:-its workspace} does not set unsafe_code = \"forbid\""
        fail=1
      fi
    elif table_forbids "$top/$m" lints.rust; then
      lint_ok=1
    else
      roots="$(cd "$top" && {
        find "$pkg_dir/src" -maxdepth 1 \( -name lib.rs -o -name main.rs \) -print
        if [ -d "$pkg_dir/src/bin" ]; then find "$pkg_dir/src/bin" -maxdepth 1 -name '*.rs' -print; fi
      } 2>/dev/null || true)"
      if [ -n "$roots" ]; then
        lint_ok=1
        while IFS= read -r r; do
          [ -n "$r" ] && { root_forbids "$top/$r" || lint_ok=0; }
        done <<< "$roots"
      fi
    fi
    if [ "$lint_ok" = 0 ]; then
      echo "error: $name ($m) does not forbid unsafe_code: inherit the workspace lints ([lints] workspace = true) or add it to docs/UNSAFE.md"
      fail=1
    fi
    if toml_table "$top/$m" lints.rust | grep -qE '^[[:space:]]*unsafe_code[[:space:]]*=[[:space:]]*("(allow|warn|deny|expect)"|\{[^}]*level[[:space:]]*=[[:space:]]*"(allow|warn|deny|expect)")'; then
      echo "error: $m relaxes unsafe_code"
      fail=1
    fi
    # ... and no source may hold the keyword or relax the lint.
    hits="$(cd "$top" && scan_rs "${files[@]}")" || return 1
    if [ -n "$hits" ]; then
      while IFS= read -r h; do echo "error: $h ($name is not in docs/UNSAFE.md)"; done <<< "$hits"
      fail=1
    fi
  done <<< "$pkg_list"

  # Every built row names a crate that exists.
  while IFS=$'\t' read -r name _ state _; do
    [ "$state" = "built" ] || continue
    case " $seen_inv " in
      *" $name "*) ;;
      *) echo "error: docs/UNSAFE.md lists $name as built, but no package of that name exists"; fail=1 ;;
    esac
  done <<< "$rows"
  return "$fail"
}

# The self-test: a clean fixture passes, and each planted case fails.
self_test() {
  t="$(mktemp -d)"
  trap 'rm -rf "$t"' EXIT
  local base="$t/base"
  mkdir -p "$base/docs" "$base/crates/good/src/bin" "$base/crates/x-sys/src" "$base/other/src"
  cat > "$base/docs/UNSAFE.md" <<'EOF'
| `not-a-row` | `outside` | built | `the markers` | ignored |
<!-- inventory:begin -->
| Crate | Directory | State | Modules | Why |
|---|---|---|---|---|
| `x-sys` | `crates/x-sys` | built | `src/ffi.rs` | fixture |
| `y-shell` | `crates/y-shell` | reserved | `src/com.rs` | fixture |
<!-- inventory:end -->
EOF
  cat > "$base/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.lints.rust]
unsafe_code = "forbid"   # a comment
EOF
  cat > "$base/crates/good/Cargo.toml" <<'EOF'
[package]
name = "good"
version = "0.0.0"
edition = "2021"

[lints]
workspace = true
EOF
  cat > "$base/crates/good/src/lib.rs" <<'EOF'
//! Mentions of unsafe { } and unsafe fn in a doc comment.
/* unsafe impl Send for X {} /* nested unsafe */ still a comment: unsafe */
pub fn unsafe_path(p: &str) -> bool {
    let a = "unsafe { x }";
    let b = r#"unsafe fn "quoted" inside"#;
    let c = b"unsafe";
    let d = '"';
    let e = '\'';
    let _l: &'static str = "it's unsafe";
    p.is_empty() && a.len() + b.len() + c.len() > 0 && d != e
}
EOF
  printf 'fn main() {} // unsafe\n' > "$base/crates/good/src/bin/tool.rs"
  cat > "$base/crates/x-sys/Cargo.toml" <<'EOF'
[package]
name = "x-sys"
version = "0.0.0"
edition = "2021"

[lints.rust]
unsafe_code = "allow"
EOF
  printf 'mod ffi;\n' > "$base/crates/x-sys/src/lib.rs"
  printf 'pub fn f() { unsafe { core::hint::unreachable_unchecked() } }\n' > "$base/crates/x-sys/src/ffi.rs"
  cat > "$base/other/Cargo.toml" <<'EOF'
[package]  # a comment after the header
name = "other"
version = "0.0.0"
edition = "2021"

[workspace]
EOF
  printf '#![forbid(unsafe_code)]\nfn main() {}\n' > "$base/other/src/main.rs"

  local fail=0
  if ! out="$(check_tree "$base" 2>&1)"; then
    echo "self-test: the clean fixture was refused:" >&2
    echo "$out" >&2
    fail=1
  fi

  # Each case: a name and a command run inside a fresh copy of the fixture.
  local cases=(
    "unsafe-block|printf 'pub fn g() { unsafe { } }\n' >> crates/good/src/lib.rs"
    "unsafe-fn|printf 'pub unsafe fn g() {}\n' >> crates/good/src/lib.rs"
    "unsafe-impl|printf 'struct S; unsafe impl Send for S {}\n' >> crates/good/src/lib.rs"
    "unsafe-in-test|mkdir -p crates/good/tests && printf 'fn t() { unsafe {} }\n' > crates/good/tests/t.rs"
    "allow-attribute|printf '#[allow(unsafe_code)]\nfn g() {}\n' >> crates/good/src/lib.rs"
    "no-lint-inheritance|sed -i '/^\[lints\]/,\$d' crates/good/Cargo.toml"
    "lint-relaxed|sed -i '/^\[lints\]/,\$d' crates/good/Cargo.toml && printf '[lints.rust]\nunsafe_code = \"allow\"\n' >> crates/good/Cargo.toml"
    "workspace-not-forbidding|sed -i 's/\"forbid\"/\"warn\"/' Cargo.toml"
    "root-without-forbid|printf 'fn main() {}\n' > other/src/main.rs"
    "sys-outside-its-modules|printf 'pub fn h() { unsafe {} }\n' >> crates/x-sys/src/lib.rs"
    "sys-unsafe-attribute|printf '#[no_mangle]\npub extern \"C\" fn z() {}\n' >> crates/x-sys/src/lib.rs"
    "reserved-crate-built|mkdir -p crates/y-shell/src && sed 's/\"good\"/\"y-shell\"/' crates/good/Cargo.toml > crates/y-shell/Cargo.toml && : > crates/y-shell/src/lib.rs"
    "stale-inventory|rm -rf crates/x-sys"
    "path-with-space|mkdir -p 'crates/evil crate/src' && sed 's/\"good\"/\"evil\"/' crates/good/Cargo.toml > 'crates/evil crate/Cargo.toml' && printf 'pub fn e() { unsafe {} }\n' > 'crates/evil crate/src/lib.rs'"
    "package-header-comment|mkdir -p tools/hidden/src && printf '[package] # hidden from the workspace\nname = \"hidden\"\nversion = \"0.0.0\"\nedition = \"2021\"\n' > tools/hidden/Cargo.toml && printf 'pub fn h() { unsafe {} }\n' > tools/hidden/src/lib.rs"
  )
  local c name cmd d
  for c in "${cases[@]}"; do
    name="${c%%|*}"
    cmd="${c#*|}"
    d="$t/$name"
    cp -R "$base" "$d"
    (cd "$d" && eval "$cmd")
    if check_tree "$d" > "$t/$name.out" 2>&1; then
      echo "self-test: $name was not found" >&2
      fail=1
    else
      echo "self-test: $name refused: $(head -n 1 "$t/$name.out")"
    fi
  done
  [ "$fail" = 0 ] && echo "self-test: the checks fail on ${#cases[@]} planted cases and pass the clean fixture"
  return "$fail"
}

case "${1:-}" in
  --self-test) self_test ;;
  --root) check_tree "$(cd "${2:?--root needs a directory}" && pwd)" ;;
  "")
    top="$(cd "$here/.." && pwd)"
    if check_tree "$top"; then
      echo "unsafe: confined to the modules docs/UNSAFE.md lists; every other crate forbids it"
    else
      echo "unsafe: see the errors above (docs/UNSAFE.md)" >&2
      exit 1
    fi
    ;;
  *) echo "usage: $0 [--self-test | --root DIR]" >&2; exit 2 ;;
esac
