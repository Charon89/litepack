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
