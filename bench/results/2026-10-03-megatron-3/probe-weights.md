# Probe `weights`

- corpus: `small` (manifest BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`)
- build `6c04dedc7d5a` (release profile, opt-level 3) on `megatron`, 2026-10-03T11:02:39Z, 24 thread(s) for size-only work, 1 library thread(s) in timed sections, 0.0 s elapsed
- libraries: libzstd 1.5.7
- note: no file of the class could be parsed as safetensors: this corpus profile cannot exercise this probe

Class `model-weights`: 0 file(s) parsed as safetensors, 1 not parsed. zstd level 19, window log 27, long-distance matching on, 1 thread(s), one compressor and decompressor reused. Variants: plain = tensor bytes as they are; byte planes = bytes split by position in the element (plane 0 is the lowest byte); rotated planes = each element rotated left by one bit first, then split (for BF16 and F32 the whole exponent is in the top plane). Gain = (plain - variant) / plain; negative means the variant is larger. Speeds are MB/s of the tensor bytes (10^6 bytes per second), each step timed alone.

This corpus profile cannot exercise this probe: no file of the class could be parsed as safetensors.

## By file

No file was parsed.

## Not parsed

| file | bytes | reason |
|---|---|---|
| model-weights/bert-tiny/pytorch_model.bin | 17756393 | not safetensors: ZIP archive (PyTorch checkpoint) |
