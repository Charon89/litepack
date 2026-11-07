# LitePack documentation book

An mdBook book, published at <https://charon89.github.io/litepack/> by the `docs` workflow on every push to
`main`. To build it locally install the pinned mdBook (`cargo install mdbook --version 0.5.4 --locked`; keep the
version in step with `.github/workflows/docs.yml`), then from the repository root run `bash scripts/check-book-footer.sh`
and `mdbook build book` (output in `book/book/`, git-ignored) or `mdbook serve book`. Every chapter listed in
`src/SUMMARY.md` ends with `{{#include footer.md}}`, which carries the licence line; the check enforces it.

The format specification pages (`src/spec/`) are made by include: each holds only `{{#include ../../../docs/spec/<name>}}`
and the footer include, with the same file name as its source in `docs/spec/`, so the specification's links between its
own files work in the book. Never copy the text. What the checks cover:

- The structural guard is the include allow-list (h) with (f). (h): in every file under `src/`, a `{{#...}}` directive
  is only an `{{#include}}` that resolves inside `src/` (such as the footer), or the exact wrapper include in
  `src/spec/<name>.md`, `src/cli/<name>.md` or `src/cli/<name>-<N>.md`; `rustdoc_include`, `playground` and the rest
  fail. (f): there is no `{{#` inside `docs/spec/` or `docs/cli/`.
  So nothing outside `src/`, `docs/spec/` and `docs/cli/` can reach the site by inclusion. Both mirror mdBook 0.5.4's directive
  pattern (whitespace and line breaks inside a directive count; a file that is not UTF-8 fails); an mdBook upgrade
  must re-check it.
- The other structural guards: (i) and (j) are allow-lists of exact line shapes, because mdBook's Markdown and TOML
  parsers accept many spellings of the same thing: every line of `src/SUMMARY.md` is blank, a title, `---` or a chapter
  link with a plain relative `.md` path, and every line of `book.toml` is a comment, one of three table headers or a
  listed key with a plain value (`src` must be `"src"`). A new SUMMARY title character or `book.toml` key is added to
  `allow_scan` in the script only after checking that it cannot bring a file from outside `src/` into the site. (k) no symlink under `book/` or `docs/spec/`; (h) also rejects a backslash in an
  include target. The names (c, e) and the content fingerprint (g) are the second line of defence. Residual risk: text
  reworded by hand into the book's own pages cannot be detected mechanically; the guards stop includes, routes and
  verbatim copies.
- `bash scripts/check-book-sources.sh` (before the build) also checks that the wrappers are pure includes (a page that
  is more than those two lines fails), that every `docs/spec/*.md` has a page, and that no file under `book/` names
  the unpublished method document, by content or by file name, in any letter case.
- `bash scripts/check-book-sources.sh --built book/book` (after the build): each spec page holds its source's first
  heading (mdBook only warns when an include fails, so this is what turns a missing include into a failure); no built
  file names the method document. Those names and a content fingerprint (g) of the method document (its heading and
  every long line) are a second line of defence; the fingerprint is a sample that a reworded copy can evade.
- `--self-test` proves each check fails, with its tag, on a crafted tree.

The command-line reference (`src/cli/`) is made the same way, from `docs/cli/`, which `crates/lpk-cli/tests/docs.rs`
(E3-7) keeps equal to the text generated from `lpk`'s command line. Never edit those files by hand; regenerate them with
`LPK_CLI_DOCS_WRITE=1 cargo test -p lpk-cli --test docs`.

- A source `docs/cli/NAME.md` has the page `src/cli/NAME.md`: its include (`{{#include ../../../docs/cli/NAME.md}}`)
  and the footer include.
- A man page `docs/cli/NAME.N` (N one digit, 1 to 9) has the page `src/cli/NAME-N.md` (the SUMMARY allow-list admits no
  dot in a path component): a heading, one sentence, the include inside a fence of four backticks (`text`), and the
  footer include. The script's `man_wrapper` holds the expected text; no text is copied.
- (a) each page under `src/cli/` equals the wrapper of an existing source; (b) `docs/cli/` holds only `NAME.md` and
  `NAME.N` files directly in it, each with its page; (c), (f) and (k) also cover `docs/cli/`; (h) admits the two wrapper
  includes, each only in its own page; (l) no line of a man page begins, after spaces or tabs, with four or more
  backticks (that is the only text that could close the fence); (l) splits lines at LF, CRLF and a lone CR, as
  CommonMark does, and reports FILE:LINE counted that way. The built check (d) looks for the first `# ` heading of
  each `.md` (spec and CLI) and for the first three words of the man page's first `.TH` line; it fails when a source has
  no such heading or `.TH` line to look for.

The benchmark pages (`src/bench/results.md` and `src/bench/corpus.md`, E9-6) are generated files, not includes. Never
edit them by hand; regenerate them with `cargo run -p lpk-bench -- book` from the repository root.

- The inputs are named in `bench/site.toml`: the results directories of the published run (the ones of the latest
  full-profile report in `bench/reports/`, in the report's order), the mixes file, and the corpus files. Publishing
  another run means editing that file and regenerating.
- The results page holds the report's header, the per-class tables, the per-mix tables, its sources and its caveats,
  with each number followed by the index of the result file it was read from. It holds no gate verdict, no host name
  and no antivirus detail: a results directory `<date>-<host>[-<n>]` appears as `<date> run <n>`. The corpus page
  holds each class's description (from the class table of `docs/CORPUS.md`), the lock's downloads and bytes per class
  and per source (from `bench/corpus.lock` and `bench/corpus-sources.toml`), and the built corpus's totals from the
  published run. Every figure is read from a committed file. A class description may hold a number word from the
  spec's prose ("Three working-tree snapshots"), which says how the class is built, never a measurement; a digit in
  a description fails the generator, so no count or size from the prose reaches the page.
- `cargo run -p lpk-bench -- book --check` regenerates the pages in memory and fails, naming the first differing line,
  when the committed pages differ. The test `crates/lpk-bench/tests/book.rs`, which CI's nextest runs on Windows and
  Linux, asserts that two generations are identical and equal to the committed pages (`LPK_BENCH_BOOK_WRITE=1`
  rewrites them), and that the pages hold no host name, no scanner product name, no `antivirus`, no `NO-GO` or `GO
  proposal`, no heading with `Gates` and no table row of gates G1 to G4.

The licensing and notices pages (E9-8) are includes too, so nothing is copied:

- `src/licensing.md` includes only the `published` anchor of `docs/LICENSING.md` (the sections "Our licence" and
  "Dependency policy"); the sections on planned components, patents and other compliance stay unpublished.
  `src/notices.md` shows the shipped `THIRD-PARTY-NOTICES` in a fence of four backticks. (a) both pages are exactly the
  wrapper texts held in the script (`licensing_wrapper`, `notices_wrapper`); (h) admits the two includes
  (`../../docs/LICENSING.md:published` and `../../THIRD-PARTY-NOTICES`) each only in its own page, so the whole-file
  include of `docs/LICENSING.md` fails; (c), (f) and (k) also cover `docs/LICENSING.md` and `THIRD-PARTY-NOTICES`, (l) the
  latter (a line of four backticks could close the fence); (j) admits `[output.html.search.chapter]` with only the
  line `"notices.md" = { enable = false }`.
- (m) `docs/LICENSING.md` holds exactly the lines `<!-- ANCHOR: published -->` and `<!-- ANCHOR_END: published -->`, in
  that order, no other `ANCHOR` text, and between them exactly the headings `## Our licence` and `## Dependency policy
  (...)`. Moving the end anchor, adding a heading inside the region or adding a second anchor fails, so no edit can
  widen what the book publishes without a change to this check.
- Built (`--built`): `licensing.html` holds both headings and none of the unpublished section titles; the `<code>` text of
  the only `<pre>` in `notices.html`, with its HTML entities decoded, equals `THIRD-PARTY-NOTICES` byte for byte (one
  trailing newline of difference is tolerated on the page's side; mdBook 0.5.4 needs none). This is the CI step that
  fails when the notices page differs from the shipped file. After regenerating the notices with
  `scripts/third-party-notices.sh`, rebuild the book; nothing else needs updating.
- The notices page is out of the search index (`"notices.md" = { enable = false }`): its 1.1 MB of licence text would
  dominate the index, and the built `searchindex` holds none of it while the other pages stay searchable.

The link check (`scripts/check-book-links.sh`, in the `docs` workflow after the build) is offline: it reads the built
site and fetches nothing. Locally, after `mdbook build book`: `bash scripts/check-book-links.sh book/book`
(`bash scripts/check-book-links.sh --self-test` proves each rule on a crafted site). It checks every `href` and `src`
of every `.html` file against a list of the site's files (exact, case-sensitive: GitHub Pages is case-sensitive, NTFS is
not). `https:` and `mailto:` links are counted and their hosts listed, never fetched; `http:`, `javascript:` and any
other scheme, `//host` links and root-relative links outside `/litepack/` fail; `data:` passes in `src` only. A relative
link resolves against the page (or the site root when the page has a `<base href>`, which must be the `site-url`); the
file must exist, a path may not climb above the site root, and a `#fragment` must be an `id` or `name` in the target.
On a page with a `<base href>`, an empty path (`href=""`, `#x`, `?q`) resolves to the site root's `index.html`, as a browser
does, not to the page itself. mdBook's own 404 page has that base, so `src/404.md` replaces its default text (mdBook 0.5.4
reads it without expanding includes, and it is not a chapter, so the 404 page has no footer, as mdBook's default has none)
and has no Markdown heading, which would give the page a `#...` self-anchor that dead-ends online. Any `srcset` attribute fails (the
check does not parse it), and so does an `&` in an `href` or `src` that does not start a complete entity.

(m) counts every heading form inside the published region: ATX headings (also after blockquote and list markers),
setext underlines (a line of only `=` or only `-`, which includes a thematic break) and raw `<h1>` to `<h6>` tags; only
the two allowed headings may appear. The method-document name checks (c, e) search every file, binary or not.
