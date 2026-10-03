# Probe `dedup`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `6c04dedc7d5a` (release profile, opt-level 3) on `megatron`, 2026-10-03T10:51:33Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 16.8 s elapsed
- libraries: fastcdc 5.0.0, libzstd 1.5.7, zstd (command line) 1.5.7
- note: hdiffz/hpatchz: skipped: hdiffz: not installed

Chunking: fastcdc v2020, normalization level 1, per file, minimum 4096 B, average 65536 B, maximum 524288 B, chunks identified by blake3. A class row dedups inside the class, the corpus row across all classes. Duplicate files are files equal to an earlier one (their bytes: every copy after the first). Saved = bytes not in a first-seen chunk, as a share of all bytes. Speeds are MB/s of file bytes (10^6 bytes per second): FastCDC alone, and FastCDC plus BLAKE3 of every chunk, each timed alone on one thread with the file in memory.

| class | files | bytes | dup files | dup bytes | dup share | chunks | unique chunk bytes | saved by chunk dedup | FastCDC MB/s | FastCDC+BLAKE3 MB/s |
|---|---|---|---|---|---|---|---|---|---|---|
| archives-nested | 14 | 34862973 | 0 | 0 | 0.00% | 492 | 34003771 | 2.46% | 3602.6 | 2363.7 |
| audio | 21 | 157896311 | 0 | 0 | 0.00% | 2153 | 157896311 | 0.00% | 3454.0 | 2252.6 |
| backup-versions | 1974 | 25541629 | 1302 | 16066597 | 62.90% | 2119 | 9299674 | 63.59% | 4118.4 | 2252.0 |
| encrypted-random | 2 | 67108864 | 0 | 0 | 0.00% | 946 | 67108864 | 0.00% | 3403.6 | 2251.4 |
| game-assets | 4660 | 52470892 | 499 | 401736 | 0.77% | 4928 | 52030976 | 0.84% | 4577.0 | 2294.8 |
| logs-text | 4 | 111493402 | 0 | 0 | 0.00% | 1389 | 111493402 | 0.00% | 3428.6 | 2332.5 |
| model-weights | 1 | 17756393 | 0 | 0 | 0.00% | 238 | 17756393 | 0.00% | 3623.5 | 2319.6 |
| office-pdf | 166 | 267862151 | 0 | 0 | 0.00% | 3713 | 267269932 | 0.22% | 3509.9 | 2286.9 |
| photo-jpeg | 355 | 327146817 | 0 | 0 | 0.00% | 4610 | 327146817 | 0.00% | 3668.6 | 2370.6 |
| photo-jpeg-edited | 60 | 32731056 | 0 | 0 | 0.00% | 486 | 32731056 | 0.00% | 3719.1 | 2402.8 |
| photo-raw-png | 950 | 117924515 | 80 | 54004 | 0.05% | 2414 | 117870511 | 0.05% | 3460.4 | 2217.8 |
| small-files | 20000 | 66170985 | 122 | 57816 | 0.09% | 20045 | 66113169 | 0.09% | 11020.0 | 2095.9 |
| software-installed | 433 | 126846043 | 28 | 677961 | 0.53% | 1962 | 124727346 | 1.67% | 3604.0 | 2295.1 |
| source-git | 664 | 14793892 | 7 | 1459 | 0.01% | 794 | 14792433 | 0.01% | 3946.9 | 2065.9 |
| text-prose | 51 | 148205637 | 0 | 0 | 0.00% | 2049 | 148205637 | 0.00% | 3448.6 | 2178.2 |
| video | 3 | 66228217 | 0 | 0 | 0.00% | 894 | 66228217 | 0.00% | 3368.4 | 2177.8 |
| vm-image | 2 | 174063616 | 0 | 0 | 0.00% | 1342 | 98897733 | 43.18% | 3332.6 | 2267.4 |
| (corpus) | 29360 | 1809103393 | 2690 | 25780574 | 1.43% | 50574 | 1704938774 | 5.76% | 3630.0 | 2274.3 |

Versions of `backup-versions` (top-level folders, in natural order: runs of digits compare as numbers, so v2 comes before v10): unique chunk bytes after adding each version in turn, and zstd level 19 (library defaults) of the chunks that are new in that version.

| version | files | bytes | cumulative unique chunks | cumulative unique chunk bytes | new unique chunk bytes | new unique chunks, zstd 19 bytes |
|---|---|---|---|---|---|---|
| 1 (v1) | 658 | 8510359 | 704 | 8508900 | 8508900 | 1868839 |
| 2 (v2) | 658 | 8510359 | 709 | 8556601 | 47701 | 9627 |
| 3 (v3) | 658 | 8520911 | 729 | 9299674 | 743073 | 122424 |

Delta of each version against the one before, on the in-process deterministic tar of the version folder (entries named relative to the folder, so identical content gives identical bytes). Compared against zstd level 19 of the new tar alone, with library defaults and again with the patch's window log and long-distance matching, and against zstd level 19 of the new unique chunks (previous table). Every patch is applied and the result compared with the new tar byte for byte; seconds are the wall time of the program run, with the program's own start-up.

| pair | tool | settings | old tar bytes | new tar bytes | new alone, zstd 19 bytes | new alone, zstd 19 long bytes | new unique chunks, zstd 19 bytes | patch bytes | patch vs new alone (long) | create s | apply s | status |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 2 vs 1 | zstd --patch-from | level 19, --long=27, 1 thread | 9029120 | 9029120 | 1886652 | 1886107 | 9627 | 1397 | 0.07% | 1.835 | 0.016 | verified |
| 2 vs 1 | hdiffz | zstd level 19 inside, 1 thread | 9029120 | 9029120 | 1886652 | 1886107 | 9627 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
| 3 vs 2 | zstd --patch-from | level 19, --long=27, 1 thread | 9029120 | 9040384 | 1888736 | 1888502 | 122424 | 4529 | 0.24% | 1.935 | 0.016 | verified |
| 3 vs 2 | hdiffz | zstd level 19 inside, 1 thread | 9029120 | 9040384 | 1888736 | 1888502 | 122424 | n/a | n/a | n/a | n/a | skipped: hdiffz: not installed |
