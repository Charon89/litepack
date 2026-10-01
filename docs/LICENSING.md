# Licensing policy and freedom-to-operate (FTO) list

## Our licence
Engine, CLI, format spec, benchmark harness: **Apache-2.0 OR MIT** (dual, like most of the Rust ecosystem). GUI Pro/enterprise features may be proprietary (open-core, D-01).

## Dependency policy (enforced by `cargo deny check`)
- Allowed: Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause, 0BSD, ISC, Zlib, CC0-1.0, Unicode-3.0, MPL-2.0 (as an unmodified dependency), BSL-1.0.
- Allowed with a named exception in `deny.toml` (we are open source, so LGPL's relink requirement is met by publishing source): LGPL-2.1 / LGPL-3.0 crates such as `cabac` (pulled in by `preflate-rs`), `packMP3` bindings (if used), `bzip3` (optional).
- Denied: GPL-2.0, GPL-3.0, AGPL, SSPL, proprietary. GPL projects may be *benchmarked* as external tools and *read for understanding only* — never copied, translated or "clean-room ported" by someone who read them.
- Never: `jpegxl-rs` / `jpegxl-sys` (GPL-3.0-or-later). libjxl itself is BSD-3; if ever needed, use `jxl-sys` (MIT/Apache) or build bindings in-house.

## Components planned for Phase 1 (licence · how used)
| Component | Licence | Use |
|---|---|---|
| preflate-rs 0.7.x (Microsoft) | Apache-2.0 (dep `cabac` LGPL-3.0-or-later) | depend |
| lepton_jpeg 0.5.x (Microsoft) | Apache-2.0 | depend |
| zstd / zstd-safe (libzstd 1.5.7) | BSD-3 (crate MIT) | depend |
| liblzma crate (xz 5.8) | 0BSD / MIT-Apache | depend |
| ruzstd | MIT | depend (sandboxed decode) |
| brotli (Dropbox) | BSD-3 / MIT | depend (ZIP/HTTP interop) |
| fastcdc, gearhash | MIT | depend |
| blake3 | CC0-1.0 / Apache-2.0 | depend |
| RustCrypto aes-gcm, chacha20poly1305, argon2 | MIT / Apache-2.0 | depend |
| reed-solomon-simd | MIT AND BSD-3 | depend |
| libsais-rs | Apache-2.0 | depend (text BWT) |
| libbsc 3.3 (C++) | Apache-2.0 | FFI, sandboxed worker only |
| OpenZL 0.3 (Meta, C11) | BSD | FFI, Phase 2 |
| kanzi-cpp | Apache-2.0 | FFI, Phase 2 |
| windows / windows-core (windows-rs) | MIT / Apache-2.0 | depend |
| wasmtime | Apache-2.0 WITH LLVM-exception | depend (plugins only, never inside archives) |
| Tauri 2 | MIT / Apache-2.0 | depend (GUI) |
| libarchive, 7-Zip SDK | BSD-2 / LGPL-2.1 (+unRAR clause) | sandboxed worker only; RAR *reading* only under unRAR terms |
| paq8px, cmix, Hutter Prize entries, packMP3 (source) | GPL-3.0 / LGPL-3.0 | benchmark tools and reading only |

## Patents to review before the phase named (counsel, not engineers, decide)
| Item | Holder / number | Relevant to | Action |
|---|---|---|---|
| Integer neural networks for entropy coding | Google, US 12,154,304 (priority 2018) | Research tier only | FTO or licence before any neural tier ships (D-04) |
| Sample-based compression-algorithm selection and tiering | IBM, US 8,688,654 (reported active to ~2031) | Model-stage router ("trial on a sample") | Review before E2; design around if needed (e.g., classifier-based routing without sampling-and-trying) |
| Delta compression of similarity-clustered chunks | EMC/Dell, US 9,798,731 | Fold near-duplicate deltas | Review before E2 |
| ML-predicted compression algorithm selection | Dell, US 12,111,791 | Router | Review before E2 |
| rANS patent | Microsoft, US 11,234,023 (disputed) | tANS/rANS in zstd/own coders | Note only; industry ships regardless |
| RaptorQ | Qualcomm IPR (RFC 6330 covenant) | Recovery | Excluded (D-11) |
| PMATIC-style mismatch-tolerant coding | MIT (possible filing) | Research tier | Check before Phase 3 |
Clear: Reed-Solomon/PAR2 (public domain), classic arithmetic coding patents (expired), zstd, Brotli, FLAC, BWT, CDC (FastCDC paper; check Gear-hash claims only if a patent surfaces).

## Other compliance
- Crypto export: US EAR §740.17 mass-market self-classification / open-source notification; Canadian ECL Group 1 — complete before public binaries ship (E7).
- Model weights (research track): training-data licences and EU AI Act transparency obligations to be reviewed before any weights are distributed.
