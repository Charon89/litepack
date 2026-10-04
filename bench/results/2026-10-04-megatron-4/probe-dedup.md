# Probe `dedup`

- corpus: `full` (manifest BLAKE3 `1d24f5c824fad0c85fc3b9231643b2bd80a75437c6b3d38e86ef1f6450a05f43`)
- build `2be966ed1cff` (release profile, opt-level 3) on `megatron`, 2026-10-04T22:47:04Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 865.9 s elapsed
- libraries: fastcdc 5.0.0, libzstd 1.5.7, zstd (command line) 1.5.7
- note: hdiffz/hpatchz: skipped: hdiffz: not installed

Chunking: fastcdc v2020, normalization level 1, per file, minimum 4096 B, average 65536 B, maximum 524288 B, chunks identified by blake3. A class row dedups inside the class, the corpus row across all classes. Duplicate files are files equal to an earlier one (their bytes: every copy after the first). Saved = bytes not in a first-seen chunk, as a share of all bytes. Speeds are MB/s of file bytes (10^6 bytes per second): FastCDC alone, and FastCDC plus BLAKE3 of every chunk, each timed alone on one thread with the file in memory.

| class | files | bytes | dup files | dup bytes | dup share | chunks | unique chunk bytes | saved by chunk dedup | FastCDC MB/s | FastCDC+BLAKE3 MB/s |
|---|---|---|---|---|---|---|---|---|---|---|
| archives-nested | 14 | 34975551 | 0 | 0 | 0.00% | 509 | 34499242 | 1.36% | 3317.1 | 2170.4 |
| audio | 21 | 157896311 | 0 | 0 | 0.00% | 2153 | 157896311 | 0.00% | 3402.4 | 2265.3 |
| backup-versions | 1974 | 25541629 | 1302 | 16066597 | 62.90% | 2119 | 9299674 | 63.59% | 4094.3 | 2027.0 |
| backup-versions-large | 127153 | 1663755751 | 70702 | 680429327 | 40.90% | 141288 | 790680173 | 52.48% | 3944.6 | 1936.0 |
| encrypted-random | 2 | 419430400 | 0 | 0 | 0.00% | 5700 | 419430400 | 0.00% | 3197.1 | 2193.1 |
| game-assets | 4660 | 52470892 | 499 | 401736 | 0.77% | 4928 | 52030976 | 0.84% | 4588.8 | 2228.9 |
| logs-text | 11 | 2686418017 | 0 | 0 | 0.00% | 25727 | 2671078555 | 0.57% | 3338.0 | 1942.2 |
| model-weights | 2 | 286816945 | 0 | 0 | 0.00% | 3906 | 286816945 | 0.00% | 3309.7 | 1874.7 |
| office-pdf | 2334 | 2114690575 | 0 | 0 | 0.00% | 28883 | 2084191951 | 1.44% | 3452.7 | 2077.8 |
| photo-jpeg | 3055 | 8258908517 | 0 | 0 | 0.00% | 113441 | 8258908517 | 0.00% | 3421.2 | 2040.9 |
| photo-jpeg-edited | 600 | 550674750 | 0 | 0 | 0.00% | 7863 | 550674750 | 0.00% | 3623.5 | 2281.0 |
| photo-raw-png | 1016 | 537693574 | 80 | 54004 | 0.01% | 7727 | 537639570 | 0.01% | 3317.9 | 1875.9 |
| small-files | 20000 | 50016512 | 1961 | 1260154 | 2.52% | 20037 | 48756358 | 2.52% | 13778.3 | 2003.0 |
| software-installed | 9671 | 437043701 | 1508 | 105137349 | 24.06% | 14501 | 329308991 | 24.65% | 3756.7 | 2317.4 |
| source-git | 663 | 35365500 | 7 | 1459 | 0.00% | 1091 | 35364041 | 0.00% | 3429.1 | 1982.5 |
| text-prose | 63 | 360144217 | 0 | 0 | 0.00% | 4685 | 360144217 | 0.00% | 3353.4 | 1833.2 |
| video | 4 | 959731214 | 0 | 0 | 0.00% | 12829 | 959731214 | 0.00% | 3500.4 | 2127.1 |
| vm-image | 4 | 2155872426 | 0 | 0 | 0.00% | 22507 | 1858288402 | 13.80% | 3388.2 | 2152.7 |
| (corpus) | 171247 | 20787446482 | 76782 | 813064870 | 3.91% | 419894 | 19424597658 | 6.56% | 3460.1 | 2041.6 |

Versions of `backup-versions` (top-level folders, in natural order: runs of digits compare as numbers, so v2 comes before v10): unique chunk bytes after adding each version in turn, and zstd level 19 (library defaults) of the chunks that are new in that version.

| version | files | bytes | cumulative unique chunks | cumulative unique chunk bytes | new unique chunk bytes | new unique chunks, zstd 19 bytes |
|---|---|---|---|---|---|---|
| 1 (v1) | 658 | 8510359 | 704 | 8508900 | 8508900 | 1868839 |
| 2 (v2) | 658 | 8510359 | 709 | 8556601 | 47701 | 9627 |
| 3 (v3) | 658 | 8520911 | 729 | 9299674 | 743073 | 122424 |

Delta of each version against the one before, on the in-process deterministic tar of the version folder (entries named relative to the folder, so identical content gives identical bytes). Compared against zstd level 19 of the new tar alone, with library defaults and again with the patch's window log and long-distance matching, and against zstd level 19 of the new unique chunks (previous table). Every patch is applied and the result compared with the new tar byte for byte; seconds are the wall time of the program run, with the program's own start-up.

| pair | tool | settings | old tar bytes | new tar bytes | new alone, zstd 19 bytes | new alone, zstd 19 long bytes | new unique chunks, zstd 19 bytes | patch bytes | patch vs new alone (long) | create s | apply s | status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 2 vs 1 | zstd --patch-from | level 19, --long=27, 1 thread | 9029120 | 9029120 | 1886652 | 1886107 | 9627 | 1397 | 0.07% | 1.776 | 0.015 | verified |
| 2 vs 1 | hdiffz | zstd level 19 inside, 1 thread | 9029120 | 9029120 | 1886652 | 1886107 | 9627 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
| 3 vs 2 | zstd --patch-from | level 19, --long=27, 1 thread | 9029120 | 9040384 | 1888736 | 1888502 | 122424 | 4529 | 0.24% | 2.021 | 0.016 | verified |
| 3 vs 2 | hdiffz | zstd level 19 inside, 1 thread | 9029120 | 9040384 | 1888736 | 1888502 | 122424 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |

Versions of `backup-versions-large` (top-level folders, in natural order: runs of digits compare as numbers, so v2 comes before v10): unique chunk bytes after adding each version in turn, and zstd level 19 (library defaults) of the chunks that are new in that version.

| version | files | bytes | cumulative unique chunks | cumulative unique chunk bytes | new unique chunk bytes | new unique chunks, zstd 19 bytes |
|---|---|---|---|---|---|---|
| 1 (node-v1) | 43420 | 552150264 | 41767 | 400893398 | 400893398 | 65826133 |
| 2 (node-v2) | 42295 | 560073356 | 50783 | 580151107 | 179257709 | 31494892 |
| 3 (node-v3) | 41438 | 551532131 | 61799 | 790680173 | 210529066 | 35135628 |

Delta of each version against the one before, on the in-process deterministic tar of the version folder (entries named relative to the folder, so identical content gives identical bytes). Compared against zstd level 19 of the new tar alone, with library defaults and again with the patch's window log and long-distance matching, and against zstd level 19 of the new unique chunks (previous table). Every patch is applied and the result compared with the new tar byte for byte; seconds are the wall time of the program run, with the program's own start-up.

| pair | tool | settings | old tar bytes | new tar bytes | new alone, zstd 19 bytes | new alone, zstd 19 long bytes | new unique chunks, zstd 19 bytes | patch bytes | patch vs new alone (long) | create s | apply s | status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 2 vs 1 | zstd --patch-from | level 19, --long=30, 1 thread | 586011648 | 592967680 | 71383339 | 67284898 | 31494892 | 15930134 | 23.68% | 65.693 | 0.430 | verified |
| 2 vs 1 | hdiffz | zstd level 19 inside, 1 thread | 586011648 | 592967680 | 71383339 | 67284898 | 31494892 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
| 3 vs 2 | zstd --patch-from | level 19, --long=30, 1 thread | 592967680 | 583857664 | 66369842 | 62311256 | 35135628 | 16503784 | 26.49% | 77.529 | 0.449 | verified |
| 3 vs 2 | hdiffz | zstd level 19 inside, 1 thread | 592967680 | 583857664 | 66369842 | 62311256 | 35135628 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
