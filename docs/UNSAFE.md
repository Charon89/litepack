# Where LitePack uses `unsafe` (E8-7)

Every crate forbids `unsafe` (`CLAUDE.md`: `#![forbid(unsafe_code)]` everywhere except `*-sys` and `*-simd` crates),
through the workspace lint `unsafe_code = "forbid"` that both workspaces set (`Cargo.toml`, `workers/Cargo.toml`) and
`fuzz/Cargo.toml` sets for itself. The crates below are the only exceptions, and inside each the `unsafe` code is
confined to the modules its row lists. `scripts/check-unsafe.sh` enforces this on CI (both OSes, the `check` job of
`ci.yml`): it fails when a crate outside this table does not forbid `unsafe_code`, relaxes it (`allow`, `warn`,
`expect`), or holds an `unsafe` keyword (block, `fn`, `impl`, `trait`, `extern`, attribute) anywhere in its sources,
tests included; when an inventoried crate holds `unsafe` outside its listed modules; when a reserved crate appears
before its row is filled in; and when a row names a crate that no longer exists. Its `--self-test` plants each case
in a fixture tree.

The table between the markers is read by the scripts: one row per crate, the crate name, its directory and the state
in backticks (`built` or `reserved`), the modules as backticked paths relative to the crate's directory. A new
`-sys` or `-simd` crate, or a new module that needs `unsafe`, is added here in the same change, with the reason.

<!-- inventory:begin -->
| Crate | Directory | State | Modules allowed `unsafe` | Why `unsafe` is needed |
|---|---|---|---|---|
| `lpk-procstat-sys` | `crates/lpk-procstat-sys` | `built` | `src/windows_impl.rs`, `src/unix.rs`, `src/alloc.rs` | Measures a child process tree for the benchmark runner: Job Objects, process creation and accounting queries through `windows` (Win32 FFI); `waitid`/`wait4` and process-group signals through `libc`; a counting `GlobalAlloc` for the in-process memory tests (`unsafe impl GlobalAlloc`). Not shipped in `lpk` (lpk-bench and tests only). |
| `lpk-sandbox-sys` | `crates/lpk-sandbox-sys` | `built` | `src/windows_impl.rs`, `src/job.rs`, `src/child.rs`, `src/unix_impl.rs`, `src/bin/lpk-sandbox-testworker.rs`, `tests/windows.rs` | Starts the foreign-format worker sandboxed (D-97): LPAC AppContainer profiles, restricted tokens, security descriptors, the job object, a suspended process with an explicit handle list and the bootstrap page, all Win32 FFI; the worker's side takes its handles from that page (`child.rs`); `setrlimit` before `exec` on Linux. The test worker and `tests/windows.rs` call Win32 directly to probe the sandbox from inside and outside (test-only, not shipped). Every block carries a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks` denied). |
| `lpk-libarchive-sys` | `crates/lpk-libarchive-sys` | `built` | `src/real.rs`, `src/ffi.rs` | Binds libarchive 3.8.9's read API (vendored C, E4-4) for the sandboxed `lpk-worker-libarchive` only: the `extern "C"` declarations (`ffi.rs`) and the safe wrapper with its read/seek/skip/switch callbacks (`real.rs`). Every block carries a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks` denied). While `vendor/libarchive-3.8.9/` is absent the crate builds a stub with no `unsafe`. |
| `lpk-shell` | `crates/lpk-shell` | `reserved` | the COM boundary module (named by E6-1) | Explorer context-menu handler (E6-1): the COM interfaces of a windows-rs cdylib. Only the COM boundary module may hold `unsafe`; the rest of the crate forbids it. |
| `lpk-gui-sys` | `crates/lpk-gui-sys` | `reserved` | named by its E6 task | Shell registration (E6). |
| `lpk-fs-sys` | `crates/lpk-fs-sys` | `reserved` | named by its E6 task | Extraction through directory handles, `OBJ_DONT_REPARSE`, reparse tags and file IDs (E6-5/E6-6). |
<!-- inventory:end -->

There is no `*-simd` crate today. `vendor/` (patched third-party crates, D-66) is not ours and not checked; the
`unsafe` in dependencies is the dependencies' (`cargo deny check` and the advisory database cover them).

## The release statement

Each release states the Phase 1 criterion — zero memory-safety findings — with its evidence, generated rather than
typed: `scripts/memory-safety-statement.sh [--since <previous-tag>]` prints, as Markdown, this inventory, the runs of
the fuzz workflow (`fuzz.yml`) on `main` since the previous release with their conclusions (`gh run list`), and the
count of open issues labelled `memory-safety` (every fuzz crash and every memory-safety report is filed under that
label). It needs `gh` authenticated for this repository. E7-2's release workflow runs it and puts its output in the
draft release; `docs/RELEASING.md` (E8-8) will name the step. Without `--since` it lists every run.
