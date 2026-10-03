# P0-4 pre-flight notes

Prepared on 2026-10-02 while P0-3 was in review, so that P0-4 can start without repeating the research.
Nothing here is decided beyond what `docs/DECISIONS.md` says; the brief drafts are working material for
the implementers and may be changed. Crate sources in the local Cargo registry are the authority for
any API detail; this page is a pointer.

## Plan of work
1. **Framework first** (one implementer, then review): the `probe` command, the common result envelope with
   validation and table rendering, shared helpers, all dependencies (so later probe tasks do not touch
   `Cargo.toml`, `Cargo.lock` or `deny.toml`), a stub per probe, and `probe weights` as the worked example.
2. **Then the other probes in parallel**, at most four at a time, one file each: `jpeg`, `deflate`, `dedup`,
   `text`; `entropy-gate` after them. Each gets its own review.
3. Official probe runs for the report are made later on an idle machine; the implementers' runs are smoke tests.

Points settled while preparing (to be recorded in `docs/DECISIONS.md` when the code lands):
- `preflate-rs` 0.7.6 on crates.io has only the stream-level API; its container scanner is not published.
  The Deflate probe therefore walks containers itself (the PLAN text says "use the crate's container
  support"). No git dependency.
- `cabac` (LGPL-3.0-or-later, required by `preflate-rs`) gets the named `deny.toml` exception that D-01 and
  `docs/LICENSING.md` already provide for. Every other planned dependency passed the allow-list in a trial.
- The entropy-gate probe also measures the raw read speed of the disk, which the D-07 video gate needs.
  It needs `libc` on Unix for `O_DIRECT` (add it with the other dependencies in the framework task).
- XWRT is GPL and no permissively licensed equivalent was found: the text probe records that pre-pass as skipped.
- `bsc`, `kanzi` and `hdiffz` are optional and not installed on the measuring machine (owner's call).

## Verified facts

### Licence pre-check (a trial crate checked with `cargo deny check licenses` against the repo's `deny.toml`)
- lepton_jpeg 0.5.8 (Apache-2.0), preflate-rs 0.7.6 (Apache-2.0), fastcdc 5.0.0 (MIT), zstd 0.14.0 (BSD-3-Clause; zstd-sys 2.1.0+zstd.1.5.7), liblzma 0.4.8 (MIT OR Apache-2.0; liblzma-sys 0.4.9, `static`): whole tree passes EXCEPT `cabac 0.15.0` = LGPL-3.0-or-later (required by preflate-rs).
- Already decided (D-01, docs/LICENSING.md line 8, comment in deny.toml): named exception for `cabac` when preflate-rs is introduced. No owner decision is needed; record a decision entry when it lands.

### lepton_jpeg 0.5.8
- API: `encode_lepton(reader, writer, &EnabledFeatures, thread_pool)`, `decode_lepton(...)`, `encode_lepton_verify`, `dump_jpeg`; return `Result<Metrics, LeptonError>`; `DEFAULT_THREAD_POOL` exists. Encode reader must be buffered + seekable.
- `ExitCode` variants usable as failure causes: `Unsupported4Colors` (CMYK), `ProgressiveUnsupported` (only when `progressive` off), `SamplingBeyondTwoUnsupported`, `UnsupportedJpeg`, `UnsupportedJpegWithZeroIdct0`, `InvalidResetCode`, `InvalidPadding`, `CoefficientOutOfRange`, `ShortRead`, `AssertionFailure`, `OsError`, `VerificationLengthMismatch`, `VerificationContentMismatch`, `OutOfMemory`; decode side `StreamInconsistent`, `VersionUnsupported`, `BadLeptonFile`.
- `EnabledFeatures` fields: `progressive`, `reject_dqts_with_zeros`, `use_16bit_dc_estimate`, `use_16bit_adv_predict`, `accept_invalid_dht`, `stop_reading_at_eoi`, `max_jpeg_width`, `max_jpeg_height` (16386 for writing), `max_partitions` (8), `max_processor_threads` (8), `max_jpeg_file_size` (128 MB). Presets `compat_lepton_vector_write()` etc.
- UNCONFIRMED from docs: behaviour on arithmetic-coded / 12-bit / lossless JPEGs, trailing data limits, identical output across CPUs/thread counts (the probe must test bit-exactness itself and classify causes by its own marker scan: SOF2 progressive, SOF9+ arithmetic, 4 components/Adobe APP14, MPF APP2 / data after EOI for gain maps).

### preflate-rs 0.7.6
- Published crate is STREAM-LEVEL ONLY: `preflate_whole_deflate_stream(compressed, &PreflateConfig) -> Result<(PreflateStreamChunkResult, PlainText)>`, `recreate_whole_deflate_stream(plain, corrections) -> Result<Vec<u8>>`, streaming `PreflateStreamProcessor` / `RecreateStreamProcessor`. `PreflateConfig { max_chain_length, plain_text_limit, verify_compression }`.
- `PreflateStreamChunkResult { corrections, compressed_size, parameters: Option<TokenPredictorParameters>, blocks }`; `TokenPredictorParameters { strategy, window_bits, nice_length, max_chain, hash_algorithm, add_policy, zlib_compatible, .. }`. Errors: `ExitCode::NoCompressionCandidates`, `PredictionFailure`, `InvalidDeflate`.
- README: recognised encoders = zlib, zlib-ng, libdeflate, miniz_oxide, Windows zlib; others round-trip with more overhead.
- The container scanner (`preflate_whole_into_container`, in the repo's `container/` directory) is NOT on crates.io (checked with `cargo search`). => `probe deflate` must walk containers itself (ZIP entries raw, PNG IDAT, PDF FlateDecode, gzip, raw zlib scan) and call the stream API. PLAN text says "use the crate's container support" — deviation to record as a decision when the probe lands (no git dependency).

### Delta tools
- `zstd --patch-from=<old> <new> -o <patch>`; apply `zstd -d --patch-from=<old> <patch> -o <new>`; single files; large references need `--long=N` (decoder too when window > 27) and `-M` memory limit; reference limit about 2 GB. (zstd v1.5.7 manual + wiki "Zstandard as a patching engine")
- HDiffPatch: MIT, v5.1.3 (2026-07-31), GitHub release assets `hdiffpatch_v5.1.3_bin_windows64.zip`, `..._bin_linux64.zip`; `hdiffz old new out.diff`, `hpatchz old diff out`; directories accepted directly; `-c-zstd-N`, `-c-lzma2-N`, `-p-N` threads, `-m`/`-s` match modes. Not installed here; not in Ubuntu apt (unconfirmed for winget).

### Optional text baselines (not installed here)
- libbsc: Apache-2.0, v3.3.12 (2025-09-10), release asset `bsc-3.3.12-x64.zip`; `bsc e in out`, `bsc d in out`, `-b<MB>` block size, `-t`/`-T` threading.
- kanzi-cpp: Apache-2.0, 2.6.0 (2026-09-26), NO binaries attached to releases (would need building); `-c`/`-d`, `-i`, `-o`, `-b`, `-l 0..9`, `-j` jobs.
- XWRT: GPLv2 (per its SourceForge page; unconfirmed from repo) => not usable; no permissive XWRT-style implementation found => "skip and note".

### Crates
- fastcdc 5.0.0: `fastcdc::v2020::FastCDC::new(&data, min, avg, max)`; also streaming variant.
- zstd 0.14.0: feature `zstdmt` for multithreaded compression; prefix/patch-from equivalent not checked (zstd-safe has `ref_prefix`).
- liblzma 0.4.8: features `parallel` (multithreaded encoder), `static`.

## Brief draft: the framework task (P0-4a)

Implement **P0-4a: the probe framework and `probe weights`** — the first sub-task of P0-4 in `docs/PLAN.md`. Read the whole P0-4 block and the first checkbox of P0-5 (the report is the consumer of what you produce). Follow `CLAUDE.md` strictly.

**First:** your worktree starts from `main`; check `git log -1` shows <FILL: main head subject>. Do not run `git stash`. Work in few, large steps and **commit after each numbered item** with tests passing; if you run short of turns, stop at a commit and report what is left.

### What exists (read, reuse, do not rewrite)
- `lpk-bench probe` is a stub in `crates/lpk-bench/src/main.rs`.
- `crates/lpk-bench/src/corpus/manifest.rs`: manifest and `build-info.json` types. `crates/lpk-bench/src/run/host.rs`: `check_build` (clean-build gate), host description, `free_results_dir_name`. `crates/lpk-bench/src/run/exec.rs`: verification of a class's input files against the manifest before use. `crates/lpk-bench/src/run/validate.rs`: `run --validate <dir>`. `bench/results/README.md` and `docs/BASELINES.md`: the conventions for result directories (units, what may appear in a result file).

### 1. Command
`lpk-bench probe <name|all> [--profile small|full] [--corpus <dir>] [--results <dir>] [--threads N] [--allow-dirty-build]`, names `jpeg`, `deflate`, `dedup`, `text`, `weights`, `entropy-gate`. Defaults as for `run`. `--results` may name a new directory (default: `free_results_dir_name`) or an existing results directory — of a baseline run or of earlier probes — on the same host and for the same corpus (same manifest hash); otherwise refuse. The clean-build gate applies as for `run`. `probe all` runs each probe, continues after one fails, and exits non-zero if any failed.

### 2. Output
For each probe, `probe-<name>.json` and `probe-<name>.md` in the results directory (plus `host.json` if the directory is new).
- JSON envelope, same for every probe: probe name, format version, corpus identity (profile, manifest hash, `private`), build stamp, host name, date, thread count, `libraries` (name → version of every compression library the probe used, including linked C libraries), elapsed seconds, `notes` (skipped parts and why), and `data` — the probe's own typed structure.
- Store raw quantities (bytes, seconds, counts); percentages and MB/s are computed when rendering. Use the same unit conventions as `docs/BASELINES.md`.
- The Markdown table is produced by a pure function from the parsed JSON (the report in P0-5 will call the same function), never from live values. No number is printed or written anywhere else except a short console summary taken from the same JSON.
- Output is deterministic apart from timings, date and host: fixed ordering of files, classes and keys.
- Private corpus (`build-info.json` has `private: true`): per-file records carry no file names or paths (an index only), and the file says `private: true`.

### 3. Validation
`run --validate <dir>` also checks `probe-*.json`: typed parse with unknown fields rejected, the consistency rules each probe declares (for example, counts add up; reconstructed sizes match), agreement of host and corpus with the directory's other files, and `probe-<name>.md` equal to the table rendered from the JSON. A directory holding only probe files is valid. Document the format in `bench/results/README.md`.

### 4. Shared helpers (in `crates/lpk-bench/src/probe/`)
- Class file access through the manifest with input verification before use (a mismatch aborts, naming the file).
- An order-preserving parallel map over items using `std::thread::scope` (no new dependency) for work where only sizes matter.
- A timing helper for throughput. Rule: a timed section runs alone (no other probe work in parallel), on data already in memory, and the JSON records how many threads the library used.

### 5. Dependencies — add all of them now, so the later probe tasks do not touch `Cargo.toml`, `Cargo.lock` or `deny.toml`
`zstd` 0.14 (with multithreading), `liblzma` 0.4 (static, with the multithreaded encoder), `lepton_jpeg` 0.5.8, `preflate-rs` 0.7.6, `fastcdc` 5. `cargo deny check` must stay green: the only licence outside the allow-list is `cabac` (LGPL-3.0-or-later, required by `preflate-rs`); give it the named exception that the comment in `deny.toml` and `docs/LICENSING.md` already prescribe (decision D-01), and add nothing else to the allow-list. If any other crate in the tree fails the licence check, stop and report it — do not widen the list.

### 6. Stubs
One module per probe (`probe/jpeg.rs`, `probe/deflate.rs`, `probe/dedup.rs`, `probe/text.rs`, `probe/weights.rs`, `probe/entropy_gate.rs`), each with one entry function and its `data` type. Five of them return a "not implemented yet" error, so that later tasks edit only their own file and its tests.

### 7. `probe weights` (complete)
On the `model-weights` class. For each safetensors file: parse the header (8-byte little-endian length, then JSON mapping tensor name → dtype, shape, data offsets; reject malformed or out-of-range headers without panicking). For each floating-point tensor (F16, BF16, F32, F64): split its bytes into byte planes (byte *i* of every element), compress each plane with zstd level 19, and compare with zstd level 19 on the same tensor bytes unsplit; also record zstd level 19 on the whole file. Verify that merging the planes reproduces the tensor bytes exactly. Record per file and per dtype: original bytes, plain compressed bytes, plane-split compressed bytes, split+compress seconds, decompress+merge seconds. Files that are not safetensors are counted and listed as not parsed.

### Tests
Header parsing (valid, truncated, offsets out of range, overlapping tensors, huge declared length); plane split and merge round trip for every element width; envelope validation (unknown field, table that does not match the JSON, host or corpus mismatch); private-corpus redaction; `probe all` continuing after a failing probe; an end-to-end run of `probe weights` on a tiny corpus built in a temp directory (JSON and table written, directory validates).

### Constraints
- `#![forbid(unsafe_code)]` stays; no `unwrap` outside tests; platform-specific code only under `cfg` with both branches (CI builds and tests on Linux and Windows).
- No size, ratio or speed figure in any doc, comment, test name or commit message. Numbers belong only in the probe's JSON and table, and in your report to me.
- No absolute paths, user names or environment dumps in result files.
- Do not tick PLAN boxes or edit `docs/DECISIONS.md` or `docs/LICENSING.md`.

### Session notes
- Commit in your worktree; do not push. `cargo` is not on your shell's PATH: start Bash commands with `export PATH="$HOME/.cargo/bin:$PATH"`.
- CI runs, with `RUSTFLAGS="-D warnings"`: `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo build --locked --workspace --all-targets`, `cargo nextest run --locked --workspace --no-tests=pass`, `cargo deny check`. Run exactly these before reporting.
- The built small corpus is at `<main checkout>/bench/corpus/small` (read-only). Write real-run output under your worktree's `target/`, never under `bench/results/`.
- Do not launch PowerShell scripts or kill processes by image name. If a command is refused by your sandbox, report it; do not ask me to run it for you.

### Report back
Branch and commits; which items are done; the envelope's field list; the exact `deny.toml` change; what `probe weights` printed on the small profile (table as produced) and how long it took; commands with pass/fail; open questions (max 3). No file contents, no diffs.

## Brief drafts: the probes


### COMMON (prepend to every probe brief)
Implement **`probe <NAME>`** — one probe of P0-4 in `docs/PLAN.md` (read the whole P0-4 block and the first checkbox of P0-5). Follow `CLAUDE.md` strictly.

**First:** your worktree starts from `main`; check `git log -1` shows <FILL>. Do not run `git stash`. Work in few, large steps, commit after each numbered item with tests passing; if you run short of turns, stop at a commit and report what is left.

**What exists:** the probe framework in `crates/lpk-bench/src/probe/` — read `mod.rs` and `weights.rs` (the worked example) first: the result envelope, the table renderer, validation hooks, class access with input verification, the order-preserving parallel map, the timing rule (a timed section runs alone, on data already in memory). Your probe is the stub `crates/lpk-bench/src/probe/<FILE>.rs`. Edit only that file and its tests. All dependencies you need are already in `Cargo.toml`; do not touch `Cargo.toml`, `Cargo.lock` or `deny.toml` — if you believe you need another crate, stop and report. If you need a change in the shared framework, keep it minimal and list it in your report.

**Rules for every probe**
- Raw quantities in the JSON (bytes, seconds, counts); percentages and MB/s only when rendering the table. No number anywhere except the JSON, the table rendered from it, and your report to me.
- Deterministic output apart from timings, date and host. Private corpora: no file names or paths in the JSON.
- Every transformation is verified by reversing it and comparing bytes; a mismatch is counted as a failure with its cause, never silently dropped.
- Declare validation rules for your `data` (counts add up, sizes consistent) so `run --validate` checks them.
- `#![forbid(unsafe_code)]`, no `unwrap` outside tests, platform code under `cfg` with both branches, no figures in docs, comments, test names or commit messages.
- Tests run without the corpus: build tiny inputs in the test (or in a temp directory); cover each failure cause you report.

**Real run:** `probe <NAME> --profile small --corpus "<main checkout>/bench/corpus/small"` with results under your worktree's `target/`; it must finish in under 30 minutes. Report the table it printed and the elapsed time.

**Session notes:** commit in your worktree, do not push. `export PATH="$HOME/.cargo/bin:$PATH"` first. CI commands (with `RUSTFLAGS="-D warnings"`): `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo build --locked --workspace --all-targets`, `cargo nextest run --locked --workspace --no-tests=pass`, `cargo deny check`. No PowerShell scripts, no killing processes by image name; if the sandbox refuses a command, report it, do not ask me to run it. Leave no process running when you finish.

**Report back:** branch and commits; what is done; the table as printed and the elapsed time; framework changes if any; commands with pass/fail; open questions (max 3). No file contents, no diffs.

### SPECIFIC: jpeg  (file `jpeg.rs`)
Input: every file in classes `photo-jpeg` and `photo-jpeg-edited` that starts with a JPEG SOI marker.
1. **Describe each file by your own marker scan** (independent of the library): frame type (baseline, extended sequential, progressive, lossless, arithmetic-coded), component count, sample precision, width and height, restart interval present, an MPF `APP2` segment or gain-map XMP (`hdrgm`) present, bytes after the first image's EOI.
2. **Encode with `lepton_jpeg` 0.5.8** using the library's default write features (record the feature values in the JSON), decode, and compare with the input. Record per file: outcome, Lepton size, encode seconds, decode seconds. Timed with the library's default threading, and again with one processor thread (record both).
3. **Failure causes, reported explicitly:** progressive, CMYK / four components, arithmetic-coded, dimension cap, gain map / multi-picture, trailing data over 4 MiB, verification mismatch, other (with the library's `ExitCode` name). A failed file counts as stored as-is (output size = input size). Decide the cause from the library's exit code first and your marker scan second; state the mapping in a comment and test each branch with a synthetic input where you can build one.
4. **Summary:** per class, per sub-source (top-level folder under the class) and overall: files, input bytes, bytes after Lepton for the successes, bytes after fallback, counts and bytes per failure cause, encode and decode seconds (both thread settings).

### SPECIFIC: deflate  (file `deflate.rs`)
Note: `preflate-rs` 0.7.6 on crates.io has only the stream-level API (`preflate_whole_deflate_stream`, `recreate_whole_deflate_stream`); its container scanner is not published. So the probe walks containers itself. (Deviation from the PLAN wording, to be recorded as a decision by the orchestrator.)
Input: every file of classes `office-pdf`, `photo-raw-png`, `game-assets`, `archives-nested`, `software-installed`.
1. **Find Deflate streams by container kind**, decided by content, not extension: ZIP family (entries with method 8: the raw compressed bytes; label Office documents, JAR/APK and plain ZIP by the entry names or extension), PNG (the concatenated `IDAT` payload, a zlib stream), gzip members, PDF (at each `stream` keyword try a zlib stream), and for files of any other kind a scan for zlib streams at any offset (accept only streams that inflate cleanly to at least 1 KiB). One level only: do not scan inside inflated data. Bound memory with the library's plain-text limit; count streams skipped for size.
2. **For each stream:** run the analysis, record the correction size and the encoder parameters the library estimated (as the library names them), recreate the stream from plain data plus corrections and compare bytes; time analysis and recreation.
3. **Net gain per file:** A = the original file compressed with zstd level 19 and with xz preset 9 (each single-threaded, whole file, in-process); B = the file with every reconstructed stream replaced in place by its inflated data, compressed the same two ways, plus the sum of the correction sizes. Streams that failed stay as they are. Record A and B for both compressors.
4. **Summary per container kind and per class:** files, streams found, reconstructed exactly, failed (by the library's error name), skipped; streams grouped by the library's encoder estimate and by overhead bucket (corrections as a share of the Deflate stream size); totals of original bytes, A and B.

### SPECIFIC: dedup  (file `dedup.rs`)
Input: every class, plus the corpus as a whole.
1. **Chunking:** FastCDC (the crate's 2020 variant) with minimum 4 KiB, average 64 KiB, maximum 512 KiB, BLAKE3 per chunk. Per class and for the whole corpus: files, bytes, whole-file duplicates (count and bytes), chunks, unique-chunk bytes. Chunking seconds for FastCDC alone and FastCDC plus BLAKE3 (single thread, data in memory).
2. **Versions:** for `backup-versions`, the cumulative unique-chunk bytes after version 1, after 1+2, after 1+2+3 (versions are the top-level folders).
3. **Delta between consecutive versions:** make a deterministic tar of each version in-process (sorted entries, fixed metadata). With the `zstd` command-line tool found by the runner's discovery: `--patch-from` of the previous version at level 19 (with the long-window option the manual requires for inputs of this size), verified by applying it; compare with level 19 on the new version alone. With `hdiffz`/`hpatchz` if found on PATH or in `bench/tools.local.toml`: the same, verified by patching. A tool that is not installed is recorded as skipped with the reason. External tools are run through `lpk-procstat-sys` like the baseline runner does (no shell, relative paths, scratch under `bench/tmp`).

### SPECIFIC: text  (file `text.rs`)
Input: classes `text-prose`, `logs-text`, `small-files` and `backup-versions`, each as one solid stream (deterministic in-process tar).
1. In-process, single-threaded: xz preset 9 and zstd level 19 — compressed size, compress seconds, decompress seconds, round trip verified.
2. If found (PATH or `bench/tools.local.toml`): `bsc` and `kanzi` at their strongest documented general settings, run through `lpk-procstat-sys`; same quantities; round trip verified. Not installed → skipped with the reason.
3. XWRT-style word-replacement pre-pass: XWRT is GPL and no permissively licensed implementation was found, so record it as skipped with that reason. Do not write one.

### SPECIFIC: entropy-gate  (file `entropy_gate.rs`)
1. **Blocks:** 1 MiB blocks of every file (a final partial block counts if it is at least 64 KiB). Main table: classes `video` and `encrypted-random`. Second table: all other classes (the false-positive check).
2. **Per block:** order-0 Shannon entropy of the whole block; the same on a sample (the first 4 KiB of every 64 KiB); zstd level 1 compressed size; xz preset 9 compressed size (the ground truth).
3. **Evaluation stored as counts** (true/false positives and negatives) for each gate and threshold — entropy and sampled entropy at 7.0, 7.5, 7.8, 7.9, 7.95, 7.99 bits per byte; zstd level 1 output at 90, 95, 98, 99 percent of the input — against the ground truth "xz output at least 95 / 98 / 99 percent of the input". Precision and recall are computed when rendering.
4. **Gate cost:** seconds for each gate (full entropy, sampled entropy, zstd level 1) over all blocks, single thread, data in memory, timed alone.
5. **Raw read speed** (needed by the D-07 video gate): read every corpus file sequentially with the operating system's cache bypassed — Windows `FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN` through `std::os::windows::fs::OpenOptionsExt::custom_flags`, Linux `O_DIRECT` — into a 4 MiB buffer aligned to 4096 bytes (take an aligned sub-slice of a larger `Vec`; no `unsafe`). Three passes; record bytes and seconds of each. If unbuffered reading is refused by the file system, record a note and a buffered pass marked as cached.
