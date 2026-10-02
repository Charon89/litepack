# Licensing policy and freedom-to-operate (FTO) list

Facts in the two tables below were checked on 2026-10-02 against the sources linked in each row
(crates.io, the projects' own licence files, Google Patents, IETF IPR disclosures). Re-check a row
before the component it names is introduced: licences and versions change.

## Our licence
Engine, CLI, format spec, benchmark harness: **Apache-2.0 OR MIT** (dual, like most of the Rust ecosystem). GUI Pro/enterprise features may be proprietary (open-core, D-01).

## Dependency policy (enforced by `cargo deny check`)
- Allowed — `deny.toml` is the authoritative list: Apache-2.0, Apache-2.0 WITH LLVM-exception, MIT, MIT-0, BSD-2-Clause, BSD-3-Clause, 0BSD, ISC, Zlib, CC0-1.0, Unicode-3.0, Unicode-DFS-2016, MPL-2.0 (as an unmodified dependency), BSL-1.0.
- Allowed with a named exception in `deny.toml` (we are open source, so LGPL's relink requirement is met by publishing source): LGPL-2.1 / LGPL-3.0 crates, one entry per crate name. Planned: `cabac` (required by `preflate-rs`; the exception is added in the change that introduces `preflate-rs`). Possible later: `packMP3` bindings, `bzip3`. LGPL code is depended on, never vendored or copied.
- Denied: GPL-2.0, GPL-3.0, AGPL, SSPL, proprietary. GPL projects may be *benchmarked* as external tools and *read for understanding only* — never copied, translated or "clean-room ported" by someone who read them.
- Never: `jpegxl-rs` / `jpegxl-sys` (GPL-3.0-or-later). libjxl itself is BSD-3; if ever needed, use `jxl-sys` (MIT/Apache) or build bindings in-house.
- A crate that offers a choice (`A OR B`) is used under the allowed alternative; a crate that combines licences (`A AND B`) needs every one of them on the list.

## Components planned for Phase 1
How a component is used:
**depend** — a Cargo dependency compiled into our binaries; **link** — a C/C++ library reached through FFI;
**sandbox** — runs only inside the restricted worker process (E4); **reference-only** — read or benchmarked, never linked, copied or ported.

| Component (version checked) | Licence as declared | Use | Link |
|---|---|---|---|
| preflate-rs 0.7.6 (Microsoft) | Apache-2.0 | depend (Deflate peel) | https://crates.io/crates/preflate-rs |
| cabac 0.15.0 (required by preflate-rs) | LGPL-3.0-or-later | depend, under the named `deny.toml` exception | https://crates.io/crates/cabac |
| lepton_jpeg 0.5.8 (Microsoft) | Apache-2.0 | depend (JPEG peel) | https://crates.io/crates/lepton_jpeg |
| zstd 0.14.0, zstd-safe 8.0.0, zstd-sys 2.1.0 (bundles libzstd 1.5.7) | crates: BSD-3-Clause; libzstd: BSD-3-Clause OR GPL-2.0-only, used under BSD-3-Clause | depend | https://crates.io/crates/zstd |
| liblzma 0.4.8, liblzma-sys 0.4.9 (bundles XZ Utils 5.8.4 liblzma) | crates: MIT OR Apache-2.0; liblzma: 0BSD | depend, statically linked | https://crates.io/crates/liblzma |
| ruzstd 0.9.0 | MIT | depend, sandbox (decode of untrusted input) | https://crates.io/crates/ruzstd |
| brotli 9.0.0 (Dropbox) | BSD-3-Clause AND MIT | depend (ZIP/HTTP interop) | https://crates.io/crates/brotli |
| fastcdc 5.0.0 | MIT | depend (chunking) | https://crates.io/crates/fastcdc |
| gearhash 0.1.4 | MIT OR Apache-2.0 | depend (chunking) | https://crates.io/crates/gearhash |
| blake3 1.8.7 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | depend | https://crates.io/crates/blake3 |
| RustCrypto aes-gcm 0.11.1, chacha20poly1305 0.11.0, argon2 0.6.0 | MIT OR Apache-2.0 | depend | https://crates.io/crates/aes-gcm · https://crates.io/crates/chacha20poly1305 · https://crates.io/crates/argon2 |
| reed-solomon-simd 3.1.0 | MIT AND BSD-3-Clause | depend (recovery records, D-11) | https://crates.io/crates/reed-solomon-simd |
| libsais 0.2.0 with libsais-sys (binding to the C library libsais) | crates: MIT OR Apache-2.0; libsais: Apache-2.0 | link (text BWT) — candidate; E2 chooses between this and the next row | https://crates.io/crates/libsais |
| libsais-rs 0.2.2 (pure-Rust translation of libsais; its README describes it as machine-assisted and not endorsed by the libsais author) | Apache-2.0 | depend (text BWT) — candidate; must be tested against the C library before it is trusted | https://crates.io/crates/libsais-rs |
| libbsc 3.3.12 (C++) | Apache-2.0 | link, sandbox | https://github.com/IlyaGrebnov/libbsc |
| HDiffPatch 5.1.3 | MIT (its licence file also carries libdivsufsort's MIT notice; both must be reproduced) | link — candidate for delta coding; E2 chooses between it, zstd's patch-from mode and a delta coder of our own | https://github.com/sisong/HDiffPatch |
| zip 8.6.0 | MIT (optional codec features pull in further crates; each is checked by `cargo deny` when enabled) | depend (ZIP reading and writing) | https://crates.io/crates/zip |
| sevenz-rust2 0.23.0 | Apache-2.0 | depend (7z reading) | https://crates.io/crates/sevenz-rust2 |
| ppmd-rust 1.5.0 (Rust port of the PPMd code in 7-Zip, which its README describes as public domain) | CC0-1.0 OR MIT-0 | depend (PPMd inside ZIP and 7z) | https://crates.io/crates/ppmd-rust |
| OpenZL 0.3.0 (Meta; C11 and C++17) | BSD-3-Clause | link, Phase 2 | https://github.com/facebook/openzl |
| kanzi-cpp 2.6.0 | Apache-2.0 | link, Phase 2 | https://github.com/flanglet/kanzi-cpp |
| libcubwt 1.6.3 (GPU BWT) | Apache-2.0; building and running it needs NVIDIA's proprietary CUDA toolkit | link, optional, not Phase 1 | https://github.com/IlyaGrebnov/libcubwt |
| windows 0.62 (windows-rs) | MIT OR Apache-2.0 | depend | https://crates.io/crates/windows |
| rappct 0.13.3 (AppContainer helper; small single-maintainer crate, last release 2025-10) | MIT | depend, for the sandboxed worker (E4) — review its code, or replace it with direct `windows` calls, before relying on it | https://crates.io/crates/rappct |
| wgpu 30 | MIT OR Apache-2.0 | depend, research track only, not Phase 1 | https://crates.io/crates/wgpu |
| ort 2.0.0-rc.13 with ONNX Runtime 1.30.0 | crate: MIT OR Apache-2.0 (release candidates only, no stable release); ONNX Runtime: MIT | research track only, not Phase 1 (D-04) | https://crates.io/crates/ort · https://github.com/microsoft/onnxruntime |
| wasmtime 49 | Apache-2.0 WITH LLVM-exception | depend (app-side plugins only, never inside archives, D-03) | https://crates.io/crates/wasmtime |
| tauri 2.12 | Apache-2.0 OR MIT | depend (GUI) | https://crates.io/crates/tauri |
| libarchive 3.8.9 | BSD-2-Clause for most files; some files BSD-3-Clause, public domain, or CC0-1.0/OpenSSL/Apache-2.0 (BLAKE2 sources). No LGPL code | link, sandbox | https://github.com/libarchive/libarchive |
| LZMA SDK 26.03 | public domain | link, sandbox | https://www.7-zip.org/sdk.html |
| 7-Zip 26.03 source (beyond the LZMA SDK) | LGPL-2.1-or-later for most files; BSD-3-Clause and BSD-2-Clause parts; plus the unRAR restriction | link, sandbox, only if the LZMA SDK is not enough; RAR *reading* only. The unRAR code may not be used to re-create the RAR compression algorithm, and that statement must be kept in documentation and source comments | https://www.7-zip.org/license.txt |
| paq8px | GPL-2.0 | reference-only | https://github.com/hxim/paq8px |
| cmix, Hutter Prize entries | GPL-3.0 (cmix); varies per entry | reference-only | https://github.com/byronknoll/cmix |
| packMP3 source (unmaintained since 2016) | LGPL-3.0-or-later | reference-only; a binding would need its own named exception | https://github.com/packjpg/packMP3 |
| xEnc3 (named in the method document as a context-mixing candidate) | no public source or licence found | not a component | — |

Benchmark tools (7-Zip, WinRAR `rar`, zstd, xz, zpaqfranz, t-saur, tar, ffmpeg and the optional WinZip and
PowerArchiver command-line tools) are run as separate programs found on the machine. They are not linked,
bundled or redistributed, so their licences place no condition on our code; `docs/BASELINES.md` notes which
of them need a paid or trial licence to run.

## Patents to review (counsel, not engineers, decide)
This is a watch-list, not a legal opinion: engineers record what each patent is about and when counsel
must look at it. Titles, holders and dates are from Google Patents; the expiry dates are that site's
estimates and the status was not checked against USPTO fee records. "Subject" restates the first
independent claim in neutral words and says nothing about its scope.

| Patent | Holder | Priority · granted · estimated expiry | Subject | Planned feature it touches | Assignment |
|---|---|---|---|---|---|
| [US 12,154,304](https://patents.google.com/patent/US12154304B2/en) "Data compression using integer neural networks" | Google LLC | 2018-09-27 · 2024-11-26 · 2039-09-18 | Entropy-coding sequential data with a neural network that uses only integer parameters and integer operations to produce symbol probabilities | Neural tier (research track only, D-04) | **Not relevant to Phase 1.** Needs counsel before any neural tier ships (Phase 3 at the earliest) |
| [US 8,688,654](https://patents.google.com/patent/US8688654B2/en) "Data compression algorithm selection and tiering" | IBM | 2009-10-06 · 2014-04-01 · 2031-08-27 | Selecting sample data at random, compressing the sample with several engines, picking the best ratio, then applying that engine to the whole data set | Model-stage router and the store-or-compress gate, if either decides by trial compression of a sample | **Needs counsel before Phase 1 (E2).** Until then the router is designed around classification by magic bytes and byte statistics rather than compress-and-compare |
| [US 9,798,731](https://patents.google.com/patent/US9798731) "Delta compression of probabilistically clustered chunks of data" | Dell Products LP (EMC family) | 2013-03-06 · 2017-10-24 · 2035-02-17 | Computing randomised sketches per chunk, finding similar sketches probabilistically, storing one chunk and deltas for the others | Fold: delta coding between similar files or chunks | **Needs counsel before Phase 1 (E2)**, before any similarity-based delta ships. Exact-match chunk dedup and file ordering (D-12) are the baseline |
| [US 12,111,791](https://patents.google.com/patent/US12111791B2/en) "Using machine learning to select compression algorithms for compressing binary datasets" | EMC / Dell Products LP | 2020-12-07 · 2024-10-08 · 2043-07-11 | A trained model that predicts compression efficiency per algorithm, with a recommendation engine that picks one | A learned router | **Not relevant to Phase 1** (the router uses no trained model). Needs counsel before a learned router (Phase 2 or later) |
| [US 11,234,023](https://patents.google.com/patent/US11234023B2/en) "Features of range asymmetric number system encoding and decoding" | Microsoft Technology Licensing LLC | 2019-06-28 · 2022-01-25 · 2039-06-28 | An rANS encoder that selects one of several static probability models per fragment and signals the choice in the bitstream | An entropy coder of our own based on rANS | **Not relevant to Phase 1** (Phase 1 writes no rANS coder of its own). Needs counsel before an in-house rANS coder (Phase 2 or later). The inventor of ANS has objected to the patent publicly ([The Register, 2022-02-17](https://www.theregister.com/2022/02/17/microsoft_ans_patent/)) |
| RaptorQ: [IETF IPR disclosure 2554](https://datatracker.ietf.org/ipr/2554/) for RFC 6330 (lists US 7,139,960, US 7,451,377 and others) | Qualcomm | — | The declaration is conditional: a non-assert for non-wireless implementations of RFC 6330 with carve-outs, and licensing under Qualcomm's terms for wireless wide-area devices | Recovery records | **Not relevant: RaptorQ is excluded (D-11);** recovery uses Reed-Solomon. Needs counsel before any use of RFC 6330 codes |
| PMATIC, mismatch-tolerant coding for model-driven compression ([arXiv 2601.10678](https://arxiv.org/abs/2601.10678)) | MIT authors | — | Published papers only; no patent filing was found | Research tier | **Not relevant to Phase 1.** Search for filings before Phase 3 |

No patent is known to us for the following; this is engineering knowledge, not the result of a patent search,
and counsel should confirm it before public binaries ship (E7): Reed-Solomon coding and PAR2, classic
arithmetic coding (the well-known patents have expired), FLAC, the Burrows-Wheeler transform, and
content-defined chunking as published (FastCDC, Gear hash).
Two libraries deserve an exact statement rather than "clear": **zstd** is licensed BSD-3-Clause OR GPL-2.0-only;
its repository has carried no separate patent grant since the licence change of v1.3.1 in 2017
([changelog](https://github.com/facebook/zstd/blob/dev/CHANGELOG), [licence](https://github.com/facebook/zstd/blob/dev/LICENSE)); **Brotli** is MIT-licensed without patent
text, and Google filed IETF IPR declarations for it ([2396](https://datatracker.ietf.org/ipr/2396/), offering
royalty-free terms, and [3147](https://datatracker.ietf.org/ipr/3147/) for a shared-dictionary application).

## Other compliance
- Crypto export: US EAR §740.17 mass-market self-classification / open-source notification; Canadian ECL Group 1 — complete before public binaries ship (E7).
- Model weights (research track): training-data licences and EU AI Act transparency obligations to be reviewed before any weights are distributed.
