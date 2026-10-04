# Probe `entropy-gate`

- corpus: `full` (manifest BLAKE3 `37fd79b4563887d48932715918eaf72e24b1bc8fe7588fd27a280e2fcc4a0cf0`)
- build `78bc9601a767` (release profile, opt-level 3) on `megatron`, 2026-10-04T01:57:47Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 551.0 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libzstd 1.5.7
- note: gate-cost seconds are single-threaded on this machine; raw-read speeds are of this machine's disk; a file system may accept O_DIRECT and still serve reads from its cache (ZFS before 2.3), which cache_bypassed cannot detect
- note: xz preset 9 sizes ran on at most 8 threads (encoder memory)

Blocks of 1 MiB (a final partial block counts from 64 KiB). Positive means incompressible: a gate predicts it when the entropy is at or above the threshold (bits/byte) or when zstd level 1 keeps at least the percentage of the block; the ground truth is xz preset 9 keeping at least the percentage given in the column. Precision = TP/(TP+FP), recall = TP/(TP+FN).

## Main table: video and encrypted-random

### `video`

917 blocks; incompressible blocks by ground truth (xz>=95%: 914, xz>=98%: 913, xz>=99%: 911)

| gate | threshold | precision (xz>=95%) | recall (xz>=95%) | precision (xz>=98%) | recall (xz>=98%) | precision (xz>=99%) | recall (xz>=99%) |
|---|---|---|---|---|---|---|---|
| entropy | 7 | 99.67% | 100.00% | 99.56% | 100.00% | 99.35% | 100.00% |
| entropy | 7.5 | 99.78% | 100.00% | 99.67% | 100.00% | 99.45% | 100.00% |
| entropy | 7.8 | 99.78% | 100.00% | 99.67% | 100.00% | 99.45% | 100.00% |
| entropy | 7.9 | 99.78% | 100.00% | 99.67% | 100.00% | 99.45% | 100.00% |
| entropy | 7.95 | 99.89% | 100.00% | 99.78% | 100.00% | 99.56% | 100.00% |
| entropy | 7.99 | 99.89% | 99.02% | 99.89% | 99.12% | 99.78% | 99.23% |
| sampled_entropy | 7 | 99.67% | 100.00% | 99.56% | 100.00% | 99.35% | 100.00% |
| sampled_entropy | 7.5 | 99.78% | 100.00% | 99.67% | 100.00% | 99.45% | 100.00% |
| sampled_entropy | 7.8 | 99.78% | 99.89% | 99.78% | 100.00% | 99.56% | 100.00% |
| sampled_entropy | 7.9 | 99.78% | 99.89% | 99.78% | 100.00% | 99.56% | 100.00% |
| sampled_entropy | 7.95 | 99.89% | 99.78% | 99.89% | 99.89% | 99.67% | 99.89% |
| sampled_entropy | 7.99 | 99.89% | 97.16% | 99.89% | 97.26% | 99.78% | 97.37% |
| zstd1 | 90% | 99.89% | 100.00% | 99.78% | 100.00% | 99.56% | 100.00% |
| zstd1 | 95% | 100.00% | 100.00% | 99.89% | 100.00% | 99.67% | 100.00% |
| zstd1 | 98% | 100.00% | 99.89% | 100.00% | 100.00% | 99.78% | 100.00% |
| zstd1 | 99% | 100.00% | 99.67% | 100.00% | 99.78% | 100.00% | 100.00% |

### `encrypted-random`

400 blocks; incompressible blocks by ground truth (xz>=95%: 400, xz>=98%: 400, xz>=99%: 400)

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

1317 blocks; incompressible blocks by ground truth (xz>=95%: 1314, xz>=98%: 1313, xz>=99%: 1311)

| gate | threshold | precision (xz>=95%) | recall (xz>=95%) | precision (xz>=98%) | recall (xz>=98%) | precision (xz>=99%) | recall (xz>=99%) |
|---|---|---|---|---|---|---|---|
| entropy | 7 | 99.77% | 100.00% | 99.70% | 100.00% | 99.54% | 100.00% |
| entropy | 7.5 | 99.85% | 100.00% | 99.77% | 100.00% | 99.62% | 100.00% |
| entropy | 7.8 | 99.85% | 100.00% | 99.77% | 100.00% | 99.62% | 100.00% |
| entropy | 7.9 | 99.85% | 100.00% | 99.77% | 100.00% | 99.62% | 100.00% |
| entropy | 7.95 | 99.92% | 100.00% | 99.85% | 100.00% | 99.70% | 100.00% |
| entropy | 7.99 | 99.92% | 99.32% | 99.92% | 99.39% | 99.85% | 99.47% |
| sampled_entropy | 7 | 99.77% | 100.00% | 99.70% | 100.00% | 99.54% | 100.00% |
| sampled_entropy | 7.5 | 99.85% | 100.00% | 99.77% | 100.00% | 99.62% | 100.00% |
| sampled_entropy | 7.8 | 99.85% | 99.92% | 99.85% | 100.00% | 99.70% | 100.00% |
| sampled_entropy | 7.9 | 99.85% | 99.92% | 99.85% | 100.00% | 99.70% | 100.00% |
| sampled_entropy | 7.95 | 99.92% | 99.85% | 99.92% | 99.92% | 99.77% | 99.92% |
| sampled_entropy | 7.99 | 99.92% | 98.02% | 99.92% | 98.10% | 99.84% | 98.17% |
| zstd1 | 90% | 99.92% | 100.00% | 99.85% | 100.00% | 99.70% | 100.00% |
| zstd1 | 95% | 100.00% | 100.00% | 99.92% | 100.00% | 99.77% | 100.00% |
| zstd1 | 98% | 100.00% | 99.92% | 100.00% | 100.00% | 99.85% | 100.00% |
| zstd1 | 99% | 100.00% | 99.77% | 100.00% | 99.85% | 100.00% | 100.00% |

## All other classes (false-positive check)

### pooled

20306 blocks; incompressible blocks by ground truth (xz>=95%: 9954, xz>=98%: 7924, xz>=99%: 6629)

| gate | threshold | precision (xz>=95%) | recall (xz>=95%) | precision (xz>=98%) | recall (xz>=98%) | precision (xz>=99%) | recall (xz>=99%) |
|---|---|---|---|---|---|---|---|
| entropy | 7 | 76.80% | 100.00% | 61.14% | 100.00% | 51.15% | 100.00% |
| entropy | 7.5 | 79.64% | 100.00% | 63.40% | 100.00% | 53.04% | 100.00% |
| entropy | 7.8 | 86.83% | 99.26% | 69.63% | 99.97% | 58.26% | 100.00% |
| entropy | 7.9 | 92.43% | 92.62% | 77.30% | 97.30% | 65.61% | 98.72% |
| entropy | 7.95 | 95.95% | 70.05% | 84.84% | 77.80% | 74.43% | 81.60% |
| entropy | 7.99 | 92.46% | 4.68% | 85.32% | 5.43% | 76.39% | 5.81% |
| sampled_entropy | 7 | 77.33% | 100.00% | 61.56% | 100.00% | 51.50% | 100.00% |
| sampled_entropy | 7.5 | 81.19% | 99.81% | 64.75% | 99.99% | 54.17% | 100.00% |
| sampled_entropy | 7.8 | 88.57% | 96.56% | 71.87% | 98.42% | 60.61% | 99.22% |
| sampled_entropy | 7.9 | 93.47% | 84.43% | 79.67% | 90.40% | 69.09% | 93.71% |
| sampled_entropy | 7.95 | 95.50% | 56.28% | 86.36% | 63.93% | 78.49% | 69.45% |
| sampled_entropy | 7.99 | 91.32% | 3.70% | 84.12% | 4.28% | 79.16% | 4.81% |
| zstd1 | 90% | 86.56% | 100.00% | 68.90% | 100.00% | 57.64% | 100.00% |
| zstd1 | 95% | 93.19% | 99.96% | 74.22% | 100.00% | 62.09% | 100.00% |
| zstd1 | 98% | 98.92% | 89.36% | 88.07% | 99.94% | 73.72% | 100.00% |
| zstd1 | 99% | 99.53% | 76.02% | 95.19% | 91.33% | 87.10% | 99.89% |

### Per class: blocks a gate calls incompressible although xz>=95% says they are not (false positives)

| class | files | dropped tails (<64 KiB) | dropped bytes | blocks | xz>=95% blocks | FP entropy 7.95 | FP sampled_entropy 7.95 | FP zstd1 98% |
|---|---|---|---|---|---|---|---|---|
| archives-nested | 14 | 0 | 0 | 38 | 25 | 11 | 11 | 0 |
| audio | 21 | 1 | 52204 | 160 | 70 | 0 | 0 | 0 |
| backup-versions | 1974 | 1839 | 10177817 | 117 | 9 | 3 | 3 | 0 |
| game-assets | 4660 | 4580 | 39182201 | 80 | 39 | 0 | 0 | 2 |
| logs-text | 11 | 2 | 2954 | 2565 | 0 | 0 | 0 | 0 |
| model-weights | 2 | 0 | 0 | 274 | 0 | 0 | 0 | 0 |
| office-pdf | 2334 | 541 | 19500289 | 3121 | 520 | 239 | 208 | 24 |
| photo-jpeg | 3055 | 224 | 7170561 | 9247 | 8366 | 17 | 19 | 70 |
| photo-jpeg-edited | 600 | 10 | 286379 | 793 | 626 | 8 | 3 | 1 |
| photo-raw-png | 1016 | 910 | 874682 | 564 | 151 | 0 | 0 | 0 |
| small-files | 20000 | 20000 | 50016512 | 0 | 0 | no block >= 64 KiB | no block >= 64 KiB | no block >= 64 KiB |
| software-installed | 9671 | 9010 | 62629621 | 851 | 7 | 0 | 0 | 0 |
| source-git | 663 | 616 | 3394042 | 65 | 18 | 11 | 11 | 0 |
| text-prose | 63 | 1 | 32008 | 375 | 0 | 0 | 0 | 0 |
| vm-image | 4 | 1 | 170 | 2056 | 123 | 5 | 9 | 0 |

## Gate cost (single thread, blocks in memory)

| gate | blocks | bytes | seconds | MB/s of block bytes |
|---|---|---|---|---|
| entropy | 21623 | 18930371291 | 4.202 | 4504.6 |
| sampled_entropy | 21623 | 18930371291 | 0.363 | 52150.2 |
| zstd1 | 21623 | 18930371291 | 13.126 | 1442.2 |

Single-threaded on this machine; each block is touched before the gates are timed, and the zstd output buffer is allocated outside the timed section. Per class (seconds for each gate):

| class | block bytes | entropy | sampled_entropy | zstd1 |
|---|---|---|---|---|
| video | 959731214 | 0.206 (4668.1 MB/s) | 0.014 (70957.7 MB/s) | 0.168 (5714.6 MB/s) |
| encrypted-random | 419430400 | 0.088 (4757.4 MB/s) | 0.006 (67723.2 MB/s) | 0.046 (9213.2 MB/s) |
| archives-nested | 34975551 | 0.008 (4302.7 MB/s) | 0.001 (68714.2 MB/s) | 0.007 (4680.6 MB/s) |
| audio | 157844107 | 0.036 (4342.0 MB/s) | 0.003 (59800.8 MB/s) | 0.099 (1594.1 MB/s) |
| backup-versions | 15363812 | 0.003 (4643.7 MB/s) | 0.000 (32007.9 MB/s) | 0.022 (687.5 MB/s) |
| game-assets | 13288691 | 0.003 (4572.5 MB/s) | 0.000 (33380.3 MB/s) | 0.011 (1237.3 MB/s) |
| logs-text | 2686415063 | 0.561 (4784.4 MB/s) | 0.045 (60354.1 MB/s) | 1.902 (1412.2 MB/s) |
| model-weights | 286816945 | 0.059 (4902.6 MB/s) | 0.004 (67988.7 MB/s) | 0.184 (1556.5 MB/s) |
| office-pdf | 2095190286 | 0.476 (4402.9 MB/s) | 0.045 (46532.5 MB/s) | 1.710 (1225.0 MB/s) |
| photo-jpeg | 8251737956 | 1.760 (4688.5 MB/s) | 0.127 (65126.6 MB/s) | 4.357 (1893.7 MB/s) |
| photo-jpeg-edited | 550388371 | 0.118 (4670.6 MB/s) | 0.009 (61333.5 MB/s) | 0.288 (1910.6 MB/s) |
| photo-raw-png | 536818892 | 0.119 (4495.6 MB/s) | 0.012 (44998.1 MB/s) | 0.689 (778.6 MB/s) |
| software-installed | 374414080 | 0.088 (4244.5 MB/s) | 0.010 (37295.2 MB/s) | 0.636 (588.8 MB/s) |
| source-git | 31971458 | 0.007 (4826.8 MB/s) | 0.001 (62837.0 MB/s) | 0.019 (1659.8 MB/s) |
| text-prose | 360112209 | 0.080 (4479.0 MB/s) | 0.007 (49319.6 MB/s) | 0.665 (541.2 MB/s) |
| vm-image | 2155872256 | 0.590 (3656.5 MB/s) | 0.080 (26935.1 MB/s) | 2.321 (929.0 MB/s) |

## Raw read speed (sequential)

mode: unbuffered, cache bypassed: true, buffer 4194304 bytes aligned to 4096. Read seconds cover the read and close of each file; per-file open calls are timed apart (open seconds). The whole-corpus MB/s columns therefore state both: read only, and including opens. `cache bypassed` is what the open flags asked for; some file systems (ZFS before 2.3) accept O_DIRECT and still serve reads from cache, which this cannot detect.

### Whole corpus

| pass | files | bytes | read seconds | open seconds | MB/s (read only) | MB/s (including opens) |
|---|---|---|---|---|---|---|
| 1 | 44094 | 19123690731 | 8.993 | 3.150 | 2126.5 | 1574.9 |
| 2 | 44094 | 19123690731 | 8.842 | 1.451 | 2162.8 | 1858.0 |
| 3 | 44094 | 19123690731 | 8.830 | 1.440 | 2165.8 | 1862.1 |

### Per class (each row stands on its own)

| pass | class | files | read buffered | bytes | read seconds | open seconds | MB/s (read only) | MB/s (including opens) |
|---|---|---|---|---|---|---|---|---|
| 1 | archives-nested | 14 | 0 | 34975551 | 0.009 | 0.003 | 3741.4 | 2918.3 |
| 1 | audio | 21 | 0 | 157896311 | 0.032 | 0.008 | 4936.6 | 3956.9 |
| 1 | backup-versions | 1974 | 0 | 25541629 | 0.243 | 0.077 | 105.0 | 79.8 |
| 1 | encrypted-random | 2 | 0 | 419430400 | 0.078 | 0.016 | 5347.6 | 4443.4 |
| 1 | game-assets | 4660 | 0 | 52470892 | 0.618 | 0.203 | 84.8 | 63.9 |
| 1 | logs-text | 11 | 0 | 2686418017 | 0.540 | 0.174 | 4976.8 | 3761.3 |
| 1 | model-weights | 2 | 0 | 286816945 | 0.053 | 0.023 | 5423.7 | 3763.8 |
| 1 | office-pdf | 2334 | 0 | 2114690575 | 0.861 | 0.275 | 2457.5 | 1862.2 |
| 1 | photo-jpeg | 3055 | 0 | 8258908517 | 1.918 | 0.803 | 4306.7 | 3035.5 |
| 1 | photo-jpeg-edited | 600 | 0 | 550674750 | 0.212 | 0.074 | 2596.0 | 1922.1 |
| 1 | photo-raw-png | 1016 | 0 | 537693574 | 0.168 | 0.080 | 3205.6 | 2168.4 |
| 1 | small-files | 20000 | 0 | 50016512 | 2.164 | 0.683 | 23.1 | 17.6 |
| 1 | software-installed | 9671 | 0 | 437043701 | 1.308 | 0.429 | 334.2 | 251.6 |
| 1 | source-git | 663 | 0 | 35365500 | 0.096 | 0.032 | 369.2 | 275.8 |
| 1 | text-prose | 63 | 0 | 360144217 | 0.080 | 0.029 | 4521.4 | 3301.9 |
| 1 | video | 4 | 0 | 959731214 | 0.203 | 0.067 | 4725.2 | 3559.2 |
| 1 | vm-image | 4 | 0 | 2155872426 | 0.410 | 0.173 | 5256.2 | 3698.4 |
| 2 | archives-nested | 14 | 0 | 34975551 | 0.008 | 0.001 | 4204.6 | 3965.5 |
| 2 | audio | 21 | 0 | 157896311 | 0.032 | 0.001 | 4939.7 | 4794.8 |
| 2 | backup-versions | 1974 | 0 | 25541629 | 0.243 | 0.061 | 105.2 | 84.1 |
| 2 | encrypted-random | 2 | 0 | 419430400 | 0.080 | 0.000 | 5215.4 | 5203.9 |
| 2 | game-assets | 4660 | 0 | 52470892 | 0.599 | 0.174 | 87.5 | 67.9 |
| 2 | logs-text | 11 | 0 | 2686418017 | 0.538 | 0.001 | 4989.0 | 4978.5 |
| 2 | model-weights | 2 | 0 | 286816945 | 0.053 | 0.000 | 5409.4 | 5391.5 |
| 2 | office-pdf | 2334 | 0 | 2114690575 | 0.855 | 0.077 | 2472.3 | 2269.1 |
| 2 | photo-jpeg | 3055 | 0 | 8258908517 | 1.869 | 0.142 | 4420.0 | 4108.5 |
| 2 | photo-jpeg-edited | 600 | 0 | 550674750 | 0.205 | 0.026 | 2689.8 | 2390.5 |
| 2 | photo-raw-png | 1016 | 0 | 537693574 | 0.161 | 0.034 | 3339.2 | 2756.0 |
| 2 | small-files | 20000 | 0 | 50016512 | 2.198 | 0.554 | 22.8 | 18.2 |
| 2 | software-installed | 9671 | 0 | 437043701 | 1.211 | 0.354 | 360.8 | 279.2 |
| 2 | source-git | 663 | 0 | 35365500 | 0.088 | 0.023 | 403.5 | 319.2 |
| 2 | text-prose | 63 | 0 | 360144217 | 0.080 | 0.003 | 4496.7 | 4323.0 |
| 2 | video | 4 | 0 | 959731214 | 0.206 | 0.000 | 4670.1 | 4663.4 |
| 2 | vm-image | 4 | 0 | 2155872426 | 0.416 | 0.000 | 5186.4 | 5181.3 |
| 3 | archives-nested | 14 | 0 | 34975551 | 0.009 | 0.001 | 3720.1 | 3445.8 |
| 3 | audio | 21 | 0 | 157896311 | 0.032 | 0.001 | 4945.7 | 4807.3 |
| 3 | backup-versions | 1974 | 0 | 25541629 | 0.245 | 0.060 | 104.1 | 83.6 |
| 3 | encrypted-random | 2 | 0 | 419430400 | 0.080 | 0.000 | 5256.1 | 5246.9 |
| 3 | game-assets | 4660 | 0 | 52470892 | 0.597 | 0.167 | 88.0 | 68.8 |
| 3 | logs-text | 11 | 0 | 2686418017 | 0.537 | 0.001 | 5000.9 | 4993.4 |
| 3 | model-weights | 2 | 0 | 286816945 | 0.053 | 0.000 | 5380.8 | 5363.1 |
| 3 | office-pdf | 2334 | 0 | 2114690575 | 0.836 | 0.077 | 2530.6 | 2316.2 |
| 3 | photo-jpeg | 3055 | 0 | 8258908517 | 1.894 | 0.142 | 4361.5 | 4057.3 |
| 3 | photo-jpeg-edited | 600 | 0 | 550674750 | 0.208 | 0.024 | 2652.8 | 2379.6 |
| 3 | photo-raw-png | 1016 | 0 | 537693574 | 0.166 | 0.035 | 3241.5 | 2676.7 |
| 3 | small-files | 20000 | 0 | 50016512 | 2.188 | 0.549 | 22.9 | 18.3 |
| 3 | software-installed | 9671 | 0 | 437043701 | 1.202 | 0.357 | 363.5 | 280.3 |
| 3 | source-git | 663 | 0 | 35365500 | 0.089 | 0.023 | 395.4 | 313.7 |
| 3 | text-prose | 63 | 0 | 360144217 | 0.079 | 0.002 | 4551.8 | 4432.8 |
| 3 | video | 4 | 0 | 959731214 | 0.202 | 0.000 | 4752.0 | 4743.9 |
| 3 | vm-image | 4 | 0 | 2155872426 | 0.413 | 0.001 | 5217.5 | 5210.1 |
