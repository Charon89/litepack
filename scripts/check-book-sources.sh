#!/usr/bin/env bash
# E9-3: the book's format-specification pages are includes of docs/spec/, never copies, and the unpublished
# method document stays out of the book.
#   bash scripts/check-book-sources.sh                 check the sources (before `mdbook build book`)
#   bash scripts/check-book-sources.sh --built DIR     check the built site (DIR is book/book) as well
#   bash scripts/check-book-sources.sh --self-test     check that each check fails on a crafted tree
#   ... --root DIR                                     use another tree (DIR/book, DIR/docs/spec)
# Source checks:
#   (a) each page under book/src/spec/ is exactly its include line plus the footer include;
#   (b) each docs/spec/*.md has a page of the same name there;
#   (c) no file under book/ (the build output book/book aside), by content or by name, names the method
#       document (case-insensitive);
#   (f) no file under docs/spec/ holds `{{`, optional whitespace and `#` (a directive of any kind inside the
#       spec could pull in any file);
# Built checks:
#   (d) each spec page's HTML holds the first `# ` heading text of its source file (mdBook only warns when an
#       include fails, so this is what turns a missing include into a failure);
#   (e) no built file, by content or by name, names the method document (case-insensitive);
#   (g) no built file holds a content fingerprint of the method document: its first `# ` heading and every
#       other line that has 60 or more letters and digits and no `](`; both sides are reduced to letters and
#       digits (HTML: tags and entities removed first), so markup differences do not matter. A sample: a
#       reworded or heavily reformatted copy can evade it.
# Source check (h), the structural guard together with (f):
#   (h) in every file under book/src/ each {{#...}} is an allowed include: {{#include P}} (anchor or range
#       allowed) with P resolving inside book/src/, or exactly the wrapper include of docs/spec/NAME.md in
#       book/src/spec/NAME.md; rustdoc_include, playground and the rest fail. With (f), nothing outside
#       book/src and docs/spec can reach the site by inclusion. (h) and (f) mirror mdBook 0.5.4's directive
#       pattern (mdbook-driver-0.5.4, src/builtin_preprocessors/links.rs, lines 413-423): whitespace and
#       newlines inside a directive count. An mdBook upgrade must re-check the pattern (see scan_directives).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
needle='LitePack-Method'

nonblank() { grep -q '[^[:space:]]' <<<"$1"; }

# scan_directives MODE DIR. Prints "FILE:LINE: ..." for each finding in the files under DIR. Each file is read
# whole as UTF-8 (one that is not valid UTF-8 is a finding) and matched with mdBook 0.5.4's own directive
# pattern (mdbook-driver-0.5.4 src/builtin_preprocessors/links.rs, lines 413-423): an escaped `\{{#...}}`
# is rendered as text and never included; otherwise `{{`, optional whitespace (newlines too), `#TYPE`,
# whitespace, a target of any non-`}` characters, `}}`. An mdBook upgrade must re-check this pattern.
#   MODE h (DIR is book/src): every directive must be `include` with the target's first token a path that
#     resolves inside DIR (an `:anchor` or `:start:end` suffix allowed), or, in spec/NAME.md, exactly the
#     wrapper include of docs/spec/NAME.md; anything else (rustdoc_include, playground...) is a finding.
#   MODE f (DIR is docs/spec): any `{{` + optional whitespace + `#`, escaped or not, is a finding.
scan_directives() {
  find "$2" -type f -print0 | perl -0ne '
    use Encode qw(decode FB_CROAK);
    BEGIN { $mode = shift @ARGV; $root = shift @ARGV; }
    chomp; my $p = $_;
    open my $h, "<:raw", $p or next;
    my $s; { local $/; $s = <$h>; } close $h;
    my $line = sub { my $o = shift; my $pre = substr($s, 0, $o); 1 + ($pre =~ tr/\n//) };
    my $u = eval { decode("UTF-8", $s, FB_CROAK) };
    if (!defined $u) { print "$p:0: not valid UTF-8\n"; next; }
    $s = $u;
    (my $rel = $p) =~ s{^\Q$root\E/?}{};
    my @dir = split m{/}, $rel; pop @dir;
    if ($mode eq "f") {
      while ($s =~ /\{\{\s*\#/g) { print "$p:", $line->($-[0]), ": a directive inside the spec\n"; }
      next;
    }
    while ($s =~ /\\\{\{\#.*\}\}|\{\{\s*\#([a-zA-Z0-9_]+)\s+([^}]+)\}\}/g) {
      next unless defined $1;
      my ($type, $target, $start) = ($1, $2, $-[0]);
      my $ok = 0;
      my ($path) = split " ", $target;
      if ($type eq "include" && defined $path) {
        (my $t = $target) =~ s/^\s+|\s+$//g;
        if ($rel =~ m{^spec/([^/]+)$} && $t eq "../../../docs/spec/$1") { $ok = 1; }
        else {
          $path =~ s/:.*//;
          my @st = @dir; my $in = $path !~ m{^/} && length $path;
          for my $c (split m{/}, $path) {
            next if $c eq "" || $c eq ".";
            if ($c eq "..") { if (@st) { pop @st } else { $in = 0; last } } else { push @st, $c }
          }
          $ok = $in ? 1 : 0;
        }
      }
      if (!$ok) { (my $d = $&) =~ tr/\n/ /; print "$p:", $line->($start), ": $d\n"; }
    }
  ' "$1" "$2"
}


check_sources() {
  local root="$1" fail=0 f name page
  local spec="$root/docs/spec" pages="$root/book/src/spec"
  # (a)
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
  # (b)
  if [ -d "$spec" ]; then
    for f in "$spec"/*.md; do
      [ -f "$f" ] || continue
      name="$(basename "$f")"
      if [ ! -f "$pages/$name" ]; then echo "(b) no book page for docs/spec/$name"; fail=1; fi
    done
  fi
  # (c) The build output book/book is pruned; (e) and (g) cover it.
  if [ -d "$root/book" ]; then
    local hits
    hits="$(cd "$root" && find book -path book/book -prune -o -type f -print0 | xargs -0 -r grep -Iil -e "$needle" || true)"
    hits="$hits"$'\n'"$(cd "$root" && find book -path book/book -prune -o -iname '*litepack-method*' -print || true)"
    if nonblank "$hits"; then
      printf '(c) names the method document: %s\n' "$hits"; fail=1
    fi
  fi
  # (f)
  if [ -d "$spec" ]; then
    local inc
    inc="$(scan_directives f "$spec" || true)"
    if nonblank "$inc"; then printf '(f) an include inside docs/spec: %s\n' "$inc"; fail=1; fi
  fi
  # (h)
  if [ -d "$root/book/src" ]; then
    local bad
    bad="$(scan_directives h "$root/book/src" || true)"
    if nonblank "$bad"; then printf '(h) a directive outside the include allow-list: %s\n' "$bad"; fail=1; fi
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

# Prints the fingerprint of the method document $1, one reduced string per line (see the header).
fingerprint() {
  perl -e '
    open my $fh, "<:raw", $ARGV[0] or die "$ARGV[0]: $!";
    my ($n, $head) = (0, 0);
    while (my $l = <$fh>) {
      $n++;
      $l =~ s/\r?\n\z//;
      next if index($l, "](") >= 0;
      my $heading = !$head && $l =~ /^# /;
      $head = 1 if $heading;
      (my $s = $l) =~ s/[^A-Za-z0-9]//g;
      print "$n:$s\n" if length($s) >= ($heading ? 8 : 60);
    }
  ' "$1"
}

# Prints "FILE: line N" for each file of the tree $2 whose reduced content holds a line of the fingerprint
# file $1 (the first such line, by number).
scan_fingerprint() {
  find "$2" -type f -print0 | perl -0ne '
    BEGIN { open my $f, "<", shift @ARGV or die; $/ = "\n"; chomp(@fp = <$f>); close $f; $/ = "\0"; }
    chomp; my $p = $_;
    open my $h, "<:raw", $p or next; local $/; my $c = <$h>; close $h;
    $c =~ s/<[^>]*>//g; $c =~ s/&[#A-Za-z0-9]+;//g; $c =~ s/[^A-Za-z0-9]//g;
    for my $e (@fp) { my ($n, $s) = split /:/, $e, 2; if (index($c, $s) >= 0) { print "$p: method document line $n\n"; last; } }
  ' "$1"
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
  hits="$(grep -rIil -e "$needle" "$built" 2>/dev/null || true)"
  hits="$hits"$'\n'"$(find "$built" -iname '*litepack-method*' -print || true)"
  if nonblank "$hits"; then printf '(e) built files name the method document: %s\n' "$hits"; fail=1; fi
  # (g)
  local method="$root/docs/LitePack-Method-v2.md" fp
  if [ ! -f "$method" ]; then
    echo "(g) no method document to fingerprint at docs/LitePack-Method-v2.md"; fail=1
  else
    fp="$(mktemp)"
    fingerprint "$method" > "$fp"
    if [ ! -s "$fp" ]; then echo "(g) the method document gave no fingerprint"; fail=1; fi
    hits="$(scan_fingerprint "$fp" "$built" || true)"
    rm -f "$fp"
    if [ -n "$hits" ]; then printf '(g) built files hold the method document'"'"'s text: %s\n' "$hits"; fail=1; fi
  fi
  return "$fail"
}

# A good tree in $1: a spec of two files and their wrappers; a built site for it; a method document.
make_tree() {
  local r="$1" n
  mkdir -p "$r/docs/spec" "$r/book/src/spec" "$r/site/spec"
  for n in a B; do
    printf '# Spec `%s` title\n\nbody\n' "$n" > "$r/docs/spec/$n.md"
    printf '{{#include ../../../docs/spec/%s.md}}\n\n{{#include ../footer.md}}\n' "$n" > "$r/book/src/spec/$n.md"
    printf '<html><h1 id="x">Spec <code>%s</code> title</h1></html>\n' "$n" > "$r/site/spec/$n.html"
  done
  printf '# Summary\n\n- [A](spec/a.md)\n' > "$r/book/src/SUMMARY.md"
  {
    printf '# The Secret Method Plan\n\n'
    for n in one two three four; do
      printf 'Paragraph %s of the secret method plan talks at some length about nothing public at all, so that this line reaches the length that the fingerprint rule demands of it.\n' "$n"
    done
  } > "$r/docs/LitePack-Method-v2.md"
}

# expect_fail TAG DESCRIPTION COMMAND...: the command must fail and its output must carry (TAG).
expect_fail() {
  local tag="$1" what="$2" out; shift 2
  if out="$("$@" 2>&1)"; then echo "self-test FAILED: $what was not caught"; exit 1; fi
  if ! grep -qF -e "($tag)" <<<"$out"; then echo "self-test FAILED: $what failed, but not on ($tag): $out"; exit 1; fi
  echo "self-test ok: ($tag) $what is caught"
}

expect_pass() { # DESCRIPTION COMMAND...
  local what="$1"; shift
  if ! "$@" >/dev/null 2>&1; then echo "self-test FAILED: $what does not pass"; exit 1; fi
  echo "self-test ok: $what passes"
}

self_test() {
  t="$(mktemp -d)"; trap 'rm -rf "${t:-}"' EXIT
  make_tree "$t/good"
  expect_pass "the good tree, sources" check_sources "$t/good"
  expect_pass "the good tree, built" check_built "$t/good" "$t/good/site"
  make_tree "$t/crlf"; printf '{{#include ../../../docs/spec/a.md}}\r\n\r\n{{#include ../footer.md}}\r\n' > "$t/crlf/book/src/spec/a.md"
  expect_pass "a wrapper with CRLF line ends" check_sources "$t/crlf"

  make_tree "$t/a"; printf 'copied text\n' >> "$t/a/book/src/spec/a.md"
  expect_fail a "a copied page" check_sources "$t/a"
  make_tree "$t/a2"; printf '{{#include ../../../docs/spec/a.md:10:20}}\n\n{{#include ../footer.md}}\n' > "$t/a2/book/src/spec/a.md"
  expect_fail a "a range include" check_sources "$t/a2"
  make_tree "$t/a3"; printf '{{#include ../../../docs/spec/a.md}}\n{{#rustdoc_include x.rs}}\n\n{{#include ../footer.md}}\n' > "$t/a3/book/src/spec/a.md"
  expect_fail a "a rustdoc_include in a wrapper" check_sources "$t/a3"
  make_tree "$t/a4"; printf '{{#include ../../../docs/spec/a.md}}\n{{#playground x.rs}}\n\n{{#include ../footer.md}}\n' > "$t/a4/book/src/spec/a.md"
  expect_fail a "a playground in a wrapper" check_sources "$t/a4"
  make_tree "$t/b"; printf '# C\n' > "$t/b/docs/spec/C.md"
  expect_fail b "a spec file without a page" check_sources "$t/b"
  make_tree "$t/c"; printf '[m](../docs/LitePack-Method-v2.md)\n' > "$t/c/book/src/m.md"
  expect_fail c "a link to the method document" check_sources "$t/c"
  make_tree "$t/c2"; printf '[m](../docs/litepack-method-v2.md)\n' > "$t/c2/book/src/m.md"
  expect_fail c "a lower-case link to the method document" check_sources "$t/c2"
  make_tree "$t/f"; printf '{{#include ../LitePack-Method-v2.md}}\n' >> "$t/f/docs/spec/B.md"
  expect_fail f "a nested include in a spec file" check_sources "$t/f"
  make_tree "$t/d"; printf '<html>other</html>\n' > "$t/d/site/spec/a.html"
  expect_fail d "a page without its source heading" check_built "$t/d" "$t/d/site"
  make_tree "$t/e"; printf '<a href="LitePack-Method-v2.html">m</a>\n' > "$t/e/site/m.html"
  expect_fail e "a built file naming the method document" check_built "$t/e" "$t/e/site"
  make_tree "$t/e2"; printf '<a href="litepack-method-v2.html">m</a>\n' > "$t/e2/site/m.html"
  expect_fail e "a lower-case name in a built file" check_built "$t/e2" "$t/e2/site"
  make_tree "$t/e3"; printf '<html>x</html>\n' > "$t/e3/site/LitePack-Method-v2.html"
  expect_fail e "a built file named like the method document" check_built "$t/e3" "$t/e3/site"
  make_tree "$t/g"
  printf '<html><p>Paragraph <em>two</em> of the secret method plan talks at some length about nothing public at all, so that this line reaches the length that the fingerprint rule demands of it.</p></html>\n' > "$t/g/site/spec/x.html"
  expect_fail g "a built page holding the method document's text" check_built "$t/g" "$t/g/site"
  make_tree "$t/foot"; printf 'Text.\n\n{{#include footer.md}}\n{{#include footer.md:2:3}}\n' > "$t/foot/book/src/page.md"
  mkdir -p "$t/foot/book/src/sub"; printf '{{#include ../footer.md}}\n' > "$t/foot/book/src/sub/p.md"
  expect_pass "footer includes in a non-spec page" check_sources "$t/foot"
  make_tree "$t/h"; printf '{{#include LitePack-Method-v2.md:2:4}}\n' > "$t/h/docs/x.md"
  printf 'About.\n\n{{#include ../../docs/x.md}}\n' > "$t/h/book/src/about.md"
  expect_fail h "an indirect include of the method document" check_sources "$t/h"
  make_tree "$t/h2"; printf 'Other.\n\n{{#include ../../docs/anything.md}}\n' > "$t/h2/book/src/other.md"
  expect_fail h "a docs include from a non-spec page" check_sources "$t/h2"
  make_tree "$t/h3"; printf '{{#include ../../../docs/spec/B.md}}\n\n{{#include ../footer.md}}\n' > "$t/h3/book/src/spec/a.md"
  expect_fail h "the wrapper include in the wrong page" check_sources "$t/h3"
  make_tree "$t/g2"
  printf '<html><p>Paragraph four of the secret method plan talks at some length about nothing public at all, so that this line reaches the length that the fingerprint rule demands of it.</p></html>\n' > "$t/g2/site/about.html"
  expect_fail g "an indirect copy in a built page" check_built "$t/g2" "$t/g2/site"
  make_tree "$t/w1"; printf 'Text.\n\n{{ #include ../../docs/x.md}}\n' > "$t/w1/book/src/p.md"
  expect_fail h "an include with a space after the braces" check_sources "$t/w1"
  make_tree "$t/w2"; printf 'Text.\n\n{{\n#include\n../../docs/x.md}}\n' > "$t/w2/book/src/p.md"
  expect_fail h "a directive split over lines" check_sources "$t/w2"
  make_tree "$t/w3"; printf 'Text\n{{ #include ../x.md}}\n' >> "$t/w3/docs/spec/B.md"
  expect_fail f "a spaced directive inside the spec" check_sources "$t/w3"
  make_tree "$t/w4"; printf 'One.\nTwo.\n{{#include ../../docs/x.md}}\n' > "$t/w4/book/src/p.md"
  out="$(check_sources "$t/w4" 2>&1 || true)"
  if ! grep -q 'p.md:3:' <<<"$out"; then echo "self-test FAILED: (h) does not name line 3: $out"; exit 1; fi
  echo "self-test ok: (h) names the line of a finding (:3:)"
  make_tree "$t/w5"; printf 'Shown as text: \\{{#include ../../docs/x.md}}\n' > "$t/w5/book/src/p.md"
  expect_pass "an escaped directive in a page" check_sources "$t/w5"
  make_tree "$t/w6"; printf 'Text \377\376 bytes.\n' > "$t/w6/book/src/p.md"
  expect_fail h "a page that is not valid UTF-8" check_sources "$t/w6"
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
