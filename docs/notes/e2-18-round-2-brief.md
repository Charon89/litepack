# E2-18 round 2 — the store path pipelined (brief; written 2026-10-16)

Round 1 of E2-18 (`docs/notes/e2-18-video-brief.md`; branch `task/e2-18-video`) removed the identity copy, moved
the frame hash to the pool thread and kept stored-by-class clusters away from every compression context, all
byte-identical. Its measurements (on `small`'s 66 MB `video`, so with the fixed cost of a process start inside every
figure) put the feeding thread's time into: the copy into the pending block about 30% — dominated by first-touch
page faults of freshly allocated 64 MiB buffers, not by the copy itself — the read about 26%, the write about 17%,
the FastCDC cut about 12%, the chunk hashes about 7.5%. All of it runs on one thread, one step after another.

The orchestrator's ruling on round 1's finding: **byte identity stays** (chunk cuts, chunk hashes, block boundaries,
frame bytes and the index are those of the sequential writer for the same input and options), and the G3 target
(`lpk --fast` on `video` at 80% or more of the `store` control's rate, D-27) is judged where the gate is judged —
`video` of `full` (960 MB) against `tar` in one run of the runner — where a process's fixed cost is amortized.
Round 2 therefore pipelines the store path so that reading, cutting and writing overlap instead of adding up.

## Required properties (mechanisms yours unless marked exactly)
1. **Byte identity (exactly)**, as round 1: every archive byte unchanged against `task/e2-18-video`'s head for the
   same input and options, at 1 and N threads, for every tier, with dedup, given (peeled) blocks, sealing and
   recovery interleaved. The existing byte-identity tests stay; extend them to whatever new path you add.
2. **No first-touch cost per block.** Block buffers and read buffers are recycled: allocated once (the pool sized by
   the blocks in flight plus the ones being filled and written), returned when the frame that used them is
   written, never reused while any job or pending write references them. Document the pool's size in the writer's
   "Memory:" paragraph (E2-20 will budget it).
3. **Reading overlaps the cut.** A reader thread reads file contents ahead of the feeding thread into recycled read
   buffers through a bounded queue (depth documented); the feeding thread cuts, hashes and adds chunks from them.
   Files are read in the order the pipeline feeds them today; a read error is reported for the file it belongs
   to, at the point the feeding thread would have hit it today, and abandons the run as today.
4. **Writing overlaps the cut.** The in-order write step hands finished frame bytes to a writer thread through a
   bounded queue; positions, sequence numbers and hashes are decided in order exactly as today (only the `write`
   system calls move). `finish` drains it; a write error surfaces at the next writer call and abandons the archive
   as today; `sync` semantics (the journal's data synced before the trailer, D-59) are kept.
5. **Optional, only if the measurement says it matters:** the chunk hashes of a cut segment computed by a few
   threads in parallel and consumed in order (the dedup lookups stay sequential and in order, so byte identity
   holds).
6. **Threads, panics, errors (D-54's rule).** Every thread this round adds is joined on success, error, panic and
   drop; a panic in one is re-raised after the writer is marked failed; no thread outlives the writer; no deadlock
   when one side stops while the other waits on a full or empty queue (test it).
7. **Measure where the gate is judged.** Before and after, on `video` of `full`
   (`D:/DEVELOPMENT/Personal Projects/Claude/lite-pack/bench/corpus/full` of the main checkout, read only), release
   build, `lpk a --fast` at 1 and 24 threads: process wall and the feeding thread's stage shares (temporary
   timers, removed before the final commit). Report shares and walls in the report file only; the orchestrator
   runs the acceptance rows (`--tools lpk,store`, `video` and `encrypted-random`, `small` and `full`) on an idle
   machine.

## Tests
- Byte identity of the pipelined path against round 1's (threads 1 and N; a mixed tree; incompressible files; a
  tree with duplicates; sealed; with recovery).
- Buffer recycling: an instrumented pool never hands out a buffer still referenced (a test hook that fills a
  returned buffer with a pattern and checks no written frame ever contains it).
- Reader and writer queues: a read error mid-file, a write error mid-archive, a panic in each thread, and dropping
  the writer with both queues non-empty — each joins every thread, reports or re-raises, and leaves no thread.
- Ignored corpus test: `video` and `encrypted-random` of `small`, `--fast --threads 1` and `--threads 24`, archive
  bytes identical to round 1's, wall times printed.

## Constraints
Branch: continue `task/e2-18-video` (your worktree starts at `main`: first `git merge --ff-only task/e2-18-video`
— if it is not a fast-forward, merge `main` into it instead and say so). `#![forbid(unsafe_code)]`; no new
dependency (`std::thread::scope` or owned threads joined in `Drop`, `std::sync::mpsc` / `Mutex` + `Condvar`); no
`unwrap` outside tests; no figures in docs, comments or commit messages; Conventional Commits ending with
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; commit per property (2, 3, 4, then tests); fmt, clippy
`-D warnings`, nextest workspace, deny green; no PowerShell, no `git stash`, no process killing, no
`git reset --hard`, no subagents, do not run `lpk-bench run`. If you near a turn limit, commit what works and write
what is left into the report before anything else. Report to the path the dispatch names: what moved to which
thread, the buffer pool's size, the before/after walls and shares on `full`'s `video`, the tests, the four tools'
last lines; reply with status, commits, test count, the measured result in words, concerns — at most 8 lines.
