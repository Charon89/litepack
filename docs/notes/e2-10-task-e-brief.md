# E2-10/11 task E — `deflate-reconstruct` in the independent decoder, from the text alone (brief; written 2026-10-16)

Revision 1.2 of the `.lpk` specification (`docs/spec/lpk-v1.md`, sections 2, 8, 9, 11, 12 and 16; `docs/spec/
CHANGES.md`'s revision 1.2 section; `docs/spec/CONFORMANCE.md`) specifies the decoding of `deflate-reconstruct` (0x0008)
and the bound of the step before it. You are the author of that decoding in the independent decoder `crates/lpk-check`
— the independence gate of D-49 item 5: you work from the text alone, and may use the named codec library for the codec
itself, never for the framing, the record checks or the bound rule. **What you may read is restricted, and the
restriction is the test.**

## You may read
- `docs/spec/lpk-v1.md`, `docs/spec/CONFORMANCE.md`, `docs/spec/CHANGES.md`.
- The vectors under `crates/lpk-format/tests/vectors/` (their bytes and the `expected*.toml` and `vectors.toml` files) —
  as files, not their builders.
- `crates/lpk-check/` (the decoder you are changing).
- The public documentation of `preflate-rs` 0.7.6 (its docs on docs.rs or the crate's README/rustdoc comments of the
  functions you call). The workspace resolves `preflate-rs` to a bounded copy in `vendor/preflate-rs-0.7.6/` (D-66); you
  may read that directory's `LITEPACK-CHANGES.md` and the rustdoc of the limited recreation function to learn how to call
  it, nothing else in it.

## You must not read
`crates/lpk-format/src/`, `crates/lpk-format/tests/` (other than the vector files above), `crates/lpk-core/` (the
reference decoder and the vector builder live there), `docs/notes/`, the git history of the spec's revision 1.2 text,
or `preflate-rs`'s source beyond what the paragraph above allows. If the text is insufficient, that is the finding:
stop and report BLOCKED with the exact sentence(s) that left you guessing — never fill the gap from another source.

## What to build
1. **The step** in `lpk-check`: the order of checks section 8 states for `deflate-reconstruct`, the error classes it
   names, the memory term, and recreation through the bounded library function under `catch_unwind`.
2. **The bound rule:** before the step that precedes a `deflate-reconstruct` step, that step's record checks run and its
   output bound is the record's `plain_len` (section 8, "Bounds").
3. **`--revision <minor>`** (0, 1 or 2, default 2; the existing `--revision-1-0` stays as an alias): a lower revision
   reports `UnimplementedPrimitive` for the primitives it does not run.
4. **The conformance test:** every vector decodes, including `deflate-peel.lpk` to `expected-1.2.toml`; `--revision 1`
   and `--revision 0` reproduce the `_1_0` keys, including the new `files_1_0`.
5. **Your attestation** in the report: every file you read, and every sentence you had to reread or guess at — those
   become spec fixes before the revision is final.

## Constraints
Your worktree starts at `main`; confirm the head the dispatch names and create the branch `task/e2-10-check`.
`#![forbid(unsafe_code)]`; no new dependency beyond `preflate-rs = "=0.7.6"` (resolved to the vendored copy); no
`unwrap` outside tests; no figures; Conventional Commits (`feat(check): ...`) ending with `Co-Authored-By: Claude Opus
5.5 <noreply@anthropic.com>`; commit per item; fmt, clippy `-D warnings`, `cargo nextest run -p lpk-check`, deny green;
no PowerShell, no `git stash`, no process killing, no `git reset --hard`, no subagents. Do not edit `docs/spec/*` (report
defects), any vector, `crates/lpk-format`, `crates/lpk-core`, `docs/DECISIONS.md`, `docs/PLAN.md`, `docs/STATUS.md`.
Report to the path the dispatch names; reply with status, commits, test count, the gate verdict in words (decoded from
the text alone, or not and why), concerns — at most 8 lines.
