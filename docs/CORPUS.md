# LitePack Real-World Corpus — specification (Phase 0)

Purpose: a reproducible, redistributable-by-recipe corpus that mirrors what people actually archive, so every size/speed claim is measured, never estimated. The builder downloads public sources and derives the rest; nothing private is ever included in the public profile. A `--private <dir>` mode measures the user's own folders locally with the same class scheme.

Profiles: `small` (1–2 GB, for iteration and CI) and `full` (10–20 GB, for the Phase 0 report). Every source is pinned by URL + BLAKE3 in `corpus.lock`; a rebuild must produce a byte-identical `manifest.json`.

| Class | What | Public source (licence) — small / full | Derived variants |
|---|---|---|---|
| `photo-jpeg` | Camera JPEGs, mixed sizes, baseline + progressive | Wikimedia Commons CC0/CC-BY "Featured pictures" via API (small: 300 files; full: 3,000); Kodak set (24 images); Google libultrahdr sample images (Apache-2.0) for UltraHDR/gain-map cases | `photo-jpeg-edited`: for 20% of files, re-save at quality 85 and crop 5% (lossy edits → near-duplicates) |
| `photo-raw-png` | Lossless images: PNG screenshots/UI, PNG photos, BMP/TIFF | Kenney.nl asset packs (CC0) PNG textures/UI; CLIC 2021 / DIV2K PNG samples (research use); convert 50 JPEGs to PNG and BMP locally | — |
| `office-pdf` | PDF, DOCX, XLSX, PPTX, DOC/XLS/PPT | Digital Corpora GovDocs1 threads (public US government documents; small: 1 thread ≈ 1,000 files; full: 5 threads); arXiv PDFs (CC-BY subset) | `office-versions`: open 30 DOCX/XLSX via LibreOffice headless (if available) and re-save with small text edits (3 versions each) |
| `source-git` | A source tree with full git history | `git clone` of a permissively licensed mid-size repo (e.g., facebook/zstd, BSD) incl. `.git`; small: shallow 200 commits; full: full history | `backup-v1/v2/v3`: working-tree snapshots at 3 commits ~1 month apart |
| `software-installed` | Installed application bits (PE, DLLs, resources) | Python embeddable package for Windows (PSF, ~25 MB); Git for Windows portable (GPL — used as *test data only*, never linked); 7-Zip extra console package | — |
| `game-assets` | Textures, audio, models, scripts | Kenney.nl "All-in-1" style CC0 packs (PNG/OGG/OBJ/JSON) | — |
| `logs-text` | Server/app logs, CSV, JSON lines | Loghub (LogPai) datasets: Apache, HDFS, Linux, Windows (research use); NYC taxi CSV sample (public domain) | `small-files`: 20,000 files < 8 KB cut from the logs/JSON (the small-file case) |
| `text-prose` | Natural-language text and markup | enwik8 (Wikipedia, CC-BY-SA; 100 MB); 50 Project Gutenberg books (public domain); Silesia corpus (calibration against published benchmarks) | — |
| `audio` | WAV/PCM, FLAC, MP3 | LibriVox public-domain MP3s (Internet Archive); Wikimedia Commons CC0 FLAC → decode to WAV locally | — |
| `video` | H.264/HEVC/AV1 in MP4/MKV | Blender open movies (Big Buck Bunny, Sintel; CC-BY) MP4; HEVC/AV1 variants encoded locally with ffmpeg if available (else download provided encodes) | `encrypted-random`: 200 MB from a CSPRNG + the same file AES-encrypted (control for the entropy gate) |
| `vm-image` | A small virtual disk | Alpine Linux "virt" ISO (MIT-ish/open) and a CirrOS qcow2 → raw (small: 60–120 MB; full: a 2–4 GB Ubuntu cloud image converted to raw) | — |
| `model-weights` | fp16/bf16 tensors | HuggingFaceTB/SmolLM2-135M safetensors (Apache-2.0, ~270 MB); prajjwal1/bert-tiny (MIT) for `small` | — |
| `archives-nested` | ZIP/JAR/APK/EPUB/7z containing the above | Build locally: ZIP (Info-ZIP, 7-Zip Deflate, Windows Explorer ZIP) of office/source/logs subsets; a JAR from Maven Central (Apache-2.0); an EPUB from Project Gutenberg | Tests container peel and the recognised/unrecognised Deflate encoder split |

Manifest schema (`manifest.json`): `{ "profile", "built_at", "classes": { "<class>": { "files": [ { "path", "bytes", "blake3", "source", "licence" } ], "bytes_total" } } }`. Paths are relative to the corpus root and sorted; the manifest is written with sorted keys and no timestamps inside file entries so rebuilds are byte-identical (`built_at` lives in a separate `build-info.json`).

Private mode: `lpk-bench corpus scan --private <dir>` assigns classes by extension + magic bytes, writes a manifest with hashes only (no content copied), and the runner uses it exactly like the public corpus. Results from private runs are tagged `private: true` and excluded from any published report unless the user opts in.

Blended-disk weights for the report (three personas): photo/document-heavy (JPEG 40%, office-pdf 15%, video 15%, photo-raw-png 10%, other 20%); developer (source 25%, software 20%, logs 15%, model-weights 15%, office 10%, other 15%); video-heavy (video 60%, JPEG 20%, other 20%).
