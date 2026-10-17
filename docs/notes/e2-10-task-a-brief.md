# E2-10/11 task A — `lpk-format` plumbing for the Deflate peel (brief; written 2026-10-16)

The design of PLAN E2-10 + E2-11 is `docs/notes/e2-10-11-design.md`; read its sections 0, 1.1, 1.3, 4 and the task A
paragraph of section 5. This task adds to the reference reader and writer the four things the revision (task C) and
the pipeline (task D) need, **without any new decoding and without changing a byte of any archive**: every committed
vector stays byte-identical, `crates/lpk-check` is untouched and stays green.

## Required (exact behaviour; mechanisms yours)
1. **The input-bound hook.** `PrimitiveDecoder` gains `fn input_bound(&self, ctx, params) -> Result<Option<u64>, _>`
   (name and signature yours), default `None`. In `crates/lpk-format/src/decode.rs::decode_block_in`, before running
   step i (when step i is not the last step), the reader asks step i+1's decoder for its input bound: `None` keeps
   today's bound (`max_block_plain`), `Some(n)` makes `n` the output bound of step i; an output longer than the bound
   is `PayloadTooLarge` as today. The hook may return an error (the record checks a later step needs before the
   previous step runs — task C uses it for checks 1–5 of the design's section 2.2(c)); that error is the block's
   error. No shipped decoder implements the hook in this task: test it with a fake decoder registered in a test.
2. **The records cache.** Today `ArchiveContext::record` (or the function that looks a record up by id) re-reads and
   re-hashes the whole `Records` frame and walks from record 0 on every lookup — quadratic in the number of records.
   Parse and verify the frame once per opened archive, share the result with `fork`s (the extraction workers), and
   keep exactly today's error outcomes: a lookup of id n reports the first error among records 0 to n, as now (re-walk
   only on the error path if that is simplest). Tests: every existing malformed-records test gives the same outcome;
   a frame of 100000 small records answers 100000 lookups quickly (assert a generous wall-clock bound only if the
   suite already does such things; otherwise count frame reads with a test hook and assert one).
3. **`Writer::add_parts_encoded(spans, plain, encoded, memory)`.** The given-block counterpart of
   `add_part_encoded` for a stream that lies in several spans of the open entry (a PNG's IDAT payloads): `spans` is a
   list of `(offset, len)` in the entry, sorted and disjoint; `plain` is the concatenation of those spans' bytes
   (the block's plain bytes); chunks are cut **per span**, so no chunk crosses a span boundary; the block is written
   with `encoded` as `add_part_encoded` does today (same graph checks, same record rules, same envelope `memory`);
   the entry's chunk list places each span's chunks at its offset, interleaved with plain parts added before or
   after (the writer already sorts parts by offset at `end_entry`). `add_part_encoded` becomes the one-span case of
   it (same bytes as today — the vectors prove it).
4. **`Writer::part_known(plain) -> Option<...>`** — a query that commits nothing: whether every chunk the chunker
   would cut from `plain` (as a part at the current position — define the cut rule exactly as `add_part_known` uses
   it) is already stored, returning what `add_part_known` needs to reference them. Read-only on the writer's state.

## Tests
- The fake decoder with an input bound: a two-step graph whose first step's output exceeds `max_block_plain` decodes
  when the bound allows it and fails with `PayloadTooLarge` when it does not; a hook error is the block's error.
- The records cache: the existing malformed-records tests unchanged; the many-records lookup test; a `fork` shares
  the cache (no second parse).
- Multi-span given blocks interleaved with plain parts round-trip; no chunk crosses a span boundary; the entry
  extracts bit-exactly; dedup of an identical multi-span block in a second entry behaves as for a single span (or is
  documented as whole-block only, as the design's 3.6 says for PNG).
- `part_known` returns the same answer `add_part_known` would act on, and changes nothing.
- Every committed vector byte-identical (`tests/vectors.rs`, `tests/conformance.rs`); `cargo nextest run -p lpk-check`
  green and `crates/lpk-check` unchanged.

## Constraints
Your worktree starts at `main`; confirm the head the dispatch names and create the branch `task/e2-10-format`.
Touch only `crates/lpk-format/src/{decode.rs, reader.rs or archive.rs (wherever the records lookup lives),
writer.rs, lib.rs}` and `crates/lpk-format/tests/`; no spec text (task C writes it), no `lpk-core` change beyond
keeping it compiling. `#![forbid(unsafe_code)]`; no new dependency; no `unwrap` outside tests; no figures in docs,
comments or commit messages; Conventional Commits ending with `Co-Authored-By: Claude Opus 5.5
<noreply@anthropic.com>`, commit per item; fmt, clippy `-D warnings`, nextest workspace, deny green; no PowerShell,
no `git stash`, no process killing, no `git reset --hard`, no subagents. Report to the path the dispatch names; reply
with status, commits, test count, concerns — at most 8 lines.
