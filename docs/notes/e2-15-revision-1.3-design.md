# Revision 1.3: `bwt` (0x0003) gets its decoding — design (E2-15 after D-73; E2-16 may join)

Drafted 2026-10-17 by a read-only planning agent against `main` 8aaa72e and `task/e2-15-probe` (D-73, PR #72). Read with
spec sections 2, 7, 8, 9, 11 and 16, decisions D-05, D-41, D-49, D-67 to D-69 and D-73, and the designs
`e2-10-11-design.md` and `e2-16-design.md`. Sizes and times come from `bench/results/2026-10-17-megatron-16/probe-bwt.md`;
percentages and memory figures are arithmetic on those rows and on the formulas below. Nothing is decided until the D-entry.

**The orchestrator's rulings on section 7's questions (2026-10-17, ruling R21; the owner may overturn the first before
revision 1.3's text is final, see `docs/STATUS.md`):** (1) the entropy stage is `[lzma, bwt]` — option (a); the folded
coder (b2) is not taken: it would freeze an untuned coder in v1 for the gain section 1 prices, and (b) stays recorded as
v2's `[coder, bwt]`; (2) BWT on by default in Balanced (Fast never uses it); (3) a new error class `BwtError` for the newly
decoded primitive is additive within D-49 and recorded in the D-entry; (4) the revision text waits for E2-16's gate only
while E2-16's probe runs.

## 0. In seven lines
1. Minor 3 decodes `bwt` after the existing `lzma` (`[lzma, bwt]`). There is no coder primitive, because v1 has no ID for
   one.
2. Input: `out_len`, then ⌈out_len/B⌉ BWT blocks (B = `block_size`), all full except the last. Each block holds derived
   row indices and the transformed bytes.
3. The text writes out the transform, its inverse and their checks. libsais is cited only as information.
4. The step before `bwt` is bounded by I(O, B) = 8 + O + 128·⌈O/B⌉, with one level of look-ahead to `deflate-reconstruct`.
5. Permanent term: I(O, B) + 4·min(O, B) + 1 MiB, compared with the decode memory left after `max_block_plain` (`Refused`).
6. Balanced blocks hinted `Text` get a third candidate in the four-stripe trial. Fast and the peel never use BWT.
7. If E2-16's gate opens, `png-filter` joins minor 3 with its own vectors and goes through the same lpk-check gate.

## 1. The entropy stage: `[lzma, bwt]` only
**There is no ID for a new primitive.** The registry lists 0x0000–0x000C (the last is `container-reconstruct`, 0x000C);
0x000D–0x7FFF are "reserved for later versions of this spec", and 0x8000+ are never emitted. Section 2 lets a revision
decode only "primitives the registry already lists", and D-41 froze the registry. D-49 (1) relies on earlier readers
reporting `UnimplementedPrimitive`; an unlisted ID is `UnknownPrimitive` at graph parse, and no revision may change that
outcome. So candidate (b)'s coder can enter v1 only folded into `bwt`'s own input layout ("(b2)"); as a separate
primitive it can come only in v2.

| class | (c') LZMA, Balanced blocks | (c) LZMA, 64 MiB | (a) bwt+lzma | (b) bwt+ari | (a) vs smaller LZMA | (b) vs (a) |
|---|---|---|---|---|---|---|
| text-prose | 38,023,384 | 38,844,551 | 37,044,622 | 35,876,961 | −978,762 (−2.57%) | −1,167,661 (−3.15%) |
| logs-text | 7,254,691 | 7,254,388 | 6,564,062 | 6,322,582 | −690,326 (−9.52%) | −241,480 (−3.68%) |

Over both classes, (a) saves 1,669,088 bytes against the smaller LZMA rows (3.69%) and (b) saves 3,078,229 (6.80%), so
(a) gets 54% of (b)'s gain (46% on text-prose, 74% on logs-text). Times, one thread, text-prose / logs-text: compress (a)
82.19 / 24.68 s, (b) 7.18 / 3.84 s, LZMA 123.01 / 64.21 s; decompress (a) 5.20 / 2.70 s, (b) 4.84 / 2.37 s, LZMA 2.18 /
0.66 s (the inverse, 2.93 / 2.03 s, is most of each BWT row).

**What (b2) costs.** The coder is ours: no published format or maintained library to cite under D-49 (2); the text would
give the decoder bit for bit — work on the scale of section 13's "The code" (E1-15), with the same independence gate:
MTF and the RUNA/RUNB zero-run digits; the binarisation and its context tables (8 buckets, 3 for the 128-node mantissa
trees, 8 groups); the two-rate bit model (shifts 4 and 7, clamps 64 and 65472 of 65536); the range decoder (5-byte start,
normalisation at 2^24); end rules the probe never fixed (past the end it reads zeros; the first and trailing bytes are
unchecked). D-68's independent decoder had to reread nine sentences on LZMA and zstd end conditions with LzmaSpec and RFC
8878 behind them; here nothing would be behind the text. The untuned constants would be frozen for all of v1, `bwt` would
stop being a plain transform, and a better coder (libbsc's QLFC is Apache-2.0) could never replace it in v1.

**What (a) costs.** One bound rule (1.2's input-bound hook exists) and about 40 lines of transform text; lpk-check
already decodes LZMA from LzmaSpec. The trial runs on stripes; when BWT wins, its path replaces the plain LZMA encode and
is faster on both classes. `lc`/`pb` on BWT output can be tuned with no format change. **Recommendation: (a)**; record (b)
with these figures as v2's coder primitive (`[coder, bwt]`).

## 2. Decoding `bwt` (section 8, new subsection)
**Definition.** For a text T of n ≥ 1 bytes, sort the n + 1 rotations of T·$, $ a sentinel below every byte (all
rotations distinct); rows 0..=n, row 0 is $·T. The transformed bytes are the last column without the sentinel's entry.
r(n) is the smallest power of two ≥ max(2, ⌈n/32⌉), c(n) = ⌈n/r(n)⌉ ≤ 32. index[j] is the row of the rotation starting at
j·r(n); index[0] is the primary index (its rotation ends in $; its value is in 1..=n). Example (the crate's test):
"banana" gives "annbaa", r = 2, indices 4, 6, 5. The indices are derived, not stored as a rate: any suffix-array writer
produces them, and they let a decoder run 32 interleaved walks (or 32 threads) per block for at most 128 bytes.

**Input:** `out_len` (u64 LE), then k = ⌈out_len/B⌉ BWT blocks; block j has n_j = B bytes except the last
(out_len − (k−1)·B); each block is c(n_j) indices (u32 LE) then n_j bytes. The input length is exactly
8 + out_len + 4·Σc(n_j). **Output:** the k texts, in order.

**Inverse.** L = the transformed bytes with the sentinel inserted at row index[0]; C[c] = 1 + the number of bytes below c;
LF(i) = C[L[i]] + the occurrences of L[i] in L[0..i), and LF(index[0]) = 0. Set p_n = 0 and p_k = LF(p_{k+1}). The block
is valid iff p_0 = index[0], p_k ≠ index[0] for 0 < k < n, and p_{j·r} = index[j] for all j; then T[k] = L[p_{k+1}].
Informative: a decoder walking forward from all indices at once uses the successor LF⁻¹ and reads each byte from C; for it
the conditions read: chain j ends on index[j+1], the last chain ends on row 0, and no earlier step lands on row 0. Every
decoder gives the same bytes or the same error; a walk from the primary index alone ignores damaged auxiliary indices and
does not conform.

**Checks, in order** (the first fault in stream order decides): (1) the term above min(`decode_memory`, memory) −
`max_block_plain`: `Refused`, field `decode_memory`; (2) input shorter than 8 bytes: `BwtError` `length`; (3) `out_len`
against the output bound — on the last step ≠ `plain_len` is `BlockLengthMismatch`, otherwise above the bound is
`PayloadTooLarge` (nothing allocated before; it keeps check 4's arithmetic in range); (4) the input length ≠ 8 + out_len +
4·Σc(n_j): `BwtError` `length`; (5) per block in order: an index outside 1..=n_j is `BwtError` `index`; a failed validity
condition is `BwtError` `inverse`. A BWT block's length needs no separate check: n_j ≤ B ≤ the envelope's `max_bwt_block`
(block check 5, `EnvelopeMismatch`) ≤ the reader's resource (`Refused` at open); check 1 covers the successor table
before it is allocated, check 3 the output.

**Bounds.** O is the block's `plain_len` when `bwt` is last; when the next step is `deflate-reconstruct`, that record's
`plain_len` after its checks (1)–(5) with their own bound (1.2's Bounds); otherwise `max_block_plain`. The step before
`bwt` is bounded by I(O, B) (saturating). Check 1 runs first; its error is the block's error and the step does not run. A
longer output is `PayloadTooLarge`, a shorter one fails check 4; if the step's actual bound differs from O, 1.2's rule
applies (`PayloadTooLarge` either way). The look-ahead must be in 1.3 (no later revision may change a 1.3 outcome;
without it `[M, bwt, deflate-reconstruct]` could never decode an inflated text above `max_block_plain`).

**The term (permanent).** term(O, B) = I(O, B) + 4·min(O, B) + 1048576 (64-bit saturating): the step's input, a u32
successor table of n + 1 entries for the largest block, and 1 MiB for the counts, a first-column lookup and the chain
cursors (the first column is read from the 256 counts, so L is not kept). The output is the block's own buffer, counted
in `max_block_plain`. The probe's inverse (u64 links packing the next byte, about ten bytes per block byte, D-73) does not
fit and task C rewrites it; libsais's own inverse (an int32 array of n + 1 entries plus small tables) fits. A
5-bytes-per-byte term (packed-byte layout) costs 64 MiB more per thread at defaults: not recommended. At defaults
(O = 256 MiB, B = 64 MiB) the term is 537,920,008 bytes and `decode_memory` 806,355,464 (an LZMA-only Balanced archive
declares 336,592,896).

**Other spec changes.** Registry row 0x0003, Resources: "bwt block = block_size; memory per step, declared by
`decode_memory`" (as 1.2 did for 0x0008). A new error class `BwtError { reason }` with normative reasons `length`,
`index`, `inverse`, beside `ZstdError` and `LzmaError` (ruling R21 (3)); the `Refused` and `PayloadTooLarge` rows
extended. **Written out, not cited:** the transform is a definition plus an O(n) walk; citing libsais would tie its
meaning to a C library's convention and version, and the D-49 (5) gate wants a decoder written without a library;
Burrows–Wheeler (1994) and libsais 2.10 cited as information. **Code:** the inverse and the layout in `lpk-codecs-bwt`
(safe Rust; libsais behind an `encode` feature); `lpk-core` wraps them as a `PrimitiveDecoder` registered by
`register_full_reader`; `lpk-format` and `lpk-decode` stay 1.0 readers (D-49 (4)) reporting `UnimplementedPrimitive` 3.

## 3. E2-16 joining
Both gates are decided before C's text is written. Both open: one minor (3), one Revisions sentence ("adds the decoding
of `bwt` (0x0003) and `png-filter` (0x0009) and the output bounds of the steps before them, and nothing else"), two
Bounds paragraphs sharing one look-ahead rule and one plumbing task A, separate decoding subsections, vectors and
malformed vectors, one `expected-1.3.toml`, one `CHANGES.md` entry, one D-entry, one lpk-check task E (`--revision 0..3`)
and one independence gate. The writer writes minor 3 when either stage is on; no graph it makes contains both. If E2-16's
result or the owner's margin is not in when C is ready for review, 1.3 carries `bwt` alone and `png-filter` becomes 1.4
(D-62).

## 4. The writer and the pipeline
**Which blocks.** Balanced blocks hinted `Text` (a block takes its first cluster's hint; D-05); not Fast, not
stored-by-class blocks, not the Deflate peel's part encoder in 1.3 (D-69 (6); the look-ahead keeps it open).
**Per block.** The four-stripe trial gains a third candidate on `Text` blocks (each stripe transformed as one BWT block,
then LZMA at the stripe's dictionary); the smallest sum wins, BWT only when strictly smaller than both (a tie stays
LZMA). When BWT wins: BWT blocks of B = min(`bwt_block`, `plain_len`), default `bwt_block` 64 MiB
(`DEFAULT_MAX_BWT_BLOCK`); each transformed with libsais (one thread, indices at r(n)), section 2's input written, then
LZMA with the end marker (a non-final `lzma` step must carry one); graph `[lzma {dict_for(input)}, bwt {B}]`; then the
stored-no-gain rule as today. A pure function of the block: byte-identical at 1 and N threads.
**Memory (D-67).** Compression per block in flight on a `Text` block: transform phase 4·B (the i32 suffix array; 5 bytes
per byte with the output byte) + 1 MiB (libsais) + the transformed buffer I(block_size, B); LZMA phase: the transformed
buffer + the LZMA encoder; transform every BWT block first and free the suffix array before LZMA starts (feeding blocks
to LZMA one at a time would keep the encoder beside the suffix array). These terms enter the model only when a `Text`
cluster is present (as D-69 (4)). Decoding: a block declares max(term(`plain_len`, B), I + `lzma_decode_memory(dict, I)`),
and `decode_memory` = `max_block_plain` + the largest. The extraction engine needs no change (its `step_input` counts the
input twice, conservatively).
**The rest.** `lpk a --bwt on|off`, on in Balanced; header minor 3 when on; an append onto minor < 3 runs without BWT and
says so (D-69 (2)); `BalancedSummary` gains `bwt_blocks` and their sample bytes, `-v` prints them; control setting
`balanced-no-bwt` in `bench/tools.toml` and the catalogue; candidate (b)'s code (`entropy.rs`, `rc.rs`) stays bench-only.

## 5. Vectors, the independence gate and fuzz targets
**`bwt-text.lpk`** from `crates/lpk-core/tests/bwt_vector.rs` (writer API, explicit encoders; chunk 4096, blocks 16 KiB,
archive id 0x5A, mtime 1000, minor 3, no dedup, plain blocks zstd 3): `prose.txt` `[lzma (dict 64 KiB, lc 3, lp 0, pb 2,
end marker), bwt {4096}]` (four BWT blocks per block; one block exactly `max_block_plain`, its `lzma` output passing it —
the bound); `log.txt` `[lzma, bwt {16384}]` (a short last block); `tiny.txt` (1 byte) `[bwt {1}]` (a first step, n = 1);
`runs.bin` (a long run, then all 256 byte values) `[zstd (level 3, window_log 17), bwt {8192}]`; `notes.txt.gz` (the
existing Deflate fixture) `[lzma, bwt, deflate-reconstruct]`, its inflated text above `max_block_plain` (the look-ahead);
`readme.txt`, a plain block for `files_1_0`. **Malformed:** `malformed-bwt-bound.lpk` (the `lzma` output past I:
`PayloadTooLarge` before `bwt` runs); `malformed-bwt-inverse.lpk` (one auxiliary index wrong, the primary walk valid:
`BwtError` `inverse` — a primary-only walk would miss it). **Expectations:** `expected-1.3.toml` (`files`, `verify`, the
malformed `error`s); `expected.toml` tables (`list`; `info` showing 1.3; the `_1_0` keys = `UnimplementedPrimitive` 3 for
every earlier reader; `files_1_0` = `readme.txt`); one sentence in section 8 (a graph with several primitives the reader
cannot run reports the first in step order); CONFORMANCE rows; section 11 lines. Byte identity needs the same liblzma,
libzstd, `preflate-rs`, `bitcode` and `cabac`; the BWT is unique by definition; no earlier vector changes.
**Independence gate (task E):** lpk-check adds, from the text alone and with no library, the inverse, the layout, checks
1–5, the term, the bound and its look-ahead, and `--revision 0..3`; every vector decodes and the `_1_0` keys reproduce
under 2, 1 and 0, with an attestation of every file read; both decoders agree on class and reason over crafted inputs (as
D-68's 47); every reread sentence becomes a spec fix before 1.3 is final.
**Fuzz targets (Linux):** `bwt_decode` (arbitrary input, small B and bounds; no panic, nothing allocated past the bound or
the term — a counting allocator); `bwt_roundtrip` (libsais forward, our inverse); `block_header_and_decode` with the full
reader.

## 6. Tasks (PR #72 first; A → C → D ∥ E; each one implementer, then a read-only review)
- **A — plumbing, no decoding.** `crates/lpk-format/src/decode.rs`: one-level look-ahead (before step i, ask step i+1
  for its input bound, passing step i+2's when it has one; the same change as E2-16's task A). `writer.rs`: STATUS's
  parked fix (refuse at every minor any primitive no revision decodes) and `check_bwt_steps` (`bwt` last or directly
  before a last `deflate-reconstruct`, else `BadGraph` `bwt position`). `lpk-codecs-bwt` on `main` with the `encode`
  feature. Acceptance: fake decoders on three-step chains; `bcj-x86` and the other undecoded primitives refused at minors
  0–2; every vector byte-identical; lpk-check untouched and green; fmt, clippy, nextest, deny.
- **C — revision 1.3.** Spec sections 2, 8, 9 (the informative layout and the decode-memory declaration), 11, 16;
  `CHANGES.md`; `CONFORMANCE.md`; the u32 inverse; `BwtDecoder` (`decode_in`, `input_bound`); `Bwt.revision()` 3 and
  `FormatVersion::LATEST` minor 3 landing with the text; the vectors, the fuzz targets, a draft D-entry. Acceptance: a test
  per reason plus `Refused`, `PayloadTooLarge` before the `lzma` step, `BlockLengthMismatch`; the inverse equal to a naive
  rotation sort (lengths 1–200, alphabets 1–256) and to libsais's inverse on 64 MiB of text; crafted indices refused; the
  counting-allocator peak within term + output on a 64 MiB block; the new inverse's time against the probe's on `small`'s
  text, reported; the vector to `expected-1.3.toml`, `UnimplementedPrimitive` 3 under `lpk-decode` and a 1.2 full reader.
- **D — the pipeline.** `balanced.rs`, `memory.rs`, `pipeline.rs`, `lpk-cli`, `tools.toml`, the catalogue, tests.
  Acceptance: synthetic text chooses BWT, binary data in a `Text`-hinted block chooses LZMA; byte identity at 1 and 8
  threads; sealed; an append onto a 1.2 archive says BWT is off; the D-67 plan with and without `Text` (blocks in flight
  printed); every class of `small` round-trips through the D-54 engine (and lpk-check after E); on the text classes the
  trial's choice per block against whole-block encodings both ways (an agreement count, informative). **The row that
  ticks E2-15:** release `lpk-bench run --tools lpk --profile small` with `balanced` and `balanced-no-bwt` on every
  class, extractions verified, committed, pooled; tick when `lpk/balanced` is smaller than `lpk/balanced-no-bwt` on
  `text-prose` and `logs-text`, with both rows' extraction times and peak memory and any class larger with BWT; then the
  D-NN, PLAN, STATUS.
- **E — lpk-check from the text alone** (a fresh implementer): the gate in section 5.

## 7. Risks and owner questions
**Risks:** text extraction slower (one thread 2.4× on text-prose, 4.1× on logs-text) and decode memory per thread about
2.4× LZMA's at defaults (fewer extraction workers) — the runner rows must report both; the 4-byte inverse may be slower
than the probe's; 1 MiB trial stripes may not predict 64 MiB BWT blocks against 256 MiB LZMA blocks; `Text`-hinted blocks
can carry non-text tails; the transformed buffer (+block_size) may cost a block in flight at the 4 GiB ceiling; libsais's
C code enters the shipped writer; the layout, the index rule and the term are permanent; `full` not run (the margins on
`small` are wide).
**Owner questions** (ruled for now by R21 above): (1) the entropy stage (a) as recommended, or (b2) folding the coder into
`bwt` for about 3.2% more on the text classes, frozen for v1; (2) BWT on by default in Balanced despite slower text
extraction; (3) a new error class `BwtError` within D-49; (4) hold C's text for E2-16's gate.
