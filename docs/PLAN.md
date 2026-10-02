# LitePack — Implementation Plan

Status legend: `[ ]` not started · `[~]` in progress · `[x]` done (add one line of evidence under the task).
Owner column suggests which subagent/model to use (see `README.md` → "Working with Claude Code").

Gates are decided in `docs/DECISIONS.md` (D-07). Phase 1 does **not** start until the Phase 0 report says GO.

---

## Phase 0 — Prove it (target: 3–4 weeks)

Goal: a reproducible corpus + harness that measures every claim in `docs/LitePack-Method-v2.md` §5 against real tools, and a GO/NO-GO report.

### P0-1 Repository bootstrap — owner: implementer (sonnet)
- [x] Cargo workspace with `crates/lpk-bench` (binary, clap CLI), `rust-toolchain.toml` (stable), `deny.toml`, `.gitignore`, `LICENSE-MIT`, `LICENSE-APACHE` (fetch canonical text from https://www.apache.org/licenses/LICENSE-2.0.txt), `README.md`. Evidence: workspace builds; `LICENSE-APACHE` is the canonical 11358-byte text; `cargo deny check` exit 0 ("advisories ok, bans ok, licenses ok, sources ok").
- [x] CI: `.github/workflows/ci.yml` runs fmt, clippy `-D warnings`, nextest, `cargo deny check` on `ubuntu-24.04` and `windows-2025`. Evidence: the first push (`80fc03e`) ran green — every step (fmt, clippy, build, nextest, `cargo deny check`) succeeded on both OSes: https://github.com/Charon89/litepack/actions/runs/36937314839.
- [x] `cargo run -p lpk-bench -- --help` prints the subcommands `corpus`, `run`, `probe`, `report`. Evidence: `cargo run -p lpk-bench -- --help` lists corpus, run, probe, report; `cargo nextest run --workspace` 7 passed.
- Acceptance: CI green on both OSes on the first push; `cargo deny check` passes with the allow-list in `deny.toml`. Met 2026-10-01: run 36937314839 (above) was the first push and is green on both OSes, including `cargo deny check` with `deny.toml` unchanged from the starter pack.
- Parked (LOW, from the P0-1 review): `scripts/setup-windows.ps1` prints its "could not update the Rust toolchain" warning on every run if rustup came from a package manager that disables `rustup self update`; check only the toolchain update, or reword, when the script is next touched.

### P0-2 Corpus specification and builder — owner: implementer (sonnet); spec review: reviewer (opus)
- [~] Implement `docs/CORPUS.md` exactly: `lpk-bench corpus build --profile {small|full} --out <dir>` downloads/derives every class, writes `manifest.json` (per file: relative path, size, BLAKE3, class, source URL/licence) and `corpus.lock` (pinned URLs + hashes). Evidence so far: `corpus build --profile small` at `14f9977` builds 17 classes from `bench/corpus-sources.toml`, every download pinned in `bench/corpus.lock`; where the spec could not be followed literally see D-14, D-15, D-17 and D-18. Open: the `full` profile is not pinned or built yet.
- [x] `--private <dir>` mode: builds a manifest over the user's own folder with the same class heuristics (by extension + magic), never copies or uploads anything. Evidence: `corpus scan --private <dir> --out <dir>`; tests show the scanned folder is left untouched (same listing, sizes and times before and after), `--out` inside the scanned folder is refused, and two scans give byte-identical manifests; three review passes, CI green on both OSes at `8571575`.
- [x] Derived classes are generated deterministically (edited-photo versions, backup v1/v2/v3 snapshots, small-file set). Evidence: three builds of `small` at `14f9977`, the third after deleting the output, give byte-identical `manifest.json` (BLAKE3 `8daaaf66f1e06460ce264c73c101aa5dc388668b2006d82c803b7fe016124db0`); golden BLAKE3 tests pin one output of every in-process derivation and pass on Linux and Windows CI (run for `5d884b4`).
- Acceptance: `corpus build --profile small` completes on Windows and Linux from a clean machine (only `curl`/`git`/`ffmpeg` optional); manifest has ≥ 11 classes; second build produces a byte-identical `manifest.json`; total size for `small` is 1–2 GB, `full` 10–20 GB. Status 2026-10-01: Windows — three builds at `14f9977` give 29,360 files, 1,809,103,393 bytes, 17 classes, byte-identical manifests, nothing skipped, unavailable, missing or altered. Linux — the `corpus-smoke` workflow on a clean `ubuntu-24.04` runner at `ab0aab8` built `small` cold, then again from scratch, with identical manifests (https://github.com/Charon89/litepack/actions/runs/36959483153); comparing its uploaded `manifest.json` with the Windows one, 16 of the 17 classes are byte-identical (same paths and BLAKE3, including the git-derived classes under a different git version), and `video` differs only because the runner has no ffmpeg, so its two derived encodes are skipped and recorded. `full` — not built yet.
- Note (from the P0-1 review): write `corpus.lock` to a committed path (e.g. `bench/corpus.lock`), not inside the git-ignored corpus directory, so other machines rebuild the same corpus (D-13).
- Open (carried forward): `office-versions` is not built (LibreOffice re-saves are not byte-deterministic); `model-weights` in `small` is bert-tiny, which is fp32 rather than fp16/bf16 (matters for `probe weights` in P0-4); the private scan has not been tried on a OneDrive-backed folder; on Windows `libz-sys` can link a vcpkg zlib when vcpkg is configured (the golden tests would catch a difference).

### P0-3 Baseline tool runner — owner: implementer (sonnet)
- [~] `lpk-bench run --tools <list|all> --profile small --repeats 3`: for each class × tool × setting: compress, measure wall time, CPU time, peak RSS, output size; extract to temp; verify every file's BLAKE3 against the manifest; delete temp. Median of repeats.
- [~] Tool adapters (skip gracefully if not installed; record why): 7-Zip (`7z`/`7zz`; `-mx5`, `-mx9 -mqs`), WinRAR `rar` (`-m3`, `-m5 -md256m`, `-m5 -rr3%`; licence note), zstd (`-3`, `-19`, `--ultra -22 --long=27`), xz (`-6`, `-9`), zpaqfranz (`-m1`, `-m5`), t-saur (default + max), WinZip `wzzip` and PowerArchiver `PACL` as **optional/manual** adapters with a documented procedure if the user has licences; "store" (tar, no compression) as the control.
- [~] Peak RSS: Windows Job Object (`windows` crate: `JOB_OBJECT_LIMIT_INFORMATION`/`QueryInformationJobObject` → `PeakProcessMemoryUsed`); Linux `getrusage`/`/usr/bin/time -v`.
- [~] Results: `bench/results/<date>-<host>/<tool>-<setting>-<class>.json` with a schema file `bench/results/schema.json`; `bench/tools.toml` records tool versions.
- Acceptance: on the `small` profile all installed tools complete with 100% round-trip verification; a second run differs by < 3% in time; results validate against the schema.
- Note: started 2026-10-02 while P0-2's `full` profile is still unpinned — the runner needs only the `small` profile and the manifest format; `full` is needed for P0-5.

### P0-4 Component probes (the headroom experiments) — owner: implementer (sonnet), 4 probes may run as parallel worktree subagents
Each probe is a `lpk-bench probe <name>` subcommand producing `bench/results/.../probe-<name>.json` and a Markdown table.
- [ ] `probe jpeg`: run `lepton_jpeg` encode→decode on every JPEG in the photo class; record gain %, encode/decode MB/s, bit-exact verification, and the **failure rate** by cause (progressive, CMYK, arithmetic, dimension cap, UltraHDR/gain-map, trailing data > 4 MB). Store-as-is fallback counted.
- [ ] `probe deflate`: run `preflate-rs` over every Deflate stream found in PDF/Office/PNG/ZIP/JAR classes (use the crate's container support + a raw-stream scanner); record recognised vs unrecognised encoder, correction overhead %, and **net gain** after re-compressing the inflated data with zstd -19 and xz -9 versus compressing the original file. Per-container summary.
- [ ] `probe dedup`: FastCDC (avg 64 KB, min 4 KB, max 512 KB) + BLAKE3 over backup/project/photo classes; record unique-chunk ratio, whole-file-duplicate ratio, chunking throughput; also `zstd --patch-from` and `hdiffz` (if installed) delta sizes between backup versions.
- [ ] `probe text`: compress text/log/source classes with xz -9, zstd -19, bsc (if installed) and kanzi (if installed); record ratio and speed; optional XWRT-style pre-pass if a permissive implementation is available (else skip and note).
- [ ] `probe weights`: on the model-weights class, byte-plane split (exponent/mantissa planes for bf16/fp16) + zstd -19 per plane vs plain zstd -19; record gain.
- [ ] `probe entropy-gate`: per 1 MB block, Shannon entropy and zstd -1 ratio over the video/encrypted class; record how reliably a cheap gate predicts "incompressible" (precision/recall vs xz -9 ground truth).
- Acceptance: each probe runs on the `small` profile in < 30 min on an 8-core machine; JSON + table produced; `probe jpeg` and `probe deflate` report failure causes explicitly; no probe writes a number anywhere except its JSON/table.

### P0-5 Report and GO/NO-GO — owner: bench-runner (haiku) for runs, reviewer (opus) for the verdict
- [ ] `lpk-bench report --results <dir>` writes `bench/reports/phase0-<date>.md`: per-class table (size %, compress MB/s, extract MB/s, peak RSS) for every tool; probe tables; **blended estimate** for three disk mixes (photo/doc-heavy, developer, video-heavy); gate evaluation (D-07) with PASS/FAIL per gate.
- [ ] Run the `full` profile once on the primary Windows machine (and `small` on Linux CI as a smoke test); commit results and the report.
- [ ] Record the verdict in `docs/DECISIONS.md` (D-08) with the numbers.
- Acceptance: report generated by one command from committed JSON; every number in the report traces to a JSON file; verdict recorded.

### P0-6 Licensing and FTO checklist — owner: researcher (sonnet), reviewer (opus)
- [ ] `cargo deny check` policy finalised (allow: Apache-2.0, MIT, BSD-2/3, 0BSD, ISC, CC0-1.0, Unicode-3.0, Zlib, MPL-2.0 (dependency only); LGPL-3.0 flagged as an exception with the crate name — allowed because the engine is open source; GPL denied).
- [ ] `docs/LICENSING.md` lists every third-party component planned for Phase 1 with licence, link, and how it is used (depend/link/sandbox/reference-only).
- [ ] FTO list reviewed and each patent assigned "not relevant to Phase 1 / needs counsel before Phase N" (see `docs/LICENSING.md`).
- Acceptance: `cargo deny check` green; LICENSING.md complete; no GPL-licensed code in the tree (grep for "GNU General Public License" in `vendor`/`crates` returns nothing).

---

## Phase 1 — Beat them on real data (outline; detail after the GO decision)

Each epic gets its own task breakdown (10–25 tasks) written **after** P0-5, using the measured numbers to set priorities (for example: if `probe deflate` shows most PNGs come from unrecognised encoders, PNG peel is deprioritised).

- **E1 Format spec v1 + reference decoder.** `.lpk` container: frames, seek table, BLAKE3 Merkle tree, decode-resource envelope, reconstruction records, journal. Deliverable: `docs/spec/lpk-v1.md` + `crates/lpk-format` (pure Rust, `forbid(unsafe)`), conformance test vectors. Gate: an independent decoder written only from the spec decodes all vectors.
- **E2 Pipeline core.** `crates/lpk-core`: ingest, classifier (byte statistics + magic), Fold (CDC + BLAKE3 + file-level similarity ordering + deltas), Peel (containers, Deflate via preflate-rs, JPEG via lepton_jpeg, PNG filters, base64/UTF-16), Model (zstd fast tier; LZMA-class balanced; BWT only for text), Seal (AES-256-GCM/Argon2id, Reed-Solomon recovery via `reed-solomon-simd`, journaling). Gate: round-trip on the full corpus with 0 mismatches; Balanced tier meets D-07 gates.
- **E3 CLI (`lpk`).** `a/x/l/t/repair`, tiers `--fast/--balanced/--strong`, `--password`, `--recovery 5%`, `--export zip|7z`, progress, exit codes, UTF-8/VT output. Gate: CLI drives the full corpus benchmark through the harness.
- **E4 Sandboxed workers.** `lpk-worker.exe` with restricted token + job object; foreign-format parsers (ZIP/7z/RAR/tar/ISO via sandboxed libarchive/7-Zip SDK) run only there. Gate: a deliberately crashing parser cannot take down the host; fuzz targets for every Peel parser run nightly on Linux CI.
- **E5 Windows GUI (Tauri 2).** Archive browser with virtual list (1M entries), create/extract wizards, tier selector with time estimates, password + recovery options, preview pane. Gate: usable end-to-end on Windows 10 2004+ and Windows 11, x64 and ARM64.
- **E6 Explorer integration.** `lpk-shell` windows-rs cdylib implementing `IExplorerCommand` (Windows 11 menu, with sparse-package identity for the NSIS install and full MSIX for Store), `IContextMenu` (classic), file associations, MoTW propagation via `IAttachmentExecute`, extraction policy (paths, symlinks, ADS). Gate: menu appears on Windows 11 for files/folders/background; extraction policy tests pass.
- **E7 Packaging, signing, updates.** MSIX (Store/winget) + Tauri NSIS installer + portable zip; Azure Artifact Signing with timestamping; Tauri updater with signed manifests; silent-install flags. Gate: clean install/update/uninstall on a fresh VM; SmartScreen shows the publisher.
- **E8 QA and release engineering.** Fuzzing corpus, 10k-file real-world round-trip CI job (private corpus mode), reproducible builds (`/Brepro`), SBOM + attestations, `cargo deny` in CI, benchmark regression check on every PR. Gate: zero memory-safety findings; benchmark regression < 1% blocks merge.
- **E9 Docs and site.** Format spec published, user docs, benchmark page generated from `bench/reports`.

Phase 1 success criteria (from D-07): Balanced tier ≥ 10% smaller than WinZip Zipx and PowerArchiver `.pa` and ≥ 15% smaller than 7-Zip Ultra on photo/document classes; ≥ 2× smaller on the versioned-backup class; video class stored at ≥ 80% of raw disk read speed; extraction no slower than 7-Zip; 0 memory-safety findings.

---

## Phase 2+ (not planned in detail)
Strong tier (CM backend), OpenZL structured engine, model-weights class in the product, text/log transforms, executable deltas, GPU BWT, macOS/Linux builds, enterprise features. Research track: pretrained priors + integer kernels + mismatch-tolerant coder (blocked on FTO, see `docs/LICENSING.md`).
