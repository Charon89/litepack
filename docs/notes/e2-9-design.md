# E2-9: the delta stage — revision 1.5 of `.lpk` (design)

Drafted 2026-10-22 against `origin/main` f243e06 (E3-4 merged), from the E2-9 research note (`e2-9-research.md`,
cited by its sections), PLAN E2-9, the D-entries cited, spec sections 2, 5, 7, 8, 9, 15 and `docs/LICENSING.md`; fix
round 1 applied. **No figure here comes from a run**; nothing is decided until the D-entries of section 6. Revision
1.5 follows E2-16's 1.4.

## 1. Pairing: versions by path and name alone
**Today.** Inputs are walked in path order (`crates/lpk-core/src/ingest.rs:169`); `cluster` groups files by (class,
dictionary kind) in that order (`cluster.rs:94`); `order_cluster` (`pipeline.rs:681-692`) applies
`FileOrder::{None, Extension}` (`fold/ordering.rs:13`). So a file's versions sit a version apart (D-85), beyond any
window (`balanced.rs:104`, `fast.rs:83`).

**The rule (writer only).** Version roots are the run's inputs when several are given (`Sources::roots`,
`ingest.rs:137`, `:196`), else the one input's children (`Root::contents`, `:146`; `Sources::contents_of`, `:203`);
what `--exclude` removes
(`:198`) is not counted. Two roots are siblings when at least half of the smaller root's files have the same relative
path in the other (a writer threshold); connected sibling sets are version groups, ranked by the natural order of their
names (digit runs compared numerically, the rest by bytes: `v2` < `v10`). A file's base file is the same relative path
in the nearest lower rank that has it; for an append, the same path in the previous generation's entry table
(`old_paths`, `pipeline.rs:1131`). An archive without sibling versions is unchanged byte for byte.

**The ordering change.** `FileOrder::Versions`: inside each cluster the key is (path with the root stripped, rank)
for a grouped file and (path, 0) otherwise, so a file's versions are adjacent in the window; everything else keeps
path order and D-55's locality. Rejected: name-only pairing, extension order (D-55), sketches (the patent row), roots
without the threshold.

**The patent.** Claims 1 and 18 require (a) randomized sketches per chunk, (b) a probabilistic selection of similar
sketches, (c) deltas of the selection, (d) one chunk stored plus deltas (research, "US 9,798,731").
This design has (c) and (d) and lacks (a) and (b): nothing is computed from content to choose a pair. Counsel's
reading stays the owner's (`docs/STATUS.md`).

## 2. The base
**Which chunk.** A chunk c = [o, o + n) of file F, cut by FastCDC per file (`fold/chunker.rs:11-15`; D-53), is
*changed* when `find_known` misses (`crates/lpk-format/src/writer.rs:1964`). Its candidate base is the base file's
chunk whose span covers o by the running sum of `plain_len` over its chunk list (as `verify_range`, `chunk.rs:295`);
past the base file's end, its last chunk. Shifted cuts still overlap; rule B2 also tries the two neighbours. The spec
leaves the choice open.

**Depth and substitution.** `DELTA_DEPTH = 1`, in the spec: a base chunk lies in a block whose graph names neither
`delta` nor a reconstruction primitive (section 12). A derived candidate (the pipeline knows its own; an append reads
the old block's graph, `writer.rs:1999`) is replaced by its own base (P1; the probe prices it against P2); a
reconstruction block gives no delta. Deeper chains: rejected.

**Acyclicity and order.** The base block's index is below the derived block's (records' `chunk order` rule,
`reader.rs:267`) and names no `delta`: no cycle, and block-ordered extraction (D-54 (1)) meets every base first.
**The writer guarantees it by deferral:** a derived chunk gets a placeholder index in the open
entry's chunk list (`OpenEntry`, `writer.rs:408`) and in the dedup map (`own`, `:490`); after the next plain-block
flush (`flush_block_with`, `:1469`) the queued patches are written as one-chunk blocks and the placeholders resolve: the chunk
table stays in block order (`index.rs:75`) and derived blocks follow their bases' plain block.
Rejected: a block close per derived chunk.

**Base bytes for the diff.** One-shot: the base file is re-read and sliced by the chunk table's lengths, each chunk
checked against its BLAKE3 first. Append: decoded from the old generation through the writer's
`Base` (`writer.rs:415`) with a one-block cache. Rejected: an in-memory pool (D-67).

**Which chunks become deltas.** A paired changed chunk whose base lies in the *open* block is coded plain unless
`delta_in_window` is on; any other is derived when its coded patch block, overhead included,
is at most r times its own coded size (r from the probe). The tier's coder precedes `delta` (`balanced.rs:295`).

## 3. The two patch formats
The registry row (section 8, 0x0006; `primitive.rs:24`, `:151`, `:186`) fixes the params: `base_chunk` u64 LE,
`patch_format` u8 ∈ {0, 1}. Input: the patch; output: the block's plain bytes; base: one chunk. `delta` bounds nothing
(`bounds_input` false, `decode.rs:150`); a 1.5 reader runs `patch_format` 1 only, 0 being `UnimplementedPrimitive` 6
at step 8 of section 8's block checks, before any step runs.

**Format 1 — the suffix-array patch (ours, byte-exact).** Canonical varints; L_b is the base chunk's `plain_len`.

| Field | Size | Meaning |
|---|---|---|
| `out_len` | u64 LE | output length |
| `cover_count` | varint | number of covers |
| per cover: `gap`, `base_off`, `len` | 3 varints | new bytes before the cover; offset in the base; `len` ≥ 1, `base_off + len` ≤ L_b |
| `tail` | varint | new bytes after the last cover |
| `new` | Σ gap + tail bytes | the bytes no cover produces, consumed in output order |
| `diff` | Σ len bytes | per covered byte: out = (base[base_off + i] + diff) mod 256, consumed in cover order |

Output, in order: per cover `gap` bytes from `new`, then `len` bytes from the base plus `diff`; then `tail` bytes from
`new`. Base spans may overlap; output positions never do. The input length is exactly the layout's; decode memory L_b
and the input, no table. Encoder: HDiffPatch's method over libsais's suffix array of the base chunk (MIT; from the
algorithm, not ported: research, "Shortlist" 1; libsais in tree, `lpk-codecs-bwt`). Rejected: bsdiff's framing, RLE, VCDIFF.

**Format 0 — the zstd patch.** Exactly one RFC 8878 frame without `Dictionary_ID`, decoded as if the base chunk's bytes
preceded the frame's output (offsets reach into the base); `Window_Size` checked against `max_window` (`zstd` rules 2,
5, 7); decode memory the reference's ring for that window (`memory.rs:564`) plus L_b and the input. **Open risk:**
whether `ruzstd` 0.9.1, the in-tree decoder (D-67), decodes against a raw-content prefix is not established (research,
"Shortlist" 2, "Not established"); the encoder exists (`zstd` 0.14's `ref_prefix`). Ways out: (a) task A's one test (a
`ref_prefix` frame through `ruzstd`'s raw-content dictionary) decides, and 1.5 specifies both formats if it decodes
bit-exactly; (b) format 1 only, 0 reserved, outcome unchanged.

**Recommendation: (b), with (a)'s test run and recorded:** one decoder for both tiers, decodable by `lpk-check` from
the text alone (D-49 (5)); a copy-speed apply (G4); no library prefix semantics in place of RFC 8878 text.

**The lasting term (the 1.3 pattern).** For a step whose base lies in block β: term = P_β + copies · F_β + D(P_β) +
L_b + I + 1 MiB, saturating: β's `plain_len` and `frame_len` (`index.rs:24-37`; copies = 2 when sealed, as D-67 (5)
counts a frame read); D(P_β) = `max_window` + (`max_bwt_block` > 0 ? term_bwt(P_β, `max_bwt_block`) : 0); I the step's *input bound*, never its input's length — `max_block_plain` when a step
precedes `delta` (section 8's Bounds, `lpk-v1.md:727`), the header's `encoded_len` when `delta` is first — so the
writer, the reference and `lpk-check` compute one term from the header and the index alone (D-49 (5)). Check (1): a
term above `decode_memory − max_block_plain` is `Refused`, nothing read.

**Order of checks** after section 8's: (1) `base_chunk` below the table's length (`ChunkIndexOutOfRange`), in a lower
block, with L_b > 0 (else `DeltaError` `base`: a delta against nothing is a plain chunk); (2) the term; (3) input under
8 bytes (`DeltaError` `length`); `out_len` against the bound (`BlockLengthMismatch` last, else `PayloadTooLarge`); (4)
the header, `DeltaError` `patch` at the first fault, nothing allocated before: `cover_count` above the remaining input
/ 3 (a cover is at least three bytes); per cover a non-canonical varint, `len` 0 or a span past L_b; Σ(gap + len) +
tail, saturating, ≠ `out_len` (a wrapping sum equal to it is refused); a wrong input length; (5) the base: its block's
header (a `delta` or reconstruction step is `DeltaError` `base`), decoded under
its own rules, the chunk compared with the table (`ChunkMismatch`); (6) the apply. `DeltaError` is additive within
D-49 (3); `delta` is written last (`BadGraph`, `delta position`), decoded anywhere.

## 4. The extraction plan (D-54)
`plan` (`crates/lpk-core/src/extract.rs:717`) maps each chunk to its block and placements through `ChunkIndex::locate`
(`chunk.rs:448`); the plan does not change. `needed` omits base blocks, as records' part blocks are omitted: the
worker's reader fetches the base on demand through `ArchiveContext::chunk` (`reader.rs:256-275`; the one-block cache,
`archive.rs:32`), counted in `nested_decodes` (`extract.rs:178`; `decode_in`, `decode.rs:93`).

**Cost.** Selective extraction of a derived file costs its derived blocks plus one nested decode of each distinct base
block per worker; a full extraction every block once plus the nested decodes. Derived blocks follow their bases' plain
block, so the dispatcher (`extract.rs:1605`) keeps affinity (the next block to the same worker) and a run costs one
nested decode.

**Memory.** No new engine term: `decoder = decode_memory − max_block_plain` (`extract.rs:1021`) carries the base block
because the writer declares the term and check (1) refuses less; it is charged per block in flight, so delta archives get
fewer workers.
Rejected: a nested term for every archive (D-67).

**`lpk-check` (D-49 (5), from the text alone).** It holds the archive in memory (`crates/lpk-check/src/archive.rs:266`)
and decodes any block (`:668`, `:721`); `block.rs:177-182` parses 0x0006 to `Unimplemented`
today; `--revision` (`main.rs:29`, `0..=3` on `main`) gains 5.

## 5. The measured gate before any writer code
**`lpk-bench probe delta`** (`probe/mod.rs:64-77`): on `backup-versions` of `small` and
`backup-versions-large` of `full` as E0-2 re-pins it. It chunks as the dedup probe does (`dedup.rs:49-51`), pairs
files by relative path and sorts each later version's chunks into unchanged, changed-with-base and new. Per later
version v, pairing rule (P1 previous version with substitution; P2 first version), base rule (B1, B2) and format (F1
the in-tree encoder, bench-only until C; F0 `zstd`'s `ref_prefix`) it records, each patch coded alone by the tier's
coder and applied back byte for byte: Y_v = Σ coded
patches, n_v = derived chunks, create and apply seconds; X_v = the coder over the changed chunks in walk order; X_v^adj
= the same with each chunk right after its base (ordering alone); O_block, the per-block constant; the
append estimate A_v = Y_v + n_v · O_block + the coder over v's new chunks alone (an appended generation, every paired
changed chunk derived, D-59); the `ruzstd` fact; the manifest hash.

**The formula.** S_bal and B come from one manifest, E0-2's directories only: the incumbents' run
(`target/acceptance/e0-2-measure.sh`, under way) and a following `lpk` run of both tiers on the class from the same
manifest (the controller adds it; the 2026-10-20 `lpk/balanced` row is on the old class). B is the best row among
tools without `dedup = true` (`bench/tools.toml:215`, `:291`). The pre-gate refuses rows whose manifest hash differs
from the probe's (D-48, D-74):

- S_est(rule, format) = S_bal − Σ_v X_v + Σ_v (Y_v + n_v · O_block)
- S_ord = S_bal − Σ_v X_v + Σ_v X_v^adj

**Decision rule** (μ = 0.02 · B, in D-NN (1), the owner's to change): (i) S_ord ≤ 0.5 · B: E
alone meets G2 (D-43, D-89 (1)); B, C, D are not built; (ii) else C is built only when min over (rule, format) of
S_est ≤ S_ord − μ and ≤ 0.5 · B; (iii) else both estimates are reported to the owner. E's
criterion on `small` is "not larger" (ordering shows only past the dictionary, D-55 (3)); its `full` row and **the
row that ticks E2-9 are F's.**

## 6. Tasks
Order: A and E first; B after A; C and D after B (D's `lpk-check` half last); F in
E3-9's measurement of `full`.

- **A — probe.** Acceptance: `lpk-bench probe delta --profile small|full` writes section 5's JSON and table, every
  patch applied and compared, with the `ruzstd` outcome, the manifest hash and the formulas; the directory committed;
  D-NN (1).
- **B — revision 1.5.** Spec sections 2, 8, 9 (the layout, informative), 11, 16 and their conformance files;
  `lpk-format`'s plumbing (`PrimitiveId::Delta.revision()` 5, `primitive.rs:117`; the writer's
  undecoded and position rules, `writer.rs:658-697`; `DeltaError`); `DeltaDecoder` in `lpk-core` (`bwt.rs:104`'s
  pattern; registered at `peel/jpeg.rs:783`). Vectors through `add_part_encoded`
  (`writer.rs:2352`): `delta-versions.lpk` (three graphs, a dedup-reference base, an append); malformed
  vectors one per `patch` sub-reason (cover count, varint, `len` 0, span, sum, length) plus `base`, `chain`, `term`,
  `ChunkMismatch` on a damaged base and `BadGraph` `delta position`, all in `expected-1.5.toml`; the `_1_0` keys.
  Acceptance: a test per check; every earlier vector byte-identical; `lpk-decode` reports
  `UnimplementedPrimitive` 6; fuzz targets; fmt, clippy, nextest, deny; D-NN (2).
- **C — writer.** Version groups and the pairing map; the base bytes; the format-1 encoder moved from the probe; r and
  `delta_in_window`; the D-67 terms (`memory.rs:636`, `:656`, `:760`); the deferred derived chunks and the declared
  term in `lpk-format`; `lpk a|append --delta on|off`, on by default; header minor 5 when on; an append onto a
  pre-1.5 archive drops the stage; `lpk info` reports the derived-block count (headers read once); `lpk l` marks
  derived entries only behind `--long` (headers read then), never in the plain listing (a block's graph is only in
  its frame header, read whole when sealed; E3-1's streamed bound, open + 1 MiB); the A/B
  settings. Acceptance: a synthetic three-version tree derives the edited chunks only, byte-identical at 1 and 8
  threads; v3 derives from v1 where v2 was derived; an append with deltas rolled back to generation 0 bit-exactly;
  every class of `small` round-trips through the D-54 engine;
  `journal_corpus`'s excess printed both ways; D-NN (3).
- **D — engine and `lpk-check`.** Engine: the affinity dispatch; `nested_decodes` reported; counting-allocator tests at
  the term at 24 workers; selective extraction and export (`export.rs`) of one derived
  entry. `lpk-check`: a fresh implementer with the text, the vectors and D-49 only; `--revision 0..=5` (rebased on
  1.4's 4). Acceptance: every outcome of `expected-1.5.toml` at revision 5 and the `_1_0` keys at 0–4; the attestation
  of files read; crafted inputs agree with the reference. Final when it passes (D-78).
- **E — ordering.** `FileOrder::Versions` with its detection, `lpk a --ordering versions`, the setting
  `balanced-versions`. Acceptance: unit tests of the detection (threshold, exclusion, natural order, no sibling roots:
  bytes unchanged); the runner's A/B on `small` (`balanced` against `balanced-versions`, every class, extractions
  verified, committed): no class larger beyond the frame overhead; D-NN (4), amending D-55.
- **F — the measurement.** E3-9's form on `full`, both tiers, pooled with E0-2's baseline (D-74); the report's G2
  line, the deduplicating rows as reference. Acceptance: PLAN E2-9 ticked with the directory; D-NN (5);
  `docs/STATUS.md`.

**D-NNs:** (1) the pre-gate; (2) revision 1.5; (3) the writer's rules; (4) the ordering default; (5) the measured
state; the LICENSING row only on counsel's word.

**What stays out of Phase 1.** Format 0's decoding; chains deeper than 1; several bases per block (v2); renamed files
and similarity pairing (D-55); base retention in the engine; deltas on peeled parts; `lpk-decode` stays a 1.0 reader
(D-49 (4)); whole-file deltas (D-67).
