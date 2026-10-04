# Probe `text`

- corpus: `full` (manifest BLAKE3 `37fd79b4563887d48932715918eaf72e24b1bc8fe7588fd27a280e2fcc4a0cf0`)
- build `78bc9601a767` (release profile, opt-level 3) on `megatron`, 2026-10-04T00:42:03Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 4248.3 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libzstd 1.5.7, xz-cli xz (XZ Utils) 5.8.4, zstd-cli *** Zstandard CLI (64-bit) v1.5.7, by Yann Collet ***

Each class is one solid stream (a deterministic tar of its files). Sizes are compressed bytes as a percentage of the class's content bytes (the baseline runner's basis) and of the tar bytes; speeds are MB/s of the tar (10^6 bytes per second), compress and decompress each timed alone. Timing `in-process`: the library call on a stream in memory. Timing `process-wall`: the external program's wall time on files in the scratch directory, which includes its start-up and file I/O; the `xz-cli` and `zstd-cli` rows run the installed command-line tools on the same stream so the two bases can be compared. Comparisons are like-for-like only within this probe: the baseline runner runs at the machine's thread count on bsdtar's tar, these rows on one thread on this probe's tar. Every result is verified by decompressing and comparing bytes.

| class | compressor | setting | timing | content bytes | tar bytes | % of content bytes | % of tar bytes | compress MB/s | decompress MB/s | status |
|---|---|---|---|---|---|---|---|---|---|---|
| text-prose | xz | preset 9, 1 thread | in-process | 360144217 | 360192000 | 23.88% | 23.87% | 1.7 | 103.7 | verified |
| text-prose | zstd | level 19, library defaults, 1 thread | in-process | 360144217 | 360192000 | 25.92% | 25.92% | 2.5 | 844.2 | verified |
| text-prose | xz-cli | -9 -T1 -c (stdin IN, stdout OUT) | process-wall | 360144217 | 360192000 | 23.88% | 23.87% | 1.9 | 147.6 | verified |
| text-prose | zstd-cli | -19 -T1 -q -f -o OUT IN | process-wall | 360144217 | 360192000 | 25.93% | 25.93% | 2.1 | 1112.7 | verified |
| text-prose | bsc | e IN OUT -b256 -t -T (flags from documentation, not run) | process-wall | 360144217 | 360192000 | - | - | - | - | skipped: not installed |
| text-prose | kanzi | -c -f -i IN -o OUT -j 1 -l 9 -b 64m (flags from documentation, not run) | process-wall | 360144217 | 360192000 | - | - | - | - | skipped: not installed |
| logs-text | xz | preset 9, 1 thread | in-process | 2686418017 | 2686427648 | 5.29% | 5.29% | 3.5 | 370.7 | verified |
| logs-text | zstd | level 19, library defaults, 1 thread | in-process | 2686418017 | 2686427648 | 5.36% | 5.36% | 2.7 | 2334.8 | verified |
| logs-text | xz-cli | -9 -T1 -c (stdin IN, stdout OUT) | process-wall | 2686418017 | 2686427648 | 5.29% | 5.29% | 4.0 | 424.2 | verified |
| logs-text | zstd-cli | -19 -T1 -q -f -o OUT IN | process-wall | 2686418017 | 2686427648 | 5.35% | 5.35% | 2.7 | 3248.5 | verified |
| logs-text | bsc | e IN OUT -b256 -t -T (flags from documentation, not run) | process-wall | 2686418017 | 2686427648 | - | - | - | - | skipped: not installed |
| logs-text | kanzi | -c -f -i IN -o OUT -j 1 -l 9 -b 64m (flags from documentation, not run) | process-wall | 2686418017 | 2686427648 | - | - | - | - | skipped: not installed |
| small-files | xz | preset 9, 1 thread | in-process | 50016512 | 65761280 | 8.28% | 6.30% | 4.6 | 290.9 | verified |
| small-files | zstd | level 19, library defaults, 1 thread | in-process | 50016512 | 65761280 | 9.23% | 7.02% | 3.9 | 1868.5 | verified |
| small-files | xz-cli | -9 -T1 -c (stdin IN, stdout OUT) | process-wall | 50016512 | 65761280 | 8.28% | 6.30% | 5.4 | 325.1 | verified |
| small-files | zstd-cli | -19 -T1 -q -f -o OUT IN | process-wall | 50016512 | 65761280 | 9.17% | 6.98% | 4.4 | 1883.8 | verified |
| small-files | bsc | e IN OUT -b256 -t -T (flags from documentation, not run) | process-wall | 50016512 | 65761280 | - | - | - | - | skipped: not installed |
| small-files | kanzi | -c -f -i IN -o OUT -j 1 -l 9 -b 64m (flags from documentation, not run) | process-wall | 50016512 | 65761280 | - | - | - | - | skipped: not installed |
| backup-versions | xz | preset 9, 1 thread | in-process | 25541629 | 27096576 | 7.35% | 6.93% | 5.0 | 315.7 | verified |
| backup-versions | zstd | level 19, library defaults, 1 thread | in-process | 25541629 | 27096576 | 21.58% | 20.34% | 4.4 | 1438.8 | verified |
| backup-versions | xz-cli | -9 -T1 -c (stdin IN, stdout OUT) | process-wall | 25541629 | 27096576 | 7.35% | 6.93% | 6.2 | 397.9 | verified |
| backup-versions | zstd-cli | -19 -T1 -q -f -o OUT IN | process-wall | 25541629 | 27096576 | 21.59% | 20.35% | 4.9 | 1264.4 | verified |
| backup-versions | bsc | e IN OUT -b256 -t -T (flags from documentation, not run) | process-wall | 25541629 | 27096576 | - | - | - | - | skipped: not installed |
| backup-versions | kanzi | -c -f -i IN -o OUT -j 1 -l 9 -b 64m (flags from documentation, not run) | process-wall | 25541629 | 27096576 | - | - | - | - | skipped: not installed |

- skipped: xwrt-style word-replacement pre-pass: XWRT is GPL-2.0 and no permissively licensed implementation of a word-replacement pre-pass was found; not written here

