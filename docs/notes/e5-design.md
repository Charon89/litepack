# E5 design: the Windows GUI (Tauri 2 over `lpk-api`)

2026-10-25; for E5-1 to E5-11 (`docs/PLAN.md:185–205`), on `origin/main` ea84414. Numbers are design defaults,
never measurements (D-07); each choice becomes the `D-NN` named in §8. "UX §n" cites the competitor study of
2026-10-25 (not committed).

## 1. Processes, crates, threads (E5-1, E5-2)

- `crates/lpk-api` (new; Apache-2.0 OR MIT, `forbid(unsafe)`): `Session` and every front-end operation;
  `docs/API.md` v0.
- `crates/lpk-gui` (new): the Tauri host — a plain-Rust command layer testable without a webview, thin adapters.
  Tauri is a Windows-only dependency; on Linux the crate builds the command layer and a stub `main`.
- `crates/lpk-gui-sys` (new, Windows; the `-sys` exception with E4-1's lint table): the password dialog, the
  no-replace rename, file IDs.
- `apps/gui/`: `index.html`, CSS, vanilla ES modules, `strings/en.json`; no `package.json`, no bundler.

`lpk-gui.exe` links the engine in-process (PLAN E5 (a), `docs/PLAN.md:187–188`) and brokers foreign archives, one
sandboxed worker each (E4 §1, §6).

**Threads.** A non-`async` command would run on the main thread, tao's event loop [T-CALL], so every command is
`async` and passes engine work, with E3-3's `Monitor` (`crates/lpk-core/src/progress.rs:155`), to `spawn_blocking`;
a queue runs one heavy operation at a time, as each may take half the memory (D-67 (1)).

**Exit.** Closing or quitting mid-operation asks, then cancels every token and joins the runs (`ExitRequested` can
wait [T-RUN]), so D-87 (2)'s cleanup completes. At log-off tao ignores `WM_QUERYENDSESSION`; on `WM_ENDSESSION` it
ends its loop (Tauri's `RunEvent::Exit`), then exits [TAO-EL]; the session may end once that message
returns [WM-ES], so the `Exit` handler cancels and joins first.

**Interrupted writes.** A GUI `create` writes `<name>.lpk.partial` and publishes it with `MoveFileExW` without
`MOVEFILE_REPLACE_EXISTING` [MFX] (`std::fs::rename` would replace an existing file [RSRN]). Partial files, with
their identity (volume serial and file ID [FIDI]), and running appends are recorded under `%LOCALAPPDATA%\LitePack`;
the next start deletes a leftover partial, or rolls an interrupted `append` back to its last complete generation
(D-59; `crates/lpk-format/src/archive.rs:997–1015`), only if the identity matches; a test covers both. Residual:
without `WM_QUERYENDSESSION`, a forced log-off can still kill a run mid-write, for the next start to recover.

**Closed build.** No copyleft in the engine (D-96 (5)): a closed GUI later moves `lpk-gui` and `apps/gui/` to a
private repository over a published `lpk-api`; a manifest test keeps the seam (no `lpk-*` dependency but `lpk-api`
and `lpk-gui-sys`, no `lpk_core::` or `lpk_format::` path). Rejected: `lpk_core::session` (21 exported modules,
`crates/lpk-core/src/lib.rs:14–71`), a C ABI `cdylib` (`unsafe` FFI for nothing), a GUI workspace (two lockfiles,
two engines).

## 2. The engine API v0 (E5-1)

```rust
pub enum Selection { All, Globs(lpk_core::Selection), Ids(Arc<IdSet>) }
pub enum Destination { Dir(PathBuf), Smart { beside: PathBuf }, NewFolder { beside: PathBuf } }
impl Session {                                                         // Session: Send + Sync
    pub fn open(path: &Path, o: &OpenOptions) -> Result<Session, Error>;
    pub fn unlock(&mut self, c: Credentials) -> Result<(), Error>;
    pub fn info(&self) -> &Info;
    pub fn for_each(&self, s: &Selection, f: &mut dyn FnMut(&EntryRef<'_>) -> ControlFlow<()>)
        -> Result<(), Error>;
    pub fn model(&self, c: &CancelToken) -> Result<Arc<ListModel>, Error>;
    pub fn read_entry(&self, id: u32, cap: u64, c: &CancelToken) -> Result<Vec<u8>, Error>;
    pub fn preflight(&self, s: &Selection, to: &Destination, c: &CancelToken) -> Result<Preflight, Error>;
    pub fn extract(&self, s: &Selection, to: &Destination, o: &ExtractOptions, m: &Monitor)
        -> Result<ExtractReport, Error>;
    pub fn export(&self, s: &Selection, out: &Path, o: &ExportOptions, m: &Monitor)
        -> Result<ExportSummary, Error>;
    pub fn test(&self, m: &Monitor) -> Result<TestReport, Error>;
    pub fn check(&self, m: &Monitor) -> Result<RecoveryReport, Error>;    // .lpk only, as the next four
    pub fn repair(&self, out: &Path, m: &Monitor) -> Result<RecoveryReport, Error>;
    pub fn history(&self, c: &CancelToken) -> Result<Vec<GenerationInfo>, Error>;
    pub fn rollback(self, generation: u64) -> Result<GenerationInfo, Error>;
    pub fn append(self, dir: &Path, s: &CreateSettings, m: &Monitor) -> Result<(Session, RunSummary), Error>;
    pub fn convert(&self, out: &Path, s: &CreateSettings, m: &Monitor) -> Result<RunSummary, Error>;
}
pub fn create(inputs: &Sources, out: &Path, s: &CreateSettings, m: &Monitor) -> Result<RunSummary, Error>;
pub fn calibrate(inputs: &Sources, s: &CreateSettings, sample: u64, m: &Monitor) -> Result<Rate, Error>;
impl ListModel {                                    // also rows(&View, first, n) and totals(&View, picks)
    pub fn view(&self, scope: Scope, order: Order, filter: &Filter, c: &CancelToken) -> Result<View, Error>;
}
#[non_exhaustive] pub enum Class { Usage, NotFound, AccessDenied, DiskFull, PasswordRequired,
    WrongPassword, Damaged, Unsupported, TooLarge, Unsafe, Exists, Cancelled, Sandbox, Internal }
```

`OpenOptions`: credentials, memory, priors, generation, E4's limits; `CreateSettings`: `TierArgs`'s fields
(`crates/lpk-cli/src/lib.rs:489–570`), seal, recovery; `Tier` cannot name Strong.

- **Lazy model.** `for_each` streams as `lpk l` does (`crates/lpk-cli/src/list.rs:13–14`), keeping E3-1's bound
  (`crates/lpk-cli/tests/list_memory.rs:1–7`). `model` builds a name arena, a `Row` per entry, `u32` vectors for
  order, filter and children, a selection bit set; a re-sort builds its vector beside the serving one, the peak the
  counting-allocator test states paging and sorting E3-1's million-entry archive (`list_memory.rs:30–52`):
  `open + N·(size_of::<Row>() + 4·size_of::<u32>()) + P + ⌈N/8⌉ + 1 MiB`, P the name bytes.
- **Sharing.** Reads fork a reader (`Archive::fork`, `crates/lpk-format/src/archive.rs:730`) under a shared guard,
  registering their token; `history` locks the archive (`&mut`, `:682`); `rollback(self)` and `append(self)` cancel
  those tokens and await the guards. A superseded preview is cancelled: `read_entry` streams through E3-6's engine
  into a capped `Sink` (`crates/lpk-core/src/export.rs:78–86, 665`), polling per block.
- **`Ids`**: picked names would need rewriting as globs (D-90 (3): no escape); a million picks are not patterns.
- **Hooks.** `verify`, `check_recovery` and `repair_with_credentials` have none (`crates/lpk-format/src/reader.rs:541`,
  `recovery.rs:600, 645`): E5-1 adds per-block hooks (old functions call them with a no-op) for progress and cancel.
  `lpk t` stays on the reference tool (`crates/lpk-cli/src/lib.rs:1672–1687`).
- **Generations.** The reader finds the trailer at `seek(End(0))` (`archive.rs:425`): a view of the file's first
  `g.end()` bytes opens generation g read-only, as `rollback` would leave it (`:997–1015`).
- **`calibrate`** runs the pipeline in memory on a bounded sample: E5-5's only source of estimates.
- **Errors (D-77).** D-77's `kind` (`crates/lpk-cli/src/failure.rs:6–8`), a `Class` keying messages, the path,
  entry errors and `Refusal` figures (`crates/lpk-format/src/envelope.rs:88–95`); the classification moves into
  `lpk-api` behind `failure.rs`; `Class::Usage` is exit 1.
- **CLI.** `l` → `open` + `for_each` (`list_archive` keeps its signature); `x` → `open` + `extract`
  (`lib.rs:1349–1370`); `a` → `create`, `build_pipeline` (`:910–976`) moving into `CreateSettings`;
  `crates/lpk-cli/tests` untouched. **Foreign** (E4 §6): `Unsupported` until E4-2/E4-3.

## 3. IPC (E5-3 and every command)

Commands mirror the API, all `async`, long ones taking an op id and a `Channel<Progress>`; plus `view` (scope,
order, filter), `rows`, `select`, `pick` (→ grant), `cancel`, `settings` (no path field) and `subscribe`
(second-instance verbs, drops).

**Batch v1** (little-endian): a 28-byte header (magic, version, generation, total, first row, count, names length);
32 bytes per row (id u32, kind u8, marks u8, flags u16, size u64, mtime i64, name offset and length u32); UTF-8
names; an `ArrayBuffer` through `tauri::ipc::Response` [T-CALL]. Size: `visible + 2·MARGIN_ROWS` rows, at most
4,096, cut at 1 MiB (`docs/notes/e4-design.md:78`); stale generations dropped.

**Sort and filter** build a new permutation and generation in Rust; the page keeps its offset (or follows the
focus) and refetches one window; selection and totals stay in Rust; filters are substrings or D-90 globs.

**Progress** comes one event per 200 ms per stage plus first and last (D-87 (1), `progress.rs:199`), through a
`Channel` per operation, ordered and fast where events are JSON evaluated as script [T-FRONT]; `cancel` sets the
token and D-87 (2) cleans up.

**Bounds.** Backend: E5-1's bound plus a batch. DOM: at most `visible + 2·MARGIN_ROWS` recycled rows (`MARGIN_ROWS`
= 64); the spacer capped at `MAX_SCROLL_PX`, scaled beyond for E4's 10-million cap (`docs/notes/e4-design.md:92`);
`aria-rowcount`/`aria-rowindex` give the true size [ARIA].

## 4. Security (E5-2, E5-3, E5-6, E5-7)

**CSP** (Tauri adds hashes and nonces [T-CSP]): `default-src 'none'; script-src 'self'; style-src 'self'; img-src
'self' blob:; font-src 'self'; connect-src ipc: http://ipc.localhost; frame-src 'none'; object-src 'none'; base-uri
'none'; form-action 'none'; require-trusted-types-for 'script'; trusted-types 'none'`; no remote origin [T-CSP].
Trusted Types make strings reaching `innerHTML` throw [TT] (dropped by a `D-NN` if Tauri's scripts break). A grep
test bars HTML sinks, `eval`, inline handlers and URLs from `apps/gui/`.

**Capabilities.** `AppManifest::commands` makes each command a permission, else every window may call
all [T-CAP]. One file per window kind lists its commands, no plugin permission (dialogs and the update check run
in Rust), core ones added singly; a test parses the files.

**Grants.** No command takes a raw path. Rust mints opaque grants from native dialogs [T-DLG], drops (delivered to
Rust [T-WVB]), verbs and recents, and derives "extract here" from the archive's grant. `settings` has no path field:
Rust alone records recent archives (user-chosen, from dialogs, verbs or drops) and recent destinations (native
dialogs only); a test's path through `settings` is rejected. `rollback` confirms in a native box [T-DLG]. Why:
image-decoder exploits reach renderers (CVE-2023-4863 [CVE]).

**Passwords** go into `lpk-gui-sys`'s dialog (an `ES_PASSWORD` edit [ES-PW], twice to create), run on the main
thread and owned by the GUI window, straight into `Zeroizing` memory, never the web process; the page learns only
the outcome. Dark mode: `DWMWA_USE_IMMERSIVE_DARK_MODE` for the title bar (documented from Windows
11 build 22000 [DWMA]; E5-10 checks 22H2) and `WM_CTLCOLOR*` for the controls [CTLC]. Rejected: `TaskDialogIndirect`
(no text field [TDC]), CredUI (a user name, no confirmation, a sign-in look [CRED]), a second webview.

**No isolation pattern.** It guards against compromised frontend dependencies, mostly build-time [T-ISO]; we have
none, it shares the renderer it would guard, and its docs are silent on binary payloads [T-ISO].

**Preview (E5-6).** One `read_entry(entry, PREVIEW_CAP)` (16 MiB), larger entries refused naming the cap; text by
`textContent`, else hex; images sniffed by Rust (PNG, JPEG, GIF, WebP, BMP, ICO; no SVG) into a typed `Blob` for
`img.src`; a script blob leaves a test sentinel unset. Hidden and bidirectional characters show escaped [TR36].

**Opening an entry (E5-3).** A double-click extracts that entry alone (no sibling or same-named folder) under
E3-2's policy, which refuses a trailing dot or space (`crates/lpk-core/src/policy.rs:356–357`), into a fresh folder
under `%LOCALAPPDATA%\LitePack\Open\`, zoned like the archive (Internet for a zoneless foreign one). The shell opens
only allow-listed types (document, image, media, text) by the real last extension, the rest goes to the preview:
`doc.pdf .cmd` (CVE-2023-38831's class [WRAR]) counts as `.cmd`. Cleaned at exit; no write-back.

**Network.** One request: the update check (`tauri-plugin-updater` in Rust; signatures mandatory, TLS [T-UPD]),
on by default ("opt-out", `docs/notes/phase1-e3-e9-breakdown.md:56–57`), switchable off. Its User-Agent is the
plugin's name and version, our endpoint uses none of its URL variables [UPD-SRC], and a test at a local server
finds no user or host name. No telemetry, crash reports or web storage.

## 5. Windows integration (E5-2, E5-7, E5-9)

**WebView2.** Windows 11 ships the Evergreen runtime; most Windows 10 devices have it, some do not, and Edge cannot
stand in [WV-DIST]. NSIS uses `downloadBootstrapper` (decision (f)), the Store package `offlineInstaller` [T-WIN];
the portable zip checks the `pv` value [WV-DIST] and names Microsoft's download page.

**DPI, theme.** tao sets per-monitor-v2 awareness [TAO-DPI]; the theme follows the system unless set [T-CFG], CSS
`prefers-color-scheme` too, `forced-colors` for high contrast [MDN-FC] (UX §7).

**Activation (E5-7).** `tauri-plugin-single-instance`, registered first, passes a second instance's arguments and
directory to the first [T-SI]; `@file` response files beat the 32,767-character command line [CPW]. E6's menus send
these verbs (UX synthesis 4); a test checks the parser against `docs/API.md`'s table: `--open` or a bare path (Open,
double-click); `--extract-here <archive>...` (Extract here: smart, §6); `--extract-to <archive>...` (`Extract to
"<name>\"`: `<name>\` beside each, `<name> (k)` if taken); `--create <path>...` (Add to archive…); `--create …
--name <file>.lpk` (`Add to "<name>.lpk"`: at once); `--test <archive>...` (Test); `--convert <archive>` (Convert
to…, then E4 §6).

**Settings.** `%APPDATA%\LitePack\settings.json` (`config_dir`, not `app_config_dir` [T-PATH]), versioned, written
temp-then-rename, beside the executable when portable; WebView2's data folder [T-WVB] is
`%LOCALAPPDATA%\LitePack\WebView2`.

**Accessibility (E5-9).** F6 between panes, a key for every pointer action, one tab stop per composite, visible
focus [MS-KB]; named controls and an ARIA `treegrid` list [ARIA]. Checker: **axe-core**
(MPL-2.0 [AXE], allowed unmodified, `docs/LICENSING.md:12`; test-only, pinned by SHA-256, injected over WebDriver
into each view and dialog): a `critical` impact fails [AXE-API].

**Strings** live in `apps/gui/strings/en.json`, for the page and the native dialogs; a test fails on missing or
unused keys and on literal text in markup or scripts.

## 6. Visual and interaction direction (E5-3 to E5-8)

**Main window.** Command bar, breadcrumb, filter; tree, list (the flat view adds Path), preview; a status bar with
selection and protection (revision, generation, names encrypted, recovery); a drop zone when empty.

**Extract (E5-4)**, one dialog. Destination: Smart (default), new folder, this folder, Choose…. Smart puts a lone
top-level item whose name is free beside the archive, anything else in a fresh `<name>\`, touching nothing existing
[UX §1–3, synthesis 3]; several archives queue in one window. Refuse-first: `preflight` counts existing targets and
free space before any write (D-54 (4)) and asks once: Skip, Keep both, Replace, Cancel. An envelope refusal reads
"needs X; this PC allows Y", offering "Allow X". Progress: files, bytes, MB/s, ETA, Cancel. Result: entry errors,
skipped symlinks and the check — `.lpk`: "every file matched its BLAKE3 hash", true by construction
(`crates/lpk-core/src/extract.rs:29–31`); ZIP and 7z: each member's check from E4 (`MemberEnd { checked }`,
`docs/notes/e4-design.md:83`); tar: "not verifiable".

**Create (E5-5).** Inputs, destination, tier cards (Fast, Balanced, Store) timed by `calibrate`; a password (native
dialog) with "Encrypt file names" on, the non-listable default [UX synthesis 5]; recovery 5% proposed (CLI 0,
D-58); "Test when done"; a summary from the run's counters.

**Maintenance (E5-8).** Test with cancel; a failure names block and entry and offers Repair, which writes a copy
(D-58 (1)). History: browse, restore, or roll back after a native confirm. Update from folder warns the generation
mirrors the folder (`crates/lpk-cli/src/lib.rs:138–141`); dropped files never append.

**Density.** Newcomers get one primary action, smart defaults and folded options; power users the flat view, globs,
keyboard, engine counters and "Copy as command" (no password) [UX §1–3].

**Better than the field:** verification shown [UX synthesis 8]; nothing half-done; explained refusals; Mark of the
Web by default via E6-5 [UX §5: off in 7-Zip]; usable history; smooth million-entry archives; no ads, nag or
telemetry [UX synthesis 7].

**Open from the study:** competitors' layouts, search, prompts, pause, verification, defaults, WinZip, screen
readers, Windows 11's own handling; E5-10 compares tasks in 7-Zip and WinRAR.

## 7. Testing

- **`lpk-api`, both OSes:** a doc test per entry point; the model bound in its own binary; generation views against
  `rollback` on `journal-3gen.lpk`.
- **Command layer, both OSes:** E5-4's destinations × selection × `Overwrite` × password × envelope override ×
  cancel × entry errors; E5-5's archive through `lpk t`, the no-rate grep, recovery 5 and 0; E5-6's refusal; E5-8
  on the vectors; E5-7's verbs, capabilities, a `settings` path, the update request, silence when off; exit and
  next-start recovery; opening `doc.pdf .cmd`, `doc.pdf ` and a same-named folder; E5-3's paging; after E6-5, a
  zoned-archive test per crafted class of E4-7 (UX synthesis 1).
- **E5-11, `windows-2025`:** `tauri-driver` [T-WD] with the Edge WebDriver of the runtime's version [WV-WD], driven
  from Rust by `fantoccini` 0.22.1 (MIT OR Apache-2.0), not Tauri's Node guide [T-WDCI]: the million-entry archive
  scrolled with `[role=row]` ≤ `visible + 2·MARGIN_ROWS`, axe-core per view and dialog, a fixture extracted and
  BLAKE3-compared; `error`, `unhandledrejection` and `securitypolicyviolation` listeners set flags the driver reads,
  any failing the job (so axe-core breaking under Trusted Types surfaces).
- **E5-10, human:** Windows 10 22H2 x64, 11 x64, 11 ARM64 (pending): every dialog, keyboard path, Narrator, high
  contrast, mixed DPI, both themes, no runtime, the verbs, a log-off mid-create.

## 8. Order, acceptance, decisions, scope, risks

Order: E5-1, E5-2, E5-11's harness (an archive opens; E5-3's DOM count and E5-9's axe-core pass join it; ticked
last), E5-3, E5-7, E5-4, E5-6, E5-5, E5-8, E5-9, E5-10; E6-1 starts with E5-1's verb table. Additions, then
`D-NN`:

- **E5-1:** `docs/API.md` v0, the model bound, hooks, the manifest test. D-NN: crate, model, sharing, `Ids`, hooks,
  `calibrate`, `Class`, generations.
- **E5-2:** `cargo tauri build` (`cargo install tauri-cli` [T-NEW]); the Linux build; `cargo deny check`; no Node;
  CSP, capability, grep, exit and recovery tests; notices over `lpk-gui`. D-NN: Tauri 2.12, Windows-only Tauri,
  `lpk-gui-sys`, `deny.toml`, CSP, no isolation, grants, partial files.
- **E5-3, E5-4:** scrolling, opening, extract paths. D-NN: batch v1, open allow-list, smart rule, refuse-first.
- **E5-5 to E5-9, E5-11:** PLAN's (E5-7's reworded). D-NN: calibration; preview cap; verbs, settings, update
  default; history views, rollback confirm; checker; client.

**PLAN edits proposed.** E5-2: the command layer builds on both OSes, the Tauri host on Windows. E5-3: the backend
bound in the command layer. E5-7: capabilities scope only the page's IPC [T-CAP] and the check runs in Rust, so the
test reads the capability files, the CSP and the updater's endpoint list.

**Out of Phase 1.** Merge append, writing opened entries back, editing entries, thumbnails, pause, a password
manager, SFX and volumes, drag-out, Mica, other languages, Linux or macOS GUIs.

**Risks.**
1. **Licences.** tauri 2.12.5, wry 0.57.1, tao 0.37.2, webview2-com 0.39.1 (MIT and/or Apache-2.0; a scratch
   manifest, 2026-10-25). `cargo deny check` with our `deny.toml` fails on `webpki-root-certs` 1.0.11
   (CDLA-Permissive-2.0, on none of our triples: `[graph] targets` = `about.toml:22–26` drops it) and
   `target-lexicon` 0.12.16 (Apache-2.0 WITH LLVM-exception), reached only through `system-deps` → `cfg-expr`'s
   `targets` feature [CFGX], a build dependency of GTK's `-sys` crates compiled neither on Windows nor on Linux, yet
   kept once Linux is listed (observed). The answer: restore that licence (dropped as unused,
   `docs/LICENSING.md:12`), an owner question.
2. **MPL-2.0.** `option-ext` is used only in `dirs-sys`'s `cfg(unix)` module [DSYS], so no MPL-2.0 code reaches the
   Windows exe (E5-2: no `option_ext` symbol in the PDB); `selectors`, `cssparser`, `dtoa-short` are build-time.
   If shipped: source available, recipients told (§3.2), notices kept (§3.4), the larger work under any terms
   (§3.3), no relinking duty [MPL].
3. **WebView2 loader:** `webview2-com-sys` links Microsoft's `WebView2LoaderStatic.lib` on MSVC [WV2SYS], whose
   licence requires its notice [WV2LIC]; the crate says MIT, so `about.hbs` needs a static section.
4. **Also:** Tauri bumps re-run `cargo deny`, notices and E5-11; renderer bugs (bounded by grants; runtime updates
   apply at restart [WV-DIST]); Trusted Types; Narrator on a virtual `treegrid`; E4-2/E4-3 timing.

[T-CALL]: https://v2.tauri.app/develop/calling-rust/
[T-FRONT]: https://v2.tauri.app/develop/calling-frontend/
[T-CAP]: https://v2.tauri.app/security/capabilities/
[T-CSP]: https://v2.tauri.app/security/csp/
[T-ISO]: https://v2.tauri.app/concept/inter-process-communication/isolation/
[T-WIN]: https://v2.tauri.app/distribute/windows-installer/
[T-SI]: https://v2.tauri.app/plugin/single-instance/
[T-UPD]: https://v2.tauri.app/plugin/updater/
[T-DLG]: https://v2.tauri.app/plugin/dialog/
[T-WD]: https://v2.tauri.app/develop/tests/webdriver/
[T-WDCI]: https://v2.tauri.app/develop/tests/webdriver/ci/
[T-NEW]: https://v2.tauri.app/start/create-project/
[T-CFG]: https://v2.tauri.app/reference/config/#theme
[T-RUN]: https://docs.rs/tauri/2.12.5/tauri/enum.RunEvent.html
[T-PATH]: https://docs.rs/tauri/2.12.5/tauri/path/struct.PathResolver.html
[T-WVB]: https://docs.rs/tauri/2.12.5/tauri/webview/struct.WebviewWindowBuilder.html
[TAO-DPI]: https://docs.rs/crate/tao/0.37.2/source/src/platform_impl/windows/dpi.rs
[TAO-EL]: https://docs.rs/crate/tao/0.37.2/source/src/platform_impl/windows/event_loop.rs
[UPD-SRC]: https://docs.rs/crate/tauri-plugin-updater/2.14.0/source/src/updater.rs
[CFGX]: https://docs.rs/crate/cfg-expr/0.15.8/source/Cargo.toml
[DSYS]: https://docs.rs/crate/dirs-sys/0.5.0/source/src/lib.rs
[RSRN]: https://doc.rust-lang.org/std/fs/fn.rename.html
[WV2SYS]: https://docs.rs/crate/webview2-com-sys/0.39.1/source/src/lib.rs
[WV2LIC]: https://www.nuget.org/packages/Microsoft.Web.WebView2/1.0.4314.35/License
[WV-DIST]: https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution
[WV-WD]: https://learn.microsoft.com/en-us/microsoft-edge/webview2/how-to/webdriver
[WM-ES]: https://learn.microsoft.com/en-us/windows/win32/shutdown/wm-endsession
[MFX]: https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw
[FIDI]: https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info
[ES-PW]: https://learn.microsoft.com/en-us/windows/win32/controls/edit-control-styles
[CTLC]: https://learn.microsoft.com/en-us/windows/win32/controls/wm-ctlcoloredit
[DWMA]: https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute
[TDC]: https://learn.microsoft.com/en-us/windows/win32/api/commctrl/ns-commctrl-taskdialogconfig
[CRED]: https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-creduipromptforwindowscredentialsw
[CPW]: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw
[MS-KB]: https://learn.microsoft.com/en-us/windows/apps/design/accessibility/keyboard-accessibility
[ARIA]: https://www.w3.org/TR/wai-aria-1.2/#aria-rowcount
[TT]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/require-trusted-types-for
[MDN-FC]: https://developer.mozilla.org/en-US/docs/Web/CSS/@media/forced-colors
[AXE]: https://github.com/dequelabs/axe-core
[AXE-API]: https://www.deque.com/axe/core-documentation/api-documentation/
[CVE]: https://www.helpnetsecurity.com/2023/09/12/cve-2023-4863/
[WRAR]: https://group-ib.com/blog/cve-2023-38831-winrar-zero-day/
[TR36]: https://www.unicode.org/reports/tr36/
[MPL]: https://www.mozilla.org/en-US/MPL/2.0/
