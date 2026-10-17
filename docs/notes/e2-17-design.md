# E2-17: text encodings (base64, UTF-16) — a probe first, a revision only if a gate opens (design)

Drafted 2026-10-17 by a read-only planning agent against `main` 8aaa72e and `task/e2-15-probe` 96d931f (spec sections 2,
7–9, 12, 16; D-09, D-45, D-49, D-68 to D-71, D-73; the E2-16 and 1.3 designs). No harness was run for this note: sizes
come from the `small` manifest, the PEM and BOM facts from reading those files, and the one ratio is arithmetic on them
and a committed row.

## 0. In short
1. No format change first: `lpk-bench probe text-encodings` counts base64 runs and UTF-16 texts per class and prices the
   writer that would ship against today's Balanced archive; the transforms live in `lpk_core::peel::text` from the start.
2. Gate per primitive: the predicted saving reaches the owner's margin on a class of `small`, and no class gets larger.
   `small` holds base64 only in two PEM bundles and no UTF-16 text; even if the PEM text cost nothing to store, the saving
   would be about 1.3% of its class's row. Both gates are expected to close.
3. If a gate opens, the frozen record bodies suffice for regular, canonical streams; no new bound, memory term, error class
   or library is needed; the primitive joins 1.3 if decided before 1.3's text is written, otherwise 1.4. If both close: a
   measured negative result, both primitives stay reserved, and 1.3 does not wait for E2-17.

## 1. The measurement (no format change)
**Where they live.** In user data: MIME parts of mail (76-character lines, CRLF or LF); PEM bundles (64-character lines);
data URIs in HTML, SVG and CSS; unbroken base64 in JSON and XML (notebooks, glTF, `.resx`); Windows UTF-16LE text (Registry
exports, Windows PowerShell 5.1 redirection, INF files). In `small`: `software-installed/mingit/ucrt64/etc/ssl/cert.pem`
(identical to `certs/ca-bundle.crt`: 185,311 bytes, 121 PEM blocks, 64-character LF lines; dedup keeps one copy) and
`certs/ca-bundle.trust.crt` (278,680 bytes, 172 blocks, largely repeating `cert.pem`'s lines). No data URI in any HTML file;
the one `.reg` is REGEDIT4 ASCII; none of the text-like files read is UTF-16; there is no mail, notebook or Windows-text
class; UTF-16 runs inside binaries (PE resources, OLE documents) are short — counted, never peeled.

**What decoding gains.** (a) Already-compressed payloads (JPEG, PNG, ZIP, PDF; most base64): only what LZMA loses coding a
64-symbol alphabet as 8-bit literals, plus the line breaks, minus the stream's cost. (b) Compressible payloads (DER
certificates, raw fonts, text): more — a repeat at a byte distance not divisible by 3 looks different in base64, and line
breaks cut matches. (c) Peelable payloads (an embedded JPEG or PNG): the gain needs a nested peel v1 cannot express
(section 6). UTF-16: ASCII-heavy text halves and LZMA's literal context becomes useful again; part of this is reachable
without a revision by setting `lp`/`pb` on a UTF-16 cluster; CJK text grows by half in UTF-8.

**Cost per stream, exact.** A base64 record takes 82 bytes of the `Records` frame (kind 2, flags 2, `body_len` 1, body 45,
`body_hash` 32), a utf16 record 79; both add the Deflate stage's `STREAM_OVERHEAD` of 160 bytes. A graph covers its whole
block, so each stream is a block coded alone: a PEM bundle's 121 runs pay 121 records and lose the certificates' shared
structure.

**The probe.** `crates/lpk-bench/src/probe/text_encodings.rs`, registered beside `deflate` and `jpeg`, writing
`probe-text-encodings.{json,md}`; clean release build; `small` decides the gate, `full` informative; private manifests work
with names withheld. It scans base64 in `Text` files and text-like `Other` files (printable share ≥ 0.9), UTF-16 texts in
every file (BOM, or alternating zero bytes), and counts only UTF-16 runs in binaries and base64 in depth-1 inflated texts. A
candidate is a maximal run of at least 256 characters over one RFC 4648 alphabet, labelled by what precedes it (MIME, PEM,
`;base64,`, JSON, XML, bare; hex-only runs counted apart). A run is **regular** when every line but the last has the same
length, one line ending (LF or CRLF) separates the lines and none follows the last line, `=` appears only as final padding,
and the unused bits of the last quantum are zero; anything else is counted by cause. Each regular run is decoded and
re-encoded from its record's fields; a mismatch fails the run.

**Rows** (the stage's `PartEncoder::balanced`): per stream M(text alone), M(payload), the payload's length, the record, the
overhead, the payload's class; per block on the Balanced tier's own clusters **A** (today's archive, from a real Balanced
archive's index), **B-cut** (the writer that would ship: each block without its kept runs, split at each peeled file —
D-70's cut — plus parts, records and overhead, under the stream gate, the file gate and the share rule) and **B-defer** (the
same without the split, task W). UTF-16 per file: **A**, **A'** (a UTF-16 cluster over a grid of LZMA `lc/lp/pb`, no
revision), **B** (per-file `[M, utf16]`), **B'** (one `[lzma, utf16]` stream per cluster block); the CJK share reported. The
stream gate uses M(text alone) as today's cost, checked against B-cut − A.

**The gate.** Per primitive: open when B-cut saves the owner's margin (question 1) on a class of `small` and no class is
predicted larger. By arithmetic the distinct PEM text (463,991 bytes) is 1.3% of `software-installed`'s `lpk/balanced`
bytes (27.72% of 126,846,043; `bench/results/2026-10-17-megatron-12`): a 1% margin would need more than three quarters of
that text saved, while today's coding already costs less than its raw size and each certificate's key and signature do not
compress. UTF-16 has no stream in `small`.

## 2. What the inversion must record (section 12's frozen bodies)
**base64** (`variant`, `line_len`, `line_ending`, `padding`, `original_len`, `original_hash`) suffices for a regular,
canonical run. It cannot express irregular or mixed line breaks or a bare CR, indentation, escaped breaks (`&#xA;`, JSON
`\n`), padding inside a run, nonzero unused bits, or other alphabets; no field can be added (every reader since 1.0 reports
bytes after a body's last field as `TrailingBytes`), so such runs stay framing or are split into regular runs each paying a
record and a block. The revision states the line rule exactly (no line ending after the last line), refining 1.0's
"characters per line" (no earlier reader applies the record, so no outcome changes).
**utf16** (`endian`, `bom`, `original_len`, `original_hash`) suffices once the step's input is defined: UTF-16 → UTF-8 is
lossless only for well-formed text (an unpaired surrogate has no UTF-8 form), so the input is *generalized UTF-8* — UTF-8's
shortest forms of every code point up to 0x10FFFF with the surrogates D800–DFFF allowed as three-byte forms (WTF-8, cited
for information); every sequence of 16-bit units maps to exactly one byte string and back; an odd last byte stays framing;
`bom` 1 puts U+FEFF first in the record's byte order, counted in `original_len`.

## 3. Decoding (the revision text, only for a gate that opens)
**`base64` (0x000A).** Input: the payload, n bytes; output: the text, `original_len` bytes. Checks: (1) record lookup;
(2) kind (`BadRecord` `kind`); (3) `original_len` against the output bound (last step ≠ `plain_len` `BlockLengthMismatch`,
otherwise above the bound `PayloadTooLarge`); (4) L = c + e·b with c = 4⌈n/3⌉ if `padding` else 4⌊n/3⌋ + (0, 2, 3)[n mod 3],
b = ⌈c/`line_len`⌉ − 1 when both are nonzero else 0, e = 1 (LF) or 2 (CRLF); L ≠ `original_len` or an overflow is
`BadRecord` `original_len`; (5) encode by RFC 4648 (cited normatively) with the record's alphabet, unused bits zero, `=`
exactly when `padding`, a line ending after every `line_len` characters except the last; (6) BLAKE3 against
`original_hash`.
**`utf16` (0x000B).** Input: generalized UTF-8; output: UTF-16 in the record's `endian`, U+FEFF first when `bom`. Checks
(1)–(3) as base64; (4) an odd `original_len`, or `bom` 1 with fewer than 2 bytes, is `BadRecord` `original_len`; (5) convert
by a table written out in the spec (RFC 3629's plus the three-byte surrogates; code points above 0xFFFF become a pair as in
RFC 2781): `BadRecord` `input` (a new reason) for a byte starting no shortest form, a bad continuation, an overlong form, a
code point above 0x10FFFF or input ending inside a sequence; `BadRecord` `original_len` for output that would pass
`original_len` (judged after the sequence's validity) or ends short; (6) BLAKE3.
**Bounds and resources: nothing new.** A base64 input is always shorter than its output; a `utf16` input is an intermediate
bounded by `max_block_plain`, which a writer MUST NOT exceed (the reference writer peels only texts whose generalized UTF-8
is not longer than their UTF-16, leaving CJK-heavy texts alone; if the probe shows them gaining, the alternative is a
1.2-style bound of ⌊3·`original_len`/2⌋ with the look-ahead, decided before the text is written). "Resources: none" stands.
**Joining 1.3** if decided before 1.3's task C text: one minor 3, separate subsections, no Bounds paragraph, nothing for
task A; vectors `base64-peel.lpk` (MIME CRLF/76, PEM LF/64, a data URI, a URL-safe unpadded run, a one-step block, a
nonzero-bits run left as framing, a plain file) and `utf16-peel.lpk` (LE with BOM, supplementary characters and lone
surrogates, BE without BOM, an odd-length file), two malformed vectors, one `expected-1.3.toml` (`_1_0` keys:
`UnimplementedPrimitive` 10 and 11), one `CHANGES.md` entry, one D-entry, one lpk-check task E. Decided later: 1.4.

## 4. The writer and the pipeline
Detection as in the probe; UTF-16 texts get a classifier rule and a cluster (D-45 left them `Other` "until the
text-encodings task"); runs inside binaries never peeled; each stream re-encoded and compared before it is kept (D-09).
Gates (D-69, D-70): the exact pre-gate, the stream gate (min(M(payload), payload length) + record + 160 below today's cost),
the records cap, the file gate, the share rule. **Task W** (D-70's follow-up) defers part blocks until the solid block
closes (the format allows it; the Deflate peel gains too). Layout: framing first as plain parts, then per kept stream its
record and block `[M, base64 {id}]`, `[base64 {id}]` or `[M, utf16 {id}]`; the writer refuses other positions, a kind
mismatch, `original_len` ≠ `plain_len`, minor < 3. The pool (D-71) prepares on workers and commits in order; on in Balanced,
off in Fast (`lpk a --text-peel on|off`); minor 3 when on; an append onto minor < 3 drops the stage and says so; control
setting `balanced-no-text-peel`.

## 5. Tasks and acceptance (P → gate → [W] → C → D ∥ E)
- **P — probe and transforms** (`crates/lpk-core/src/peel/text/`, `crates/lpk-bench/src/probe/text_encodings.rs`): RFC 4648
  section 10 vectors; property tests (payloads 0–300 bytes × both alphabets × `line_len` none/1/4/64/76/65535 × LF/CRLF ×
  padding); a test per irregular cause; random unit sequences (lone and paired surrogates, both orders, with and without
  BOM) against `std`; release runs committed with zero mismatches and A equal to the real archives; a D-entry. A closed
  gate stops here.
- **W — deferred part blocks**, only if B-defer beats B-cut by the margin (`crates/lpk-format/src/writer.rs`).
- **C — the revision** (1.3 or 1.4): spec, `CHANGES.md`, `CONFORMANCE.md`, vectors, decoders, `revision()` 3, writer rules,
  fuzz targets.
- **D — the pipeline**: the synthetic vectors through the D-54 engine and lpk-check; the gates; 1 vs 8 threads; sealed; an
  append onto minor 2; every class of `small` round-trips; on/off differences equal to B-cut; the A/B rows; D-NN.
- **E — lpk-check from the text alone** (1.3's task E if joined).
The PLAN's acceptance: synthetic vectors round-trip (D); A/B `balanced` against `balanced-no-text-peel` on every class of
`small`, ticked when no class is larger. If the gate closes: question 3.

## 6. Risks, and whether it is worth a revision
Prevalence: the corpus lacks the data where these encodings live; on `small` the probe can at best confirm the arithmetic.
Little gain per byte: most payloads are already compressed; the large gains (Lepton on an embedded JPEG, the Deflate peel
on an embedded PNG or ZIP) need nested peels v1 cannot write — v2 material. One stream per block: short runs pay about 240
bytes each and lose shared context; a peeled file cuts the solid block until W lands. Detection must stay linear.
Permanence: small but real. **Recommendation:** run P after E2-16's probe; do not hold 1.3 for it; if both gates close on
`small` (expected), close E2-17 as a measured negative result (a D-entry; the PLAN per question 3), keep `base64` and
`utf16` reserved, and list them in STATUS as 1.4/v2 candidates tied to question 2.

## 7. Owner questions
1. **The margin** (a gate threshold): recommended, open a primitive only if B-cut saves at least 1% of a class's
   `lpk/balanced` bytes (E2-10's committed rows) on `small`, with no class predicted larger.
2. **Corpus coverage**: decide on `small` as the PLAN says (expected: closed), or first add a class and re-run the probe for
   1.4 — a corpus spec change (candidates: public mailing-list mboxes with attachments, Jupyter notebooks, SVG/HTML with
   data URIs, builder-generated UTF-16 text such as Registry exports); a private scan (CORPUS.md's private mode) can inform
   either.
3. **A closed gate and the acceptance**: recommended, the synthetic clause met by P's transform tests on the same inputs,
   the "with" figure the probe's committed prediction and the "without" E2-10's rows, both recorded with the negative
   result.
