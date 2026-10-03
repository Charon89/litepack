# Probe `jpeg`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `6c04dedc7d5a` (release profile, opt-level 3) on `megatron`, 2026-10-03T10:47:23Z, 24 thread(s) for size-only work, 8 library thread(s) in timed sections, 69.1 s elapsed
- libraries: lepton_jpeg 0.5.8-0
- note: library threads: the default lap uses the library's default pool and lets it use up to max_processor_threads processor threads (fewer when the image has fewer partitions); the one-thread lap runs inline on the calling thread (SingleThreadPool, max_processor_threads 1). One untimed warm-up encode and decode precede the first file

- features (`compat_lepton_vector_write`): progressive true, reject_dqts_with_zeros true, use_16bit_dc_estimate true, use_16bit_adv_predict true, accept_invalid_dht false, stop_reading_at_eoi false, max size 16386x16386, max_partitions 8, max_processor_threads 8, max_jpeg_file_size 134217728
- one-thread lap: max_processor_threads 1 on the library's `SingleThreadPool`; the scan table counts bytes after the first EOI over 4194304 as over the limit
- class `photo-jpeg`: 355 manifest files, 0 not starting with a JPEG SOI marker (not measured)
- class `photo-jpeg-edited`: 60 manifest files, 0 not starting with a JPEG SOI marker (not measured)

## Sizes (bytes)

A recompressed file means: the primary image recompressed by Lepton and the data after its EOI (if any) deflated by the library, verified byte for byte. A failed file is stored as-is.

| Scope | Files | Input | Recompressed files | Their input | Their bytes after EOI | of them MPF / gain-map files | After Lepton | Gain on those | Failed files | Failed bytes | After fallback | Gain overall | Failure rate |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| all | 415 | 359877873 | 413 | 358198275 | 39920 | 2 (101566 bytes) | 273964515 | 23.52% | 2 | 1679598 | 275644113 | 23.41% | 0.48% |
| photo-jpeg | 355 | 327146817 | 353 | 325467219 | 39920 | 2 (101566 bytes) | 249288192 | 23.41% | 2 | 1679598 | 250967790 | 23.29% | 0.56% |
| photo-jpeg / commons-featured-small | 300 | 322046427 | 298 | 320366829 | 26026 | 0 (0 bytes) | 245084499 | 23.50% | 2 | 1679598 | 246764097 | 23.38% | 0.67% |
| photo-jpeg / kodak-jpeg | 48 | 4879542 | 48 | 4879542 | 0 | 0 (0 bytes) | 4043417 | 17.14% | 0 | 0 | 4043417 | 17.14% | 0.00% |
| photo-jpeg / libultrahdr-jpegs | 7 | 220848 | 7 | 220848 | 13894 | 2 (101566 bytes) | 160276 | 27.43% | 0 | 0 | 160276 | 27.43% | 0.00% |
| photo-jpeg-edited | 60 | 32731056 | 60 | 32731056 | 0 | 0 (0 bytes) | 24676323 | 24.61% | 0 | 0 | 24676323 | 24.61% | 0.00% |
| photo-jpeg-edited / edited-small | 60 | 32731056 | 60 | 32731056 | 0 | 0 (0 bytes) | 24676323 | 24.61% | 0 | 0 | 24676323 | 24.61% | 0.00% |

## Speed of the recompressed files, MB/s of JPEG bytes (and total seconds)

| Scope | Files | Encode, default threads | Decode, default threads | Encode, one thread | Decode, one thread | Same Lepton bytes on one thread |
|---|---|---|---|---|---|---|
| all | 413 | 39.4 (9.096 s) | 55.0 (6.518 s) | 13.7 (26.113 s) | 13.3 (26.839 s) | 413 / 413 |
| photo-jpeg | 353 | 39.7 (8.190 s) | 54.7 (5.946 s) | 13.7 (23.739 s) | 13.3 (24.486 s) | 353 / 353 |
| photo-jpeg / commons-featured-small | 298 | 40.6 (7.886 s) | 56.8 (5.641 s) | 13.7 (23.409 s) | 13.3 (24.152 s) | 298 / 298 |
| photo-jpeg / kodak-jpeg | 48 | 17.0 (0.287 s) | 16.9 (0.289 s) | 15.6 (0.313 s) | 15.3 (0.318 s) | 48 / 48 |
| photo-jpeg / libultrahdr-jpegs | 7 | 13.0 (0.017 s) | 14.1 (0.016 s) | 13.0 (0.017 s) | 13.8 (0.016 s) | 7 / 7 |
| photo-jpeg-edited | 60 | 36.1 (0.906 s) | 57.2 (0.572 s) | 13.8 (2.374 s) | 13.9 (2.353 s) | 60 / 60 |
| photo-jpeg-edited / edited-small | 60 | 36.1 (0.906 s) | 57.2 (0.572 s) | 13.8 (2.374 s) | 13.9 (2.353 s) | 60 / 60 |

## Failures by cause, files

| Scope | progressive (disabled) | progressive (rejected by the library) | four components (CMYK) | arithmetic-coded | dimension cap | file too large (size cap) | verification mismatch | other |
|---|---|---|---|---|---|---|---|---|
| all | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / commons-featured-small | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / kodak-jpeg | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / libultrahdr-jpegs | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited / edited-small | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

## Failures by cause, input bytes

| Scope | progressive (disabled) | progressive (rejected by the library) | four components (CMYK) | arithmetic-coded | dimension cap | file too large (size cap) | verification mismatch | other |
|---|---|---|---|---|---|---|---|---|
| all | 0 | 1679598 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg | 0 | 1679598 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / commons-featured-small | 0 | 1679598 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / kodak-jpeg | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / libultrahdr-jpegs | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited / edited-small | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

## Failures by cause and library code, all files

| Cause | Library code | Library message | Files |
|---|---|---|---|
| progressive (rejected by the library) | UnsupportedJpeg | progress can't have two DC first stages [default lap] | 2 |

## Marker scan, files

| Scope | baseline | extended sequential | progressive | lossless | arithmetic | differential | other frame | no frame | four components | restart interval | MPF | gain-map marker | Adobe APP14 | bytes after EOI | over limit | scan stopped early |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| all | 355 | 0 | 60 | 0 | 0 | 0 | 0 | 0 | 0 | 159 | 2 | 0 | 184 | 5 | 0 | 0 |
| photo-jpeg | 295 | 0 | 60 | 0 | 0 | 0 | 0 | 0 | 0 | 159 | 2 | 0 | 184 | 5 | 0 | 0 |
| photo-jpeg / commons-featured-small | 265 | 0 | 35 | 0 | 0 | 0 | 0 | 0 | 0 | 157 | 0 | 0 | 184 | 2 | 0 | 0 |
| photo-jpeg / kodak-jpeg | 24 | 0 | 24 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg / libultrahdr-jpegs | 6 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 2 | 0 | 0 | 3 | 0 | 0 |
| photo-jpeg-edited | 60 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| photo-jpeg-edited / edited-small | 60 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
