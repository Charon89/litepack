# LitePack — project instructions for Claude Code

LitePack is an open-core, Windows-first archiver whose `.lpk` format beats 7-Zip/WinRAR/WinZip on
real user data by (1) bit-exact recompression of already-compressed files (JPEG; Deflate inside
PDF/Office/PNG/APK), (2) dedup + delta across files, and (3) a modern container (recovery records,
AEAD encryption, Merkle integrity, seekable, journaling). Fast tier extracts 5–10× faster than 7-Zip.
No neural tier in the product (research track only). Full rationale: `docs/LitePack-Method-v2.md`.

## Where things are
- `docs/PLAN.md` — the ordered task list with acceptance criteria. Work through it top to bottom.
- `docs/DECISIONS.md` — decisions already made (do not re-litigate; append new ones).
- `docs/CORPUS.md` — benchmark corpus specification (Phase 0).
- `docs/LICENSING.md` — licence allow-list and freedom-to-operate items.
- `docs/LitePack-Method-v2.md` — research, competitor analysis, architecture, stack.
- `crates/` — Cargo workspace. Phase 0 lives in `crates/lpk-bench`.

## Stack (decided)
- Rust stable, MSVC targets (`x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`), Linux for CI/fuzzing.
- `#![forbid(unsafe_code)]` in every crate except `*-sys` crates and crates named `*-simd`.
- GUI later: Tauri 2 (HTML/CSS/vanilla JS). Shell extension later: windows-rs cdylib. Not in Phase 0.
- Licences: engine/CLI/format are Apache-2.0 OR MIT. Dependencies must pass `cargo deny check`
  (allow-list in `deny.toml`). Never copy or port code from GPL/LGPL projects (paq8px, cmix, Hutter
  entries, packMP3, bzip3). LGPL crates may be *depended on* (we are open source) but not vendored.
- Never use `jpegxl-rs` (GPL-3). JPEG recompression = `lepton_jpeg`. Deflate = `preflate-rs`.

## Commands
- Build/test: `cargo build --workspace` · `cargo nextest run --workspace` (fallback `cargo test`)
- Lint: `cargo fmt --all` · `cargo clippy --workspace --all-targets -- -D warnings`
- Licences: `cargo deny check`
- Bench (Phase 0): `cargo run -p lpk-bench -- corpus build --profile small` · `cargo run -p lpk-bench -- run --tools all` · `cargo run -p lpk-bench -- report`
- All four of build, test, clippy and deny must pass before a task is marked done.

## How to work
- One PLAN task at a time. Read the task's acceptance criteria first; implement; prove each criterion
  with a command whose output you cite in the commit message or task note.
- Commit per task (Conventional Commits: `feat(bench): ...`, `fix:`, `docs:`, `chore:`). Small commits.
- When a task is done: tick its checkbox in `docs/PLAN.md`, add a one-line note with the evidence.
- New decisions (library choice, format detail, measured gate result) go in `docs/DECISIONS.md` as
  `D-NN — date — decision — reason`. Never silently change a decided item.
- Numbers: never write a compression/speed figure anywhere unless a harness run produced it and the
  JSON result file is committed under `bench/results/`.
- Prefer `Grep`/`Glob` over reading whole files; do not read `bench/results/*.json` into context —
  summarise them with the report command.
- Return summaries, not file dumps, from subagents. Keep the main session's context small.
- Ask before: adding a dependency with a non-allow-listed licence, changing the corpus spec,
  changing a gate threshold, or touching anything under `docs/LitePack-Method-v2.md`.

## Working across machines (D-13)
- GitHub (`origin`) is the only shared state. Start a session with `git pull --ff-only`; push `main` when a
  task is done and reviewed; if you stop mid-task, push the task branch and leave the task `[~]` in PLAN.md.
- New machine: clone, then `scripts/setup-windows.ps1` (idempotent; re-run it when local and CI disagree).
- Durable project knowledge goes in this file or `docs/`, never only in Claude's per-machine memory.
- Nothing machine-specific in git: no absolute paths; `bench/corpus/` is rebuilt from `corpus.lock`;
  each machine's measurements stay under their own `bench/results/<date>-<host>/`.

## Windows notes
- Use PowerShell-safe paths; prefer `std::path` over string concatenation. Long paths: Rust std
  handles `\\?\`; tests must not assume paths < 260 chars.
- External tools for benchmarks are discovered via PATH or `bench/tools.toml`; never hard-code
  `C:\Program Files\...`. If a tool is missing, skip it and record `"skipped": "not installed"`.
- Peak memory on Windows is measured via a Job Object (`windows` crate); on Linux via `/usr/bin/time -v`
  or `getrusage`.

## Definition of done (every task)
1. Acceptance criteria met and evidenced. 2. `fmt`, `clippy -D warnings`, `nextest`, `deny` green.
3. No new `unsafe`. 4. PLAN.md checkbox ticked with evidence. 5. Committed.
