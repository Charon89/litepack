# Probe `deflate`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `6c04dedc7d5a` (release profile, opt-level 3) on `megatron`, 2026-10-03T10:48:32Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 180.9 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libzstd 1.5.7, preflate-rs 0.7.6

Settings: max chain 4096, plain text limit 67108864 bytes per stream, 536870912 bytes per file; library verification off; zstd level 19, xz preset 9; A = original file compressed whole, B = reconstructed streams replaced by plain data, plus the correction bytes. Streams found by the zlib scan must inflate to at least 1024 bytes. Recognition: the library has no recognised/unrecognised flag: every accepted stream gets an estimate; judge by the overhead buckets Stored ZIP entries are not examined (one level deep), so the gains are a lower bound; per-stream metadata is not counted.

## By container kind

|  | files | streams found | reconstructed | failed | skipped | original bytes | A zstd | B zstd + corr | B/A zstd | A xz | B xz + corr | B/A xz |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| office | 1 | 36 | 36 | 0 | 0 | 474012 | 404656 | 351270 | 86.81% | 407076 | 348100 | 85.51% |
| jar | 1 | 228 | 225 | 3 | 0 | 298435 | 259549 | 156605 | 60.34% | 261408 | 142263 | 54.42% |
| zip | 13 | 1527 | 1428 | 99 | 0 | 38243364 | 37864475 | 34872444 | 92.10% | 37906108 | 34692558 | 91.52% |
| png | 2740 | 2740 | 2715 | 25 | 0 | 44548087 | 44124811 | 40837770 | 92.55% | 44210496 | 39689246 | 89.77% |
| pdf | 74 | 4685 | 4578 | 103 | 4 | 92405895 | 82056924 | 72094730 | 87.86% | 82068244 | 69772063 | 85.02% |
| other | 3394 | 1693 | 1686 | 7 | 0 | 423996781 | 145388839 | 144523525 | 99.40% | 135448296 | 134105914 | 99.01% |
| all | 6223 | 10909 | 10668 | 237 | 4 | 599966574 | 310099254 | 292836344 | 94.43% | 300301628 | 278750144 | 92.82% |

## By class

|  | files | streams found | reconstructed | failed | skipped | original bytes | A zstd | B zstd + corr | B/A zstd | A xz | B xz + corr | B/A xz |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| office-pdf | 166 | 4829 | 4722 | 103 | 4 | 267862151 | 137137090 | 126616196 | 92.33% | 133286512 | 120056114 | 90.07% |
| photo-raw-png | 950 | 940 | 933 | 7 | 0 | 117924515 | 66381133 | 63499471 | 95.66% | 64080644 | 60085638 | 93.77% |
| game-assets | 4660 | 3346 | 3321 | 25 | 0 | 52470892 | 22043022 | 21332584 | 96.78% | 21704772 | 20772109 | 95.70% |
| archives-nested | 14 | 1228 | 1205 | 23 | 0 | 34862973 | 34443756 | 31922589 | 92.68% | 34487276 | 31807505 | 92.23% |
| software-installed | 433 | 566 | 487 | 79 | 0 | 126846043 | 50094253 | 49465504 | 98.74% | 46742424 | 46028778 | 98.47% |
| all | 6223 | 10909 | 10668 | 237 | 4 | 599966574 | 310099254 | 292836344 | 94.43% | 300301628 | 278750144 | 92.82% |

## Failures and skips by cause

| cause | office | jar | apk | zip | png | gzip | pdf | other | total |
|---|---|---|---|---|---|---|---|---|---|
| (skipped for size) | 0 | 0 | 0 | 0 | 0 | 0 | 4 | 0 | 4 |
| InvalidDeflate | 0 | 0 | 0 | 3 | 0 | 0 | 11 | 0 | 14 |
| NoCompressionCandidates | 0 | 0 | 0 | 79 | 0 | 0 | 2 | 0 | 81 |
| PredictionFailure | 0 | 3 | 0 | 17 | 25 | 0 | 90 | 7 | 142 |

Structural refusals (encrypted, out-of-range or overlapping entries): 0; zlib-scan false hits: 24657; walker panics: 0.

## Reconstructed streams by the library's encoder estimate

|  | streams | deflate bytes | plain bytes | correction bytes | corrections / deflate |
|---|---|---|---|---|---|
| strategy=Default hash=Libdeflate4 add=AddAll match=Lazy zlib_compatible=false | 1626 | 2899487 | 305274044 | 56682 | 1.95% |
| strategy=Default hash=Libdeflate4 add=AddFirst match=Greedy zlib_compatible=false | 2436 | 2820686 | 13392402 | 179095 | 6.35% |
| strategy=Default hash=Libdeflate4 add=AddFirstAndLast match=Greedy zlib_compatible=false | 90 | 75146 | 348079 | 5869 | 7.81% |
| strategy=Default hash=Libdeflate4 add=AddFirstWith32KBoundary match=Greedy zlib_compatible=false | 1 | 7729 | 262144 | 82 | 1.06% |
| strategy=Default hash=Libdeflate4Fast add=AddAll match=Lazy zlib_compatible=true | 133 | 5838937 | 117673875 | 194767 | 3.34% |
| strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true | 1015 | 3403388 | 8589874 | 95594 | 2.81% |
| strategy=Default hash=Libdeflate4Fast add=AddFirstAndLast match=Greedy zlib_compatible=true | 38 | 2677162 | 4944027 | 110610 | 4.13% |
| strategy=Default hash=Libdeflate4Fast add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 154 | 19674 | 200901 | 3918 | 19.91% |
| strategy=Default hash=Libdeflate4Fast add=AddFirstWith32KBoundary match=Greedy zlib_compatible=true | 5 | 14097 | 3212520 | 188 | 1.33% |
| strategy=Default hash=MiniZFast add=AddFirst match=Greedy zlib_compatible=false | 2 | 1046 | 15424 | 46 | 4.40% |
| strategy=Default hash=RandomVector add=AddAll match=Lazy zlib_compatible=false | 26 | 3067584 | 21643650 | 2881 | 0.09% |
| strategy=Default hash=RandomVector add=AddAll match=Lazy zlib_compatible=true | 79 | 3384375 | 72217843 | 49723 | 1.47% |
| strategy=Default hash=RandomVector add=AddFirst match=Greedy zlib_compatible=false | 1 | 62890 | 65536 | 89 | 0.14% |
| strategy=Default hash=RandomVector add=AddFirst match=Greedy zlib_compatible=true | 295 | 5039563 | 11154480 | 93925 | 1.86% |
| strategy=Default hash=RandomVector add=AddFirstAndLast match=Greedy zlib_compatible=true | 12 | 52789 | 110730 | 1421 | 2.69% |
| strategy=Default hash=RandomVector add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 4 | 143086 | 223301 | 162 | 0.11% |
| strategy=Default hash=RandomVector add=AddFirstWith32KBoundary match=Greedy zlib_compatible=true | 1 | 2403 | 65536 | 36 | 1.50% |
| strategy=Default hash=Zlib add=AddAll match=Lazy zlib_compatible=false | 115 | 25317488 | 80152380 | 125921 | 0.50% |
| strategy=Default hash=Zlib add=AddAll match=Lazy zlib_compatible=true | 650 | 21102516 | 390038372 | 29908 | 0.14% |
| strategy=Default hash=Zlib add=AddFirst match=Greedy zlib_compatible=false | 5 | 8374 | 26548 | 575 | 6.87% |
| strategy=Default hash=Zlib add=AddFirst match=Greedy zlib_compatible=true | 3403 | 41452961 | 162552265 | 968295 | 2.34% |
| strategy=Default hash=Zlib add=AddFirstAndLast match=Greedy zlib_compatible=true | 187 | 7432476 | 18176933 | 360148 | 4.85% |
| strategy=Default hash=Zlib add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 228 | 25130 | 71472 | 6455 | 25.69% |
| strategy=Default hash=ZlibNG add=AddAll match=Lazy zlib_compatible=true | 2 | 23054 | 54626 | 420 | 1.82% |
| strategy=Default hash=ZlibNG add=AddFirst match=Greedy zlib_compatible=true | 8 | 65256 | 147590 | 868 | 1.33% |
| strategy=HuffOnly hash=None add=AddAll match=Greedy zlib_compatible=true | 76 | 1698 | 1519 | 1806 | 106.36% |
| strategy=RleOnly hash=Libdeflate4Fast add=AddFirstAndLast match=Greedy zlib_compatible=true | 62 | 69084 | 70181910 | 1839 | 2.66% |
| strategy=RleOnly hash=Libdeflate4Fast add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 3 | 18 | 375 | 75 | 416.67% |
| strategy=RleOnly hash=Zlib add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 2 | 80 | 88 | 50 | 62.50% |
| strategy=Store hash=None add=AddAll match=Greedy zlib_compatible=true | 9 | 2541 | 2496 | 252 | 9.92% |

## Reconstructed streams by correction overhead

|  | streams | deflate bytes | plain bytes | correction bytes | corrections / deflate |
|---|---|---|---|---|---|
| under_1_pct | 1305 | 76206881 | 738497134 | 95842 | 0.13% |
| 1_to_5_pct | 3411 | 27596624 | 445006545 | 779019 | 2.82% |
| 5_to_10_pct | 2378 | 20481995 | 87947333 | 1285690 | 6.28% |
| 10_to_25_pct | 1990 | 625683 | 6143527 | 85826 | 13.72% |
| 25_to_50_pct | 774 | 71870 | 2193072 | 24558 | 34.17% |
| 50_pct_or_more | 810 | 27665 | 1013329 | 20765 | 75.06% |

## Speed on reconstructed streams, by container kind

|  | deflate bytes | plain bytes | analyse s | analyse MB/s (deflate in) | recreate s | recreate MB/s (deflate out) |
|---|---|---|---|---|---|---|
| office | 209385 | 491917 | 0.013 | 16.6 | 0.006 | 34.3 |
| jar | 251027 | 574879 | 0.038 | 6.5 | 0.012 | 21.0 |
| zip | 35876252 | 77738514 | 1.791 | 20.0 | 0.984 | 36.4 |
| png | 42884273 | 473307185 | 9.501 | 4.5 | 5.322 | 8.1 |
| pdf | 40726298 | 694871660 | 9.239 | 4.4 | 5.298 | 7.7 |
| other | 5063483 | 33816785 | 0.923 | 5.5 | 0.634 | 8.0 |
| all | 125010718 | 1280800940 | 21.506 | 5.8 | 12.255 | 10.2 |
