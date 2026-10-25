# E6 design: Explorer integration (`lpk-shell`, associations, Mark of the Web, the Windows extraction policy)

2026-10-25; for E6-1 to E6-7 (`docs/PLAN.md:209–225`), on `origin/main` 73da11a. Numbers are design defaults, never
measurements (D-07); each choice becomes a `D-NN` of §7. UX §4: the uncommitted competitor study of 2026-10-25.

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

**Shape.** `src/model.rs` (safe, tested on both OSes) maps a selection to states, titles and verb lines; `src/com.rs`,
the only module allowed `unsafe`, holds `DllGetClassObject`, `DllCanUnloadNow`, the class factory, the `#[implement]`
objects (`IExplorerCommand`, `IEnumExplorerCommand`, `IObjectWithSite`) and `CreateProcessW`. Dependencies: `windows`
and `windows-core` 0.62.2, already locked (`Cargo.lock:3109–3131`); a manifest test fails on any `lpk-*`, Tauri or
WebView2 crate in the normal tree (engine and webview stay out); an `rlib` crate type lets tests call the objects in
process.

**In a foreign process.** Methods run on the UI thread and must not touch the network [IEC]: `GetState` and `GetTitle`
read names and SFGAO attributes only; no thread, COM initialisation or global state. An unwind out of an
`extern "system"` frame aborts the process [R181], here Explorer: each COM method body runs in `catch_unwind`
returning `E_UNEXPECTED`, the release profile keeps unwinding (`Cargo.toml:40–43`), and `unwrap_used`, `expect_used`,
`indexing_slicing` and `panic` are denied.

**Invoke** resolves `lpk-gui.exe` beside the DLL (`GetModuleFileNameW`, never `PATH`), always passes the paths in a
response file under `%LOCALAPPDATA%\LitePack\Verbs\` (E5-7's `@file`), and calls `CreateProcessW` with
`bInheritHandles = FALSE`, as Windows Terminal's extension does [WT-SRC], from the install folder (a working directory
would pin the user's folder). The GUI deletes it and sweeps leftovers.

**`D-NN` (the exception).** The crate's lint table sets `unsafe_code = "deny"` (the workspace's `forbid`,
`Cargo.toml:27–28`, cannot be relaxed: `docs/PLAN.md:219`); only `src/com.rs` carries `#![allow(unsafe_code)]`;
`unsafe_op_in_unsafe_fn` and `clippy::undocumented_unsafe_blocks` are denied as in
`crates/lpk-sandbox-sys/Cargo.toml:43–51`; a CI grep fails on `allow(unsafe_code)` elsewhere; E8-7's inventory lists
it.

## 2. Verbs and states (E6-2)

One top-level command, "LitePack" (`ECF_HASSUBCOMMANDS`), holds the seven verbs: Windows 11 takes one top-level
command or submenu per application [RAR-NEW], Explorer one level of subcommands [ESC]. Registered for `*`, `Directory`
and `Directory\Background` only (no selection matches two), it recognises archives by name. Kinds: **F** files, none
an archive; **D** folders; **B** the background; **L** one `.lpk`; **A** one foreign archive (by extension, a list a
test keeps equal to E4-4's `docs/FORMATS.md`); **S** several archives; **M** anything else. A ZIP (folder-and-stream
to Explorer) counts as a file; an item without a file-system path (a library, This PC, a ZIP's inside) hides the menu,
as in Windows Terminal's `GetState` [WT-SRC].

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
items their parent's. Titles escape control and bidirectional characters [TR36] and shorten names past 40 characters
in the middle; the verb gets the full name. Archive verbs first, no separators; past 16,384 items only
`Add to archive…` remains, bounding the UI-thread scan. The root icon is `lpk-gui.exe,0` (E5 (e)'s placeholder set);
per-verb icons wait for a resource step.

**Rivals' quirks** (duplicated or missing legacy entries in WinRAR's notes [RAR-NEW], Explorer-tab fixes in UX §4):
one route per user (§3); the background folder from the item array or, when empty, this command's own site
(`SID_SFolderView`, `IFolderView::GetFolder`) as in Windows Terminal [WT-SRC], never the foreground window or window
enumeration; no cache between calls.

## 3. Registration routes and associations (E6-3, E6-4)

**Route P, the package.** The NSIS install's sparse package (E6-3) and E7-4's MSIX declare `windows.comServer`
(`com:SurrogateServer`, `ThreadingModel="STA"`) and `windows.fileExplorerContextMenus` [IPF], an element present since
Windows 10 1803 [FECM] that Windows Terminal ships with a 19041 minimum [WT-MAN]; "Show more options" loads the
Windows 10 menu as-is [BLOG], so one package should serve both Windows 11 menus and Windows 10 22H2's (E6-7 checks).
**Route C, classic:** `HKCU\Software\Classes\{*,Directory,Directory\Background}\shell\LitePack` with
`ExplorerCommandHandler` naming the class under a second CLSID (distinct from the package's), `InProcServer32`
(`Apartment`) under `HKCU\Software\Classes\CLSID` [ECV], and `MultiSelectModel = Player` (`Document` stops at 15
items [CMH]). One route per user, never both: P when it registers; C before E7-1's identity exists, where AppX
deployment is refused, and on builds where E6-7 finds the packaged submenu missing from the legacy menu; the installer
checks for the Store package first.

**The sparse package** needs a `Publisher` equal to the certificate's subject, a production certificate trusted on
users' machines, `MinVersion` 10.0.19041.0 and `ProcessorArchitecture="neutral"`; a version registers once
(0x80073CF9), so it follows the product's [GI]. Development and CI sign with a throwaway `CN=LitePack Test`
certificate in Trusted People [GI]; E7-1's publisher is pending, so E6-3's acceptance waits on E7-1 and E7-3.
`lpk-gui.exe` gets an `msix` element only if E6-3 shows the menu needs one.
`lpk-gui.exe --register-shell|--unregister-shell` (installer, uninstaller, settings) calls
`PackageManager.AddPackageByUriAsync` with `ExternalLocationUri` [GI] through the `windows` crate's WinRT projection:
no `unsafe`, no PowerShell.

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
whose data are still ours, then listed keys if empty, then the list, keeping values others changed since ([FT]'s
uninstall caution).

## 4. Mark of the Web (E6-5)

**Rule.** An archive whose `Zone.Identifier` gives `ZoneId` 3 or 4 (the zones Windows marks [OFC]), or an unparseable
stream (read as 3), gives `[ZoneTransfer]`/`ZoneId=<n>` [OFC] to every file derived from it: extracted files (`.lpk`
in process, foreign archives through the broker, hard-link copies), E5's opened entries
(`docs/notes/e5-design.md:156–160`, Internet for a zoneless foreign archive), `--convert`'s output, export and repair
copies. Zones 0–2 or none: nothing. The zone is read once, at open; the stream is written after the file's create-new
and before its first byte, through its relative open (§5), so no extracted file exists unmarked, even after an
interruption. The mark exists only on NTFS [OFC]: elsewhere, one warning. Opt-out: `lpk x --motw on|off` (default on;
`OnOff`, `crates/lpk-cli/src/lib.rs:626`) and a GUI setting.

**ZoneId only**: Office's macro blocking keys on `ZoneId` [OFC]; copied `HostUrl`/`ReferrerUrl` lines would spread
tokens or local paths. **Rejected:** `IAttachmentExecute` (the outline, `docs/PLAN.md:78`): made for clients that
interact with the user, a COM object per file, an apartment per writer thread [IAE]; `MapUrlToZone` for zoneless
archives on shares (Phase 2). **Nested archives** are files: they carry the zone, and their extraction propagates it;
7-Zip shipped a propagation bypass until 24.09 (CVE-2025-0411 [ZDI-0411]).

**Cost (E6-5's row).** A per-setting catalogue key `zone = 3` makes the runner write the stream onto that setting's
archive between compression and extraction (Windows only, recorded in the row); `lpk` gains `fast-zoned` and
`fast-zoned-no-motw` (`extract = ["--motw", "off"]`) beside `fast` (`bench/tools.toml:236–305`), measured in one run
on `small-files` (20,000 files under 8 KB, `docs/CORPUS.md:16`: a stream per file is the worst case), at least three
repeats (E2-24's form), release build, idle machine; the directory committed and summarised by the report command. No
figure exists until then.

## 5. The Windows extraction policy (E6-6)

**Today** every write goes by path (`Shared::create`, `crates/lpk-core/src/extract.rs:398–411`; reopen `:509–512`;
cleanup `:389–396`; `create_dir_all` `:1015–1021`; `remove_existing`, `crates/lpk-core/src/policy.rs:508–531`; the
broker's `FsOut`, `crates/lpk-core/src/foreign.rs:1239–1278`), and the linked-parent check runs once, at plan time
(`extract.rs:690–725`): a parent swapped later is followed.

**Rule (decision (e)).** A new `-sys` crate, `lpk-fs-sys` (E4-1's lint table), under a safe `lpk_core::outtree` both
writers use:
- the destination is opened once, as chosen; each directory below is opened or created one component at a time by
  `NtCreateFile` relative to its verified parent's handle, with `FILE_OPEN_REPARSE_POINT` [NTCF] and
  `OBJ_DONT_REPARSE` [OA]; a name-surrogate tag (symlink, junction, mount point [RPT]) fails as D-54's "linked
  parent", other tags (cloud placeholders, dedup) pass, as at `extract.rs:456–460`;
- directory handles are held without `FILE_SHARE_DELETE` [NTCF], so a verified directory cannot be renamed or replaced
  in use; past 4,096 one is closed, and its reopening compares the file ID [FIDI];
- files are created with `FILE_CREATE` [NTCF] relative to the held parent; reopens, cleanup and `replace` use the
  parent handle and the file ID recorded at creation;
- `lpk-fs-sys` returns `OwnedHandle` and `File`, so `lpk-core` keeps `forbid(unsafe_code)`.

The plan-time check remains the refusal before output (D-79); the relative opens make it binding. **Rejected:** path
opens re-checked by `GetFinalPathNameByHandleW` (detects after writing); full-path `OBJ_DONT_REPARSE` (refuses a
destination reached through a link); `cap-std` (a dependency tree for five calls, still needing the audit).

**Symlinks (decision (d))** are created only with `--symlinks` (E3-2's opt-in) and a token holding
`SeCreateSymbolicLinkPrivilege`, which `FSCTL_SET_REPARSE_POINT` requires [FSRP], on a node created relative to the
held parent; else skipped with a warning naming the reason, not today's failure (`crates/lpk-cli/src/lib.rs:728–733`).
Developer Mode alone does not qualify. Nothing is written through a created link (name surrogates below the
destination are refused): the escape 7-Zip fixed in 25.00 [ZDI-11001]. A seam lets CI run both branches; one test
checks the probe against the real token.

**Names.** The colon refusal (`policy.rs:353–355`) keeps entries out of alternate streams (WinRAR's
CVE-2025-8088 [ESET]); NT opens skip Win32's name parsing [NAME], so the trailing-dot and device refusals
(`policy.rs:356–361`) stay the guard. One component per open, so `MAX_PATH` never applies; a component over 255 UTF-16
units, the usual limit [MAXP], is refused before output. Names differing only by case stay refused
(`policy.rs:443–480`), even in case-sensitive folders (Windows 10 17107+, inherited [CS]): Windows applications cannot
tell such files apart [CS].

**TOCTOU test.** A test-only hook between a directory's verification and its child's creation renames the parent and
plants a junction to an outside folder holding a canary, under `--overwrite replace`: (a) with held handles the rename
fails; (b) with a test switch adding `FILE_SHARE_DELETE` the swap succeeds and the file lands in the moved original.
Both assert the outside folder's listing and the canary's bytes unchanged; a red run against today's writer recorded.

## 6. Testing

**CI (`windows-2025`):** the model's table per kind (both OSes), a property test over arbitrary names (no panic,
bounded titles), the COM objects called in process, both targets built; the sparse package registered and removed with
the test certificate (`Get-AppxPackage`); E6-4's `scripts/e6-registry-diff.ps1` (`reg export` of every key §3 touches
and each `FileExts\<ext>\UserChoice`, before registering and after unregistering, diffed; `AssocQueryStringW` [AQS]
for `.lpk`); §4's and §5's tests, foreign archives through E4's worker; a DLL load test on `windows-11-arm` (E7 (g)).
No runner shows an Explorer menu or runs Windows 10 (`docs/notes/e4-design.md:248–250`).

**By hand (E6-7)**, Windows 11 x64 (ARM64 when the owner's device exists) and Windows 10 22H2 x64, per route: the
submenu for a file, a folder, the background, a `.lpk`, a foreign archive, several and mixed selections, each verb
reaching the GUI with its flag (logged); "Show more options"; names with `&` and bidirectional characters; extraction
into a OneDrive folder (§5's cloud tags); install, upgrade, uninstall and any Explorer restart. Screenshots: a local
account `tester`, generic files under `C:\lpk-e6\`, cropped to the window, no taskbar or notifications.

## 7. Order, acceptance, decisions, scope, risks

Order: E6-1 → E6-6 → E6-5 (CLI only; they protect `lpk x` now) → E6-2 (after E5-7) → E6-4 → E6-3 (after E7-1, E7-3) →
E6-7.

| Task | Acceptance additions | `D-NN` records |
|---|---|---|
| E6-1 | lint table, both targets, the grep, the manifest test | the `unsafe` exception; in-process shape |
| E6-6 | §5's tests, the TOCTOU red run, a long path, the privilege seam | parent-handle rule; symlinks |
| E6-5 | PLAN's three cases, nested and derived files; the committed row | rule, content, opt-out, row |
| E6-2 | §2's table per kind, titles, the response file | titles, states, launch |
| E6-4 | the diff empty; `UserChoice` unchanged; route C on 22H2 | routes, associations |
| E6-3 | `Get-AppxPackage` after install and uninstall; the modern-menu screenshot | identity, versioning |
| E6-7 | §6's records | route per OS |

**PLAN edits proposed.** E6-2: "registered through the package manifest, out of process" → "loaded in process from the
package manifest; the work runs in `lpk-gui.exe`". E6-4: "Classic `IContextMenu`" → route C. The outline's
`IAttachmentExecute` → the stream written directly.

**Out of Phase 1.** Drive and desktop menus (`desktop10:ItemType`, 22H2+ [FECM]); right-drag, thumbnail, preview and
property handlers; browsing `.lpk` as a folder; an ARM64X DLL for x64 programs on ARM64; per-verb icons; Linux
`openat2`; foreign "Open with" in the Store package.

**Risks.** (1) Our code runs inside Explorer and other hosts, and can harm them [CMH]; §1's rules bound it. (2)
Registration changes may need an Explorer restart or sign-out [IPF]; each version's DLL gets its own folder, the old
one deleted once unused; the installer never kills Explorer. (3) No modern menu until E7-1's identity and certificate
exist (weeks of validation). (4) Windows 10 22H2's packaged submenu is supported in principle [FECM][WT-MAN] but
unproven with subcommands, and no CI covers it: E6-7 decides its route.

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
[FT]: https://learn.microsoft.com/en-us/windows/win32/shell/fa-file-types
[DP]: https://learn.microsoft.com/en-us/windows/win32/shell/default-programs
[LDA]: https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-default-apps-settings
[AQS]: https://learn.microsoft.com/en-us/windows/win32/api/shlwapi/nf-shlwapi-assocquerystringw
[OFC]: https://learn.microsoft.com/en-us/microsoft-365-apps/security/internet-macros-blocked
[IAE]: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-iattachmentexecute
[ZDI-0411]: https://www.zerodayinitiative.com/advisories/ZDI-25-045/
[ZDI-11001]: https://www.zerodayinitiative.com/advisories/ZDI-25-949/
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
[TR36]: https://www.unicode.org/reports/tr36/
