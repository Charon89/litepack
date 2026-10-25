# vp8-cross: the oracle for the VP8 bool coder port

These files are the cross set of PLAN task LIC-2 (`docs/notes/lic-2-design.md`, sections 2 and 5, task 1): symbol
sequences and the bytes an adaptive binary arithmetic coder of the VP8 kind wrote for them. They are the oracle the
clean-room port in `crates/lpk-codecs-vp8` is checked against, in both directions: the port's writer must produce
`bytes` exactly from `ops`, and the port's reader, given `bytes`, must read back every value of `ops`.

## Provenance

Generated once on 2026-10-25 from `cabac` 0.15.0 (LGPL-3.0-or-later), used as an opaque library by a scratch program
outside this repository (never committed, never part of the workspace's dependency graph). No code of `cabac` is in
this repository; these files are data: the symbol sequences and the bytes that library wrote. Every case was decoded
again by the same library under the same operation sequence and every value read equalled the value written before
the file was written (a case that did not round-trip would have been a generator bug). Family (c) was captured from
the vendored `preflate-rs` 0.7.6 (`vendor/preflate-rs-0.7.6`) with a recording wrapper around its coder, and the
recorded operations, replayed, reproduced preflate's own bytes exactly. Two runs of the generator produced the same
file (sha256 of `cases.jsonl.gz`: `83dc43a2af04607fe12a58ba746d33ece19505a7702bf9d180e202a86405b38b`).

## File

`cases.jsonl.gz`: gzip of `cases.jsonl` (48,428,833 bytes uncompressed, above the 8 MB limit for an uncompressed
file), one JSON object per line, 5,415 lines:

```
{"id": n, "family": "a"|"b"|"c", "seed": s|null, "note": "...", "groups": [len, ...],
 "ops": [[kind, group, width, value], ...], "bytes": "<lowercase hex>"}
```

- `id`: the line number from 0.
- `family`: `"a"` random streams, `"b"` edge streams, `"c"` streams captured from preflate (see below).
- `seed`: the `u64` seed of the ChaCha8 generator (`rand_chacha` 0.9, `seed_from_u64`) that drew the case, or
  `null` for a case that is not drawn at random. It is provenance only: a replay needs nothing but `groups`, `ops`
  and `bytes`.
- `note`: present in families (b) and (c) only; a human description of the case.
- `groups`: the context model. Each entry is one group: a fresh array of `len` adaptive binary contexts per group,
  every context in its initial state at the start of each stream. Lengths used: 1, 2, 3, 4, 5, 7, 8, 16, 31, 32, 33,
  64 (preflate itself uses only 1 and 32). `groups` may list a group no operation uses, and may be empty.
- `ops`: the operations in stream order, each `[kind, group, width, value]`, `group` an index into `groups`:
  - `["bit", g, i, b]`: one binary symbol `b` (0 or 1) coded with context `i` of group `g` (`0 <= i < len`).
    Preflate's single-symbol calls always use a group of length 1, so `i = 0`; family (a) also codes single
    symbols with other contexts of longer groups.
  - `["nbits", g, w, v]`: the value `v` written as a field of `w` bits with group `g`'s contexts,
    `1 <= w <= 64`, `w` may exceed the group's length. `v` may have bits at or above position `w` set: preflate
    always passes the field's value with bit `w` set (its exponent-coded values carry their leading one there), and
    families (a) and (b) do the same in about half of their fields. Reading the field back yields
    `v mod 2^w`. A width of 0 is not in the set (preflate never passes it and the library's behaviour for it is
    not defined).
  - `["unary", g, 0, v]`: the value `v` written unary with group `g`'s contexts, `0 <= v <= len` (preflate
    passes up to `len`; `v = len` is included for every length) in `cases.jsonl.gz`, and up to `len + 70` in
    `cases-d.jsonl.gz` (family (d)). The third element is always 0. Reading back yields `v`.
- `bytes`: the complete coded stream after the writer's end-of-stream flush, lowercase hex; may be empty (an empty
  or short stream of highly probable symbols flushes to no bytes).

Values up to `2^64 - 1` occur, so parse `value` as `u64`, not as a floating-point number.

A replay: for each line, create the groups, run the writer over `ops`, finish it, compare with `bytes`; then run the
reader over `bytes` with fresh groups under the same `ops` and compare each value read (`v mod 2^w` for `nbits`).

## Families

- (a) 2,400 random streams (seeds `0x4C4943320000 + k`, `k` = 0..2399): 1 to 8 groups; 0 to 4,000 operations
  (every hundredth stream exactly 4,000, the rest log-uniform, about 3% empty) mixing the three kinds with per-stream
  weights; every context draws its symbols with its own skewed probability (0.001 to 0.999) so the adaptive
  contexts move far from their start; unary values geometric or uniform in `0..=len`; field widths mostly within
  the group's length, sometimes up to 64.
- (b) 3,002 edge streams: the empty stream (with no group and with an unused group); one symbol of each value;
  one `nbits` of every width 1 to 64 for groups of 1, 4, 16 and 32 contexts, with values 0, 1, the maximum, the
  maximum and 0 with bit `w` set; every value of widths 1 to 6 in sequence; one `unary` of 0 to `len` for every
  group length, and `len` down to 0 in sequence; runs of 100,000 operations (one symbol in one context, both values,
  each also followed by 1 and 64 of the other symbol; alternating symbols in one context; alternating contexts;
  cycling over the 32 contexts of a group; `unary` 32 and 0 in a group of 32; `nbits` of width 31 maximum and zero
  in a group of 32; `nbits` of width 40 maximum in a group of 4); every prefix (0 to 300 operations) of four fixed
  sequences (balanced symbols, symbols with probability 0.98 and 0.02 of a one, mixed kinds), so streams end in
  every coder state those sequences pass through; 400 short streams (1 to 96 symbols) with probability 0.0005,
  0.002, 0.998 or 0.9995 of a one.
- (c) 13 streams captured from preflate-rs 0.7.6's analysis (configuration: chain length 4096, plain-text limit
  64 MiB, no verification; one analysis call per stream, as `lpk-core` does): the Deflate streams of
  `crates/lpk-core/tests/fixtures/deflate/` (`gzip.gz`, `notes.txt.gz`, the IDAT streams of `png.png` and
  `image.png`, the first deflated entry of `zip.zip`, `doc.zip` and `troubled.zip`) and streams made by
  miniz_oxide (through `flate2`) from `readme.txt` (levels 1, 6, 9), `bin.bin` (level 9) and synthetic numbered
  lines (levels 2 and 4). Each group is one context or context array of preflate's correction coder, numbered in
  order of first use; preflate's widths seen here are 1 to 10 and 16.
- (d) 360 streams in a separate file, `cases-d.jsonl.gz` (same schema, `"family": "d"`, ids 5415 to 5774 continuing
  after `cases.jsonl.gz`; 1,712,711 bytes uncompressed; sha256 of the gzip
  `29b4bdc92bd43c54f0211290eb3ebf3d5751e4f6daadc86e7ebb8fdd0b6c8f5a`, identical over two runs, generated on
  2026-10-25 by the same scratch program and library): random streams of family (a)'s kind (seeds
  `0x4C494332D000 + k`, 1 to 600 operations, every tenth cut to 0 to 8 operations) with 1 to 12 `unary` operations
  inserted at random positions whose value is above the group's length, by 1 to 3 (about 30%) or 1 to 70; 2,248 such
  values in all, the largest 70 above `len`, for every group length listed above. The library wrote these without
  error and read every one back as the exact value written (no clamp to `len`); the coded stream grows with the
  excess. All 360 streams round-tripped; none was rejected. `note` gives the count of such values in the stream.
- (e) 680 reader-only streams in a separate file, `cases-e.jsonl.gz` (ids 5775 to 6454; 2,224,808 bytes
  uncompressed; sha256 of the gzip `22560d170c40be84b37911667de29bee7dfbee6e5ad422f48a85217c12b821ec`, identical over
  two runs, generated on 2026-10-25 by the same scratch program and library). These show what the library's reader
  does with byte strings its writer does not produce, so they test the reader only: there is nothing to encode.
  Each line has the schema above plus `"reader_only": true`; `bytes` is the reader's input, `ops` gives the kind,
  group and width (or context index) of each read with its fourth element always 0 (no value is written), and the
  values the library's reader returned, in order, are in `"values": [...]` (`u64`, one per operation; for `nbits` the
  `w`-bit field, for `unary` the count). A case on which the library failed or stopped would carry
  `"outcome": "error"` and no `values`; none did: the reader returned values for every operation of every case,
  including the 7 empty inputs and reads far past the end of the input. A replay creates fresh groups, runs the
  reader over `bytes` under `ops` and compares each value read with `values`. (e1) 340 streams (seeds
  `0x4C494332E100 + k`) whose first byte is `0x80` or above, so the initial marker symbol decodes as one; (e2) 340
  streams (seeds `0x4C494332E200 + k`) of arbitrary bytes. In both, 332 inputs are 1 (e1) or 0 (e2) to 64 random
  bytes with 0 to 400 operations and 8 are 4,096 random bytes with 0 to 40,000 operations (log-uniform, about 3%
  with none); 1 to 6 groups of the lengths listed above; per-stream weights for the three kinds; 106,872 values in
  all. `unary` reads on such input return values above the group's length 2,316 times, up to 907 above `len`.
