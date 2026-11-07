# E6 design: Explorer integration (`lpk-shell`, associations, Mark of the Web, the Windows extraction policy)

2026-10-25; for E6-1 to E6-7 (`docs/PLAN.md:209–225`), on `origin/main` 8855ac5. Numbers are design defaults, never
measurements (D-07); each choice becomes a `D-NN` of §7. "UX": the uncommitted competitor study of 2026-10-25.

## 1. `lpk-shell`: an in-process handler that only launches the GUI (E6-1, E6-2)

**Decision.** `crates/lpk-shell`, a `cdylib` (`lpk_shell.dll`, `x86_64-` and `aarch64-pc-windows-msvc`): one COM class
implementing `IExplorerCommand`, loaded in process by Explorer and any program showing a shell menu [CMH]; its one
action starts `lpk-gui.exe` with E5-7's verbs (`docs/notes/e5-design.md:176–182`). In process because nothing else
exists: `IExplorerCommand` "supports in-process activation only" [SCM], and Windows 11's menu takes it only with
package identity, as a DLL matching Explorer's architecture [IPF]. E6-2's "out of process" holds for the work, done in
`lpk-gui.exe` (§7).

**Rejected.** An EXE server: `IExplorerCommand` cannot run out of process, and the verbs that can (DropTarget,
`IExecuteCommand`) have no dynamic title or state [SCM]; static `command` verbs: fixed titles, 2,000-character command
lines [SCM]; `IContextMenu` (E6-4's title): in process too, "the most complicated", discouraged [SCM].

**Shape.** `src/model.rs` (safe, tested on both OSes) maps a selection to states, titles and verb records;
`src/com.rs`, the only module allowed `unsafe`, holds `DllGetClassObject`, `DllCanUnloadNow`, the class factory, the
`#[implement]` objects (`IExplorerCommand`, `IEnumExplorerCommand`, `IObjectWithSite`), the `GuiPath` read,
`CreateProcessW`, and the `#[cfg(test)]` helpers (`CoInitializeEx`, `SHCreateItemFromParsingName`), so tests needing
COM are unit tests inside it. Dependencies: `windows` and `windows-core` 0.62.2, already locked
(`Cargo.lock:3109–3131`); a manifest test fails on any `lpk-*`, Tauri or WebView2 crate in the normal tree (engine and
webview stay out).

**In a foreign process.** Methods run on the UI thread and must not touch the network [IEC]: `GetState` and `GetTitle`
read names and SFGAO attributes only; no thread, COM initialisation or global state. An unwind out of an
`extern "system"` frame aborts the process [R181], here Explorer: each COM method body runs in `catch_unwind`
returning `E_UNEXPECTED`, the release profile keeps unwinding (`Cargo.toml:40–43`), a `compile_error!` under
`cfg(panic = "abort")` [RCFG] stops a profile change, and `unwrap_used`, `expect_used`, `indexing_slicing` and `panic`
are denied.

**Invoke: the contract with the GUI.**
- The paths travel in a binary response file, `%LOCALAPPDATA%\org.litepack.gui\Verbs\shell-<32 hex>.verbs` (the GUI's local state, E5-7), created
  create-new: a magic, a protocol version and the verb, then per path a `u32` count of UTF-16LE code units and the
  units, unpaired surrogates kept; the GUI rebuilds each path with `OsString::from_wide`, which is lossless [FROMW],
  so D-89 (4)'s names round-trip. The command line is `"<GuiPath>" @<file>`.
- `CreateProcessW` gets `lpApplicationName` explicitly: `GuiPath`, recorded in `HKCU\Software\LitePack` by the
  installer or `--register-shell`, so a DLL still loaded after an upgrade starts the current GUI;
  `bInheritHandles = FALSE`, as Windows Terminal's extension does [WT-SRC]; the install folder as working directory (a
  working directory would pin the user's folder). E7-4 settles the launch for the full MSIX, where no installer writes
  HKCU.
- The GUI accepts the current protocol version and the previous one; after reading, it deletes only a file in that
  folder matching the shell's pattern, never another `@file`, and its start-up sweep only such files older than 24
  hours.

**`D-NN` (the exception) and where E6's `unsafe` lives.** `lpk-shell`'s lint table sets `unsafe_code = "deny"` (the
workspace's `forbid`, `Cargo.toml:27–28`, cannot be relaxed: `docs/PLAN.md:219`) and only `src/com.rs` carries
`#![allow(unsafe_code)]`, its test helpers included; `unsafe_op_in_unsafe_fn` and `clippy::undocumented_unsafe_blocks`
are denied as in `crates/lpk-sandbox-sys/Cargo.toml:43–51`; a CI grep fails on `allow(unsafe_code)` elsewhere.
Registration's `unsafe` (registry writes, `SHChangeNotify`) sits in E5's `lpk-gui-sys`, or the registry writes go
through `windows-registry` if it passes `cargo deny` (E6-4 records which); the extraction policy's in `lpk-fs-sys`
(§5). The `D-NN` and E8-7's inventory name all three: `src/com.rs`, `lpk-gui-sys`, `lpk-fs-sys`.

## 2. Verbs and states (E6-2)

One top-level command, "LitePack" (`ECF_HASSUBCOMMANDS`), holds the seven verbs: Windows 11 shows one entry per
application [BLOG], and Explorer one level of subcommands [ESC]. No flat layout (UX synthesis 4): Windows 11 groups
the verbs anyway [BLOG], and a second layout doubles E6-2's table and E6-7's records. Registered for `*`, `Directory`
and `Directory\Background` only (no selection matches two), it recognises archives by name. Kinds: **F** files, none
an archive; **D** folders; **B** the background; **L** one `.lpk`; **A** one foreign archive (by extension, a list a
test keeps equal to E4-4's `docs/FORMATS.md`); **S** several archives; **M** anything else. The volumes of one
multi-volume set (`.part1.rar`, `.7z.001` and onward) count as one archive. A ZIP (folder-and-stream to Explorer)
counts as a file; an item without a file-system path (a library, This PC, a ZIP's inside) hides the menu, as in
Windows Terminal's `GetState` [WT-SRC].

| Verb (E5-7 flag) | F | D | B | L | A | S | M |
|---|---|---|---|---|---|---|---|
| `Add to archive…` (`--create`) | ✓ | ✓ | ✓¹ | ✓ | ✓ | ✓ | ✓ |
| `Add to "<name>.lpk"` (`--create … --name`) | ✓ | ✓ |  |  |  |  | ✓ |
| `Extract here` (`--extract-here`) |  |  |  | ✓ | ✓ | ✓ |  |
| `Extract to "<name>\"` (`--extract-to`) |  |  |  | ✓ | ✓ | ✓² |  |
| `Open` (`--open`) |  |  |  | ✓ | ✓ |  |  |
| `Test` (`--test`) |  |  |  | ✓ | ✓ | ✓ |  |
| `Convert to…` (`--convert`) |  |  |  |  | ✓ |  |  |

A blank is `ECS_HIDDEN`. ¹ The folder as input, in the wizard (the destination visible). ² Titled `Extract to "*\"`.

`<name>`: an item's name less its last extension (an archive's `.tar.gz`-style pair), a folder's name, or for several
items their parent's. Titles double `&`, which marks a menu access key [MENU], escape control and bidirectional
characters [TR36] and shorten names past 40 characters in the middle; the verb gets the full name. Archive verbs
first, no separators; past 16,384 items only `Add to archive…` remains, bounding the UI-thread scan. Root icon:
`lpk-gui.exe,0`.

**The verb table E6-2 tests (E6-1, 2026-10-26).** The texts are E5-1's (`docs/API.md`, "Activation verbs"), which
`crates/lpk-shell/src/model.rs` holds and a unit test keeps equal to that table; "(proposed)" marks what neither
E5-1 nor E5-7 settles, §2's proposal until E6-2's `D-NN`. The command line is the argument list `lpk-gui.exe`
receives: `Invoke` passes it as `"<GuiPath>" @<file>`, the response file (§1) carrying the verb and the paths in
selection order. `<p>` is each selected path, `<a>` each selected archive, `<dir>` the background's folder; `<name>` as
above (the shown title shortened and escaped, the argument in full). A blank cell is `ECS_HIDDEN`.

| Selection kind | Verb texts, in menu order | Command line of each verb |
|---|---|---|
| **F** file(s), none an archive (a ZIP counts as a file) | `Add to archive…`; `Add to "<name>.lpk"` | `--create <p>...`; `--create <p>... --name <name>.lpk` |
| **D** folder(s) | `Add to archive…`; `Add to "<name>.lpk"` | `--create <p>...`; `--create <p>... --name <name>.lpk` |
| **B** folder background | `Add to archive…` | `--create <dir>` (the wizard, the destination visible) |
| **L** one `.lpk` | `Add to archive…`; `Extract here`; `Extract to "<name>\"`; `Open`; `Test` | `--create <a>`; `--extract-here <a>`; `--extract-to <a>`; `--open <a>`; `--test <a>` |
| **A** one foreign archive (one multi-volume set included) | `Add to archive…`; `Extract here`; `Extract to "<name>\"`; `Open`; `Test`; `Convert to…` | `--create <a>`; `--extract-here <a>`; `--extract-to <a>`; `--open <a>`; `--test <a>`; `--convert <a>` |
| **S** several archives (`.lpk` or foreign) | `Add to archive…`; `Extract here`; `Extract to "*\"` (proposed); `Test` | `--create <a>...`; `--extract-here <a>...`; `--extract-to <a>...`; `--test <a>...` |
| **M** mixed selection (archives with other items, or files with folders) | `Add to archive…`; `Add to "<name>.lpk"` | `--create <p>...`; `--create <p>... --name <name>.lpk` |
| more than 16,384 items, any kind | `Add to archive…` | `--create <p>...` |
| an item without a file-system path | (the menu is hidden) | none |

The root's text is `LitePack` (proposed; `model::ROOT_TITLE`), the one title E6-1's skeleton returns.

**The site.** Explorer gives the root its site (`IObjectWithSite::SetSite`); `EnumSubCommands` hands each subcommand a
reference to the root, so `Invoke` takes the background folder from the item array or, when empty, the root's site
(`SID_SFolderView`, `IFolderView::GetFolder`), as Windows Terminal does [WT-SRC], never the foreground window or a
window enumeration; nothing is cached between calls.

## 3. Registration routes and associations (E6-3, E6-4)

**Route P, the package.** The NSIS install's sparse package (E6-3) and E7-4's MSIX declare `windows.comServer`
(`com:SurrogateServer`, `ThreadingModel="STA"`) and `windows.fileExplorerContextMenus` [IPF], an element present since
Windows 10 1803 [FECM] that Windows Terminal ships with a 19041 minimum [WT-MAN]; "Show more options" loads the
Windows 10 menu as-is [BLOG], so one package should serve both Windows 11 menus and Windows 10 22H2's (E6-7 checks).
**Route C, classic:** `HKCU\Software\Classes\{*,Directory,Directory\Background}\shell\LitePack` with
`ExplorerCommandHandler` naming the class under a second CLSID (distinct from the package's), `InProcServer32`
(`Apartment`) under `HKCU\Software\Classes\CLSID` [ECV], and `MultiSelectModel = Player` (`Document` stops at 15
items [CMH]).

**One entry per menu, one registration per user.** P when it registers; C before E7-1's identity exists or where AppX
deployment is refused; P and C together only on builds where E6-7 finds the packaged submenu missing from the legacy
menu, which C then serves alone (WinRAR reports both duplicated and missing legacy entries [RAR-NEW]). Across NSIS and
the Store package: an unpackaged install skips its registration, saying so, when the user has the Store package; the
Store package's GUI, running with package identity, removes a classic HKCU registration (packaged HKCU writes may be
redirected [MSIX-RT]; E7-4 proves the removal reaches the real registry).

**The sparse package** needs a `Publisher` equal to the certificate's subject, a production certificate trusted on
users' machines, `MinVersion` 10.0.19041.0 and `ProcessorArchitecture="neutral"`; a version registers once
(0x80073CF9), so it follows the product's [GI]. Development and CI sign with a throwaway `CN=LitePack Test`
certificate in Trusted People [GI]; E7-1's publisher is pending, so E6-3's acceptance waits on E7-1 and E7-3.
`lpk-gui.exe` gets an `msix` element only if E6-3 shows the menu needs one.
`lpk-gui.exe --register-shell|--unregister-shell` (installer, uninstaller, settings) run before the single-instance
plugin (`docs/notes/e5-design.md:176–177`) and exit with a code, never forwarded to a running instance; they register
the package (`PackageManager.AddPackageByUriAsync` with `ExternalLocationUri` [GI], a safe WinRT call) and write
`GuiPath`.

**Associations** (per user, `HKCU\Software\Classes` [FT]): ProgID `LitePack.lpk` (`shell\open\command` =
`"…\lpk-gui.exe" --open "%1"`, `DefaultIcon`, `PerceivedType` = `compressed`); `.lpk`'s default value set only when
absent or ours, plus `OpenWithProgids` [FT]. ZIP, 7z, RAR and tar (`.zip .7z .rar .tar .tgz`) are opt-in (an unticked
installer box, a setting): `LitePack.zip`-style ProgIDs in each extension's `OpenWithProgids`, never its default.
Default Programs: `HKCU\Software\LitePack\Capabilities` and `HKCU\Software\RegisteredApplications` [DP][LDA].
`UserChoice` is never written: Windows 10 changed defaults and the choice is the user's [DP]; a button opens
`ms-settings:defaultapps?registeredAppUser=LitePack` on Windows 11 [LDA]. Each change ends with
`SHChangeNotify(SHCNE_ASSOCCHANGED)` [FT]. The Store package declares `.lpk` only: a package's associations are fixed
at install.

**As found.** `HKCU\Software\LitePack\Registration` lists the keys registration created; unregister deletes our values
whose data are still ours, then listed keys if empty, then the list, keeping values others changed since [FT].

## 4. Mark of the Web (E6-5)

**Rule.** Only an archive whose `Zone.Identifier` gives `ZoneId` 3 or 4 (the zones Windows marks [OFC]), or an
unparseable stream (read as 3), passes `[ZoneTransfer]`/`ZoneId=<n>` [OFC] to every file derived from it: extracted
files (`.lpk` in process, foreign archives through the broker, hard-link copies), E5's opened entries, `--convert`'s
output, export and repair copies. Zones 0–2 or none: nothing, which refines E5 §4's "zoned like the archive"
(`docs/notes/e5-design.md:156–160`); entries opened from the GUI follow this rule, with E5's Internet for a zoneless
foreign archive, whatever the opt-out says. The zone is read once, at open; the stream is written after the file's
create-new and before its first byte, through its relative open (§5): no extracted file exists unmarked. A destination
whose volume lacks `FILE_NAMED_STREAMS` [GVI], or where the probe or the write fails, cannot hold the mark: one
warning. Opt-out: `lpk x --motw on|off` (default on; `OnOff`, `crates/lpk-cli/src/lib.rs:626`) and a GUI setting,
switched off only after a native confirm (`lpk-gui-sys`, as E5's rollback confirm, `docs/notes/e5-design.md:140`).

**ZoneId only**: Office's macro blocking keys on `ZoneId` [OFC]; copying `HostUrl`/`ReferrerUrl` would spread tokens
or local paths. **Rejected:** `IAttachmentExecute` (the outline, `docs/PLAN.md:78`): made for clients that interact
with the user, a COM object per file, an apartment per writer thread [IAE]; `MapUrlToZone` for zoneless archives on
shares (Phase 2). **Nested archives** are files: they carry the zone, and their extraction propagates it; 7-Zip
shipped a propagation bypass until 24.09 (CVE-2025-0411 [ZDI-0411]).

**Cost (E6-5's row).** A per-setting catalogue key `zone = 3` makes the runner write the stream onto that setting's
archive between compression and extraction (Windows only, recorded in the row); `lpk` gains `fast-zoned` and
`fast-zoned-no-motw` (`extract = ["--motw", "off"]`) beside `fast` (`bench/tools.toml:236–305`), measured in one run
on `small-files` (20,000 files under 8 KB, `docs/CORPUS.md:16`: the worst case), at least three repeats (E2-24's
form), release build, idle machine; the directory committed and reported. No figure exists until then.

## 5. The Windows extraction policy (E6-6)

**Today** every write goes by path (`Shared::create`, `crates/lpk-core/src/extract.rs:398–411`; reopen `:509–512`;
cleanup `:389–396`; `create_dir_all` `:1015–1021`; `remove_existing`, `crates/lpk-core/src/policy.rs:508–531`; the
broker's `FsOut`, `crates/lpk-core/src/foreign.rs:1239–1278`, whose `copy` opens a hard-link source by path, `:1286`),
and the linked-parent check runs once, at plan time (`extract.rs:690–725`): a parent swapped later is followed.

**Rule (decision (e)).** A new `-sys` crate, `lpk-fs-sys` (E4-1's lint table), under a safe `lpk_core::outtree` both
writers use:
- the destination is opened once, as chosen; each directory below is opened or created one component at a time by
  `NtCreateFile` relative to its verified parent's handle, with `FILE_OPEN_REPARSE_POINT` [NTCF], `OBJ_DONT_REPARSE`
  and `OBJ_CASE_INSENSITIVE` [OA]; a name-surrogate tag (symlink, junction, mount point [RPT]) fails as D-54's "linked
  parent", other tags (cloud placeholders, dedup) pass, as at `extract.rs:456–460`;
- directory handles are held without `FILE_SHARE_DELETE` [NTCF], so a verified directory cannot be renamed or replaced
  in use; past 4,096 one is closed, and its reopening compares the file ID [FIDI];
- files are created with `FILE_CREATE` [NTCF] relative to the held parent; reopens, cleanup, `replace` and
  `FsOut::copy`'s source go through the parent handle and the file ID recorded at creation;
- `lpk-fs-sys` returns `OwnedHandle` and `File`, so `lpk-core` keeps `forbid(unsafe_code)`.

The plan-time check remains the refusal before output (D-79); the relative opens make it binding. **Rejected:** path
opens re-checked by `GetFinalPathNameByHandleW` (detects after writing); full-path `OBJ_DONT_REPARSE` (refuses a
destination reached through a link); `cap-std` (a dependency tree for five calls, still needing the audit).

**Symlinks (decision (d); D-79 (3) amended).** Created only with `--symlinks` (E3-2's opt-in) and a token holding
`SeCreateSymbolicLinkPrivilege`, which `FSCTL_SET_REPARSE_POINT` requires [FSRP], on a node created relative to the
held parent; otherwise skipped with a warning naming the reason (today an unprivileged creation fails the extraction,
`crates/lpk-cli/src/lib.rs:728–733`). Developer Mode alone does not qualify. **Targets:** absolute, UNC, device
(`\\?\`, `\\.\`), drive-relative (`C:x`) or rooted (`\x`) targets, and any escaping the destination after
normalisation, are skipped with a warning, counted, never created; allowing them is out of Phase 1. **Kind:** from the
archive, never the file system: a target resolving, in the archive's namespace, to a directory entry gives a directory
link, anything else a file link. Today's Windows `make_link` (`crates/lpk-core/src/extract.rs:1050–1058`) reads
`parent.join(target)`'s metadata first, so under `--symlinks` a UNC target is contacted even when creation then fails
(links are skipped by default: `policy.rs:56`, `:672`); a standalone fix lands before E6-6, which inherits the rule.
Precedent: 7-Zip's link hardening in 25.00 and 25.01 [7Z-HIST][ZDI-11001]. Nothing is written through a created link
(a name surrogate below the destination is refused). Tests cover each refused target form and the kind rule; a seam
lets CI run both privilege branches, one test checking the probe against the real token.

**Names.** The colon refusal (`policy.rs:353–355`) keeps entries out of alternate streams (WinRAR's
CVE-2025-8088 [ESET]); NT opens skip Win32's name parsing [NAME], so the trailing-dot and device refusals
(`policy.rs:356–361`) stay the guard. One component per open, so `MAX_PATH` never applies; a component over 255 UTF-16
units, the usual limit, or a total path over 32,767 [MAXP] is refused by name before output. Names differing only by
case stay refused (`policy.rs:443–480`), even in case-sensitive folders (Windows 10 17107+ [CS]): Windows applications
cannot tell such files apart [CS].

**TOCTOU test.** A hook between a directory's verification and its child's creation renames the parent and plants a
junction to an outside folder holding a canary, under `--overwrite replace`: (a) with held handles the rename fails;
(b) with a switch adding `FILE_SHARE_DELETE` the swap succeeds and the file lands in the moved original. Both assert
the outside folder's listing and the canary's bytes unchanged, with a red run against today's writer recorded. Hook
and switch exist only under `cfg(test)` or a test-only feature, absent from release as D-99 (8)'s hooks.

## 6. Testing

**CI (`windows-2025`):** the model's table per kind (both OSes), a property test over arbitrary names (no panic,
bounded titles), the COM objects called in process, both targets built; the sparse package registered and removed with
the test certificate (`Get-AppxPackage`); E6-4's `scripts/e6-registry-diff.ps1` (`reg export` of every key §3 touches
and each `FileExts\<ext>\UserChoice`, before registering and after unregistering, diffed; `AssocQueryStringW` [AQS]
for `.lpk`); the response file's round trip with an unpaired surrogate; §4's and §5's tests, foreign archives through
E4's worker; a DLL load test on `windows-11-arm` (E7 (g)). No runner shows Explorer menus or runs Windows 10
(`docs/notes/e4-design.md:248–250`).

**By hand (E6-7)**, Windows 11 x64 (ARM64 when available) and Windows 10 22H2 x64, per route (route C on 22H2 too):
the submenu for a file, a folder, the background, a `.lpk`, a foreign archive, several and mixed selections and more
than 15 items on route P, each verb reaching the GUI with its flag (logged); "Show more options"; names with `&`,
emoji, CJK and bidirectional characters; extraction into a OneDrive folder (§5's cloud tags); install, upgrade,
uninstall and any Explorer restart. Screenshots: a local account `tester`, generic files under `C:\lpk-e6\`, cropped
to the window.

## 7. Order, acceptance, decisions, scope, risks

Order: the standalone symlink fix (§5), then E6-6 → E6-5 (CLI only; they protect `lpk x` now) → E6-1 (needs E5-1's
verb text, `docs/PLAN.md:219`) → E6-2 (after E5-7) → E6-4 → E6-3 (after E7-1, E7-3) → E6-7.

| Task | Acceptance additions | `D-NN` records |
|---|---|---|
| E6-1 | lint table, both targets, the grep, the manifest test | the exception's three places; in-process shape |
| E6-6 | §5's tests and TOCTOU red run | parent-handle rule; symlinks, D-79 (3) amended |
| E6-5 | PLAN's three cases, nested and derived files; the committed row | rule, content, opt-out, row |
| E6-2 | §2's table per kind, titles, the response file | titles, states, verb protocol |
| E6-4 | the diff empty; `UserChoice` unchanged | routes, associations, registration's `unsafe` |
| E6-3 | PLAN's `Get-AppxPackage` output and screenshot | identity, versioning |
| E6-7 | §6's records, route C on 22H2 (no Windows 10 runner) | route per OS |

**PLAN edits proposed.** E6-2: "out of process" → "in process; the work runs in `lpk-gui.exe`". E6-4: "Classic
`IContextMenu`" → route C. The outline's `IAttachmentExecute` → the stream written directly.

**Out of Phase 1.** Drive and desktop menus (`desktop10:ItemType`, 22H2+ [FECM]); right-drag, thumbnail, preview and
property handlers; browsing `.lpk` as a folder; an ARM64X DLL for x64 programs on ARM64; per-verb icons; Linux
`openat2`; foreign "Open with" in the Store package.

**Risks.** (1) Our code runs inside Explorer and other hosts, and can harm them [CMH]; §1's rules bound it. (2)
Registration changes may need an Explorer restart or sign-out [IPF]; each version's DLL gets its own folder, the old
one deleted once unused; the installer never kills Explorer. (3) No modern menu until E7-1's identity and certificate
exist (weeks of validation). (4) Windows 10 22H2: the packaged submenu is unproven there [FECM][WT-MAN] and no CI
covers it; E6-7 decides its route.

[IPF]: https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/integrate-packaged-app-with-file-explorer
[GI]: https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/grant-identity-to-nonpackaged-apps
[FECM]: https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop4-fileexplorercontextmenus
[IEC]: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-iexplorercommand
[ESC]: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-iexplorercommand-enumsubcommands
[SCM]: https://learn.microsoft.com/en-us/windows/win32/shell/shortcut-choose-method
[CMH]: https://learn.microsoft.com/en-us/windows/win32/shell/context-menu-handlers
[BLOG]: https://blogs.windows.com/windowsdeveloper/2021/07/19/extending-the-context-menu-and-share-dialog-in-windows-11/
[ECV]: https://github.com/microsoft/Windows-classic-samples/blob/main/Samples/Win7Samples/winui/shell/appshellintegration/ExplorerCommandVerb/RegisterExtension.cpp
[WT-SRC]: https://github.com/microsoft/terminal/blob/main/src/cascadia/ShellExtension/OpenTerminalHere.cpp
[WT-MAN]: https://github.com/microsoft/terminal/blob/main/src/cascadia/CascadiaPackage/Package.appxmanifest
[RAR-NEW]: https://www.rarlab.com/rarnew.htm
[MENU]: https://learn.microsoft.com/en-us/windows/win32/menurc/about-menus
[MSIX-RT]: https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-behind-the-scenes
[FT]: https://learn.microsoft.com/en-us/windows/win32/shell/fa-file-types
[DP]: https://learn.microsoft.com/en-us/windows/win32/shell/default-programs
[LDA]: https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-default-apps-settings
[AQS]: https://learn.microsoft.com/en-us/windows/win32/api/shlwapi/nf-shlwapi-assocquerystringw
[OFC]: https://learn.microsoft.com/en-us/microsoft-365-apps/security/internet-macros-blocked
[GVI]: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationbyhandlew
[IAE]: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-iattachmentexecute
[ZDI-0411]: https://www.zerodayinitiative.com/advisories/ZDI-25-045/
[ZDI-11001]: https://www.zerodayinitiative.com/advisories/ZDI-25-949/
[7Z-HIST]: https://www.7-zip.org/history.txt
[ESET]: https://www.welivesecurity.com/en/eset-research/update-winrar-tools-now-romcom-and-others-exploiting-zero-day-vulnerability/
[NTCF]: https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile
[OA]: https://learn.microsoft.com/en-us/windows/win32/api/ntdef/ns-ntdef-_object_attributes
[RPT]: https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-point-tags
[FIDI]: https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info
[FSRP]: https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_set_reparse_point
[MAXP]: https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation
[NAME]: https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
[CS]: https://learn.microsoft.com/en-us/windows/wsl/case-sensitivity
[R181]: https://blog.rust-lang.org/2024/09/05/Rust-1.81.0/
[RCFG]: https://doc.rust-lang.org/reference/conditional-compilation.html#panic
[FROMW]: https://doc.rust-lang.org/std/os/windows/ffi/trait.OsStringExt.html#tymethod.from_wide
[TR36]: https://www.unicode.org/reports/tr36/
