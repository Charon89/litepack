# Probe `bwt`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `7f614f9be362` (release profile, opt-level 3) on `megatron`, 2026-10-17T20:14:37Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 565.9 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libsais 2.10.3, libzstd 1.5.7, lzma-rs (decode) 0.3.0, ruzstd (decode) 0.9.0

The E2-15 measured gate: BWT plus entropy coding against the Balanced tier's LZMA on the text classes. Files in the Balanced tier's order (walk, then clusters), cut into blocks the way the Balanced pipeline closes them: BWT candidates and `lzma` on 67108864 byte blocks (the envelope's default BWT cap), `lzma-balanced-blocks` on 268435456 byte blocks (the Balanced block); a block also closes at a cluster change once it holds 33554432 bytes. One thread, blocks one after another, each step timed alone; MB/s are of the plain bytes (10^6 bytes per second). BWT rows include the transform in compress and the inverse in decompress. Every block is decoded back and compared.

| class | candidate | blocks | plain bytes | compressed bytes | of plain | compress s | decompress s | bwt / unbwt s | status |
|---|---|---|---|---|---|---|---|---|---|
| text-prose | bwt+lzma | 3 | 148205637 | 37044622 | 25.00% | 82.19 (1.8 MB/s) | 5.20 (28.5 MB/s) | 3.79 / 2.93 | verified |
| text-prose | bwt+zstd | 3 | 148205637 | 38929998 | 26.27% | 69.46 (2.1 MB/s) | 3.51 (42.2 MB/s) | 3.79 / 2.93 | verified |
| text-prose | bwt+ari | 3 | 148205637 | 35876961 | 24.21% | 7.18 (20.6 MB/s) | 4.84 (30.6 MB/s) | 3.79 / 2.93 | verified |
| text-prose | lzma | 3 | 148205637 | 38844551 | 26.21% | 101.45 (1.5 MB/s) | 2.14 (69.1 MB/s) | - / - | verified |
| text-prose | lzma-balanced-blocks | 1 | 148205637 | 38023384 | 25.66% | 123.01 (1.2 MB/s) | 2.18 (67.9 MB/s) | - / - | verified |
| logs-text | bwt+lzma | 2 | 111493402 | 6564062 | 5.89% | 24.68 (4.5 MB/s) | 2.70 (41.2 MB/s) | 2.47 / 2.03 | verified |
| logs-text | bwt+zstd | 2 | 111493402 | 6809783 | 6.11% | 20.83 (5.4 MB/s) | 2.28 (49.0 MB/s) | 2.47 / 2.03 | verified |
| logs-text | bwt+ari | 2 | 111493402 | 6322582 | 5.67% | 3.84 (29.0 MB/s) | 2.37 (47.0 MB/s) | 2.47 / 2.03 | verified |
| logs-text | lzma | 2 | 111493402 | 7254388 | 6.51% | 64.21 (1.7 MB/s) | 0.66 (168.0 MB/s) | - / - | verified |
| logs-text | lzma-balanced-blocks | 1 | 111493402 | 7254691 | 6.51% | 64.23 (1.7 MB/s) | 0.64 (175.6 MB/s) | - / - | verified |

Candidates:

- `bwt+lzma`: (a) libsais BWT, then the Balanced LZMA (64 MiB dict, lc3 lp0 pb2, preset 9e, nice 273)
- `bwt+zstd`: (a) libsais BWT, then zstd level 22, long matching, window = LZMA dictionary rule
- `bwt+ari`: (b) libsais BWT, then MTF + zero-run + order-1 adaptive binary arithmetic coder
- `lzma`: (c) the Balanced LZMA on the same (BWT-cap) blocks
- `lzma-balanced-blocks`: (c') the Balanced LZMA on the Balanced tier's own blocks

Verdict per class (smallest BWT candidate against the smaller LZMA row):

- `text-prose`: `bwt+ari` 35876961 bytes against `lzma-balanced-blocks` 38023384 bytes: smaller
- `logs-text`: `bwt+ari` 6322582 bytes against `lzma` 7254388 bytes: smaller

Gate: OPEN (a BWT candidate is smaller than the LZMA row on both classes)
