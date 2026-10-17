# E2-10/11 task C — format revision 1.2: `deflate-reconstruct` (brief; written 2026-10-16)

The design is `docs/notes/e2-10-11-design.md`; read its sections 0, 1.1, 1.2, 2 (all of it), 3.10, 4 and the task C
paragraph of section 5. Tasks A (D-64's neighbour: the input-bound hook, the records cache, multi-span given blocks,
`part_known`) and B (the walker in `lpk_core::peel::deflate`, D-64) are on `main`. The owner decided on 2026-10-16 to
carry a patched `preflate-rs` 0.7.6 (D-66). Task C runs in two halves on **one branch**, `task/e2-10-revision`, and
lands as one PR, because the code that allows primitive 0x0008 lands together with the spec text (D-49):

- **C1 — the code** (this dispatch): the patched library, the decoder, the writer's revision rules, the fuzz targets.
- **C2 — the text and the vector** (the next dispatch, on the same branch): the spec's sections 2, 8, 9, 11, 12 and
  16, `CHANGES.md`, `CONFORMANCE.md`, the vector `deflate-peel.lpk` with its builder and expectation tables.

## C1 — required (exact behaviour; mechanisms yours)
1. **The patched `preflate-rs` (D-66).** Copy `preflate-rs` 0.7.6 from the cargo registry into
   `vendor/preflate-rs-0.7.6/` (outside the workspace members; keep its `Cargo.toml` metadata, its `LICENSE`/notice
   files, and add `LITEPACK-CHANGES.md` stating each change, as Apache-2.0 section 4 requires), and point the
   workspace at it with `[patch.crates-io] preflate-rs = { path = "vendor/preflate-rs-0.7.6" }` in the root
   `Cargo.toml`. The changes, as small as possible: (a) recreation takes an output limit (the record's
   `original_len`) and returns an error as soon as its output would exceed it; (b) a cap on the number of Deflate
   blocks a recreation may produce, derived from the limit (each block emits at least one byte, or a documented
   constant for stored blocks of length 0 — read the code); (c) the predictor parameters decoded from the
   corrections are checked against the ranges analysis itself produces, refusing others; (d) nothing else — no
   output change for any valid record. Expose the limit through a new function (e.g. `recreate_whole_deflate_stream_
   limited(plain, corrections, limit)`), leaving the original function in place. `cargo deny check` must stay green
   (a path source; the licence read from the vendored manifest). The vendored crate may contain `unsafe` (it is not
   ours and not a workspace member): do not add any.
   Tests: the patched recreation equals upstream's output on every stream of the walker's golden fixtures
   (`crates/lpk-core/tests/fixtures/deflate/`) and on synthetic streams of every Deflate block type and compression
   level; a crafted correction record that makes upstream loop or grow (find one by reading the code, or by the
   fuzz target) is refused by the patched copy within the limit.
2. **The decoder** (`crates/lpk-core/src/peel/deflate/decode.rs`): `DeflateDecoder` implementing `decode_in` with the
   design's checks 1–8 in that order and the error classes it names (section 2.2(c)), and `input_bound` (task A's
   hook) running checks 1–5 and returning `Some(plain_len)` — honouring the hook's contract: the bound is checked
   against the archive's envelope and the reader's limits (check 5, the memory term `6 × plain_len + 16777216`
   against the decode memory left after `max_block_plain`) before it is returned. Recreation goes through the patched
   limited function under `catch_unwind` (a panic is `BadRecord`, `corrections`). Register it in
   `register_full_reader` beside `JpegDecoder`.
3. **The writer's revision rules** (`crates/lpk-format`): `DeflateReconstruct` is revision 2 (`version_minor` 2);
   `FormatVersion::LATEST` minor 2; the writer refuses 0x0008 under minor below 2, and at block close refuses what
   checks 2–4 refuse (a wrong kind, `original_len` not the block's `plain_len` when last, corrections longer than
   `original_len`). The minimal reader (`lpk-format` alone) still reports `UnimplementedPrimitive` (8) — the full
   reader is `lpk-core`.
4. **The bound rule end to end:** a two-step graph `[zstd or lzma, deflate-reconstruct]` whose intermediate output
   (the inflated text) is larger than `max_block_plain` decodes through the hook; without the hook's bound it would
   be `PayloadTooLarge` (test both).
5. **Fuzz targets** (`fuzz/`, Linux; `fuzz/Cargo.toml` gains `lpk-core`, `fuzz/deny.toml` the `cabac` exception if
   needed): `deflate_walk` runs `lpk_core::peel::deflate` `find_streams` on arbitrary bytes; `deflate_recreate` runs
   the patched limited recreation on arbitrary `(plain, corrections, limit)` with small bounds. Run each locally if a
   Linux fuzz toolchain is not available to you, at least build them (`cargo check` in `fuzz/` on this machine may
   not work — say so); CI's fuzz job runs them.
6. **Tests:** one per `BadRecord` reason (`kind`, `corrections`, `plain_len`, `original_len`, `original_hash`), the
   `Refused` (`decode_memory`) path, `PayloadTooLarge`, `BlockLengthMismatch`; a round trip of a real Deflate stream
   (from the fixtures) through `[zstd, deflate-reconstruct]` and `[lzma, deflate-reconstruct]`; panic containment
   through a test hook; the minimal reader's `UnimplementedPrimitive`; the writer's refusals.

## C2 — required (dispatched after C1 lands on the branch)
The spec text of the design's sections 2.1–2.3 and 2.5–2.7, written to match C1's code exactly (the check order and
error classes as implemented); `CHANGES.md` "revision 1.2" section (2.6); the vector `deflate-peel.lpk` (2.4) with its
fixtures, builder (`crates/lpk-core/tests/deflate_vector.rs`), `expected-1.2.toml`, the `expected.toml` table with the
new `files_1_0` key, the CONFORMANCE rows; every earlier vector byte-identical.

## Constraints (both halves)
Your worktree starts at `main`; confirm the head the dispatch names and create (C1) or fast-forward to (C2) the branch
`task/e2-10-revision`. `#![forbid(unsafe_code)]` in our crates; no new dependency beyond the vendored replacement; no
`unwrap` outside tests; no figures in docs, comments or commit messages; Conventional Commits ending with
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`, commit per item; fmt, clippy `-D warnings`, nextest
workspace, deny green; no PowerShell, no `git stash`, no process killing, no `git reset --hard`, no subagents. Do not
edit `crates/lpk-check` (task E's author works from the text alone) or `docs/DECISIONS.md`, `docs/PLAN.md`,
`docs/STATUS.md`. Report to the path the dispatch names; reply with status, commits, test count, concerns — at most
8 lines.
