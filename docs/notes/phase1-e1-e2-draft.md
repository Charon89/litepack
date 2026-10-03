# Phase 1 — E1 and E2 task breakdown (draft)

> **Status: draft, written 2026-10-03 while the `full` measurement runs.** PLAN says each Phase 1 epic gets its
> 10–25-task breakdown *after* P0-5, from the measured numbers. This draft is cut from the `small`-profile
> report (`bench/reports/phase0-2026-10-03.md`) and is to be re-cut once the `full` report and the D-08 verdict
> exist. Nothing in it is decided: every "proposed" item below becomes a decision (`docs/DECISIONS.md`) only
> when its task lands. When adopted, the tasks move into `docs/PLAN.md` under Phase 1 with checkboxes.
>
> Sources: `docs/LitePack-Method-v2.md` (§6 pipeline, §6.4 container, §9 crates, §12 phases), PLAN lines
> 73–74 (E1, E2) and the Phase 1 criteria, decisions D-01 to D-12, D-22, D-25, D-26, and the report named above.
> The only numbers in this file are cited from that report; the method document's figures are design claims,
> not measurements, and are not repeated here.

## 1. What Phase 0 says about priorities (from `bench/reports/phase0-2026-10-03.md`, `small` profile)

| Signal | Where in the report | What it means for E2's order |
|---|---|---|
| JPEG recompression is the one proven large gain: Lepton recompressed 413 of 415 files, 23.4% smaller overall, 0.48% of files failed (two); the `photo-jpeg` estimate is 76.7% of class bytes against 94.2% for the best incumbent (zpaqfranz `-m5`) and 97.1% for 7-Zip Ultra. G1 passes on `small` on this alone. | §4 probe `jpeg`; §5 estimates; §6 G1 | The JPEG peel is the first Peel task and the first thing to put under the full round-trip gate. |
| Deflate reconstruction works (10,668 of 10,909 streams, 97.8%) and per-file re-compression with xz gains 15% on PDF and Office, 46% on JAR, 8–10% on ZIP and PNG — but the per-file estimate **loses** to solid incumbents on `game-assets` (39.6% of class vs 30.8%), `software-installed` (36.3% vs 25.1%) and `archives-nested` (91.2% vs 76.5%). | §4 probe `deflate` (by container); §5 estimates | Peel without cross-file modelling loses to a solid 7-Zip. Solid blocks with a long-window model (Model) and Fold come *before* the Deflate peel, so its gain shows on top of parity rather than from behind. |
| Dedup matters on one class: `backup-versions` has 62.9% duplicate bytes (whole files) and chunk dedup saves 63.6%; `zstd --patch-from` deltas between versions are 0.07–0.24% of a version. Every other class is ≤ 1.7% duplicates. But the incumbents' solid mode already reaches 6.7% of class there and the estimate is 7.3%: G2 (2× smaller) fails as proxied. | §4 probe `dedup`; §6 G2 | Fold's dedup and delta are necessary for parity on versioned sets, not sufficient for 2×. G2's meaning is the owner's call before D-08; E2 carries an early check of Fold + solid LZMA against zpaqfranz on that class. |
| Text: in-process xz preset 9 gives 25.68% on `text-prose` and 7.11% on `logs-text`; zstd 19 gives 27.38% / 7.00%; 7-Zip Ultra lands at 25.6% / 7.1%; zpaqfranz `-m5` (context mixing) at 19.9% / 4.1%. BWT (`bsc`) was not measured (not installed). | §4 probe `text`; §5 estimates | An LZMA-class Balanced tier reaches 7-Zip parity on text. D-05's "BWT only for text" is still an unmeasured claim: the BWT task is gated on its own measurement. |
| Store speed: the `store` control writes `video` at 1121.5 MB/s against a raw read of 4836.6 MB/s (23.2%) — the write path, not the entropy gate, bounds "video at disk speed"; the gate itself costs little (sampled entropy ~70 GB/s, full entropy ~4.5–4.9 GB/s, zstd level 1 1.6–8.5 GB/s, single thread). | §4 probe `entropy-gate`; §6 G3 | The ingest/store path must be built for sequential throughput from the start (large sequential writes, no per-file round trips). G3's definition is the owner's call. |
| Extraction: zstd level 3 extracts the `photo-doc` mix at 401.4 MB/s against 156.1 MB/s for 7-Zip `-mx5` (G4 passes). | §6 G4 | A zstd-based Fast tier keeps G4. In Balanced, JPEG decode will dominate extraction of photo archives, so independent streams must decode in parallel — a constraint on both the frame design (E1) and the pipeline (E2). |
| `probe weights` had nothing to parse in `small`; the `full` run decides whether the byte-plane transform earns a Phase 1 task. | §4 probe `weights` | Weights stay a Phase 2 item (method §12) unless the `full` report says otherwise. |

## 2. What the method document leaves to E1 (must be decided, proposals below)

The method document names the container's parts but gives no byte-level layout. E1's first task settles these as
recorded decisions; the proposals here are starting points, not choices made:

1. **Magic, header, versioning** — proposed: 8-byte magic in the PNG style (non-ASCII lead byte, `LPK`, CR LF, SUB,
   LF), little-endian throughout, a format version as `major.minor` (`u16`,`u16`), a header flag word; a reader
   refuses an unknown major version and refuses any frame or primitive carrying a must-understand flag it does
   not know; unknown optional frames are skipped. Varint (LEB128) lengths inside frames, fixed-width in headers.
2. **Frames** — proposed: the archive is a header followed by self-describing frames, each `type, flags, length,
   payload, BLAKE3 of payload`; frame types in v1: entry table, chunk data (a solid block of one primitive graph),
   reconstruction records, recovery, index (seek table + Merkle roots), journal commit (trailer). A trailer
   points back to the latest index frame; the file ends with a trailer.
3. **Solid blocks** — proposed: streams are grouped into solid blocks per class and similarity cluster (D-12
   order), with a block size cap aligned to the decode envelope (the method document's ≤ 256 MB window cap);
   random access extracts a block; the Fast tier uses smaller blocks than Balanced.
4. **Merkle tree** — proposed: BLAKE3 over the *original* bytes of each content-defined chunk (so verification
   covers what the user gets back, not the compressed form), a binary tree of chunk hashes per archive, roots in
   the index; a file is a list of chunk references, so any file or byte range verifies from the chunk hashes.
5. **Decode envelope** — proposed fields: maximum window bytes, maximum BWT block bytes, estimated decode
   memory, thread hint; defaults 256 MB and 64 MB as the method document states; a decoder compares the envelope
   with local resources before decoding and refuses with a clear message unless the caller overrides.
6. **Reconstruction records** — one record type per peel: JPEG (Lepton stream, trailing data as a nested
   stream, gain-map secondaries as nested streams), Deflate (preflate corrections plus the parameters needed to
   re-encode), container framing (verbatim local headers and central directory, member order), PNG row filters
   (filter bytes), text encodings (base64/UTF-16 parameters). Records are versioned primitives (D-03).
7. **Recovery** — proposed: Reed-Solomon over fixed-size shards in GF(2^16) (`reed-solomon-simd`), a recovery
   frame covering a range of frames, 1–20% configurable, default off in v1 of the format with the CLI default
   decided in E3 (`--recovery 5%` appears in PLAN E3).
8. **Encryption** (D-06) — proposed: a random 256-bit archive key wrapped under a key-encryption key from
   Argon2id (RFC 9106 second recommended parameters by default) with an optional keyfile mixed in by HKDF;
   per-chunk nonces derived from the chunk index; associated data = archive id, frame type, chunk index;
   AES-256-GCM default, XChaCha20-Poly1305 optional; "listable" mode leaves the entry table in clear.
9. **Journal** — proposed: an append adds frames, a new index and a new trailer; earlier trailers stay valid;
   rollback truncates to a previous trailer; dedup against the existing chunk hashes; recovery frames cover only
   what they were written for (an append writes its own).
10. **Priors by ID** (D-10) — proposed: a prior is identified by the BLAKE3 of its content plus a short name;
    the reader ships every prior it can reference; an archive lists the priors it uses in the index.
11. **Limits** — proposed: 64-bit sizes and counts everywhere; UTF-8 paths with a stated maximum; no limit on
    entry count other than memory, which the envelope declares.
12. **Primitive set v1** — the enumerated list of codecs and transforms an archive may reference (zstd, LZMA,
    BWT-for-text, BCJ/ARM64 filters, delta, Deflate reconstruction, JPEG reconstruction, PNG filter, base64,
    UTF-16, container framing, store), each with an ID and a parameter encoding; nothing executable (D-03).

## 3. Crates and files

- `crates/lpk-format` — pure Rust, `#![forbid(unsafe_code)]`, no compression libraries except what the primitive
  set needs for *decoding* (zstd decode through `ruzstd` keeps the decoder free of C; LZMA decode needs a
  pure-Rust LZMA decoder — to be chosen in E1 task 9, against the allow-list of D-25). Modules: `magic`,
  `header`, `varint`, `frame`, `entry`, `chunk`, `merkle`, `envelope`, `record` (one file per record type),
  `recovery`, `crypto`, `journal`, `index`, `reader`, `writer`, `primitives`. The reference decoder is the
  `reader` side plus a small binary `lpk-decode` (list, verify, extract) in the same crate.
- `docs/spec/lpk-v1.md` — the format specification, written from the `lpk-format` types and kept in step with
  them by a test that renders the primitive table and frame types from code and compares them with the spec.
- `crates/lpk-format/tests/vectors/` — conformance vectors: small archives with known contents, one per feature,
  generated by the writer and checked by the reader; the independent decoder of E1's gate reads only the spec
  and these vectors.
- `crates/lpk-core` — the pipeline, as PLAN E2 names it, with one module per stage: `ingest`, `classify`,
  `fold`, `peel` (submodules `jpeg`, `deflate`, `container`, `png`, `text`), `model`, `seal`, and `pipeline`
  (the orchestration). It links the C libraries (zstd, liblzma, lepton, preflate) and therefore is not the
  decoder of record; the method document's split into separate `lpk-peel`/`lpk-fold`/`lpk-model` crates can
  be made later without a format change.
- `crates/lpk-bench` — gains an `lpk` tool entry in `bench/tools.toml` once a command line exists, so the
  Phase 1 gates are measured by the same runner and report that measured the incumbents.
- `fuzz/` — `cargo-fuzz` targets for every `lpk-format` reader path (Linux CI job, method §8).

## 4. E1 — format spec v1 and reference decoder (14 tasks)

Order matters: the method document says "property/round-trip tests and format test vectors are written before
the engine". Each task ends with tests, a spec section and a commit; each task that fixes a format detail
records it as a decision.

| # | Task | Files | Produces (what later tasks use) | Acceptance |
|---|---|---|---|---|
| E1-1 | **Frame grammar and header.** Magic, header, version, flags, varints, frame envelope with per-frame BLAKE3. | `lpk-format/src/{magic,header,varint,frame}.rs`; `docs/spec/lpk-v1.md` §1–3 | `Frame { kind: FrameKind, flags, payload }`, `read_frame`, `write_frame`, `FormatVersion` | Round-trip property tests (proptest) over random frames; a reader rejects a bad hash, a wrong magic, an unknown major version, an unknown must-understand flag; spec sections match code (rendered-table test). Decisions recorded for §2 items 1–2. |
| E1-2 | **Entry table.** Paths, sizes, times, attributes, symlink/ADS policy fields, chunk reference lists. | `entry.rs`; spec §4 | `Entry`, `EntryTable` with `iter()`, `find(path)` | Vectors with Unicode paths, long paths, empty files, directories; a 1,000,000-entry table reads in bounded memory (streamed) — a test on a generated table. |
| E1-3 | **Chunks and Merkle tree.** Chunk reference, content hash, binary tree over chunk hashes, per-file verification from chunk references. | `chunk.rs`, `merkle.rs`; spec §5 | `ChunkRef`, `MerkleTree::{build, root, verify_file, verify_range}` | Property test: any single-byte corruption in a chunk is detected and localised to that chunk; a byte range of a file verifies without reading other chunks; decision for §2 item 4. |
| E1-4 | **Index and seek table; trailer.** Where each chunk and block lives; the trailer that finds the index. | `index.rs`; spec §6 | `Index`, `Trailer`, `locate(chunk) -> (block, offset)` | Open an archive from its tail without reading the body; a truncated archive is reported as truncated, not corrupt; the index of a 1,000,000-chunk archive stays within the envelope's memory. |
| E1-5 | **Decode envelope.** Fields, defaults, the refusal rule. | `envelope.rs`; spec §7 | `Envelope`, `Envelope::check(local: &Resources) -> Result<(), Refusal>` | Vectors with envelopes above and below the local limits; the refusal names the limit. Decision for §2 item 5. |
| E1-6 | **Primitive set v1.** The ID registry and parameter encodings for every codec and transform of §2 item 12, with `store` the only one the decoder must implement in this task. | `primitives.rs`; spec §8 | `PrimitiveId`, `Params`, `DecodeGraph` (a small DAG of primitives per block, zpaq/OpenZL lineage) | A graph with an unknown primitive fails before any data is read; the rendered primitive table equals the spec table; decision for §2 item 12 (and the forward-compatibility rule, item 1). |
| E1-7 | **Writer and reader for store-only archives.** The first end-to-end: pack a directory with `store`, read it back. | `writer.rs`, `reader.rs`, `bin/lpk-decode.rs` | `Writer::{begin, add_entry, add_block, finish}`, `Reader::{open, entries, extract(entry, sink)}` | Round-trip of the P0-2 `small` corpus's `small-files` and `source-git` classes with BLAKE3 equality; `lpk-decode list|verify|extract` work on the vectors. |
| E1-8 | **zstd decoding in the decoder (pure Rust).** `ruzstd` as the decoder's zstd implementation; the writer may use libzstd. | `primitives/zstd.rs` | zstd primitive usable in graphs | Vectors written with libzstd (through a test-only dependency) decode byte-exactly with `ruzstd`; dictionary-by-ID vectors (D-10) decode when the prior is present and fail with its ID when absent. Decision for §2 item 10. |
| E1-9 | **LZMA decoding in the decoder.** Choose and integrate a pure-Rust LZMA decoder from the allow-list (candidates are checked in this task: `lzma-rs` and `xz2`-free alternatives), or write the decoder in-tree if none qualifies. | `primitives/lzma.rs`; `deny.toml` unchanged or a recorded decision | LZMA primitive | Vectors written with liblzma decode byte-exactly; the choice recorded as a decision with the licence check. |
| E1-10 | **Reconstruction record types.** JPEG, Deflate, container framing, PNG filters, text encodings — encodings only; the decoder applies them in E2 (Lepton and preflate decoding live in `lpk-core` because they link C; the format defines the records, the reference decoder reports them as "requires the full reader" with the record's own verification). | `record/{jpeg,deflate,container,png,text}.rs`; spec §9 | `Record` enum, `RecordRef` in `ChunkRef` | Each record type round-trips through the frame grammar; vectors contain one of each; `lpk-decode verify` checks record hashes without decoding them. Decision for §2 item 6. |
| E1-11 | **Recovery frames.** Reed-Solomon over shards, coverage ranges, repair. | `recovery.rs`; spec §10 | `RecoveryFrame::{build(frames, percent), repair(archive)}` | Property test: with `p`% recovery, any damage up to the covered amount repairs to the original hashes; damage beyond it is reported, not silently "repaired". Decision for §2 item 7. |
| E1-12 | **Encryption.** Key derivation, key wrap, per-chunk nonces and associated data, listable mode. | `crypto.rs`; spec §11 | `Keyring`, `Sealed<Frame>` | Vectors with password, password + keyfile, listable and non-listable; a wrong password fails at the key wrap, never at a chunk; nonce uniqueness proven by construction and by a test over chunk indices; Argon2id parameters recorded as a decision (§2 item 8). |
| E1-13 | **Journal.** Append, re-index, trailer chain, rollback by truncation. | `journal.rs`; spec §12 | `Writer::append(existing)`, `Reader::history()` | Append two generations to a vector; the latest trailer wins; truncating to the first trailer restores the first generation bit-exactly; recovery frames of an append cover only the append. Decision for §2 item 9. |
| E1-14 | **Fuzzing, spec review, the independent-decoder gate.** Fuzz targets for every reader path (Linux CI); the spec read end to end by a reviewer who has not seen the code; then the gate: an implementer who is given only `docs/spec/lpk-v1.md` and the vectors writes a decoder (a separate crate, kept as `crates/lpk-decode-independent` or outside the repo) that decodes every vector. | `fuzz/`, CI job; spec complete | The E1 gate evidence | Fuzzing runs clean for the configured time; the independent decoder decodes all vectors; differences found are spec bugs and are fixed in the spec (and vectors) before the gate is ticked. |

## 5. E2 — pipeline core (22 tasks), in the order the measurements suggest

Each task: tests first, a measured check on the `small` corpus through the runner where the task changes a
number, and nothing claimed that the harness did not measure.

| # | Task | Files | Interfaces | Acceptance |
|---|---|---|---|---|
| E2-1 | **Ingest.** Walk a directory, read files (memory-mapped above a size threshold, buffered below), capture metadata, hand out `Input` streams in a stable order; a `store` path that writes sequentially. | `lpk-core/src/ingest.rs` | `Ingest::walk(root) -> impl Iterator<Item = Input>`, `Input { path, len, source }` | The `store` path through `lpk-format` reaches a write throughput measured by the runner (the control's own number on this machine is the comparison, not a target). Round-trip of `small` with the `store` graph. |
| E2-2 | **Classifier and entropy gate.** Magic-byte table plus byte-statistics features; `Class` labels for the routing table (text, jpeg, deflate container, png, executable, audio, image-raw, high-entropy, other); the gate stores high-entropy chunks as-is (sampled entropy first, full entropy on doubt, D-07's "video at disk speed"). | `classify.rs` | `classify(&[u8]) -> Class`, `is_incompressible(&[u8]) -> bool` | Precision/recall of the gate against the `small` corpus's xz ground truth from `probe entropy-gate`'s method, reproduced as a unit test on the corpus; the classifier labels every file of every corpus class with the expected class on `small` (a table test). |
| E2-3 | **Model, Fast tier.** zstd (libzstd) with a long window, per-class dictionaries bundled as priors, BCJ/ARM64 filters for executables, solid blocks per cluster. | `model/{zstd,filters}.rs`, `priors/` | `Model::compress(block, tier) -> Graph + bytes` | The runner's `lpk --fast` row on `small`: archive smaller than `store`, extraction speed and size reported; no claim beyond the measured row. |
| E2-4 | **Pipeline orchestration and the CLI stub for the runner.** Stage ordering (classify → peel → fold → model → seal, with Fold able to run on peeled streams), parallelism by independent stream/block, a minimal `lpk a|x|t` so `lpk-bench run` can measure it. | `pipeline.rs`, `bin/lpk.rs` (test-grade; the real CLI is E3), `bench/tools.toml` entry `lpk` | `Pipeline::new(Tier)`, `archive(inputs, out)`, `extract(archive, dir)` | `lpk-bench run --tools lpk` completes on `small` with 100% verification; the `report` lists `lpk/fast` next to the incumbents. |
| E2-5 | **Peel: JPEG (Lepton).** Verify-by-re-encode, the fallback rules of D-09, trailing data and gain-map secondaries as nested streams, dimension limit raised, failure causes counted exactly as `probe jpeg` counts them. | `peel/jpeg.rs` | `peel_jpeg(&[u8]) -> Peeled { stream, record } \| Store(reason)` | Round-trip of `photo-jpeg` and `photo-jpeg-edited` with 0 mismatches; the runner's `lpk --balanced` row on those classes reproduces the probe's size (same files, same library) within the fallback count; parallel decode of independent JPEG streams at extraction. |
| E2-6 | **Model, Balanced tier (LZMA-class).** liblzma with a large dictionary for solid blocks, zstd `--ultra --long` as the alternative chosen per block by a trial on a sample; filters as in E2-3. | `model/lzma.rs`, `model/select.rs` | `Tier::Balanced` in `Model::compress` | On `text-prose`, `logs-text`, `office-pdf` the Balanced row is not larger than the 7-Zip Ultra row of the same runner report (parity check on `small`); extraction time reported. |
| E2-7 | **Fold: content-defined chunking and dedup.** FastCDC (the parameters PLAN P0-4 used: 64 KB average, 4 KB min, 512 KB max) plus BLAKE3, global chunk dedup within the archive, chunk references in entries. | `fold/{cdc,dedup}.rs` | `fold(inputs) -> Chunks`, `ChunkStore` | `backup-versions` on `small`: duplicate bytes removed equal to `probe dedup`'s "saved by chunk dedup" figure for the same parameters; round-trip 0 mismatches; chunking throughput reported by the runner, not claimed. |
| E2-8 | **Fold: file-level similarity ordering.** Per directory/type cluster (D-12), a cheap similarity feature (super-features over chunk hashes, the method document's "multi-tier super-features"), ordering files inside a solid block by similarity. | `fold/order.rs` | `order(files) -> Vec<FileId>` | Measured on `small`: `software-installed`, `game-assets`, `small-files` Balanced rows smaller than without ordering (an A/B through the runner, both rows committed); ordering cost reported. |
| E2-9 | **Fold: deltas.** `zstd --patch-from`-style deltas in Fast, suffix-array deltas (HDiffPatch-class, MIT, or in-tree) in Balanced, between near-duplicate files found by E2-8. | `fold/delta.rs` | `delta(base, target) -> Patch`, `apply(base, patch)` | On `backup-versions`: Balanced row against zpaqfranz `-m5` and 7-Zip Ultra from the same report — this is the first measured check of the G2 question with real LitePack output; the number goes to the owner, not into a claim. |
| E2-10 | **Peel: Deflate (preflate-rs).** Raw Deflate/zlib/gzip streams found by the container walkers of E2-11, reconstructed with corrections, net-gain gate per stream and per file (D-09), documented fallbacks (Deflate64, preset dictionaries, unrecognised encoders). | `peel/deflate.rs` | `peel_deflate(stream) -> Peeled \| Store(reason)` | `office-pdf`, `photo-raw-png`, `software-installed` Balanced rows against the same report's incumbents; the reconstructed-stream share equals `probe deflate`'s on the same files; 0 mismatches. |
| E2-11 | **Peel: containers, one level.** ZIP (including Office packages, JAR/APK), PDF object streams, PNG chunks, gzip; members become routed streams, the framing a reconstruction record; recursion depth 1 in this task. | `peel/container/{zip,pdf,png,gzip}.rs` | `walk(container) -> Members + FramingRecord` | Round-trip of every container of `small` bit-exactly; member streams reach the classifier; the walker is the one `probe deflate` validated, moved and tested. |
| E2-12 | **Seal: AEAD encryption.** The `lpk-format` crypto from E1-12 driven by the pipeline: password, keyfile, listable mode; per-chunk nonces from chunk indices. | `seal/crypto.rs` | `Seal::encrypt(frames, keyring)` | Encrypted round-trip of `small`; the runner row with `--password` shows the cost against the plain row; a wrong password is refused before any data is written to disk. |
| E2-13 | **Seal: recovery records.** Writing recovery frames at the configured percentage; repair on extraction. | `seal/recovery.rs` | `Seal::add_recovery(percent)` | A `small` archive damaged by the test's random overwrites up to the covered amount repairs and verifies; the archive size overhead equals the configured percentage within the frame rounding. |
| E2-14 | **Seal: journal (append and rollback).** Append a changed directory to an existing archive with dedup against its chunks; rollback. | `seal/journal.rs` | `Pipeline::append`, `Pipeline::rollback(generation)` | `backup-versions` as three appends (v1, then v2, then v3): the archive after three appends is within the frame overhead of the one-shot archive of all three; rollback to v1 reproduces v1 bit-exactly. |
| E2-15 | **Model: BWT for text (measured gate).** libsais (Apache-2.0) in a `lpk-codecs-bwt` crate, applied only to text-classified streams (D-05), block size under the envelope cap, with a measurement first: BWT+entropy coding against the LZMA-class row on `text-prose` and `logs-text`. | `model/bwt.rs`, `crates/lpk-codecs-bwt` | `Tier::Balanced` text route | The task ships only if the measured row is smaller than the LZMA-class row on both text classes with extraction time reported; otherwise the route stays LZMA and the result is recorded as a decision — D-05 is confirmed or amended by measurement, not by assumption. |
| E2-16 | **Peel: PNG row filters.** Undo per-row filters after the Deflate peel (filter bytes as side info), raw pixels to the model; measured against "preflate + model on filtered rows" as the method document asks. | `peel/png.rs` | `peel_png(member) -> Peeled` | `photo-raw-png` Balanced row with and without the filter undo (both committed); keep whichever is smaller and record it. |
| E2-17 | **Peel: text encodings.** base64 and UTF-16 detection and inversion with verification; line endings left alone in v1. | `peel/text.rs` | `peel_text(stream) -> Peeled \| Store` | Synthetic vectors (base64 blocks inside text, UTF-16 files) round-trip; on `small` the rows that contain such streams are not larger than before (A/B committed). |
| E2-18 | **Entropy gate in the write path and the video target.** Store high-entropy input through the sequential store path without compression attempts; measured against the `store` control and the raw-read line of `probe entropy-gate` on this machine. | `pipeline.rs`, `ingest.rs` | — | The `video` and `encrypted-random` rows of the runner for `lpk --fast` and `--balanced`; the G3 comparison in the report, whichever definition the owner settles. |
| E2-19 | **Extraction performance.** Parallel decode of independent blocks and streams, sequential writes in entry order, safe extraction policy hooks (the policy itself is E3/E6). | `pipeline.rs`, `extract.rs` | `extract` parallelism settings | G4 as measured by the runner: `lpk --fast` extraction against 7-Zip `-mx5` on every mix of the report. |
| E2-20 | **Memory and envelope enforcement on the compress side.** Thread and memory budgets per tier, the envelope written from what was used, refusal on decode when the local machine cannot meet it. | `pipeline.rs`, `envelope` use | — | Peak RSS of `lpk` rows in the runner within the tier's budget on `small` and `full`; an archive written with a 256 MB window is refused on a decoder with a smaller configured limit. |
| E2-21 | **Full-corpus round-trip gate.** `lpk-bench run --tools lpk --profile full` for both tiers, 0 mismatches, results committed; the report with the `lpk` rows next to the incumbents. | `bench/results/`, `bench/reports/` | — | PLAN E2 gate, first clause: "round-trip on the full corpus with 0 mismatches", evidenced by the committed run. |
| E2-22 | **D-07 check and the Phase 1 verdict input.** The report's gate table with real `lpk` rows (not estimates); differences from the Phase 0 estimates explained per class; the owner's D-08 interpretation of G2 and G3 applied. | `bench/reports/`, `docs/DECISIONS.md` | — | PLAN E2 gate, second clause: "Balanced tier meets D-07 gates" — PASS/FAIL per gate from the report, recorded as a decision with the numbers. |

## 6. Sequencing and dependencies

- **Weeks 1–3: E1-1 to E1-7** (the format core and store-only archives) in parallel with **E2-1, E2-2** (ingest,
  classifier) — the format must exist before any pipeline stage can write an archive; vectors before engine.
- **Weeks 3–6: E2-3, E2-4, E2-5** — Fast tier, the runner integration, the JPEG peel. From E2-4 on, every
  task is measured by `lpk-bench run --tools lpk` on `small`; the report gains an `lpk` column.
- **Weeks 6–10: E2-6 to E2-11** with **E1-8 to E1-10** — Balanced tier, Fold, Deflate and containers; the G2
  check (E2-9) goes to the owner as soon as it exists.
- **Weeks 10–13: E1-11 to E1-13 and E2-12 to E2-14** — recovery, encryption, journal, in that order on both sides.
- **Weeks 13–16: E2-15 to E2-20, E1-14** — the measured options (BWT, PNG filters, text encodings), the speed work,
  fuzzing and the independent-decoder gate.
- **Then E2-21, E2-22** — the `full` round-trip and the gate table. The method document's "~3–4 months" is a
  claim, not a plan; the weeks above are a sketch for ordering only.

Items that need the owner before the breakdown is adopted: the G2 and G3 definitions (D-08); whether MP3
(packMP3, LGPL-3.0, D-26) and the structured/OpenZL engine stay out of Phase 1 as the method document's §12
says; whether `lpk-core` stays one crate; the Argon2id and recovery defaults; and the licence check of the
pure-Rust LZMA decoder (E1-9).
