# E2-18 round 3 — the chunk hashes and (if needed) the cut off the feeding thread (brief; written 2026-10-16)

Rounds 1 and 2 of E2-18 (D-61, D-63) made the store path byte-identical, pipelined and recycled. The acceptance rows
(`bench/reports/phase1-2026-10-17-store-path.md`) put `lpk/fast` at 916.6 MB/s on `full`'s `video` against the
`store` control's 1199.6 in the same run — 76.4%, short of G3's 80% (D-27). `lpk/store`, whose pipeline has no fold
stage and therefore no content-defined chunking, writes the same class at 1469.1 MB/s, faster than `tar`. So the gap
is what the Fast tier does on the feeding thread even for a stored class: the FastCDC cut (`crates/lpk-core/src/fold/
chunker.rs`, the `fastcdc` crate's v2020 algorithm with D-53's parameters 4096/65536/524288, normalisation 1) and the
BLAKE3 of every chunk (`Writer::add_chunk` and the dedup lookups). Round 2's stage timing put the cut at about a
quarter of the feeding thread's time and the chunk hashes at about an eighth.

## The rule that does not move (exactly)
Byte identity: every chunk cut, chunk hash, dedup decision, block boundary, frame byte and index byte is the one the
current writer produces for the same input and options, at every thread count. The existing byte-identity tests stay
and pass; extend them to whatever you add.

## Step (a) — the chunk hashes in parallel
With two or more blocks in flight, the BLAKE3 of the chunks of a cut segment is computed by a small pool (or by the
reader thread for the segment it just read, or by the block's job ahead of the in-order dedup — your choice), and the
feeding thread consumes the hashes in order, so the dedup lookups stay sequential and in order. Bound the pool and
document it with the other threads in the "Memory:" and thread-layout paragraphs (D-63 (2)); one block in flight stays
fully sequential. Measure on `full`'s `video` (release, `lpk a --fast --threads 24`, three runs, and `tar` in the same
session as round 2 did) and write the walls into the report **before** going on.

## Step (b) — only if step (a) leaves `lpk --fast` below about 85% of `tar` in your indicative measurement
Compute the FastCDC cut points in parallel with an **exact** resynchronisation, so the cuts are those of the
sequential chunker:
- The v2020 gear hash is windowed: after 64 bytes have been hashed, the starting state has shifted out entirely, so
  the hash at a position depends only on the 64 bytes before it — except within the first 64 hashed bytes of a chunk,
  where the chunk's own zero start state still matters. The crate checks the hash after skipping `min_size` bytes
  from the chunk start, under the strict mask before `avg_size` and the loose mask after it, and its 2020 variant
  hashes two bytes per step, so which positions are checked depends on the chunk's start.
- So: workers scan disjoint segments (with 64 bytes of overlap) and record, per position, whether the windowed hash
  satisfies the strict mask and the loose mask; a sequential merge walks chunk by chunk from the true previous cut:
  it recomputes the first 64 hashed bytes of each chunk exactly, then takes the first recorded candidate at a checked
  position under the mask its distance requires, bounded by `max_size` — reproducing the crate's decision. Read the
  crate's source (`fastcdc` 5.0.0 in the cargo registry, MIT) for the exact step, masks and alignment; do not change
  the parameters.
- Prove it before using it: a property test comparing the parallel cuts with the crate's sequential cuts on many
  random and structured inputs (all-zero runs, repeated patterns, files shorter than `min_size`, lengths around every
  bound, segment boundaries at every offset modulo 64), plus the byte-identity tests on the store path and on the
  corpus test.
Measure again as in step (a).

## Tests
- Byte identity of the store path and of a mixed tree at 1 and N threads against the current writer (dedup, given
  blocks, sealing, recovery), the ignored corpus test's pinned hashes for `small`'s `video` and `encrypted-random`.
- Step (a): the hash pool's results equal the sequential hashes; a panic or error in the pool is handled as D-54 and
  D-63 require (joined, re-raised or reported, no thread left).
- Step (b), if done: the property test above.

## Constraints
Your worktree starts at `main`; confirm the head the dispatch names and create the branch `task/e2-18-round-3`.
`#![forbid(unsafe_code)]`; no new dependency (`std::thread` and `std::sync`); no `unwrap` outside tests; no figures in
docs, comments or commit messages; Conventional Commits ending with `Co-Authored-By: Claude Opus 5.5
<noreply@anthropic.com>`, commit per step; fmt, clippy `-D warnings`, nextest workspace, deny green; no PowerShell,
no `git stash`, no process killing, no `git reset --hard`, no subagents, do not run `lpk-bench run`. Treat timings as
indicative (another task may build on this machine; repeat a noisy one). If you near a turn limit, write the numbers
into the report and commit what works before anything else. Report to the path the dispatch names (what moved, the
walls before and after each step, the tests, the four tools' last lines); reply with status, commits, test count, the
measured result in words, concerns — at most 8 lines.
