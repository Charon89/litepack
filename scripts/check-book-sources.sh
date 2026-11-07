#!/usr/bin/env bash
# E9-3: the book's format-specification pages are includes of docs/spec/, never copies, and the unpublished
# method document stays out of the book.
#   bash scripts/check-book-sources.sh                 check the sources (before `mdbook build book`)
#   bash scripts/check-book-sources.sh --built DIR     check the built site (DIR is book/book) as well
#   bash scripts/check-book-sources.sh --self-test     check that each check fails on a crafted tree
#   ... --root DIR                                     use another tree (DIR/book, DIR/docs/spec)
# Source checks:
#   (a) each page under book/src/spec/ is exactly its include line plus the footer include;
#   (b) each file in docs/spec/ has a page of the same name there (and the page includes that file);
#   (c) no file under book/ (the build output book/book aside) names the method document.
# Built checks:
#   (d) each spec page's HTML holds the first `# ` heading text of its source file;
#   (e) no built file names the method document.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
needle='LitePack-Method'

check_sources() {
  local root="$1" fail=0 f name page
  local spec="$root/docs/spec" pages="$root/book/src/spec"
  # (a) and (b)
  if [ -d "$pages" ]; then
    for page in "$pages"/*; do
      [ -e "$page" ] || continue
      name="$(basename "$page")"
      local got want
      got="$(grep -v '^[[:space:]]*$' "$page" | tr -d '\r')"
      want="$(printf '%s\n%s' "{{#include ../../../docs/spec/$name}}" '{{#include ../footer.md}}')"
      if [ "$got" != "$want" ]; then
        echo "(a) not an include plus the footer include (a copy?): book/src/spec/$name"; fail=1
      fi
    done
  fi
  if [ -d "$spec" ]; then
    for f in "$spec"/*; do
      [ -f "$f" ] || continue
      name="$(basename "$f")"
      if [ ! -f "$pages/$name" ]; then echo "(b) no book page for docs/spec/$name"; fail=1; fi
    done
  fi
  # (c)
  if [ -d "$root/book" ]; then
    local hits
    # The build output book/book is pruned; (e) covers it.
    hits="$(cd "$root" && find book -path book/book -prune -o -type f -print0 | xargs -0 -r grep -Il -e "$needle" || true)"
    if [ -n "$hits" ]; then
      printf '(c) names the method document: %s\n' "$hits"; fail=1
    fi
  fi
  return "$fail"
}

# First `# ` heading of a source file, as it reads rendered: backticks dropped, HTML-escaped.
heading_text() {
  local h
  h="$(grep -m1 '^# ' "$1" | tr -d '\r')"
  h="${h#\# }"
  h="${h//\`/}"
  h="${h//&/&amp;}"; h="${h//</&lt;}"; h="${h//>/&gt;}"
  printf '%s' "$h"
}

check_built() {
  local root="$1" built="$2" fail=0 f name html text plain
  [ -d "$built" ] || { echo "no built site at $built"; return 1; }
  for f in "$root"/docs/spec/*.md; do
    [ -f "$f" ] || continue
    name="$(basename "$f" .md)"
    html="$built/spec/$name.html"
    if [ ! -f "$html" ]; then echo "(d) not built: spec/$name.html"; fail=1; continue; fi
    text="$(heading_text "$f")"
    plain="$(perl -0pe 's/<[^>]*>//g' "$html")"
    if ! grep -qF -e "$text" <<<"$plain"; then
      echo "(d) spec/$name.html lacks the heading of docs/spec/$name.md: $text"; fail=1
    fi
  done
  local hits
  hits="$(grep -rIl -e "$needle" "$built" 2>/dev/null || true)"
  if [ -n "$hits" ]; then printf '(e) built files name the method document: %s\n' "$hits"; fail=1; fi
  return "$fail"
}

# A good tree in $1: a spec of three files and their wrappers; a built site for it.
make_tree() {
  local r="$1" n
  mkdir -p "$r/docs/spec" "$r/book/src/spec" "$r/site/spec"
  for n in a B; do
    printf '# Spec `%s` title\n\nbody\n' "$n" > "$r/docs/spec/$n.md"
    printf '{{#include ../../../docs/spec/%s.md}}\n\n{{#include ../footer.md}}\n' "$n" > "$r/book/src/spec/$n.md"
    printf '<html><h1 id="x">Spec <code>%s</code> title</h1></html>\n' "$n" > "$r/site/spec/$n.html"
  done
  printf '# Summary\n\n- [A](spec/a.md)\n' > "$r/book/src/SUMMARY.md"
}

expect_fail() { # description, command...
  local what="$1"; shift
  if "$@" >/dev/null 2>&1; then echo "self-test FAILED: $what was not caught"; exit 1; fi
  echo "self-test ok: $what is caught"
}

self_test() {
  t="$(mktemp -d)"; trap 'rm -rf "${t:-}"' EXIT
  make_tree "$t/good"
  check_sources "$t/good" >/dev/null || { echo "self-test FAILED: the good tree fails the source checks"; exit 1; }
  check_built "$t/good" "$t/good/site" >/dev/null || { echo "self-test FAILED: the good tree fails the built checks"; exit 1; }
  echo "self-test ok: the good tree passes"

  make_tree "$t/a"; printf 'copied text\n' >> "$t/a/book/src/spec/a.md"
  expect_fail "(a) a copied page" check_sources "$t/a"
  make_tree "$t/b"; printf '# C\n' > "$t/b/docs/spec/C.md"
  expect_fail "(b) a spec file without a page" check_sources "$t/b"
  make_tree "$t/c"; printf '[m](../docs/LitePack-Method-v2.md)\n' > "$t/c/book/src/m.md"
  expect_fail "(c) a link to the method document" check_sources "$t/c"
  make_tree "$t/d"; printf '<html>other</html>\n' > "$t/d/site/spec/a.html"
  expect_fail "(d) a page without its source heading" check_built "$t/d" "$t/d/site"
  make_tree "$t/e"; printf '<a href="LitePack-Method-v2.html">m</a>\n' > "$t/e/site/m.html"
  expect_fail "(e) a built file naming the method document" check_built "$t/e" "$t/e/site"
  echo "self-test: all checks proved"
}

root="$here/.."
mode=sources
built=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --self-test) mode=self ;;
    --built) mode=built; built="${2:?--built needs a directory}"; shift ;;
    --root) root="${2:?--root needs a directory}"; shift ;;
    *) echo "unknown argument: $1"; exit 2 ;;
  esac
  shift
done

case "$mode" in
  self) self_test ;;
  sources) check_sources "$root" && echo "book sources check: ok" ;;
  built) check_built "$root" "$built" && echo "built site check: ok" ;;
esac
