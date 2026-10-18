# Probe `png-filter`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `ec002766cded` (release profile, opt-level 3) on `megatron`, 2026-10-18T05:36:50Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 118.4 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libzstd 1.5.7, preflate-rs 0.7.6
- note: undo and re-filter seconds were measured inside the parallel pass (24 workers), not alone

The E2-16 measured gate: per PNG stream the 1.2 Deflate peel reconstructs, the stage's Balanced part encoder (LZMA dictionary 67108864 bytes, zstd level 22, sample 4194304 bytes) on today's filtered rows (`A`) against the undone rows in each layout (`B`), plus the exact side information (the `png-filter` record and 7 bytes for its graph step). The stage's gates: a stream is kept below its length after M, the Deflate record and 160 bytes; a file's streams gain more than 256 bytes and at least 1/32 of the file. Writers: `today` (`A`), `always` (`B` for every eligible stream), `per stream` (the cheaper). Class bytes are the class's files' lengths less their kept gains, so the differences between writers predict the row differences up to varint widths. Files above 268435456 bytes are skipped as the stage does; 24 workers.

## Gate

`photo-raw-png`: today 113808959 bytes; best layout `up` per stream 111994341 bytes; saving 1814618 bytes (1.59% of today). The owner's margin decides.

## Layouts

| layout | what |
|---|---|
| raw | raw rows, no filter bytes (Adam7 in pass order) |
| sub | raw rows re-filtered with Sub throughout |
| up | raw rows re-filtered with Up throughout |
| average | raw rows re-filtered with Average throughout |
| paeth | raw rows re-filtered with Paeth throughout |
| raster | Adam7 images de-interlaced (raw rows otherwise) |

## Class `office-pdf`

- files 166 (PNG 0), 267862151 bytes; streams 0 (over several IDATs 0); eligible 0 (all filters None 0, palette or depth below 8 0); ineligible: none
- eligible streams: filtered 0 bytes, raw 0 bytes, side information 0 bytes; scanlines by filter: none 0, sub 0, up 0, average 0, paeth 0
- winning models: none
- undo n/a MB/s (0.00 s), re-filter n/a MB/s (0.00 s), of the filtered bytes

### Writers

| layout | today | always | always vs today | per stream | per stream vs today | undone (always) | undone (per stream) |
|---|---|---|---|---|---|---|---|
| raw | 267862151 | 267862151 | 0 (0.00%) | 267862151 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| sub | 267862151 | 267862151 | 0 (0.00%) | 267862151 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| up | 267862151 | 267862151 | 0 (0.00%) | 267862151 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| average | 267862151 | 267862151 | 0 (0.00%) | 267862151 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| paeth | 267862151 | 267862151 | 0 (0.00%) | 267862151 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| raster | 267862151 | 267862151 | 0 (0.00%) | 267862151 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |

### By source group

| group | files | streams | eligible | today | best layout | per stream | saving |
|---|---|---|---|---|---|---|---|
| arxiv-cc-by-small | 20 | 0 | 0 | 38322996 | raw | 38322996 | 0 (0.00%) |
| govdocs-small | 146 | 0 | 0 | 229539155 | raw | 229539155 | 0 (0.00%) |

## Class `photo-raw-png`

- files 950 (PNG 940), 117924515 bytes; streams 933 (over several IDATs 24); eligible 933 (all filters None 877, palette or depth below 8 844); ineligible: none
- eligible streams: filtered 119385292 bytes, raw 119297764 bytes, side information 170075 bytes; scanlines by filter: none 58287, sub 6620, up 1714, average 4504, paeth 16403
- winning models: average/lzma 424, average/zstd 509, paeth/lzma 308, paeth/zstd 625, raw/lzma 332, raw/zstd 601, sub/lzma 671, sub/zstd 262, today/lzma 329, today/zstd 604, up/lzma 394, up/zstd 539
- undo 578.0 MB/s (0.21 s), re-filter 564.1 MB/s (0.21 s), of the filtered bytes

### Writers

| layout | today | always | always vs today | per stream | per stream vs today | undone (always) | undone (per stream) |
|---|---|---|---|---|---|---|---|
| raw | 113808959 | 117283857 | +3474898 (3.05%) | 113702248 | -106711 (0.09%) | 19 | 5 (all None 1, palette or depth < 8 0) |
| sub | 113808959 | 113874150 | +65191 (0.06%) | 113432873 | -376086 (0.33%) | 35 | 13 (all None 0, palette or depth < 8 0) |
| up | 113808959 | 112474556 | -1334403 (1.17%) | 111994341 | -1814618 (1.59%) | 33 | 19 (all None 0, palette or depth < 8 0) |
| average | 113808959 | 115196409 | +1387450 (1.22%) | 113800063 | -8896 (0.01%) | 24 | 4 (all None 0, palette or depth < 8 0) |
| paeth | 113808959 | 114656943 | +847984 (0.75%) | 113799955 | -9004 (0.01%) | 26 | 2 (all None 0, palette or depth < 8 0) |
| raster | 113808959 | 117283857 | +3474898 (3.05%) | 113702248 | -106711 (0.09%) | 19 | 5 (all None 1, palette or depth < 8 0) |

### By source group

| group | files | streams | eligible | today | best layout | per stream | saving |
|---|---|---|---|---|---|---|---|
| convert-small | 20 | 10 | 10 | 98852033 | up | 97270106 | 1581927 (1.60%) |
| kenney-pixel-ui-pack | 36 | 36 | 36 | 151920 | raw | 138211 | 13709 (9.02%) |
| kenney-ui-pack | 870 | 863 | 863 | 694619 | raw | 694619 | 0 (0.00%) |
| kodak | 24 | 24 | 24 | 14110387 | up | 13877696 | 232691 (1.65%) |

### By colour type, bit depth and interlace

| group | files | streams | eligible | today | best layout | per stream | saving |
|---|---|---|---|---|---|---|---|
| ct2-d8-i0 | 36 | 36 | 36 | 33793800 | up | 31979182 | 1814618 (5.37%) |
| ct3-d1-i0 | 1 | 1 | 1 | 108 | raw | 108 | 0 (0.00%) |
| ct3-d2-i0 | 2 | 2 | 2 | 284 | raw | 284 | 0 (0.00%) |
| ct3-d4-i0 | 12 | 12 | 12 | 2638 | raw | 2638 | 0 (0.00%) |
| ct3-d8-i0 | 829 | 829 | 829 | 683850 | raw | 683850 | 0 (0.00%) |
| ct6-d8-i0 | 53 | 53 | 53 | 41938 | raw | 37368 | 4570 (10.90%) |

## Class `game-assets`

- files 4660 (PNG 1800), 52470892 bytes; streams 1782 (over several IDATs 11); eligible 1782 (all filters None 1777, palette or depth below 8 1777); ineligible: none
- eligible streams: filtered 353921893 bytes, raw 353218142 bytes, side information 863345 bytes; scanlines by filter: none 701188, sub 30, up 532, average 3, paeth 1998
- winning models: average/lzma 497, average/zstd 1285, paeth/lzma 262, paeth/zstd 1520, raw/lzma 920, raw/zstd 862, sub/lzma 392, sub/zstd 1390, today/lzma 902, today/zstd 880, up/lzma 492, up/zstd 1290
- undo 7601.5 MB/s (0.05 s), re-filter 12187.8 MB/s (0.03 s), of the filtered bytes

### Writers

| layout | today | always | always vs today | per stream | per stream vs today | undone (always) | undone (per stream) |
|---|---|---|---|---|---|---|---|
| raw | 51996817 | 52168467 | +171650 (0.33%) | 51984734 | -12083 (0.02%) | 21 | 1 (all None 0, palette or depth < 8 0) |
| sub | 51996817 | 52256023 | +259206 (0.50%) | 51989113 | -7704 (0.01%) | 5 | 1 (all None 0, palette or depth < 8 0) |
| up | 51996817 | 52262830 | +266013 (0.51%) | 51996817 | 0 (0.00%) | 5 | 0 (all None 0, palette or depth < 8 0) |
| average | 51996817 | 52270859 | +274042 (0.53%) | 51996817 | 0 (0.00%) | 5 | 0 (all None 0, palette or depth < 8 0) |
| paeth | 51996817 | 52249911 | +253094 (0.49%) | 51996817 | 0 (0.00%) | 5 | 0 (all None 0, palette or depth < 8 0) |
| raster | 51996817 | 52168467 | +171650 (0.33%) | 51984734 | -12083 (0.02%) | 21 | 1 (all None 0, palette or depth < 8 0) |

### By source group

| group | files | streams | eligible | today | best layout | per stream | saving |
|---|---|---|---|---|---|---|---|
| kenney-music-jingles | 89 | 0 | 0 | 1433222 | raw | 1433222 | 0 (0.00%) |
| kenney-nature-kit | 3618 | 1622 | 1622 | 29230230 | raw | 29230230 | 0 (0.00%) |
| kenney-platformer-kit | 777 | 160 | 160 | 13335785 | raw | 13323702 | 12083 (0.09%) |
| kenney-sci-fi-sounds | 77 | 0 | 0 | 6009542 | raw | 6009542 | 0 (0.00%) |
| kenney-voiceover-pack | 99 | 0 | 0 | 1988038 | raw | 1988038 | 0 (0.00%) |

### By colour type, bit depth and interlace

| group | files | streams | eligible | today | best layout | per stream | saving |
|---|---|---|---|---|---|---|---|
| ct3-d4-i0 | 6 | 6 | 6 | 3310 | raw | 3310 | 0 (0.00%) |
| ct3-d8-i0 | 1771 | 1771 | 1771 | 5033563 | raw | 5033563 | 0 (0.00%) |
| ct6-d8-i0 | 5 | 5 | 5 | 357265 | raw | 345182 | 12083 (3.38%) |

## Class `archives-nested`

- files 14 (PNG 0), 34862973 bytes; streams 0 (over several IDATs 0); eligible 0 (all filters None 0, palette or depth below 8 0); ineligible: none
- eligible streams: filtered 0 bytes, raw 0 bytes, side information 0 bytes; scanlines by filter: none 0, sub 0, up 0, average 0, paeth 0
- winning models: none
- undo n/a MB/s (0.00 s), re-filter n/a MB/s (0.00 s), of the filtered bytes

### Writers

| layout | today | always | always vs today | per stream | per stream vs today | undone (always) | undone (per stream) |
|---|---|---|---|---|---|---|---|
| raw | 34862973 | 34862973 | 0 (0.00%) | 34862973 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| sub | 34862973 | 34862973 | 0 (0.00%) | 34862973 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| up | 34862973 | 34862973 | 0 (0.00%) | 34862973 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| average | 34862973 | 34862973 | 0 (0.00%) | 34862973 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| paeth | 34862973 | 34862973 | 0 (0.00%) | 34862973 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| raster | 34862973 | 34862973 | 0 (0.00%) | 34862973 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |

### By source group

| group | files | streams | eligible | today | best layout | per stream | saving |
|---|---|---|---|---|---|---|---|
| built-zips | 12 | 0 | 0 | 34090526 | raw | 34090526 | 0 (0.00%) |
| gson-jar | 1 | 0 | 0 | 298435 | raw | 298435 | 0 (0.00%) |
| gutenberg-epub | 1 | 0 | 0 | 474012 | raw | 474012 | 0 (0.00%) |

## Class `software-installed`

- files 433 (PNG 0), 126846043 bytes; streams 0 (over several IDATs 0); eligible 0 (all filters None 0, palette or depth below 8 0); ineligible: none
- eligible streams: filtered 0 bytes, raw 0 bytes, side information 0 bytes; scanlines by filter: none 0, sub 0, up 0, average 0, paeth 0
- winning models: none
- undo n/a MB/s (0.00 s), re-filter n/a MB/s (0.00 s), of the filtered bytes

### Writers

| layout | today | always | always vs today | per stream | per stream vs today | undone (always) | undone (per stream) |
|---|---|---|---|---|---|---|---|
| raw | 126846043 | 126846043 | 0 (0.00%) | 126846043 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| sub | 126846043 | 126846043 | 0 (0.00%) | 126846043 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| up | 126846043 | 126846043 | 0 (0.00%) | 126846043 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| average | 126846043 | 126846043 | 0 (0.00%) | 126846043 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| paeth | 126846043 | 126846043 | 0 (0.00%) | 126846043 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |
| raster | 126846043 | 126846043 | 0 (0.00%) | 126846043 | 0 (0.00%) | 0 | 0 (all None 0, palette or depth < 8 0) |

### By source group

| group | files | streams | eligible | today | best layout | per stream | saving |
|---|---|---|---|---|---|---|---|
| 7zip-extra | 23 | 0 | 0 | 6667460 | raw | 6667460 | 0 (0.00%) |
| mingit | 373 | 0 | 0 | 95772593 | raw | 95772593 | 0 (0.00%) |
| python-embed | 37 | 0 | 0 | 24405990 | raw | 24405990 | 0 (0.00%) |

