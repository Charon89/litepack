# Probe `jpeg`

- corpus: `full` (manifest BLAKE3 `37fd79b4563887d48932715918eaf72e24b1bc8fe7588fd27a280e2fcc4a0cf0`)
- build `78bc9601a767` (release profile, opt-level 3) on `megatron`, 2026-10-03T23:52:11Z, 24 thread(s) for size-only work, 8 library thread(s) in timed sections, 1532.8 s elapsed
- libraries: lepton_jpeg 0.5.8-0
- note: library threads: the default lap uses the library's default pool and lets it use up to max_processor_threads processor threads (fewer when the image has fewer partitions); the one-thread lap runs inline on the calling thread (SingleThreadPool, max_processor_threads 1). One untimed warm-up encode and decode precede the first file

- features (`compat_lepton_vector_write`): progressive true, reject_dqts_with_zeros true, use_16bit_dc_estimate true, use_16bit_adv_predict true, accept_invalid_dht false, stop_reading_at_eoi false, max size 16386x16386, max_partitions 8, max_processor_threads 8, max_jpeg_file_size 134217728
- one-thread lap: max_processor_threads 1 on the library's `SingleThreadPool`; the scan table counts bytes after the first EOI over 4194304 as over the limit
- class `photo-jpeg`: 3055 manifest files, 0 not starting with a JPEG SOI marker (not measured)
- class `photo-jpeg-edited`: 600 manifest files, 0 not starting with a JPEG SOI marker (not measured)

## Sizes (bytes)

A recompressed file means: the primary image recompressed by Lepton and the data after its EOI (if any) deflated by the library, verified byte for byte. A failed file is stored as-is.

| Scope | Files | Input | Recompressed files | Their input | Their bytes after EOI | of them MPF / gain-map files | After Lepton | Gain on those | Failed files | Failed bytes | After fallback | Gain overall | Failure rate |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| all | 3655 | 8809583267 | 3640 | 8771177232 | 3658033 | 14 (30963146 bytes) | 6817264082 | 22.28% | 15 | 38406035 | 6855670117 | 22.18% | 0.41% |
| photo-jpeg | 3055 | 8258908517 | 3040 | 8220502482 | 3658033 | 13 (30179366 bytes) | 6403875775 | 22.10% | 15 | 38406035 | 6442281810 | 22.00% | 0.49% |
| photo-jpeg / commons-featured-full | 3000 | 8253808127 | 2985 | 8215402092 | 3644139 | 11 (30077800 bytes) | 6399672082 | 22.10% | 15 | 38406035 | 6438078117 | 22.00% | 0.50% |
| photo-jpeg / kodak-jpeg | 48 | 4879542 | 48 | 4879542 | 0 | 0 (0 bytes) | 4043417 | 17.14% | 0 | 0 | 4043417 | 17.14% | 0.00% |
| photo-jpeg / libultrahdr-jpegs | 7 | 220848 | 7 | 220848 | 13894 | 2 (101566 bytes) | 160276 | 27.43% | 0 | 0 | 160276 | 27.43% | 0.00% |
| photo-jpeg-edited | 600 | 550674750 | 600 | 550674750 | 0 | 1 (783780 bytes) | 413388307 | 24.93% | 0 | 0 | 413388307 | 24.93% | 0.00% |
| photo-jpeg-edited / edited-full | 600 | 550674750 | 600 | 550674750 | 0 | 1 (783780 bytes) | 413388307 | 24.93% | 0 | 0 | 413388307 | 24.93% | 0.00% |

## Speed of the recompressed files, MB/s of JPEG bytes (and total seconds)

| Scope | Files | Encode, default threads | Decode, default threads | Encode, one thread | Decode, one thread | Same Lepton bytes on one thread |
|---|---|---|---|---|---|---|
| all | 3640 | 46.2 (189.795 s) | 66.7 (131.591 s) | 15.0 (585.447 s) | 14.3 (614.784 s) | 3640 / 3640 |
| photo-jpeg | 3040 | 46.9 (175.224 s) | 67.1 (122.492 s) | 15.1 (543.299 s) | 14.4 (572.773 s) | 3040 / 3040 |
| photo-jpeg / commons-featured-full | 2985 | 47.0 (174.881 s) | 67.3 (122.129 s) | 15.1 (542.931 s) | 14.4 (572.401 s) | 2985 / 2985 |
| photo-jpeg / kodak-jpeg | 48 | 15.0 (0.326 s) | 14.0 (0.347 s) | 13.9 (0.351 s) | 13.7 (0.357 s) | 48 / 48 |
| photo-jpeg / libultrahdr-jpegs | 7 | 12.5 (0.018 s) | 14.4 (0.015 s) | 13.5 (0.016 s) | 14.1 (0.016 s) | 7 / 7 |
| photo-jpeg-edited | 600 | 37.8 (14.571 s) | 60.5 (9.099 s) | 13.1 (42.148 s) | 13.1 (42.011 s) | 600 / 600 |
| photo-jpeg-edited / edited-full | 600 | 37.8 (14.571 s) | 60.5 (9.099 s) | 13.1 (42.148 s) | 13.1 (42.011 s) | 600 / 600 |

## Failures by cause, files

| Scope | progressive (disabled) | progressive (rejected by the library) | four components (CMYK) | arithmetic-coded | dimension cap | file too large (size cap) | verification mismatch | other |
|---|---|---|---|---|---|---|---|---|
| all | 0 | 12 | 0 | 0 | 0 | 0 | 0 | 3 |
| photo-jpeg | 0 | 12 | 0 | 0 | 0 | 0 | 0 | 3 |
| photo-jpeg / commons-featured-full | 0 | 12 | 0 | 0 | 0 | 0 | 0 | 3 |
| photo-jpeg / kodak-jpeg | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / libultrahdr-jpegs | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited / edited-full | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

## Failures by cause, input bytes

| Scope | progressive (disabled) | progressive (rejected by the library) | four components (CMYK) | arithmetic-coded | dimension cap | file too large (size cap) | verification mismatch | other |
|---|---|---|---|---|---|---|---|---|
| all | 0 | 27851002 | 0 | 0 | 0 | 0 | 0 | 10555033 |
| photo-jpeg | 0 | 27851002 | 0 | 0 | 0 | 0 | 0 | 10555033 |
| photo-jpeg / commons-featured-full | 0 | 27851002 | 0 | 0 | 0 | 0 | 0 | 10555033 |
| photo-jpeg / kodak-jpeg | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / libultrahdr-jpegs | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited / edited-full | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

## Failures by cause and library code, all files

| Cause | Library code | Library message | Files |
|---|---|---|---|
| other | UnsupportedJpeg | non optimal eobruns not supported (could have encoded up to 255 zero runs in a row, but only did 7 followed by 1 [default lap] | 1 |
| other | UnsupportedJpeg | non optimal eobruns not supported (could have encoded up to 31 zero runs in a row, but only did 22 followed by 2 [default lap] | 1 |
| other | UnsupportedJpeg | non optimal eobruns not supported (could have encoded up to 63 zero runs in a row, but only did 41 followed by 7 [default lap] | 1 |
| progressive (rejected by the library) | UnsupportedJpeg | progress can't have two DC first stages [default lap] | 12 |

## Marker scan, files

| Scope | baseline | extended sequential | progressive | lossless | arithmetic | differential | other frame | no frame | four components | restart interval | MPF | gain-map marker | Adobe APP14 | bytes after EOI | over limit | scan stopped early |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| all | 3316 | 0 | 339 | 0 | 0 | 0 | 0 | 0 | 0 | 1809 | 12 | 2 | 2016 | 18 | 0 | 0 |
| photo-jpeg | 2716 | 0 | 339 | 0 | 0 | 0 | 0 | 0 | 0 | 1809 | 12 | 1 | 2016 | 18 | 0 | 0 |
| photo-jpeg / commons-featured-full | 2686 | 0 | 314 | 0 | 0 | 0 | 0 | 0 | 0 | 1807 | 10 | 1 | 2016 | 15 | 0 | 0 |
| photo-jpeg / kodak-jpeg | 24 | 0 | 24 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / libultrahdr-jpegs | 6 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 2 | 0 | 0 | 3 | 0 | 0 |
| photo-jpeg-edited | 600 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited / edited-full | 600 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 |
