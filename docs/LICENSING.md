# Licensing policy and freedom-to-operate (FTO) list

Facts in the two tables below were checked on 2026-10-02 against the sources linked in each row
(crates.io, the projects' own licence files, Google Patents, IETF IPR disclosures). Where something
could not be checked, the row says so. Re-check a row before the component it names is introduced:
licences and versions change.

## Our licence
Engine, CLI, format spec, benchmark harness: **Apache-2.0 OR MIT** (dual, like most of the Rust ecosystem). GUI Pro/enterprise features may be proprietary (open-core, D-01).

## Dependency policy (enforced by `cargo deny check`)
- Allowed — `deny.toml` is the authoritative list: Apache-2.0, Apache-2.0 WITH LLVM-exception, MIT, MIT-0, BSD-2-Clause, BSD-3-Clause, 0BSD, ISC, Zlib, CC0-1.0, Unicode-3.0, Unicode-DFS-2016, MPL-2.0 (as an unmodified dependency), BSL-1.0.
- Allowed with a named exception in `deny.toml`, one entry per crate name: LGPL-2.1-or-later / LGPL-3.0-or-later crates. We publish the complete source of the binaries that contain them, which is how LGPL's relink requirement is met. This holds only for binaries whose complete source we publish; closed-source (GUI Pro) code reaches LGPL components only through the open-source engine as a separate process or DLL. Planned: `cabac` (required by `preflate-rs`; the exception is added in the change that introduces `preflate-rs`). Possible later: `packMP3` bindings, `bzip3`. LGPL code is depended on, never vendored or copied.
- Denied: GPL-2.0, GPL-3.0, AGPL, SSPL, proprietary. GPL projects may be *benchmarked* as external tools and *read for understanding only* — never copied, translated or "clean-room ported" by someone who read them.
- Never: `jpegxl-rs` / `jpegxl-sys` (GPL-3.0-or-later). libjxl itself is BSD-3-Clause; if ever needed, use `jxl-sys` (MIT OR Apache-2.0) or build bindings in-house.
- A crate that offers a choice (`A OR B`) is used under the allowed alternative; a crate that combines licences (`A AND B`) needs every one of them on the list, and the notices of each are reproduced.

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
| SeqCDC and VectorCDC chunking (Udayashankar, Baba, Al-Kiswany, University of Waterloo; Middleware 2024 and FAST 2025) | our own implementation from the papers; the authors' reference code (UWASL dedup-bench) is Apache-2.0 | reference-only for the reference code | https://github.com/UWASL/dedup-bench |
| blake3 1.8.7 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | depend | https://crates.io/crates/blake3 |
| RustCrypto aes-gcm 0.11.1, chacha20poly1305 0.11.0, argon2 0.6.0 | aes-gcm and chacha20poly1305: Apache-2.0 OR MIT; argon2: MIT OR Apache-2.0 | depend | https://crates.io/crates/aes-gcm · https://crates.io/crates/chacha20poly1305 · https://crates.io/crates/argon2 |
| reed-solomon-simd 3.1.0 | MIT AND BSD-3-Clause | depend (recovery records, D-11) | https://crates.io/crates/reed-solomon-simd |
| libsais 0.2.0 with libsais-sys (binding to the C library libsais) | crates: MIT OR Apache-2.0; libsais: Apache-2.0 | link (text BWT) — candidate; E2 chooses between this and the next row | https://crates.io/crates/libsais |
| libsais-rs 0.2.2 (pure-Rust translation of libsais; its README calls it "LLM-mediated" and not endorsed by the original authors) | Apache-2.0 | depend (text BWT) — candidate; must be tested against the C library before it is trusted | https://crates.io/crates/libsais-rs |
| libbsc 3.3.12 (C++) | Apache-2.0 | link, sandbox | https://github.com/IlyaGrebnov/libbsc |
| bzip3 1.5.4 | LGPL-3.0-only (the libsais and LZP code inside it is Apache-2.0) | link, optional, not planned for Phase 1; only through a separately published crate with its own named `deny.toml` exception | https://github.com/kspalaiologos/bzip3 |
| HDiffPatch 5.1.3 | MIT (its licence file also carries libdivsufsort's MIT notice; both must be reproduced) | link — candidate for delta coding; E2 chooses between it, zstd's patch-from mode and a delta coder of our own | https://github.com/sisong/HDiffPatch |
| zip 8.6.0 | MIT (optional codec features pull in further crates; each is checked by `cargo deny` when enabled) | depend (ZIP reading and writing; whether foreign-archive parsing runs in-process or inside the sandboxed worker is decided in E4) | https://crates.io/crates/zip |
| sevenz-rust2 0.23.0 | Apache-2.0 | depend (7z reading; in-process or sandboxed is decided in E4) | https://crates.io/crates/sevenz-rust2 |
| ppmd-rust 1.5.0 (Rust port of the PPMd code in 7-Zip; its README says the original PPMd authors put their code in the public domain) | CC0-1.0 OR MIT-0 | depend (PPMd inside ZIP and 7z; in-process or sandboxed is decided in E4) | https://crates.io/crates/ppmd-rust |
| libjxl 0.12.0 through jxl-sys 0.1.12 | libjxl: BSD-3-Clause; jxl-sys: MIT OR Apache-2.0 | link, only if a PNG/JPEG XL chain is adopted; never `jpegxl-rs` or `jpegxl-sys` | https://github.com/libjxl/libjxl · https://crates.io/crates/jxl-sys |
| OpenZL 0.3.0 (Meta; C11 and C++17) | BSD-3-Clause (its LICENSE file is the BSD-3-Clause text) | link, Phase 2 | https://github.com/facebook/openzl |
| kanzi-cpp 2.6.0 | Apache-2.0 | link, Phase 2 | https://github.com/flanglet/kanzi-cpp |
| libcubwt 1.6.3 (GPU BWT) | Apache-2.0; building and running it needs NVIDIA's proprietary CUDA toolkit | link, optional, not Phase 1 | https://github.com/IlyaGrebnov/libcubwt |
| windows 0.62 (windows-rs) | MIT OR Apache-2.0 | depend | https://crates.io/crates/windows |
| rappct 0.13.3 (AppContainer helper; small single-maintainer crate, last release 2025-10) | MIT | depend, for the sandboxed worker (E4) — review its code, or replace it with direct `windows` calls, before relying on it | https://crates.io/crates/rappct |
| wgpu 30 | MIT OR Apache-2.0 | depend, research track only, not Phase 1 | https://crates.io/crates/wgpu |
| ort 2.0.0-rc.13 with ONNX Runtime 1.30.0 | crate: MIT OR Apache-2.0 (release candidates only, no stable release); ONNX Runtime: MIT | depend, research track only, not Phase 1 (D-04) | https://crates.io/crates/ort · https://github.com/microsoft/onnxruntime |
| wasmtime 49 | Apache-2.0 WITH LLVM-exception | depend (app-side plugins only, never inside archives, D-03) | https://crates.io/crates/wasmtime |
| tauri 2.12 | Apache-2.0 OR MIT | depend (GUI) | https://crates.io/crates/tauri |
| libarchive 3.8.9 | BSD-2-Clause for most files; some files BSD-3-Clause, public domain, or CC0-1.0/OpenSSL/Apache-2.0 (BLAKE2 sources). No LGPL code | link, sandbox | https://github.com/libarchive/libarchive |
| LZMA SDK 26.03 | public domain | link, sandbox | https://www.7-zip.org/sdk.html |
| 7-Zip 26.03 source (beyond the LZMA SDK) | LGPL-2.1-or-later for most files; BSD-3-Clause and BSD-2-Clause parts; plus the unRAR restriction | link, sandbox, via a separately published `-sys` crate with its own named `deny.toml` exception (never vendored here), only if the LZMA SDK is not enough; RAR *reading* only. The unRAR code may not be used to re-create the RAR compression algorithm; if modified unRAR sources are distributed, the documentation and source comments must state that the code may not be used to develop a RAR (WinRAR) compatible archiver | https://www.7-zip.org/license.txt |
| libFLAC 1.5.0 and WavPack 5.9.0 (designs studied for our own audio predictors) | libFLAC: BSD-3-Clause (the `flac` tools are GPL and the documentation GFDL); WavPack: BSD-3-Clause | reference-only | https://github.com/xiph/flac · https://github.com/dbry/WavPack |
| paq8px | GPL-2.0-or-later | reference-only | https://github.com/hxim/paq8px |
| cmix, Hutter Prize entries | cmix: GPL-3.0-only per its licence file and README (source file headers not checked); other entries vary | reference-only | https://github.com/byronknoll/cmix |
| packMP3: the original (unmaintained since 2016) and the YadeWira fork (updated 2026-08) | original: LGPL-3.0-or-later; fork: LGPL-3.0 (whether "or later" was not checked) | reference-only; a binding would need its own named exception | https://github.com/packjpg/packMP3 · https://github.com/YadeWira/packMP3 |
| xEnc3 (named in the method document as a context-mixing candidate) | no public source or licence found | reference-only (no source or licence found) | — |

Benchmark tools (7-Zip, WinRAR `rar`, zstd, xz, zpaqfranz, t-saur, tar, ffmpeg and the optional WinZip and
PowerArchiver command-line tools) are run as separate programs found on the machine. They are not linked,
bundled or redistributed, so their licences place no condition on our code; `docs/BASELINES.md` notes which
of them need a paid or trial licence to run.

## Patents to review (counsel, not engineers, decide)
This is a watch-list, not a legal opinion: engineers record what each patent is about and when counsel
must look at it. Titles, holders and dates are from Google Patents; the expiry dates are that site's
estimates and the status was not checked against USPTO fee records. "Subject" restates the first
independent claim — or, where marked, the abstract — in neutral words and says nothing about its scope.
"Planned feature it touches" lists where counsel should look, not where a claim applies.

| Patent | Holder | Priority · granted · estimated expiry | Subject | Planned feature it touches | Assignment |
|---|---|---|---|---|---|
| [US 12,154,304](https://patents.google.com/patent/US12154304B2/en) "Data compression using integer neural networks" | Google LLC | 2018-09-27 · 2024-11-26 · 2039-09-18 | Entropy-coding sequential data with a neural network that uses only integer parameters and integer operations to produce symbol probabilities | Neural tier (research track only, D-04) | **Not relevant to Phase 1** (the product has no neural tier, D-04). Needs counsel before any neural tier ships (Phase 3 at the earliest) |
| [US 8,688,654](https://patents.google.com/patent/US8688654B2/en) "Data compression algorithm selection and tiering" | IBM | 2009-10-06 · 2014-04-01 · 2031-08-27 | Selecting sample data at random, compressing the sample with several engines, picking the best ratio, then applying that engine to the whole data set | Model-stage router, store-or-compress gate, and the Peel net-gain gate (D-09) — each compares compression results | **Needs counsel before Phase 1 (E2).** The E2 classifier as outlined in PLAN.md uses magic bytes and byte statistics; counsel reviews every place where compression results are compared |
| [US 9,798,731](https://patents.google.com/patent/US9798731) "Delta compression of probabilistically clustered chunks of data" | Dell Products LP (EMC family) | 2013-03-06 · 2017-10-24 · 2035-02-17 | Computing randomised sketches per chunk, finding similar sketches probabilistically, storing one chunk and deltas for the others | Fold: similarity-based file ordering (D-12) and delta coding between similar files or chunks | **Needs counsel before Phase 1 (E2).** |
| [US 12,111,791](https://patents.google.com/patent/US12111791B2/en) "Using machine learning to select compression algorithms for compressing binary datasets" | EMC / Dell Products LP | 2020-12-07 · 2024-10-08 · 2043-07-11 | A storage compute node with a trained model that predicts compression efficiency per algorithm, and a recommendation engine that picks one | A learned router | **Not relevant to Phase 1** (the router uses no trained model). Needs counsel before a learned router (Phase 2 or later) |
| [US 11,234,023](https://patents.google.com/patent/US11234023B2/en) "Features of range asymmetric number system encoding and decoding" | Microsoft Technology Licensing LLC | 2019-06-28 · 2022-01-25 · 2039-06-28 | From the abstract and the claims as shown: a hardware rANS decoder that organises its operations in phases, and adapting rANS coding per fragment of symbols (symbol width, choice among static probability models, flushing or keeping the decoder state) | ANS-family entropy coding: zstd's FSE coder (bundled from Phase 1) and any in-house rANS coder | **Needs counsel before public binaries ship (E7, Phase 1)** for the bundled zstd; needs counsel before an in-house rANS coder (Phase 2 or later). The inventor of ANS has criticised the patent publicly ([The Register, 2022-02-17](https://www.theregister.com/2022/02/17/microsoft_ans_patent/)) |
| RaptorQ: [IETF IPR disclosure 2554](https://datatracker.ietf.org/ipr/2554/) for RFC 6330 (lists US 7,139,960, US 7,451,377 and others) | Qualcomm | — | Summary of the declaration; read the original. It is conditional: a non-assert for non-wireless implementations of RFC 6330 with carve-outs (including a reservation against parties that assert their own patents against Qualcomm), and licensing under Qualcomm's terms for wireless wide-area devices | Recovery records | **Not relevant: RaptorQ is excluded (D-11);** recovery uses Reed-Solomon. Needs counsel before any use of RFC 6330 codes |
| PMATIC, mismatch-tolerant coding for model-driven compression | Academic authors (A. Adler, J. Tang) | — | Published papers ([arXiv 2601.10678](https://arxiv.org/abs/2601.10678), 2601.17684; ICML 2026); no patent search was done, so whether a filing exists is unknown | Research tier | **Not relevant to Phase 1.** Search for filings before Phase 3 |

No patent is known to us for the following; this is engineering knowledge, not the result of a patent search,
and counsel should confirm it before public binaries ship (E7): Reed-Solomon coding and PAR2, classic
arithmetic coding (the well-known patents are reported expired; not checked), FLAC, the Burrows-Wheeler
transform, and content-defined chunking as published (FastCDC, Gear hash, SeqCDC, VectorCDC).
Two libraries deserve an exact statement rather than "clear": **zstd** is licensed BSD-3-Clause OR GPL-2.0-only;
its repository has carried no separate patent grant since the licence change of v1.3.1 in 2017
([changelog](https://github.com/facebook/zstd/blob/dev/CHANGELOG), [licence](https://github.com/facebook/zstd/blob/dev/LICENSE)); **Brotli** is MIT-licensed without patent
text, and Google filed IETF IPR declarations for it ([2396](https://datatracker.ietf.org/ipr/2396/), offering
royalty-free, reasonable and non-discriminatory terms, and [3147](https://datatracker.ietf.org/ipr/3147/) for a
shared-dictionary application).

## Other compliance
- Crypto export: US EAR §740.17 mass-market self-classification / open-source notification; Canadian ECL Group 1 — complete before public binaries ship (E7).
- Model weights (research track): training-data licences and EU AI Act transparency obligations to be reviewed before any weights are distributed.
