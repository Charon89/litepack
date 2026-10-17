# E2-16: PNG row filters — a probe first, revision 1.3 only if it pays (design)

Drafted 2026-10-17 by a read-only planning agent against `task/e2-10-pipeline` 5cab4dc (with D-69). Read with spec
sections 2, 8, 9, 11, 12, 16, decisions D-09, D-49, D-64, D-66, D-68, D-69 and `docs/notes/e2-10-11-design.md`.
No figure here comes from a new run; nothing is decided until the task's D-entry.

## 0. The design in six lines
1. No format change first: `lpk-bench probe png-filter` encodes each PNG stream the 1.2 peel reconstructs with the
   stage's own Balanced part encoder, filtered rows (today) against unfiltered rows plus exact side information.
2. The filter code lives in `lpk_core::peel::png` from the start, so 1.3's decoder is the code the probe verified.
3. Gate: the saving on `photo-raw-png` of `small` reaches the owner's margin. Closed: a D-entry records the negative
   result and `png-filter` stays reserved.
4. Open: revision 1.3 decodes `png-filter` (0x0009), body as reserved, in `[M, png-filter {p}, deflate-reconstruct {d}]`.
5. One new bound: the step before `png-filter` is bounded by that record's raw length.
6. If E2-15's gate opens too, both decodings make one revision 1.3.

## 1. The measurement (no format change)
**Where.** `crates/lpk-bench/src/probe/png_filter.rs`, registered beside `deflate` and `jpeg` in `probe/mod.rs`,
writing `probe-png-filter.{json,md}` like every probe (clean release build, `small`; workers capped so their encoders'
`memory_for` fits half the physical memory, as E2-20 plans). Classes: `probe deflate`'s; `photo-raw-png` is the gate,
the rest (`game-assets` above all) informative. Per file of walker kind `png` (above `MAX_FILE` skipped, as the stage
does), `lpk_core::peel::deflate::examine` with `Limits::standard()` (D-64, D-69) yields exactly the streams the stage
reconstructs (933 of 940 in `photo-raw-png` per `probe deflate`), with inflated text and corrections. Several IDAT
chunks are one stream over several spans; the undo works on the inflated text, so the split changes nothing (counted).
**Eligible** (else counted by cause, same cost on both sides): IHDR first, 13 bytes; compression and filter method 0
(the record has no field for others); a pair PNG allows (colour type 0: depth 1/2/4/8/16; 3: 1/2/4/8; 2, 4, 6: 8/16);
width, height ≥ 1; the filtered length (checked u64) equal to the inflated length; every filter byte ≤ 4.
**Inverse.** Channels 1, 3, 1, 2, 4 for colour types 0, 2, 3, 4, 6; bits = channels × depth; row bytes ⌈width × bits /
8⌉; filter unit max(1, bits / 8); `height` scanlines, or Adam7's seven passes where an empty pass has no scanline and no
filter byte; filtered length Σ(1 + row bytes), raw length Σ row bytes. Undo with filter method 0 (None, Sub, Up,
Average, Paeth with PNG's tie order), the prior being the previous raw scanline of the same pass, zeros at its start.
Rows stay packed below depth 8 and big-endian at 16; palette rows are indices; PLTE, tRNS and the rest stay framing.
Bit-exact three ways: re-filtering gives the inflated text byte for byte; each uniform layout below undoes to the raw
rows; the raw rows equal the `png` crate's (already in lpk-bench, no transformations) on every eligible PNG. A mismatch
fails the run: filtering is fully specified, so a mismatch is a bug, never a fallback.
**What is compressed.** Each column is one part through `PartEncoder::balanced(&BalancedOptions::default(),
PLAIN_TEXT_LIMIT)`, the encoder `pipeline.rs` builds for the stage: the four-stripe trial (1 MiB stripes above the 4 MiB
sample, else the whole text) between LZMA (preset 9e, BT4, nice_len 273, lc 3, lp 0, pb 2, `dict_for(len)` to 64 MiB)
and zstd level 22, the winner encoding the whole text — one text, one part, one block, as the stage writes it.
`A`: the inflated text ("preflate + model on filtered rows", today). `B(raw)`: raw rows without filter bytes, Adam7 in
pass order (the PLAN's "raw pixels"). `B(F)`, F ∈ {Sub, Up, Average, Paeth}: raw rows re-filtered with F throughout, no
filter bytes — the record fixes the step's output (`original_hash` is the filtered rows), not its input, which 1.3
fixes for good (section 2), so the admissible layouts are measured first (one for all, or one per colour type and bit
depth, which the record carries). `B(raster)`, Adam7 only: de-interlaced raw rows; pass order stays unless it matters.
**Side information, exact.** The record in the never-compressed `Records` frame: kind 2, flags 2, varint(body_len),
body_hash 32, body = width 4, height 4, bit_depth, color_type, interlace 3, varint(scanlines), one filter byte per
scanline, original_hash 32 (computed with lpk-format's `PngFilterRecord` and `stage::record_len`, made public); the
third graph step counted as 7 bytes (an upper bound for ids below 2^21). The deflate record, the block, its frame, index
entry, chunk records and sealing do not change; varint widths (a byte or two per stream) are left to the rows.
**The gate.** Per stream the stage's rule (`stage.rs`): cost = M + deflate record + 160 (`STREAM_OVERHEAD`), plus the
side information when undone, kept only below the stream's length, then the file gate (gain over 256). Three writers per
layout — today (`A`), always (`B`), per stream (the cheaper) — whose class totals predict the row differences up to
varint widths; rendered per class, per source group (Kenney, Kodak, converted photos) and per (colour type, bit depth,
interlace), with winner counts and models. Open when the best layout's per-stream saving on `photo-raw-png` reaches the
margin (question 1). Also: how close "always" comes (one encode per PNG, not two), cheap rules for the winner (all
filters 0; palette or depth below 8), undo and re-filter MB/s. Validation: an ignored corpus test asserts the "today"
writer equals `Pipeline::balanced` on `photo-raw-png` with dedup off (kept, `model_bytes`, `record_bytes`).

## 2. Revision 1.3, if the gate opens (the D-49 process)
**Section 2.** `version_minor` 3: 1.3 "adds the decoding of `png-filter` (0x0009) and the output bound of the step
before it, and nothing else" (plus `bwt` if E2-15 joins); same declaration rule; 1.0–1.2 readers accept minor 3 and
report `UnimplementedPrimitive` 9 on blocks naming it. Section 8 should say that a graph with several unrun primitives
reports the first in step order (as the reference does): 1.0/1.1 readers meet 9 and 8 in one graph for the first time.
**Bounds.** A step whose next step is `png-filter` is bounded by that record's raw length. Before it runs, the reader
applies `png-filter`'s checks (1)–(4) with `png-filter`'s own output bound: the block's `plain_len` when last; when its
next step is `deflate-reconstruct`, that record's `plain_len`, after that step's checks (1)–(5) as Bounds runs them;
otherwise `max_block_plain` (one level of look-ahead). They run again at the step with its actual bound; a length
passing one bound and not the other is `PayloadTooLarge` either way (the 1.2 precedent). Only graphs naming
`png-filter` change, and no earlier reader runs a step of one, so no earlier outcome changes.
**Decoding `png-filter`.** Input: the scanlines in PNG order, undone and without filter bytes (or the chosen layout),
raw-length bytes. Output: filter method 0 applied with `filters`, each scanline's filter byte then its filtered bytes,
by citation of the PNG specification (W3C third edition, ISO/IEC 15948), with section 1's geometry, the Adam7 table, the
predictors and Paeth's tie order written out. The body stays as section 12 reserves it. Checks: (1) record lookup;
(2) kind (`BadRecord` `kind`); (3) what the 1.0 parse rules lack — width, height ≥ 1 (`width`, `height`), the pair
(`bit_depth`), an interlaced record's scanline count and filter bytes ≤ 4 (`filters`); (4) the filtered length against
the output bound — last step, unequal to `plain_len`, `BlockLengthMismatch`; else above the bound (or past 2^64 − 1)
`PayloadTooLarge`; (5) input not the raw length (`BadRecord` `scanlines`); (6) re-filter; (7) BLAKE3 against
`original_hash` (`BadRecord` `original_hash`). (1)–(4) need only the record. Resources: none, the registry row stands —
the input is shorter than the output, which (4) bounds by the next record's `plain_len` (whose term counts it) or by
`max_block_plain`; the reference drops the raw rows before `deflate-reconstruct` runs. One image per block, no chunks
named. A writer writes the step only right before a last `deflate-reconstruct`, after one `lzma` or `zstd` step (else
`BadGraph` `png-filter not before deflate-reconstruct`); a reader decodes it anywhere.
**Elsewhere.** Section 9: "How a writer lays out a peeled PNG image (1.3, informative but exact)": per undone stream the
`deflate` record, the `png-filter` record (IHDR fields, filter bytes, `original_hash` = BLAKE3 of the inflated text),
then the block, only when the inverse reproduced the text. Section 12: kind 9 in parts-fit and in what the reference
decoder applies. Section 16: reasons `width`, `height`, `bit_depth`, `filters`, `scanlines`, `original_hash`.
`CHANGES.md`; `CONFORMANCE.md` rows, `expected-1.3.toml`, the `_1_0` keys covering every reader before 1.3.
**Writer and pipeline.** `PngFilter.revision()` 3, `FormatVersion::LATEST` minor 3, `check_png_steps` beside
`check_deflate_steps` (position, kind, check 3, filtered length equal to the deflate record's `plain_len`), refused
under minor < 3. lpk-core: the undo inside the Deflate stage by the probe's writer rule, verified before kept, the side
information in the cost; minor 3 when on; appends only onto minor 3 (D-69 (2)'s rule); `lpk a --png-filter on|off`;
control setting `balanced-no-png-filter`; summary counts by cause.
**Vectors.** `png-filter.lpk` from fixtures committed in `crates/lpk-core/tests/fixtures/png/` (no flate2 dependency):
RGB8 128×96 using all five filters over two IDATs, M `lzma`, raw rows above the 16 KiB blocks' `max_block_plain` (the
new bound); 2-bit grey Adam7 with empty passes, M `zstd`; 16-bit RGBA with Average and Paeth rows; a text file. No block
names `deflate-reconstruct` without `png-filter`, so one set of `_1_0` keys fits every earlier reader. Builder
`tests/png_filter_vector.rs`, `deflate-peel.lpk`'s settings at minor 3. `malformed-png-filter-bound.lpk`: a filtered
length above the deflate record's `plain_len`, `PayloadTooLarge` before any step. Earlier vectors byte-identical.
**Independence gate.** `lpk-check` adds the step and the bound from the text alone (the filter arithmetic from the PNG
specification, no library), `--revision 0..3`; it decodes every vector, reproduces the `_1_0` keys under 2, 1 and 0,
attests what it read; every reread sentence becomes a spec fix before 1.3 is final.
**With E2-15.** Both gates are decided before any 1.3 text. Both open: one 1.3 (minor 3 when either stage is on, one
`CHANGES.md` entry, one `expected-1.3.toml`, vectors per primitive, one lpk-check task, one D-entry). One open: 1.3
holds it; the other, opening later, is 1.4 (text-only revisions take no minor, D-62).

## 3. Tasks (P → gate → A → C → D ∥ E; implementer, then read-only review)
- **P — probe and filter code.** `crates/lpk-core/src/peel/png.rs` (new), `stage.rs` (`record_len` public),
  `crates/lpk-bench/src/probe/{png_filter.rs, mod.rs}`. Evidence: property tests over 15 pairs × 2 interlace methods ×
  widths 1–17 × heights 1–9 × random filters (undo and re-filter inverse both ways); known answers (Paeth ties, Average
  rounding, the filter-unit edge, depth 16, sub-byte rows, empty Adam7 passes, `u32::MAX` overflow); the `png` crate
  agreeing on synthetic PNGs of every pair; the release run on `small` committed with zero mismatches; the validation
  test; fmt, clippy, nextest, deny; a D-entry with the gate's figures (closed: the PLAN tick per question 2, stop).
- **A — lpk-format plumbing, no decoding.** `decode.rs`: before the step preceding `png-filter` runs, ask a following
  `deflate-reconstruct` for its input bound and pass it to `png-filter`'s `input_bound`; every other graph is asked as
  now. Evidence: fake decoders on a three-step chain (the bound passed, deflate's checks first, errors the block's);
  every vector byte-identical with unchanged outcomes; `lpk-check` untouched and green.
- **C — revision 1.3** (with `bwt` if E2-15 joins). `PngFilterDecoder` (checks, `input_bound`, registered by
  `register_full_reader`), `crates/lpk-format/src/{primitive.rs, header.rs, writer.rs}`, spec, `CHANGES.md`,
  `CONFORMANCE.md`, vectors, a `png_filter_decode` fuzz target. Evidence: a test per reason, `PayloadTooLarge` before M,
  `BlockLengthMismatch` last, every pair × interlace decoded, a three-step round trip with raw rows above
  `max_block_plain`, a counting-allocator test of such a block (decoder and D-54 engine) within `max_block_plain` plus
  the deflate term (the claim behind "Resources: none"); the vector to `expected-1.3.toml`, `UnimplementedPrimitive` 9
  under `lpk-decode` and without the new decoder, the text file extracting; the malformed vector; review; draft D-entry.
- **D — the pipeline.** `stage.rs`, `pipeline.rs`, `lpk-cli`, `bench/tools.toml`, the catalogue, tests, docs. Evidence:
  synthetic PNGs of every pair and interlace round-trip; gates (an all-None palette image not undone, a sprite undone, a
  file-gate case); minor 3; byte identity at 1 and 8 threads; sealed; a 1.2 archive appends without the undo and says
  so; every PNG of `small` round-trips (D-54 engine and, after E, `lpk-check`); the on/off size difference equal to the
  probe's within varint widths; rows `balanced` against `balanced-no-png-filter` on `photo-raw-png` and `game-assets`
  (release `lpk-bench run --tools lpk --profile small`, extractions verified, committed, pooled report); the smaller
  row sets the default ("keep whichever is smaller"); D-NN; PLAN; STATUS.
- **E — `lpk-check` from the text alone** (a fresh implementer): the independence gate above, plus both decoders
  agreeing on crafted records (class and normative reason), as D-68's 47 inputs did.

## 4. Risks
- **Likely to fail on photos.** LZMA and zstd have no 2-D predictor; in a PNG the filter is the predictor. Filtered
  photo rows are residuals near zero, which LZMA's literal coder exploits; raw pixels rarely repeat exactly — why xz
  and 7-Zip offer a Delta filter (in effect Sub) for raw bitmaps. Expect `B(raw)` above `A` on the photo sources.
- **Unfiltered-pixel coders** (paq8px's image models, cmix, JPEG XL and WebP lossless, FLIF) gain from a stronger
  predictor with context modelling, not from removing filters. v1 cannot carry one (revisions decode listed primitives
  only, and none is an image model), so a closed gate leaves the method document's "pixels → image model" a v2 question.
- **Where it may win**: synthetic images (Kenney UI and sprites, also in `game-assets`) — flat areas, repeated runs and
  rows, where per-row filter choices give equal regions different bytes and hide matches; small files, so only a
  per-stream writer collects it. The uniform layouts (one predictor, homogeneous statistics; the PNG specification
  names Paeth the likeliest best fixed filter) are the likeliest photo win, hence measured before 1.3 fixes the layout.
- **No gain by construction**: the PNG specification prefers filter None for palette and sub-8-bit images, where the
  undo drops zero bytes and pays `height` bytes; the per-stream gate must refuse them.
- **Cost, permanence, corpus**: a per-stream choice doubles the Balanced encode of PNG streams on the feeding thread
  (D-69 (6)); a revision is text, vectors, an independence gate and lasting upkeep — the margin's reason; the converted
  photos come from one encoder (the `png` crate, `Compression::Balanced`), and `full` checks a near-margin result.

## 5. Owner questions
1. **The margin** (a gate threshold): recommended, open only if the per-stream saving on `photo-raw-png` is at least 1%
   of that class's `lpk/balanced` bytes in E2-10's committed row.
2. **A closed gate and the acceptance**: no build writes the undo without 1.3; recommended, the "with" figure is the
   probe's committed prediction and the "without" row E2-10's `lpk/balanced`, recorded with the negative result.
