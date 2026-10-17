# E2-10 + E2-11: the Deflate peel and containers at depth 1, as format revision 1.2 (design)

Drafted 2026-10-16 by a read-only planning agent against `main` 3906330 and recorded here by the orchestrator, for
the implementer briefs of PLAN E2-10 and E2-11. Read it with the spec's sections 2, 8, 9 and 12 and decisions D-09,
D-49, D-50, D-53, D-56, D-60 and D-62. Figures are quoted from committed reports with their sources. Nothing here is
decided until the task's D-entry records it; the owner question is in section 6.

## 0. The design in six lines
1. Revision 1.2 (header `version_minor` 2) specifies the decoding of `deflate-reconstruct` (0x0008) and nothing
   else. `container-reconstruct` (0x000C) stays reserved.
2. A peeled Deflate stream is one block with the graph `[M, deflate-reconstruct {id}]`. The model step M (the tier's
   `lzma` or `zstd`) yields the inflated text; the reconstruction step re-deflates it with the record's corrections.
   The block's `plain_len` and chunk records are those of the original compressed bytes, so verification works as
   before.
3. A container's framing (every byte of the file that is not a kept stream) is written as plain parts of the same
   entry. The entry's chunk list interleaves framing chunks and stream chunks in file order. There is no container
   record.
4. One rule is needed beyond the step itself: the output of the step before `deflate-reconstruct` is bounded by that
   record's `plain_len` instead of `max_block_plain`.
5. Containers are invisible to the format and to `lpk-check`; they are only the writer's layout (section 9,
   informative).
6. Balanced carries the Deflate peel by default; Fast has it as an option, off by default.

## 1. What the archive stores

### 1.1 What the frozen v1 text forces
- **No chunk list in the deflate record.** The body (section 12, kind 8) is `original_len`, `plain_len`,
  `corrections`, `library` (0), `original_hash`. A revision cannot add a chunk list: section 2 forbids changing the
  meaning of a byte, and 1.0/1.1 readers walk every record when they verify and on every lookup of a later id. So the
  inflated text cannot be chunks in plain blocks. It can only be the reconstruction step's input, which is the output
  of the step before it in the same block.
- **One stream per block.** A graph applies to the whole block and each step names one record, so a block holds one
  stream (just as 1.1 holds one Lepton stream per block).
- **Intermediate outputs are capped.** Section 8 "Bounds" caps every intermediate output at the envelope's
  `max_block_plain`, and section 6 makes that equal to the largest block `plain_len`. A stream block's own
  `plain_len` is the compressed length, which is smaller than the inflated text. Unless some other block is at least
  as large as the inflated text, the archive would be undecodable (`PayloadTooLarge`).
  `crates/lpk-format/src/decode.rs::decode_block_in` passes `max_block_plain` as the bound of every non-final step.
  Hence rule 4.
- **A container record cannot hold peeled members or PNG.** The container body (kind 12) holds a member's "original
  bytes" through chunks, and section 12 "Nesting" requires those chunks to lie in blocks that name no reconstruction
  primitive, so it cannot point at a peeled member. Its `format` field allows only ZIP, PDF, gzip and TAR; a new value
  would be `BadRecord` (`format`) for every 1.0/1.1 reader. (The body's note "a nested peel of a member is undone…"
  contradicts the nesting rule; 2.3 handles that.)

### 1.2 A peeled Deflate stream
The stream's bytes are exactly the spans `probe deflate`'s walker reports:

| Found in | Stream bytes (the block's plain bytes) | Framing (plain parts) |
|---|---|---|
| ZIP member, method 8 | the member's compressed data | local header, data descriptor, central directory, end record, stored members |
| gzip member | raw Deflate between the RFC 1952 header and the 8-byte trailer | header, trailer |
| zlib stream (PDF Flate stream, a scan hit) | raw Deflate between the 2-byte header and the Adler-32 | header, Adler-32, the rest of the file |
| PNG | all IDAT payloads concatenated, minus the zlib header and Adler-32 (several spans) | signature, chunk headers and CRCs, ancillary chunks |

Only the first `compressed_size` bytes that preflate consumed belong to the stream
(`trim_spans(spans, 0, deflate_len)`, as in the probe); the rest of a candidate is framing.

**Record (kind 8):** `original_len` = the stream's length; `plain_len` = its inflated length; `corrections` = the
`corrections` of `preflate_whole_deflate_stream`, run once on the whole stream with the probe's config
`{max_chain_length: 4096, plain_text_limit: 64 MiB, verify_compression: false}`; `library` = 0;
`original_hash` = BLAKE3 of the stream's bytes.

**Block:** graph `[M, deflate-reconstruct {id}]`; `plain_len` = `original_len`; chunk records cover the stream's bytes
and are cut per span, so no chunk crosses a span boundary (a PNG's IDAT framing sits between its chunks in the
entry); M's bytes are the tier's encoding of the inflated text as one block (3.5); the block's envelope `memory` is
the term of 2.2(c).

### 1.3 A peeled container file
One entry. Its parts are written in this order (the writer sorts parts by offset at `end_entry`):
1. Every framing gap, in file order, through `add_part` — the normal path: the file's cluster block, chunk dedup, the
   model.
2. For each kept stream in file order: the dedup probe (3.6), then `add_record`, then the stream's block through
   `add_parts_encoded`.

As with the JPEG peel, the first stream block closes the pending plain block, so each peeled file causes one early
close. Writing the framing first means it is never split across closes.

**Why there is no container record** (the PLAN's "the framing a reconstruction record" becomes "the framing as plain
parts"; the D-entry records the departure):
- It cannot hold what matters: peeled members (the nesting rule) or PNG (no `format` value).
- What it could hold, raw framing, is better off as chunks: ZIP local headers and the central directory repeat file
  names, and PDF object syntax is text, so both compress under the model; the `Records` frame is never compressed.
- It would hash every member twice and make extraction decode the members' blocks again to rebuild the container.
- It would add a second primitive to the text, the vectors and `lpk-check`, for no gain at depth 1.
- **Where it would pay:** routing stored members to their own class's cluster, and TAR. That needs section 12's
  nesting rule relaxed by one level (container → reconstruction block); no 1.0/1.1 reader runs 0x000C, so a later
  revision can do it. Recorded as the path forward, not built now.

### 1.4 Nested references; what depth 1 allows
- A `deflate` record names no chunks, so section 12's chunk-order and nesting rules have nothing to check for it;
  `jpeg` records are unchanged. A deflated ZIP member needs exactly one record; nothing needs two levels of records.
- **Depth 1 means:** the walker runs once, on the file's bytes as ingested. Each stream it reports is peeled with one
  `deflate-reconstruct` step. The inflated text is classified (3.5) but never walked, scanned or peeled again.
- **Not peeled in this task:** a ZIP, PNG, PDF or gzip inside a member; a JPEG inside a deflated member; stored ZIP
  members (they stay framing, as in the probe, which is why the probe's result is a lower bound); Deflate64 and other
  ZIP methods; PNG's ancillary zlib data (iCCP, zTXt, iTXt).

### 1.5 Limits of the frozen layout (state them in the D-entry; the acceptance measures them)
- **No shared model context across streams.** Each inflated text is compressed alone. The probe's B column compressed
  whole files with the plain text in place, so files with many small streams (JAR/APK, PDFs) should land above its
  B/A ratio, and one-stream files (PNG, gzip) near it.
- **A deflated member never dedups against an equal loose file**, because its inflated text is only an intermediate
  inside a block. Equal compressed streams and equal framing still dedup (3.6).
- **Per-stream cost** (by the layouts of sections 3, 6, 8, 12 and 14): the record is 85 bytes plus the corrections and
  two varints; about 160 more bytes go to the frame, the block header, the index entry, one extra chunk record and a
  sealing tag.
- **A later way out inside v1, without touching kind 8:** a model step that reaches shared context through one chunk,
  such as `delta` (`base_chunk`, E2-9's revision) placed before `deflate-reconstruct`.

## 2. Revision 1.2

### 2.1 Section 2 and the status line
- Header table: `version_minor` becomes "0, 1 for revision 1.1, or 2 for revision 1.2".
- Revisions paragraph: "Revision 1.2 (minor 2) adds the decoding of `deflate-reconstruct` (0x0008, section 8
  'Decoding `deflate-reconstruct`') and the bound of the step before it (section 8, 'Bounds'), and nothing else."
- Same declaration rule as 1.1: a 1.2 writer writes 2 whether or not any block names 0x0008 (informative: the
  reference pipeline writes 2 whenever its Deflate peel is enabled). A 1.0 or 1.1 reader accepts minor 2, lists the
  archive, verifies its frames and records, and reports `UnimplementedPrimitive` (8) for every block that names the
  primitive.
- Status line: one sentence for revision 1.2.

### 2.2 Section 8
**(a) Registry row 0x0008, Resources:** "memory per stream, declared by `decode_memory`" (was "none"; D-49 item 2
has a revision state the step's resources).

**(b) "Bounds" gains:** "Revision 1.2. A step whose next step is `deflate-reconstruct` has that step's record's
`plain_len` as its output bound instead of `max_block_plain`. Before the step runs, the reader applies checks (1) to
(5) of 'Decoding `deflate-reconstruct`' to that record. An output longer than `plain_len` is `PayloadTooLarge`; a
shorter one is reported by the reconstruction step (check 6). Every other intermediate output stays bounded by
`max_block_plain`."

**(c) New subsection "Decoding `deflate-reconstruct` (revision 1.2)":**
- **Input and output.** The input is the plain text of one Deflate stream, `plain_len` bytes: the previous step's
  output, or the encoded bytes when it is the first step. The output is the original stream, `original_len` bytes: a
  raw Deflate stream as RFC 1951 defines it (cited normatively, as RFC 8878 is for `zstd`), with no zlib or gzip
  wrapper; wrappers are bytes of the entry outside the block.
- **Codec, defined by citing the library.** `corrections` are the bytes preflate-rs 0.7.6
  (<https://github.com/microsoft/preflate-rs>, Apache-2.0) returns as `corrections` from
  `preflate_whole_deflate_stream` when the whole stream is analysed in one call. The step's output is what that
  version's `recreate_whole_deflate_stream(input, corrections)` returns. Upstream publishes no format document, so
  the version is the definition. A conforming decoder may use the library (D-49), and a later release conforms if it
  recreates the same streams from the same bytes. Informative: the bytes are a `bitcode` 0.6 encoding of the
  predictor parameters followed by a CABAC-coded correction stream (`cabac` 0.15); the reference locks bitcode 0.6.9
  and cabac 0.15.0. Unlike the Lepton citation (0.5.x), pin 0.7.6 exactly: the bitcode layout of
  `TokenPredictorParameters` may change between patch releases.
- **Corrections are not canonical.** The output is guaranteed by `original_len`, `original_hash` and the chunk hashes,
  never by the correction bytes.
- **One block holds exactly one stream.** A writer writes the step last, after exactly one `lzma` or `zstd` step; a
  reader decodes the step in any position by the rules below.
- **Order of checks, exactly as the reference performs them.** Checks 1–5 need only the record; they run before the
  previous step, or at the step itself when it is the first step.
  1. Look up the record by id (the errors of the `Records` frame).
  2. Kind must be `deflate`, otherwise `BadRecord`, `kind`.
  3. `original_len` against the step's output bound: last step — if it differs from the block's `plain_len`,
     `BlockLengthMismatch`; not the last step — if above `max_block_plain`, `PayloadTooLarge`.
  4. `corrections` longer than `original_len`: `BadRecord`, `corrections`.
  5. The memory term (below) above the decode memory left after `max_block_plain`: `Refused`, field
     `decode_memory`; nothing is decoded.
  6. The input not exactly `plain_len` bytes: `BadRecord`, `plain_len`.
  7. Recreation: the library refuses the corrections or panics — `BadRecord`, `corrections`; output longer than
     `original_len` — `BadRecord`, `original_len` (a decoder may stop as soon as it would be longer).
  8. Output shorter than `original_len`: `BadRecord`, `original_len`; BLAKE3 differs from `original_hash`:
     `BadRecord`, `original_hash`.
- **Resources.** The term is 6 × `plain_len` + 16777216 bytes: the step's input, the library's own copy of it, at
  most four bytes per plain byte for the token lists 0.7.6 keeps while recreating, and a fixed allowance for its
  tables — an allowance by construction, not a measured bound. The figure is lasting: a 1.2 reader refuses any
  archive that declares less. The writer declares `max_block_plain` plus the largest term, JPEG terms included. A
  reader compares the term with the archive's `decode_memory` (never more than its own `memory`) minus
  `max_block_plain`, exactly as for `jpeg-reconstruct`.
- **Robustness.** A decoder SHOULD stop a recreation once its output exceeds `original_len`.

**(d) "What the reference decoder runs":** `deflate-reconstruct` needs the full reader; revision 1.2's full reader is
`lpk-core`.

### 2.3 Sections 9, 11, 12 and 16
- **Section 9:** a new subsection, "How a writer lays out a peeled Deflate stream (revision 1.2, informative but
  exact)", containing 1.2 and 1.3 above: framing first, then the record, then the block; the spans per kind; chunks
  cut per span; an already-stored stream referenced with no record or block; streams not kept stay framing; the
  fallbacks of 3.4.
- **Section 11:** the vector line (2.4).
- **Section 12:** `corrections` becomes "the correction data of section 8"; `library` 0 becomes "the correction format
  of preflate-rs 0.7.6"; "How a record's parts fit together" gains "deflate: names no chunks; its plain text is the
  step's input and the stream its output, so chunk order and nesting have nothing to check"; after "Nesting": "No
  revision specifies `container-reconstruct` yet; under the nesting rule a member's chunks lie in plain blocks, so the
  nested peel mentioned in the container body's `chunks` row does not occur"; "What the reference decoder does with
  records": revision 1.2's full reader applies `deflate` records.
- **Section 16:** new `BadRecord` reasons `plain_len` and `corrections`; `original_len` now also applies to kind 8.

### 2.4 The vector `deflate-peel.lpk`, built by the reference writer
Inputs are committed fixtures in `crates/lpk-core/tests/fixtures/deflate/`, made once so the vector does not depend
on the flate2 backend: `doc.zip` (`mimetype` stored, `content.xml` about 40 KiB of XML at zlib level 6, `styles.xml`
about 8 KiB at level 9, `thumb.bin` 2 KiB pseudo-random stored); `image.png` (64×48 RGB, its zlib stream split over
three IDAT chunks); `notes.txt.gz` (one gzip member of about 20 KiB of text); `readme.txt` (generated text, not a
container).

Builder `crates/lpk-core/tests/deflate_vector.rs`: `lpk-core`'s walker and analysis, the writer API called directly
with explicit encoders following section 9's layout (no dependence on the Balanced trial). Settings: chunk size 4096
(fixed chunker), block size 16 KiB, archive id 0x5A repeated, mtime 1000, minor 2, no dedup; plain blocks zstd level 3
(the vectors' convention); `content.xml` and `image.png` as `[lzma (dict 64 KiB, lc 3, lp 0, pb 2, end marker),
deflate-reconstruct]`, `styles.xml` and `notes.txt.gz` as `[zstd (level 3, window_log 17), deflate-reconstruct]`.

It proves: both model steps as non-final steps (no earlier vector has a multi-step graph); rule 4 (`content.xml`
inflates to more than `max_block_plain`); the per-span chunk cut with PNG framing interleaved; ZIP and gzip framing as
plain chunks; a pre-1.2 reader still extracts `readme.txt`.

Expectations: `expected-1.2.toml` (new; `files` and `verify` for the full reader; checked by
`cargo test -p lpk-core --test deflate_vector`); a `["deflate-peel.lpk"]` table in `expected.toml` written by
lpk-format's conformance test (`list`; `info` with `format: 1.2`, `records: true`; `error_1_0`, `message_1_0`,
`verify_exit_1_0`, `verify_stderr_1_0` — `UnimplementedPrimitive`, ID 8; a new key `files_1_0` listing the entries a
pre-1.2 reader can extract, `readme.txt`); CONFORMANCE: a new table row, item 3 reading "for a revision 1.1 or 1.2
vector every record's `original_hash`", a paragraph saying that for this vector the `_1_0` keys describe every reader
before 1.2, the `files_1_0` key in the schema. Regenerating: `cargo test -p lpk-core --test deflate_vector --
--ignored`, then `cargo test -p lpk-format --test conformance -- --ignored`; byte identity needs the same liblzma,
libzstd, preflate-rs, bitcode and cabac versions; no byte of an earlier vector changes.

### 2.5 Negative expectation for earlier readers
`lpk-decode` (a 1.0 reader) and a 1.1 full reader both open the archive, list it, print `info` and verify every frame
and record; then `verify`, and extracting any entry with a chunk in a deflate block, report `UnimplementedPrimitive`
(8); `readme.txt` still extracts. `lpk-check --revision 1` reproduces this.

### 2.6 `CHANGES.md`: "## <date> — revision 1.2: deflate-reconstruct decoding"
1. Minor 2 and the declaration rule (code: the writer refuses 0x0008 under minor < 2). 2. The step: input, output,
codec by citation, order of checks, reasons, the memory term, `Refused`, the registry row. 3. The bound rule (code).
4. Section 9's layout (informative). 5. Section 12's clarifications (no behaviour change). 6. The vector,
`expected-1.2.toml`, the `expected.toml` table and the `files_1_0` key; no byte of an earlier vector changed.

### 2.7 What `lpk-check` implements from the text alone (D-49 item 5)
The bound check ahead of the previous step (checks 1–5 before the step that precedes a deflate step, whose bound is
`plain_len`); the step itself (checks 6–8; the only library call is `preflate_rs::recreate_whole_deflate_stream` from
`preflate-rs = "=0.7.6"`, under `catch_unwind`); `--revision <minor>` 0, 1 or 2, default 2, the old `--revision-1-0`
kept as an alias; the `files_1_0` key in its conformance test. It already parses kind-8 bodies; no record assembly,
no container logic (framing is just chunks). It must not read `lpk-format` or `lpk-core` code; it works from the spec,
the vectors, CONFORMANCE and preflate-rs's documentation.

## 3. The pipeline

### 3.1 The walker, moved (E2-11's last acceptance clause)
- Into `crates/lpk-core/src/peel/deflate/walk.rs` (moved, not rewritten): `Candidate`, `Found`, `find_streams(bytes,
  ext)`, `find_zip/png/gzip/pdf`, `scan_zlib`, `inflate_extent`, `zlib_candidate`, `valid_zlib_header`,
  `dictionary_has_flate`, `gzip_header_len`, `trim_spans`, `KINDS`, `MIN_SCANNED_PLAIN` (1024), `INFLATE_CAP` (2^32).
- Into `analyse.rs`: `analyse_stream` (its `Done` result now carries the correction bytes, not just their length),
  `code_name`, `PANIC`, `FAILURE_NAMES`, `PLAIN_TEXT_LIMIT` (64 MiB), `FILE_PLAIN_BUDGET` (512 MiB), the probe's
  `PreflateConfig`; a new `examine(bytes, ext, limits, on_done)`: the probe's `process_file` loop (sorting, overlap
  refusals, the per-file budget, outcomes, `catch_unwind` around the walker), calling back once per reconstructed
  stream instead of building the replaced file. Counts come back in a plain `lpk-core` struct, which the probe
  converts into its serde `StreamStats`.
- `lpk-bench` gains `lpk-core` as a dependency; `probe/deflate.rs` keeps only the A/B compression, validation,
  rendering and estimate labels, and calls `examine`. The walker tests move to `lpk-core`; the probe's own tests stay
  green and its end-to-end result is unchanged.
- New `lpk-core` dependencies: `zip = { version = "9.0.4", default-features = false, features = ["deflate"] }` and
  `preflate-rs = "=0.7.6"`, both allow-listed and already in the tree. `zip` brings zopfli, typed-path, indexmap and
  nix (Unix) into the product, all checked by `cargo deny`.
- flate2 backend: the walker's `inflate_extent` uses flate2, which links C zlib in the workspace build and miniz_oxide
  when `lpk-core` is built alone. The share test runs in the workspace build, as the probe did. A different backend
  can change which damaged streams become candidates, never whether an archive round-trips.

### 3.2 Per file (Balanced; Fast with `--deflate-peel on`)
1. Classes: walk `DeflateContainer` and `Png`; scan `Executable`, `Other`, `ImageRaw` and `HighEntropy` for embedded
   zlib streams (the probe scanned every non-container file of its classes; `find_streams` tells ZIP, PNG, gzip, PDF
   and scan apart by magic); never touch `Text`, `Jpeg`, `Audio`, `Video` or `Compressed`.
2. Size cap: a file above `max_file` (256 MiB) is refused unread — streamed through the normal path and counted as
   `size cap`.
3. Whole-file dedup: read the file once, whole; try `add_file_if_known` first — a whole duplicate is referenced and
   never walked.
4. Examine: `examine` applies the probe's rules unchanged. For each reconstructed stream, in order: classify the
   inflated text (its first 1 MiB); run the dedup probe (single-span streams only, via `Writer::part_known`); encode
   with the part encoder unless the stream is already stored; apply the gates (3.3). Each plain text is dropped once
   its stream is decided.
5. Write: apply the file gate, then either the write path of 1.3, or `add_file` from the buffer when nothing is kept.

### 3.3 Gates (D-09: per stream and per file)
- Stream gate: cost = M's encoded bytes + the record's exact encoded length + 160 (a fixed estimate: frame 37–40,
  block header with both steps about 19–53, index entry 13–20, one extra framing chunk record 33–35, sealing tag 16).
  Keep the stream if cost < the stream's length (the stored compressed bytes are the baseline, as with Lepton's gate).
  A stream already stored costs 0.
- Block cap: a stream longer than the tier's block size is not kept.
- Memory cap: if the memory term plus the block size exceeds the reader's default `memory` (2 GiB), the stream is not
  kept (every stream fits today with the 64 MiB plain limit; the rule is for non-default options).
- File gate: keep the file's streams only if the sum of (length − cost) over kept and already-stored streams exceeds
  256, which covers the early plain-block close and the extra framing chunk; otherwise every stream is counted "not
  kept: file gate" and the file goes whole through the normal path.
- Counting: `reconstructed` is decided before any gate, exactly as in the probe; the pipeline's outcomes (kept, no
  gain, block cap, memory cap, already stored, file gate) partition `reconstructed`.

### 3.4 Documented fallbacks (each counted)

| Case | What happens | Counted as |
|---|---|---|
| ZIP method 9 (Deflate64) or another non-Deflate method | not a candidate; stays framing | `zip_deflate64_members`, `zip_other_method_members` (informative) |
| zlib preset dictionary (FDICT) | PDF and scan: not a candidate; PNG: fails analysis | — / failed, by library name |
| unrecognised encoder | preflate fails, or its corrections eat the gain | failed by name (`NoCompressionCandidates`, `PredictionFailure`, …) / not kept: no gain |
| library error, incomplete stream, recreation mismatch | stored as found | failed, using the probe's 29 names |
| panic in the walker, analysis or recreation | caught by `catch_unwind`, as in the probe | walker panic / failed `Panic` |
| plain text over 64 MiB, or the file's 512 MiB plain budget used up | not analysed | skipped |
| file over 256 MiB | refused unread | file: size cap |

Verification by re-encoding: recreation from the plain text and corrections must reproduce the original bytes
exactly, and the plain size must equal the declared or measured size (the probe's `analyse_stream`). M is trusted
like any plain block's coder.

### 3.5 Routing to the classifier and the tiers ("member streams reach the classifier")
- Each kept stream's inflated text goes through `classify::classify` (first 1 MiB); the class steers M and is counted
  in `kept_by_class`.
- `PartEncoder` (new, in `lpk-core`): Balanced — its own `LzmaEncoder` (the tier's dictionary cap and `dict_for(len)`;
  the end marker always written) and its own zstd core, running the tier's four-stripe trial; it skips the class rule
  (`stored_by_class`) and the "stored, no gain" fallback, because the peel's own gate decides. Fast (option) — zstd at
  the tier's level and window, `Gate::OFF`, `DictionaryKind::None`. Its blocks are peel work: counted in
  `DeflateSummary`, not in the tier summaries.
- Balanced on, Fast off by default: the method document turns Peel off in Fast except for cheap unpacking;
  `probe deflate` analysed at 4.4–20.0 MB/s of Deflate input (`bench/reports/phase0-2026-10-03-small.md`, speed table)
  against Fast's 348.1 MB/s compression on `office-pdf` (`bench/results/2026-10-14-megatron`); G4 is measured with
  `lpk --fast`; the JPEG peel stays in both tiers (D-50).
- Header minor: 2 when the Deflate peel is enabled, otherwise 1 with the JPEG peel, otherwise 0.

### 3.6 Dedup (D-53)
A whole duplicate file is referenced and never walked, as with JPEG. A single-span stream whose original bytes are
all stored already (cut by the chunker) is referenced with `add_part_known`: no record, no block, counted as already
stored; analysis still runs, so the counts equal the probe's; encoding does not. Multi-span (PNG) streams get
whole-file dedup only. Framing gets the normal path's chunk dedup. Not possible: a deflated member's inflated text
against an equal loose file (1.5).

### 3.7 Parallel encoding and determinism (D-60)
The walker, the analysis, the part encoding, `add_record` and `add_parts_encoded` all run on the feeding thread, in
input order. Stream blocks are given blocks, written in order; plain blocks stay pool jobs. Archives are therefore
byte-identical at 1 and N threads (a test). The per-file stream work is a pure function of the file's bytes, so a
later peel pool can move it off the feeding thread (a follow-up; D-60 (5)).

### 3.8 Appends (amends D-56 (3))
An append runs only the peel stages the old archive's minor allows: the Deflate stage needs minor 2, the JPEG stage
minor 1. A dropped stage is reported in the summary and by `-v`. D-56 (3) refused peeled appends to minor-0 archives;
under the default Balanced pipeline that rule would make every pre-1.2 archive refuse appends. Records join the old
ones as D-56 describes; every append that adds records restates the whole `Records` frame (section 15), a cost that
grows with the archive's record count.

### 3.9 Summary, CLI and bench
- `PeelSummary.deflate: DeflateSummary`: files (walked by kind, peeled, size cap, deduplicated whole, no streams, none
  kept, file gate, walker panics); streams in the probe's taxonomy (found, reconstructed, skipped, failed by name,
  structural refusals, scan false hits, bytes); the pipeline's partition of reconstructed (kept, no gain, block cap,
  memory cap, already stored, file gate); `kept_by_class`; bytes (stream, plain, M, corrections, records); seconds
  (analyse, recreate, encode); the ZIP method counters; `files: Option<Vec<FileStreams>>`, filled only when
  `record_files` is set (for the share test).
- CLI: `lpk a --deflate-peel on|off`, defaulting per tier; `-v` prints the summary.
- `bench/tools.toml`: `balanced` now includes the peel (the build hash separates measurement eras, D-53 (5)); new
  control setting `balanced-no-deflate` = `["--balanced", "--deflate-peel", "off"]`, so one report shows the A/B.

### 3.10 Robustness
- Panic containment: `catch_unwind` around `find_streams`, the analysis and recreation at compression time, and
  recreation at decode time, as in the probe. The two panics on `full`'s PDFs (STATUS) become a regression fixture if
  their streams can be cut out and committed (`docs/CORPUS.md` describes GovDocs1 as public US government documents).
- Fuzzing (`fuzz/`, Linux): `deflate_walk` runs `find_streams` on arbitrary bytes; `deflate_recreate` runs the decoder
  on arbitrary `(plain, corrections)` pairs with small bounds. `fuzz/Cargo.toml` gains `lpk-core`; `fuzz/deny.toml`
  gains the `cabac` exception.
- **Known gap (the owner question):** preflate-rs 0.7.6's `recreate_blocks` has no output limit and takes its
  parameters from the corrections; it loops until a decoded "last" flag, and stored or Huffman blocks may consume no
  input. So a crafted record can push memory and time far past the term.
- Compression side: `INFLATE_CAP` stays at 2^32 so the counts equal the probe's; lowering it to the plain limit plus
  one is a follow-up, with its count change recorded.

## 4. Decode side
- The decoder (`lpk-core/src/peel/deflate/decode.rs`): `DeflateDecoder` implements `decode_in` (checks 1–8) and the
  new `PrimitiveDecoder::input_bound` (checks 1–5, returning `Some(plain_len)`); `register_full_reader` registers it
  beside `JpegDecoder`.
- lpk-format prerequisites (task A; no new decoding): (1) `decode_block_in` asks step i+1's decoder for `input_bound`
  before running step i; the default `None` keeps `max_block_plain`. (2) A records cache: today
  `ArchiveContext::record` re-reads and re-hashes the whole `Records` frame and walks from record 0 on every lookup —
  fine for a few JPEGs, quadratic in the number of records (the probe reconstructed 4722 streams on `office-pdf` of
  `small` and 123223 on `full`). Parse the records once per archive, share the result with `fork`s, and keep "the
  first error among records 0 to n" by re-walking only on the error path.
- Extraction through D-54's engine is unchanged: stream blocks are independent blocks for the pool, reading only their
  record (`nested_decodes` 0). Each worker's cost includes `decode_memory`, so large streams lower the worker count at
  the default 2 GiB budget — an input for E2-20. Refine `blocks_held` to 2 only when some record names chunks.
- `lpk t` and `verify` run every step, so every `original_hash` is checked.
- Envelope: the writer's `extra_memory` takes the largest term, by the same path as JPEG's `memory`.

## 5. Implementer tasks, in dependency order (A ∥ B → C → D ∥ E)

**A — lpk-format plumbing.** Files: `crates/lpk-format/src/{decode.rs, reader.rs, writer.rs}`. Work: the
`input_bound` hook; the records cache; `Writer::add_parts_encoded(spans, plain, encoded, memory)` with chunks cut per
span (`add_part_encoded` becomes the one-span case); `Writer::part_known(plain)`, a query that commits nothing.
Acceptance: a fake decoder with an input bound; cache outcomes equal to the walking implementation on the existing
malformed-records tests and 100000 lookups on a large frame running fast; multi-span parts interleaved with plain
parts; every committed vector byte-identical; `lpk-check` untouched and green; fmt, clippy, nextest, deny pass.

**B — the walker moved.** Files: `crates/lpk-core/src/peel/deflate/{mod.rs, walk.rs, analyse.rs}`,
`crates/lpk-core/Cargo.toml`, `crates/lpk-bench/{Cargo.toml, src/probe/deflate.rs}`. Acceptance: the walker tests
live in `lpk-core`; a partition property test (spans sorted, disjoint, inside the file; spans plus gaps rebuild the
file); no panic on truncated or garbage input of every kind; the probe's tests stay green and its end-to-end result is
unchanged; deny passes.

**C — revision 1.2.** Files: `crates/lpk-core/src/peel/{mod.rs, deflate/decode.rs}`;
`crates/lpk-format/src/{primitive.rs, header.rs, writer.rs}` (`DeflateReconstruct.revision()` = 2,
`FormatVersion::LATEST` minor 2, and at block close the writer refuses what checks 2–4 refuse);
`crates/lpk-core/tests/{deflate_vector.rs, deflate_decode.rs}` plus fixtures;
`crates/lpk-format/tests/{vectors/*, conformance.rs}`; `docs/spec/{lpk-v1.md, CHANGES.md, CONFORMANCE.md}`; `fuzz/`.
Acceptance: a test for every `BadRecord` reason, `Refused` and `PayloadTooLarge`; a two-step round trip where the
plain text exceeds `max_block_plain`; panic containment through a test hook; the vector decodes with the full reader
to `expected-1.2.toml` and fails with `UnimplementedPrimitive` (8) under `lpk-decode`; a read-only review; a draft
D-entry. The code that allows 0x0008 lands together with the spec text.

**D — the pipeline.** Files: `crates/lpk-core/src/{pipeline.rs, peel/deflate/mod.rs, peel/mod.rs, balanced.rs,
fast.rs, extract.rs, lib.rs}`, `crates/lpk-cli/src/lib.rs`, `bench/tools.toml`,
`crates/lpk-bench/src/run/catalogue.rs`, `crates/lpk-core/tests/{deflate_peel.rs, parallel.rs, journal.rs,
deflate_share_corpus.rs, deflate_corpus.rs}`, `docs/{DECISIONS.md, PLAN.md, STATUS.md, LICENSING.md}`. Work: sections
3.2–3.9; keep the JPEG `PeelStage` untouched and add `Pipeline.deflate: Option<DeflatePeel>` with its own write path.
Acceptance: synthetic ZIP, docx, JAR, multi-IDAT PNG, gzip, PDF and scan inputs (round trip; gates — a tiny stream
not kept, a file-gate case; dedup — an equal stream in two files, a whole duplicate); a docx's XML member counted
under `Text` in `kept_by_class`; header minor 2; byte identity at 1 and 8 threads; sealed archives; appends (a 1.1
archive appends without the Deflate stage and says so); the CLI flag; the corpus tests and runner rows of section 7;
D-NN, PLAN ticks with evidence, STATUS.

**E — lpk-check from the text alone.** Files: `crates/lpk-check/src/{block.rs, archive.rs, main.rs, deflate.rs
(new)}`, its `Cargo.toml` and tests. A fresh implementer, given only the spec, the vectors, CONFORMANCE and
preflate-rs's docs. Acceptance: all vectors decode, including `deflate-peel.lpk` to `expected-1.2.toml`;
`--revision 1` and `--revision 0` reproduce the `_1_0` keys; an attestation of every file read; every sentence the
implementer had to guess at becomes a spec fix before C's text is final.

Follow-ups, outside this acceptance: a peel pool; the JPEG peel of stored ZIP members; `container-reconstruct` for
stored-member routing and TAR, with the one-level nesting relaxation; lowering `INFLATE_CAP`; `delta` before
`deflate-reconstruct`.

## 6. Owner question
1. **Hostile records.** Do we carry a small patched preflate-rs 0.7.6 (Apache-2.0)? Its recreation would stop at
   `original_len`, cap the block count, and enforce the parameter limits that analysis enforces, so a crafted record
   costs at most the term and bounded time — what D-62 did for repair. Recommended once C's fuzz target confirms the
   gap. The alternative is to depend on upstream and document the gap until upstream adds a limit. Either way the
   format cites 0.7.6's correction format, and `lpk-check` stays on upstream.

Settled here, not asked (to be recorded in the D-entry): no container record (1.1, 1.3); one stream per block and the
bound rule (1.1); the zlib scan of non-container files (the probe's stream share includes these streams —
software-installed, game-assets); Balanced on, Fast off (3.5); appends drop peel stages the archive's revision does
not allow (3.8; amends D-56 (3)); the `zip` crate stays (what the probe validated; fuzzed); `cabac`
(LGPL-3.0-or-later) enters the shipped `lpk` binary for the first time, so D-26's duties (a prominent notice, the GPL
and LGPL texts, the published source) become live as a packaging task and `docs/LICENSING.md` should say so; the
revision number is 1.2 (D-62: text-only revisions take no minor).

## 7. Acceptance evidence
- **E2-10 rows:** release `lpk-bench run --tools lpk --profile small` with `balanced` and `balanced-no-deflate` on all
  classes, every extraction verified; pooled with the committed incumbents of `bench/results/2026-10-03-megatron` into
  `bench/reports/phase1-<date>-small-deflate.md`; the PLAN note cites `lpk/balanced` against every incumbent row on
  `office-pdf`, `photo-raw-png` and `software-installed`, plus the A/B. No claim beyond the rows.
- **Stream share (asserted):** `deflate_share_corpus.rs` (ignored, release, workspace build), dedup off and
  `record_files` on: the probe settings recorded in the probe file equal the moved constants; no file of the probe's
  classes exceeds 256 MiB; every file the probe found streams in was walked; for every walked file, kind, found,
  reconstructed, skipped, failed by name, structural refusals and scan false hits equal that file's record in
  `bench/results/2026-10-03-megatron-3/probe-deflate.json`; class sums equal. The probe's figures on `small`:
  office-pdf 4722 of 4829, photo-raw-png 933 of 940, software-installed 487 of 566, game-assets 3321 of 3346,
  archives-nested 1205 of 1228.
- **0 mismatches; every container of `small` round-trips (asserted):** `deflate_corpus.rs` (ignored, release) runs
  every class of `small` through the default Balanced pipeline; the D-54 engine extracts each archive and every file
  is compared; `verify` is clean; `lpk-check` also extracts and verifies the same archives; it prints the Deflate
  summary (kept by class, not kept by cause) and the sizes.
- **Member streams reach the classifier:** the unit test, and `kept_by_class` printed by the corpus test.
- **The walker is moved and tested:** `lpk-bench` calls `lpk-core`'s walker; the tests live in `lpk-core`.
- **Byte identity:** `parallel_corpus.rs` extended to `office-pdf` and `photo-raw-png`, 1 against 24 threads, with the
  peel on.
- **Spec gate:** the vector, CONFORMANCE, and `lpk-check` (task E) decoding the vector from the text alone.

Critical files: `crates/lpk-bench/src/probe/deflate.rs` (the walker to move); `crates/lpk-format/src/decode.rs` (bound
hook) and `crates/lpk-format/src/reader.rs` (records cache); `crates/lpk-format/src/writer.rs` (multi-span given
blocks, revision table, refusals); `crates/lpk-core/src/pipeline.rs` and `crates/lpk-core/src/peel/` (stage, decoder,
write path); `docs/spec/lpk-v1.md` (sections 2, 8, 9, 11, 12, 16), `CHANGES.md` and `CONFORMANCE.md`.
