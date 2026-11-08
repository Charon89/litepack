#!/usr/bin/env bash
# E9-3: the book's format-specification pages are includes of docs/spec/, never copies, and the unpublished
# method document stays out of the book.
#   bash scripts/check-book-sources.sh                 check the sources (before `mdbook build book`)
#   bash scripts/check-book-sources.sh --built DIR     check the built site (DIR is book/book) as well
#   bash scripts/check-book-sources.sh --self-test     check that each check fails on a crafted tree
#   ... --root DIR                                     use another tree (DIR/book, DIR/docs/spec, DIR/docs/cli)
# E9-4 adds the CLI pages: book/src/cli/NAME.md includes docs/cli/NAME.md; book/src/cli/NAME-N.md shows the man
# page docs/cli/NAME.N in a four-backtick fence (man_wrapper has the exact text). Extra source checks: (a) each CLI
# page is the wrapper of an existing source; (b) docs/cli holds only NAME.md / NAME.N files, each with its page;
# (c), (f), (k) also cover docs/cli (and (c) docs/spec); (h) admits the two wrapper includes, each in its own page;
# (l) no line of a man page opens, after spaces or tabs, with four or more backticks (it could close the fence).
# Built (d) also covers cli/NAME.html (the first `# ` heading) and cli/NAME-N.html (the first three words of the
# `.TH ` line).
# E9-8 adds the licensing and notices pages: book/src/licensing.md includes the `published` anchor of
# docs/LICENSING.md, book/src/notices.md shows THIRD-PARTY-NOTICES in a four-backtick fence (licensing_wrapper and
# notices_wrapper have the exact texts). (a) both pages are exactly those texts; (h) admits those two includes, each in
# its own page, so the whole-file include of docs/LICENSING.md fails; (c), (f), (k) also cover docs/LICENSING.md and
# THIRD-PARTY-NOTICES, (l) the latter; (j) admits [output.html.search.chapter] with only the line
# `"notices.md" = { enable = false }`; (m) docs/LICENSING.md has the anchors `published` (start, then end), no other
# ANCHOR marker, and between them exactly the headings `## Our licence` and `## Dependency policy (...)`, so an edit
# cannot widen what the book publishes without failing. Built: licensing.html has both headings and none of the
# unpublished ones; the code text of notices.html, unescaped, equals THIRD-PARTY-NOTICES (the CI step of E9-8).
# (n) book/src/404.md, when it exists, holds no `{{`, optional whitespace and `#`, escaped or not: mdBook renders it
# raw (it is not a chapter and no preprocessor runs on it, D-153), so a directive there would be published as text.
# It is read as (f) reads its sources; an unreadable or non-UTF-8 file is a finding.
# Source checks:
#   (a) each page under book/src/spec/ is exactly its include line plus the footer include;
#   (b) each docs/spec/*.md has a page of the same name there;
#   (c) no file under book/ (the build output book/book aside), by content or by name, names the method
#       document (case-insensitive);
#   (f) no file under docs/spec/ or docs/cli/ holds `{{`, optional whitespace and `#` (a directive of any kind
#       inside a source could pull in any file);
# Built checks:
#   (d) each spec page's HTML holds the first `# ` heading text of its source file (mdBook only warns when an
#       include fails, so this is what turns a missing include into a failure);
#   (e) no built file, by content or by name, names the method document (case-insensitive);
#   (g) no built file holds a content fingerprint of the method document: its first `# ` heading and every
#       other line that has 60 or more letters and digits and no `](`; both sides are reduced to letters and
#       digits (HTML: tags and entities removed first), so markup differences do not matter. A sample: a
#       reworded or heavily reformatted copy can evade it.
# Structural guards (they close the routes by which a file outside the book could be published):
#   (f) and (h) below; (i) every line of book/src/SUMMARY.md is one of the exact shapes the book uses and
#       every chapter path is a plain relative path of [A-Za-z0-9_-] components ending in .md; (j) every line
#       of book/book.toml is a blank, a comment, one of three table headers, or a listed key with a plain value
#       (`[book] src` must be "src"). (i) and (j) are allow-lists, not deny-lists, because mdBook's Markdown and
#       TOML parsers accept many spellings of the same thing (entities, reference links, dotted keys, spaced
#       headers); see allow_scan for the exact shapes. (k) no symlink under book/ (the build output aside) or
#       docs/spec/ or docs/cli/ (mdBook follows symlinks, find -type f does not). (h) also rejects a backslash
#       in an include
#       target, which a Windows build would resolve outside src/.
# The second line of defence is (c) and (e), by name, and (g), by a content sample. Residual risk: text reworded
# by hand into the book's own pages cannot be found mechanically; the guards stop includes, routes and
# verbatim copies.
# Source check (h), the structural guard together with (f):
#   (h) in every file under book/src/ each {{#...}} is an allowed include: {{#include P}} (anchor or range
#       allowed) with P resolving inside book/src/, or exactly the wrapper include of docs/spec/NAME.md in
#       book/src/spec/NAME.md, of docs/cli/NAME.md in book/src/cli/NAME.md, or of docs/cli/NAME.N in
#       book/src/cli/NAME-N.md; rustdoc_include, playground and the rest fail. With (f), nothing outside
#       book/src, docs/spec and docs/cli can reach the site by inclusion. (h) and (f) mirror mdBook 0.5.4's directive
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
#     wrapper include of docs/spec/NAME.md (likewise cli/NAME.md and cli/NAME-N.md, see check (h));
#     anything else (rustdoc_include, playground...) is a finding.
#     Also exactly: ../../docs/LICENSING.md:published in licensing.md, ../../THIRD-PARTY-NOTICES in notices.md.
#   MODE f (DIR is docs/spec or docs/cli, or one file: docs/LICENSING.md, THIRD-PARTY-NOTICES):
#     any `{{` + optional whitespace + `#`, escaped or not, is a finding.
scan_directives() {
  find "$2" -type f -print0 | perl -0ne '
    use Encode qw(decode FB_CROAK);
    BEGIN { $mode = shift @ARGV; $root = shift @ARGV; }
    chomp; my $p = $_;
    open my $h, "<:raw", $p or do { print "$p:0: cannot be read: $!\n"; next };
    my $s; { local $/; $s = <$h>; } close $h;
    my $line = sub { my $o = shift; my $pre = substr($s, 0, $o); 1 + ($pre =~ tr/\n//) };
    my $u = eval { decode("UTF-8", $s, FB_CROAK) };
    if (!defined $u) { print "$p:0: not valid UTF-8\n"; next; }
    $s = $u;
    (my $rel = $p) =~ s{^\Q$root\E/?}{};
    my @dir = split m{/}, $rel; pop @dir;
    if ($mode eq "f") {
      while ($s =~ /\{\{\s*\#/g) { print "$p:", $line->($-[0]), ": a directive inside the source file\n"; }
      next;
    }
    while ($s =~ /\\\{\{\#.*\}\}|\{\{\s*\#([a-zA-Z0-9_]+)\s+([^}]+)\}\}/g) {
      next unless defined $1;
      my ($type, $target, $start) = ($1, $2, $-[0]);
      my $ok = 0;
      my ($path) = split " ", $target;
      # A backslash would resolve outside src/ in a Windows build, so it is never allowed.
      if ($type eq "include" && defined $path && index($target, "\\") < 0) {
        (my $t = $target) =~ s/^\s+|\s+$//g;
        if ($rel =~ m{^spec/([^/]+)$} && $t eq "../../../docs/spec/$1") { $ok = 1; }
        elsif ($rel =~ m{^cli/([A-Za-z0-9_-]+)\.md$} && $t eq "../../../docs/cli/$1.md") { $ok = 1; }
        elsif ($rel =~ m{^cli/([A-Za-z0-9_-]+)-([1-9])\.md$} && $t eq "../../../docs/cli/$1.$2") { $ok = 1; }
        elsif ($rel eq "licensing.md" && $t eq "../../docs/LICENSING.md:published") { $ok = 1; }
        elsif ($rel eq "notices.md" && $t eq "../../THIRD-PARTY-NOTICES") { $ok = 1; }
        elsif ($rel eq "security.md" && $t eq "../../docs/SECURITY.md") { $ok = 1; }
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


# allow_scan summary|toml FILE. Prints "FILE:LINE: text" for each line that is not one of the exact shapes the
# book uses; the guard is an allow-list because mdBook's Markdown and TOML parsers accept many spellings of the
# same thing (entities, reference links, balanced parentheses, dotted keys, spaced headers).
#   summary: blank; `# T`; `---`; `[T](P)`; `<indent>- [T](P)` (indent: groups of 2 or 4 spaces, no tabs);
#            T is [A-Za-z0-9 .,()'-]+; P is empty or [A-Za-z0-9_-]+(/[A-Za-z0-9_-]+)*\.md.
#   toml:    blank; `#` comment; the headers [book], [output.html], [output.html.search],
#            [output.html.search.chapter], [output.html.playground]; `key = value` with a key from the table's list and a value that is a
#            double-quoted string without backslash, true, false or an integer (the [book] src must equal "src", the playground runnable must be false);
#            under the chapter header only the exact line `"notices.md" = { enable = false }`.
allow_scan() {
  perl -e '
    my ($mode, $file) = @ARGV;
    open my $fh, "<:raw", $file or die "$file: $!";
    my %keys = (
      "[book]" => [qw(title authors description language src)],
      "[output.html]" => [qw(git-repository-url site-url default-theme preferred-dark-theme)],
      "[output.html.search]" => [qw(enable limit-results use-boolean-and expand heading-split-level)],
      "[output.html.playground]" => [qw(runnable)],
      "[output.html.search.chapter]" => [],
    );
    my $chapter_line = q{"notices.md" = { enable = false }};
    my $T = q{[A-Za-z0-9 .,()\x27-]+};
    my $P = q{[A-Za-z0-9_-]+(?:/[A-Za-z0-9_-]+)*\.md};
    my ($n, $sec) = (0, "");
    while (my $l = <$fh>) {
      $n++; $l =~ s/\r?\n\z//;
      my $ok = 0;
      if ($mode eq "summary") {
        $ok = 1 if $l =~ /\A\z/ || $l =~ /\A# $T\z/ || $l eq "---"
          || $l =~ /\A\[$T\]\((?:$P)?\)\z/ || $l =~ /\A(?:  |    )*- \[$T\]\((?:$P)?\)\z/;
      } else {
        if ($l =~ /\A\s*\z/ || $l =~ /\A\s*#/) { $ok = 1; }
        elsif (exists $keys{$l}) { $sec = $l; $ok = 1; }
        elsif ($sec eq "[output.html.search.chapter]") { $ok = 1 if $l eq $chapter_line; }
        elsif ($l =~ /\A([a-z-]+) = ("[^"\\]*"|true|false|-?[0-9]+)\z/ && $sec ne "" && grep { $_ eq $1 } @{ $keys{$sec} }) {
          $ok = 1;
          $ok = 0 if $sec eq "[book]" && $1 eq "src" && $2 ne "\"src\"";
          $ok = 0 if $sec eq "[output.html.playground]" && $2 ne "false";
        }
      }
      print "$file:$n: $l\n" unless $ok;
    }
  ' "$1" "$2"
}

check_sources() {
  local root="$1" fail=0 f name page
  local spec="$root/docs/spec" pages="$root/book/src/spec"
  local cli="$root/docs/cli" cpages="$root/book/src/cli"
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
  # (a) for the CLI pages: each page under book/src/cli/ is the wrapper of an existing source in docs/cli/
  if [ -d "$cpages" ]; then
    local cgot cwant cok
    while IFS= read -r -d '' page; do
      name="$(basename "$page")"
      if [ ! -f "$page" ] || [ "$(dirname "$page")" != "$cpages" ]; then
        echo "(a) not a plain page directly in book/src/cli/: ${page#"$root"/}"; fail=1; continue
      fi
      cgot="$(grep -v '^[[:space:]]*$' "$page" | tr -d '\r')"
      cok=0
      if [[ "$name" =~ ^([A-Za-z0-9_-]+)\.md$ ]] && [ -f "$cli/${BASH_REMATCH[1]}.md" ]; then
        cwant="$(printf '%s\n%s' "{{#include ../../../docs/cli/${BASH_REMATCH[1]}.md}}" '{{#include ../footer.md}}')"
        [ "$cgot" = "$cwant" ] && cok=1
      fi
      if [ "$cok" = 0 ] && [[ "$name" =~ ^([A-Za-z0-9_-]+)-([1-9])\.md$ ]] && [ -f "$cli/${BASH_REMATCH[1]}.${BASH_REMATCH[2]}" ]; then
        cwant="$(man_wrapper "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" | grep -v '^[[:space:]]*$')"
        [ "$cgot" = "$cwant" ] && cok=1
      fi
      if [ "$cok" = 0 ]; then
        echo "(a) not the wrapper of an existing docs/cli source (a copy, or a page with no source?): book/src/cli/$name"; fail=1
      fi
    done < <(find "$cpages" -mindepth 1 -print0)
  fi
  # (a) for the licensing and notices pages: exactly their wrappers, and their sources exist
  if [ -d "$root/book/src" ]; then
    local lp cgot cwant
    for lp in licensing notices security; do
      page="$root/book/src/$lp.md"
      if [ ! -f "$page" ]; then echo "(a) no book page book/src/$lp.md"; fail=1; continue; fi
      cgot="$(grep -v '^[[:space:]]*$' "$page" | tr -d '\r')"
      cwant="$("${lp}_wrapper" | grep -v '^[[:space:]]*$')"
      if [ "$cgot" != "$cwant" ]; then
        echo "(a) not the exact wrapper text (a copy, or a changed include?): book/src/$lp.md"; fail=1
      fi
    done
    for f in docs/LICENSING.md THIRD-PARTY-NOTICES docs/SECURITY.md; do
      if [ ! -f "$root/$f" ]; then echo "(a) the source of a book page is missing: $f"; fail=1; fi
    done
  fi
  # (b) for docs/cli: only NAME.md and NAME.N (N 1-9) files directly in it, each with its page, no two to one page
  if [ -d "$cli" ]; then
    local seen=" " pg
    while IFS= read -r -d '' f; do
      name="${f#"$cli"/}"
      if [ -L "$f" ] || [ ! -f "$f" ] || [[ "$name" == */* ]]; then
        echo "(b) docs/cli holds something other than a plain file directly in it: docs/cli/$name"; fail=1; continue
      fi
      if [[ "$name" =~ ^([A-Za-z0-9_-]+)\.md$ ]]; then pg="${BASH_REMATCH[1]}.md"
      elif [[ "$name" =~ ^([A-Za-z0-9_-]+)\.([1-9])$ ]]; then pg="${BASH_REMATCH[1]}-${BASH_REMATCH[2]}.md"
      else echo "(b) docs/cli/$name is not NAME.md or NAME.N"; fail=1; continue; fi
      case "$seen" in *" $pg "*) echo "(b) two docs/cli sources map to one page: docs/cli/$name -> book/src/cli/$pg"; fail=1 ;; esac
      seen="$seen$pg "
      if [ ! -f "$cpages/$pg" ]; then echo "(b) no book page for docs/cli/$name (book/src/cli/$pg)"; fail=1; fi
    done < <(find "$cli" -mindepth 1 -print0)
  fi
  # (c) The build output book/book is pruned; (e) and (g) cover it. docs/spec, docs/cli, docs/LICENSING.md and
  # THIRD-PARTY-NOTICES are covered too.
  if [ -d "$root/book" ]; then
    local hits d
    hits="$(cd "$root" && find book -path book/book -prune -o -type f -print0 | xargs -0 -r grep -il -e "$needle" || true)"
    hits="$hits"$'\n'"$(cd "$root" && find book -path book/book -prune -o -iname '*litepack-method*' -print || true)"
    for d in docs/LICENSING.md THIRD-PARTY-NOTICES docs/SECURITY.md; do
      [ -f "$root/$d" ] || continue
      hits="$hits"$'\n'"$(cd "$root" && grep -il -e "$needle" "$d" || true)"
    done
    for d in docs/spec docs/cli; do
      [ -d "$root/$d" ] || continue
      hits="$hits"$'\n'"$(cd "$root" && find "$d" -type f -print0 | xargs -0 -r grep -il -e "$needle" || true)"
      hits="$hits"$'\n'"$(cd "$root" && find "$d" -iname '*litepack-method*' -print || true)"
    done
    if nonblank "$hits"; then
      printf '(c) names the method document: %s\n' "$hits"; fail=1
    fi
  fi
  # (f)
  if [ -d "$spec" ]; then
    local inc
    inc="$(scan_directives f "$spec" || true)"
    if nonblank "$inc"; then printf '(f) an include, or an unreadable file, inside docs/spec: %s\n' "$inc"; fail=1; fi
  fi
  if [ -d "$cli" ]; then
    local cinc
    cinc="$(scan_directives f "$cli" || true)"
    if nonblank "$cinc"; then printf '(f) an include, or an unreadable file, inside docs/cli: %s\n' "$cinc"; fail=1; fi
  fi
  for f in docs/LICENSING.md THIRD-PARTY-NOTICES docs/SECURITY.md; do
    [ -e "$root/$f" ] || continue
    local sinc
    sinc="$(scan_directives f "$root/$f" || true)"
    if nonblank "$sinc"; then printf '(f) an include, or an unreadable file, inside %s: %s\n' "$f" "$sinc"; fail=1; fi
  done
  # (l) no line of a man page or of THIRD-PARTY-NOTICES may open with four or more backticks (it could close the
  # four-backtick fence of its page). Lines end at LF, CRLF or a lone CR, as in CommonMark.
  local fence="" lf
  for lf in "$cli"/*.[1-9] "$root/THIRD-PARTY-NOTICES"; do
    [ -f "$lf" ] || continue
    fence="$fence"$'\n'"$(perl -e '
        open my $h, "<:raw", $ARGV[0] or do { print "$ARGV[0]:0: cannot be read: $!\n"; exit };
        local $/; my $s = <$h>; my $n = 0;
        for my $l (split /\r\n|\r|\n/, $s, -1) { $n++; print "$ARGV[0]:$n:$l\n" if $l =~ /^[ \t]*`{4,}/ }
      ' "$lf" || true)"
  done
  if nonblank "$fence"; then printf '(l) a line of four or more backticks (FILE:LINE:text): %s\n' "$fence"; fail=1; fi
  # (i) SUMMARY.md and (j) book.toml: allow-lists of exact line shapes (see allow_scan)
  if [ -f "$root/book/src/SUMMARY.md" ]; then
    local bad_i
    bad_i="$(allow_scan summary "$root/book/src/SUMMARY.md" || true)"
    if nonblank "$bad_i"; then printf '(i) a SUMMARY.md line outside the allow-list: %s\n' "$bad_i"; fail=1; fi
  fi
  if [ -f "$root/book/book.toml" ]; then
    local bad_j
    bad_j="$(allow_scan toml "$root/book/book.toml" || true)"
    if nonblank "$bad_j"; then
      printf '(j) a book.toml line outside the allow-list (widen it only after checking that the key cannot bring a file from outside book/src into the site): %s\n' "$bad_j"; fail=1
    fi
  fi
  # (k) no symlink under book/ (the build output aside), docs/spec/ or docs/cli/, and neither docs/LICENSING.md nor
  # THIRD-PARTY-NOTICES nor docs/SECURITY.md is one: mdBook follows symlinks, find -type f skips them
  local links
  links="$( { [ -d "$root/book" ] && (cd "$root" && find book -path book/book -prune -o -type l -print)
    [ -d "$spec" ] && (cd "$root" && find docs/spec -type l -print)
    [ -d "$cli" ] && (cd "$root" && find docs/cli -type l -print)
    for f in docs/LICENSING.md THIRD-PARTY-NOTICES docs/SECURITY.md; do [ -L "$root/$f" ] && echo "$f"; done; } || true)"
  if nonblank "$links"; then printf '(k) a symlink: %s\n' "$links"; fail=1; fi
  # (m) the anchors of docs/LICENSING.md and the headings between them (what licensing.md publishes)
  if [ -d "$root/book/src" ]; then
    local anchors
    anchors="$(check_anchors "$root/docs/LICENSING.md" || true)"
    if nonblank "$anchors"; then printf '(m) the published region of docs/LICENSING.md: %s\n' "$anchors"; fail=1; fi
  fi
  # (n) book/src/404.md is rendered raw (no preprocessor runs on it), so any directive in it is a finding
  if [ -e "$root/book/src/404.md" ]; then
    local nf
    nf="$(scan_directives f "$root/book/src/404.md" || true)"
    if nonblank "$nf"; then printf '(n) a directive, or an unreadable file, in book/src/404.md (it is rendered raw): %s\n' "$nf"; fail=1; fi
  fi
  # (h)
  if [ -d "$root/book/src" ]; then
    local bad
    bad="$(scan_directives h "$root/book/src" || true)"
    if nonblank "$bad"; then printf '(h) a directive outside the include allow-list: %s\n' "$bad"; fail=1; fi
  fi
  return "$fail"
}

# man_wrapper NAME N: the book page of the man page docs/cli/NAME.N, exactly (blank lines are not compared).
man_wrapper() {
  printf '# The %s(%s) man page\n\n' "$1" "$2"
  printf 'The committed `docs/cli/%s.%s` in its roff source, generated from `%s`'"'"'s command line; a test keeps the file equal to the generated page. In a checkout, `man ./docs/cli/%s.%s` shows it formatted.\n\n' "$1" "$2" "$1" "$1" "$2"
  printf '````text\n{{#include ../../../docs/cli/%s.%s}}\n````\n\n{{#include ../footer.md}}\n' "$1" "$2"
}

# licensing_wrapper and notices_wrapper: book/src/licensing.md and book/src/notices.md, exactly (blank lines are
# not compared).
licensing_wrapper() {
  cat <<'EOF'
# Licensing

{{#include ../../docs/LICENSING.md:published}}

{{#include footer.md}}
EOF
}

security_wrapper() {
  cat <<'EOF'
{{#include ../../docs/SECURITY.md}}

{{#include footer.md}}
EOF
}

notices_wrapper() {
  cat <<'EOF'
# Third-party notices

The notices and licence texts of the third-party software in LitePack's shipped programs, exactly as shipped in `THIRD-PARTY-NOTICES`. They belong to their authors and stay under their own terms; the documentation licence below covers only this page's own text. The file is generated by `scripts/third-party-notices.sh`.

````text
{{#include ../../THIRD-PARTY-NOTICES}}
````

{{#include footer.md}}
EOF
}

# check_anchors FILE (check (m)): prints a finding per problem in docs/LICENSING.md. mdBook takes the text between the
# lines holding `ANCHOR: published` and `ANCHOR_END: published`; this pins both markers (exact lines, in that order,
# no other ANCHOR text) and the headings between them, so an edit cannot widen what is published without failing.
# A heading is an ATX heading (`#` to `######`, after any blockquote or list-item markers), a setext underline (a
# line of only `=` or only `-`, which also catches a thematic break), or a raw <h1> to <h6> tag in any letter case.
check_anchors() {
  perl -e '
    my $p = $ARGV[0];
    open my $h, "<:raw", $p or do { print "$p: cannot be read: $!\n"; exit };
    local $/; my $s = <$h>; close $h;
    my @lines = split /\r\n|\r|\n/, $s, -1;
    my (@marks, @heads, $in);
    my ($start, $end) = ("<!-- ANCHOR: published -->", "<!-- ANCHOR_END: published -->");
    for my $i (0 .. $#lines) {
      my $l = $lines[$i];
      if ($l =~ /ANCHOR/) { push @marks, [$i + 1, $l]; }
      if (@marks == 1 && $marks[0][1] eq $start) { $in = 1 }
      if (@marks >= 2) { $in = 0 }
      if ($in && $l !~ /ANCHOR/) {
        my $s = $l;
        1 while $s =~ s/^\s{0,3}(?:>[ \t]?|[-*+][ \t]+|[0-9]+[.)][ \t]+)//;
        push @heads, $l if $s =~ /^\s*\#{1,6}/ || $s =~ /^\s*(?:=+|-+)\s*$/ || $l =~ /<h[1-6]\b/i;
      }
    }
    if (@marks != 2) { print "$p: ", scalar(@marks), " lines with ANCHOR, not 2\n"; }
    if (@marks >= 1 && $marks[0][1] ne $start) { print "$p:$marks[0][0]: the first ANCHOR line is not exactly $start\n"; }
    if (@marks >= 2 && $marks[1][1] ne $end) { print "$p:$marks[1][0]: the second ANCHOR line is not exactly $end\n"; }
    my @want = ("## Our licence", "## Dependency policy (enforced by `cargo deny check`)");
    if (join("\n", @heads) ne join("\n", @want)) {
      print "$p: the headings between the anchors are [", join(" | ", @heads), "], not [", join(" | ", @want), "]\n";
    }
  ' "$1"
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
    open my $h, "<:raw", $p or do { print "$p: cannot be read: $!\n"; next }; local $/; my $c = <$h>; close $h;
    $c =~ s/<[^>]*>//g; $c =~ s/&[#A-Za-z0-9]+;//g; $c =~ s/[^A-Za-z0-9]//g;
    for my $e (@fp) { my ($n, $s) = split /:/, $e, 2; if (index($c, $s) >= 0) { print "$p: method document line $n\n"; last; } }
  ' "$1"
}

# html_unescape is the perl routine shared by the two built checks below (named and numeric entities; an entity it
# does not know is a finding, not a silent pass).
html_unescape_perl='
  sub html_unescape {
    my ($t, $bad) = @_;
    my %n = (amp => "&", lt => "<", gt => ">", quot => "\"", apos => "\x27", nbsp => "\x{a0}");
    $t =~ s{&(?:\#([0-9]+)|\#[xX]([0-9a-fA-F]+)|([A-Za-z][A-Za-z0-9]*));}{
      defined $1 ? chr($1) : defined $2 ? chr(hex($2)) : exists $n{$3} ? $n{$3} : do { push @$bad, $3; "" }
    }ge;
    return $t;
  }
'

# built_licensing FILE: prints a finding per problem in the built licensing page.
built_licensing() {
  perl -e "$html_unescape_perl"'
    my $p = $ARGV[0];
    open my $h, "<:raw", $p or do { print "not built ($p): $!\n"; exit };
    local $/; my $s = <$h>; close $h;
    utf8::decode($s);
    my @bad;
    my @heads;
    while ($s =~ m{<h[1-6][^>]*>(.*?)</h[1-6]>}sg) { (my $t = $1) =~ s/<[^>]*>//g; push @heads, html_unescape($t, \@bad); }
    (my $plain = $s) =~ s/<[^>]*>//g;
    $plain = html_unescape($plain, \@bad);
    for my $w ("Our licence", "Dependency policy (enforced by cargo deny check)") {
      print "no heading \"$w\"\n" unless grep { $_ eq $w } @heads;
    }
    for my $w ("Components planned for Phase 1", "Patents to review", "Other compliance",
               "Dictionaries and third-party notices") {
      print "holds the unpublished text \"$w\"\n" if index($plain, $w) >= 0;
    }
  ' "$1"
}

# built_notices PAGE SOURCE: prints a finding unless the text of the <code> in the only <pre> of the built page,
# unescaped, equals the source byte for byte. The one allowed difference is a single trailing newline on the page's
# side (see the comment at the comparison) and nothing else.
built_notices() {
  perl -e "$html_unescape_perl"'
    my ($page, $src) = @ARGV;
    open my $h, "<:raw", $page or do { print "not built ($page): $!\n"; exit };
    local $/; my $s = <$h>; close $h;
    open my $f, "<:raw", $src or do { print "cannot read $src: $!\n"; exit };
    my $want = <$f>; close $f;
    my $pres = () = $s =~ /<pre[\s>]/g;
    if ($pres != 1) { print "$pres <pre> elements, not 1\n"; exit }
    if ($s !~ m{<pre[^>]*><code[^>]*>(.*?)</code></pre>}s) { print "the <pre> holds no <code> element\n"; exit }
    my $code = $1;
    utf8::decode($code);
    my @bad;
    my $got = html_unescape($code, \@bad);
    print "unknown entities: @bad\n" if @bad;
    utf8::encode($got);
    # mdBook 0.5.4 gives an exact match (the page keeps the source'"'"'s four final newlines). The allowance of one "\n"
    # is for a future mdBook: the PAGE side may lack it ($got . "\n") or carry one more ($want . "\n").
    if ($got eq $want || $got . "\n" eq $want || $got eq $want . "\n") { exit }
    my $i = 0; my $n = length($got) < length($want) ? length($got) : length($want);
    $i++ while $i < $n && substr($got, $i, 1) eq substr($want, $i, 1);
    print "differs from THIRD-PARTY-NOTICES at byte $i (page ", length($got), " bytes, file ", length($want), ")\n";
  ' "$1" "$2"
}

check_built() {
  local root="$1" built="$2" fail=0 f name html text plain
  [ -d "$built" ] || { echo "no built site at $built"; return 1; }
  for f in "$root"/docs/spec/*.md; do
    [ -f "$f" ] || continue
    name="$(basename "$f" .md)"
    html="$built/spec/$name.html"
    if [ ! -f "$html" ]; then echo "(d) not built: spec/$name.html"; fail=1; continue; fi
    if ! grep -q '^# ' "$f" || ! text="$(heading_text "$f")" || [ -z "$text" ]; then
      echo "(d) docs/spec/$name.md has no \`# \` heading to look for"; fail=1; continue
    fi
    plain="$(perl -0pe 's/<[^>]*>//g' "$html")"
    if ! grep -qF -e "$text" <<<"$plain"; then
      echo "(d) spec/$name.html lacks the heading of docs/spec/$name.md: $text"; fail=1
    fi
  done
  for f in "$root"/docs/cli/*; do
    [ -f "$f" ] || continue
    name="$(basename "$f")"
    if [[ "$name" =~ ^([A-Za-z0-9_-]+)\.md$ ]]; then
      html="$built/cli/${BASH_REMATCH[1]}.html"
      if ! grep -q '^# ' "$f" || ! text="$(heading_text "$f")" || [ -z "$text" ]; then
        echo "(d) docs/cli/$name has no \`# \` heading to look for"; fail=1; continue
      fi
    elif [[ "$name" =~ ^([A-Za-z0-9_-]+)\.([1-9])$ ]]; then
      html="$built/cli/${BASH_REMATCH[1]}-${BASH_REMATCH[2]}.html"
      # perl reads the whole file, so no writer is left to die of SIGPIPE on a large page
      text="$(perl -e '
        open my $h, "<:raw", $ARGV[0] or exit; local $/; my $s = <$h>;
        for my $l (split /\r\n|\r|\n/, $s) {
          my @w = split " ", $l;
          if ($l =~ /^\.TH / && @w >= 3) { print "$w[0] $w[1] $w[2]"; last }
        }
      ' "$f")"
      if [ -z "$text" ]; then echo "(d) docs/cli/$name has no \`.TH\` line to look for"; fail=1; continue; fi
      text="${text//&/&amp;}"; text="${text//</&lt;}"; text="${text//>/&gt;}"
    else continue; fi
    if [ ! -f "$html" ]; then echo "(d) not built: ${html#"$built"/}"; fail=1; continue; fi
    plain="$(perl -0pe 's/<[^>]*>//g' "$html")"
    if ! grep -qF -e "$text" <<<"$plain"; then
      echo "(d) ${html#"$built"/} lacks the text of docs/cli/$name: $text"; fail=1
    fi
  done
  # licensing.html: both published headings, none of the unpublished sections of docs/LICENSING.md
  local lic
  # security.html: the first `# ` heading of docs/SECURITY.md (E9-7)
  if [ -f "$root/docs/SECURITY.md" ]; then
    if ! grep -q '^# ' "$root/docs/SECURITY.md" || ! text="$(heading_text "$root/docs/SECURITY.md")" || [ -z "$text" ]; then
      echo "(d) docs/SECURITY.md has no \`# \` heading to look for"; fail=1
    elif [ ! -f "$built/security.html" ]; then
      echo "(d) not built: security.html"; fail=1
    else
      plain="$(perl -0pe 's/<[^>]*>//g' "$built/security.html")"
      if ! grep -qF -e "$text" <<<"$plain"; then
        echo "(d) security.html lacks the heading of docs/SECURITY.md: $text"; fail=1
      fi
    fi
  fi
  lic="$(built_licensing "$built/licensing.html" || true)"
  if nonblank "$lic"; then printf '(d) licensing.html: %s\n' "$lic"; fail=1; fi
  # notices.html: the code text of its only <pre>, unescaped, is THIRD-PARTY-NOTICES (the CI step of E9-8)
  lic="$(built_notices "$built/notices.html" "$root/THIRD-PARTY-NOTICES" || true)"
  if nonblank "$lic"; then printf '(d) notices.html: %s\n' "$lic"; fail=1; fi
  local hits
  hits="$(grep -ril -e "$needle" "$built" 2>/dev/null || true)"
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
  mkdir -p "$r/docs/cli" "$r/book/src/cli" "$r/site/cli"
  printf '# Tool help\n\nbody\n' > "$r/docs/cli/tool.md"
  printf '.TH tool 1  "tool 0.1" \n.SH NAME\ntool \\- a tool\n' > "$r/docs/cli/tool.1"
  printf '{{#include ../../../docs/cli/tool.md}}\n\n{{#include ../footer.md}}\n' > "$r/book/src/cli/tool.md"
  man_wrapper tool 1 > "$r/book/src/cli/tool-1.md"
  printf '<html><h1>Tool help</h1></html>\n' > "$r/site/cli/tool.html"
  printf '<html><pre><code>.TH tool 1  "tool 0.1"</code></pre></html>\n' > "$r/site/cli/tool-1.html"
  printf '# Summary\n\n- [A](spec/a.md)\n' > "$r/book/src/SUMMARY.md"
  # E9-8: the licensing source with its anchors, the notices file, their pages and built pages
  printf '# Licensing\n\nintro\n\n<!-- ANCHOR: published -->\n## Our licence\ntext\n\n## Dependency policy (enforced by `cargo deny check`)\ntext\n\n<!-- ANCHOR_END: published -->\n## Components planned for Phase 1\nmore\n' > "$r/docs/LICENSING.md"
  printf "LitePack: notices <a> & 'b' \"c\"\n\ntext line\n" > "$r/THIRD-PARTY-NOTICES"
  licensing_wrapper > "$r/book/src/licensing.md"
  notices_wrapper > "$r/book/src/notices.md"
  printf '# Security notes: the worker model\n\nbody\n' > "$r/docs/SECURITY.md"
  security_wrapper > "$r/book/src/security.md"
  printf '<html><h1 id="s">Security notes: the worker model</h1></html>\n' > "$r/site/security.html"
  printf '<html><h2 id="a"><a class="header" href="#a">Our licence</a></h2>\n<h2 id="d"><a class="header" href="#d">Dependency policy (enforced by <code>cargo deny check</code>)</a></h2></html>\n' > "$r/site/licensing.html"
  printf '<html><pre><code class="language-text">LitePack: notices &lt;a&gt; &amp; &#39;b&#39; &quot;c&quot;\n\ntext line\n</code></pre></html>\n' > "$r/site/notices.html"
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
  make_tree "$t/bs"; printf 'Text.\n\n{{#include ..\\..\\docs\\x.md}}\n' > "$t/bs/book/src/p.md"
  expect_fail h "a backslash include target" check_sources "$t/bs"
  make_tree "$t/i0"; printf -- '- [N](spec/a.md)\n- [S](sub/dir/p.md)\n' >> "$t/i0/book/src/SUMMARY.md"
  expect_pass "a normal nested SUMMARY entry" check_sources "$t/i0"
  make_tree "$t/i1"; printf -- '- [X](../../docs/x.md)\n' >> "$t/i1/book/src/SUMMARY.md"
  expect_fail i "a SUMMARY entry with .." check_sources "$t/i1"
  make_tree "$t/i2"; printf -- '- [X](/etc/x.md)\n' >> "$t/i2/book/src/SUMMARY.md"
  expect_fail i "a SUMMARY entry starting with a slash" check_sources "$t/i2"
  make_tree "$t/i3"; printf -- '- [X](sub\\x.md)\n' >> "$t/i3/book/src/SUMMARY.md"
  expect_fail i "a SUMMARY entry with a backslash" check_sources "$t/i3"
  make_tree "$t/j0"; printf '[book]\ntitle = "X"\nsrc = "src"\n\n[output.html]\nsite-url = "/x/"\n' > "$t/j0/book/book.toml"
  expect_pass "a plain book.toml" check_sources "$t/j0"
  make_tree "$t/j1"; printf '[book]\nsrc = "../docs"\n' > "$t/j1/book/book.toml"
  expect_fail j "src outside src" check_sources "$t/j1"
  make_tree "$t/j2"; printf '[book]\nsrc = "src"\n[preprocessor.x]\ncommand = "y"\n' > "$t/j2/book/book.toml"
  expect_fail j "a preprocessor table" check_sources "$t/j2"
  make_tree "$t/j3"; printf '[book]\nsrc = "src"\n[output.html]\nadditional-js = ["../docs/x.js"]\n' > "$t/j3/book/book.toml"
  expect_fail j "an additional-js key" check_sources "$t/j3"
  make_tree "$t/i4"; printf -- '- [X](&#46;&#46;&#47;&#46;&#46;&#47;docs/x.md)\n' >> "$t/i4/book/src/SUMMARY.md"
  expect_fail i "an entity-encoded SUMMARY target" check_sources "$t/i4"
  make_tree "$t/i5"; printf -- '- [X][r]\n\n[r]: ../../docs/x.md\n' >> "$t/i5/book/src/SUMMARY.md"
  expect_fail i "a reference-style SUMMARY entry" check_sources "$t/i5"
  make_tree "$t/i6"; printf -- '- [X](a(b).md)\n' >> "$t/i6/book/src/SUMMARY.md"
  expect_fail i "a balanced-parentheses SUMMARY destination" check_sources "$t/i6"
  make_tree "$t/i7"; printf -- '- [X](%%2e%%2e/x.md)\n' >> "$t/i7/book/src/SUMMARY.md"
  expect_fail i "a percent-encoded SUMMARY target" check_sources "$t/i7"
  make_tree "$t/i8"; printf -- '\t- [X](x.md)\n' >> "$t/i8/book/src/SUMMARY.md"
  expect_fail i "a tab-indented SUMMARY entry" check_sources "$t/i8"
  make_tree "$t/i9"; printf -- '<!-- x -->\n' >> "$t/i9/book/src/SUMMARY.md"
  expect_fail i "an HTML comment in SUMMARY" check_sources "$t/i9"
  make_tree "$t/i10"; printf -- '- [A](spec/a.md)\n    - [B](spec/b.md)\n' >> "$t/i10/book/src/SUMMARY.md"
  expect_pass "a SUMMARY chapter nested at four spaces" check_sources "$t/i10"
  make_tree "$t/j4"; printf '[book]\nsrc = "src"\n[ preprocessor.x ]\ncommand = "y"\n' > "$t/j4/book/book.toml"
  expect_fail j "a spaced preprocessor header" check_sources "$t/j4"
  make_tree "$t/j5"; printf '[book]\nsrc = "src"\noutput.html.additional-js = "x"\n' > "$t/j5/book/book.toml"
  expect_fail j "a dotted key under [book]" check_sources "$t/j5"
  make_tree "$t/j6"; printf '[book]\nsrc = "src"\n[output.html.fold]\nenable = true\n' > "$t/j6/book/book.toml"
  expect_fail j "an unlisted table" check_sources "$t/j6"
  make_tree "$t/j7"; printf '[book]\nsrc = "src"\nbuild-dir = "x"\n' > "$t/j7/book/book.toml"
  expect_fail j "a key not on the list" check_sources "$t/j7"
  make_tree "$t/k"
  if ln -s "../../../docs/LitePack-Method-v2.md" "$t/k/book/src/x.md" 2>/dev/null && [ -L "$t/k/book/src/x.md" ]; then
    expect_fail k "a symlink under book/" check_sources "$t/k"
  else
    echo "SKIPPED (k) self-test: cannot create a symlink here"
  fi
  make_tree "$t/w5"; printf 'Shown as text: \\{{#include ../../docs/x.md}}\n' > "$t/w5/book/src/p.md"
  expect_pass "an escaped directive in a page" check_sources "$t/w5"
  make_tree "$t/w6"; printf 'Text \377\376 bytes.\n' > "$t/w6/book/src/p.md"
  expect_fail h "a page that is not valid UTF-8" check_sources "$t/w6"
  # E9-4: the CLI pages
  make_tree "$t/ca"; printf 'copied text\n' >> "$t/ca/book/src/cli/tool.md"
  expect_fail a "a CLI wrapper with an extra line" check_sources "$t/ca"
  make_tree "$t/ca2"; printf '{{#include ../footer.md}}\n' > "$t/ca2/book/src/cli/orphan.md"
  expect_fail a "a CLI page with no source" check_sources "$t/ca2"
  make_tree "$t/ca3"; printf 'extra\n' >> "$t/ca3/book/src/cli/tool-1.md"
  expect_fail a "a man-page wrapper with an extra line" check_sources "$t/ca3"
  make_tree "$t/cb"; printf '# Other\n' > "$t/cb/docs/cli/other.md"
  expect_fail b "a docs/cli source with no page" check_sources "$t/cb"
  make_tree "$t/cb2"; printf 'x\n' > "$t/cb2/docs/cli/notes.txt"
  expect_fail b "a docs/cli file with another name" check_sources "$t/cb2"
  make_tree "$t/cb3"; mkdir -p "$t/cb3/docs/cli/sub"
  expect_fail b "a docs/cli subdirectory" check_sources "$t/cb3"
  make_tree "$t/cb4"; mkdir -p "$t/cb4/docs/cli/sub"; printf '# S\n' > "$t/cb4/docs/cli/sub/s.md"
  expect_fail b "a file in a docs/cli subdirectory" check_sources "$t/cb4"
  make_tree "$t/cb5"; printf '.TH tool 1\n' > "$t/cb5/docs/cli/tool-1.md"
  expect_fail b "two docs/cli sources for one page" check_sources "$t/cb5"
  make_tree "$t/cc"; printf 'see LitePack-Method-v2\n' >> "$t/cc/docs/cli/tool.md"
  expect_fail c "the method document's name in a docs/cli file" check_sources "$t/cc"
  make_tree "$t/cf"; printf '{{#include ../x.md}}\n' >> "$t/cf/docs/cli/tool.md"
  expect_fail f "a directive in a docs/cli .md" check_sources "$t/cf"
  make_tree "$t/cf2"; printf '{{ #include ../x.md}}\n' >> "$t/cf2/docs/cli/tool.1"
  expect_fail f "a directive in a man page" check_sources "$t/cf2"
  make_tree "$t/ch"; printf '{{#include ../../../docs/cli/tool.1}}\n\n{{#include ../footer.md}}\n' > "$t/ch/book/src/cli/tool.md"
  expect_fail h "a CLI page including the man page" check_sources "$t/ch"
  make_tree "$t/ch2"; printf 'About.\n\n{{#include ../../docs/cli/tool.md}}\n' > "$t/ch2/book/src/about.md"
  expect_fail h "an about page including a docs/cli source" check_sources "$t/ch2"
  make_tree "$t/ck"
  if ln -s "tool.md" "$t/ck/docs/cli/link.md" 2>/dev/null && [ -L "$t/ck/docs/cli/link.md" ]; then
    expect_fail k "a symlink under docs/cli" check_sources "$t/ck"
  else
    echo "SKIPPED (k) docs/cli self-test: cannot create a symlink here"
  fi
  make_tree "$t/cl"; printf '````\n' >> "$t/cl/docs/cli/tool.1"
  expect_fail l "a man-page line of four backticks" check_sources "$t/cl"
  make_tree "$t/cl2"; printf '\t````\n' >> "$t/cl2/docs/cli/tool.1"
  expect_fail l "a man-page line of four backticks after a tab" check_sources "$t/cl2"
  make_tree "$t/cl3"; printf '```\n' >> "$t/cl3/docs/cli/tool.1"
  expect_pass "a man-page line of three backticks" check_sources "$t/cl3"
  make_tree "$t/cd"; printf '<html>other</html>\n' > "$t/cd/site/cli/tool.html"
  expect_fail d "a built CLI page without its heading" check_built "$t/cd" "$t/cd/site"
  make_tree "$t/cd2"; printf '<html>other</html>\n' > "$t/cd2/site/cli/tool-1.html"
  expect_fail d "a built man page without its .TH words" check_built "$t/cd2" "$t/cd2/site"
  make_tree "$t/cd3"; rm "$t/cd3/site/cli/tool-1.html"
  expect_fail d "a missing built man page" check_built "$t/cd3" "$t/cd3/site"
  # E9-4 fix round 1
  make_tree "$t/dv1"; printf 'no heading here\n' > "$t/dv1/docs/spec/a.md"
  expect_fail d "a spec source without a heading" check_built "$t/dv1" "$t/dv1/site"
  make_tree "$t/dv2"; printf 'no heading here\n' > "$t/dv2/docs/cli/tool.md"
  expect_fail d "a CLI source without a heading" check_built "$t/dv2" "$t/dv2/site"
  make_tree "$t/dv3"; printf '.SH NAME\ntool\n' > "$t/dv3/docs/cli/tool.1"
  expect_fail d "a man page without a .TH line" check_built "$t/dv3" "$t/dv3/site"
  make_tree "$t/lc1"; printf 'x\r````\rmore\n' >> "$t/lc1/docs/cli/tool.1"
  expect_fail l "four backticks after a lone CR" check_sources "$t/lc1"
  make_tree "$t/lc2"; printf '````\r\n' >> "$t/lc2/docs/cli/tool.1"
  expect_fail l "four backticks ended by CRLF" check_sources "$t/lc2"
  make_tree "$t/lc3"; printf '   ````\n' >> "$t/lc3/docs/cli/tool.1"
  expect_fail l "four backticks indented by three spaces" check_sources "$t/lc3"
  make_tree "$t/lc4"; printf '`````\n' >> "$t/lc4/docs/cli/tool.1"
  expect_fail l "five backticks" check_sources "$t/lc4"
  make_tree "$t/hs"; printf '{{#include ../../../docs/cli/tool.md}}\n\n{{#include ../footer.md}}\n' > "$t/hs/book/src/spec/a.md"
  expect_fail h "a spec page including a docs/cli source" check_sources "$t/hs"
  # E9-4 fix round 2
  make_tree "$t/big"
  perl -e 'print ".TH tool 1  \"tool 0.1\"\n"; print "filler line of man page text without anything special\n" x 50000;' \
    > "$t/big/docs/cli/tool.1"
  if [ "$(wc -c < "$t/big/docs/cli/tool.1")" -le 2000000 ]; then echo "self-test FAILED: the big man page is under 2 MB"; exit 1; fi
  expect_pass "a man page over 2 MB, sources" check_sources "$t/big"
  expect_pass "a man page over 2 MB, built" check_built "$t/big" "$t/big/site"
  make_tree "$t/ur"; chmod 000 "$t/ur/docs/cli/tool.md" 2>/dev/null || true
  if [ ! -r "$t/ur/docs/cli/tool.md" ]; then
    expect_fail f "an unreadable docs/cli file" check_sources "$t/ur"
    chmod 644 "$t/ur/docs/cli/tool.md"
  else
    echo "SKIPPED (f) unreadable-file self-test: chmod 000 does not make a file unreadable here"
  fi
  # E9-8: the licensing and notices pages
  make_tree "$t/la"; printf 'extra\n' >> "$t/la/book/src/licensing.md"
  expect_fail a "a licensing wrapper with an extra line" check_sources "$t/la"
  make_tree "$t/la2"; printf 'extra\n' >> "$t/la2/book/src/notices.md"
  expect_fail a "a notices wrapper with an extra line" check_sources "$t/la2"
  make_tree "$t/lh"; printf '# Licensing\n\n{{#include ../../docs/LICENSING.md}}\n\n{{#include footer.md}}\n' > "$t/lh/book/src/licensing.md"
  expect_fail h "licensing.md including the whole docs/LICENSING.md" check_sources "$t/lh"
  make_tree "$t/lh2"; printf 'About.\n\n{{#include ../../THIRD-PARTY-NOTICES}}\n' > "$t/lh2/book/src/about.md"
  expect_fail h "about.md including THIRD-PARTY-NOTICES" check_sources "$t/lh2"
  make_tree "$t/lh3"; printf 'About.\n\n{{#include ../../docs/LICENSING.md:published}}\n' > "$t/lh3/book/src/about.md"
  expect_fail h "about.md including the published anchor" check_sources "$t/lh3"
  make_tree "$t/lf"; printf '{{#include ../x.md}}\n' >> "$t/lf/docs/LICENSING.md"
  expect_fail f "a directive in docs/LICENSING.md" check_sources "$t/lf"
  make_tree "$t/lf2"; printf '{{ #include ../x.md}}\n' >> "$t/lf2/THIRD-PARTY-NOTICES"
  expect_fail f "a directive in THIRD-PARTY-NOTICES" check_sources "$t/lf2"
  make_tree "$t/lc"; printf 'see LitePack-Method-v2\n' >> "$t/lc/docs/LICENSING.md"
  expect_fail c "the method document's name in docs/LICENSING.md" check_sources "$t/lc"
  make_tree "$t/lc2"; printf 'see LitePack-Method-v2\n' >> "$t/lc2/THIRD-PARTY-NOTICES"
  expect_fail c "the method document's name in THIRD-PARTY-NOTICES" check_sources "$t/lc2"
  make_tree "$t/ll"; printf '````\n' >> "$t/ll/THIRD-PARTY-NOTICES"
  expect_fail l "a four-backtick line in THIRD-PARTY-NOTICES" check_sources "$t/ll"
  make_tree "$t/ll2"; printf '```\n' >> "$t/ll2/THIRD-PARTY-NOTICES"
  expect_pass "a three-backtick line in THIRD-PARTY-NOTICES" check_sources "$t/ll2"
  make_tree "$t/m1"; perl -ni -e 'print unless /ANCHOR_END/' "$t/m1/docs/LICENSING.md"
  expect_fail m "no end anchor" check_sources "$t/m1"
  make_tree "$t/m2"; printf '<!-- ANCHOR: published -->\n' >> "$t/m2/docs/LICENSING.md"
  expect_fail m "a second start anchor" check_sources "$t/m2"
  make_tree "$t/m3"; perl -pi -e 's/^text$/text\n### Extra heading/ if $. == 7' "$t/m3/docs/LICENSING.md"
  expect_fail m "a third heading inside the region" check_sources "$t/m3"
  make_tree "$t/m4"; perl -pi -e 's/^<!-- ANCHOR: published -->$//' "$t/m4/docs/LICENSING.md"
  expect_fail m "no start anchor" check_sources "$t/m4"
  make_tree "$t/m5"; perl -pi -e 's/^<!-- ANCHOR_END: published -->$/<!-- ANCHOR_END: other -->/' "$t/m5/docs/LICENSING.md"
  expect_fail m "an end anchor of another name" check_sources "$t/m5"
  make_tree "$t/m6"; perl -pi -e 's/^## Our licence$/## Our licences/' "$t/m6/docs/LICENSING.md"
  expect_fail m "a renamed published heading" check_sources "$t/m6"
  make_tree "$t/m7"; perl -pi -e 's/^(<!-- ANCHOR: published -->)$/<!-- ANCHOR_END: published -->\n$1/' "$t/m7/docs/LICENSING.md"
  expect_fail m "the end anchor before the start anchor" check_sources "$t/m7"
  make_tree "$t/j8"; printf '[book]\nsrc = "src"\n[output.html.search.chapter]\n"notices.md" = { enable = false }\n' > "$t/j8/book/book.toml"
  expect_pass "the search-chapter exclusion of notices.md" check_sources "$t/j8"
  make_tree "$t/j9"
  printf '[book]\nsrc = "src"\n[output.html.search.chapter]\n"notices.md" = { enable = false }\n"x.md" = { enable = false }\n' > "$t/j9/book/book.toml"
  expect_fail j "a second key under the search-chapter header" check_sources "$t/j9"
  make_tree "$t/j10"; printf '[book]\nsrc = "src"\n[output.html.search.chapter]\n"../x.md" = { enable = false }\n' > "$t/j10/book/book.toml"
  expect_fail j "another path in the search-chapter line" check_sources "$t/j10"
  make_tree "$t/j11"; printf '[book]\nsrc = "src"\n[output.html.search.chapter]\n"notices.md" = { enable = true }\n' > "$t/j11/book/book.toml"
  expect_fail j "another value in the search-chapter line" check_sources "$t/j11"
  make_tree "$t/j12"; printf '[book]
src = "src"
[output.html.playground]
runnable = false
' > "$t/j12/book/book.toml"
  expect_pass "the playground header with runnable = false" check_sources "$t/j12"
  make_tree "$t/j13"; printf '[book]
src = "src"
[output.html.playground]
runnable = true
' > "$t/j13/book/book.toml"
  expect_fail j "runnable = true under the playground header" check_sources "$t/j13"
  make_tree "$t/j14"; printf '[book]
src = "src"
[output.html.playground]
runnable = false
editable = false
' > "$t/j14/book/book.toml"
  expect_fail j "another key under the playground header" check_sources "$t/j14"
  make_tree "$t/bl"; printf '<html><h2>Our licence</h2></html>\n' > "$t/bl/site/licensing.html"
  expect_fail d "a licensing page without the second heading" check_built "$t/bl" "$t/bl/site"
  make_tree "$t/bl2"; printf '<html><h2>x</h2></html>\n' > "$t/bl2/site/licensing.html"
  expect_fail d "a licensing page without a heading" check_built "$t/bl2" "$t/bl2/site"
  make_tree "$t/bl3"; printf '<h2>Patents to review (counsel)</h2>\n' >> "$t/bl3/site/licensing.html"
  expect_fail d "a licensing page holding Patents to review" check_built "$t/bl3" "$t/bl3/site"
  make_tree "$t/bl4"; printf '<p>Other compliance</p>\n' >> "$t/bl4/site/licensing.html"
  expect_fail d "a licensing page holding Other compliance" check_built "$t/bl4" "$t/bl4/site"
  make_tree "$t/bn"
  printf '<html><pre><code class="language-text">LitePack: notices &lt;a&gt; &amp; &#39;b&#39; &quot;c&quot;\n\ntext linE\n</code></pre></html>\n' > "$t/bn/site/notices.html"
  expect_fail d "a notices page that differs by one byte" check_built "$t/bn" "$t/bn/site"
  make_tree "$t/bn2"
  printf '<html><pre><code class="language-text">{{#include ../../THIRD-PARTY-NOTICES}}\n</code></pre></html>\n' > "$t/bn2/site/notices.html"
  expect_fail d "a notices page that still holds the include text" check_built "$t/bn2" "$t/bn2/site"
  # E9-7: the security page
  make_tree "$t/sa"; printf 'extra\n' >> "$t/sa/book/src/security.md"
  expect_fail a "a security wrapper with an extra line" check_sources "$t/sa"
  make_tree "$t/sh"; printf 'About.\n\n{{#include ../../docs/SECURITY.md}}\n' > "$t/sh/book/src/about.md"
  expect_fail h "about.md including docs/SECURITY.md" check_sources "$t/sh"
  make_tree "$t/sf"; printf '{{#include ../x.md}}\n' >> "$t/sf/docs/SECURITY.md"
  expect_fail f "a directive in docs/SECURITY.md" check_sources "$t/sf"
  make_tree "$t/sc"; printf 'see LitePack-Method-v2\n' >> "$t/sc/docs/SECURITY.md"
  expect_fail c "the method document's name in docs/SECURITY.md" check_sources "$t/sc"
  make_tree "$t/sk"; mv "$t/sk/docs/SECURITY.md" "$t/sk/real.md"
  if ln -s ../real.md "$t/sk/docs/SECURITY.md" 2>/dev/null && [ -L "$t/sk/docs/SECURITY.md" ]; then
    expect_fail k "docs/SECURITY.md as a symlink" check_sources "$t/sk"
  else
    echo "SKIPPED (k) docs/SECURITY.md self-test: cannot create a symlink here"
  fi
  make_tree "$t/sb"; printf '<html><h1>Other</h1></html>\n' > "$t/sb/site/security.html"
  expect_fail d "a security page without the heading of docs/SECURITY.md" check_built "$t/sb" "$t/sb/site"
  make_tree "$t/bn3"; printf '<pre></pre>\n' >> "$t/bn3/site/notices.html"
  expect_fail d "a notices page with a second pre" check_built "$t/bn3" "$t/bn3/site"
  make_tree "$t/bn4"
  printf '<html><pre><code>LitePack: notices &lt;a&gt; &amp; &#x27;b&#39; &quot;c&quot; &bogus;\n\ntext line\n</code></pre></html>\n' > "$t/bn4/site/notices.html"
  expect_fail d "a notices page with an unknown entity" check_built "$t/bn4" "$t/bn4/site"
  make_tree "$t/bn5"
  printf '<html><pre><code>LitePack: notices &lt;a&gt; &amp; &#x27;b&#39; &quot;c&quot;\n\ntext line</code></pre></html>\n' > "$t/bn5/site/notices.html"
  expect_pass "a notices page with numeric entities and one trailing newline less" check_built "$t/bn5" "$t/bn5/site"
  make_tree "$t/bn6"; printf '\n' >> "$t/bn6/THIRD-PARTY-NOTICES"; printf '\n' >> "$t/bn6/THIRD-PARTY-NOTICES"
  expect_fail d "a notices page two trailing newlines short" check_built "$t/bn6" "$t/bn6/site"
  make_tree "$t/bk"
  rm "$t/bk/THIRD-PARTY-NOTICES"; printf 'x\n' > "$t/bk/real-notices"
  if ln -s "real-notices" "$t/bk/THIRD-PARTY-NOTICES" 2>/dev/null && [ -L "$t/bk/THIRD-PARTY-NOTICES" ]; then
    expect_fail k "a symlinked THIRD-PARTY-NOTICES" check_sources "$t/bk"
  else
    echo "SKIPPED (k) THIRD-PARTY-NOTICES self-test: cannot create a symlink here"
  fi
  # E9-8 fix round 1
  make_tree "$t/m8"; perl -pi -e 's/^text$/text\n-/ if $. == 7' "$t/m8/docs/LICENSING.md"
  expect_fail m "a single-dash setext underline in the region" check_sources "$t/m8"
  make_tree "$t/m9"; perl -pi -e 's/^text$/text\n> ## X/ if $. == 7' "$t/m9/docs/LICENSING.md"
  expect_fail m "a heading inside a blockquote in the region" check_sources "$t/m9"
  make_tree "$t/m10"; perl -pi -e 's/^text$/text\n- ## X/ if $. == 7' "$t/m10/docs/LICENSING.md"
  expect_fail m "a heading inside a list item in the region" check_sources "$t/m10"
  make_tree "$t/m11"; perl -pi -e 's/^text$/text\n<H2>X<\/H2>/ if $. == 7' "$t/m11/docs/LICENSING.md"
  expect_fail m "a raw upper-case h2 tag in the region" check_sources "$t/m11"
  make_tree "$t/m12"; perl -pi -e 's/^text$/text\n1) # X/ if $. == 7' "$t/m12/docs/LICENSING.md"
  expect_fail m "a heading inside a numbered list item in the region" check_sources "$t/m12"
  make_tree "$t/nc"; printf 'x\0LitePack-Method\n' >> "$t/nc/THIRD-PARTY-NOTICES"
  expect_fail c "the method document's name after a NUL byte in THIRD-PARTY-NOTICES" check_sources "$t/nc"
  make_tree "$t/ne"; printf 'x\0LitePack-Method\n' > "$t/ne/site/n.html"
  expect_fail e "a built file with a NUL byte and the method document's name" check_built "$t/ne" "$t/ne/site"
  # The 404 page (n)
  make_tree "$t/n0"; printf '**Document not found (404).** Text.\n' > "$t/n0/book/src/404.md"
  expect_pass "a plain 404 page" check_sources "$t/n0"
  make_tree "$t/n1"; printf '**Document not found (404).**\n\n{{#include footer.md}}\n' > "$t/n1/book/src/404.md"
  expect_fail n "a directive in the 404 page" check_sources "$t/n1"
  make_tree "$t/n2"; printf 'Not found.\n\n{{ #include x }}\n' > "$t/n2/book/src/404.md"
  expect_fail n "a spaced directive in the 404 page" check_sources "$t/n2"
  make_tree "$t/n3"; printf 'Not found.\n\n\\{{#x}}\n' > "$t/n3/book/src/404.md"
  expect_fail n "an escaped directive in the 404 page" check_sources "$t/n3"
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
