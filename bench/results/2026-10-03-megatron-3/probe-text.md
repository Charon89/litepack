# Probe `text`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `6c04dedc7d5a` (release profile, opt-level 3) on `megatron`, 2026-10-03T10:51:50Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 649.9 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libzstd 1.5.7, xz-cli xz (XZ Utils) 5.8.2, zstd-cli *** Zstandard CLI (64-bit) v1.5.7, by Yann Collet ***

Each class is one solid stream (a deterministic tar of its files). Sizes are compressed bytes as a percentage of the class's content bytes (the baseline runner's basis) and of the tar bytes; speeds are MB/s of the tar (10^6 bytes per second), compress and decompress each timed alone. Timing `in-process`: the library call on a stream in memory. Timing `process-wall`: the external program's wall time on files in the scratch directory, which includes its start-up and file I/O; the `xz-cli` and `zstd-cli` rows run the installed command-line tools on the same stream so the two bases can be compared. Comparisons are like-for-like only within this probe: the baseline runner runs at the machine's thread count on bsdtar's tar, these rows on one thread on this probe's tar. Every result is verified by decompressing and comparing bytes.

| class | compressor | setting | timing | content bytes | tar bytes | % of content bytes | % of tar bytes | compress MB/s | decompress MB/s | status |
|---|---|---|---|---|---|---|---|---|---|---|
| text-prose | xz | preset 9, 1 thread | in-process | 148205637 | 148244992 | 25.68% | 25.68% | 1.3 | 104.9 | verified |
| text-prose | zstd | level 19, library defaults, 1 thread | in-process | 148205637 | 148244992 | 27.38% | 27.37% | 1.8 | 686.9 | verified |
| text-prose | xz-cli | -9 -T1 -c (stdin IN, stdout OUT) | process-wall | 148205637 | 148244992 | 25.68% | 25.68% | 1.2 | 123.5 | verified |
| text-prose | zstd-cli | -19 -T1 -q -f -o OUT IN | process-wall | 148205637 | 148244992 | 27.39% | 27.38% | 2.0 | 1133.1 | verified |
| text-prose | bsc | e IN OUT -b256 -t -T (flags from documentation, not run) | process-wall | 148205637 | 148244992 | - | - | - | - | skipped: not installed |
| text-prose | kanzi | -c -f -i IN -o OUT -j 1 -l 9 -b 64m (flags from documentation, not run) | process-wall | 148205637 | 148244992 | - | - | - | - | skipped: not installed |
| logs-text | xz | preset 9, 1 thread | in-process | 111493402 | 111497728 | 7.11% | 7.11% | 3.5 | 311.1 | verified |
| logs-text | zstd | level 19, library defaults, 1 thread | in-process | 111493402 | 111497728 | 7.00% | 7.00% | 2.6 | 1683.0 | verified |
| logs-text | xz-cli | -9 -T1 -c (stdin IN, stdout OUT) | process-wall | 111493402 | 111497728 | 7.11% | 7.11% | 3.9 | 354.1 | verified |
| logs-text | zstd-cli | -19 -T1 -q -f -o OUT IN | process-wall | 111493402 | 111497728 | 6.99% | 6.99% | 2.9 | 2424.1 | verified |
| logs-text | bsc | e IN OUT -b256 -t -T (flags from documentation, not run) | process-wall | 111493402 | 111497728 | - | - | - | - | skipped: not installed |
| logs-text | kanzi | -c -f -i IN -o OUT -j 1 -l 9 -b 64m (flags from documentation, not run) | process-wall | 111493402 | 111497728 | - | - | - | - | skipped: not installed |
| small-files | xz | preset 9, 1 thread | in-process | 66170985 | 81995264 | 5.96% | 4.81% | 5.0 | 375.6 | verified |
| small-files | zstd | level 19, library defaults, 1 thread | in-process | 66170985 | 81995264 | 7.15% | 5.77% | 3.5 | 1903.1 | verified |
| small-files | xz-cli | -9 -T1 -c (stdin IN, stdout OUT) | process-wall | 66170985 | 81995264 | 5.96% | 4.81% | 5.8 | 397.7 | verified |
| small-files | zstd-cli | -19 -T1 -q -f -o OUT IN | process-wall | 66170985 | 81995264 | 7.10% | 5.73% | 3.8 | 2153.7 | verified |
| small-files | bsc | e IN OUT -b256 -t -T (flags from documentation, not run) | process-wall | 66170985 | 81995264 | - | - | - | - | skipped: not installed |
| small-files | kanzi | -c -f -i IN -o OUT -j 1 -l 9 -b 64m (flags from documentation, not run) | process-wall | 66170985 | 81995264 | - | - | - | - | skipped: not installed |
| backup-versions | xz | preset 9, 1 thread | in-process | 25541629 | 27096576 | 7.35% | 6.93% | 5.1 | 307.8 | verified |
| backup-versions | zstd | level 19, library defaults, 1 thread | in-process | 25541629 | 27096576 | 21.58% | 20.34% | 4.4 | 1460.9 | verified |
| backup-versions | xz-cli | -9 -T1 -c (stdin IN, stdout OUT) | process-wall | 25541629 | 27096576 | 7.35% | 6.93% | 6.2 | 412.2 | verified |
| backup-versions | zstd-cli | -19 -T1 -q -f -o OUT IN | process-wall | 25541629 | 27096576 | 21.59% | 20.35% | 4.9 | 1177.0 | verified |
| backup-versions | bsc | e IN OUT -b256 -t -T (flags from documentation, not run) | process-wall | 25541629 | 27096576 | - | - | - | - | skipped: not installed |
| backup-versions | kanzi | -c -f -i IN -o OUT -j 1 -l 9 -b 64m (flags from documentation, not run) | process-wall | 25541629 | 27096576 | - | - | - | - | skipped: not installed |

- skipped: xwrt-style word-replacement pre-pass: XWRT is GPL-2.0 and no permissively licensed implementation of a word-replacement pre-pass was found; not written here

