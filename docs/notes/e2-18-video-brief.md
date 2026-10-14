# E2-18 — the entropy gate in the write path and the video target (brief; written 2026-10-13 for the next session)

PLAN Phase 1 task **E2-18**: "Store high-entropy input through the sequential store path without compression
attempts; measured against the `store` control in the same run (D-27). Acceptance: the `video` and
`encrypted-random` rows of the runner for `lpk --fast` and `--balanced`; G3 as restated: `lpk --fast` store
throughput on `video` at least 80% of the `store` control's in the same report." The orchestrator records the
decision and runs the runner rows (`--tools lpk,store` at least, one results directory, so the control and the
subject share the run — D-27's rule). Do not edit `docs/DECISIONS.md`, PLAN, STATUS, `docs/spec/*`,
`crates/lpk-check`; `crates/lpk-format`'s writer is in scope under the rule below.

## What the numbers say today (committed sources)
- The `store` control (`tar`, a raw copy) writes `full`'s `video` at about 1.5 GB/s (`bench/reports/phase0-2026-10-03.md`,
  the `store/store` row of that class). `lpk/fast` writes `small`'s `video` at about 320 MB/s with blocks in
  flight (`bench/reports/phase1-2026-10-14-small-threads.md`) — the same as `lpk/store` and as before E2-4b,
  because the video class is already stored by class (no compression attempt is made: the gate is not the cost).
- The cost is the single feeding thread's per-byte work on the store path: the read, the FastCDC cut
  (`crates/lpk-core/src/fold/chunker.rs`), the BLAKE3 hash of every chunk in `Writer::add_chunk`
  (`crates/lpk-format/src/writer.rs`), the copy into the pending block, the identity encoder's copy of the block,
  and the frame's BLAKE3 over the whole block at the write step — three passes over every byte and two copies,
  all but the identity copy on one thread. That is why `lpk/store` sits near 400 MB/s on every class whatever
  the thread count.

## Required properties (mechanisms yours unless marked exactly)
1. **Byte identity (exactly).** Nothing in this task may change the bytes of an archive: the chunk cuts, the
   chunk hashes, the block boundaries, the frame hashes and the index are the same as today for the same input
   and options (the byte-identity tests of E2-4b are the guard; extend them to the store path with
   `--threads 1` against `--threads N`).
2. **The gate decides before any compression work (exactly the rule).** A class stored by class (D-47: video,
   encrypted-random, the other stored classes) never reaches an encoder trial, in either tier; a block the
   entropy gate stores is decided on its sample, as today. Document where each decision is made and show in a
   test that no zstd or LZMA context is created for a stored-by-class cluster.
3. **Take the per-byte work off the feeding thread.** Measure first, with a release build and the probe-style
   timing the pipeline already prints with `-v` (stage timings): how the store path's time divides between the
   read, the cut, the chunk hashes, the copies and the frame hash on `small`'s `video`. Then remove or move the
   big items, in this order of preference: (a) the frame hash computed by the block's job on the pool thread and
   handed back with the encoding (a reference-writer change: the writer takes the hash the job computed over
   exactly the bytes it will write — head plus encoded — and uses it as it uses its own today; no byte change;
   with `encode_threads` 1 the writer still hashes itself); (b) the chunk hashes computed ahead of the writer
   by a small pool over the chunker's output (the cut stays sequential — FastCDC's cut points depend on the
   rolling hash, but the BLAKE3 of each chunk does not — so chunks are hashed in parallel and handed to the
   writer in order with their hashes; `Writer::add_chunk_hashed(data, hash)` or the existing `add_part_known`
   path, whichever keeps the dedup lookups exactly as today); (c) one copy fewer where the identity encoder is
   known (the store graph: the pool job can hand back the plain buffer it was given instead of copying it).
   Each step is measured on `video` and `encrypted-random` before the next is taken; stop when the `lpk/fast`
   row reaches the acceptance or when the remaining items are the read and the write themselves.
4. **No change to what the Fast tier compresses.** Only the stored path and the hashing change; the zstd and
   LZMA rows of every other class keep their sizes (the runner rows show it: every class of `small`, the sizes
   equal to `bench/results/2026-10-14-megatron`).
5. **Report, do not claim.** The acceptance is the runner's `video` and `encrypted-random` rows for `lpk/fast`,
   `lpk/balanced` and `lpk/store` against the `store/store` control in the same results directory; the stage
   timings go into the task report; no figure is written into docs, comments or commit messages.

## Tests
- Byte identity of the store path with threads 1 and N (temp tree of incompressible files and a mixed tree).
- The gate's decision point: a stored-by-class cluster creates no compression context (a counting test
  encoder or the tier's summary counts).
- The frame hash handed back by a job equals the writer's own (the writer verifies it in debug builds, or a
  test compares the two paths); a job that hands back a wrong hash is refused (`BadOptions` or the error class
  the writer uses for a graph that fails its check — the archive is abandoned, never written with a wrong hash).
- Parallel chunk hashing: the hashes and the order are those of the sequential path (identical archive bytes).
- Ignored corpus test (`LPK_CORPUS`, `bench/corpus/small` of the main checkout, release, once): `video` and
  `encrypted-random` with `--fast --threads 1` and `--threads 24`, identical bytes, wall times printed.

## Constraints
`#![forbid(unsafe_code)]`; no new dependency (`std::thread::scope` is the pool pattern); no `unwrap` outside
tests; no figures in docs, comments or commit messages; Conventional Commits ending with
`Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; commit per deliverable; fmt, clippy `-D warnings`,
nextest workspace, deny green; no PowerShell, no `git stash`, no process killing, no `git reset --hard`, no
subagents, do not run `lpk-bench run`. Write the report to the path the dispatch names; reply with status,
commits, test count, the measured division of the store path's time in words, concerns — at most 8 lines.
