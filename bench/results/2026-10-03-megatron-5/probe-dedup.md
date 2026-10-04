# Probe `dedup`

- corpus: `full` (manifest BLAKE3 `37fd79b4563887d48932715918eaf72e24b1bc8fe7588fd27a280e2fcc4a0cf0`)
- build `78bc9601a767` (release profile, opt-level 3) on `megatron`, 2026-10-04T00:41:20Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 43.5 s elapsed
- libraries: fastcdc 5.0.0, libzstd 1.5.7, zstd (command line) 1.5.7
- note: hdiffz/hpatchz: skipped: hdiffz: not installed

Chunking: fastcdc v2020, normalization level 1, per file, minimum 4096 B, average 65536 B, maximum 524288 B, chunks identified by blake3. A class row dedups inside the class, the corpus row across all classes. Duplicate files are files equal to an earlier one (their bytes: every copy after the first). Saved = bytes not in a first-seen chunk, as a share of all bytes. Speeds are MB/s of file bytes (10^6 bytes per second): FastCDC alone, and FastCDC plus BLAKE3 of every chunk, each timed alone on one thread with the file in memory.

| class | files | bytes | dup files | dup bytes | dup share | chunks | unique chunk bytes | saved by chunk dedup | FastCDC MB/s | FastCDC+BLAKE3 MB/s |
|---|---|---|---|---|---|---|---|---|---|---|
| archives-nested | 14 | 34975551 | 0 | 0 | 0.00% | 509 | 34499242 | 1.36% | 3413.3 | 1937.0 |
| audio | 21 | 157896311 | 0 | 0 | 0.00% | 2153 | 157896311 | 0.00% | 3418.1 | 2034.4 |
| backup-versions | 1974 | 25541629 | 1302 | 16066597 | 62.90% | 2119 | 9299674 | 63.59% | 3642.7 | 1642.9 |
| encrypted-random | 2 | 419430400 | 0 | 0 | 0.00% | 5700 | 419430400 | 0.00% | 3353.9 | 2191.9 |
| game-assets | 4660 | 52470892 | 499 | 401736 | 0.77% | 4928 | 52030976 | 0.84% | 4550.2 | 2184.8 |
| logs-text | 11 | 2686418017 | 0 | 0 | 0.00% | 25727 | 2671078555 | 0.57% | 3255.7 | 2038.7 |
| model-weights | 2 | 286816945 | 0 | 0 | 0.00% | 3906 | 286816945 | 0.00% | 3196.5 | 1761.3 |
| office-pdf | 2334 | 2114690575 | 0 | 0 | 0.00% | 28883 | 2084191951 | 1.44% | 3297.6 | 1852.1 |
| photo-jpeg | 3055 | 8258908517 | 0 | 0 | 0.00% | 113441 | 8258908517 | 0.00% | 3312.2 | 1882.4 |
| photo-jpeg-edited | 600 | 550674750 | 0 | 0 | 0.00% | 7863 | 550674750 | 0.00% | 3530.6 | 2146.3 |
| photo-raw-png | 1016 | 537693574 | 80 | 54004 | 0.01% | 7727 | 537639570 | 0.01% | 3408.9 | 2068.1 |
| small-files | 20000 | 50016512 | 1961 | 1260154 | 2.52% | 20037 | 48756358 | 2.52% | 13171.6 | 1820.1 |
| software-installed | 9671 | 437043701 | 1508 | 105137349 | 24.06% | 14501 | 329308991 | 24.65% | 3574.6 | 2060.9 |
| source-git | 663 | 35365500 | 7 | 1459 | 0.00% | 1091 | 35364041 | 0.00% | 3402.6 | 1776.1 |
| text-prose | 63 | 360144217 | 0 | 0 | 0.00% | 4685 | 360144217 | 0.00% | 3293.0 | 1915.9 |
| video | 4 | 959731214 | 0 | 0 | 0.00% | 12829 | 959731214 | 0.00% | 3296.0 | 1873.4 |
| vm-image | 4 | 2155872426 | 0 | 0 | 0.00% | 22507 | 1858288402 | 13.80% | 3262.3 | 1823.4 |
| (corpus) | 44094 | 19123690731 | 6010 | 131442300 | 0.69% | 278606 | 18635413083 | 2.55% | 3319.4 | 1913.3 |

Versions of `backup-versions` (top-level folders, in natural order: runs of digits compare as numbers, so v2 comes before v10): unique chunk bytes after adding each version in turn, and zstd level 19 (library defaults) of the chunks that are new in that version.

| version | files | bytes | cumulative unique chunks | cumulative unique chunk bytes | new unique chunk bytes | new unique chunks, zstd 19 bytes |
|---|---|---|---|---|---|---|
| 1 (v1) | 658 | 8510359 | 704 | 8508900 | 8508900 | 1868839 |
| 2 (v2) | 658 | 8510359 | 709 | 8556601 | 47701 | 9627 |
| 3 (v3) | 658 | 8520911 | 729 | 9299674 | 743073 | 122424 |

Delta of each version against the one before, on the in-process deterministic tar of the version folder (entries named relative to the folder, so identical content gives identical bytes). Compared against zstd level 19 of the new tar alone, with library defaults and again with the patch's window log and long-distance matching, and against zstd level 19 of the new unique chunks (previous table). Every patch is applied and the result compared with the new tar byte for byte; seconds are the wall time of the program run, with the program's own start-up.

| pair | tool | settings | old tar bytes | new tar bytes | new alone, zstd 19 bytes | new alone, zstd 19 long bytes | new unique chunks, zstd 19 bytes | patch bytes | patch vs new alone (long) | create s | apply s | status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 2 vs 1 | zstd --patch-from | level 19, --long=27, 1 thread | 9029120 | 9029120 | 1886652 | 1886107 | 9627 | 1397 | 0.07% | 1.906 | 0.015 | verified |
| 2 vs 1 | hdiffz | zstd level 19 inside, 1 thread | 9029120 | 9029120 | 1886652 | 1886107 | 9627 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
| 3 vs 2 | zstd --patch-from | level 19, --long=27, 1 thread | 9029120 | 9040384 | 1888736 | 1888502 | 122424 | 4529 | 0.24% | 2.120 | 0.016 | verified |
| 3 vs 2 | hdiffz | zstd level 19 inside, 1 thread | 9029120 | 9040384 | 1888736 | 1888502 | 122424 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
