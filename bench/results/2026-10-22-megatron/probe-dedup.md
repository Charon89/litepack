# Probe `dedup`

- corpus: `full` (manifest BLAKE3 `a52af02774b9134362467adc7b6bf273f6f89592085ce71fe51f05e10574e12d`)
- build `5e34803eead7` (release profile, opt-level 3) on `megatron`, 2026-10-22T03:21:41Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 671.3 s elapsed
- libraries: fastcdc 5.0.0, libzstd 1.5.7, zstd (command line) 1.5.7
- note: hdiffz/hpatchz: skipped: hdiffz: not installed

Chunking: fastcdc v2020, normalization level 1, per file, minimum 4096 B, average 65536 B, maximum 524288 B, chunks identified by blake3. A class row dedups inside the class, the corpus row across all classes. Duplicate files are files equal to an earlier one (their bytes: every copy after the first). Saved = bytes not in a first-seen chunk, as a share of all bytes. Speeds are MB/s of file bytes (10^6 bytes per second): FastCDC alone, and FastCDC plus BLAKE3 of every chunk, each timed alone on one thread with the file in memory.

| class | files | bytes | dup files | dup bytes | dup share | chunks | unique chunk bytes | saved by chunk dedup | FastCDC MB/s | FastCDC+BLAKE3 MB/s |
|---|---|---|---|---|---|---|---|---|---|---|
| archives-nested | 14 | 34975551 | 0 | 0 | 0.00% | 509 | 34499242 | 1.36% | 3224.7 | 2125.1 |
| audio | 21 | 157896311 | 0 | 0 | 0.00% | 2153 | 157896311 | 0.00% | 3372.2 | 2162.6 |
| backup-versions | 1974 | 25541629 | 1302 | 16066597 | 62.90% | 2119 | 9299674 | 63.59% | 4272.1 | 2300.0 |
| backup-versions-large | 130215 | 1658978348 | 88756 | 1133104605 | 68.30% | 144042 | 453500406 | 72.66% | 4217.0 | 2262.0 |
| encrypted-random | 2 | 419430400 | 0 | 0 | 0.00% | 5700 | 419430400 | 0.00% | 3243.4 | 2240.8 |
| game-assets | 4660 | 52470892 | 499 | 401736 | 0.77% | 4928 | 52030976 | 0.84% | 4673.0 | 2268.4 |
| logs-text | 11 | 2686418017 | 0 | 0 | 0.00% | 25727 | 2671078555 | 0.57% | 3334.6 | 2226.4 |
| model-weights | 2 | 286816945 | 0 | 0 | 0.00% | 3906 | 286816945 | 0.00% | 3413.1 | 2209.6 |
| office-pdf | 2334 | 2114690575 | 0 | 0 | 0.00% | 28883 | 2084191951 | 1.44% | 3491.4 | 2269.4 |
| photo-jpeg | 3055 | 8258908517 | 0 | 0 | 0.00% | 113441 | 8258908517 | 0.00% | 3504.3 | 2272.0 |
| photo-jpeg-edited | 600 | 550674750 | 0 | 0 | 0.00% | 7863 | 550674750 | 0.00% | 3593.5 | 2317.7 |
| photo-raw-png | 1016 | 537693574 | 80 | 54004 | 0.01% | 7727 | 537639570 | 0.01% | 3372.0 | 2267.4 |
| small-files | 20000 | 50016512 | 1961 | 1260154 | 2.52% | 20037 | 48756358 | 2.52% | 13446.0 | 1931.7 |
| software-installed | 9671 | 437043701 | 1508 | 105137349 | 24.06% | 14501 | 329308991 | 24.65% | 3698.0 | 2283.8 |
| source-git | 663 | 35365500 | 7 | 1459 | 0.00% | 1091 | 35364041 | 0.00% | 3733.6 | 2324.4 |
| text-prose | 63 | 360144217 | 0 | 0 | 0.00% | 4685 | 360144217 | 0.00% | 3424.2 | 2244.4 |
| video | 4 | 959731214 | 0 | 0 | 0.00% | 12829 | 959731214 | 0.00% | 3412.4 | 2280.0 |
| vm-image | 4 | 2155872426 | 0 | 0 | 0.00% | 22507 | 1858288402 | 13.80% | 3432.1 | 2219.4 |
| (corpus) | 174309 | 20782669079 | 94772 | 1264559733 | 6.08% | 422648 | 19088900661 | 8.15% | 3517.5 | 2257.1 |

Versions of `backup-versions` (top-level folders, in natural order: runs of digits compare as numbers, so v2 comes before v10): unique chunk bytes after adding each version in turn, and zstd level 19 (library defaults) of the chunks that are new in that version.

| version | files | bytes | cumulative unique chunks | cumulative unique chunk bytes | new unique chunk bytes | new unique chunks, zstd 19 bytes |
|---|---|---|---|---|---|---|
| 1 (v1) | 658 | 8510359 | 704 | 8508900 | 8508900 | 1868839 |
| 2 (v2) | 658 | 8510359 | 709 | 8556601 | 47701 | 9627 |
| 3 (v3) | 658 | 8520911 | 729 | 9299674 | 743073 | 122424 |

Delta of each version against the one before, on the in-process deterministic tar of the version folder (entries named relative to the folder, so identical content gives identical bytes). Compared against zstd level 19 of the new tar alone, with library defaults and again with the patch's window log and long-distance matching, and against zstd level 19 of the new unique chunks (previous table). Every patch is applied and the result compared with the new tar byte for byte; seconds are the wall time of the program run, with the program's own start-up.

| pair | tool | settings | old tar bytes | new tar bytes | new alone, zstd 19 bytes | new alone, zstd 19 long bytes | new unique chunks, zstd 19 bytes | patch bytes | patch vs new alone (long) | create s | apply s | status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 2 vs 1 | zstd --patch-from | level 19, --long=27, 1 thread | 9029120 | 9029120 | 1886652 | 1886107 | 9627 | 1397 | 0.07% | 1.840 | 0.013 | verified |
| 2 vs 1 | hdiffz | zstd level 19 inside, 1 thread | 9029120 | 9029120 | 1886652 | 1886107 | 9627 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
| 3 vs 2 | zstd --patch-from | level 19, --long=27, 1 thread | 9029120 | 9040384 | 1888736 | 1888502 | 122424 | 4529 | 0.24% | 2.105 | 0.013 | verified |
| 3 vs 2 | hdiffz | zstd level 19 inside, 1 thread | 9029120 | 9040384 | 1888736 | 1888502 | 122424 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |

Versions of `backup-versions-large` (top-level folders, in natural order: runs of digits compare as numbers, so v2 comes before v10): unique chunk bytes after adding each version in turn, and zstd level 19 (library defaults) of the chunks that are new in that version.

| version | files | bytes | cumulative unique chunks | cumulative unique chunk bytes | new unique chunk bytes | new unique chunks, zstd 19 bytes |
|---|---|---|---|---|---|---|
| 1 (node-v1) | 43420 | 552150264 | 41767 | 400893398 | 400893398 | 65826133 |
| 2 (node-v2) | 43408 | 552547639 | 44156 | 439234676 | 38341278 | 14798825 |
| 3 (node-v3) | 43387 | 554280445 | 44572 | 453500406 | 14265730 | 1426865 |

Delta of each version against the one before, on the in-process deterministic tar of the version folder (entries named relative to the folder, so identical content gives identical bytes). Compared against zstd level 19 of the new tar alone, with library defaults and again with the patch's window log and long-distance matching, and against zstd level 19 of the new unique chunks (previous table). Every patch is applied and the result compared with the new tar byte for byte; seconds are the wall time of the program run, with the program's own start-up.

| pair | tool | settings | old tar bytes | new tar bytes | new alone, zstd 19 bytes | new alone, zstd 19 long bytes | new unique chunks, zstd 19 bytes | patch bytes | patch vs new alone (long) | create s | apply s | status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 2 vs 1 | zstd --patch-from | level 19, --long=30, 1 thread | 586011648 | 586387968 | 69474735 | 65515133 | 14798825 | 11498052 | 17.55% | 30.647 | 0.435 | verified |
| 2 vs 1 | hdiffz | zstd level 19 inside, 1 thread | 586011648 | 586387968 | 69474735 | 65515133 | 14798825 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
| 3 vs 2 | zstd --patch-from | level 19, --long=30, 1 thread | 586387968 | 588097024 | 69510524 | 65552932 | 1426865 | 304521 | 0.46% | 26.054 | 0.583 | verified |
| 3 vs 2 | hdiffz | zstd level 19 inside, 1 thread | 586387968 | 588097024 | 69510524 | 65552932 | 1426865 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
