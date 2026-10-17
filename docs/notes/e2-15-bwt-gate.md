# E2-15 — the BWT measured gate: result on `small` (report; 2026-10-17)

Source of every figure below: `bench/results/2026-10-17-megatron-15/probe-bwt.json` (its table is
`probe-bwt.md` next to it). Command: `cargo run --release -p lpk-bench -- probe bwt --profile small`
(build `7f614f9be362`, release, one thread in timed sections, corpus `small` manifest `8daaaf66…`).
The brief is `docs/notes/e2-15-probe-brief.md`; the codec is `crates/lpk-codecs-bwt`.

## What was measured

Files in the Balanced tier's order (`walk` + `cluster`), cut into blocks the way the Balanced
pipeline closes them. BWT candidates and the `lzma` row use 64 MiB blocks (the envelope's default
`max_bwt_block`); `lzma-balanced-blocks` uses the Balanced 256 MiB block. Every block of every
candidate was decoded back and compared (all rows `verified`). Decoders are the ones a reader ships:
the format's `lzma` (lzma-rs) and `zstd` (ruzstd) decoders, and the crate's own decoder and safe
inverse BWT.

## Sizes (compressed bytes; smaller is better)

| class | plain | (a) bwt+lzma | (a) bwt+zstd | (b) bwt+ari | (c) lzma, 64 MiB blocks | (c') lzma, Balanced blocks |
|---|---|---|---|---|---|---|
| text-prose | 148205637 | 37044622 | 38929998 | **35876961** | 38844551 | 38023384 |
| logs-text | 111493402 | 6564062 | 6809783 | **6322582** | 7254388 | 7254691 |

## Times (seconds, one thread, blocks one after another)

| class | candidate | compress s | decompress s | of which bwt / unbwt s |
|---|---|---|---|---|
| text-prose | bwt+ari | 7.18 | 4.84 | 3.79 / 2.93 |
| text-prose | bwt+lzma | 82.19 | 5.20 | 3.79 / 2.93 |
| text-prose | lzma-balanced-blocks | 123.01 | 2.18 | - |
| logs-text | bwt+ari | 3.84 | 2.37 | 2.47 / 2.03 |
| logs-text | bwt+lzma | 24.68 | 2.70 | 2.47 / 2.03 |
| logs-text | lzma | 64.21 | 0.66 | - |

(All five rows per class, with MB/s, are in `probe-bwt.md`.)

## Verdict

**The gate opens on `small`.** On both classes the smallest BWT candidate, (b) `bwt+ari`, is
smaller than the smaller LZMA row: text-prose 35876961 against 38023384 (`lzma-balanced-blocks`),
logs-text 6322582 against 7254388 (`lzma`). Candidate (a) `bwt+lzma` (the `[lzma, bwt]` graph the
format can express today) is also smaller than both LZMA rows on both classes; `bwt+zstd` is not
smaller than `lzma-balanced-blocks` on text-prose.

Extraction cost: (b) decodes more slowly than LZMA through lzma-rs — 4.84 s against 2.18 s on
text-prose, 2.37 s against 0.66 s on logs-text, single-threaded; the inverse BWT is most of it.
Compression with (b) is much faster than the Balanced LZMA (7.18 s against 123.01 s on text-prose).

## Reference-row check

The brief asks that the reference row match the runner's `lpk/balanced` size within the frame
overhead. The latest committed runner rows on this corpus (`bench/results/2026-10-17-megatron-7`,
`lpk-balanced-*.json`) are 38101440 bytes (text-prose) and 7306531 bytes (logs-text) for the whole
archive. `lzma-balanced-blocks` is 78056 and 51840 bytes smaller respectively. That gap is larger
than block frames alone; the archive also carries the entry table, index, hashes and trailer, and
the pipeline's zstd trial and Fold are not replayed by the probe. The gap is under 1% of each row
and far below the BWT margin on both classes, so it does not change the verdict, but it is not
explained to the byte here.

## Next

Per the brief: D-05 is to be amended (the gate opens), and the revision 1.3 design for BWT
(a decoding written out, the entropy primitive for (b), vectors, the independence gate) follows as
the next task. `full` was not run: the margins are not close.

## The gap between candidate (c) and the runner's row (review note)

The read-only review traced the probe's LZMA candidate being smaller than the runner's `lpk/balanced` archive to the archive's chunk table: about 35 bytes per chunk (a varint length and a 32-byte BLAKE3 hash) at the Fold's 64 KiB average chunk — the reviewer's arithmetic, not a measurement; a BWT archive pays the same table, so the comparison between the candidates stands.
