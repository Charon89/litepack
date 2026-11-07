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
  `src/spec/<name>.md`; `rustdoc_include`, `playground` and the rest fail. (f): there is no `{{#` inside `docs/spec/`.
  So nothing outside `src/` and `docs/spec/` can reach the site by inclusion. Both mirror mdBook 0.5.4's directive
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
  backticks (that is the only text that could close the fence). The built check (d) looks for the first `# ` heading of
  each `.md` and for the first three words of the man page's `.TH` line.
