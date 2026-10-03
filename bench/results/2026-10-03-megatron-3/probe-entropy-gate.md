# Probe `entropy-gate`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `6c04dedc7d5a` (release profile, opt-level 3) on `megatron`, 2026-10-03T11:02:39Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 81.5 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libzstd 1.5.7
- note: gate-cost seconds are single-threaded on this machine; raw-read speeds are of this machine's disk; a file system may accept O_DIRECT and still serve reads from its cache (ZFS before 2.3), which cache_bypassed cannot detect
- note: xz preset 9 sizes ran on at most 8 threads (encoder memory)

Blocks of 1 MiB (a final partial block counts from 64 KiB). Positive means incompressible: a gate predicts it when the entropy is at or above the threshold (bits/byte) or when zstd level 1 keeps at least the percentage of the block; the ground truth is xz preset 9 keeping at least the percentage given in the column. Precision = TP/(TP+FP), recall = TP/(TP+FN).

## Main table: video and encrypted-random

### `video`

64 blocks; incompressible blocks by ground truth (xz>=95%: 63, xz>=98%: 63, xz>=99%: 61)

| gate | threshold | precision (xz>=95%) | recall (xz>=95%) | precision (xz>=98%) | recall (xz>=98%) | precision (xz>=99%) | recall (xz>=99%) |
|---|---|---|---|---|---|---|---|
| entropy | 7 | 98.44% | 100.00% | 98.44% | 100.00% | 95.31% | 100.00% |
| entropy | 7.5 | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| entropy | 7.8 | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| entropy | 7.9 | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| entropy | 7.95 | 100.00% | 96.83% | 100.00% | 96.83% | 96.72% | 96.72% |
| entropy | 7.99 | 100.00% | 3.17% | 100.00% | 3.17% | 100.00% | 3.28% |
| sampled_entropy | 7 | 98.44% | 100.00% | 98.44% | 100.00% | 95.31% | 100.00% |
| sampled_entropy | 7.5 | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| sampled_entropy | 7.8 | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| sampled_entropy | 7.9 | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| sampled_entropy | 7.95 | 100.00% | 95.24% | 100.00% | 95.24% | 96.67% | 95.08% |
| sampled_entropy | 7.99 | n/a | 0.00% | n/a | 0.00% | n/a | 0.00% |
| zstd1 | 90% | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| zstd1 | 95% | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| zstd1 | 98% | 100.00% | 100.00% | 100.00% | 100.00% | 96.83% | 100.00% |
| zstd1 | 99% | 100.00% | 98.41% | 100.00% | 98.41% | 98.39% | 100.00% |

### `encrypted-random`

64 blocks; incompressible blocks by ground truth (xz>=95%: 64, xz>=98%: 64, xz>=99%: 64)

| gate | threshold | precision (xz>=95%) | recall (xz>=95%) | precision (xz>=98%) | recall (xz>=98%) | precision (xz>=99%) | recall (xz>=99%) |
|---|---|---|---|---|---|---|---|
| entropy | 7 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| entropy | 7.5 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| entropy | 7.8 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| entropy | 7.9 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| entropy | 7.95 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| entropy | 7.99 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| sampled_entropy | 7 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| sampled_entropy | 7.5 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| sampled_entropy | 7.8 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| sampled_entropy | 7.9 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| sampled_entropy | 7.95 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| sampled_entropy | 7.99 | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| zstd1 | 90% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| zstd1 | 95% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| zstd1 | 98% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |
| zstd1 | 99% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% | 100.00% |

### both classes pooled

128 blocks; incompressible blocks by ground truth (xz>=95%: 127, xz>=98%: 127, xz>=99%: 125)

| gate | threshold | precision (xz>=95%) | recall (xz>=95%) | precision (xz>=98%) | recall (xz>=98%) | precision (xz>=99%) | recall (xz>=99%) |
|---|---|---|---|---|---|---|---|
| entropy | 7 | 99.22% | 100.00% | 99.22% | 100.00% | 97.66% | 100.00% |
| entropy | 7.5 | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| entropy | 7.8 | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| entropy | 7.9 | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| entropy | 7.95 | 100.00% | 98.43% | 100.00% | 98.43% | 98.40% | 98.40% |
| entropy | 7.99 | 100.00% | 51.97% | 100.00% | 51.97% | 100.00% | 52.80% |
| sampled_entropy | 7 | 99.22% | 100.00% | 99.22% | 100.00% | 97.66% | 100.00% |
| sampled_entropy | 7.5 | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| sampled_entropy | 7.8 | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| sampled_entropy | 7.9 | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| sampled_entropy | 7.95 | 100.00% | 97.64% | 100.00% | 97.64% | 98.39% | 97.60% |
| sampled_entropy | 7.99 | 100.00% | 50.39% | 100.00% | 50.39% | 100.00% | 51.20% |
| zstd1 | 90% | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| zstd1 | 95% | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| zstd1 | 98% | 100.00% | 100.00% | 100.00% | 100.00% | 98.43% | 100.00% |
| zstd1 | 99% | 100.00% | 99.21% | 100.00% | 99.21% | 99.21% | 100.00% |

## All other classes (false-positive check)

### pooled

2151 blocks; incompressible blocks by ground truth (xz>=95%: 836, xz>=98%: 646, xz>=99%: 513)

| gate | threshold | precision (xz>=95%) | recall (xz>=95%) | precision (xz>=98%) | recall (xz>=98%) | precision (xz>=99%) | recall (xz>=99%) |
|---|---|---|---|---|---|---|---|
| entropy | 7 | 73.92% | 100.00% | 57.12% | 100.00% | 45.36% | 100.00% |
| entropy | 7.5 | 79.62% | 100.00% | 61.52% | 100.00% | 48.86% | 100.00% |
| entropy | 7.8 | 86.47% | 99.40% | 67.22% | 100.00% | 53.38% | 100.00% |
| entropy | 7.9 | 92.80% | 92.46% | 76.23% | 98.30% | 60.62% | 98.44% |
| entropy | 7.95 | 95.62% | 73.09% | 84.35% | 83.44% | 69.01% | 85.96% |
| entropy | 7.99 | 100.00% | 17.94% | 97.33% | 22.60% | 90.00% | 26.32% |
| sampled_entropy | 7 | 73.92% | 100.00% | 57.12% | 100.00% | 45.36% | 100.00% |
| sampled_entropy | 7.5 | 80.74% | 98.80% | 62.95% | 99.69% | 50.15% | 100.00% |
| sampled_entropy | 7.8 | 86.78% | 90.31% | 70.34% | 94.74% | 57.24% | 97.08% |
| sampled_entropy | 7.9 | 92.50% | 75.24% | 78.09% | 82.20% | 65.44% | 86.74% |
| sampled_entropy | 7.95 | 95.36% | 54.07% | 86.71% | 63.62% | 73.00% | 67.45% |
| sampled_entropy | 7.99 | 97.86% | 16.39% | 94.29% | 20.43% | 88.57% | 24.17% |
| zstd1 | 90% | 81.72% | 100.00% | 63.15% | 100.00% | 50.15% | 100.00% |
| zstd1 | 95% | 92.67% | 99.88% | 71.70% | 100.00% | 56.94% | 100.00% |
| zstd1 | 98% | 99.32% | 87.92% | 87.30% | 100.00% | 69.32% | 100.00% |
| zstd1 | 99% | 99.66% | 71.05% | 95.64% | 88.24% | 86.07% | 100.00% |

### Per class: blocks a gate calls incompressible although xz>=95% says they are not (false positives)

| class | files | dropped tails (<64 KiB) | dropped bytes | blocks | xz>=95% blocks | FP entropy 7.95 | FP sampled_entropy 7.95 | FP zstd1 98% |
|---|---|---|---|---|---|---|---|---|
| archives-nested | 14 | 0 | 0 | 38 | 33 | 3 | 3 | 0 |
| audio | 21 | 1 | 52204 | 160 | 70 | 0 | 0 | 0 |
| backup-versions | 1974 | 1839 | 10177817 | 117 | 9 | 3 | 3 | 0 |
| game-assets | 4660 | 4580 | 39182201 | 80 | 39 | 0 | 0 | 2 |
| logs-text | 4 | 1 | 36837 | 108 | 0 | 0 | 0 | 0 |
| model-weights | 1 | 0 | 0 | 17 | 0 | 0 | 0 | 0 |
| office-pdf | 166 | 48 | 1599120 | 329 | 52 | 15 | 10 | 1 |
| photo-jpeg | 355 | 29 | 926490 | 504 | 455 | 1 | 2 | 2 |
| photo-jpeg-edited | 60 | 1 | 27565 | 63 | 48 | 1 | 0 | 0 |
| photo-raw-png | 950 | 907 | 843583 | 130 | 50 | 0 | 0 | 0 |
| small-files | 20000 | 20000 | 66170985 | 0 | 0 | no block >= 64 KiB | no block >= 64 KiB | no block >= 64 KiB |
| software-installed | 433 | 278 | 4501517 | 225 | 5 | 0 | 0 | 0 |
| source-git | 664 | 617 | 3395846 | 46 | 8 | 2 | 2 | 0 |
| text-prose | 51 | 1 | 32008 | 168 | 0 | 0 | 0 | 0 |
| vm-image | 2 | 0 | 0 | 166 | 67 | 3 | 2 | 0 |

## Gate cost (single thread, blocks in memory)

| gate | blocks | bytes | seconds | MB/s of block bytes |
|---|---|---|---|---|
| entropy | 2279 | 1682157220 | 0.377 | 4463.4 |
| sampled_entropy | 2279 | 1682157220 | 0.036 | 46585.2 |
| zstd1 | 2279 | 1682157220 | 1.512 | 1112.9 |

Single-threaded on this machine; each block is touched before the gates are timed, and the zstd output buffer is allocated outside the timed section. Per class (seconds for each gate):

| class | block bytes | entropy | sampled_entropy | zstd1 |
|---|---|---|---|---|
| video | 66228217 | 0.014 (4875.0 MB/s) | 0.001 (72104.8 MB/s) | 0.041 (1626.6 MB/s) |
| encrypted-random | 67108864 | 0.015 (4521.5 MB/s) | 0.001 (68576.4 MB/s) | 0.008 (8503.9 MB/s) |
| archives-nested | 34862973 | 0.007 (4928.5 MB/s) | 0.000 (71352.8 MB/s) | 0.008 (4605.0 MB/s) |
| audio | 157844107 | 0.034 (4669.4 MB/s) | 0.003 (61489.7 MB/s) | 0.094 (1675.1 MB/s) |
| backup-versions | 15363812 | 0.003 (4512.3 MB/s) | 0.000 (31671.4 MB/s) | 0.021 (723.0 MB/s) |
| game-assets | 13288691 | 0.003 (4415.6 MB/s) | 0.000 (32811.6 MB/s) | 0.011 (1183.9 MB/s) |
| logs-text | 111456565 | 0.023 (4902.8 MB/s) | 0.002 (60981.9 MB/s) | 0.114 (977.3 MB/s) |
| model-weights | 17756393 | 0.004 (4371.5 MB/s) | 0.000 (71425.6 MB/s) | 0.012 (1473.0 MB/s) |
| office-pdf | 266263031 | 0.062 (4311.9 MB/s) | 0.007 (38785.0 MB/s) | 0.280 (951.3 MB/s) |
| photo-jpeg | 326220327 | 0.067 (4871.1 MB/s) | 0.005 (63009.7 MB/s) | 0.181 (1806.4 MB/s) |
| photo-jpeg-edited | 32703491 | 0.007 (4864.3 MB/s) | 0.001 (59973.4 MB/s) | 0.017 (1937.6 MB/s) |
| photo-raw-png | 117080932 | 0.025 (4594.9 MB/s) | 0.002 (53490.9 MB/s) | 0.123 (953.7 MB/s) |
| software-installed | 122344526 | 0.028 (4369.7 MB/s) | 0.003 (41944.8 MB/s) | 0.213 (575.1 MB/s) |
| source-git | 11398046 | 0.002 (4764.9 MB/s) | 0.000 (45720.2 MB/s) | 0.010 (1191.0 MB/s) |
| text-prose | 148173629 | 0.031 (4773.3 MB/s) | 0.002 (66048.7 MB/s) | 0.340 (435.3 MB/s) |
| vm-image | 174063616 | 0.052 (3347.0 MB/s) | 0.008 (21746.5 MB/s) | 0.040 (4375.0 MB/s) |

## Raw read speed (sequential)

mode: unbuffered, cache bypassed: true, buffer 4194304 bytes aligned to 4096. Read seconds cover the read and close of each file; per-file open calls are timed apart (open seconds). The whole-corpus MB/s columns therefore state both: read only, and including opens. `cache bypassed` is what the open flags asked for; some file systems (ZFS before 2.3) accept O_DIRECT and still serve reads from cache, which this cannot detect.

### Whole corpus

| pass | files | bytes | read seconds | open seconds | MB/s (read only) | MB/s (including opens) |
|---|---|---|---|---|---|---|
| 1 | 29360 | 1809103393 | 4.746 | 1.949 | 381.2 | 270.2 |
| 2 | 29360 | 1809103393 | 4.028 | 1.105 | 449.2 | 352.5 |
| 3 | 29360 | 1809103393 | 4.079 | 1.164 | 443.5 | 345.0 |

### Per class (each row stands on its own)

| pass | class | files | read buffered | bytes | read seconds | open seconds | MB/s (read only) | MB/s (including opens) |
|---|---|---|---|---|---|---|---|---|
| 1 | archives-nested | 14 | 0 | 34862973 | 0.009 | 0.003 | 3996.2 | 2875.6 |
| 1 | audio | 21 | 0 | 157896311 | 0.032 | 0.016 | 4968.9 | 3294.8 |
| 1 | backup-versions | 1974 | 0 | 25541629 | 0.247 | 0.083 | 103.3 | 77.3 |
| 1 | encrypted-random | 2 | 0 | 67108864 | 0.013 | 0.006 | 5345.7 | 3689.4 |
| 1 | game-assets | 4660 | 0 | 52470892 | 0.526 | 0.200 | 99.7 | 72.3 |
| 1 | logs-text | 4 | 0 | 111493402 | 0.021 | 0.010 | 5236.6 | 3584.6 |
| 1 | model-weights | 1 | 0 | 17756393 | 0.003 | 0.002 | 5087.1 | 3484.5 |
| 1 | office-pdf | 166 | 0 | 267862151 | 0.079 | 0.031 | 3369.8 | 2415.0 |
| 1 | photo-jpeg | 355 | 0 | 327146817 | 0.126 | 0.049 | 2593.3 | 1868.3 |
| 1 | photo-jpeg-edited | 60 | 0 | 32731056 | 0.018 | 0.007 | 1815.3 | 1314.7 |
| 1 | photo-raw-png | 950 | 0 | 117924515 | 0.078 | 0.046 | 1518.1 | 956.4 |
| 1 | small-files | 20000 | 0 | 66170985 | 3.341 | 1.399 | 19.8 | 14.0 |
| 1 | software-installed | 433 | 0 | 126846043 | 0.084 | 0.031 | 1509.0 | 1104.7 |
| 1 | source-git | 664 | 0 | 14793892 | 0.083 | 0.029 | 178.9 | 132.7 |
| 1 | text-prose | 51 | 0 | 148205637 | 0.036 | 0.015 | 4094.9 | 2885.9 |
| 1 | video | 3 | 0 | 66228217 | 0.013 | 0.006 | 4951.9 | 3507.5 |
| 1 | vm-image | 2 | 0 | 174063616 | 0.036 | 0.018 | 4902.8 | 3282.9 |
| 2 | archives-nested | 14 | 0 | 34862973 | 0.009 | 0.001 | 4077.8 | 3718.7 |
| 2 | audio | 21 | 0 | 157896311 | 0.032 | 0.001 | 4965.2 | 4792.4 |
| 2 | backup-versions | 1974 | 0 | 25541629 | 0.251 | 0.069 | 101.6 | 79.8 |
| 2 | encrypted-random | 2 | 0 | 67108864 | 0.013 | 0.000 | 5295.1 | 5238.6 |
| 2 | game-assets | 4660 | 0 | 52470892 | 0.526 | 0.178 | 99.8 | 74.5 |
| 2 | logs-text | 4 | 0 | 111493402 | 0.021 | 0.000 | 5213.6 | 5156.1 |
| 2 | model-weights | 1 | 0 | 17756393 | 0.003 | 0.000 | 5169.0 | 5034.7 |
| 2 | office-pdf | 166 | 0 | 267862151 | 0.075 | 0.006 | 3581.9 | 3320.8 |
| 2 | photo-jpeg | 355 | 0 | 327146817 | 0.120 | 0.014 | 2725.8 | 2443.2 |
| 2 | photo-jpeg-edited | 60 | 0 | 32731056 | 0.017 | 0.002 | 1898.4 | 1683.3 |
| 2 | photo-raw-png | 950 | 0 | 117924515 | 0.074 | 0.029 | 1587.0 | 1143.3 |
| 2 | small-files | 20000 | 0 | 66170985 | 2.630 | 0.757 | 25.2 | 19.5 |
| 2 | software-installed | 433 | 0 | 126846043 | 0.087 | 0.020 | 1462.3 | 1192.3 |
| 2 | source-git | 664 | 0 | 14793892 | 0.085 | 0.026 | 174.6 | 133.8 |
| 2 | text-prose | 51 | 0 | 148205637 | 0.036 | 0.003 | 4108.7 | 3814.3 |
| 2 | video | 3 | 0 | 66228217 | 0.013 | 0.000 | 4983.6 | 4905.1 |
| 2 | vm-image | 2 | 0 | 174063616 | 0.035 | 0.000 | 5003.0 | 4971.2 |
| 3 | archives-nested | 14 | 0 | 34862973 | 0.008 | 0.001 | 4207.6 | 3903.9 |
| 3 | audio | 21 | 0 | 157896311 | 0.032 | 0.001 | 4986.1 | 4783.0 |
| 3 | backup-versions | 1974 | 0 | 25541629 | 0.262 | 0.085 | 97.6 | 73.7 |
| 3 | encrypted-random | 2 | 0 | 67108864 | 0.013 | 0.000 | 5295.4 | 5252.0 |
| 3 | game-assets | 4660 | 0 | 52470892 | 0.573 | 0.209 | 91.5 | 67.1 |
| 3 | logs-text | 4 | 0 | 111493402 | 0.021 | 0.000 | 5270.0 | 5204.5 |
| 3 | model-weights | 1 | 0 | 17756393 | 0.004 | 0.000 | 4946.6 | 4854.0 |
| 3 | office-pdf | 166 | 0 | 267862151 | 0.077 | 0.007 | 3488.9 | 3180.0 |
| 3 | photo-jpeg | 355 | 0 | 327146817 | 0.122 | 0.019 | 2674.7 | 2322.8 |
| 3 | photo-jpeg-edited | 60 | 0 | 32731056 | 0.017 | 0.003 | 1901.9 | 1647.3 |
| 3 | photo-raw-png | 950 | 0 | 117924515 | 0.078 | 0.040 | 1511.1 | 1002.4 |
| 3 | small-files | 20000 | 0 | 66170985 | 2.615 | 0.750 | 25.3 | 19.7 |
| 3 | software-installed | 433 | 0 | 126846043 | 0.085 | 0.019 | 1485.8 | 1212.4 |
| 3 | source-git | 664 | 0 | 14793892 | 0.087 | 0.028 | 170.2 | 128.6 |
| 3 | text-prose | 51 | 0 | 148205637 | 0.037 | 0.002 | 4047.0 | 3804.2 |
| 3 | video | 3 | 0 | 66228217 | 0.013 | 0.000 | 4912.9 | 4836.6 |
| 3 | vm-image | 2 | 0 | 174063616 | 0.035 | 0.000 | 4951.1 | 4927.0 |
