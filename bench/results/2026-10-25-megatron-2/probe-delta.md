# Probe `delta`

- corpus: `full` (manifest BLAKE3 `a52af02774b9134362467adc7b6bf273f6f89592085ce71fe51f05e10574e12d`)
- build `e9790b809a73` (release profile, opt-level 3) on `megatron`, 2026-10-25T22:40:03Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 109.8 s elapsed
- libraries: liblzma liblzma 5.8.4 (bundled by liblzma-sys 0.4.9; generic C build, no SIMD or unaligned-access paths), libsais 2.10.3, libzstd 1.5.7, ruzstd 0.9.1

- chunking: fastcdc v2020, normalization level 1, per file, 4096/65536/524288 bytes, blake3
- coder: the Balanced tier's raw LZMA1 (liblzma preset 9 extreme, BT4, nice 273, lc 3 lp 0 pb 2, dictionary min(64 MiB, next power of two of the input)); a patch is stored when the coder is not smaller
- O_block 86 bytes (frame 4 + 3 + 32, header 23 + 3 + 3, index row 5 + 3 + 3 + 1 + 3 + 3); margin 0.02 of B; zstd level 19
- ruzstd 0.9.1 raw-prefix decode: **yes** (crafted pair decoded, corpus frames 3235 of 3235 bit-exact); method: a zstd frame written against a raw prefix (libzstd ref_prefix, level 19), decoded by ruzstd::decoding::FrameDecoder. ruzstd has no raw-content dictionary constructor, so the decode relies on public but undocumented internals: a Dictionary parsed (Dictionary::decode_dict) from a libzstd-trained template, then its public fields rewritten: the FSE tables (offsets, literal lengths, match lengths) and the Huffman table emptied with their public reset(), the RLE modes cleared, dict_content set to the base, offset_hist to [1, 4, 8]; added with add_dict and forced onto the frame with force_dict (the frame has no Dictionary_ID), which copies the emptied tables, the offsets and the content into the decoder

## `backup-versions`

| root | group | rank | files | bytes |
|---|---|---|---|---|
| v1 | 0 | 0 | 658 | 8510359 |
| v2 | 0 | 1 | 658 | 8510359 |
| v3 | 0 | 2 | 658 | 8520911 |

Files in no group root: 0.

| version | paired / files | unchanged chunks / bytes | changed chunks / bytes | new chunks / bytes | X_v | X_v^adj | new coded | changed coded alone |
|---|---|---|---|---|---|---|---|---|
| v2 (g0 r1) | 652 / 658 | 701 / 8462658 | 5 / 47701 | 0 / 0 | 9413 | 230 | 0 | 11436 |
| v3 (g0 r2) | 652 / 658 | 687 / 7777838 | 20 / 743073 | 0 / 0 | 119714 | 3936 | 0 | 144829 |

| version | rule | n_v | patch bytes | Y_v | Y_v + n_v O_block | A_v | not smaller than alone | substituted | applied and equal | create MB/s | apply MB/s |
|---|---|---|---|---|---|---|---|---|---|---|---|
| v2 (r1) | P1/B1/F1 | 5 | 47772 | 458 | 888 | 888 | 0 | 0 | 5 / 5 | 57.6 | 8672.9 |
| v2 (r1) | P1/B1/F0 | 5 | 278 | 278 | 708 | 708 | 0 | 0 | 5 / 5 | 21.1 | 748.8 |
| v2 (r1) | P1/B2/F1 | 5 | 47772 | 458 | 888 | 888 | 0 | 0 | 5 / 5 | 57.6 | 8672.9 |
| v2 (r1) | P1/B2/F0 | 5 | 278 | 278 | 708 | 708 | 0 | 0 | 5 / 5 | 21.1 | 748.8 |
| v2 (r1) | P2/B1/F1 | 5 | 47772 | 458 | 888 | 888 | 0 | 0 | 5 / 5 | 57.6 | 8672.9 |
| v2 (r1) | P2/B1/F0 | 5 | 278 | 278 | 708 | 708 | 0 | 0 | 5 / 5 | 21.1 | 748.8 |
| v2 (r1) | P2/B2/F1 | 5 | 47772 | 458 | 888 | 888 | 0 | 0 | 5 / 5 | 57.6 | 8672.9 |
| v2 (r1) | P2/B2/F0 | 5 | 278 | 278 | 708 | 708 | 0 | 0 | 5 / 5 | 21.1 | 748.8 |
| v3 (r2) | P1/B1/F1 | 20 | 753062 | 18858 | 20578 | 20578 | 0 | 5 | 20 / 20 | 51.4 | 8219.8 |
| v3 (r2) | P1/B1/F0 | 20 | 13745 | 13745 | 15465 | 15465 | 0 | 5 | 20 / 20 | 12.8 | 1469.7 |
| v3 (r2) | P1/B2/F1 | 20 | 749550 | 11504 | 13224 | 13224 | 0 | 5 | 20 / 20 | 11.8 | 889.2 |
| v3 (r2) | P1/B2/F0 | 20 | 7872 | 7872 | 9592 | 9592 | 0 | 5 | 20 / 20 | 3.6 | 519.4 |
| v3 (r2) | P2/B1/F1 | 20 | 753062 | 18858 | 20578 | 20578 | 0 | 0 | 20 / 20 | 48.5 | 8050.6 |
| v3 (r2) | P2/B1/F0 | 20 | 13745 | 13745 | 15465 | 15465 | 0 | 0 | 20 / 20 | 12.7 | 1289.6 |
| v3 (r2) | P2/B2/F1 | 20 | 749550 | 11504 | 13224 | 13224 | 0 | 0 | 20 / 20 | 12.1 | 966.4 |
| v3 (r2) | P2/B2/F0 | 20 | 7872 | 7872 | 9592 | 9592 | 0 | 0 | 20 / 20 | 3.5 | 478.3 |

Rows read from: 2026-10-22-megatron, 2026-10-22-megatron-2, 2026-10-22-megatron-3; S_bal absent, B absent.

Decision: not evaluated (no --rows with S_bal and B for this class).

## `backup-versions-large`

| root | group | rank | files | bytes |
|---|---|---|---|---|
| node-v1 | 0 | 0 | 43420 | 552150264 |
| node-v2 | 0 | 1 | 43408 | 552547639 |
| node-v3 | 0 | 2 | 43387 | 554280445 |

Files in no group root: 0.

| version | paired / files | unchanged chunks / bytes | changed chunks / bytes | new chunks / bytes | X_v | X_v^adj | new coded | changed coded alone |
|---|---|---|---|---|---|---|---|---|
| node-v2 (g0 r1) | 43031 / 43408 | 45590 / 514060593 | 2032 / 26073774 | 387 / 12413272 | 3727617 | 546463 | 10997206 | 5795309 |
| node-v3 (g0 r2) | 43249 / 43387 | 47587 / 540014715 | 364 / 14198938 | 52 / 66792 | 1370404 | 113667 | 17596 | 2634093 |

| version | rule | n_v | patch bytes | Y_v | Y_v + n_v O_block | A_v | not smaller than alone | substituted | applied and equal | create MB/s | apply MB/s |
|---|---|---|---|---|---|---|---|---|---|---|---|
| node-v2 (r1) | P1/B1/F1 | 2032 | 26673778 | 1313301 | 1488053 | 12485259 | 122 | 0 | 2032 / 2032 | 31.6 | 3903.3 |
| node-v2 (r1) | P1/B1/F0 | 2032 | 978027 | 978027 | 1152779 | 12149985 | 7 | 0 | 2032 / 2032 | 10.4 | 767.1 |
| node-v2 (r1) | P1/B2/F1 | 2032 | 26488053 | 1004172 | 1178924 | 12176130 | 94 | 0 | 2032 / 2032 | 14.7 | 1674.2 |
| node-v2 (r1) | P1/B2/F0 | 2032 | 748977 | 748977 | 923729 | 11920935 | 7 | 0 | 2032 / 2032 | 5.4 | 511.5 |
| node-v2 (r1) | P2/B1/F1 | 2032 | 26673778 | 1313301 | 1488053 | 12485259 | 122 | 0 | 2032 / 2032 | 31.6 | 3903.3 |
| node-v2 (r1) | P2/B1/F0 | 2032 | 978027 | 978027 | 1152779 | 12149985 | 7 | 0 | 2032 / 2032 | 10.4 | 767.1 |
| node-v2 (r1) | P2/B2/F1 | 2032 | 26488053 | 1004172 | 1178924 | 12176130 | 94 | 0 | 2032 / 2032 | 14.7 | 1674.2 |
| node-v2 (r1) | P2/B2/F0 | 2032 | 748977 | 748977 | 923729 | 11920935 | 7 | 0 | 2032 / 2032 | 5.4 | 511.5 |
| node-v3 (r2) | P1/B1/F1 | 364 | 14456881 | 480383 | 511687 | 529283 | 20 | 87 | 364 / 364 | 37.8 | 6296.9 |
| node-v3 (r2) | P1/B1/F0 | 364 | 434474 | 434474 | 465778 | 483374 | 10 | 87 | 364 / 364 | 11.0 | 1455.3 |
| node-v3 (r2) | P1/B2/F1 | 364 | 14426380 | 365401 | 396705 | 414301 | 11 | 85 | 364 / 364 | 10.3 | 1230.7 |
| node-v3 (r2) | P1/B2/F0 | 364 | 293148 | 293148 | 324452 | 342048 | 8 | 85 | 364 / 364 | 3.3 | 560.9 |
| node-v3 (r2) | P2/B1/F1 | 364 | 14454619 | 475604 | 506908 | 524504 | 19 | 0 | 364 / 364 | 35.6 | 5252.6 |
| node-v3 (r2) | P2/B1/F0 | 364 | 430823 | 430823 | 462127 | 479723 | 10 | 0 | 364 / 364 | 10.8 | 1195.7 |
| node-v3 (r2) | P2/B2/F1 | 364 | 14426380 | 365401 | 396705 | 414301 | 11 | 0 | 364 / 364 | 9.4 | 1134.8 |
| node-v3 (r2) | P2/B2/F0 | 364 | 293148 | 293148 | 324452 | 342048 | 8 | 0 | 364 / 364 | 3.2 | 465.2 |

Rows read from: 2026-10-22-megatron, 2026-10-22-megatron-2, 2026-10-22-megatron-3; S_bal 89011072, B 77811524 (`7z/ultra`).

| quantity | bytes | of B |
|---|---|---|
| S_bal | 89011072 |  |
| B | 77811524 |  |
| 0.5 B | 38905762 |  |
| mu = 0.02 B | 1556230 |  |
| S_ord = S_bal - sum X_v + sum X_v^adj | 84573181 | 108.69% |
| S_est P1/B1/F1 | 85912791 | 110.41% |
| S_est P1/B1/F0 (reference) | 85531608 | 109.92% |
| S_est P1/B2/F1 | 85488680 | 109.87% |
| S_est P1/B2/F0 (reference) | 85161232 | 109.45% |
| S_est P2/B1/F1 | 85908012 | 110.41% |
| S_est P2/B1/F0 (reference) | 85527957 | 109.92% |
| S_est P2/B2/F1 | 85488680 | 109.87% |
| S_est P2/B2/F0 (reference) | 85161232 | 109.45% |

The decision compares the best format-1 estimate, `P1/B2/F1` (85488680 bytes), with S_ord; format 0's estimates are for reference (format 0 stays reserved).
Decision: (iii) both estimates go to the owner.

