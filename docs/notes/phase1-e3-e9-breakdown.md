# Phase 1 — E3 to E9 task breakdown (draft)

> **Status: draft for adoption, cut 2026-10-17 by a read-only planning agent from PLAN's Phase 1 outline, the E1/E2
> breakdown (`docs/notes/phase1-e1-e2-draft.md`), decisions D-01 to D-73, the method document §6.4, §8, §9 and §12, and
> the code on `main` at `e654bc1`.** Nothing here is decided; a "proposed" item becomes a `D-NN` when its task lands. Task
> lines follow the adopted E1/E2 shape: bold title, scope, an `Acceptance:` checkable by a command or a committed artefact.
> Each title carries a size (S: a day of a subagent; M: two or three; L: a week or more, several review rounds) and where it
> must run (any OS unless marked Windows; "human" = the owner's own step). No compression or speed figure appears in this
> file (CLAUDE.md); where a task measures, the number goes into `bench/`. E3 is adopted into `docs/PLAN.md` on 2026-10-17
> with its decisions ruled (R22, section 2); E4 to E9 wait for the owner's decisions.

## 1. What exists already (no task below redoes it)

- **CLI `lpk` (`crates/lpk-cli`):** `a x t check repair append rollback info`; `--fast` (default) `--balanced` `--store`;
  `--threads`, `--memory` (D-67); `--password`/`--password-file`/`--keyfile`, `--suite`, `--listable` (D-57);
  `--recovery 0–20` (D-58, default 0); `--level`, `--window-log`, `--dict-size`, `--no-dictionaries`, `--no-dedup`,
  `--ordering`, `--deflate-peel on|off`, `--prior`; exit codes 0/1/2, never a prompt (D-48); `-v` stage report;
  `--version` with the build hash. Catalogue row `lpk` in `bench/tools.toml` with settings `fast`, `balanced`,
  `balanced-extension`, `balanced-no-deflate`, `store`, `fast-encrypted`; the runner already drove the full corpus with it
  (E2-21, D-72) — PLAN E3's gate is met in substance and E3-9 only re-runs it on the final CLI.
- **Engine `lpk-core`:** ingest walk (D-44), classifier and gate (D-45), Fast (D-47) and Balanced (D-51) tiers, parallel
  blocks (D-60), the pipelined store path (D-63/D-65), JPEG (D-50) and Deflate (D-69–D-71) peels with the peel pool,
  FastCDC dedup (D-53), the block-ordered extraction engine with the `ExtractPolicy` hook and `DefaultPolicy` (D-54),
  memory budgets on both sides (D-67), encryption, recovery and journal through the pipeline (D-57–D-59),
  `register_full_reader`. Missing for the epics below: progress events, cancellation, a listing API, selective extraction,
  several input roots, an overwrite mode, foreign-format reading, ZIP/7z writing.
- **Format `lpk-format`:** v1 frozen (D-41), revisions 1.1 and 1.2 (1.3 in design, D-73); the reference tool `lpk-decode`
  whose extraction refusals (devices, colon, trailing dot or space, `..`, symlinks, overwrite) the CLI calls; the
  conformance vectors; the independent decoder `crates/lpk-check`; `fuzz/` with libFuzzer targets (every reader path, the
  Deflate walker and recreation) run weekly or by hand on Linux (`fuzz.yml`), NCSA exception (D-42).
- **Harness `lpk-bench`:** corpus build and the private scan mode, `run` (`--validate`, `--compare`, a private-corpus
  `{list}` template per tool), the probes, the pooled report (D-48); `corpus-smoke.yml` on Linux.
- **CI:** fmt, clippy `-D warnings`, build, nextest, `cargo deny` on `ubuntu-24.04` and `windows-2025`;
  `scripts/setup-windows.ps1`; `rust-toolchain.toml` on `stable`; `vendor/preflate-rs-0.7.6` (D-66); `lpk-procstat-sys`
  holds the Job Object code (D-19).
- **Nothing exists yet** for the GUI, the shell extension, the worker, packaging, signing, updates, a nightly fuzz corpus,
  SBOM, reproducible-build checks, ARM64 CI, third-party notices, or a site.

## 2. Decisions the owner must make before each epic starts

- **E3** (ruled for adoption, R22, the owner may overturn): (a) `--strong` reserved (parses, refused as Phase 2); (b) the
  product's default recovery percentage — CLI 0 (D-58), the GUI wizard proposing 5; (c) interactive password prompting only
  behind `--password-prompt` or when stdin is a terminal without a credential (D-48's "never a prompt" kept for the
  measured tool); (d) export formats: ZIP in Phase 1, 7z within E3-6 only if its writer crate passes `cargo deny` and
  round-trips, otherwise deferred; (e) overwrite modes `refuse|skip|rename|replace`, default `refuse` (today's behaviour).
- **E4:** (a) the parser stack: pure Rust (`zip`, `sevenz-rust2`, `ppmd-rust`, `tar`) in the worker for ZIP/7z/tar, plus
  libarchive 3.8 (BSD, already in `docs/LICENSING.md`) in an LPAC worker only if RAR and ISO are wanted in Phase 1 — RAR
  has no mature permissive pure-Rust reader and the `unrar` library's licence is not on the allow-list; the 7-Zip archive
  handlers are LGPL (D-26 would apply) and are not recommended; (b) the P0-6 open point: the pure-Rust foreign parsers also
  run in the worker (recommended), while `.lpk` decoding stays in-process bounded by D-66/D-67/D-68 and fuzzed; (c)
  `rappct` audited or replaced by direct `windows` calls; (d) the Linux worker runs unsandboxed (CI only); (e) nightly fuzz
  minutes per target.
- **E5:** (a) the Phase 1 GUI links `lpk-core` in-process and is therefore open source (D-26: `cabac` reaches any binary
  only through published source); a later closed Pro layer needs a process boundary — confirm; (b) minimum Windows: the
  method's 10 2004 is moot since Windows 10 left support on 2025-10-14 — Windows 10 22H2 plus 11, or 11 only; (c) no Node
  toolchain (`tauri-cli` via cargo, static vanilla front end); (d) telemetry none, crash reports none, the update check the
  only network call (opt-out); (e) the icon and visual assets (human; the Store needs a set); (f) the WebView2 bootstrapper
  mode of the installer (download, embedded, or offline).
- **E6:** (a) the COM cdylib cannot `forbid(unsafe_code)`: name it `lpk-shell-sys` or record a D-NN extending the
  exception to COM servers with `SAFETY:` comments required; (b) a "LitePack" submenu or flat verbs, and the verb list; (c)
  associations: `.lpk` only by default, ZIP/7z/tar "Open with" opt-in (recommended); (d) symlink entries skipped with a
  warning unless privileged and opted in (the method's rule); (e) the parent-handle rule (`NtCreateFile` relative opens,
  FFI) or re-verification before each create (E2-19's parked item); (f) MoTW propagation on by default.
- **E7:** (a) the signing provider: Azure Artifact Signing (the method document: short-lived certificates with mandatory
  timestamping, a small monthly fee, but public trust only for organisations or for individuals in the US and Canada —
  eligibility to be checked), or an OV certificate from a CA with a hardware token or cloud HSM (yearly cost, identity
  validation weeks), or SignPath Foundation for open source; the certificate subject becomes the MSIX publisher and must be
  fixed before E6-3 and E7-4; (b) channels: winget (free, a manifest PR) and the Store (individual registration free since
  2025 per the method document; needs a privacy policy URL and signs the MSIX itself) — recommended both, winget first; (c)
  the publisher's legal name, the product name and a trademark search (human); (d) the export-control self-classification
  note (method §10); (e) per-user install by default (no UAC); (f) the updater endpoint (GitHub Releases `latest.json`) and
  the minisign key custody; (g) ARM64 built cross on x64 or natively on `windows-11-arm`; (h) an ARM64 test device or a
  volunteer tester.
- **E8:** (a) where the private 10,000-file round-trip runs: a self-hosted runner on the owner's machine restricted to
  `main` and manual dispatch (fork PRs never), or a local scheduled task; and which folder; (b) the "benchmark regression <
  1% blocks merge" gate: applied to archive size on CI (deterministic, D-60) and to time only on the measuring machine with
  the threshold read against D-24's noise — a gate change is the owner's; (c) pin the toolchain to a version for
  reproducible builds; (d) the fuzz minutes budget.
- **E9:** (a) domain and hosting (GitHub Pages recommended); (b) the generator (mdBook, cargo-installable, no Node); (c) the
  docs licence; (d) whether the method document is published (recommended not in Phase 1; it is unchanged either way); (e)
  the privacy policy text (required by the Store) and the security contact.

## 3. Tasks

### E3 — CLI: closing the gaps and the gate (9 tasks; adopted into PLAN 2026-10-17)
- **E3-1 `lpk l` (list).** (S) `lpk l <archive>` prints path, kind, size, mtime, flags and the generation; `--long`;
  `--json` (one object per line) for scripts; a listable sealed archive lists without a key, a non-listable one says
  `PasswordRequired`; the entry table is streamed, control characters escaped as the reference tool does. Acceptance:
  `crates/lpk-cli/tests/cli.rs` lists the plain, sealed-listable, sealed and `journal-3gen` vectors and a generated
  million-entry archive within a counting-allocator bound; every `--json` line parses.
- **E3-2 The extraction policy module and overwrite modes.** (M) One audited `lpk_core::policy` module (the method's
  single policy module) that the CLI, the GUI (E5) and the shell (E6) call: today's rules moved, not rewritten (absolute,
  `..`, devices, colon, trailing dot or space, the parent chain of D-54), plus case-insensitive collisions on
  case-insensitive targets, symlink entries skipped with a warning unless `--symlinks`, and
  `--overwrite refuse|skip|rename|replace` (default `refuse`; `replace` never follows a link). Windows-only hooks (MoTW,
  privileges) are E6-5/E6-6. Acceptance: a table test per rule on both OSes; the E2-19 policy tests pass unchanged;
  `lpk x --overwrite skip` over a half-extracted tree completes and prints the skipped count.
- **E3-3 Progress, cancellation and console output.** (M) Engine: `Progress` events (stage, files, bytes in and out,
  blocks) through a callback and a `CancelToken` polled between files and blocks; a cancelled `a` or `append` cleans up
  like a failure (D-59's truncation), a cancelled `x` like D-54. CLI: a progress line on stderr only when it is a terminal
  (allow-listed crates), `--quiet`, `--no-progress`, Ctrl-C handled; VT colour only when the console enables it.
  Acceptance: tests cancel at every stage and assert no partial output remains; with stderr redirected no progress byte is
  written; archives byte-identical to the committed `small` hashes. Needed by E5-1.
- **E3-4 Several inputs, list files, selective extraction.** (M) `lpk a <archive> <path>...` (files and directories, each
  by its own name, duplicates refused), `--list <file>` (the runner's private-corpus `{list}` template, D-20 (6)),
  `--exclude <glob>`; `lpk x <archive> <outdir> [<pattern>...]` and `lpk l` with patterns, the subset fed to D-54's plan so
  only the blocks needed are decoded. Acceptance: tests for several roots, a list file and a glob subset; the catalogue row
  gains `create_private`; `lpk-bench run --tools lpk --private <dir>` succeeds on a temp tree in a test. Needed by E5-5,
  E6-2 and E8-4.
- **E3-5 Password prompting and secret hygiene.** (S) `--password-prompt` (and a terminal without a credential) reads
  without echo, with confirmation on `a`; the password lives in `Zeroizing` memory in `lpk-cli` and reaches
  `lpk-format::cli` through a `run_with_credentials` entry instead of an argument string (the E2-12 parked item); an
  unneeded `--keyfile` on a password-only archive is refused. Acceptance: the prompt refused on a pipe (test), accepted on
  a pseudo-terminal where the OS offers one; no test output or `-v` line ever contains the password (asserted).
- **E3-6 `lpk export <archive> <out.zip|out.7z>` (PLAN's `--export`).** (L) A streaming sink fed by the block-ordered
  engine: ZIP through the `zip` crate (Zip64, Deflate or zstd method 93 by `--method`, AES-256 with a credential, UTF-8
  names, mtimes); 7z through `sevenz-rust2` per decision E3 (d); writers in-process (they parse nothing untrusted).
  Acceptance: round trips through the crates' own readers and through external `7z t`/`7z x` and `unzip -t` when installed
  (skipped and recorded otherwise); an ignored test writes a file above 4 GiB as Zip64.
- **E3-7 Exit codes, error output and the generated reference.** (S) One exit code per failure class in a table test over
  every `CoreError` and `FormatError` variant (a partial extraction with per-entry errors is 2 with the list); `--json`
  errors on stderr; `docs/cli/lpk.md` and a man page generated with `clap-markdown`/`clap_mangen`, with a test that the
  committed text equals the generated one. Acceptance: the equality test and the exit-code table test pass on both OSes.
- **E3-8 `--strong` placeholder and the tier long form.** (S) `--strong` reserved and refused with a message (Phase 2);
  `--tier fast|balanced` as the form the GUI mirrors. Acceptance: `lpk a --strong` refused as decided; `--help` says so.
- **E3-9 The E3 gate re-run.** (M; measuring machine) The final CLI drives `lpk-bench run --tools lpk --profile full`, both
  tiers, one repeat, pooled with E2-22's incumbent baseline: every archive size equal to E2-22's `lpk` rows (E3 changed no
  archive byte), every extraction verified; directory and report committed. Acceptance: PLAN E3's gate cited to the
  committed directory.

### E4 — Sandboxed workers (8 tasks)
- **E4-1 `lpk-sandbox-sys`: restricted token and job object.** (M; Windows for the real sandbox) A `-sys` crate (the
  CLAUDE.md exception; every `unsafe` block with a `SAFETY:` comment, `clippy::undocumented_unsafe_blocks` denied) that
  starts a child with a restricted token (low or untrusted integrity, restricting SIDs) in a job object (one process,
  memory and CPU-time limits, no UI, kill-on-close, die-on-unhandled-exception; the Job Object code of `lpk-procstat-sys`
  reused) with only two pipe handles inherited; on Linux a plain child under `setrlimit`, documented as unsandboxed.
  Acceptance: tests on `windows-2025`: the child cannot open a file outside its allowance, is killed past the memory limit,
  cannot outlive the job; the crate is the only new `unsafe` in the tree.
- **E4-2 `lpk-worker` and the broker protocol.** (M) A separate binary; versioned length-prefixed framing over the pipes;
  requests `open`, `list`, `read-member` (streamed), `close`; the broker (`lpk_core::foreign`) owns every file write and
  applies E3-2's policy to member names; the worker receives the archive as an inherited read-only handle and never the
  destination; timeouts, a stderr capture cap, a protocol fuzz target. Acceptance: protocol round trips on both OSes; a
  worker that answers garbage, hangs or closes the pipe yields a named error within the timeout.
- **E4-3 Foreign readers, pure Rust, in the worker.** (L) `lpk-foreign` (`forbid(unsafe)`) behind one `ForeignReader`
  trait: ZIP (with ZipCrypto and AES), 7z, tar and tar.gz/bz2/xz/zst; listing with sizes, times, flags and encryption
  marks; credentials through the protocol; detection by magic in the broker. Acceptance: round trips with BLAKE3 equality
  of fixtures written by 7-Zip, Windows Explorer and GNU tar; `lpk x foo.zip out/` and `lpk l foo.7z` through the worker;
  the `archives-nested` class of `small` lists and extracts (ignored corpus test); `cargo deny` green.
- **E4-4 libarchive in an LPAC worker for RAR and ISO (only if decision E4 (a) says so).** (L; Windows) RAR4/RAR5 and
  ISO9660/UDF only inside an AppContainer (LPAC) worker; everything else stays pure Rust. Acceptance: the E4-1 tests pass
  under the AppContainer; committed RAR and ISO fixtures list and extract; `docs/LICENSING.md` and `cargo deny` green.
- **E4-5 Crash containment.** (M; Windows) A test-only worker "parser" that aborts, dereferences null, allocates past the
  job limit, spins past the CPU limit, spawns, opens the network and a file outside; the host reports a failed extraction
  naming the cause and continues. Acceptance: PLAN E4 gate, first clause: one test per case on `windows-2025`.
- **E4-6 Fuzz targets for every parser, run nightly.** (M; Linux) New targets: `jpeg-reconstruct`, `bwt` after revision
  1.3, the PNG/text peels if they ship, each `ForeignReader`, the worker protocol; `fuzz.yml` nightly with a persistent
  corpus. Acceptance: PLAN E4 gate, second clause: the nightly workflow on `main` with every target listed; the first week
  clean or each crash filed and fixed.
- **E4-7 The hostile-archive set.** (M) Generated fixtures (never downloaded): nested and overlapping-entry bombs, 4 GiB
  declared sizes over 1 KiB bodies, `..`/absolute/UNC/device/ADS names, tar links pointing outside, names differing only by
  case, a 1,000,000-member central directory. Acceptance: one test per fixture through `lpk x`, each refused or bounded with
  a named reason, time and memory asserted.
- **E4-8 Foreign formats in the harness and the security notes.** (S) Information rows: `lpk` extracting ZIP and 7z
  fixtures beside `7z x` (no claim); the worker model written for `docs/SECURITY.md`. Acceptance: one committed results
  directory; the section reviewed read-only.

### E5 — Windows GUI, Tauri 2 (11 tasks)
- **E5-1 The engine API for a front end (library API v0).** (M; needs E3-3, E3-4) `Session` (open, list in pages with
  totals, sort and filter in Rust, `info`), `extract(selection, policy, progress, cancel)`, `read_entry(entry, cap)`,
  `create`, `test`, `check`/`repair`, `history`/`rollback`, `append`; stable error enums with user-facing messages;
  `docs/API.md` v0 including the activation verbs of E5-7. Acceptance: `lpk l|x|a` moved onto the API with the CLI tests
  unchanged; a doc test per entry point; a million-entry archive pages within the envelope's memory.
- **E5-2 Tauri 2 skeleton.** (M; Windows to run) `crates/lpk-gui` and `apps/gui/` (HTML/CSS/vanilla JS, no framework, no
  bundler, `tauri-cli` from cargo); a strict CSP, no remote content; dark mode and DPI; CI builds on both OSes.
  Acceptance: `cargo tauri build` on `windows-2025` yields an exe that opens an archive from the command line; `cargo deny`
  green with the Tauri tree.
- **E5-3 Archive browser with the virtual list.** (L) Open by double-click, association, drag-drop or `--open`; tree and
  flat views; sort and filter in Rust; the webview holds the visible window plus a margin, fetched as binary batches;
  keyboard navigation, multi-selection, a status bar. Acceptance: the million-entry archive scrolls with the backend within
  the envelope and the DOM holding fewer than a few hundred rows.
- **E5-4 Extract wizard.** (M) Destination, all or selected, E3-2's overwrite modes, the password dialog, the envelope
  refusal shown with its figure and an override, progress with cancel, a per-entry error list. Acceptance: command-layer
  tests per path; dialogs on E5-10's checklist.
- **E5-5 Create wizard.** (L) Inputs (E3-4), the tier selector whose time estimates come from a calibration the app runs
  on this machine (never a constant in the source), password, listable, recovery percent, progress with cancel, a summary.
  Acceptance: command-layer tests; a created archive passes `lpk t`; a grep test that the estimate code holds no literal
  rate.
- **E5-6 Preview pane.** (M) Text, images through the webview's own decoders from a blob, hex for the rest; one capped
  `read_entry`; never HTML rendering of archive content. Acceptance: a capped entry refused with the cap named; a blob
  carrying a script does not execute.
- **E5-7 Activation protocol, single instance, settings.** (M) Verbs `--open`, `--extract-here`, `--extract-to`,
  `--create`, `--test`; response files; single instance; settings in `%APPDATA%\LitePack`; no telemetry; the update check
  opt-out. Acceptance: a second instance's verbs reach the first; the verbs match `docs/API.md`. Needed by E6-2.
- **E5-8 Maintenance dialogs: test, check and repair, history and rollback, append.** (M) Acceptance: command-layer tests
  on the vectors.
- **E5-9 Accessibility, keyboard and strings.** (S) Acceptance: an automated accessibility pass with no critical issue.
- **E5-10 The E5 gate on four configurations.** (M; human, Windows) A checklist run on Windows 10 22H2 x64, Windows 11 x64
  and Windows 11 ARM64 per decision E5 (b). Acceptance: PLAN E5's gate.
- **E5-11 GUI smoke test on CI.** (M; Windows) `tauri-driver` with Edge WebDriver on `windows-2025`. Acceptance: the job
  green on `main` (or the command-layer fallback recorded as a decision).

### E6 — Explorer integration (7 tasks)
- **E6-1 Design and the unsafe exception.** (S; needs E5-1's verb text) A COM cdylib that never loads the webview or the
  engine and only launches `lpk-gui.exe` with E5-7's verbs; x64 and ARM64 builds. Acceptance: the design note committed;
  the crate builds for both targets in CI.
- **E6-2 `IExplorerCommand` server.** (L; Windows) Submenu, title/icon/state per selection, `Invoke` launching the GUI;
  registered through the package manifest, out of process. Acceptance: in-process tests enumerate the verbs and check their
  state per selection kind; the manifest fragment validated in E7-4.
- **E6-3 Sparse package for the NSIS install.** (M; Windows; needs E7-3's certificate) Acceptance: the package present after
  install and absent after uninstall on a Windows 11 VM; the modern menu shows the verbs.
- **E6-4 Classic `IContextMenu`, associations, Default Programs.** (M; Windows) Per-user registration; the `.lpk` ProgID;
  "Open with" for ZIP/7z/tar per decision E6 (c). Acceptance: register and unregister leave the registry as found.
- **E6-5 Mark-of-the-Web propagation.** (M; Windows) Zoned archives give every extracted file the zone; measured on
  `small-files`. Acceptance: tests on `windows-2025`; the measured row committed.
- **E6-6 Extraction policy, the Windows part.** (M; Windows) Symlinks, junction and reparse-point parent checks, the
  parent-handle rule per decision E6 (e), long paths, NTFS case collisions. Acceptance: PLAN E6 gate, second clause, with a
  TOCTOU test for the chosen rule.
- **E6-7 The E6 gate evidence.** (S; human, Windows) Acceptance: PLAN E6 gate, first clause.

### E7 — Packaging, signing, updates (10 tasks)
- **E7-1 Accounts, identity, lead times.** (S; human; start first) The signing provider and account, the Store account, the
  publisher name and package identity (fixed before E6-3/E7-4), the export-control note, the minisign key pair.
  Acceptance: `docs/RELEASING.md` "Accounts" lists each with its status; a test file signs and `signtool verify` output is
  committed (no certificate material).
- **E7-2 `release.yml`.** (M; Windows runners) Tag builds for x64 and ARM64, `cargo auditable`, CLI zip, NSIS, MSIX,
  sparse package, `SHA256SUMS`, SBOM, a draft release. Acceptance: a test tag runs green with every artefact.
- **E7-3 Signing with timestamps.** (M; Windows; needs E7-1) Acceptance: every artefact of a test release verifies and the
  publisher equals the manifest's.
- **E7-4 MSIX.** (M; Windows) Acceptance: the bundle installs and uninstalls on `windows-2025`; WACK passes or each failure
  is listed with its reason.
- **E7-5 NSIS installer through Tauri.** (M; Windows) Per-user by default, silent install, upgrade, a clean uninstall, the
  notices installed. Acceptance: a scripted install/upgrade/uninstall with a residue check, green.
- **E7-6 Portable zip.** (S) Acceptance: run from a temp folder leaves nothing under `%APPDATA%`.
- **E7-7 Updater.** (M) `tauri-plugin-updater` with minisign; `latest.json` on GitHub Releases; disabled under MSIX; a
  tampered manifest refused. Acceptance: build N updates to N+1 and refuses a bad signature.
- **E7-8 Third-party notices generated.** (S) `cargo about`; a CI check when `Cargo.lock` changes. Acceptance: the check
  green; the file shipped.
- **E7-9 winget and the Store.** (S; human) Acceptance: `winget install` installs the signed release; the Store listing
  live (or decision E7 (b) limits the channels).
- **E7-10 The E7 gate on fresh VMs.** (M; human, Windows) Acceptance: PLAN E7's gate.

### E8 — QA and release engineering (9 tasks)
- **E8-1 Nightly fuzzing with a corpus.** (S; Linux; with E4-6) Acceptance: thirty consecutive nightly runs with zero open
  crashes.
- **E8-2 Size regression on every PR.** (M) A deterministic generated fixture set, golden sizes per tier and class, the job
  failing when any archive grows by more than 1%. Acceptance: a deliberate regression in a test PR is caught.
- **E8-3 Speed regression on the measuring machine.** (M; measuring machine) A detached weekly run with `run --compare`.
  Acceptance: two consecutive weekly directories with the compare output cited.
- **E8-4 The 10,000-file real-world round-trip.** (M; owner's machine; needs E3-4) Acceptance: a committed results
  directory per release with `verified n/n` on every row.
- **E8-5 Reproducible builds.** (L; Windows) Pinned toolchain, path remapping, `/Brepro`; two CI builds compared.
  Acceptance: the compare job green on `main`.
- **E8-6 SBOM and attestations.** (S) Acceptance: `gh attestation verify` succeeds on a test release's `lpk.exe`.
- **E8-7 Memory-safety evidence.** (S) An inventory of crates allowed `unsafe`; CI fails elsewhere. Acceptance: the Phase 1
  criterion stated per release with its evidence.
- **E8-8 Release procedure and versioning.** (S) Acceptance: a dry run yields a tagged pre-release build.
- **E8-9 Corpus smoke with `lpk`, and ARM64 CI.** (S) Acceptance: both green on `main`.

### E9 — Docs and site (8 tasks)
- **E9-1 Decisions and skeleton.** (S; human for the domain) Acceptance: the site builds in CI and serves over HTTPS.
- **E9-2 Landing and download page.** (S) Acceptance: every number on the page appears in the `bench/reports` file it links.
- **E9-3 The format specification published.** (M; after revision 1.3 is final) Acceptance: the pages render from the
  committed files.
- **E9-4 CLI reference.** (S; needs E3-7) Acceptance: E3-7's equality test guards it.
- **E9-5 User guide.** (M; needs E5) Acceptance: every GUI dialog has a page its help link reaches.
- **E9-6 Benchmark and corpus pages generated.** (M) Acceptance: produced by one command from committed files.
- **E9-7 Security and privacy pages, `SECURITY.md`.** (S) Acceptance: `SECURITY.md` at the root; the privacy URL used by
  E7-9.
- **E9-8 Licensing and notices pages; docs CI.** (S) Acceptance: the link check green; the notices page equals the shipped
  file.

## 4. Order, parallelism, sizes
- **Dependencies across epics:** E5-1 needs E3-3 and E3-4; E5 proper needs E5-1; E6 needs E5-1's verb text and E5-7; E6-3
  and E7-3..E7-5 need E7-1's certificate; E6-7 and E7-10 need the signed build; E8-4 needs E3-4; E8-1 extends E4-6; E9-3
  needs revision 1.3; E9-4 needs E3-7; E9-5 needs E5-10's screenshots; E3-9 needs E2-22's baseline. E4 depends on nothing
  but its decision.
- **Wave 1, beside E2's remaining tasks:** E7-1 (human, longest lead time — first); E3-1 to E3-8 (one implementer,
  sequential); E8-1, E8-2, E8-6, E8-7, E8-9; E4-1, E4-2 after decision E4 (a)/(b); E9-1, E9-2.
- **Wave 2:** E5-1; E4-3, E4-5, E4-6, E4-7; E8-5; E9-3 once 1.3 is final; E7-2, E7-8.
- **Wave 3:** E5-2 to E5-9 in parallel with E6-1, E6-2, E6-4, E6-6; E4-4 and E4-8 if chosen; E8-8.
- **Wave 4, with the certificate:** E7-3 to E7-7; E6-3, E6-5; then the gates E5-10, E5-11, E6-7, E7-9, E7-10, E8-3, E8-4,
  E3-9; E9-4 to E9-8.
- **Rough size:** 62 tasks — about 9 L, 29 M, 24 S. E5 and E6 are the critical path; E7-1's lead time is the schedule risk,
  so it starts before any E7 code.
