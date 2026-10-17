# E2-20 — memory and envelope enforcement on the compress side (brief; written 2026-10-16)

PLAN Phase 1 task **E2-20**: "Thread and memory budgets per tier, the envelope written from what was used, refusal
on decode when the local machine cannot meet it. Acceptance: Peak RSS of `lpk` rows in the runner within the tier's
budget on `small` and `full`; an archive written with a 256 MB window is refused on a decoder with a smaller
configured limit." The orchestrator records the decision and runs the runner rows. Do not edit
`docs/DECISIONS.md`, PLAN, STATUS, `docs/spec/*`, `crates/lpk-check`, `bench/results/`.

## What exists (read before designing)
- The format writes the decode envelope (`crates/lpk-format/src/envelope.rs`; spec sections 6–7; D-32): six
  varints in the index (`max_window`, `max_bwt_block`, `max_block_plain`, `max_frame_payload`, `decode_memory`,
  `threads_hint`), and the reader refuses at open, field by field, when one exceeds its local `Resources`
  (`Envelope::check`, `Refusal`; defaults `DEFAULT_MEMORY` 2 GiB and the others in `envelope.rs`). The writer fills
  `decode_memory` as `max_block_plain` plus the largest per-block working memory the encoders declare
  (`Encoded::resources`, `add_part_encoded`'s `memory`), and `threads_hint` is written as 0.
- The extraction engine bounds its decoders by a memory budget (`ExtractOptions::memory`, default the reader's
  `Resources::memory`; `crates/lpk-core/src/extract.rs`, D-54).
- The compress side has no budget: blocks in flight are `--threads` (Fast) or `min(threads, 4)` (Balanced, D-60's
  provisional cap). Measured peak working sets at 24 threads (`bench/reports/phase1-2026-10-14-full-vm-image-threads.md`,
  `vm-image` of `full`): `lpk/balanced` 4829 MiB, `lpk/fast` 1659 MiB, `lpk/store` 1805 MiB; on `small`
  (`phase1-2026-10-14-small-threads.md`) the Balanced rows reach about 1.7 GiB on `office-pdf`.
- The method document's minimum requirements (`docs/LitePack-Method-v2.md`, read only): "2–4 GB free RAM for
  Fast/Balanced", and decode envelopes capped by default (≤ 256 MB windows, ≤ 64 MB BWT blocks) "so a 4 GB laptop or
  a NAS can" extract.

## The orchestrator's rulings (exact values)
1. **Budgets.** A tier's memory budget is the ceiling its whole process may use while compressing or extracting:
   default `min(ceiling, half of the machine's physical memory)`, with ceilings **store 1 GiB, Fast 2 GiB,
   Balanced 4 GiB** (the method document's "2–4 GB free RAM for Fast/Balanced" read as the tool's own peak).
   `lpk a|x|t|append --memory <size>` overrides (accepts `512M`, `2G`, plain bytes; refuses a value below what one
   block in flight needs, with the figure named). The physical memory comes from a system query (the workspace
   already depends on `sysinfo` in `lpk-bench`; a dependency that is already in the tree and allow-listed is not a
   new licence question — check `cargo deny check` stays green, and prefer a `windows`-crate call on Windows plus
   `/proc/meminfo` on Linux if adding `sysinfo` to `lpk-core` or `lpk-cli` pulls more than it should).
2. **Blocks in flight from the budget, not a constant.** The writer's `encode_threads` for a run becomes
   `max(1, min(threads, floor((budget − fixed) / per_block)))`, where `per_block` is one block's plain bytes plus
   its encoded size plus one encoder working set for the tier and settings (zstd at the level and window used;
   LZMA at the dictionary and match finder used — take the figures from the libraries' own memory functions where
   they exist, e.g. liblzma's encoder memory usage query, and from a documented allowance where they do not), and
   `fixed` covers the walk, the chunker, the dedup map, the peel's working set and the index (each documented with
   its formula). D-60's cap of four for Balanced goes; the budget replaces it. `-v` prints the budget, its source
   (default or `--memory`), `per_block`, `fixed` and the resulting blocks in flight.
3. **Extraction within the budget.** `lpk x|t --memory` sets `ExtractOptions::memory` and the reader's
   `Resources::memory`, so an archive whose envelope exceeds the configured limit is refused at open, before any
   output exists (the refusal names the field, the needed and the allowed amounts); the default is the tier-free
   ceiling of `min(4 GiB, half the physical memory)` (extraction does not know the writer's tier).
4. **The envelope from what was used.** `decode_memory` must cover what the shipped decoders really need per
   thread for the blocks written (zstd: the frame's window plus the decoder's context; LZMA: the dictionary plus
   the decoder state; `jpeg-reconstruct`: its documented allowance) — check the current figures against the
   libraries' own estimates and correct any that are low; `threads_hint` is written as the number of blocks the
   archive has, capped at the writer's blocks in flight (so a reader can size its pool without scanning). These
   are reference-writer changes in `crates/lpk-format` that change archive bytes only in the envelope varints;
   every conformance vector must stay byte-identical (vectors are written with explicit options — if one changes,
   stop and report NEEDS_CONTEXT).
5. **Peak RSS evidence.** The runner already records the peak working set per row (Job Object on Windows). The
   acceptance is the `lpk` rows of one run on `small` and one on the largest classes of `full` (`vm-image`,
   `office-pdf`, `software-installed`) at 24 threads, every row's peak within its tier's ceiling — the
   orchestrator runs those. Your tests show the mechanism.

## Tests
- Budget arithmetic: blocks in flight for given budgets, block sizes and settings (Fast and Balanced), including
  `--memory` below one block (refused with the figure) and a budget that allows exactly one.
- A writer with a tiny `--memory` still writes byte-identical archives (blocks in flight change, bytes do not).
- `decode_memory` and `threads_hint` as written for Fast, Balanced and a peeled archive, against the formulas.
- The acceptance case: an archive written with a 256 MiB window (Fast with `--window-log 28`) is refused by
  `lpk x --memory 128M` and by `lpk t --memory 128M` before any output file or directory exists, and extracts with
  the default limit.
- Extraction under a small budget still extracts bit-exactly (fewer decoders), and the extraction engine never
  holds more blocks than the budget allows (an instrumented test, as E2-19's tests do).

## Constraints
`#![forbid(unsafe_code)]`; no new dependency beyond the rule in ruling 1; no `unwrap` outside tests; no figures in
docs, comments or commit messages; Conventional Commits ending with
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; commit per deliverable; fmt, clippy `-D warnings`,
nextest workspace, deny green; no PowerShell, no `git stash`, no process killing, no `git reset --hard`, no
subagents, do not run `lpk-bench run`. Report to the path the dispatch names: the memory model with its formulas,
the per-tier figures your tests compute for the default settings, the tests, the four tools' last lines; reply
with status, commits, test count, concerns — at most 8 lines.
