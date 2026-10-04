# Probe `deflate`

- corpus: `full` (manifest BLAKE3 `37fd79b4563887d48932715918eaf72e24b1bc8fe7588fd27a280e2fcc4a0cf0`)
- build `78bc9601a767` (release profile, opt-level 3) on `megatron`, 2026-10-04T00:17:44Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 1415.7 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libzstd 1.5.7, preflate-rs 0.7.6

Settings: max chain 4096, plain text limit 67108864 bytes per stream, 536870912 bytes per file; library verification off; zstd level 19, xz preset 9; A = original file compressed whole, B = reconstructed streams replaced by plain data, plus the correction bytes. Streams found by the zlib scan must inflate to at least 1024 bytes. Recognition: the library has no recognised/unrecognised flag: every accepted stream gets an estimate; judge by the overhead buckets Stored ZIP entries are not examined (one level deep), so the gains are a lower bound; per-stream metadata is not counted.

## By container kind

|  | files | streams found | reconstructed | failed | skipped | original bytes | A zstd | B zstd + corr | B/A zstd | A xz | B xz + corr | B/A xz |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| office | 1 | 36 | 36 | 0 | 0 | 474012 | 404656 | 351270 | 86.81% | 407076 | 348100 | 85.51% |
| jar | 1 | 228 | 225 | 3 | 0 | 298435 | 259549 | 156605 | 60.34% | 261408 | 142263 | 54.42% |
| zip | 13 | 1495 | 1396 | 99 | 0 | 38355942 | 34697708 | 26271079 | 75.71% | 34527092 | 26128087 | 75.67% |
| png | 2801 | 2801 | 2774 | 27 | 0 | 133949354 | 133480568 | 119228199 | 89.32% | 133581832 | 115846864 | 86.72% |
| pdf | 1380 | 119692 | 117965 | 1707 | 20 | 1127159589 | 913633968 | 791428787 | 86.62% | 912811592 | 760761288 | 83.34% |
| other | 13499 | 6904 | 6874 | 30 | 0 | 1876636961 | 841040212 | 812037403 | 96.55% | 807471360 | 770305135 | 95.40% |
| all | 17695 | 131156 | 129270 | 1866 | 20 | 3176874293 | 1923516661 | 1749473343 | 90.95% | 1889060360 | 1673531737 | 88.59% |

## By class

|  | files | streams found | reconstructed | failed | skipped | original bytes | A zstd | B zstd + corr | B/A zstd | A xz | B xz + corr | B/A xz |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| office-pdf | 2334 | 124973 | 123223 | 1730 | 20 | 2114690575 | 1453984967 | 1303133651 | 89.62% | 1441356368 | 1252603928 | 86.90% |
| photo-raw-png | 1016 | 973 | 966 | 7 | 0 | 537693574 | 263182788 | 249392447 | 94.76% | 251016476 | 233894517 | 93.18% |
| game-assets | 4660 | 3346 | 3321 | 25 | 0 | 52470892 | 22043022 | 21332584 | 96.78% | 21704772 | 20772109 | 95.70% |
| archives-nested | 14 | 1196 | 1173 | 23 | 0 | 34975551 | 31276989 | 23321224 | 74.56% | 31108260 | 23243034 | 74.72% |
| software-installed | 9671 | 668 | 587 | 81 | 0 | 437043701 | 153028895 | 152293437 | 99.52% | 143874484 | 143018149 | 99.40% |
| all | 17695 | 131156 | 129270 | 1866 | 20 | 3176874293 | 1923516661 | 1749473343 | 90.95% | 1889060360 | 1673531737 | 88.59% |

## Failures and skips by cause

| cause | office | jar | apk | zip | png | gzip | pdf | other | total |
|---|---|---|---|---|---|---|---|---|---|
| (skipped for size) | 0 | 0 | 0 | 0 | 0 | 0 | 20 | 0 | 20 |
| InvalidDeflate | 0 | 0 | 0 | 3 | 0 | 0 | 12 | 0 | 15 |
| NoCompressionCandidates | 0 | 0 | 0 | 79 | 0 | 0 | 49 | 1 | 129 |
| NonZeroPadding | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 1 |
| Panic | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 2 |
| PredictionFailure | 0 | 3 | 0 | 17 | 27 | 0 | 1643 | 29 | 1719 |

Structural refusals (encrypted, out-of-range or overlapping entries): 0; zlib-scan false hits: 116702; walker panics: 0.

## Reconstructed streams by the library's encoder estimate

|  | streams | deflate bytes | plain bytes | correction bytes | corrections / deflate |
|---|---|---|---|---|---|
| strategy=Default hash=Crc32cHash add=AddAll match=Lazy zlib_compatible=true | 1 | 87996 | 480000 | 25 | 0.03% |
| strategy=Default hash=Libdeflate4 add=AddAll match=Lazy zlib_compatible=false | 5994 | 30210756 | 1432506032 | 892093 | 2.95% |
| strategy=Default hash=Libdeflate4 add=AddFirst match=Greedy zlib_compatible=false | 27305 | 45358477 | 246143030 | 2960866 | 6.53% |
| strategy=Default hash=Libdeflate4 add=AddFirstAndLast match=Greedy zlib_compatible=false | 2132 | 1681284 | 9529088 | 138731 | 8.25% |
| strategy=Default hash=Libdeflate4 add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=false | 37 | 11223 | 13352 | 972 | 8.66% |
| strategy=Default hash=Libdeflate4 add=AddFirstWith32KBoundary match=Greedy zlib_compatible=false | 1 | 7729 | 262144 | 82 | 1.06% |
| strategy=Default hash=Libdeflate4Fast add=AddAll match=Lazy zlib_compatible=false | 6 | 2323076 | 137743821 | 139203 | 5.99% |
| strategy=Default hash=Libdeflate4Fast add=AddAll match=Lazy zlib_compatible=true | 1422 | 10501656 | 299384613 | 293208 | 2.79% |
| strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=false | 5 | 599 | 3428 | 166 | 27.71% |
| strategy=Default hash=Libdeflate4Fast add=AddFirst match=Greedy zlib_compatible=true | 4568 | 4392932 | 13074939 | 208479 | 4.75% |
| strategy=Default hash=Libdeflate4Fast add=AddFirstAndLast match=Greedy zlib_compatible=true | 2532 | 3179708 | 20471026 | 198155 | 6.23% |
| strategy=Default hash=Libdeflate4Fast add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=false | 1 | 22 | 278 | 29 | 131.82% |
| strategy=Default hash=Libdeflate4Fast add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 8241 | 1506204 | 3227652 | 264345 | 17.55% |
| strategy=Default hash=Libdeflate4Fast add=AddFirstWith32KBoundary match=Greedy zlib_compatible=true | 19 | 134318 | 28392896 | 962 | 0.72% |
| strategy=Default hash=MiniZFast add=AddAll match=Lazy zlib_compatible=false | 1 | 1219 | 17217 | 38 | 3.12% |
| strategy=Default hash=MiniZFast add=AddFirst match=Greedy zlib_compatible=false | 3 | 1864 | 25372 | 82 | 4.40% |
| strategy=Default hash=MiniZFast add=AddFirstAndLast match=Greedy zlib_compatible=false | 3 | 2977 | 35977 | 106 | 3.56% |
| strategy=Default hash=RandomVector add=AddAll match=Lazy zlib_compatible=false | 70 | 21434729 | 82820253 | 25099 | 0.12% |
| strategy=Default hash=RandomVector add=AddAll match=Lazy zlib_compatible=true | 1486 | 45702281 | 911569044 | 235914 | 0.52% |
| strategy=Default hash=RandomVector add=AddFirst match=Greedy zlib_compatible=false | 3 | 183443 | 219191 | 771 | 0.42% |
| strategy=Default hash=RandomVector add=AddFirst match=Greedy zlib_compatible=true | 3750 | 26983762 | 104048648 | 897156 | 3.32% |
| strategy=Default hash=RandomVector add=AddFirstAndLast match=Greedy zlib_compatible=true | 262 | 3956129 | 7004086 | 49794 | 1.26% |
| strategy=Default hash=RandomVector add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 341 | 233449 | 480304 | 10533 | 4.51% |
| strategy=Default hash=RandomVector add=AddFirstWith32KBoundary match=Greedy zlib_compatible=true | 2 | 10127 | 288320 | 180 | 1.78% |
| strategy=Default hash=Zlib add=AddAll match=Lazy zlib_compatible=false | 320 | 118680105 | 480405623 | 784865 | 0.66% |
| strategy=Default hash=Zlib add=AddAll match=Lazy zlib_compatible=true | 6734 | 222881696 | 3192995017 | 390624 | 0.18% |
| strategy=Default hash=Zlib add=AddFirst match=Greedy zlib_compatible=false | 30 | 1289446 | 3209413 | 68410 | 5.31% |
| strategy=Default hash=Zlib add=AddFirst match=Greedy zlib_compatible=true | 42183 | 251750108 | 1287383419 | 8701741 | 3.46% |
| strategy=Default hash=Zlib add=AddFirstAndLast match=Greedy zlib_compatible=false | 4 | 196805 | 289125 | 951 | 0.48% |
| strategy=Default hash=Zlib add=AddFirstAndLast match=Greedy zlib_compatible=true | 5756 | 38187994 | 93841934 | 1503726 | 3.94% |
| strategy=Default hash=Zlib add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 7455 | 1041774 | 1353053 | 220724 | 21.19% |
| strategy=Default hash=Zlib add=AddFirstWith32KBoundary match=Greedy zlib_compatible=true | 3 | 116426 | 22587432 | 547 | 0.47% |
| strategy=Default hash=ZlibNG add=AddAll match=Lazy zlib_compatible=true | 13 | 1016240 | 2999554 | 824 | 0.08% |
| strategy=Default hash=ZlibNG add=AddFirst match=Greedy zlib_compatible=true | 9 | 65328 | 147665 | 899 | 1.38% |
| strategy=Default hash=ZlibNG add=AddFirstAndLast match=Greedy zlib_compatible=true | 1 | 19575 | 29628 | 168 | 0.86% |
| strategy=HuffOnly hash=None add=AddAll match=Greedy zlib_compatible=true | 6482 | 520864 | 526422 | 184465 | 35.42% |
| strategy=RleOnly hash=Libdeflate4Fast add=AddFirstAndLast match=Greedy zlib_compatible=true | 634 | 1311741 | 1338841765 | 18574 | 1.42% |
| strategy=RleOnly hash=Libdeflate4Fast add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 1408 | 9540 | 369343 | 35178 | 368.74% |
| strategy=RleOnly hash=Zlib add=AddFirstAndLast match=Greedy zlib_compatible=true | 2 | 340 | 1536 | 54 | 15.88% |
| strategy=RleOnly hash=Zlib add=AddFirstExcept4kBoundary match=Greedy zlib_compatible=true | 2 | 80 | 88 | 50 | 62.50% |
| strategy=Store hash=None add=AddAll match=Greedy zlib_compatible=true | 49 | 23805 | 23560 | 1372 | 5.76% |

## Reconstructed streams by correction overhead

|  | streams | deflate bytes | plain bytes | correction bytes | corrections / deflate |
|---|---|---|---|---|---|
| under_1_pct | 11610 | 499521295 | 7007960252 | 798430 | 0.16% |
| 1_to_5_pct | 21266 | 170501515 | 1690976134 | 4783172 | 2.81% |
| 5_to_10_pct | 33202 | 148164905 | 876773648 | 9662991 | 6.52% |
| 10_to_25_pct | 28513 | 14730045 | 119589357 | 1934720 | 13.13% |
| 25_to_50_pct | 15002 | 1497145 | 19188954 | 488047 | 32.60% |
| 50_pct_or_more | 19677 | 602922 | 8256943 | 562801 | 93.35% |

## Speed on reconstructed streams, by container kind

|  | deflate bytes | plain bytes | analyse s | analyse MB/s (deflate in) | recreate s | recreate MB/s (deflate out) |
|---|---|---|---|---|---|---|
| office | 209385 | 491917 | 0.013 | 16.6 | 0.006 | 37.2 |
| jar | 251027 | 574879 | 0.040 | 6.3 | 0.013 | 19.9 |
| zip | 35992638 | 77770582 | 2.082 | 17.3 | 1.221 | 29.5 |
| png | 132273422 | 808565395 | 25.811 | 5.1 | 17.677 | 7.5 |
| pdf | 548812067 | 7450071752 | 115.961 | 4.7 | 63.263 | 8.7 |
| other | 117479288 | 1385270763 | 27.324 | 4.3 | 19.129 | 6.1 |
| all | 835017827 | 9722745288 | 171.230 | 4.9 | 101.308 | 8.2 |
