# LitePack documentation book

An mdBook book, published at <https://charon89.github.io/litepack/> by the `docs` workflow on every push to
`main`. To build it locally install the pinned mdBook (`cargo install mdbook --version 0.5.4 --locked`; keep the
version in step with `.github/workflows/docs.yml`), then from the repository root run `bash scripts/check-book-footer.sh`
and `mdbook build book` (output in `book/book/`, git-ignored) or `mdbook serve book`. Every chapter listed in
`src/SUMMARY.md` ends with `{{#include footer.md}}`, which carries the licence line; the check enforces it.

The format specification pages (`src/spec/`) are made by include: each holds only `{{#include ../../../docs/spec/<name>}}`
and the footer include, with the same file name as its source in `docs/spec/`, so the specification's links between its
own files work in the book. Never copy the text. `bash scripts/check-book-sources.sh` (before the build) fails on a page
that is more than those two lines, a spec file without a page, or any file under `book/` naming the unpublished method
document; `bash scripts/check-book-sources.sh --built book/book` (after the build) checks that each page rendered its
source's first heading and that nothing built names that document; `--self-test` proves the checks fail on crafted trees.
