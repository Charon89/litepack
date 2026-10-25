# `.lpk` v1 specification: changes of the fix round

The rulings applied to `lpk-v1.md`, `CONFORMANCE.md` and the reference implementation after two independent
readings of the text and an independent decoder. One line each; "code" marks a change of the reference reader's
behaviour. No test vector byte changed.

## Rulings that change behaviour

1. A block frame whose hash fails is `HashMismatch { kind: 2 }` from extraction as from `verify`; `ChunkMismatch`
   only for an intact frame whose chunk differs (code; section 9, CONFORMANCE hashflip row).
2. Every intermediate output of a graph is bounded by the archive's envelope `max_block_plain`, not the reader's;
   a non-final `lzma` step must end with the end-of-payload marker, otherwise `LzmaError` `marker required`
   (code; section 8).
3. RFC 8878 and the LZMA SDK's `lzma-specification.txt` are the normative decoders, input consumption included;
   "end of stream" and "trailing input" defined from them (section 8).
4. zstd: the compared window is RFC 8878's `Window_Size` (`Frame_Content_Size` for single-segment frames); a frame
   without `Dictionary_ID` under a step that names a prior uses it; a frame naming a dictionary under a zero id is
   `ZstdError`; empty input with a non-zero bound is `ZstdError` `truncated` (code for the last; section 8).
5. `LISTABLE` without `ENCRYPTED` is `BadHeaderFlags` (code); kind 0 is `InvalidKind`; MUST_UNDERSTAND on a known
   kind is ignored; no frame may exist that no generation's index accounts for (sections 2, 3; test).
6. New section 1 subsection "What is authenticated" (no behaviour change).
7. Orders of checks written as the reference reader performs them: frame read, path rules, index parse, one opening
   procedure (header, key slot when encrypted, trailer, index, generation table, envelope, entry table on demand —
   the key slot is read before the trailer, as the reader does), recorded-frame reads (`BadFrameLocation` for a
   frame the bound cuts, before its hash; `HashMismatch` before the length check for a frame shorter than its
   location), block header to first step (sections 3, 4, 6, 8).
8. Section 16: one error catalogue; every rule in the text names its class (`UnsupportedMajor`,
   `ReservedHeaderBits`, `ReservedFrameBits`, `VarintTooLong`, `PayloadTooLarge`, `Truncated` with its `what`
   strings, `BadChunk`, `DamageFound`, `BadGenerationTable { reason }`, `BadKeySlot` with the key slot's length 111
   and its order, sealed payload too short or with a wrong nonce = `AuthenticationFailed` (nonce before tag), a
   keyfile given but not required is ignored, `BadParams` `length` for a wrong `params_len`).
9. Recovery: the code by reference (GF(2^16), Leopard-RS as in `reed-solomon-simd` 3.x, 64-byte interleave, the
   library's `supports` predicate; a normative description is planned); every recorded frame, recovery included,
   counts towards `max_frame_payload`; a coverage break is an unusable frame (reason `coverage`) for
   `check`/`repair`, and open reads no recovery payload; any set of enough recovery shards may rebuild; `verify`
   with the key does not read recovery frames (`check` reports them); the key slot is covered by the first group;
   the keyless procedure step by step; a frame's generation is found by its offset against the generation table
   (sections 7, 13, 14).
10. Generations: `BadGenerationTable` carries a reason (`generation`, `first start_offset`, `first first_sequence`,
    `order` at parse; `count`, `salt`, `start_offset`, `salt not zero` at open) (code: the first-entry,
    `start_offset` and zero-salt rules are new); the index ends exactly where its trailer starts (code); a
    generation-0 trailer with a non-zero `previous_trailer_offset` is `BadTrailer` (code); the trailer-chain order
    and classes (sections 6, 15). The generation count bound stays `Truncated` at 19 bytes per entry, as the reader
    does.
11. Everything else: stale "after the last recovery location" (now the generation table); `index_hash` is over the
    payload as stored; positions count the index and trailers; stream order includes the key slot; plain archives
    have no flags; well-formed UTF-8 (RFC 3629); the reference tool refuses conflicting names such as `a` with
    `a/b` (`UnsafePath`, `conflicting name`) and creates missing parents (code; section 9); a file's `chunk_count`
    is bounded by the bytes left (`Truncated`); `TrailingBytes` `archive` covers every non-truncation frame error
    right after a trailer, a hash failure included; `BadFrameLocation` names `ChunkData`/`Recovery` for block and
    recovery frames read at their locations; the all-zero key-wrap nonce has the suite's nonce length; the
    `expected.toml` schema, the tool's output formats, negative cases and old generations as gate items
    (CONFORMANCE); section 11 points to CONFORMANCE; implementation details, task and decision references and
    "about" figures removed from normative text; superscript code points given; the `memory` resource named;
    primitives 3 to 12 are reserved: a v1 writer must not emit them and a v1 reader reports
    `UnimplementedPrimitive`.

12. `verify` leaves recovery to `check` in every mode: with the key it checks frame hashes, chunks, entries and
    records; without the key the frame hashes of every frame but the recovery frames and the sealing rules, reporting
    `chunks_checked: false`; neither reads a recovery payload or fails on a damaged recovery frame; `check` and
    `repair` are the recovery commands (code for the keyless path, which used to fail with `DamageFound`; sections 9,
    13, 14, CONFORMANCE item 7).

## For the independent decoder

Changes that alter an outcome it may have chosen differently: ruling 1 (hashflip extraction), ruling 2 (non-final
lzma marker), ruling 5 (`BadHeaderFlags`), ruling 7 (key slot before trailer; `BadFrameLocation` versus
`HashMismatch` order), ruling 10 (new generation-table and trailer rules), and `TrailingBytes` for a hash failure
right after a trailer (ruling 11).

After the independent decoder's re-run: the keyless `verify` line's `<entries>` is the entry count of a listable
archive and `0` when the table is sealed (CONFORMANCE); the read of a previous trailer in the chain is bounded by
the newer index's offset (section 15, "The trailer chain").

2026-10-04: Section 16 gains the writer class `DuplicateEntry`; section 9 describes per-block graphs, adds in any
order and early block close. No reader behaviour or vector byte changed.

## 2026-10-04 — revision 1.1: jpeg-reconstruct decoding

1. `version_minor` is the revision the writer wrote under (1 for revision 1.1, whether or not a block names a 1.1
   primitive); a reader checks primitives per block; a 1.0 reader accepts a minor-1 archive and reports
   `UnimplementedPrimitive` (7) on the blocks that name `jpeg-reconstruct` (section 2; code: the writer refuses a 1.1
   primitive in a minor-0 archive).
2. `jpeg-reconstruct` decodes one Lepton stream (`lepton_jpeg` 0.5.x, `lepton_version` 0) into the primary image;
   the original is assembled from the record and checked against `original_len` and `original_hash`; errors are
   `BadRecord` with the reasons `kind`, `lepton_version`, `lepton stream`, `primary_len`, `gainmaps`, `trailing`,
   `original_hash`, `chunk order`, `nested record`, in a stated order of checks; the codec is defined by the
   library version range; `decode_memory` is an allowance (computed coefficient term plus an unmeasured fixed
   term) and an image whose term exceeds what the archive declares is `Refused` (section 8). Nesting is normative
   and the writer refuses chunk-order and nesting violations (section 12).
3. How the reference writer lays out a peeled JPEG: nested parts first, then the record, then the primary image as a
   block of its own (section 9, informative).
4. New vector `jpeg-peel.lpk` with `expected.toml` (1.0 reader: list, info, `UnimplementedPrimitive`) and
   `expected-1.1.toml` (full reader: files, verify). No byte of an earlier vector changed.

2026-10-04: revision 1.1 text clarified after the independent decoder's reading (fifteen points); no behaviour or
byte changed.

2026-10-16: the second reading of 2026-10-04 (E1-14d round 2), landed 2026-10-16; text only, no vector byte changed,
no `version_minor` change. The recovery-list end rule in section 6 as in sections 13 and 15; the outcomes of a
modified frame in section 14; lzma at the bound and `IsFinishedOK` after a marker only (a stated difference from
LzmaSpec); `MissingKeySlot` before the key slot payload limit; the keyless walk's `BadFrameLocation` `frames`; Repair
without the key; the index-sequence and section 11 sentences; CONFORMANCE items 3 and 8. No behaviour changed.

## 2026-10-16 — Specification revision of 2026-10-16: the Reed-Solomon code described normatively (E1-15)

1. Section 13 gains "The code", which defines the recovery code without the library: the field GF(2^16) modulo
   `0x1002D`, the Cantor basis b_0 .. b_15 through which a 16-bit symbol is an element, and check values.
2. Section 13 states how a shard holds symbols: in each 64-byte unit, 32 low bytes then 32 high bytes.
3. Section 13 defines the encoding map: the evaluation points w_j, the high form (m = P(r)) and the low form
   (m = P(k)) by interpolation and by their Cauchy matrix, the normative rule that chooses the form, and check values.
4. Section 13 writes out the supported-pairs predicate (`min(P(k), P(r)) + max(k, r) <= 65536`) instead of naming
   the library's `supports`.
5. Section 13 gives the additive FFT (twist factors, butterflies, both forms by the transform) as an exact,
   equivalent fast computation, marked informative (the encoding map is the definition).
6. Section 13 makes decoding normative as any inverse of the encoding map, with why the result is unique for at most
   `recovery_shards` unknown shards, and outlines the fast decoder informatively.
7. Section 2 and the status line name this revision: no vector byte changed, no reader outcome changed,
   `version_minor` is unchanged. Decoders that left repair out may now implement it from the text (CONFORMANCE
   item 7; repair stays optional). Proof: `crates/lpk-format/tests/rs_description.rs` re-derives every recovery
   shard of `recovery-groups.lpk` from the text.
8. Section 13's repair memory figure is a threshold every reader applies, whatever its decoder needs; a reader may
   also refuse, with the same `Refused` (`recovery group`), a group whose rebuild would exceed a work bound it
   documents. Section 14's `MissingKeySlot` clause drops its short-input half, which opening never reaches, and lists
   `BadFrameLocation` (`frames`) among its errors. Found by the independent decoder's author and its review.

## 2026-10-16 — Text revisions of E2-20 (D-67): no reader outcome changes

1. Section 9: a store-only archive's envelope no longer lists `threads_hint` 0; `threads_hint` is the writer's choice
   for any archive (section 7), and the reference writer now writes it for store-only archives as for any other.
   `max_window` 0, `max_bwt_block` 0 and `decode_memory` equal to `max_block_plain` stand. The pipeline-written
   vectors `jpeg-peel.lpk` and `journal-record.lpk` were regenerated (envelope `threads_hint` only). Text revision
   (E2-20, D-67).
2. Section 8, `jpeg-reconstruct` "Resources": the reference writer declares `max_block_plain` plus the largest
   working memory any of its blocks declares (an image's term, a `zstd` block's window as its decoder holds it, an
   `lzma` block's dictionary), no longer only the largest peeled image's term. Text revision (E2-20, D-67).

## 2026-10-16 — revision 1.2: deflate-reconstruct decoding

1. `version_minor` 2 for revision 1.2, with the declaration rule of 1.1: a 1.2 writer writes 2 whether or not a block
   names `deflate-reconstruct`; a 1.0 or 1.1 reader accepts minor 2, lists the archive, verifies its frames and
   records, and reports `UnimplementedPrimitive` (8) for the blocks that name the primitive (section 2; code: the
   writer refuses 0x0008 under a minor below 2).
2. `deflate-reconstruct` recreates one raw Deflate stream (RFC 1951) from its plain text and the record's
   `corrections`; the codec is defined by citing `preflate-rs` 0.7.6 exactly (`recreate_whole_deflate_stream`), with
   `library` 0 naming it; checks (1) to (8) in the reference's order, with the `BadRecord` reasons `kind`,
   `corrections`, `plain_len`, `original_len` and `original_hash`, `BlockLengthMismatch` and `PayloadTooLarge` for
   `original_len` against the step's bound, and `Refused` (`decode_memory`) for the term 8 × `plain_len` + 2 ×
   (corrections length) + 16777216, a lasting threshold with its derivation (informative); the registry row's
   resources become "memory per stream, declared by `decode_memory`"; a reader may refuse, as `corrections`, a
   recreation past the reference's stated limits (block count, predictor parameter ranges, hops above
   `max_chain`); a writer writes the step last (code: `BadGraph` `deflate-reconstruct not last`) (section 8).
3. The step before `deflate-reconstruct` is bounded by the record's `plain_len` instead of `max_block_plain`, with
   checks (1) to (5) applied before it runs, using the deflate step's own bound (the block's `plain_len` when it is
   last, `max_block_plain` otherwise), and again at the step with its actual bound, the same class either way (code;
   section 8 "Bounds", and the bound in the `lzma` section).
4. How the reference writer lays out a peeled Deflate stream: framing as plain parts first, then per stream the
   record and a block of its own whose chunks are cut per span (section 9, informative).
5. Section 12: the deflate body's `corrections` and `library` rows name section 8 and preflate-rs 0.7.6; a
   `deflate` record names no chunks, so the chunk rules have nothing to check; the nested peel the container body
   mentions does not occur while no revision specifies `container-reconstruct`; revision 1.2's full reader applies
   `deflate` records. No behaviour change. Section 16 lists the new reasons.
6. New vector `deflate-peel.lpk` (section 11) with `expected-1.2.toml` (full reader: `files`, `verify`) and a
   `["deflate-peel.lpk"]` table in `expected.toml` (a pre-1.2 reader: `list`, `info`, `UnimplementedPrimitive` (8),
   and the new key `files_1_0`, the entries such a reader extracts). No byte of an earlier vector changed.
7. Reference correction, no text change: a non-final `lzma` step whose output crosses its bound before any other
   error of its stream is `PayloadTooLarge`, as lzma rules 3 and 6 state, also when a later match reaches back
   further than the bound; the reference reported `LzmaError` ("beyond dictionary size") in that case, because
   lzma-rs passes its output on only when its buffer is full. The defect dates from 1.0 (non-final `lzma` steps
   with a bound below `dict_size`); revision 1.2's bound makes it reachable with a `deflate` record whose
   `plain_len` is below the inflated text (code: the step is decoded again to stop one byte past the bound).
8. zstd rule 7 states which class wins when a frame is damaged and its output passes the bound, at the granularity
   of a block: the output of the complete blocks before the damaged one past the bound is `PayloadTooLarge`,
   otherwise `ZstdError`; a damaged block's partial output is never counted (section 8; code: the reference
   reported `ZstdError` whenever the damage came before ruzstd released the crossing bytes).
9. Text after the review of revision 1.2: a writer MUST NOT write a `deflate` record whose recreation under the
   stated limits is refused, and the evidence behind "not expected to" is named; check (5) of both reconstruction
   steps states informatively the `needed` and `allowed` the reference reports; the term is computed in 64-bit
   arithmetic saturating at 2^64 - 1; a reader that cannot address `original_len` bytes reports `original_len`
   (informative); the bound checks run before any of the previous step's own rules (section 8).
10. New malformed vector `malformed-deflate-bound.lpk` (a `[lzma, deflate-reconstruct]` block whose record's
    `plain_len` is below the inflated text): `PayloadTooLarge` for a revision 1.2 reader (`expected-1.2.toml`), the
    `_1_0` keys in `expected.toml` for earlier readers; its CONFORMANCE row. No byte of an earlier vector changed.
11. Text gaps found by an independent decoder written from the text alone, settled (section 8): where the rules are
    silent on which of two faults decides, the first in stream order does, and on a decoding detail the normative
    decoder decides over a library ("Normative decoders"); the `lzma` input position at the bound is LzmaSpec's (5
    bytes of initialisation plus one per normalisation, up to the normalisation after the last bit of the symbol that
    reaches the bound), and the bytes after it are `trailing input` unless they are exactly the marker with the end
    condition holding; in a non-final `lzma` step, after the bound a literal or match is `PayloadTooLarge` and input
    that ends there or inside the next symbol is `marker required`, and input after a marker (wherever it comes) is
    `trailing input`; `lzma` rule 3's dictionary size is LzmaSpec's, raised to 4096 (rule 2 still compares the stored
    `dict_size`), and a refused distance that would also cross the bound is rule 3's `LzmaError`; rule 4 states the
    class when the end condition fails after the marker, input after it or not (it is the first fault); rule 5: a marker before the bound in the last step is
    `truncated` whatever follows it; rule 6: input ending inside an invalid symbol may report `truncated` (or
    `marker required`) or the decoder's text; `zstd` rule 7: a frame cut short or damaged inside its
    `Content_Checksum` has all its blocks complete, and they count; "Bounds": the bound of the pre-checks is never
    derived from a later step; `deflate-reconstruct`: `needed` saturates at 2^64 - 1, and the block-count refusal
    never occurs with the output limit (informative). Code: the reference raises a `dict_size` below 4096 to 4096
    (it refused a distance in (`dict_size`, 4096]); a non-final `lzma` step whose input ends inside the symbol after
    the bound reports `marker required` (was `truncated`); input after a marker reports `trailing input` in a
    non-final step and `truncated` for a marker before the bound in the last step (was lzma-rs's own text, or in the
    last step `PayloadTooLarge` with a length below the bound); a `zstd`
    frame cut inside its checksum counts its last block (it was counted as damaged). No byte of a vector changed.

## 2026-10-18 — revision 1.3: bwt decoding

1. `version_minor` 3 for revision 1.3, with the declaration rule of 1.1 and 1.2: a 1.3 writer writes 3 whether or
   not a block names `bwt`; a reader of revision 1.0, 1.1 or 1.2 accepts minor 3, lists the archive, verifies its
   frames and records, and reports `UnimplementedPrimitive` (3) for the blocks that name the primitive; a graph with
   several primitives the reader does not run reports the first in step order (section 2; code: the writer refuses
   0x0003 under a minor below 3, and refuses it anywhere but last or directly before a last `deflate-reconstruct`,
   `BadGraph` `bwt position`).
2. `bwt` (0x0003) decodes the Burrows-Wheeler transform, written out in the text (the sorted rotations of T·$, the
   transformed bytes without the sentinel, the derived indices at r(n) = the smallest power of two at least
   max(2, ⌈n/32⌉), c(n) = ⌈n/r(n)⌉ ≤ 32 of them per block, index[0] the primary index; the inverse through LF with
   its validity conditions, and the forward walk as an informative equivalent); Burrows and Wheeler (1994) and
   libsais 2.10 are named as information only. The step's input: `out_len` (u64 LE), then ⌈out_len/B⌉ BWT blocks of
   B = `block_size` bytes (the last shorter), each its c(n_j) indices (u32 LE) then its bytes, exactly I(out_len, B)
   = 8 + out_len + 4·Σ c(n_j) bytes. Checks (1) to (5) in the reference's order: the memory term (`Refused`,
   `decode_memory`), an input under 8 bytes (`BwtError` `length`), `out_len` against the bound (`BlockLengthMismatch`
   on the last step, `PayloadTooLarge` otherwise), the input length against I (`BwtError` `length`), and per block
   the index range (`BwtError` `index`) and the inverse's conditions (`BwtError` `inverse`); a decoder that walks
   from the primary index alone does not conform. The term term(O, B) = I(O, B) + 4·min(O, B) + 1048576 is a lasting
   threshold with its derivation; the registry row's resources become "bwt block = block_size; memory per step,
   declared by `decode_memory`" (section 8). `BwtError { reason }` is a new error class, additive within D-49
   (section 16).
3. The step before `bwt` is bounded by I(O, B) instead of `max_block_plain`, with check (1) applied before it runs
   (code; section 8 "Bounds", and the bound in the `lzma` section). The look-ahead: a bounding step's own output
   bound is found one level ahead for every graph whatever the minor (the next step's input bound when it gives one,
   asked with `plain_len` when it is last and `max_block_plain` otherwise), so `[M, bwt, deflate-reconstruct]`
   decodes an inflated text above `max_block_plain`; this replaces revision 1.2's sentence that the pre-checks'
   bound is never derived from a later step. No graph a conforming writer produces is read differently; a graph
   with a bounding step followed by another, which no writer emits, may report an error of the later step's
   pre-checks before the first step runs, with the same classes (code: task A of E2-15, PR #73).
   Clarified (R36): the step after a step is asked only when that step bounds its input; after a step that bounds
   nothing no later step is asked, and a reader of revision 1.2 keeps that revision's rule (code: the reference
   decoder's `bounds_input`).
4. How the reference writer lays out a BWT block: Balanced text blocks by a trial, `[lzma, bwt {B}]` with
   B = min(the BWT block size, `plain_len`), the term and the model step's memory declared (section 9, informative;
   the pipeline's stage is task D of E2-15).
5. New vector `bwt-text.lpk` (section 11) with `expected-1.3.toml` (full reader: `files`, `verify`) and a
   `["bwt-text.lpk"]` table in `expected.toml` (a pre-1.3 reader: `list`, `info`, `UnimplementedPrimitive` (3),
   `files_1_0`); two malformed vectors, `malformed-bwt-bound.lpk` (the `lzma` output past I(O, B):
   `PayloadTooLarge` before `bwt` runs) and `malformed-bwt-inverse.lpk` (one auxiliary index wrong, the primary walk
   valid: `BwtError` `inverse`), with their `expected-1.3.toml` and `expected.toml` tables and CONFORMANCE rows.
   No byte of an earlier vector changed.
6. Section 16 lists `BwtError` and extends the `PayloadTooLarge`, `Refused` and `BadGraph` rows (the writer's
   `undecoded primitive` of task A and `bwt position`).
7. The independent-decoder gate (D-49 item 5) is task E of E2-15: `lpk-check` adds the inverse, the layout, the
   checks, the term, the bound and its look-ahead from the text alone, with `--revision 0..3`; the revision is final
   when it passes. Passed on 2026-10-18 (D-78 (8)): every outcome of `expected-1.3.toml` at revision 3, the
   `UnimplementedPrimitive` 3 outcomes at revisions 0-2; its two points are item 3's R36 clarification.

## 2026-10-25 — Text revision of LIC-2: the correction coder stated (no outcome changes for a record a valid writer produces)

1. Section 8, `deflate-reconstruct`: a new item states the correction coder normatively (the VP8 bool coder's
   contexts, split and renormalisation, the marker, the composite methods' bit order and context index
   min(A - 1, i), and the stream tail), so the format no longer depends on the `cabac` crate's behaviour. The
   rules are what the cited library does: the reference's new coder (`lpk-codecs-vp8`, ported from `lepton_jpeg`
   0.5.8) reproduces 5,415 oracle streams byte for byte in both directions and every committed vector unchanged.
2. The informative sentence on the correction bytes and the regeneration notes of `deflate-peel.lpk` and
   `bwt-text.lpk` (section 11, CONFORMANCE) name `lpk-codecs-vp8` instead of `cabac`. No vector byte changed; no
   reader outcome changes.
3. Fix round of the same day. The coder item states how the code value aligns with the bytes (an accumulator
   written in P + 8 bits, byte 0 at the scale of the initial range), the decoder's steps, and that the marker
   symbol is decoded and its value discarded (as the cited library does; a stream whose first byte is 0x80 or more
   is read on). The coder item refuses an n-bit field wider than 64 bits. "Limits of a recreation" gains two
   permitted refusals on records no valid writer produces: a correction exponent above 32 (the unary value read
   before an n-bit field of that value minus 1 bits), refused at the 33rd one of its run, and a decoded correction
   value of 2^32 - 1, to which the library adds 1. Corrections are u32 values, so no valid writer produces these;
   the cited library's result for them depends on how it was built (an arithmetic overflow). The resource derivation
   says "correction stream" and "coder contexts" for "CABAC". No valid record is read differently.

## 2026-10-25 — revision 1.4: png-filter decoding (draft until its independence gate)

1. `version_minor` 4 for revision 1.4, with the declaration rule of 1.1 to 1.3: a 1.4 writer writes 4 whether or
   not a block names `png-filter`; a reader of revision 1.0 to 1.3 accepts minor 4, lists the archive, verifies its
   frames and records, and reports `UnimplementedPrimitive` (9) for the blocks that name the primitive (section 2;
   code: the writer refuses 0x0009 under a minor below 4, and anywhere but directly before a last
   `deflate-reconstruct`, `BadGraph` `png-filter not before deflate-reconstruct`; with the records it holds it also
   checks the `png-filter` record's kind, its image (check (3)) and that its filtered length is the `deflate`
   record's `plain_len`, `BadRecord` `filtered length`).
2. `png-filter` (0x0009) decodes: the input is a filtered text of the image under filter method 0 (any filter byte
   0 to 4 per scanline; the reference writer gives every scanline Up, the layout E2-16's probe measured best), the
   output the image filtered with the record's filter bytes; the geometry (samples per pixel, packed rows, the
   filter unit, Adam7's passes with empty passes having no scanline), the five filter types and Paeth's tie order
   are written out, the PNG specification (third edition, ISO/IEC 15948) cited normatively. Checks (1) to (7) in
   the reference's order: the record, its kind, the image (`width`, `height`, `bit_depth`, `filters`), the filtered
   length against the output bound (`BlockLengthMismatch` on the last step, `PayloadTooLarge` otherwise or past
   2^64 − 1), the input length and the input's filter bytes (`scanlines`), the output's hash (`original_hash`).
   Resources: none (the registry row stands); the record body is unchanged (section 12).
3. The step before `png-filter` is bounded by the record's filtered length instead of `max_block_plain`, with checks
   (1) to (4) applied before it runs, and `png-filter` joins the steps that bound their input in the look-ahead, so
   in `[M, png-filter, deflate-reconstruct]` the `deflate` record's `plain_len` is the `png-filter` step's bound and
   a text above `max_block_plain` decodes (section 8 "Bounds", and the bound in the `lzma` section). No graph
   without `png-filter` is read differently (code: none in the reader; task A of E2-16 pins the look-ahead on the
   real IDs).
4. How the reference writer lays out a re-filtered PNG stream: its `deflate` record, then its `png-filter` record,
   the block `[M, png-filter, deflate-reconstruct]` with M encoding the Up text, chosen per stream by the smaller
   cost with the side information counted, never for images whose filter bytes are all 0, palette images or images
   below depth 8 (section 9, informative).
5. New vector `png-filter.lpk` (section 11) with `expected-1.4.toml` (full reader: `files`, `verify`) and a
   `["png-filter.lpk"]` table in `expected.toml` (a pre-1.4 reader: `list`, `info`, `UnimplementedPrimitive` (9),
   `files_1_0`); seven malformed vectors, one per outcome: `malformed-png-filter-bound.lpk` (`PayloadTooLarge`
   before the model step runs), `-width`, `-height`, `-bit-depth`, `-filters`, `-scanlines` and `-hash`
   (`BadRecord` with that reason), with their tables and CONFORMANCE rows; `expected-1.4.toml` carries a `reason`
   key for the `BadRecord` ones. No byte of an earlier vector changed.
6. Section 16 extends the `PayloadTooLarge`, `BadGraph` and `BadRecord` rows.
7. The independent-decoder gate (D-49 item 5, D-78 (8)'s rule) is task E of E2-16: `lpk-check` adds the step, the
   bound and its look-ahead from the text alone, with `--revision 0..4`; the revision is final when it passes.
8. Fix round 1 of the review: the revision 1.4 Bounds paragraph states the `png-filter` step's output bound as the
   look-ahead gives it (the input bound of the step after it, `max_block_plain` when it gives none), as the code
   does, instead of "the `deflate` record's `plain_len`, otherwise `max_block_plain`"; new vector
   `malformed-png-filter-lookahead.lpk` (`[lzma, png-filter, bwt]`, `PayloadTooLarge` with the `bwt` step's I(O, B)
   as the bound, where `max_block_plain` would let it through).
9. Fix round 1: check (4) states the last-step case of an F past 2^64 − 1 (`BlockLengthMismatch`, as the reference
   does), and Paeth's q, pa, pb, pc are computed in signed integers wide enough for the sums.
