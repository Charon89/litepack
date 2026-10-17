# E2-10/11 task D — the Deflate peel in the pipeline (brief; written 2026-10-16)

The design is `docs/notes/e2-10-11-design.md`; read its sections 0, 1.2–1.5, 3 (all of it), 4, the task D paragraph of
section 5, and 7 (the acceptance evidence). Tasks A, B and C (the revision 1.2: the patched `preflate-rs` of D-66, the
`deflate-reconstruct` decoder in `lpk_core::peel::deflate`, the writer's revision rules, the spec text and the vector)
are on `main` before this task starts (the dispatch names the head). This task wires the peel into `lpk-core`'s
pipeline and the `lpk` tool, measures it, and is the task whose acceptance ticks E2-10 and E2-11.

## Required (exact behaviour; mechanisms yours)
1. **The stage** (design 3.2–3.4): `Pipeline.deflate: Option<DeflatePeel>` with its own write path beside the JPEG
   `PeelStage`, which stays untouched. Per file of the classes the design names: the size cap (256 MiB, refused
   unread), whole-file dedup first (`add_file_if_known`), then `examine` from task B, for each reconstructed stream:
   classify its inflated text (first 1 MiB), the single-span dedup probe (`part_known`), encode with the part encoder,
   the gates; then the file gate; then the layout of design 1.3 (framing first through `add_part`, then per kept
   stream `add_record` and `add_parts_encoded`), or `add_file` from the buffer when nothing is kept.
2. **Gates** (design 3.3, D-09) exactly as the design states them — the per-stream cost (M's bytes + the record's
   exact encoded length + 160), the block cap, the memory cap, the file gate (gain over 256), and the counting rule:
   `reconstructed` is decided before any gate, and the outcomes kept / no gain / block cap / memory cap / already
   stored / file gate partition it.
3. **The part encoder** (design 3.5): Balanced — its own LZMA encoder at the tier's dictionary cap with the end marker,
   its own zstd core, the four-stripe trial, no class rule and no "stored, no gain" fallback; Fast (option) — zstd at
   the tier's level and window, no gate, no dictionary. Its blocks are counted in the Deflate summary, not in the tier
   summaries.
4. **Defaults and minor:** Balanced on, Fast off (`lpk a --deflate-peel on|off`, defaulting per tier); the header's
   minor is 2 when the Deflate peel is enabled, 1 with the JPEG peel only, 0 otherwise. Appends (design 3.8, amends
   D-56 (3)): an append runs only the peel stages the old archive's minor allows and says so in the summary and `-v`.
5. **Determinism** (design 3.7): the walker, analysis, part encoding, `add_record` and `add_parts_encoded` on the
   feeding thread in input order; byte-identical archives at 1 and N threads (test).
6. **Summary, CLI, bench** (design 3.9): `PeelSummary.deflate: DeflateSummary` with the fields the design lists
   (`files: Option<Vec<FileStreams>>` only when `record_files` is set); `-v` prints it; `bench/tools.toml` — `balanced`
   includes the peel by its new default, a new control setting `balanced-no-deflate` = `["--balanced",
   "--deflate-peel", "off"]` (document it in `docs/BASELINES.md` next to the other control settings).
7. **Narrow the public surface** task B left open: `lpk_core::peel::deflate` exposes what the pipeline and the probe
   need, not `preflate-rs` types (review finding of task B).

## Tests
Synthetic ZIP, docx, JAR, multi-IDAT PNG, gzip, PDF and scan inputs: round trip; a tiny stream not kept; a file-gate
case; an equal stream in two files (dedup); a whole duplicate; a docx's XML member counted under `Text` in
`kept_by_class`; the header minor; byte identity at 1 and 8 threads; sealed archives; an append to a 1.1 archive runs
without the Deflate stage and says so; the CLI flag. The ignored corpus tests of design section 7:
`deflate_share_corpus.rs` (the per-file counts equal `bench/results/2026-10-03-megatron-3/probe-deflate.json` for every
walked file of `small`, asserted) and `deflate_corpus.rs` (every class of `small` through the default Balanced
pipeline, extracted by the D-54 engine and by `lpk-check`, 0 mismatches, the Deflate summary and sizes printed) — both
run once in release on `bench/corpus/small` of the main checkout.

## Constraints
Your worktree starts at `main`; confirm the head the dispatch names and create the branch `task/e2-10-pipeline`.
`#![forbid(unsafe_code)]`; no new dependency; no `unwrap` outside tests; no figures in docs, comments or commit
messages; Conventional Commits ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`, commit per item;
fmt, clippy `-D warnings`, nextest workspace, deny green; no PowerShell, no `git stash`, no process killing, no
`git reset --hard`, no subagents, do not run `lpk-bench run` (the orchestrator measures the rows). Do not edit
`crates/lpk-check`, `docs/spec/*`, `docs/DECISIONS.md`, `docs/PLAN.md`, `docs/STATUS.md`. If you near a turn limit,
commit what works and report what is left. Report to the path the dispatch names (the stage as built, the gates, the
corpus tables, the test count, the four tools' last lines); reply with status, commits, test count, the share and
round-trip verdicts in words, concerns — at most 8 lines.
