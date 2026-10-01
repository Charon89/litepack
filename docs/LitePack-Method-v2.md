# LitePack — Method and Build Plan, v2 (post-review)

*1 October 2026. This version replaces v1 after an independent four-stream review (fact check, prior-art attack, frontier scan, Windows/build-stack evaluation). Figures were re-checked against primary sources on this date; where a figure is a vendor or forum self-report it is marked as such.*

---

## 0. What the review changed (read this first)

The diagnosis in v1 survived: mainstream archivers share two LZ engines, store already-compressed files at ~100%, do not deduplicate across files, offer no authenticated encryption, and have a poor 2023–2026 security record. The *measured component gains* also survived (JPEG recoding ~22% on supported files, Deflate inversion, dictionaries on small files, dedup on versioned data). What did not survive is a set of overclaims, and v2 corrects them:

- **"No shipping product does this" was wrong.** PowerArchiver's `.pa` format (shipping, $22.95) already combines PDF/DOCX/PNG/JPEG/MP3 recompression with content-defined-chunking dedup and advertises "30–70% smaller than ZIP, 5–25% smaller than 7Z" on those formats. WinZip Zipx recompresses JPEG/MP3 ("up to 20%"). paq8px already does the PNG → pixels → image-model → exact-PNG chain (research tool, KB/s). pcompress (2012–2016) did dedup + similarity + delta + adaptive routing + authenticated encryption in one CLI archiver. zpaq has carried its decoder inside the archive since 2009. Apple's AEA archive format ships AEAD on a billion devices. And two 2026 open-source projects — **t-saur** (Rust, Apache/MIT, rc.2, verified on Windows 11) and **sqz-next** — already run preflate + Lepton + dedup + delta + BCJ + BLAKE3 pipelines. **Nothing in LitePack is a new algorithm; the opportunity is integration, measurement, security and product execution** (Section 7).
- **Several numbers were too generous.** The "15–40% smaller on typical user data" headline becomes **10–25% on photo/document-heavy sets, ~0% on video, 2×+ only on versioned backups** (Section 5). The preflate "0.01–2.7% overhead" applies only to recognised encoders (zlib, zlib-ng ≤8, libdeflate ≤9, miniz); zopfli/oxipng, 7-Zip, Java/Go/.NET Deflate, Deflate64 and preset-dictionary streams are not covered. The zpaq-vs-7-Zip "7.7× faster" compared against a 2007 single-threaded 7-Zip build, and zpaq extracts *slower*. "BWT+CM 15–30% smaller than LZMA at similar speed" is true on text only; on binaries bsc ≈ LZMA in size and decodes 4–5× slower. The 2026 Hutter winner is 100.4 MB under contest rules (97.0 MB is its unconstrained LTCB entry), was pretrained on enwik9 itself and "will compress anything else badly unless retrained".
- **Three design decisions reversed.** (1) The embedded-WASM decoder is removed from the archive format (SmartScreen/AV reputation, second sandbox to audit, non-deterministic relaxed-SIMD, conflicts with a small auditable reader); WASM stays as an app-side plugin sandbox only. (2) The neural "Max" tier leaves the format specification and becomes a research track gated by a conformance suite and a freedom-to-operate review (Google's integer-neural-network entropy-coding patent **US 12,154,304** is granted, priority 2018). (3) The Balanced tier is LZMA/zstd + filters + Peel + Fold; BWT is used only on streams the classifier marks as text.
- **Two licence problems found.** preflate-rs 0.7.6 has a *required* dependency on the `cabac` crate, which is **LGPL-3.0-or-later**; and the Rust JPEG XL wrapper `jpegxl-rs` is **GPL-3.0** (libjxl itself is BSD-3). Resolution in Section 11.
- **New methods adopted** (Section 6): Meta **OpenZL** 0.3 (BSD, 29 Sep 2026) as the engine for structured/numeric streams and as the template for the self-describing decode graph; an **AI-model-weights** data class (33–50% on bf16/fp16 tensors); **SeqCDC/VectorCDC** chunking (5–30 GB/s); **Palantir** super-feature resemblance detection plus suffix-array deltas (HDiffPatch/bsdiff class, ~29% smaller patches than zstd `--patch-from` on binaries); JPEG-peel hardening for the ~1% of in-the-wild JPEGs that fail bit-exact reconstruction (UltraHDR gain-map files, >4 MB trailing data); mismatch-tolerant coding (PMATIC, ICLR 2026) as the *primary* mechanism for any accelerator, since NPU int8 is not bit-exact across vendors.
- **Windows is confirmed as the primary platform** with a concrete, verified stack: Rust core (MSVC x64 + ARM64), Tauri 2 GUI (HTML/CSS/JS), a windows-rs COM shell extension for the Windows 11 context menu, sandboxed worker processes, MSIX + NSIS packaging, Azure Artifact Signing (individuals in Canada are eligible). Section 9.

---

## 1. How today's archivers build an archive

Every mainstream tool — 7-Zip 26.03, WinRAR 7.23, WinZip 30, PeaZip 11.3, Bandizip 7.46, NanaZip 6.5 (stable; 7.0 is a preview), Windows 11's built-in support and macOS Archive Utility — follows the same pipeline: sort files (by extension/name), concatenate them into solid blocks, optionally apply a byte-level filter (x86 E8/E9 or BCJ/BCJ2, ARM64/RISC-V, delta), run an LZ77 match finder over a dictionary window (7-Zip 26.x: 16 MB at level 5, 128 MB at 7, 256 MB at 9 on 64-bit, up to 3,840 MiB; WinRAR: 32 MB default, up to 64 GB), and entropy-code the output (LZMA's binary range coder; Huffman in RAR5/Deflate; tANS + 4-way Huffman in zstd, which is why zstd decodes ~10× faster than LZMA for ~7–8% larger output). Only PPMd (7-Zip's text mode) and the context-mixing family (zpaq, paq8px, cmix) skip LZ and predict bits directly. The container adds names, timestamps, checksums, optional AES and split volumes; only RAR adds Reed-Solomon recovery records. Nothing in this pipeline looks inside a JPEG, PDF or DOCX, nothing deduplicates beyond the window, and the whole ecosystem runs on two engines (Pavlov's LZMA, bundled even into WinRAR and Windows; and RAR5's LZSS).

---

## 2. Incumbents: strengths and weaknesses (verified)

| Product (latest) | Strengths | Weaknesses |
|---|---|---|
| **7-Zip 26.03** (3 Sep 2026), free, LGPL | Best mainstream ratio (Silesia: 7z-Ultra 23.5% vs RAR-Best 25.8%); open format; >64-thread scaling; reads everything | No recovery records; no dedup; ZIP creation defaults to broken ZipCrypto; LZMA extraction 80–120 MB/s per thread; no Windows 11 modern context menu (still under "Show more options"); no GUI outside Windows; no auto-update; 2026 heap overflows CVE-2026-48095 (NTFS) and CVE-2026-14266 (XZ decoder), MoTW bypasses CVE-2025-0411 (exploited) and CVE-2026-58052, symlink traversal CVE-2025-11001/11002 (exploited) |
| **WinRAR 7.23** (30 Jun 2026; 7.30 beta), $29 | Fastest strong archiver (RAR-Best creates 4.8× faster than 7z-Ultra for ~10% larger output); Reed-Solomon recovery records 3–1000% and .rev volumes; mature multi-volume/repair | Proprietary; RAR5 dropped the RAR4 text/audio/image models; no dedup; no auto-update or enterprise patch channel — CVE-2025-8088 (RomCom zero-day, Jul 2025) still exploited in 2026; CVE-2023-38831, CVE-2025-6218, CVE-2026-14191 (.rev heap overflow) |
| **WinZip 30** (Sep 2025), $29.95–$39.99 | The only mainstream tool with content-specific codecs (JPEG, MP3 "up to 20%", WavPack) | Zipx lock-in; upsell pop-ups; no dedup; no recovery records; closed codecs |
| **PowerArchiver** (.pa codec pack, $22.95 Business) | PDF/DOCX/PNG/JPEG/MP3 recompression + CDC dedup; "30–70% smaller than ZIP, 5–25% smaller than 7Z" on those formats (vendor claim) | Closed, Windows-only, niche; extension-driven; little visible development since ~2018 |
| **PeaZip 11.3** / **Bandizip 7.46** / **NanaZip 6.5** | Cross-platform (PeaZip); fast UI (Bandizip); modern Windows 11 integration + hardened MSIX build (NanaZip) | All inherit 7-Zip's engine and bugs; no dedup/recompression; Bandizip ads + paywalled repair |
| **Windows 11 built-in** (24H2/25H2; ~71% desktop share) | Free extraction of 7z/RAR/tar.*, creation of ZIP/7z/TAR; keeps adding formats (KB5083631, Apr 2026) | No encryption at all; no RAR creation, splitting or repair; 6–9× slower on 7z/RAR; its own libarchive CVEs with patches that lagged upstream 3–4 months |
| **Apple Archive / AEA** | AEAD-encrypted, signed, chunked archive format on every Mac/iPhone | No 7z/RAR; no recompression; Apple-only |
| **t-saur 1.0.0-rc.2** (Jan 2026, Rust, Apache/MIT) | Open-source preflate + Lepton + CDC dedup + delta + BCJ/ARM64 filters + BLAKE3; deterministic; 62.2% vs 7-Zip 73.6% on its corpus; Windows 11 x64 verified | CLI, no GUI, no recovery records, no encryption yet, rc status — but it is the closest living implementation of Peel + Fold and licence-compatible to learn from |

---

## 3. Benchmarks that matter (corrected)

**Silesia, single thread (lzbench README, AMD EPYC 9554):** zstd -22 24.7% at 2.1 MB/s compress / **1,073 MB/s** decompress; brotli -11 23.8%; xz -9 23.0% at 2.6 / 123 MB/s; lzma -9 23.0% at 4.0 / 93 MB/s; bsc (BWT) 23.2% at 16.6 / 24.1 MB/s; kanzi -7 22.3% at 10 / 15 MB/s; kanzi -9 (CM) 19.7% at 2.4 / 2.4 MB/s; zpaq -5 ~19%; paq8px v217 13.1% at ~3 KB/s (files individually, Mahoney). Reading: at max settings LZMA beats zstd by ~7% in size while zstd decodes 10× faster; BWT matches LZMA's *size* on mixed data (the 15–17% BWT advantage is text-only) at 6× LZMA's compression speed but 4–5× slower decompression; context mixing buys 15–45% at 1–3 orders of magnitude more time.

**Archivers head-to-head (PeaZip max-compression benchmark, 303 MB):** ZPAQ-Ultra 19.0% (359 s / 358 s extract); 7z-Ultra 23.5% (137 s / 3.4 s); RAR5-Best 25.8% (28.5 s / 1.8 s).

**Text (LTCB enwik9, 30 Sep 2026):** xz -9e 197.3 MB; 7-Zip PPMd 179.0; bsc 163.9; XWRT (word transform) + strong coder 151.2; zpaq 142.3; paq8px 124.7; cmix v21 108.0; fx2-cmix-transformer 97.0 (Hutter-rules size 100.4 MB, Jul 2026 winner, €37,300); cmix-lex-transformer 95.8 (#1). The 2026 leaders are 6 M-parameter transformers pretrained offline on enwik9, int4 weights/int8 activations with AVX2 integer kernels (plus floating-point parts that needed `-mrecip=none` to behave identically on Intel and AMD), ~7.5 KB/s on one core, <10 GB RAM, GPL-3.0, and specialised to enwik9. Realistic headroom on *arbitrary* text with a general prior: 30–45% below xz, not 50%.

**Already-compressed data:** Precomp on silesia.zip: 7-Zip 99.7% → inflate-then-LZMA 69.7% (best case: a ZIP of highly compressible benchmark files). 2026 42-format test, 55 MB mixed (11 MB text, 15 MB Office, 16 MB JPEG, 13 MB H.264): zpaq 56.1%, 7-Zip 60.0%, zstd-max 61.3% — everyone stalls on the compressed half. Dropbox Lepton: ~23% average over 203 PiB of JPEGs, with 3.6% of files unsupported (progressive at the time, CMYK, arithmetic-coded, oversized chroma), 5 MB/s compress / 15 MB/s decompress per core, verify-before-store mandatory.

**Redundant data:** Mahoney's 10 GB set: zpaq -m5 2.92 GB vs 7-Zip -mx 3.59 GB (19% smaller; the 7-Zip build was 4.47b from 2007 and zpaq extraction was 852 s vs 519 s — dedup wins on size, not on speed). DwarFS vs SquashFS, same codec (lzma), 47.6 GiB of 1,139 Perl installs: 315 MiB vs 3,838 MiB (12×; a pathological-redundancy case). lrzip on a 10 GB VM image: default mode 12.8% vs xz 21.2% (the 9.9% `-zU` result takes 40 minutes to decompress). zstd `--long=31` in Facebook production: full backups −16%, diff backups −27%; fbpkg used 128 MB windows because of decode-side memory limits. Meyer & Bolosky (857 desktops): dedup *within one machine* is modest — whole-file dedup captures ~¾ of block-level savings and most of it is program binaries; multiples appear only across machines or versions.

**Small files / dictionaries:** zstd trained dictionary on 1,000 small JSON records: 2.8× → 6.9×. Compression-dictionary transport (RFC 9842, Chrome/Cloudflare): a JS bundle 92 KB gzip → 2.6 KB against its previous version.

---

## 4. Where the headroom is — honest version

Shannon is not negotiable: video (H.264/HEVC/AV1), encrypted and random data get ~0% from everyone, and on a *single generic binary at equal speed* the gain over LZMA is 5–20%. The headroom is in (a) undoing weak encoders inside JPEG/PNG/PDF/Office/MP3 files, (b) redundancy across files, (c) text/structured data with stronger models, (d) small files via priors, and (e) decode speed. Blended across a plausible consumer archive (30% JPEG, 35% video, 10% Office/PDF, 10% software, 5% text, 10% other compressed), 7-Zip lands at ~85% of input and a full LitePack pipeline at ~76% — about 11% smaller. Remove the video and it is 20–25%. That is the honest shape of the win: large where the data allows it, zero where physics forbids it, and always faster to extract.

---

## 5. Realistic gains by data class (targets to be *measured* in Phase 0)

| Data class | 7-Zip Ultra / WinRAR Best | WinZip Zipx / PowerArchiver .pa | LitePack target | Mechanism / evidence |
|---|---|---|---|---|
| JPEG photos | 0–2% | up to ~20% | **16–23% on supported files**; 1–4% of files stored as-is | Lepton-class DCT recoding (lepton_jpeg_rust "up to 22%", Dropbox 23% avg); JPEG XL `-j` 16–22% as fallback; UltraHDR/gain-map and >4 MB-tail files handled as nested streams |
| PNG, PDF, Office, APK/JAR/EPUB, nested ZIPs | 1–5% | PDF/DOCX ~40% smaller than ZIP (.pa claim) | **15–40% depending on encoder** | preflate-rs inversion where the Deflate encoder is recognised (zlib family: 0.01–2.7% overhead); net-gain gate on the full stream; unrecognised encoders (zopfli/oxipng, 7-Zip, Java/Go/.NET, Deflate64) fall back to store |
| MP3 | 0% | up to ~20% (Zipx) | **11–16%** (packMP3 class; free-format and non-compliant files stored) | Huffman-layer recoding |
| WAV/PCM, BMP/TIFF/RAW | LZMA leaves 72–84% | — | **43–60% of original** | FLAC/OptimFROG-class predictors (Monkey's Audio 52%, FLAC 54% of CD audio); image predictors |
| Text, code, XML/JSON, logs, CSV | xz = 100 | ≈100 | **83–86 (BWT, text only), 73–81 (CM), ≤60 (research tier)** | bsc/bzip3 on text; kanzi/xEnc3-class CM; word-replacement transform (XWRT −23% vs xz on enwik9); column/field transforms (CLP 2.16× over zstd on logs); OpenZL graphs for CSV/numeric |
| AI model weights (.safetensors/.gguf/.pt) — new class | ~10% | — | **33–50% on bf16/fp16** | Exponent/mantissa byte-plane split + per-plane entropy coding (ZipNN: often 33%, up to 50%+; DFloat11 ~70% of size; OpenZL −30% on bf16 embeddings) |
| Executables, installed software, game data | — | .pa: 5–25% smaller than 7Z | **5–15% (Balanced), 15–25% (Strong)** | BCJ/ARM64 filters (0–15%), CM exploits opcode structure (paq8px 50–58% below 7z on mozilla/samba, at KB/s) |
| Versioned backups, project folders, VM images, photo libraries with edits | window-limited | .pa dedup | **2×+ on versioned sets; 10–30% on a single machine's files** | CDC dedup + file-level similarity ordering + suffix-array deltas (t-saur: 45.2% vs 55.0% on versioned data; zstd --long −16/−27% in production) |
| Thousands of small files | poor (ZIP per-file; LZ cold start) | — | **2–3×** | Per-type trained dictionaries bundled with the app (zstd dict 2.8× → 6.9×) |
| Video, encrypted, random | ~0% (7-Zip still spends the CPU) | ~0% | **~0% at disk speed** | Entropy gate stores immediately |

---

## 6. The LitePack method, v2: Peel → Fold → Model → Seal (gated, measured, fallback-first)

### 6.1 Peel — bit-exact inversion of existing encodings, as a gated subsystem

Peel detects nested encodings and inverts them losslessly, storing a reconstruction record so extraction rebuilds the original bytes exactly. Every peel is **verified by re-encoding at compression time** (Dropbox's policy) and **gated on measured net gain for the full stream**; anything that fails verification or the gate is stored as-is. Coverage for v1 of the format:

- **Deflate/zlib/gzip** anywhere (ZIP, PNG, PDF streams, DOCX/XLSX/PPTX, JAR/APK, EPUB, SWF, git objects) via preflate-rs. Documented fallbacks: Deflate64, preset dictionaries, unrecognised encoders.
- **JPEG** via lepton_jpeg (Apache-2.0, 0.5.8), with explicit handling of progressive files, dimension limits (raise the 16,386 px default for modern cameras), UltraHDR/gain-map secondary JPEGs and oversized trailing data as nested streams; arithmetic-coded, CMYK and 12-bit JPEGs are stored. Expected in-the-wild failure budget: 1–4%.
- **PNG**: Deflate peel → undo per-row filters (filter bytes kept as side info) → raw pixels → lossless image model. paq8px proves the chain; Phase 0 must measure whether a JXL-modular/WebP-lossless-class predictor beats "preflate + CM on filtered rows" by enough to justify the second codec.
- **MP3** via packMP3-class recoding (LGPL — see Section 11), storing free-format and non-compliant files.
- **Encodings inside text**: base64/base85 (precomp/paq8px prior art), hex dumps and UTF-16 (not found in any existing tool), line-ending normalisation.
- **Containers** (TAR, ZIP, 7z, ISO, CAB, MSI, PDF object streams, Office packages): members become individually routed, deduplicable streams; the container framing is a reconstruction record. precomp does ZIP-as-zlib and paq8px does TAR; a general framing record across 7z/CAB/MSI/ISO is one of the few genuinely unoccupied spots.
- Not attempted (no practical bit-exact method exists): H.264/HEVC/AV1 video, HEIC/AVIF/WebP-lossy, AAC/Opus, camera RAW (a Phase-2 spike: the lossless-JPEG Huffman layer in DNG/CR2/NEF may be peelable).

Peel is recursive (depth-limited, like precomp's `-d`) and is **off in the Fast tier except for cheap container unpacking**, because Lepton-class JPEG recoding costs ~5 MB/s per core.

### 6.2 Fold — dedup, similarity ordering, delta

Streams are chunked with a SeqCDC/VectorCDC-class content-defined chunker (5–10 GB/s scalar, 30 GB/s AVX-512; dedup within 4% of the best CDC), ~64 KB average, hashed with BLAKE3, and deduplicated globally. **Files, not chunks, are ordered by similarity** (DwarFS's nilsimsa clustering is O(n²) and tops out at a few hundred thousand items, so ordering is applied per directory/type cluster); chunk-level reordering is avoided because it turns extraction into random writes. Near-duplicates are found with Palantir-style multi-tier super-features (+7% vs Odess, +26% vs Finesse) and stored as suffix-array deltas (HDiffPatch/bsdiff class; ~29% smaller than zstd `--patch-from` on an 818 MiB set of real binaries) in Balanced/Strong, or zstd `--patch-from` in Fast. Cost on data without duplicates: ~1–3 s/GB of CPU and ~0.1% size; the entropy gate disables Fold on incompressible input.

### 6.3 Model — the right model per stream, at four Pareto points

A lightweight byte-statistics classifier (Google Magika-style; extension is only a hint) routes each folded stream:

| Tier | Target speed per core | What runs | Expected vs 7-Zip Ultra on real data |
|---|---|---|---|
| **Fast** (sharing) | 100+ MB/s compress, 500+ MB/s extract | zstd-class LZ + tANS with large windows and bundled per-type dictionaries, BCJ/ARM64 filters, Fold, container unpack; OpenZL LZ/PivCo-Huffman-class decoder design (3 GB/s on Silesia) as the stored/fast path | Smaller on redundant data, 5–10× faster extraction |
| **Balanced** (default) | 5–20 MB/s | LZMA-class or zstd-ultra + filters + **full Peel + Fold**; BWT + CM only for text-classified streams; **OpenZL graphs** for structured/numeric streams (CSV, Parquet, arrays, PCM/pixel planes, tensors — Compression Transformer selector +35% vs zstd -19 on numeric streams); audio/image predictors; byte-plane transform for model weights | 10–25% smaller on photo/document sets; ≥2× on versioned sets; extraction ≥ LZMA speed |
| **Strong** | 1–3 MB/s | Context mixing (kanzi-class; obtain and benchmark **xEnc3** from AITDCC 2026: paq8px ratio at 16.8× its speed) over peeled streams; GPU CM (CuCM, DCC 2026) watched | 20–40% smaller |
| **Research** (not in format v1) | ~10 KB/s | Pretrained small-transformer priors per data class, integer CPU kernels, mismatch-tolerant coder | Gated by conformance suite + FTO review |

Two engineering rules replace v1's "determinism by construction":

1. **Reference = integer CPU kernels** (int8, fixed reduction order, no RCP/RSQRT, LUT softmax) with cross-ISA conformance vectors — Ballé/Johnston's integer-network approach, which is patented (US 12,154,304, Google, priority 2018) and must be licensed or designed around before any product use.
2. **Mismatch-tolerant coding is primary, not a backstop**, for any GPU/NPU path: NPUs do not execute bit-exact int8 across vendors (Apple NE simulates INT8 in FP16; "no bit-exact reference specification for NPU vendors" — MLVC, Jun 2026). PMATIC (ICLR 2026) survives δ=0.01 logit mismatch between Apple M2 Pro and M4 Max at 0.05–0.34% overhead but fails at δ=0.001 and does not rescue a model whose *online adaptation* has diverged; the Hybrid-LLM protocol (quantise logits, compute softmax on CPU) is the cheap complement.

**Priors.** Per-type dictionaries and (later) model weights are **bundled with the application release**, versioned and content-addressed; an archive references prior IDs, and every prior an archive can reference ships with the reader that wrote it. No network fetch is ever required to decode (SDCH died of exactly that dependency). Online fetching is an optional convenience for newer priors.

### 6.4 Seal — the `.lpk` container

- **Versioned primitive set + open spec + reference decoder.** Each archive carries a compact decode graph (zpaq/OpenZL lineage) built only from primitives in the published spec. No executable decoder inside archives.
- **Declared decode envelope.** Every archive states its maximum decode memory/CPU (window sizes, BWT block sizes); defaults are capped (≤256 MB windows, ≤64 MB BWT blocks ≈ 5n RAM) so a 4 GB laptop or a NAS can always extract; the UI warns when a user raises them.
- **Integrity.** BLAKE3 Merkle tree over chunks: any file or byte range verifies independently; corruption is localised.
- **Recovery.** Reed-Solomon recovery records (reed-solomon-simd, O(n log n), MIT/BSD; PAR3-style block layout), 1–20% configurable, sized to the blast radius of shared chunks; recovery volumes optional. RaptorQ excluded (Qualcomm's covenant covers only full RFC 6330 implementations and a patent runs to ~2030).
- **Encryption.** AES-256-GCM by default (hardware AES, FIPS-validatable path) with XChaCha20-Poly1305 optional; Argon2id (RFC 9106 second-recommended parameters as default, configurable); nonces derived from chunk index under a per-archive wrapped key; per-chunk associated data; encrypted names and metadata **optional** (default on, with a "listable" mode because users expect to list without a password); optional keyfile second factor.
- **Seekability and streaming.** Independent frames + seek table; archives can be written as a stream.
- **Incremental/journaling mode.** Append-only updates with dedup against existing content (zpaq-style); rollback = truncate.
- **Safe extraction policy** in one audited module: absolute paths, `..`, symlinks (never outside target; skipped unless privileged), NTFS alternate data streams (stripped except Zone.Identifier), Mark-of-the-Web propagation via `IAttachmentExecute`, overwrite rules.
- **Interoperability.** Reads ZIP/7z/RAR/tar.*/ISO via sandboxed parsers; writes ZIP (AES, Zip64, zstd method 93) and 7z; "smart ZIP" export for recipients without LitePack — accepting that shared archives will often be ZIPs, so the `.lpk` advantage must be sold for *storage, backup and transfer within the LitePack ecosystem* first.

### 6.5 Performance architecture

Chunk-level parallelism, SIMD kernels via `std::arch` with runtime dispatch (AVX2/AVX-512-VNNI/NEON), memory-mapped I/O, an entropy pre-check that stores high-entropy chunks at I/O speed, a zstd-style **external sequence-producer API** so GPU/NPU/QAT match finders can be plugged in later without a format change (Intel's QAT plugin pattern: 3.2× at equal ratio), and optional GPU BWT via wgpu compute (DX12/Vulkan/Metal) with CUDA/libcubwt as an optional accelerator (228–1,297 MB/s on an RTX 4070 Ti, 20.5 bytes of VRAM per input byte).

---

## 7. What is honestly new, and what we build on

**Nothing algorithmic is new.** Each stage has shipped or published precedent: Precomp (recursion depth 10, releases through 2024), paq8px (recursive peel incl. PNG→pixels→model), xtool/ytool, PowerArchiver .pa (commercial recompression + CDC dedup), WinZip Zipx; pcompress (dedup + minhash similarity + bsdiff delta + adaptive routing + encrypt-then-MAC, 2012–2016), DwarFS, zpaq/zpaqfranz, FreeArc/FA; Oodle Hydra (per-block Pareto tier selection, 2017) and OpenZL (graph compressors + universal decoder + learned selector, 2025–26); SDCH/RFC 9842 (hash-addressed dictionaries); Ballé 2019 + Google patent (integer networks), PMATIC/Hu & Tang 2026 (tolerant coding), ts_zip/nncp/Nacrith/2026 Hutter entries (neural tiers); zpaq (self-describing decoder since 2009), AnyBlox/F3 (WASM decoders in data files, 2025); PAR2/PAR3, duplicacy/kopia erasure coding, borg 2/kopia/Apple AEA (AEAD); t-saur and sqz-next (2026 Rust pipelines).

**What is unoccupied:** one maintained, open-spec, cross-platform, memory-safe *consumer* archiver that integrates a precomp/paq8px-class recursive peel, a pcompress/DwarFS-class fold, Hydra/OpenZL-class tiered routing, and a kopia-class sealed single-file container with recovery records, AEAD, Merkle integrity, seekability and journaling — plus ZIP/7z export, a modern Windows UI and Explorer integration. Small unclaimed technical deltas: UTF-16/hex-dump peels; a general container-framing reconstruction record for 7z/CAB/MSI/ISO members; the model-weights data class in an archiver; bundled per-class priors referenced by ID; recovery + AEAD + Merkle + seek + journal in a single-file format rather than a backup repository.

**The competitive window is months, not years.** t-saur and sqz-next show the open-source community converging on the same pipeline; OpenZL ships Windows binaries and a 3 GB/s decoder; Windows keeps absorbing "open anything". LitePack wins by shipping Phase 1 fast, proving numbers on a public corpus, and differentiating on recovery, encryption, safety, speed and product polish rather than on claimed novelty.

**Freedom-to-operate items before the relevant phase:** Google US 12,154,304 (integer-NN entropy coding; Research tier); IBM US 8,688,654 (sample-based engine selection/tiering, reported as active to ~2031 — review against the router design); EMC US 9,798,731 (delta of similarity-clustered chunks — review against Fold); Dell US 12,111,791 (ML algorithm prediction); Microsoft rANS patent US 11,234,023 (disputed; zstd/JXL ship regardless); possible MIT filing on PMATIC. Reed-Solomon/PAR2, arithmetic coding (classic patents), zstd, Brotli, FLAC, BWT: clear.

---

## 8. Security and trust design

Rust core with `#![forbid(unsafe_code)]` outside audited SIMD kernels and `*-sys` crates; every format parser runs in a worker process with a restricted token and job object (Chromium's model; LPAC AppContainer for C/C++ parsers), with rare-format plugins as WebAssembly modules inside that worker; one audited extraction-policy module; signed automatic updates (the absence of which keeps WinRAR's 2025 zero-day alive in 2026); enterprise channels (MSIX/Intune, GPO policies); open specification and reference decoder; reproducible builds (`/Brepro`, remapped paths, pinned toolchain), SBOM and build attestations; continuous fuzzing on Linux runners (cargo-fuzz/AFL are Unix-only) plus WinAFL for Windows-only DLLs; a ≥10,000-file real-world round-trip corpus for Peel in CI.

---

## 9. Windows feasibility and the build stack (verified 1 Oct 2026)

**Yes — Windows is the primary target, and every piece has a verified path.**

**Language.** Rust (stable) on the MSVC targets `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` — both Tier 1 with host tools. AVX2/SSE intrinsics are stable since 1.27, NEON since 1.59, AVX-512 since 1.89 (Aug 2025); portable `std::simd` is still nightly-only, so kernels use `std::arch` with runtime feature detection. Needs Visual Studio Build Tools (MSVC v143+, Windows 11 SDK); rustup installs them. C/C++ codecs (libbsc, kanzi, optionally libjxl) are built with the `cc`/`cmake` crates and run only inside sandboxes. Alternatives rejected: C++ (the incumbents' language and the source of their CVE class), Zig 0.16 (pre-1.0, no COM/shell ecosystem — though "zmix", LTCB #2, is written in it), C#/.NET NativeAOT (fine for a shell stub, wrong for SIMD kernels), Go (no SIMD intrinsics, GC).

**Core crates (versions and licences checked on crates.io):** zstd 0.14 (libzstd 1.5.7, BSD-3), liblzma 0.4.8 (maintained; `xz2` is stale), ruzstd 0.9 (pure-Rust decoder for sandboxes), brotli 9 (BSD/MIT), preflate-rs 0.7.6 (Apache-2.0, depends on cabac LGPL-3.0 — Section 11), lepton_jpeg 0.5.8 (Apache-2.0), precomp2 0.2 (Apache-2.0, early — study only), fastcdc 5 + gearhash, blake3 1.8.7 (SIMD, rayon), rayon, memmap2, RustCrypto aes-gcm/chacha20poly1305/argon2, reed-solomon-simd 3.1 (MIT/BSD), libsais-rs (Apache-2.0), wasmtime 49 (Tier 1 on x64 Windows; Tier 3 on ARM64 Windows), windows-rs (`windows` 0.62, `windows-core` 0.100 — pin versions), rappct (AppContainer/LPAC, audit first), wgpu 30, ort 2.0-rc (ONNX Runtime). **Never** `jpegxl-rs` (GPL-3.0); use `jxl-sys` (MIT/Apache) or lepton_jpeg instead. `bzip3` crate is LGPL-3.0-only (optional/dynamic or skip).

**Windows Explorer integration.** The Windows 11 top-level context menu requires an `IExplorerCommand` COM server with package identity — full MSIX, or a "package with external location" (sparse package, Windows 10 19041+) for the classic installer — hosted out-of-process in a dllhost surrogate; a working Rust implementation with windows-rs `#[implement]` exists (windows11-context-rs), and NanaZip ships the C++ equivalent via MSIX. 7-Zip still has no modern-menu support. Also: classic `IContextMenu` for "Show more options"/Windows 10, `IPreviewHandler` (runs in prevhost.exe at low integrity — naturally sandboxed in-archive preview), `IThumbnailProvider`, file associations via manifest/Tauri `fileAssociations`, drag-out via `IDataObject` with `CFSTR_FILECONTENTS`, long paths (`\\?\` + `longPathAware` manifest; Rust std already handles it), ADS via `file:stream`, MoTW via `IAttachmentExecute`/Zone.Identifier, symlinks need `SeCreateSymbolicLinkPrivilege` or Developer Mode (default: skip and warn), backup semantics only in an opt-in elevated mode, native ARM64 builds tested on GitHub's `windows-11-arm` runners.

**GUI: Tauri 2** (2.12, Sep 2026; stay on 2.x, 3.0 is alpha). WebView2 is in-box on Windows 11 and present on most Windows 10 machines; the shell is <600 KB and installers are 5–10 MB; NSIS or WiX bundling, signing hooks, file associations, and a signed-manifest updater plugin. This puts the founder's HTML/CSS/vanilla-JS skills directly on the product UI with the same Rust backend as the engine, and gives macOS/Linux parity from one codebase. Two rules: the million-entry archive index lives in Rust and the webview renders a windowed virtual list fed in binary batches; shell handlers are separate windows-rs cdylibs that never load the webview. Considered and rejected: Electron (hundreds of MB), WinUI 3 (Windows-only, C#/C++), Avalonia (.NET, another stack), Qt (licensing), Rust-native toolkits (no file-manager-grade virtual tables yet).

**Sandboxing.** `lpk-worker.exe` launched with a restricted token + job object (memory/CPU/time limits, kill-on-close); C/C++ parsers additionally in an LPAC AppContainer; broker duplicates handles in, data over pipes/shared memory; a crashed worker is a failed extraction. Win32 App Isolation is still in preview; Windows Sandbox/Hyper-V is unavailable on Home editions.

**GPU/NPU.** wgpu compute (DX12/Vulkan/Metal) for BWT; CUDA optional. Windows ML is GA (Sep 2025) with vendor execution providers (OpenVINO, QNN, VitisAI) on Windows 11 24H2+, but none promise bit-identical outputs — so accelerators sit behind the tolerant coder, and the CPU int8 kernels remain the reference.

**Build, CI, QA.** Cargo workspace: `lpk-core`, `lpk-peel`, `lpk-fold`, `lpk-model`, `lpk-codecs-*` (one per family, FFI as `*-sys`), `lpk-sandbox`, `lpk-cli`, `lpk-gui` (Tauri), `lpk-shell` (windows-rs cdylib), `fuzz/`, `xtask`. GitHub Actions on `windows-2025` (VS 2026), `windows-11-arm`, `ubuntu-24.04` (fuzzing, Miri, cargo-xwin cross-checks of MSVC targets), macOS. Tools: cargo-nextest, criterion, cargo-llvm-cov, cargo-fuzz/cargo-afl (Linux), WinAFL (Windows-only binaries; Microsoft OneFuzz is archived), Miri on kernels, cargo-deny (licence allow-list: Apache/MIT/BSD/CC0/0BSD; GPL/LGPL only as flagged optional codecs), cargo-auditable + cargo-sbom, GitHub artifact attestations, cargo-dist for CLI releases. Property/round-trip tests and format test vectors are written before the engine.

**Packaging, signing, updates.** MSIX (Store, winget, Intune; gives the identity the modern menu needs) **and** a Tauri NSIS classic installer with a sparse identity package, plus a portable zip; WiX v7 MSI only on enterprise demand (WiX binaries now carry a maintenance-fee EULA; Inno Setup asks commercial users to license). Microsoft Store individual registration is free since Sep 2025. Code signing via **Azure Artifact Signing** (formerly Trusted Signing): public-trust certificates are available to organisations in Canada and to *individual developers in the US and Canada*, short-lived certs with mandatory timestamping, reported at roughly $10/month for the basic tier; EV certificates no longer grant instant SmartScreen reputation, so reputation must be warmed by download history. Updates via the Tauri updater (classic installs) and the Store (MSIX), or Velopack if delta updates matter. macOS: Developer ID + notarytool; Linux: AppImage/deb/rpm.

**Minimum requirements.** End users: Windows 10 2004 (19041)+ (Windows 11 recommended; sparse identity needs 19041+), x64 or ARM64, no 32-bit builds, WebView2 runtime, 2–4 GB free RAM for Fast/Balanced, 8 GB for Strong, GPU/NPU optional. Developers: Windows 11 with VS 2022/2026 Build Tools, Rust stable ≥1.96, CMake + Ninja for C/C++ codecs, Node LTS for Tauri tooling, 16 GB RAM and NVMe (32 GB for prior-training work, which is a separate Python/PyTorch pipeline exporting int8 weights).

**Founder fit.** PHP/JS experience covers closures, iterators, async and package managers; the new material is ownership/borrowing, enums + `match`, traits and `Result` errors — roughly 4–8 weeks to productivity on the CLI/engine with an AI assistant, longer for `unsafe` SIMD kernels and COM. Web skills apply directly to the Tauri UI, the website/docs, the update manifest and prior registry endpoints (static JSON on a CDN; PHP is fine), and the public benchmark dashboard. Buy help for the shell-extension/MSIX plumbing and the signing/Store onboarding.

---

## 10. Risks (updated)

| Risk | Mitigation |
|---|---|
| Overclaiming (v1's own failure mode) | No public number until Phase 0 measures it on a published corpus that includes video, with 7-Zip, WinRAR, WinZip Zipx, PowerArchiver .pa, t-saur and zstd/xz as baselines |
| Peel robustness (1–4% of JPEGs, unrecognised Deflate encoders, non-compliant MP3) | Verify-by-re-encode, full-stream net-gain gate, documented store-as-is list, 10k-file round-trip CI |
| Recipient-side resources | Declared decode envelope per archive; capped defaults; UI warning |
| Format trust / recipients without LitePack | Open spec + reference decoder; smart-ZIP export; position `.lpk` for storage/backup/ecosystem transfer first |
| Priors availability | Bundled with app releases; no network needed to decode |
| Patents (Google integer-NN coding; IBM tier selection; EMC similarity delta; Dell ML selection; rANS) | FTO review per phase; Research tier blocked until cleared or licensed |
| Licences (cabac LGPL-3, packMP3 LGPL-3, bzip3 LGPL-3, jpegxl-rs GPL-3, CM references GPL-3) | Section 11; open-core decision; no "clean-room" ports by people who read GPL sources |
| Security surface grows with every parser | Sandboxed workers, fuzzing, policy module, plugins off by default |
| Crypto compliance / export | AES-GCM default for FIPS buyers; US EAR §740.17 mass-market self-classification and Canadian ECL Group 1 paperwork before distribution |
| SmartScreen/AV reputation for a new signed binary | Artifact Signing + timestamping, reputation warming, no executable content in archives |
| Competitors converge (OpenZL, t-saur, sqz-next, PowerArchiver, Windows built-in) | Ship Phase 1 fast; build on OpenZL/preflate/lepton rather than against them; compete on product, safety, speed, recovery, encryption |
| Neural tier never becomes practical (today ~10 KB/s/core) | It is a research track with no promises on the roadmap; the product stands without it |

---

## 11. Building blocks and licences (corrected)

| Need | Component | Licence | Note |
|---|---|---|---|
| Deflate peel | microsoft/preflate-rs 0.7.6 | Apache-2.0, **requires `cabac` 0.15 (LGPL-3.0-or-later)** | Options: (a) ship the core as open source (Apache/MIT) — LGPL compliance then is trivial and this also answers the format-trust problem; (b) dynamic-link the coder; (c) ask Microsoft to relicense; (d) a *genuinely* independent binary arithmetic coder written without reading the LGPL code |
| JPEG peel | microsoft/lepton_jpeg 0.5.8 | Apache-2.0 | Primary. libjxl `-j` (BSD-3) via `jxl-sys` only if PNG/JXL chain is adopted; never `jpegxl-rs` (GPL-3) |
| MP3 peel | packMP3 (YadeWira fork) | LGPL-3.0 | Dynamic link or open core; single maintainer |
| LZ fast tier | zstd 1.5.7 | BSD-3/GPL-2 dual | 1.6.0 is listed in the changelog but unreleased |
| LZMA-class | liblzma 5.8.x via `liblzma` crate | 0BSD / MIT-Apache | |
| Structured engine | Meta OpenZL 0.3.0 | BSD | C11 core, Windows binaries, NEON/SVE2; frame format changes between versions — pin |
| BWT | libsais-rs / libbsc 3.3 (+libcubwt CUDA optional) | Apache-2.0 | bzip3 is LGPL-3 (optional) |
| CM | kanzi 2.5 (Apache-2.0); xEnc3 (AITDCC 2026 — licence to check) | Apache-2.0 / TBD | paq8px, cmix, Hutter entries are GPL-3: benchmark references only |
| Dedup/hash | SeqCDC/VectorCDC (own impl.; UWASL dedup-bench), fastcdc, gearhash, blake3 | MIT / CC0-Apache | |
| Delta | HDiffPatch (MIT) or own suffix-array delta; zstd --patch-from | MIT / BSD | |
| Audio/image | FLAC/WavPack designs; own predictors | BSD | |
| Recovery | reed-solomon-simd 3.1 | MIT AND BSD-3 | RaptorQ excluded (IPR) |
| Crypto | RustCrypto aes-gcm, chacha20poly1305, argon2 | MIT/Apache | |
| Foreign formats | zip, sevenz-rust2, ppmd-rust in-process; libarchive / 7-Zip SDK sandboxed | MIT/Apache; BSD; LGPL (sandboxed) | |
| Plugins | wasmtime 49 | Apache-2.0 w/ LLVM exception | Not inside archives |
| Windows | windows-rs, rappct, wgpu, ort | MIT/Apache | |
| GUI | Tauri 2.12 | MIT/Apache | |

---

## 12. Roadmap (revised) and measurable gates

**Phase 0 — Measure (3–4 weeks).** Publish the *LitePack Real-World Corpus* (photo library with edits incl. UltraHDR files; Office/PDF set; source tree with git history; installed application; game assets; logs; a VM image; three nightly backup versions; **a video folder**; a model-weights folder) and a harness reporting size, compress/extract time and peak RAM for 7-Zip Ultra, WinRAR Best, WinZip Zipx, PowerArchiver .pa, zstd -19/--ultra -22 --long, xz -9, t-saur, zpaqfranz, and each LitePack tier. Decide open-core vs proprietary core (drives the LGPL answer). Run the FTO review for Phase 1 items. **Gate:** per-class targets in Section 5 restated as measured numbers.

**Phase 1 — Beat them on real data (~3–4 months).** Rust core; `.lpk` format v1 (Merkle, AES-GCM/Argon2id, seek table, RS recovery, journaling, decode envelope); Fast + Balanced tiers; Fold (SeqCDC + BLAKE3 + file-level similarity + deltas); Peel for Deflate, JPEG, PNG, base64/UTF-16, containers; CLI; Tauri Windows GUI; Windows 11 context menu; MSIX + NSIS; signing; auto-update. **Gate:** Balanced ≥10% smaller than WinZip Zipx and ≥15% smaller than 7-Zip Ultra on the photo/document sets, ≥2× on the backup set, 0% at disk speed on video, extraction ≥ 7-Zip's; zero memory-safety findings from fuzzing.

**Phase 2 — Strong tier, structured data, platforms (~3 months).** CM backend; OpenZL structured engine; model-weights class; XWRT-style and column transforms; executable delta; GPU BWT; macOS/Linux builds; previews; Store/winget/Intune.

**Phase 3 — Research track (parallel, unfunded by product promises).** Pretrained per-class priors; integer CPU kernels + cross-ISA conformance suite; tolerant coder; FTO/licensing of the Google patent; only enters the format spec if it clears the suite and the review.

**Phase 4 — Enterprise and ecosystem.** Policies (ADMX), SBOM/attestations, open-spec publication, independent decoder implementation, bug bounty.

---

## 13. Sources

Incumbents and formats: https://www.7-zip.org/history.txt · https://www.rarlab.com/rarnew.htm · https://www.win-rar.com/singlenewsview.html?L=0&tx_ttnews%5Btt_news%5D=304 · https://kb.winzip.com/en/130408 · https://www.powerarchiver.com/en-us/powerarchiver/ · https://forums.powerarchiver.com/topic/5740/advanced-codec-pack-engine-list-of-changes · https://peazip.github.io/changelog.html · https://www.bandisoft.com/bandizip/history/ · https://github.com/M2Team/NanaZip/releases · https://gs.statcounter.com/os-version-market-share/windows/desktop/worldwide · https://www.neowin.net/news/windows-11-gets-native-rar-support-here-is-how-it-compares-to-winrar-and-other-apps/ · https://windowsforum.com/news/windows-11-kb5083631-april-30-2026-file-explorer-archive-support-bug-fixes.416175/ · https://devco.re/blog/2025/02/12/from-convenience-to-contagion-the-half-day-threat-and-libarchive-vulnerabilities-lurking-in-windows-11-en/ · https://theapplewiki.com/wiki/Apple_Encrypted_Archive · https://github.com/iulianbondari/t-saur · https://github.com/hankitools/sqz-website

Security: https://nvd.nist.gov/vuln/detail/CVE-2026-14191 · https://thehackernews.com/2026/07/new-7-zip-vulnerability-could-let.html · https://socprime.com/blog/cve-2026-48095-7-zip-heap-overflow-flaw/ · https://www.welivesecurity.com/en/eset-research/update-winrar-tools-now-romcom-and-others-exploiting-zero-day-vulnerability/ · https://www.trendmicro.com/en_us/research/26/f/old-winrar-flaw-fuels-attacks-on-ukraine.html · https://www.trendmicro.com/en_us/research/25/a/cve-2025-0411-ukrainian-organizations-targeted.html · https://www.securityweek.com/recent-7-zip-vulnerability-exploited-in-attacks/ · https://github.com/lclevy/unarcrypto · https://datatracker.ietf.org/doc/draft-irtf-cfrg-xchacha/ · https://www.rfc-editor.org/info/rfc9106/ · https://www.law.cornell.edu/cfr/text/15/740.17

Benchmarks: https://github.com/inikep/lzbench · https://peazip.github.io/maximum-compression-benchmark.html · https://mattmahoney.net/dc/text.html · https://mattmahoney.net/dc/silesia.html · https://mattmahoney.net/dc/10gb.html · http://prize.hutter1.net/ · https://raw.githubusercontent.com/astOwOlfo/fx2-cmix-transformer-v1/main/writeup.md · https://dev.to/andrew_dyster_c88ccbb5180/i-benchmarked-42-compression-formats-spanning-four-decades-heres-what-to-actually-use-143o · https://github.com/schnaader/precomp-cpp · https://www.usenix.org/system/files/conference/nsdi17/nsdi17-horn-daniel.pdf · https://github.com/mhx/dwarfs · https://github.com/ckolivas/lrzip/blob/master/doc/README.benchmarks · https://engineering.fb.com/2018/12/19/core-infra/zstandard/ · https://engineering.fb.com/2016/08/31/core-infra/smaller-and-faster-data-compression-with-zstandard/ · https://www.usenix.org/legacy/events/fast11/tech/full_papers/Meyer.pdf · https://developers.google.com/speed/webp/docs/webp_lossless_alpha_study · https://arxiv.org/html/2606.17712v1 (AITDCC 2026)

Peel components: https://github.com/microsoft/preflate-rs · https://crates.io/crates/cabac · https://github.com/microsoft/lepton_jpeg_rust · https://crates.io/crates/lepton_jpeg · https://github.com/libjxl/libjxl/issues/3882 · https://github.com/libjxl/libjxl/issues/3604 · https://github.com/YadeWira/packMP3 · https://github.com/hxim/paq8px/blob/master/CHANGELOG · https://github.com/Razor12911/xtool · https://github.com/YadeWira/ytool · https://github.com/darkskygit/precomp2 · https://crates.io/crates/jpegxl-rs

Fold components: https://cs.uwaterloo.ca/~alkiswan/papers/SeqCDC_Middleware24.pdf · https://www.alphaxiv.org/abs/2505.21194 · https://github.com/UWASL/dedup-bench · https://henryhxu.github.io/share/hongming-asplos24.pdf (Palantir) · https://github.com/definitely-stable/ChunkShift/issues/183 · https://github.com/sisong/HDiffPatch · https://github.com/moinakg/pcompress · https://github.com/mhx/dwarfs/blob/main/doc/mkdwarfs.md · https://www.usenix.org/system/files/conference/fast14/fast14-paper_lin.pdf

Model components: https://github.com/facebook/openzl/releases/tag/v0.3.0 · https://arxiv.org/html/2605.09928v1 · https://openzl.org/ · http://cbloomrants.blogspot.com/2017/02/oodle-hydra.html · https://github.com/google/magika · https://arxiv.org/abs/2411.05239 (ZipNN) · https://arxiv.org/abs/2504.11651 (DFloat11) · https://arxiv.org/html/2507.10337v1 (LogLite) · https://github.com/y-scope/clp · https://github.com/IlyaGrebnov/libbsc · https://github.com/IlyaGrebnov/libcubwt · https://github.com/flanglet/kanzi-cpp · https://community.intel.com/t5/Blogs/Tech-Innovation/Artificial-Intelligence-AI/Intel-QuickAssist-Technology-Zstandard-Plugin-an-External/post/1509818 · https://github.com/welcome-to-the-sunny-side/misa77 · https://datacompressionconference.org/Programs/DCC2026Program.pdf

Neural / determinism: https://web.mit.edu/jstang/www/papers/pmatic_v1_iclr2026.pdf · https://web.mit.edu/jstang/www/papers/icml_2026.pdf · https://arxiv.org/html/2601.17684 · https://arxiv.org/abs/2602.19626 (Nacrith) · https://arxiv.org/html/2603.25526v1 (Hybrid-LLM) · https://arxiv.org/html/2606.28027 (MLVC, NPU determinism) · https://arxiv.org/html/2410.05078v2 · https://bellard.org/ts_zip/ · https://openreview.net/forum?id=S1zz2i0cY7 (Ballé integer networks) · https://patents.justia.com/patent/20240104786 (granted as US 12,154,304)

Patents / prior art: https://patents.google.com/patent/US8688654 · https://patents.justia.com/patent/9798731 · https://patents.google.com/patent/US12111791 · https://www.theregister.com/2022/02/17/microsoft_ans_patent/ · https://github.com/libjxl/libjxl/blob/main/PATENTS · https://datatracker.ietf.org/ipr/2554/ (RaptorQ) · https://datatracker.ietf.org/doc/html/draft-lee-sdch-spec-00 · https://www.rfc-editor.org/rfc/rfc9842.html · https://mattmahoney.net/dc/zpaq.html · https://www.vldb.org/pvldb/vol18/p4017-gienieczko.pdf (AnyBlox) · https://github.com/future-file-format/F3 · https://parchive.github.io/doc/Parity_Volume_Set_Specification_v3.0.html · https://github.com/gilbertchen/duplicacy/wiki/Erasure-coding · https://github.com/animetosho/ParPar

Windows / build stack: https://doc.rust-lang.org/nightly/rustc/platform-support.html · https://blog.rust-lang.org/2025/08/07/Rust-1.89.0/ · https://rust-lang.github.io/rustup/installation/windows-msvc.html · https://github.com/rust-cross/cargo-xwin · https://blogs.windows.com/windowsdeveloper/2021/07/19/extending-the-context-menu-and-share-dialog-in-windows-11/ · https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps · https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/integrate-packaged-app-with-file-explorer · https://github.com/SalahaldinBilal/windows11-context-rs · https://sourceforge.net/p/sevenzip/discussion/45797/thread/0cf5322150/ · https://learn.microsoft.com/en-us/windows/win32/shell/preview-handlers · https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-iattachmentexecute · https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation · https://v2.tauri.app/distribute/windows-installer/ · https://v2.tauri.app/plugin/updater/ · https://v2.tauri.app/distribute/microsoft-store/ · https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution · https://chromium.googlesource.com/chromium/src/+/main/docs/design/sandbox.md · https://learn.microsoft.com/en-us/windows/win32/secauthz/app-isolation-overview · https://docs.wasmtime.dev/stability-tiers.html · https://blogs.windows.com/windowsdeveloper/2025/09/23/windows-ml-is-generally-available-empowering-developers-to-scale-local-ai-across-windows-devices/ · https://onnxruntime.ai/docs/execution-providers/QNN-ExecutionProvider.html · https://github.com/rust-fuzz/cargo-fuzz · https://github.com/googleprojectzero/winafl · https://github.com/microsoft/onefuzz · https://github.com/axodotdev/cargo-dist · https://learn.microsoft.com/en-us/windows/msix/overview · https://robmensching.com/blog/posts/2026/02/04/osmf-v11/ · https://blogs.windows.com/windowsdeveloper/2025/09/10/free-developer-registration-for-individual-developers-on-microsoft-store/ · https://learn.microsoft.com/en-us/azure/trusted-signing/quickstart · https://textslashplain.com/2025/03/12/authenticode-in-2025-azure-trusted-signing/ · https://www.todesktop.com/blog/posts/windows-apps-psa-ev-certs-do-not-grant-immediate-reputation-anymore · https://docs.velopack.io/ · https://blogs.windows.com/windowsdeveloper/2025/04/14/github-actions-now-supports-windows-on-arm-runners-for-all-public-repos/ · crates.io API pages for every crate named in Sections 9 and 11
