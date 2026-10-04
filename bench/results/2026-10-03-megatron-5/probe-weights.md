# Probe `weights`

- corpus: `full` (manifest BLAKE3 `37fd79b4563887d48932715918eaf72e24b1bc8fe7588fd27a280e2fcc4a0cf0`)
- build `78bc9601a767` (release profile, opt-level 3) on `megatron`, 2026-10-04T01:52:51Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 295.4 s elapsed
- libraries: libzstd 1.5.7

Class `model-weights`: 1 file(s) parsed as safetensors, 1 not parsed. zstd level 19, window log 27, long-distance matching on, 1 thread(s), one compressor and decompressor reused. Variants: plain = tensor bytes as they are; byte planes = bytes split by position in the element (plane 0 is the lowest byte); rotated planes = each element rotated left by one bit first, then split (for BF16 and F32 the whole exponent is in the top plane). Gain = (plain - variant) / plain; negative means the variant is larger. Speeds are MB/s of the tensor bytes (10^6 bytes per second), each step timed alone.

## Sizes by dtype, all files

| dtype | tensors | original bytes | plain zstd bytes (of original) | byte-plane bytes (of original) | byte-plane gain | rotated-plane bytes (of original) | rotated-plane gain |
|---|---|---|---|---|---|---|---|
| BF16 | 272 | 269030016 | 209319186 (77.81%) | 181045408 (67.30%) | +13.51% | 179482350 (66.71%) | +14.25% |

## Speed by dtype, all files (MB/s)

| dtype | plain compress | plain decompress | byte-plane split+compress | byte-plane decompress+merge | rotated split+compress | rotated decompress+merge |
|---|---|---|---|---|---|---|
| BF16 | 5.4 | 860.8 | 4.8 | 2008.3 | 4.9 | 854.4 |

## Planes, all files

| dtype | plane | plane bytes | byte-plane compressed (of plane) | rotated-plane compressed (of plane) |
|---|---|---|---|---|
| BF16 | 0 | 134515008 | 134511650 (100.00%) | 134510421 (100.00%) |
| BF16 | 1 | 134515008 | 46533758 (34.59%) | 44971929 (33.43%) |

## By file

| file | bytes | tensors | whole-file zstd bytes (of file) | float tensor bytes | plain / float | byte planes / float | byte-plane gain | rotated planes / float | rotated-plane gain |
|---|---|---|---|---|---|---|---|---|---|
| model-weights/smollm2-135m/model.safetensors | 269060552 | 272 | 207333630 (77.06%) | 269030016 | 77.81% | 67.30% | +13.51% | 66.71% | +14.25% |

## Not parsed

| file | bytes | reason |
|---|---|---|
| model-weights/bert-tiny/pytorch_model.bin | 17756393 | not safetensors: ZIP archive (PyTorch checkpoint) |
