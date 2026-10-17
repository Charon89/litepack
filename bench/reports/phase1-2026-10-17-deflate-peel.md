# Phase 1 — E2-10 and E2-21: the Deflate peel against the incumbents, and the full-corpus round trip, 2026-10-17

Every figure below is a median of one repeat from the committed result directories named with it; the generated reports of the final run follow unchanged. The final run was launched detached on an idle machine from the release build `d4b2838` (the peel pool, D-71, with the gate of D-70); the first run (`18bb9e1`, before both) is kept as the evidence that led to D-70 and D-71.

- First run (`18bb9e1`): `bench/results/2026-10-17-megatron-9` (every tool on office-pdf, photo-raw-png, software-installed of `small`), `2026-10-17-megatron-10` (`lpk` on the other classes of `small`), `2026-10-17-megatron-11` (`lpk` `fast` and `balanced` on every class of `full`).
- Final run (`d4b2838`): `bench/results/2026-10-17-megatron-12`, `2026-10-17-megatron-13`, `2026-10-17-megatron-14`, the same three passes.

## E2-10 — the Balanced rows against the incumbents (`small`)

| class | `lpk/balanced` | `lpk/balanced-no-deflate` | best incumbent (same report) | `lpk/balanced` first run |
|---|---|---|---|---|
| office-pdf | 45.78% | 49.12% | `zpaqfranz/m5` 46.25% | 45.63% |
| photo-raw-png | 50.76% | 54.25% | `zpaqfranz/m5` 47.86% | 50.74% |
| software-installed | 27.72% | 27.70% | `zpaqfranz/m5` 25.08% | 27.72% |

`balanced` (the Deflate peel on, the default) against `balanced-no-deflate` on every class of `small`, archive size in percent of the class's bytes, compress seconds; the first run's `balanced` for comparison:

| class | `balanced` | `balanced-no-deflate` | difference (points) | compress s (on / off) | first run `balanced` |
|---|---|---|---|---|---|
| archives-nested | 73.88% | 76.56% | -2.68 | 11.2 / 4.2 | 92.96% |
| audio | 76.75% | 76.75% | +0.00 | 82.2 / 79.3 | 76.75% |
| backup-versions | 7.59% | 9.10% | -1.50 | 4.6 / 4.1 | 9.31% |
| encrypted-random | 100.05% | 100.05% | +0.00 | 0.2 / 0.1 | 100.05% |
| game-assets | 32.82% | 33.41% | -0.60 | 22.5 / 10.4 | 33.24% |
| logs-text | 6.55% | 6.55% | +0.00 | 65.5 / 65.0 | 6.55% |
| model-weights | 91.88% | 91.88% | +0.00 | 5.0 / 5.0 | 91.88% |
| office-pdf | 45.78% | 49.12% | -3.34 | 155.5 / 137.9 | 45.63% |
| photo-jpeg | 76.79% | 76.79% | +0.00 | 7.5 / 7.5 | 76.79% |
| photo-jpeg-edited | 75.49% | 75.49% | +0.00 | 0.9 / 0.9 | 75.49% |
| photo-raw-png | 50.76% | 54.25% | -3.49 | 26.5 / 22.9 | 50.74% |
| small-files | 7.16% | 7.16% | +0.00 | 30.1 / 30.2 | 7.16% |
| software-installed | 27.72% | 27.70% | +0.02 | 53.0 / 51.3 | 27.72% |
| source-git | 51.57% | 57.63% | -6.06 | 6.7 / 4.0 | 51.57% |
| text-prose | 25.71% | 25.71% | +0.00 | 111.6 / 111.7 | 25.71% |
| video | 100.05% | 100.05% | +0.00 | 0.1 / 0.1 | 100.05% |
| vm-image | 46.66% | 46.66% | +0.00 | 36.9 / 36.5 | 46.66% |

## E2-21 — the full-corpus round trip (`full`, `lpk` `fast` and `balanced`)

`bench/results/2026-10-17-megatron-14`: 36 rows measured, 0 failed; every measured row's extraction verified against the manifest (36 of 36).

| class | `fast` | `balanced` | `balanced` compress s | first run `balanced` |
|---|---|---|---|---|
| archives-nested | 98.69% | 67.83% | 7.9 | 67.82% |
| audio | 90.02% | 76.75% | 83.0 | 76.75% |
| backup-versions | 10.60% | 7.59% | 4.6 | 9.31% |
| backup-versions-large | 10.47% | 8.12% | 323.2 | 8.18% |
| encrypted-random | 100.05% | 100.05% | 1.3 | 100.05% |
| game-assets | 36.04% | 32.82% | 22.4 | 33.24% |
| logs-text | 8.11% | 4.75% | 1934.2 | 4.75% |
| model-weights | 79.15% | 71.78% | 248.5 | 71.78% |
| office-pdf | 70.60% | 63.07% | 1111.6 | failed |
| photo-jpeg | 78.07% | 78.07% | 188.9 | 78.07% |
| photo-jpeg-edited | 75.15% | 75.15% | 16.5 | 75.15% |
| photo-raw-png | 56.05% | 43.18% | 161.3 | 43.17% |
| small-files | 14.01% | 10.08% | 22.3 | 10.08% |
| software-installed | 23.62% | 17.14% | 142.9 | 17.14% |
| source-git | 83.33% | 73.63% | 26.3 | 73.63% |
| text-prose | 32.84% | 23.80% | 210.8 | 23.80% |
| video | 100.05% | 100.05% | 1.1 | 100.05% |
| vm-image | 25.78% | 20.60% | 693.0 | 20.60% |

The incumbents' `full` rows of Phase 0 (`bench/results/2026-10-03-megatron-4`) measured an earlier `full` manifest (the versioned class was added on 2026-10-04), so the report refuses to pool them with this run ("pooled directories must measure the same corpus"). The rows above therefore stand without incumbents beside them; a baseline of every tool on the current `full` (E2-22's input) puts them side by side.

## Memory (E2-20) with the pool

| setting | ceiling MiB | largest peak working set, `small` (class) | largest, `full` (class) |
|---|---|---|---|
| `lpk/store` | 1024 | 566.6 (photo-jpeg) | — |
| `lpk/fast` | 2048 | 470.7 (photo-jpeg) | 894.1 (logs-text) |
| `lpk/fast-encrypted` | 2048 | 471.2 (photo-jpeg) | — |
| `lpk/balanced` | 4096 | 1296.9 (office-pdf) | 1802.8 (office-pdf) |
| `lpk/balanced-extension` | 4096 | 1380.7 (office-pdf) | — |
| `lpk/balanced-no-deflate` | 4096 | 1733.1 (office-pdf) | — |

## A. `small`, every tool — `bench/results/2026-10-17-megatron-12`

# LitePack Phase 0 report, baseline run of 2026-10-17

Generated by `lpk-bench report` from result files under bench/results. Every number in sections 2 to 6 is followed by the index of its source in brackets (list in section 7); a number derived from several files carries all of them. Estimates are labelled as estimates and never share a column with measurements. The verdict is recorded by a person in D-08, not by this command.

## 1. Header

- corpus: profile `small`, manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`, 1549 [4,26,48] files, 512632709 [4,26,48] bytes, 3 classes ([4])
- host `megatron`: Intel(R) Core(TM) Ultra 9 285K, 24 logical cores, 102298484736 bytes RAM, windows Windows 11 Pro [1]
- baseline build: lpk-bench 0.0.1 at commit `d4b283867fb8`, rustc 1.99.0 (b940084d7 2026-09-28), run dated 2026-10-17 [1]
- tools: tar (store, control) bsdtar 3.8.8, 7-Zip 26.03, WinRAR (rar) 7.00, Zstandard 1.5.7, XZ Utils 5.8.4, zpaqfranz 65.4, LitePack 0.0.1+d4b283867fb8, t-saur (skipped), WinZip command line (skipped), PowerArchiver command line (skipped) [2]
- antivirus at the start of the baseline run: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000) (Windows Defender real-time: off) [1]; at the end: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000) [3]; changed during the run: no
- catalogue BLAKE3 `a8a47192aa452dd8f269bf4ba057e989badb1d05b8ab9e2e0751cef66ab8296a`; run settings: 1 repeats requested, long-run limit 120 s, settle pause 500 ms per 1000 files, 24 threads [3]

## 2. Baseline, per class

Archive size is a percentage of the class's bytes. MB/s = class bytes / median wall seconds (MB = 10^6 bytes). Peak memory is the larger of the compress and extract medians (peak working set on Windows, maximum RSS on Linux), in MiB. A tool/setting marked `*` rests on one measurement.

### office-pdf: 166 [4] files, 267862151 bytes [4]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store * | 100.0% [4] | 1109.0 [4] | 1609.7 [4] | 6.0 [4] | 1 [4] |
| 7z/mx5 * | 49.2% [5] | 23.2 [5] | 248.0 [5] | 1040.3 [5] | 1 [5] |
| 7z/ultra * | 48.9% [6] | 11.1 [6] | 137.0 [6] | 2568.7 [6] | 1 [6] |
| rar/m3 * | 52.2% [7] | 146.7 [7] | 404.3 [7] | 324.9 [7] | 1 [7] |
| rar/best * | 52.1% [8] | 103.4 [8] | 367.1 [8] | 595.7 [8] | 1 [8] |
| rar/best-solid * | 51.3% [9] | 73.6 [9] | 392.0 [9] | 2407.7 [9] | 1 [9] |
| rar/best-rr3 * | 53.7% [10] | 104.2 [10] | 386.1 [10] | 595.7 [10] | 1 [10] |
| zstd/3 * | 54.9% [11] | 758.5 [11] | 921.2 [11] | 303.5 [11] | 1 [11] |
| zstd/19 * | 50.5% [12] | 17.7 [12] | 826.9 [12] | 1040.1 [12] | 1 [12] |
| zstd/ultra22-long27 * | 50.1% [13] | 1.7 [13] | 741.5 [13] | 1096.9 [13] | 1 [13] |
| xz/6 * | 49.4% [14] | 31.9 [14] | 505.1 [14] | 1410.7 [14] | 1 [14] |
| xz/9 * | 49.1% [15] | 4.6 [15] | 198.0 [15] | 1695.4 [15] | 1 [15] |
| zpaqfranz/m1 * | 60.8% [16] | 338.4 [16] | 1097.7 [16] | 869.4 [16] | 1 [16] |
| zpaqfranz/m5 * | 46.2% [17] | 1.6 [17] | 1.6 [17] | 3452.2 [17] | 1 [17] |
| lpk/fast * | 54.9% [18] | 476.8 [18] | 918.0 [18] | 411.2 [18] | 1 [18] |
| lpk/balanced * | 45.8% [19] | 1.7 [19] | 34.9 [19] | 1296.9 [19] | 1 [19] |
| lpk/balanced-extension * | 45.7% [20] | 1.7 [20] | 37.3 [20] | 1380.7 [20] | 1 [20] |
| lpk/balanced-no-deflate * | 49.1% [21] | 1.9 [21] | 96.3 [21] | 1733.1 [21] | 1 [21] |
| lpk/store * | 100.0% [22] | 954.6 [22] | 2080.9 [22] | 501.2 [22] | 1 [22] |
| lpk/fast-encrypted * | 54.9% [23] | 373.9 [23] | 732.0 [23] | 402.0 [23] | 1 [23] |
| tsaur/default | skipped: not installed [24] | - | - | - | - |
| tsaur/max | skipped: not installed [25] | - | - | - | - |

### photo-raw-png: 950 [26] files, 117924515 bytes [26]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store * | 100.6% [26] | 858.6 [26] | 597.4 [26] | 6.0 [26] | 1 [26] |
| 7z/mx5 * | 54.0% [27] | 17.5 [27] | 133.6 [27] | 383.9 [27] | 1 [27] |
| 7z/ultra * | 53.9% [28] | 15.0 [28] | 130.0 [28] | 1155.0 [28] | 1 [28] |
| rar/m3 * | 55.2% [29] | 81.8 [29] | 87.5 [29] | 216.1 [29] | 1 [29] |
| rar/best * | 54.9% [30] | 59.2 [30] | 87.8 [30] | 359.1 [30] | 1 [30] |
| rar/best-solid * | 54.9% [31] | 37.1 [31] | 190.6 [31] | 1383.2 [31] | 1 [31] |
| rar/best-rr3 * | 56.5% [32] | 58.4 [32] | 89.9 [32] | 358.8 [32] | 1 [32] |
| zstd/3 * | 62.1% [33] | 572.7 [33] | 465.1 [33] | 182.2 [33] | 1 [33] |
| zstd/19 * | 56.0% [34] | 18.6 [34] | 433.5 [34] | 507.8 [34] | 1 [34] |
| zstd/ultra22-long27 * | 55.8% [35] | 4.2 [35] | 381.4 [35] | 889.1 [35] | 1 [35] |
| xz/6 * | 54.1% [36] | 16.0 [36] | 280.4 [36] | 644.1 [36] | 1 [36] |
| xz/9 * | 53.9% [37] | 3.8 [37] | 134.1 [37] | 851.5 [37] | 1 [37] |
| zpaqfranz/m1 * | 61.9% [38] | 228.8 [38] | 380.7 [38] | 470.4 [38] | 1 [38] |
| zpaqfranz/m5 * | 47.9% [39] | 1.0 [39] | 1.0 [39] | 1701.4 [39] | 1 [39] |
| lpk/fast * | 62.3% [40] | 300.1 [40] | 442.1 [40] | 207.7 [40] | 1 [40] |
| lpk/balanced * | 50.8% [41] | 4.5 [41] | 38.9 [41] | 995.2 [41] | 1 [41] |
| lpk/balanced-extension * | 50.8% [42] | 2.5 [42] | 38.4 [42] | 868.5 [42] | 1 [42] |
| lpk/balanced-no-deflate * | 54.3% [43] | 5.1 [43] | 82.2 [43] | 798.0 [43] | 1 [43] |
| lpk/store * | 100.1% [44] | 587.6 [44] | 898.0 [44] | 196.8 [44] | 1 [44] |
| lpk/fast-encrypted * | 62.3% [45] | 222.5 [45] | 332.8 [45] | 208.0 [45] | 1 [45] |
| tsaur/default | skipped: not installed [46] | - | - | - | - |
| tsaur/max | skipped: not installed [47] | - | - | - | - |

### software-installed: 433 [48] files, 126846043 bytes [48]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| store/store * | 100.3% [48] | 1028.8 [48] | 434.2 [48] | 6.0 [48] | 1 [48] |
| 7z/mx5 * | 28.3% [49] | 7.6 [49] | 132.7 [49] | 385.5 [49] | 1 [49] |
| 7z/ultra * | 25.6% [50] | 6.9 [50] | 141.8 [50] | 1070.3 [50] | 1 [50] |
| rar/m3 * | 37.7% [51] | 116.4 [51] | 199.5 [51] | 211.6 [51] | 1 [51] |
| rar/best * | 37.6% [52] | 88.7 [52] | 204.0 [52] | 354.7 [52] | 1 [52] |
| rar/best-solid * | 29.3% [53] | 43.0 [53] | 218.0 [53] | 1383.9 [53] | 1 [53] |
| rar/best-rr3 * | 38.8% [54] | 86.4 [54] | 205.9 [54] | 355.5 [54] | 1 [54] |
| zstd/3 * | 43.1% [55] | 649.2 [55] | 459.2 [55] | 180.1 [55] | 1 [55] |
| zstd/19 * | 32.6% [56] | 12.2 [56] | 493.7 [56] | 492.0 [56] | 1 [56] |
| zstd/ultra22-long27 * | 29.7% [57] | 2.7 [57] | 438.1 [57] | 871.7 [57] | 1 [57] |
| xz/6 * | 31.2% [58] | 17.7 [58] | 330.0 [58] | 657.8 [58] | 1 [58] |
| xz/9 * | 27.8% [59] | 3.0 [59] | 137.5 [59] | 832.7 [59] | 1 [59] |
| zpaqfranz/m1 * | 42.7% [60] | 207.8 [60] | 486.6 [60] | 614.7 [60] | 1 [60] |
| zpaqfranz/m5 * | 25.1% [61] | 1.4 [61] | 1.3 [61] | 1717.0 [61] | 1 [61] |
| lpk/fast * | 38.5% [62] | 317.5 [62] | 455.1 [62] | 210.0 [62] | 1 [62] |
| lpk/balanced * | 27.7% [63] | 2.4 [63] | 82.5 [63] | 817.1 [63] | 1 [63] |
| lpk/balanced-extension * | 28.4% [64] | 2.5 [64] | 80.9 [64] | 841.0 [64] | 1 [64] |
| lpk/balanced-no-deflate * | 27.7% [65] | 2.5 [65] | 67.2 [65] | 899.9 [65] | 1 [65] |
| lpk/store * | 100.0% [66] | 635.9 [66] | 1133.7 [66] | 237.8 [66] | 1 [66] |
| lpk/fast-encrypted * | 38.5% [67] | 236.0 [67] | 345.4 [67] | 248.9 [67] | 1 [67] |
| tsaur/default | skipped: not installed [68] | - | - | - | - |
| tsaur/max | skipped: not installed [69] | - | - | - | - |

## 3. Baseline, blended

The disk mixes come from `bench/report-mixes.toml` [70], printed here in full:

```toml
# Disk mixes for `lpk-bench report` (docs/PLAN.md P0-5). Committed; the report prints this file's
# contents as its own source. Each mix lists `class = weight`: the weights are shares of bytes and
# must sum to 100; every class must be a corpus class (docs/CORPUS.md, D-15). A mix is "n/a" in the
# report when the corpus lacks one of its classes.
#
# Blending: a size ratio is the weighted sum of the per-class ratios; a speed is the bytes-weighted
# harmonic mean of the per-class speeds.

[[mix]]
name = "photo-doc"
weights = { photo-jpeg = 45, photo-jpeg-edited = 5, photo-raw-png = 5, office-pdf = 25, text-prose = 5, archives-nested = 5, audio = 5, small-files = 5 }

[[mix]]
name = "developer"
weights = { source-git = 15, backup-versions = 15, software-installed = 20, small-files = 10, logs-text = 15, text-prose = 5, archives-nested = 10, model-weights = 5, vm-image = 5 }

[[mix]]
name = "video-heavy"
weights = { video = 60, photo-jpeg = 15, audio = 10, encrypted-random = 5, office-pdf = 5, vm-image = 5 }
```

Size: the weighted sum of the per-class size ratios. Speed: the bytes-weighted harmonic mean of the per-class speeds. A tool/setting without a measured result for every class of the mix shows `n/a`.

### Mix `photo-doc`

Shares of bytes, in percent [70]: archives-nested 5, audio 5, office-pdf 25, photo-jpeg 45, photo-jpeg-edited 5, photo-raw-png 5, small-files 5, text-prose 5.

n/a: the corpus has no class archives-nested, audio, photo-jpeg, photo-jpeg-edited, small-files, text-prose.

### Mix `developer`

Shares of bytes, in percent [70]: archives-nested 10, backup-versions 15, logs-text 15, model-weights 5, small-files 10, software-installed 20, source-git 15, text-prose 5, vm-image 5.

n/a: the corpus has no class archives-nested, backup-versions, logs-text, model-weights, small-files, source-git, text-prose, vm-image.

### Mix `video-heavy`

Shares of bytes, in percent [70]: audio 10, encrypted-random 5, office-pdf 5, photo-jpeg 15, video 60, vm-image 5.

n/a: the corpus has no class audio, encrypted-random, photo-jpeg, video, vm-image.

## 4. Probes

Each table below is rendered from the probe's JSON by the same function `lpk-bench probe` uses; the figures inside carry the one source named in the line above them.

### Probe `jpeg`

Not available: no `probe-jpeg.json` was given.

### Probe `deflate`

Not available: no `probe-deflate.json` was given.

### Probe `dedup`

Not available: no `probe-dedup.json` was given.

### Probe `text`

Not available: no `probe-text.json` was given.

### Probe `weights`

Not available: no `probe-weights.json` was given.

### Probe `entropy-gate`

Not available: no `probe-entropy-gate.json` was given.

## 5. Estimates

The estimate column is the figure derived from the probes by the rule for the class; it combines component measurements and assumes nothing about the container, deduplication across classes or the Fold stage. Measured and estimated figures are in separate columns. In the per-class table the best measured incumbent is the smallest archive among all measured tools and settings of that class (a `*` marks a row that rests on one measurement). For the disk mixes and for gate G1 the best measured incumbent is the best single tool x setting over the combined bytes, because D-07 names one competitor; the sum of the per-class bests (which no single tool achieves) is printed as a stricter second figure. For a class with no rule the estimate cell reads `= best incumbent, no rule`, for video and encrypted-random `stored, no rule`: no gain is claimed there.

| class | class bytes | best measured incumbent | best incumbent size | best incumbent, % of class | 7-Zip Ultra size | 7-Zip Ultra, % of class | LitePack Balanced, estimate | estimate, % of class |
|---|---|---|---|---|---|---|---|---|
| office-pdf | 267862151 bytes [4] | zpaqfranz/m5 * | 123885118 bytes [4-17] | 46.2% of class [4-17] | 131085117 bytes [6] | 48.9% of class [4,6] | not available | - |
| photo-raw-png | 117924515 bytes [26] | zpaqfranz/m5 * | 56443900 bytes [26-39] | 47.9% of class [26-39] | 63531230 bytes [28] | 53.9% of class [26,28] | not available | - |
| software-installed | 126846043 bytes [48] | zpaqfranz/m5 * | 31806722 bytes [48-61] | 25.1% of class [48-61] | 32524717 bytes [50] | 25.6% of class [48,50] | not available | - |

Rules, one per class group:

- photo-jpeg, photo-jpeg-edited: bytes after Lepton with failed files stored as-is (probe jpeg, `after fallback`).
- office-pdf, archives-nested, software-installed, game-assets, photo-raw-png: probe deflate figure B with xz preset 9: reconstructed streams replaced by plain data, corrections added, whole file compressed.
- backup-versions, backup-versions-large: the smaller of (a) the sum over versions of the new unique chunks compressed with zstd level 19 (version 1 whole) and (b) version 1 at zstd level 19 plus the zstd --patch-from patches of the later versions (probe dedup); the report names which was used. Gate G2 is evaluated on backup-versions-large only (D-43), against the best tool x setting that the catalogue does not flag `dedup`; deduplicating tools are printed as reference, not compared; every version of the class must exceed the largest incumbent dictionary window, 256 MiB (WinRAR -md256m in bench/tools.toml), else G2 is not evaluable.
- text-prose, logs-text, small-files: the smallest verified row of probe text for the class (xz, zstd, bsc or kanzi, whichever ran); the report names the row.
- model-weights: the smaller total of byte planes and rotated planes (probe weights) when the probe parsed at least one file, otherwise not available.
- video, encrypted-random: stored as-is (the class bytes); every other class: the best measured incumbent, no gain claimed.

What each class's estimate used:

- office-pdf: not available: probe deflate is not available.
- photo-raw-png: not available: probe deflate is not available.
- software-installed: not available: probe deflate is not available.

Per disk mix (ratios of the mix's bytes: the weighted sum of the class ratios above):

| mix | best measured incumbent (single tool x setting) | best single tool, % of mix bytes | stricter: per-class bests | 7-Zip Ultra | LitePack Balanced, estimate |
|---|---|---|---|---|---|
| photo-doc | - | n/a | n/a | n/a | n/a |
| developer | - | n/a | n/a | n/a | n/a |
| video-heavy | - | n/a | n/a | n/a | n/a |

- mix photo-doc, best single tool: n/a: not in the corpus: archives-nested, audio, photo-jpeg, photo-jpeg-edited, small-files, text-prose.
- mix photo-doc, per-class bests: n/a: not in the corpus: archives-nested, audio, photo-jpeg, photo-jpeg-edited, small-files, text-prose.
- mix photo-doc, 7-Zip Ultra: n/a: not in the corpus: archives-nested, audio, photo-jpeg, photo-jpeg-edited, small-files, text-prose.
- mix photo-doc, estimate: n/a: not in the corpus: archives-nested, audio, photo-jpeg, photo-jpeg-edited, small-files, text-prose.
- mix developer, best single tool: n/a: not in the corpus: archives-nested, backup-versions, logs-text, model-weights, small-files, source-git, text-prose, vm-image.
- mix developer, per-class bests: n/a: not in the corpus: archives-nested, backup-versions, logs-text, model-weights, small-files, source-git, text-prose, vm-image.
- mix developer, 7-Zip Ultra: n/a: not in the corpus: archives-nested, backup-versions, logs-text, model-weights, small-files, source-git, text-prose, vm-image.
- mix developer, estimate: n/a: not in the corpus: archives-nested, backup-versions, logs-text, model-weights, small-files, source-git, text-prose, vm-image.
- mix video-heavy, best single tool: n/a: not in the corpus: audio, encrypted-random, photo-jpeg, video, vm-image.
- mix video-heavy, per-class bests: n/a: not in the corpus: audio, encrypted-random, photo-jpeg, video, vm-image.
- mix video-heavy, 7-Zip Ultra: n/a: not in the corpus: audio, encrypted-random, photo-jpeg, video, vm-image.
- mix video-heavy, estimate: n/a: not in the corpus: audio, encrypted-random, photo-jpeg, video, vm-image.

## 6. Gates (D-07)

| gate | numbers | result |
|---|---|---|
| G1 photo/document size: on photo-jpeg, photo-jpeg-edited and office-pdf together, estimate at most 90% of the best measured incumbent (the best single tool x setting over the combined bytes; D-07's fallback wording: WinZip and PowerArchiver were not measured) and at most 85% of 7-Zip Ultra; the sum of the per-class bests (stricter) and the three classes alone are shown for information |  | not evaluable: class photo-jpeg is not in the corpus |
| G2 versioned backup (D-43): on backup-versions-large, estimate at most 50% of the best measured tool x setting among the tools that do not deduplicate across files |  | not evaluable: class backup-versions-large is not in the baseline directory (G2 is not evaluated on backup-versions any more) |
| G3 video store speed: the store tool's compress MB/s on video (a real program reading and writing the files) is at least 80% of the video class's raw read rate |  | not evaluable: no measured store result on class video |
| G4 fast-tier extraction (proxy): the baseline zstd/3 extraction MB/s is at least the 7z/mx5 extraction MB/s, blended over each disk mix |  | not evaluable: mix photo-doc: no result for class `archives-nested` |

**NO-GO proposal: not evaluable (the data is incomplete, not a failure): G1, G2, G3, G4**

- G1 inputs, skipped: tsaur/default (not installed); tsaur/max (not installed)
- G4: not evaluable on mix photo-doc: no result for class `archives-nested`
- G4: not evaluable on mix developer: no result for class `archives-nested`
- G4: not evaluable on mix video-heavy: no result for class `audio`

PASS needs the stated numbers; a gate whose data is missing is `not evaluable` and is never a PASS. A `*` after a tool name marks a row that rests on one measurement. G1 uses the fallback wording of D-07 with the best single tool x setting over the combined bytes; the stricter sum of per-class bests is printed beneath it. G3 uses the baseline `store` tool as the measured proxy for storing a file. The baseline's reads are cache-warm (inputs are re-read before each tool), its writes are flushed outside the timed interval and its time includes process start; the probe's raw read is uncached and read-only, so the two rates are not measured the same way. G4 uses zstd level 3 as the proxy for the Fast tier. The verdict is recorded by a person in D-08.

## 7. Sources

1. `bench/results/2026-10-17-megatron-12/host.json`
2. `bench/results/2026-10-17-megatron-12/tools.json`
3. `bench/results/2026-10-17-megatron-12/run.json`
4. `bench/results/2026-10-17-megatron-12/store-store-office-pdf.json`
5. `bench/results/2026-10-17-megatron-12/7z-mx5-office-pdf.json`
6. `bench/results/2026-10-17-megatron-12/7z-ultra-office-pdf.json`
7. `bench/results/2026-10-17-megatron-12/rar-m3-office-pdf.json`
8. `bench/results/2026-10-17-megatron-12/rar-best-office-pdf.json`
9. `bench/results/2026-10-17-megatron-12/rar-best-solid-office-pdf.json`
10. `bench/results/2026-10-17-megatron-12/rar-best-rr3-office-pdf.json`
11. `bench/results/2026-10-17-megatron-12/zstd-3-office-pdf.json`
12. `bench/results/2026-10-17-megatron-12/zstd-19-office-pdf.json`
13. `bench/results/2026-10-17-megatron-12/zstd-ultra22-long27-office-pdf.json`
14. `bench/results/2026-10-17-megatron-12/xz-6-office-pdf.json`
15. `bench/results/2026-10-17-megatron-12/xz-9-office-pdf.json`
16. `bench/results/2026-10-17-megatron-12/zpaqfranz-m1-office-pdf.json`
17. `bench/results/2026-10-17-megatron-12/zpaqfranz-m5-office-pdf.json`
18. `bench/results/2026-10-17-megatron-12/lpk-fast-office-pdf.json`
19. `bench/results/2026-10-17-megatron-12/lpk-balanced-office-pdf.json`
20. `bench/results/2026-10-17-megatron-12/lpk-balanced-extension-office-pdf.json`
21. `bench/results/2026-10-17-megatron-12/lpk-balanced-no-deflate-office-pdf.json`
22. `bench/results/2026-10-17-megatron-12/lpk-store-office-pdf.json`
23. `bench/results/2026-10-17-megatron-12/lpk-fast-encrypted-office-pdf.json`
24. `bench/results/2026-10-17-megatron-12/tsaur-default-office-pdf.json`
25. `bench/results/2026-10-17-megatron-12/tsaur-max-office-pdf.json`
26. `bench/results/2026-10-17-megatron-12/store-store-photo-raw-png.json`
27. `bench/results/2026-10-17-megatron-12/7z-mx5-photo-raw-png.json`
28. `bench/results/2026-10-17-megatron-12/7z-ultra-photo-raw-png.json`
29. `bench/results/2026-10-17-megatron-12/rar-m3-photo-raw-png.json`
30. `bench/results/2026-10-17-megatron-12/rar-best-photo-raw-png.json`
31. `bench/results/2026-10-17-megatron-12/rar-best-solid-photo-raw-png.json`
32. `bench/results/2026-10-17-megatron-12/rar-best-rr3-photo-raw-png.json`
33. `bench/results/2026-10-17-megatron-12/zstd-3-photo-raw-png.json`
34. `bench/results/2026-10-17-megatron-12/zstd-19-photo-raw-png.json`
35. `bench/results/2026-10-17-megatron-12/zstd-ultra22-long27-photo-raw-png.json`
36. `bench/results/2026-10-17-megatron-12/xz-6-photo-raw-png.json`
37. `bench/results/2026-10-17-megatron-12/xz-9-photo-raw-png.json`
38. `bench/results/2026-10-17-megatron-12/zpaqfranz-m1-photo-raw-png.json`
39. `bench/results/2026-10-17-megatron-12/zpaqfranz-m5-photo-raw-png.json`
40. `bench/results/2026-10-17-megatron-12/lpk-fast-photo-raw-png.json`
41. `bench/results/2026-10-17-megatron-12/lpk-balanced-photo-raw-png.json`
42. `bench/results/2026-10-17-megatron-12/lpk-balanced-extension-photo-raw-png.json`
43. `bench/results/2026-10-17-megatron-12/lpk-balanced-no-deflate-photo-raw-png.json`
44. `bench/results/2026-10-17-megatron-12/lpk-store-photo-raw-png.json`
45. `bench/results/2026-10-17-megatron-12/lpk-fast-encrypted-photo-raw-png.json`
46. `bench/results/2026-10-17-megatron-12/tsaur-default-photo-raw-png.json`
47. `bench/results/2026-10-17-megatron-12/tsaur-max-photo-raw-png.json`
48. `bench/results/2026-10-17-megatron-12/store-store-software-installed.json`
49. `bench/results/2026-10-17-megatron-12/7z-mx5-software-installed.json`
50. `bench/results/2026-10-17-megatron-12/7z-ultra-software-installed.json`
51. `bench/results/2026-10-17-megatron-12/rar-m3-software-installed.json`
52. `bench/results/2026-10-17-megatron-12/rar-best-software-installed.json`
53. `bench/results/2026-10-17-megatron-12/rar-best-solid-software-installed.json`
54. `bench/results/2026-10-17-megatron-12/rar-best-rr3-software-installed.json`
55. `bench/results/2026-10-17-megatron-12/zstd-3-software-installed.json`
56. `bench/results/2026-10-17-megatron-12/zstd-19-software-installed.json`
57. `bench/results/2026-10-17-megatron-12/zstd-ultra22-long27-software-installed.json`
58. `bench/results/2026-10-17-megatron-12/xz-6-software-installed.json`
59. `bench/results/2026-10-17-megatron-12/xz-9-software-installed.json`
60. `bench/results/2026-10-17-megatron-12/zpaqfranz-m1-software-installed.json`
61. `bench/results/2026-10-17-megatron-12/zpaqfranz-m5-software-installed.json`
62. `bench/results/2026-10-17-megatron-12/lpk-fast-software-installed.json`
63. `bench/results/2026-10-17-megatron-12/lpk-balanced-software-installed.json`
64. `bench/results/2026-10-17-megatron-12/lpk-balanced-extension-software-installed.json`
65. `bench/results/2026-10-17-megatron-12/lpk-balanced-no-deflate-software-installed.json`
66. `bench/results/2026-10-17-megatron-12/lpk-store-software-installed.json`
67. `bench/results/2026-10-17-megatron-12/lpk-fast-encrypted-software-installed.json`
68. `bench/results/2026-10-17-megatron-12/tsaur-default-software-installed.json`
69. `bench/results/2026-10-17-megatron-12/tsaur-max-software-installed.json`
70. `bench/report-mixes.toml`

## 8. Caveats

- tools skipped: t-saur (not installed), WinZip command line (manual (see docs/BASELINES.md)), PowerArchiver command line (manual (see docs/BASELINES.md))
- combinations that failed: none
- rows resting on one measurement (D-20 item 3): store/store on office-pdf (one repeat recorded); 7z/mx5 on office-pdf (one repeat recorded); 7z/ultra on office-pdf (one repeat recorded); rar/m3 on office-pdf (one repeat recorded); rar/best on office-pdf (one repeat recorded); rar/best-solid on office-pdf (one repeat recorded); rar/best-rr3 on office-pdf (one repeat recorded); zstd/3 on office-pdf (one repeat recorded); zstd/19 on office-pdf (one repeat recorded); zstd/ultra22-long27 on office-pdf (one repeat recorded); xz/6 on office-pdf (one repeat recorded); xz/9 on office-pdf (one repeat recorded); zpaqfranz/m1 on office-pdf (one repeat recorded); zpaqfranz/m5 on office-pdf (one repeat recorded); lpk/fast on office-pdf (one repeat recorded); lpk/balanced on office-pdf (one repeat recorded); lpk/balanced-extension on office-pdf (one repeat recorded); lpk/balanced-no-deflate on office-pdf (one repeat recorded); lpk/store on office-pdf (one repeat recorded); lpk/fast-encrypted on office-pdf (one repeat recorded); store/store on photo-raw-png (one repeat recorded); 7z/mx5 on photo-raw-png (one repeat recorded); 7z/ultra on photo-raw-png (one repeat recorded); rar/m3 on photo-raw-png (one repeat recorded); rar/best on photo-raw-png (one repeat recorded); rar/best-solid on photo-raw-png (one repeat recorded); rar/best-rr3 on photo-raw-png (one repeat recorded); zstd/3 on photo-raw-png (one repeat recorded); zstd/19 on photo-raw-png (one repeat recorded); zstd/ultra22-long27 on photo-raw-png (one repeat recorded); xz/6 on photo-raw-png (one repeat recorded); xz/9 on photo-raw-png (one repeat recorded); zpaqfranz/m1 on photo-raw-png (one repeat recorded); zpaqfranz/m5 on photo-raw-png (one repeat recorded); lpk/fast on photo-raw-png (one repeat recorded); lpk/balanced on photo-raw-png (one repeat recorded); lpk/balanced-extension on photo-raw-png (one repeat recorded); lpk/balanced-no-deflate on photo-raw-png (one repeat recorded); lpk/store on photo-raw-png (one repeat recorded); lpk/fast-encrypted on photo-raw-png (one repeat recorded); store/store on software-installed (one repeat recorded); 7z/mx5 on software-installed (one repeat recorded); 7z/ultra on software-installed (one repeat recorded); rar/m3 on software-installed (one repeat recorded); rar/best on software-installed (one repeat recorded); rar/best-solid on software-installed (one repeat recorded); rar/best-rr3 on software-installed (one repeat recorded); zstd/3 on software-installed (one repeat recorded); zstd/19 on software-installed (one repeat recorded); zstd/ultra22-long27 on software-installed (one repeat recorded); xz/6 on software-installed (one repeat recorded); xz/9 on software-installed (one repeat recorded); zpaqfranz/m1 on software-installed (one repeat recorded); zpaqfranz/m5 on software-installed (one repeat recorded); lpk/fast on software-installed (one repeat recorded); lpk/balanced on software-installed (one repeat recorded); lpk/balanced-extension on software-installed (one repeat recorded); lpk/balanced-no-deflate on software-installed (one repeat recorded); lpk/store on software-installed (one repeat recorded); lpk/fast-encrypted on software-installed (one repeat recorded)
- probes not available: jpeg, deflate, dedup, text, weights, entropy-gate
- classes without an estimate rule (the best measured incumbent is used, no gain claimed): none
- antivirus: the scanner state did not change during the run; scanner state at the start of the baseline run: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000); at the end: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000)
- unclean inputs (dirty, unknown or unoptimised builds, no settle pause, directories outside bench/results): none
- the baseline's build profile is not recorded in its host file, so whether it was a release build cannot be checked from the result files
- the `small-files` text estimate compresses a tar stream of the class, so tar header bytes are included in it; the baseline sizes are of each tool's own archive
- probe text timing: `in-process` rows time a library call on a stream in memory, `process-wall` rows time a whole external program including start-up and file I/O; the two bases are not comparable, and the estimates here use sizes only
- estimates are derived from component measurements on one corpus; they are not archives that were written and read back

## B. `full`, `lpk` — `bench/results/2026-10-17-megatron-14`

# LitePack Phase 0 report, baseline run of 2026-10-17

Generated by `lpk-bench report` from result files under bench/results. Every number in sections 2 to 6 is followed by the index of its source in brackets (list in section 7); a number derived from several files carries all of them. Estimates are labelled as estimates and never share a column with measurements. The verdict is recorded by a person in D-08, not by this command.

## 1. Header

- corpus: profile `full`, manifest BLAKE3 `1d24f5c824fad0c85fc3b9231643b2bd80a75437c6b3d38e86ef1f6450a05f43`, 171247 [4,6,8,10,12,14,16,18,20,22,24,26,28,30,32,34,36,38] files, 20787446482 [4,6,8,10,12,14,16,18,20,22,24,26,28,30,32,34,36,38] bytes, 18 classes ([4])
- host `megatron`: Intel(R) Core(TM) Ultra 9 285K, 24 logical cores, 102298484736 bytes RAM, windows Windows 11 Pro [1]
- baseline build: lpk-bench 0.0.1 at commit `d4b283867fb8`, rustc 1.99.0 (b940084d7 2026-09-28), run dated 2026-10-17 [1]
- tools: LitePack 0.0.1+d4b283867fb8 [2]
- antivirus at the start of the baseline run: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000) (Windows Defender real-time: off) [1]; at the end: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000) [3]; changed during the run: no
- catalogue BLAKE3 `b8f750709630e7d7bb8b5a8f9ae9236fdfd0cd6e2363a44bc8f51003f39960ae`; run settings: 1 repeats requested, long-run limit 120 s, settle pause 500 ms per 1000 files, 24 threads [3]

## 2. Baseline, per class

Archive size is a percentage of the class's bytes. MB/s = class bytes / median wall seconds (MB = 10^6 bytes). Peak memory is the larger of the compress and extract medians (peak working set on Windows, maximum RSS on Linux), in MiB. A tool/setting marked `*` rests on one measurement.

### archives-nested: 14 [4] files, 34975551 bytes [4]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 98.7% [4] | 483.3 [4] | 782.9 [4] | 71.0 [4] | 1 [4] |
| lpk/balanced * | 67.8% [5] | 4.5 [5] | 92.2 [5] | 506.6 [5] | 1 [5] |

### audio: 21 [6] files, 157896311 bytes [6]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 90.0% [6] | 331.8 [6] | 505.3 [6] | 313.0 [6] | 1 [6] |
| lpk/balanced * | 76.7% [7] | 1.9 [7] | 25.5 [7] | 948.9 [7] | 1 [7] |

### backup-versions: 1974 [8] files, 25541629 bytes [8]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 10.6% [8] | 122.0 [8] | 219.8 [8] | 24.4 [8] | 1 [8] |
| lpk/balanced * | 7.6% [9] | 5.6 [9] | 91.1 [9] | 596.6 [9] | 1 [9] |

### backup-versions-large: 127153 [10] files, 1663755751 bytes [10]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 10.5% [10] | 135.3 [10] | 233.3 [10] | 454.4 [10] | 1 [10] |
| lpk/balanced * | 8.1% [11] | 5.1 [11] | 193.3 [11] | 1111.5 [11] | 1 [11] |

### encrypted-random: 2 [12] files, 419430400 bytes [12]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 100.1% [12] | 821.4 [12] | 2129.2 [12] | 620.0 [12] | 1 [12] |
| lpk/balanced * | 100.1% [13] | 319.8 [13] | 798.5 [13] | 914.3 [13] | 1 [13] |

### game-assets: 4660 [14] files, 52470892 bytes [14]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 36.0% [14] | 126.0 [14] | 167.6 [14] | 86.8 [14] | 1 [14] |
| lpk/balanced * | 32.8% [15] | 2.3 [15] | 69.3 [15] | 570.2 [15] | 1 [15] |

### logs-text: 11 [16] files, 2686418017 bytes [16]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 8.1% [16] | 1048.9 [16] | 2448.7 [16] | 894.1 [16] | 1 [16] |
| lpk/balanced * | 4.7% [17] | 1.4 [17] | 758.8 [17] | 1764.0 [17] | 1 [17] |

### model-weights: 2 [18] files, 286816945 bytes [18]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 79.2% [18] | 645.0 [18] | 730.2 [18] | 736.6 [18] | 1 [18] |
| lpk/balanced * | 71.8% [19] | 1.2 [19] | 28.2 [19] | 1390.8 [19] | 1 [19] |

### office-pdf: 2334 [20] files, 2114690575 bytes [20]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 70.6% [20] | 826.2 [20] | 1985.4 [20] | 712.8 [20] | 1 [20] |
| lpk/balanced * | 63.1% [21] | 1.9 [21] | 22.6 [21] | 1802.8 [21] | 1 [21] |

### photo-jpeg: 3055 [22] files, 8258908517 bytes [22]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 78.1% [22] | 39.4 [22] | 139.4 [22] | 868.2 [22] | 1 [22] |
| lpk/balanced * | 78.1% [23] | 43.7 [23] | 139.8 [23] | 885.5 [23] | 1 [23] |

### photo-jpeg-edited: 600 [24] files, 550674750 bytes [24]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 75.1% [24] | 31.4 [24] | 192.0 [24] | 356.5 [24] | 1 [24] |
| lpk/balanced * | 75.1% [25] | 33.4 [25] | 192.4 [25] | 352.0 [25] | 1 [25] |

### photo-raw-png: 1016 [26] files, 537693574 bytes [26]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 56.0% [26] | 628.4 [26] | 1412.8 [26] | 762.1 [26] | 1 [26] |
| lpk/balanced * | 43.2% [27] | 3.3 [27] | 28.4 [27] | 1624.3 [27] | 1 [27] |

### small-files: 20000 [28] files, 50016512 bytes [28]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 14.0% [28] | 30.6 [28] | 41.9 [28] | 110.3 [28] | 1 [28] |
| lpk/balanced * | 10.1% [29] | 2.2 [29] | 38.1 [29] | 535.4 [29] | 1 [29] |

### software-installed: 9671 [30] files, 437043701 bytes [30]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 23.6% [30] | 304.3 [30] | 464.3 [30] | 471.6 [30] | 1 [30] |
| lpk/balanced * | 17.1% [31] | 3.1 [31] | 104.9 [31] | 990.1 [31] | 1 [31] |

### source-git: 663 [32] files, 35365500 bytes [32]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 83.3% [32] | 291.4 [32] | 582.8 [32] | 72.8 [32] | 1 [32] |
| lpk/balanced * | 73.6% [33] | 1.3 [33] | 20.6 [33] | 349.3 [33] | 1 [33] |

### text-prose: 63 [34] files, 360144217 bytes [34]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 32.8% [34] | 601.1 [34] | 1147.7 [34] | 604.4 [34] | 1 [34] |
| lpk/balanced * | 23.8% [35] | 1.7 [35] | 117.8 [35] | 1199.8 [35] | 1 [35] |

### video: 4 [36] files, 959731214 bytes [36]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 100.0% [36] | 1046.6 [36] | 2498.9 [36] | 633.9 [36] | 1 [36] |
| lpk/balanced * | 100.0% [37] | 846.1 [37] | 790.0 [37] | 1001.1 [37] | 1 [37] |

### vm-image: 4 [38] files, 2155872426 bytes [38]

| tool/setting | archive size (% of class bytes) | compress MB/s | extract MB/s | peak memory (MiB) | repeats used |
|---|---|---|---|---|---|
| lpk/fast * | 25.8% [38] | 1175.9 [38] | 967.5 [38] | 662.7 [38] | 1 [38] |
| lpk/balanced * | 20.6% [39] | 3.1 [39] | 189.0 [39] | 1532.4 [39] | 1 [39] |

## 3. Baseline, blended

The disk mixes come from `bench/report-mixes.toml` [40], printed here in full:

```toml
# Disk mixes for `lpk-bench report` (docs/PLAN.md P0-5). Committed; the report prints this file's
# contents as its own source. Each mix lists `class = weight`: the weights are shares of bytes and
# must sum to 100; every class must be a corpus class (docs/CORPUS.md, D-15). A mix is "n/a" in the
# report when the corpus lacks one of its classes.
#
# Blending: a size ratio is the weighted sum of the per-class ratios; a speed is the bytes-weighted
# harmonic mean of the per-class speeds.

[[mix]]
name = "photo-doc"
weights = { photo-jpeg = 45, photo-jpeg-edited = 5, photo-raw-png = 5, office-pdf = 25, text-prose = 5, archives-nested = 5, audio = 5, small-files = 5 }

[[mix]]
name = "developer"
weights = { source-git = 15, backup-versions = 15, software-installed = 20, small-files = 10, logs-text = 15, text-prose = 5, archives-nested = 10, model-weights = 5, vm-image = 5 }

[[mix]]
name = "video-heavy"
weights = { video = 60, photo-jpeg = 15, audio = 10, encrypted-random = 5, office-pdf = 5, vm-image = 5 }
```

Size: the weighted sum of the per-class size ratios. Speed: the bytes-weighted harmonic mean of the per-class speeds. A tool/setting without a measured result for every class of the mix shows `n/a`.

### Mix `photo-doc`

Shares of bytes, in percent [40]: archives-nested 5, audio 5, office-pdf 25, photo-jpeg 45, photo-jpeg-edited 5, photo-raw-png 5, small-files 5, text-prose 5.

| tool/setting | blended size (% of mix bytes) | blended compress MB/s | blended extract MB/s |
|---|---|---|---|
| lpk/fast * | 71.1% [4,6,20,22,24,26,28,34,40] | 65.1 [4,6,20,22,24,26,28,34,40] | 198.0 [4,6,20,22,24,26,28,34,40] |
| lpk/balanced * | 65.7% [5,7,21,23,25,27,29,35,40] | 4.0 [5,7,21,23,25,27,29,35,40] | 48.7 [5,7,21,23,25,27,29,35,40] |

### Mix `developer`

Shares of bytes, in percent [40]: archives-nested 10, backup-versions 15, logs-text 15, model-weights 5, small-files 10, software-installed 20, source-git 15, text-prose 5, vm-image 5.

| tool/setting | blended size (% of mix bytes) | blended compress MB/s | blended extract MB/s |
|---|---|---|---|
| lpk/fast * | 38.2% [4,8,16,18,28,30,32,34,38,40] | 160.6 [4,8,16,18,28,30,32,34,38,40] | 243.3 [4,8,16,18,28,30,32,34,38,40] |
| lpk/balanced * | 29.9% [5,9,17,19,29,31,33,35,39,40] | 2.1 [5,9,17,19,29,31,33,35,39,40] | 58.1 [5,9,17,19,29,31,33,35,39,40] |

### Mix `video-heavy`

Shares of bytes, in percent [40]: audio 10, encrypted-random 5, office-pdf 5, photo-jpeg 15, video 60, vm-image 5.

| tool/setting | blended size (% of mix bytes) | blended compress MB/s | blended extract MB/s |
|---|---|---|---|
| lpk/fast * | 90.6% [6,12,20,22,36,38,40] | 206.3 [6,12,20,22,36,38,40] | 619.3 [6,12,20,22,36,38,40] |
| lpk/balanced * | 88.6% [7,13,21,23,37,39,40] | 10.1 [7,13,21,23,37,39,40] | 120.6 [7,13,21,23,37,39,40] |

## 4. Probes

Each table below is rendered from the probe's JSON by the same function `lpk-bench probe` uses; the figures inside carry the one source named in the line above them.

### Probe `jpeg`

Not available: no `probe-jpeg.json` was given.

### Probe `deflate`

Not available: no `probe-deflate.json` was given.

### Probe `dedup`

Not available: no `probe-dedup.json` was given.

### Probe `text`

Not available: no `probe-text.json` was given.

### Probe `weights`

Not available: no `probe-weights.json` was given.

### Probe `entropy-gate`

Not available: no `probe-entropy-gate.json` was given.

## 5. Estimates

The estimate column is the figure derived from the probes by the rule for the class; it combines component measurements and assumes nothing about the container, deduplication across classes or the Fold stage. Measured and estimated figures are in separate columns. In the per-class table the best measured incumbent is the smallest archive among all measured tools and settings of that class (a `*` marks a row that rests on one measurement). For the disk mixes and for gate G1 the best measured incumbent is the best single tool x setting over the combined bytes, because D-07 names one competitor; the sum of the per-class bests (which no single tool achieves) is printed as a stricter second figure. For a class with no rule the estimate cell reads `= best incumbent, no rule`, for video and encrypted-random `stored, no rule`: no gain is claimed there.

| class | class bytes | best measured incumbent | best incumbent size | best incumbent, % of class | 7-Zip Ultra size | 7-Zip Ultra, % of class | LitePack Balanced, estimate | estimate, % of class |
|---|---|---|---|---|---|---|---|---|
| archives-nested | 34975551 bytes [4] | - | not available | - | not available | - | not available | - |
| audio | 157896311 bytes [6] | - | not available | - | not available | - | not available | - |
| backup-versions | 25541629 bytes [8] | - | not available | - | not available | - | not available | - |
| backup-versions-large | 1663755751 bytes [10] | - | not available | - | not available | - | not available | - |
| encrypted-random | 419430400 bytes [12] | - | not available | - | not available | - | stored, no rule [12] | 100.0% of class [12] |
| game-assets | 52470892 bytes [14] | - | not available | - | not available | - | not available | - |
| logs-text | 2686418017 bytes [16] | - | not available | - | not available | - | not available | - |
| model-weights | 286816945 bytes [18] | - | not available | - | not available | - | not available | - |
| office-pdf | 2114690575 bytes [20] | - | not available | - | not available | - | not available | - |
| photo-jpeg | 8258908517 bytes [22] | - | not available | - | not available | - | not available | - |
| photo-jpeg-edited | 550674750 bytes [24] | - | not available | - | not available | - | not available | - |
| photo-raw-png | 537693574 bytes [26] | - | not available | - | not available | - | not available | - |
| small-files | 50016512 bytes [28] | - | not available | - | not available | - | not available | - |
| software-installed | 437043701 bytes [30] | - | not available | - | not available | - | not available | - |
| source-git | 35365500 bytes [32] | - | not available | - | not available | - | not available | - |
| text-prose | 360144217 bytes [34] | - | not available | - | not available | - | not available | - |
| video | 959731214 bytes [36] | - | not available | - | not available | - | stored, no rule [36] | 100.0% of class [36] |
| vm-image | 2155872426 bytes [38] | - | not available | - | not available | - | not available | - |

Rules, one per class group:

- photo-jpeg, photo-jpeg-edited: bytes after Lepton with failed files stored as-is (probe jpeg, `after fallback`).
- office-pdf, archives-nested, software-installed, game-assets, photo-raw-png: probe deflate figure B with xz preset 9: reconstructed streams replaced by plain data, corrections added, whole file compressed.
- backup-versions, backup-versions-large: the smaller of (a) the sum over versions of the new unique chunks compressed with zstd level 19 (version 1 whole) and (b) version 1 at zstd level 19 plus the zstd --patch-from patches of the later versions (probe dedup); the report names which was used. Gate G2 is evaluated on backup-versions-large only (D-43), against the best tool x setting that the catalogue does not flag `dedup`; deduplicating tools are printed as reference, not compared; every version of the class must exceed the largest incumbent dictionary window, 256 MiB (WinRAR -md256m in bench/tools.toml), else G2 is not evaluable.
- text-prose, logs-text, small-files: the smallest verified row of probe text for the class (xz, zstd, bsc or kanzi, whichever ran); the report names the row.
- model-weights: the smaller total of byte planes and rotated planes (probe weights) when the probe parsed at least one file, otherwise not available.
- video, encrypted-random: stored as-is (the class bytes); every other class: the best measured incumbent, no gain claimed.

What each class's estimate used:

- archives-nested: not available: probe deflate is not available.
- audio: not available: no measured incumbent.
- backup-versions: not available: probe dedup is not available.
- backup-versions-large: not available: probe dedup is not available.
- encrypted-random: stored as-is.
- game-assets: not available: probe deflate is not available.
- logs-text: not available: probe text is not available.
- model-weights: not available: probe weights is not available.
- office-pdf: not available: probe deflate is not available.
- photo-jpeg: not available: probe jpeg is not available.
- photo-jpeg-edited: not available: probe jpeg is not available.
- photo-raw-png: not available: probe deflate is not available.
- small-files: not available: probe text is not available.
- software-installed: not available: probe deflate is not available.
- source-git: not available: no measured incumbent.
- text-prose: not available: probe text is not available.
- video: stored as-is.
- vm-image: not available: no measured incumbent.

Per disk mix (ratios of the mix's bytes: the weighted sum of the class ratios above):

| mix | best measured incumbent (single tool x setting) | best single tool, % of mix bytes | stricter: per-class bests | 7-Zip Ultra | LitePack Balanced, estimate |
|---|---|---|---|---|---|
| photo-doc | lpk/balanced * | 65.7% [4-7,20-29,34,35,40] | n/a | n/a | n/a |
| developer | lpk/balanced * | 29.9% [4,5,8,9,16-19,28-35,38-40] | n/a | n/a | n/a |
| video-heavy | lpk/balanced * | 88.6% [6,7,12,13,20-23,36-40] | n/a | n/a | n/a |

- mix photo-doc, per-class bests: n/a: class `archives-nested`: no measured incumbent.
- mix photo-doc, 7-Zip Ultra: n/a: class `archives-nested`: no 7z/ultra result.
- mix photo-doc, estimate: n/a: class `archives-nested`: probe deflate is not available.
- mix developer, per-class bests: n/a: class `archives-nested`: no measured incumbent.
- mix developer, 7-Zip Ultra: n/a: class `archives-nested`: no 7z/ultra result.
- mix developer, estimate: n/a: class `archives-nested`: probe deflate is not available.
- mix video-heavy, per-class bests: n/a: class `audio`: no measured incumbent.
- mix video-heavy, 7-Zip Ultra: n/a: class `audio`: no 7z/ultra result.
- mix video-heavy, estimate: n/a: class `audio`: no measured incumbent.

## 6. Gates (D-07)

| gate | numbers | result |
|---|---|---|
| G1 photo/document size: on photo-jpeg, photo-jpeg-edited and office-pdf together, estimate at most 90% of the best measured incumbent (the best single tool x setting over the combined bytes; D-07's fallback wording: WinZip and PowerArchiver were not measured) and at most 85% of 7-Zip Ultra; the sum of the per-class bests (stricter) and the three classes alone are shown for information |  | not evaluable: estimate for photo-jpeg: probe jpeg is not available |
| G2 versioned backup (D-43): on backup-versions-large, estimate at most 50% of the best measured tool x setting among the tools that do not deduplicate across files | tools treated as deduplicating: none | not evaluable: estimate: probe dedup is not available |
| G3 video store speed: the store tool's compress MB/s on video (a real program reading and writing the files) is at least 80% of the video class's raw read rate |  | not evaluable: no measured store result on class video |
| G4 fast-tier extraction (proxy): the baseline zstd/3 extraction MB/s is at least the 7z/mx5 extraction MB/s, blended over each disk mix |  | not evaluable: mix photo-doc: no result for class `archives-nested` |

**NO-GO proposal: not evaluable (the data is incomplete, not a failure): G1, G2, G3, G4**

- G4: not evaluable on mix photo-doc: no result for class `archives-nested`
- G4: not evaluable on mix developer: no result for class `archives-nested`
- G4: not evaluable on mix video-heavy: no result for class `audio`

PASS needs the stated numbers; a gate whose data is missing is `not evaluable` and is never a PASS. A `*` after a tool name marks a row that rests on one measurement. G1 uses the fallback wording of D-07 with the best single tool x setting over the combined bytes; the stricter sum of per-class bests is printed beneath it. G3 uses the baseline `store` tool as the measured proxy for storing a file. The baseline's reads are cache-warm (inputs are re-read before each tool), its writes are flushed outside the timed interval and its time includes process start; the probe's raw read is uncached and read-only, so the two rates are not measured the same way. G4 uses zstd level 3 as the proxy for the Fast tier. The verdict is recorded by a person in D-08.

## 7. Sources

1. `bench/results/2026-10-17-megatron-14/host.json`
2. `bench/results/2026-10-17-megatron-14/tools.json`
3. `bench/results/2026-10-17-megatron-14/run.json`
4. `bench/results/2026-10-17-megatron-14/lpk-fast-archives-nested.json`
5. `bench/results/2026-10-17-megatron-14/lpk-balanced-archives-nested.json`
6. `bench/results/2026-10-17-megatron-14/lpk-fast-audio.json`
7. `bench/results/2026-10-17-megatron-14/lpk-balanced-audio.json`
8. `bench/results/2026-10-17-megatron-14/lpk-fast-backup-versions.json`
9. `bench/results/2026-10-17-megatron-14/lpk-balanced-backup-versions.json`
10. `bench/results/2026-10-17-megatron-14/lpk-fast-backup-versions-large.json`
11. `bench/results/2026-10-17-megatron-14/lpk-balanced-backup-versions-large.json`
12. `bench/results/2026-10-17-megatron-14/lpk-fast-encrypted-random.json`
13. `bench/results/2026-10-17-megatron-14/lpk-balanced-encrypted-random.json`
14. `bench/results/2026-10-17-megatron-14/lpk-fast-game-assets.json`
15. `bench/results/2026-10-17-megatron-14/lpk-balanced-game-assets.json`
16. `bench/results/2026-10-17-megatron-14/lpk-fast-logs-text.json`
17. `bench/results/2026-10-17-megatron-14/lpk-balanced-logs-text.json`
18. `bench/results/2026-10-17-megatron-14/lpk-fast-model-weights.json`
19. `bench/results/2026-10-17-megatron-14/lpk-balanced-model-weights.json`
20. `bench/results/2026-10-17-megatron-14/lpk-fast-office-pdf.json`
21. `bench/results/2026-10-17-megatron-14/lpk-balanced-office-pdf.json`
22. `bench/results/2026-10-17-megatron-14/lpk-fast-photo-jpeg.json`
23. `bench/results/2026-10-17-megatron-14/lpk-balanced-photo-jpeg.json`
24. `bench/results/2026-10-17-megatron-14/lpk-fast-photo-jpeg-edited.json`
25. `bench/results/2026-10-17-megatron-14/lpk-balanced-photo-jpeg-edited.json`
26. `bench/results/2026-10-17-megatron-14/lpk-fast-photo-raw-png.json`
27. `bench/results/2026-10-17-megatron-14/lpk-balanced-photo-raw-png.json`
28. `bench/results/2026-10-17-megatron-14/lpk-fast-small-files.json`
29. `bench/results/2026-10-17-megatron-14/lpk-balanced-small-files.json`
30. `bench/results/2026-10-17-megatron-14/lpk-fast-software-installed.json`
31. `bench/results/2026-10-17-megatron-14/lpk-balanced-software-installed.json`
32. `bench/results/2026-10-17-megatron-14/lpk-fast-source-git.json`
33. `bench/results/2026-10-17-megatron-14/lpk-balanced-source-git.json`
34. `bench/results/2026-10-17-megatron-14/lpk-fast-text-prose.json`
35. `bench/results/2026-10-17-megatron-14/lpk-balanced-text-prose.json`
36. `bench/results/2026-10-17-megatron-14/lpk-fast-video.json`
37. `bench/results/2026-10-17-megatron-14/lpk-balanced-video.json`
38. `bench/results/2026-10-17-megatron-14/lpk-fast-vm-image.json`
39. `bench/results/2026-10-17-megatron-14/lpk-balanced-vm-image.json`
40. `bench/report-mixes.toml`

## 8. Caveats

- tools skipped: none
- combinations that failed: none
- rows resting on one measurement (D-20 item 3): lpk/fast on archives-nested (one repeat recorded); lpk/balanced on archives-nested (one repeat recorded); lpk/fast on audio (one repeat recorded); lpk/balanced on audio (one repeat recorded); lpk/fast on backup-versions (one repeat recorded); lpk/balanced on backup-versions (one repeat recorded); lpk/fast on backup-versions-large (one repeat recorded); lpk/balanced on backup-versions-large (one repeat recorded); lpk/fast on encrypted-random (one repeat recorded); lpk/balanced on encrypted-random (one repeat recorded); lpk/fast on game-assets (one repeat recorded); lpk/balanced on game-assets (one repeat recorded); lpk/fast on logs-text (one repeat recorded); lpk/balanced on logs-text (one repeat recorded); lpk/fast on model-weights (one repeat recorded); lpk/balanced on model-weights (one repeat recorded); lpk/fast on office-pdf (one repeat recorded); lpk/balanced on office-pdf (one repeat recorded); lpk/fast on photo-jpeg (one repeat recorded); lpk/balanced on photo-jpeg (one repeat recorded); lpk/fast on photo-jpeg-edited (one repeat recorded); lpk/balanced on photo-jpeg-edited (one repeat recorded); lpk/fast on photo-raw-png (one repeat recorded); lpk/balanced on photo-raw-png (one repeat recorded); lpk/fast on small-files (one repeat recorded); lpk/balanced on small-files (one repeat recorded); lpk/fast on software-installed (one repeat recorded); lpk/balanced on software-installed (one repeat recorded); lpk/fast on source-git (one repeat recorded); lpk/balanced on source-git (one repeat recorded); lpk/fast on text-prose (one repeat recorded); lpk/balanced on text-prose (one repeat recorded); lpk/fast on video (one repeat recorded); lpk/balanced on video (one repeat recorded); lpk/fast on vm-image (one repeat recorded); lpk/balanced on vm-image (one repeat recorded)
- probes not available: jpeg, deflate, dedup, text, weights, entropy-gate
- classes without an estimate rule (the best measured incumbent is used, no gain claimed): audio, source-git, vm-image
- antivirus: the scanner state did not change during the run; scanner state at the start of the baseline run: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000); at the end: Windows Defender (scanner off, state 0x60100); Avast Antivirus (scanner snoozed, state 0x42000)
- unclean inputs (dirty, unknown or unoptimised builds, no settle pause, directories outside bench/results): none
- the baseline's build profile is not recorded in its host file, so whether it was a release build cannot be checked from the result files
- the `small-files` text estimate compresses a tar stream of the class, so tar header bytes are included in it; the baseline sizes are of each tool's own archive
- probe text timing: `in-process` rows time a library call on a stream in memory, `process-wall` rows time a whole external program including start-up and file I/O; the two bases are not comparable, and the estimates here use sizes only
- estimates are derived from component measurements on one corpus; they are not archives that were written and read back
