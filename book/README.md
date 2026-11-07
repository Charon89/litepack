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
  So nothing outside `src/` and `docs/spec/` can reach the site by inclusion.
- `bash scripts/check-book-sources.sh` (before the build) also checks that the wrappers are pure includes (a page that
  is more than those two lines fails), that every `docs/spec/*.md` has a page, and that no file under `book/` names
  the unpublished method document, by content or by file name, in any letter case.
- `bash scripts/check-book-sources.sh --built book/book` (after the build): each spec page holds its source's first
  heading (mdBook only warns when an include fails, so this is what turns a missing include into a failure); no built
  file names the method document. Those names and a content fingerprint (g) of the method document (its heading and
  every long line) are a second line of defence; the fingerprint is a sample that a reworded copy can evade.
- `--self-test` proves each check fails, with its tag, on a crafted tree.
