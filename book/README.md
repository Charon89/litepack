# LitePack documentation book

An mdBook book, published at <https://charon89.github.io/litepack/> by the `docs` workflow on every push to
`main`. To build it locally install the pinned mdBook (`cargo install mdbook --version 0.5.4 --locked`; keep the
version in step with `.github/workflows/docs.yml`), then from the repository root run `bash scripts/check-book-footer.sh`
and `mdbook build book` (output in `book/book/`, git-ignored) or `mdbook serve book`. Every chapter listed in
`src/SUMMARY.md` ends with `{{#include footer.md}}`, which carries the licence line; the check enforces it.
