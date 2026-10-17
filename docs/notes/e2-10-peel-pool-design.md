# The Deflate peel pool (design; follow-up of D-69, D-70 and `e2-10-11-design.md` 3.7)

Drafted 2026-10-17 by a read-only planning agent against `main` 18bb9e1 and `task/e2-10-gate-sq` c3de4cf (D-70; merged as
`c3de4cf`). Nothing here is decided until a D-entry records it. Figures come only from committed reports. The runner
rows of 2026-10-17 that motivated it (`bench/results/2026-10-17-megatron-9` to `-11`: the peel losing on two classes,
fixed by D-70, and `lpk/balanced` on `full`'s office-pdf timing out at the runner's 3600 s) are cited without figures.
In short: each examined file splits into two parts. `prepare` is a function of the file's bytes and the settings, and
pool workers run it ahead. `commit` holds every writer-dependent decision and every write, and the feeding thread runs
it in input order. The blocks in flight are chosen as today, and the pool gets the memory they leave. The JPEG peel joins
the same pool.

## 1. Parallelism with byte identity
- **What a file's outcome depends on.** In `peel_file` (D-70) nothing is written between the whole-file check and the
  file's own writes: examine, the probes, the encodes and the gates all come in between. So file k depends on two things.
  (a) Its bytes and the settings: `examine` (its budgets are per file, and the walker's is thread-local, `walk.rs`
  `WORK_LEFT`; `preflate-rs` keeps no global state), the class, the record, the caps, the pre-encode gate, and M (the LZMA
  raw encoder is built per call and the zstd context is released after each stream; `part.rs` tests a reused encoder
  against a fresh one). (b) The writer's state after files 1..k−1: whole-file dedup, the span probe and the run's record
  bytes (records cap), and through them the file gate and the share rule.
- **Where the probe runs.** On the feeding thread, in file k's commit, after every write of files 1..k−1 and before any
  write of file k. That is the only point where the chunk set is the sequential one. A worker that probed earlier would
  miss chunks that earlier in-flight files are about to add. Example: two copies of a PNG in one window; the second
  copy's IDATs count as stored only because the first copy's commit wrote them. All of a file's probes run at that point.
- **Workers precompute, the commit re-checks.** For each reconstructed stream, in order, a worker returns the spans, the
  record, its length, the class and one pure verdict: empty, block cap, memory cap, pre-encode no gain, or encoded (M,
  model, sample bytes, cost). The commit walks the streams exactly as the callback does today: empty → no gain; probe →
  already stored; otherwise the verdict. An encoded stream with cost ≥ length is no gain; otherwise the records cap is
  checked against the run's record bytes, giving records cap or kept. Then come the file gate and the share rule, then
  `write_peeled` or `add_file`. In the sequential order the probe comes before every pure check and the records cap after
  them, so every outcome is the sequential one. Work the sequential run would have skipped is dropped: encodings of
  streams found stored, and the result for a file `add_file_if_known` references. That work is counted outside
  `PeelSummary`'s equality, like the seconds. Sample bytes count only streams the sequential run encodes.
- **Hints.** "Already stored" is monotone: chunks are only added, and `find_known` (with reconstruction blocks allowed)
  never stops answering yes. After each commit the feeding thread publishes the BLAKE3 and length of every examined file
  it wrote whole, and the hash and span lengths of every stream it wrote as a part block or referenced. A worker skips
  `examine` for a hinted file and the encode for a hinted stream. The commit still re-checks. A wrong hint should not
  happen with dedup on (the answer is monotone, and equal bytes are cut equally, which whole-file dedup already assumes);
  if one does, that file's `prepare` re-runs inline (counted; a test hook forces it). Handing each span's chunk hashes to
  the commit would turn the re-check into a lookup, but it needs a writer query by hash: deferred, since the probe already
  runs at GB/s on the feeding thread.
- **Errors and panics** stay attached to their file or stream. The commit raises them only where the sequential run would
  have hit them, so an encoder error on a stream found stored never surfaces. A failed or dropped run cancels and joins
  every worker (D-54, D-60 (2)), waiting at most one encode each. **At one thread** the same code runs: `prepare` just
  before `commit`, with the writer's own probe as the oracle, so stored streams are not encoded, as today.

## 2. Memory (D-67)
- **Reservations** use the model's own formulas (`memory.rs`, `memory_term`, `PartEncoder::memory_for`). A Deflate file
  holds h(f) = 2 × len(f) from admission to the end of its commit (its bytes, then the kept encodings and records — each
  pair shorter than its stream — and one contiguous copy). A JPEG file holds 2 × len(f) + `MODEL_ALLOWANCE`, as
  `fixed_sequential` counts it. A stream reserves r(p) = max(memory_term(p, 0), p + part.memory_for(p)) from its analysis
  to the end of its encode, p the walker's plain bound (`plain_known` + 1, or the plain limit when the size is unknown). R
  is r for the run's largest text (D-69's `min(plain limit, 1032 × largest examined file)`). P_min = 2 × largest examined
  file + R: today's one-file set without the records' term.
- **Sharing with the blocks in flight.** B is chosen first, by D-67's rule with today's one-file set in `fixed`, so B,
  `threads_hint` and the envelope stay exactly D-69's and no archive byte can change through them. The pool gets the rest:
  P = max(P_min, budget − fixed(B, without the peel set) − B × per_block); the records' term stays in `fixed`. Admission
  is a byte semaphore: file k enters the window, in input order, when used + h(k) + R ≤ P. A stream of any file except
  the oldest uncommitted one reserves when used + r + R ≤ P. Only the oldest file's streams may use the last R, so the
  oldest file always makes progress (and the commit acts only for the oldest file): no deadlock. A count cap of 4 × W
  files bounds the window. A worker inside `examine` may hand a stream's encode to an idle worker if it can reserve the
  text; otherwise it encodes inline. It never starts a second `examine` while inside one.
- **Width.** With the pool running, the CPU-bound threads stay at `--threads N`: B = min(D-67's B, N − 1) encode jobs (the
  feeding thread itself when B = 1, D-63) plus W = N − max(B, 1) workers. At N = 1 there is no pool. Store (1 GiB): no
  peel, no pool. Fast (2 GiB): the JPEG peel (and Deflate with `--deflate-peel on`, part encoder zstd, so R is the
  analysis term); D-67 gives Fast many blocks in flight, so P is small — task 4 measures it, splitting by bytes fed to
  peel stages is the follow-up. Balanced (4 GiB): B is 1 whenever a larger examined file is present (D-69 (4)), so P is
  most of what one block leaves; R comes from the 64 MiB part dictionary used for a text above 32 MiB, while typical
  texts reserve a few MiB, so many run at once.
- **One part encoder per worker; no encoder pool.** Between streams an encoder holds no tables (E2-20). Its working set
  exists only during an encode and scales with the text (`dict_for`, `window_for`). So memory is reserved per encode, not
  per worker. A pool of encoders would limit concurrency by count, wasting budget on small texts and still needing
  per-call sizing for large ones.

## 3. Cheaper work
- **Where the time goes.** The part encoder runs Balanced's trial: LZMA preset 9 extreme (BT4, nice_len 273) against
  zstd 22 with long-distance matching; for a text up to `sample_len` (4 MiB) both candidates encode the whole text. On
  `small`, the PDF streams' inflated text is about seventeen times their bytes (694.9 MB from 40.7 MB), and analysis plus
  recreation took 14.5 s on one thread (`bench/reports/phase0-2026-10-03-small.md`, speed table), so the encodes dominate.
  For comparison, Balanced runs near 1.7 MB/s on `small`'s office-pdf (`bench/reports/phase1-2026-10-14-small-threads.md`).
- **Exact savings, no byte changes (task 3).** Hinted duplicates (section 1). Bound files: a stream gains at most its
  length (exactly its length when stored). After the walker, and again after each analysis, compute G = (lengths of the
  streams reconstructed so far) + (span lengths of the candidates left). If G ≤ 256 the file gate fails whatever happens,
  so the file needs no encodes. If 32 × G < len(f) and no stream of the file is hinted stored: with no stream stored at
  commit the file goes whole whatever the encodes would say; with every stream stored no encode is needed; with a mix
  `prepare` re-runs inline (counted). D-70 currently sends these files whole after doing all their work. Their unencoded
  streams get a new bucket, `file bound`, so the outcomes still partition the streams. The analysis is never skipped: the
  share test asserts the probe's per-file taxonomy, and the stored check needs the trimmed spans.
- **Measured A/B, bytes change (task 6).** Every arm keeps the share and round-trip corpus tests green (the 1.2 vectors
  are unaffected: `deflate_vector.rs` writes its own model bytes). Arms: LZMA nice_len 64 at liblzma's default depth
  (preset 9 without extreme); HC4; a 16 MiB part dictionary cap (only texts above 16 MiB change, and R shrinks); zstd 19
  for the part candidate; a lower whole-text threshold for the trial.
- **Skipping LZMA where zstd wins clearly** needs a predictor, because zstd's win is unknown until LZMA has run, and
  every wrong prediction changes bytes. Options: (a) bounded loss — skip LZMA when zstd's stripe output is under a small
  size, losing at most that size; (b) a cheap pre-trial (LZMA preset 1 against zstd 3 on the stripes) that decides when
  its margin is wide; (c) the text's class, already computed (D-69 (6) left it unused). Run a diagnostic first: on
  `small`'s five Deflate classes, record per kept stream the two trial sums, the winner and the length, to price each
  rule. A tighter file-level estimate (a gain predicted by a fast compressor) is also only a predictor; a skip is exact
  only under an upper bound on the gain, and only lengths give one. Every rule must depend only on one file's bytes:
  state carried across files would serialize the pool.

## 4. The JPEG peel
The same pool with the same split. `JpegPeel::peel` is already a pure `&self` function of the bytes. Whole-file dedup
and the primary's `add_part_known` (in `write_peeled`) re-run at commit. The reservation is 2 × len + `MODEL_ALLOWANCE`.
On workers, set lepton's `max_processor_threads` (8 per image) to 1: lepton_jpeg 0.5.8 splits an image by
`max_partitions` and writes the partitions in a fixed order, so the bytes do not change (a test confirms it), and W
workers use W cores. One pool keeps one budget and one thread count, and it settles D-60 (5)'s open item for the photo
classes.

## 5. Tasks, tests, acceptance
1. **Split, no threads.** Split `peel_file` into `prepare` and `commit`; add `examine` hooks after the walker and before
   each analysis. Accept: the one-thread archives of the `deflate_peel.rs` tree and of `small`'s office-pdf and
   photo-raw-png match hashes pinned from the D-70 build; every test green.
2. **Deflate pool.** Work list from `feed_of` in loop order; admission with the reserved R; window cap; deferred errors
   and panics; cancel and join on every path; sequential fallback when a thread cannot start; P, W and the counters in
   `MemoryPlan`, `RunSummary` and `-v`. Accept: identical archives and equal `PeelSummary` at 1, 2, 3, 8 and 24 threads,
   with a hook that delays workers pseudo-randomly, at the minimum budget and a wide one; injected encoder errors and
   panics surface only on streams the commit encodes; no thread outlives a dropped run; a compress-side
   counting-allocator test at 24 threads with P binding stays within the budget; `parallel_corpus.rs` identical and
   matching the pinned hashes.
3. **Exact skips and hints.** Accept: the pinned hashes hold with them on and off; forced misses and the mixed case give
   the same bytes; `deflate_share_corpus.rs` and `deflate_corpus.rs` (both decoders) unchanged.
4. **JPEG through the pool.** Accept: Lepton streams identical at 1 and 8 processor threads (fixtures and `small`'s
   photo-jpeg); Fast and Balanced identical at 1 and N threads and across budgets on both JPEG classes.
5. **Rows.** Re-run `target/acceptance/e2-20b-e2-10-run.sh`, one repeat, idle machine. Accept: `lpk/balanced` on
   `full`'s office-pdf completes within the runner's 3600 s; every `lpk` peak stays within its tier's ceiling (E2-20);
   E2-10's rows and E2-21's full pass come from this run.
6. **Part-encoder A/B** on the pooled build, with a dev setting in the catalogue so one report holds both arms. Any size
   change goes to the owner with the rows; the hashes are re-pinned.
7. **Only if task 5's counters show the feeding thread encoding plain blocks while workers wait on the window.** Encode
   the single block in flight off the feeding thread when the pool runs (amends D-63 (2)). Beyond that, admitting blocks
   by their real size (peeled files close blocks early) is a D-67 follow-up in the writer.

Order: 1 → 2 → (3 ∥ 4) → 5 → 6. One D-entry covers 1–4 (amending D-69 (3), D-60 (5) and D-63 (2)'s thread count);
another covers 6.

## 6. Risks
- **The one-block floor.** At one Balanced block in flight, the plain blocks are encoded on the feeding thread,
  including PDF framing and the embedded JPEGs, which LZMA codes. No pool shortens that chain; task 7 only overlaps it
  with the commits. If task 5 misses the timeout, tasks 6 and 7 are the levers.
- **Determinism leaks**: a worker reading shared state, a nested `examine`, or an encoder with history. `prepare` takes
  only the bytes, the settings and a skip-only oracle, and shuffled-completion tests run at several widths.
- **Memory**: allowances below real use multiply by the number of jobs in flight (allocator overhead, liblzma above its
  own query, preflate past the term). The counting-allocator test and `full`'s peaks catch this; add a margin on P if
  needed.
- **Smaller risks**: wasted work on duplicate-heavy classes (copies inside one window are not hinted), counted and
  bounded by the window; error paths wait for running encodes; parallel reads stay near input order (otherwise route
  them through the D-63 reader); summary semantics change (`file bound`, pool counters), stated in the D-entry; Fast's
  JPEG pool may be memory-starved behind many blocks in flight (task 4).
