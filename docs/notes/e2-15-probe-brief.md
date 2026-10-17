# E2-15 — the BWT measured gate: a probe of what LitePack would ship (brief; written 2026-10-17, dispatched after the overnight run of 2026-10-17)

PLAN E2-15: "libsais (Apache-2.0) in a `lpk-codecs-bwt` crate, applied only to text-classified streams (D-05), block
size under the envelope cap, with a measurement first: BWT+entropy coding against the LZMA-class row on `text-prose`
and `logs-text`. Acceptance: the task ships only if the measured row is smaller than the LZMA-class row on both text
classes with extraction time reported; otherwise the route stays LZMA and the result is recorded as a decision".

Facts that shape the measurement:
- The format registers `bwt` (0x0003, `block_size: u32`, resources "bwt block = block_size") without a decoding;
  shipping it is a revision (D-49): a decoding written out, an entropy stage it pairs with (the registry has no
  BWT-specific entropy primitive; a graph `[M, bwt]` with M = zstd/lzma is what the format can express today), a
  vector, the independence gate. None of that is done before the gate opens.
- `bsc` (the BWT reference compressor) is not installed and installing it is an open owner question; the gate is
  measured with code we would ship, not with an external tool.

The probe (`lpk-bench probe bwt`, a new probe beside `probe deflate`/`probe jpeg`; the codec in a new crate
`crates/lpk-codecs-bwt` so it can ship later; libsais through a `-sys` crate or the published Rust binding whose
licence passes `cargo deny`):
1. Per class (`text-prose`, `logs-text`; `small` first, `full` if the small result is close), the files in the
   order the Balanced tier would see them, concatenated into blocks of the Balanced block size (or the envelope's
   BWT cap if smaller).
2. Three candidates per block, each round-tripped and verified: (a) BWT (libsais) then the best format-expressible
   stage — LZMA and zstd-ultra on the BWT output (the `[M, bwt]` graph); (b) BWT then a dedicated entropy coder written
   in the crate — MTF, zero-run coding, an adaptive binary arithmetic coder with order-1 contexts on the MTF ranks
   (bzip2's structure with arithmetic coding), the candidate a revision would add as a primitive; (c) the Balanced
   tier's own LZMA on the same blocks (the reference row, which must equal the runner's `lpk/balanced` size within
   the frame overhead, as a check).
3. Report per class: total compressed bytes per candidate, compress and decompress seconds (release, single block
   per thread), and the gate verdict: (a) or (b) smaller than (c) on both classes.

Acceptance of this step: the probe's JSON committed under `bench/results/<date>-<host>/probe-bwt.json` and a report
section; D-05 confirmed (LZMA stays) or amended (the gate opens, and the revision 1.3 design for BWT follows as the next
task). No figure outside the committed JSON and the report.

## From the research of 2026-10-17 (a researcher agent's answer with primary sources: crates.io, docs.rs, the libsais and libbsc repositories)
- Binding: `libsais` 0.2.0 (feldroop, MIT OR Apache-2.0), a safe wrapper over `libsais-sys` 0.2.0 (same licence),
  which compiles libsais's C sources with `cc` (no MSVC-specific code; keep the `openmp` feature off). The wrapper
  exposes BWT construction (`Bwt`, `BwtWithAuxIndices`); an inverse BWT is ours to write in safe Rust (or the
  wrapper's if it has one). Our crate keeps `#![forbid(unsafe_code)]` if it calls only the safe wrapper. Run
  `cargo deny check` on the addition; record in `docs/LICENSING.md` that libsais itself is "Apache License 2.0 with
  LLVM Exceptions" (more permissive than Apache-2.0; the crate metadata says MIT OR Apache-2.0).
- libbsc (QLFC) is Apache-2.0 (Copyright 2009-2025 Ilya Grebnov): a Rust port is allowed with the notice kept and the
  changes stated; port it only if the simple coder's gap to LZMA justifies it.
- Entropy stage for the probe's candidate (b): MTF (or rank coding), zero-run coding, an adaptive binary arithmetic
  coder with order-0/1 contexts — expected between bzip2 and bsc (no published size table found; the probe measures).
