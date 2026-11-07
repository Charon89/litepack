#!/usr/bin/env bash
# E9-8: an offline link check of the built docs site. Nothing is fetched.
#   bash scripts/check-book-links.sh DIR          check the built site (DIR is book/book)
#   bash scripts/check-book-links.sh --self-test  check that each rule fails on a crafted site
# Values: every href and src attribute of every .html file (comments, and the bodies of script and style elements,
# are skipped), HTML entities decoded, then percent-encoding.
#   https: and mailto: are counted and their hosts listed, never fetched; http: fails; data: passes in src only;
#   any other scheme fails, and so does //host without a scheme.
#   A relative value resolves against the page's directory, or against the site root when the page has a
#   <base href>, which must be the site-url below (mdBook writes it into 404.html from book.toml's site-url).
#   ?query is dropped; an empty path is the page itself; a path ending in / means its index.html; the target must be a
#   file of the site; a path that climbs above the site root fails. /litepack/... resolves under the site root and any
#   other value starting with / fails. A #fragment on an HTML target must be an id or name attribute of that file.
#   The files are listed once and looked up as exact, case-sensitive strings: GitHub Pages is case-sensitive, NTFS
#   is not, so a link that works on a developer's machine can still be dead online.
set -euo pipefail

# Keep in step with site-url in book/book.toml.
site_url="/litepack/"

check_site() {
  perl -e '
    use strict; use warnings;
    use File::Find;
    my ($dir, $siteurl) = @ARGV;
    unless (-d $dir) { print "no built site at $dir\n"; exit 1 }
    $dir =~ s{/+\z}{} if length($dir) > 1;

    # The site, listed once: exact, case-sensitive names.
    my (%files, @html);
    find({ no_chdir => 1, wanted => sub {
      my $p = $File::Find::name;
      return if $p eq $dir;
      (my $rel = $p) =~ s{\A\Q$dir\E/}{};
      if (-f $p) { $files{$rel} = 1; push @html, $rel if $rel =~ /\.html\z/; }
    } }, $dir);
    @html = sort @html;

    my %ent = (amp => "&", lt => "<", gt => ">", quot => "\"", apos => "\x27", nbsp => "\x{a0}");
    sub decode_entities {
      my $t = shift;
      $t =~ s{&(?:\#([0-9]+)|\#[xX]([0-9a-fA-F]+)|([A-Za-z][A-Za-z0-9]*));}{
        my $r;
        if (defined $1 || defined $2) {
          my $n = defined $1 ? $1 : hex($2);
          $r = ($n > 0 && $n <= 0x10FFFF) ? chr($n) : "\x{FFFD}"; utf8::encode($r);
        } elsif (exists $ent{$3}) { $r = $ent{$3}; utf8::encode($r); }
        else { $r = $& }
        $r;
      }ge;
      return $t;
    }

    # [tag, name, decoded value] for every attribute of every start tag in a file.
    sub attributes {
      my $html = shift;
      $html =~ s/<!--.*?-->//sg;
      $html =~ s{(<(script|style)\b[^>]*>).*?(</\2\s*>)}{$1$3}sgi;
      my @out;
      while ($html =~ /<([A-Za-z][A-Za-z0-9-]*)((?:[^>"\x27]|"[^"]*"|\x27[^\x27]*\x27)*)>/g) {
        my ($tag, $as) = (lc $1, $2);
        while ($as =~ /([^\s"\x27<>\/=]+)(?:\s*=\s*(?:"([^"]*)"|\x27([^\x27]*)\x27|([^\s"\x27=<>`]+)))?/g) {
          my $v = defined $2 ? $2 : defined $3 ? $3 : defined $4 ? $4 : "";
          push @out, [$tag, lc $1, decode_entities($v)];
        }
      }
      return @out;
    }

    my (%attrs_of, %ids);
    sub page_attrs {
      my $rel = shift;
      return $attrs_of{$rel} //= do {
        open my $h, "<:raw", "$dir/$rel" or die "$dir/$rel: $!\n";
        local $/; my $s = <$h>; close $h;
        [ attributes($s) ];
      };
    }
    sub page_ids {
      my $rel = shift;
      return $ids{$rel} //= { map { ($_->[2] => 1) } grep { $_->[1] eq "id" || $_->[1] eq "name" } @{ page_attrs($rel) } };
    }

    my ($checked, $internal, $frags, $nex, %hosts, @bad) = (0, 0, 0, 0);
    my $mailto = 0;
    my $root_prefix = $siteurl;
    (my $site_noslash = $siteurl) =~ s{/\z}{};

    for my $page (@html) {
      my @attr = @{ page_attrs($page) };
      my $base;
      for my $a (@attr) { $base = $a->[2] if $a->[0] eq "base" && $a->[1] eq "href" && !defined $base }
      if (defined $base && $base ne $siteurl) { push @bad, "$page: base href \"$base\" is not the site-url \"$siteurl\""; }
      my @pdir = split m{/}, $page; pop @pdir;
      my @basedir = defined $base && $base eq $siteurl ? () : @pdir;
      for my $a (@attr) {
        my ($tag, $name, $v) = @$a;
        next unless $name eq "href" || $name eq "src";
        next if $tag eq "base" && $name eq "href";
        $checked++;
        my $where = "$page: <$tag $name=\"$v\">";
        (my $u = $v) =~ s/\A[ \t\r\n\f]+|[ \t\r\n\f]+\z//g;
        if ($u =~ /[\x00-\x1f\x7f]/) { push @bad, "$where: a control character"; next }
        if ($u =~ /\A([A-Za-z][A-Za-z0-9+.-]*):/) {
          my $scheme = lc $1;
          if ($scheme eq "https") {
            if ($u =~ m{\Ahttps://(?:[^/?\#@]*@)?([^/?\#:]+)}i) { $hosts{lc $1}++; $nex++; }
            else { push @bad, "$where: an https link with no host" }
          } elsif ($scheme eq "mailto") { $mailto++; $nex++; }
          elsif ($scheme eq "http") { push @bad, "$where: http: is not allowed, use https:" }
          elsif ($scheme eq "data") { if ($name eq "src") { $internal++ } else { push @bad, "$where: data: is allowed in src only" } }
          else { push @bad, "$where: the scheme $scheme: is not allowed" }
          next;
        }
        if ($u =~ m{\A//}) { push @bad, "$where: a link with a host and no scheme"; next }
        my ($path, $frag) = ($u, undef);
        if ($path =~ s/\#(.*)\z//s) { $frag = $1 }
        $path =~ s/\?.*\z//s;
        $path =~ s/%([0-9A-Fa-f]{2})/chr(hex($1))/ge;
        if (defined $frag) { $frag =~ s/%([0-9A-Fa-f]{2})/chr(hex($1))/ge; }
        my $target;
        if ($path eq "") { $target = $page }
        else {
          my @st;
          if ($path =~ m{\A/}) {
            unless ($path eq $site_noslash || index($path, $root_prefix) == 0) {
              push @bad, "$where: a root-relative link outside $siteurl"; next;
            }
            $path = $path eq $site_noslash ? "" : substr($path, length($root_prefix));
          } else { @st = @basedir }
          my @comp = split m{/}, $path, -1;
          my $last = @comp ? $comp[-1] : "";
          my $dirlink = $last eq "" || $last eq "." || $last eq "..";
          my $ok = 1;
          for my $c (split m{/}, $path) {
            next if $c eq "" || $c eq ".";
            if ($c eq "..") { if (@st) { pop @st } else { $ok = 0; last } } else { push @st, $c }
          }
          unless ($ok) { push @bad, "$where: climbs above the site root"; next }
          push @st, "index.html" if $dirlink;
          $target = join "/", @st;
          unless ($files{$target}) { push @bad, "$where: no such file in the site: $target"; next }
        }
        $internal++;
        if (defined $frag && $frag ne "" && lc $frag ne "top" && $target =~ /\.html\z/) {
          $frags++;
          unless (page_ids($target)->{$frag}) { push @bad, "$where: no id or name \"$frag\" in $target" }
        }
      }
    }

    my @h = sort keys %hosts;
    printf "link check: %d html files, %d href/src values; %d site-internal, %d external not fetched (%d mailto), %d fragments\n",
      scalar(@html), $checked, $internal, $nex, $mailto, $frags;
    print "external hosts (", scalar(@h), "): ", join(", ", @h), "\n";
    if (@bad) { print "$_\n" for @bad; printf "link check: %d problems\n", scalar(@bad); exit 1 }
    print "link check: ok\n";
  ' "$1" "$site_url"
}

# --- self-test -------------------------------------------------------------------------------------------------

# make_site DIR: a small site where every rule is met.
make_site() {
  local d="$1"
  mkdir -p "$d/cli" "$d/js"
  printf '<html><head><script src="js/a.js"></script></head><body>\n' > "$d/index.html"
  printf '<a href="cli/">dir</a> <a href="cli/index.html#top">top</a> <a href="a%%20b.html">space</a>\n' >> "$d/index.html"
  printf '<a href="page.html#sec">frag</a> <a href="#own">self</a> <h2 id="own">x</h2> <a href="page.html?q=1">q</a>\n' >> "$d/index.html"
  printf '<a href="https://example.com/x">e</a> <a href="https://www.test.org:8443/y">e2</a> <a href="mailto:a@b.c">m</a>\n' >> "$d/index.html"
  printf '<img src="data:image/png;base64,AAAA"> <a href="/litepack/page.html#sec">root</a> <a href="">here</a>\n' >> "$d/index.html"
  printf '<script>var s = "<a href=\\"nowhere.html\\">";</script><!-- <a href="gone.html"> --></body></html>\n' >> "$d/index.html"
  printf '<html><a href="../index.html">up</a> <a href="index.html">me</a> <a href=".">dot</a></html>\n' > "$d/cli/index.html"
  printf '<html><h1 id="sec">S</h1> <a name="old">o</a> <a href="cli/index.html">c</a></html>\n' > "$d/page.html"
  printf '<html>space</html>\n' > "$d/a b.html"
  printf 'var x = 1;\n' > "$d/js/a.js"
  printf '<html><head><base href="/litepack/"></head><a href="page.html#old">p</a> <a href="cli/">c</a></html>\n' > "$d/404.html"
}

# expect_bad NAME PHRASE LINE: a copy of the good site with the extra page bad.html holding LINE must fail, and the
# output must carry PHRASE.
expect_bad() {
  local name="$1" phrase="$2" line="$3" out
  rm -rf "$t/$name"; make_site "$t/$name"
  printf '%s\n' "$line" > "$t/$name/bad.html"
  if out="$(check_site "$t/$name" 2>&1)"; then echo "self-test FAILED: $name was not caught"; exit 1; fi
  if ! grep -qF -e "$phrase" <<<"$out"; then echo "self-test FAILED: $name failed, but not with \"$phrase\": $out"; exit 1; fi
  echo "self-test ok: $name is caught"
}

self_test() {
  t="$(mktemp -d)"; trap 'rm -rf "${t:-}"' EXIT
  local out
  make_site "$t/good"
  if ! out="$(check_site "$t/good" 2>&1)"; then echo "self-test FAILED: the good site does not pass: $out"; exit 1; fi
  echo "self-test ok: the good site passes (site-url base, %20 name, directory link, fragments, externals)"
  grep -qF -e "3 external not fetched (1 mailto)" <<<"$out" || { echo "self-test FAILED: external count: $out"; exit 1; }
  grep -qF -e "example.com, www.test.org" <<<"$out" || { echo "self-test FAILED: external hosts: $out"; exit 1; }
  echo "self-test ok: externals are counted and their hosts listed"
  if out="$(check_site "$t/none" 2>&1)"; then echo "self-test FAILED: a missing site passes"; exit 1; fi
  expect_bad missing-file "no such file" '<a href="nothing.html">x</a>'
  expect_bad letter-case "no such file" '<a href="Page.html">x</a>'
  expect_bad letter-case-dir "no such file" '<a href="CLI/">x</a>'
  expect_bad missing-fragment "no id or name" '<a href="page.html#nope">x</a>'
  expect_bad missing-own-fragment "no id or name" '<a href="#nope">x</a>'
  expect_bad fragment-case "no id or name" '<a href="page.html#SEC">x</a>'
  expect_bad climb "climbs above" '<a href="../x.html">x</a>'
  expect_bad climb-deep "climbs above" '<a href="cli/../../index.html">x</a>'
  expect_bad http "http: is not allowed" '<a href="http://example.com/">x</a>'
  expect_bad javascript "scheme javascript:" '<a href="javascript:alert(1)">x</a>'
  expect_bad javascript-entity "scheme javascript:" '<a href="java&#115;cript:alert(1)">x</a>'
  expect_bad ftp "scheme ftp:" '<a href="ftp://example.com/">x</a>'
  expect_bad data-href "allowed in src only" '<a href="data:text/html,x">x</a>'
  expect_bad no-scheme-host "host and no scheme" '<a href="//example.com/x">x</a>'
  expect_bad root-outside "outside /litepack/" '<a href="/other/page.html">x</a>'
  expect_bad root-bare "outside /litepack/" '<a href="/page.html">x</a>'
  expect_bad bad-base "is not the site-url" '<base href="/other/"><a href="page.html">x</a>'
  expect_bad relative-base "is not the site-url" '<base href="sub/"><a href="page.html">x</a>'
  expect_bad control-char "a control character" "$(printf '<a href="pa\001ge.html">x</a>')"
  expect_bad https-no-host "no host" '<a href="https:page.html">x</a>'
  expect_bad src-missing "no such file" '<img src="img/none.png">'
  expect_bad percent-missing "no such file" '<a href="a%20c.html">x</a>'
  rm -rf "$t/base-ok"; make_site "$t/base-ok"
  printf '<base href="/litepack/"><a href="cli/index.html">x</a>\n' > "$t/base-ok/sub.html"
  mkdir -p "$t/base-ok/d"; printf '<base href="/litepack/"><a href="page.html#sec">x</a> <a href="/litepack/">r</a>\n' > "$t/base-ok/d/p.html"
  if ! out="$(check_site "$t/base-ok" 2>&1)"; then echo "self-test FAILED: a page with the site-url base does not pass: $out"; exit 1; fi
  echo "self-test ok: a page in a subdirectory with the site-url base resolves from the site root"
  echo "self-test: all checks proved"
}

case "${1:-}" in
  --self-test) self_test ;;
  "") echo "usage: check-book-links.sh DIR | --self-test"; exit 2 ;;
  -*) echo "unknown argument: $1"; exit 2 ;;
  *) check_site "$1" ;;
esac
