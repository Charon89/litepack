# LitePack documentation book

An mdBook book, published at <https://charon89.github.io/litepack/> by the `docs` workflow on every push to
`main`. To build it locally install the pinned mdBook (`cargo install mdbook --version 0.5.4 --locked`; keep the
version in step with `.github/workflows/docs.yml`), then from the repository root run `bash scripts/check-book-footer.sh`
and `mdbook build book` (output in `book/book/`, git-ignored) or `mdbook serve book`. Every chapter listed in
`src/SUMMARY.md` ends with `{{#include footer.md}}`, which carries the licence line; the check enforces it.

The format specification pages (`src/spec/`) are made by include: each holds only `{{#include ../../../docs/spec/<name>}}`
and the footer include, with the same file name as its source in `docs/spec/`, so the specification's links between its
own files work in the book. Never copy the text. What the checks cover:

- `bash scripts/check-book-sources.sh` (before the build): the wrappers are pure includes (a page that is more than those
  two lines fails); every `docs/spec/*.md` has a page; there is no `{{#` include inside `docs/spec/`; no file under
  `book/` names the unpublished method document, by content or by file name, in any letter case.
- `bash scripts/check-book-sources.sh --built book/book` (after the build): each spec page holds its source's first
  heading (mdBook only warns when an include fails, so this is what turns a missing include into a failure); no built
  file names the method document; no built file holds a content fingerprint of it (its heading and three long lines),
  however it got there. The fingerprint can miss a heavily reformatted copy.
- `--self-test` proves each check fails, with its tag, on a crafted tree.
