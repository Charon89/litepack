# Releasing LitePack

This page grows with the release tasks (E7, E8-8). E5-2 starts it with the GUI's supported Windows versions
and how the GUI is built.

## Supported Windows versions (D-89 (7))

| | Minimum | Build |
|---|---|---|
| Windows 10 | 22H2 | 10.0.19045 |
| Windows 11 | 21H2 and later | 10.0.22000 |

Both on x64 and ARM64. The minimum is stated here, in the CI matrix (the `gui` job of
`.github/workflows/ci.yml` carries `windows-minimum: "10.0.19045"` and fails when this page names another
build), and enforced by E7-4's MSIX (`MinVersion`) and E7-5's installer (which stops with a message below
it). There is no Windows 10 runner on GitHub: CI builds and runs the GUI on `windows-2025`; Windows 10 22H2 is
checked by hand in E5-10.

The GUI needs the WebView2 runtime: Windows 11 ships it; most Windows 10 devices have it already, though Edge itself
does not count as the runtime (Microsoft's distribution guide,
https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution); the NSIS installer checks for the
runtime and runs Microsoft's bootstrapper when it is missing (`tauri.conf.json`, `downloadBootstrapper`;
`docs/notes/e7-design.md` §3).

## Building the GUI

The GUI is a Tauri 2 app with no Node toolchain: static files in `apps/gui/`, the host in `crates/lpk-gui`.

- Install the CLI through cargo, at the pinned version: `cargo install tauri-cli --version 2.12.4 --locked`.
- Build: from `crates/lpk-gui`, `cargo tauri build --ci` writes `target/release/lpk-gui.exe` (bundling is off
  until E7-4 and E7-5).
- Check the built exe: `bash crates/lpk-gui/smoke.sh <lpk.exe> <lpk-gui.exe> <scratch dir>` opens a generated
  archive from the command line and fails unless its listing reached the window.

The icons in `crates/lpk-gui/icons` are a generated placeholder (`icons/make-placeholder.py`, then
`cargo tauri icon`); the final set is the owner's (PLAN E5 decision (e)).

## Licence checks of the GUI

`cargo deny check` excludes `lpk-gui`, whose tree `crates/lpk-gui/deny.toml` checks for the Windows triples
it ships on: `cargo deny --manifest-path crates/lpk-gui/Cargo.toml check`. `THIRD-PARTY-NOTICES` covers the GUI
(`about-gui.toml`, `about-gui.hbs`, with the WebView2 loader's notice). The release exe must hold no symbol of
the MPL-2.0 crate `option-ext` (only `dirs-sys`'s Unix module uses it): CI greps the PDB.

## Artefacts

### The portable zip and the CLI zip (E7-6)

`python -I scripts/make-zips.py --bin target/release --arch x64|arm64 --version <version> --timestamp <seconds>
--out <folder>` writes two zips from the release binaries (`cargo build --release --locked -p lpk-cli -p lpk-check
-p lpk-worker-libarchive`, the `workers/` workspace's `cargo build --release --locked`, and `cargo tauri build --ci`
in `crates/lpk-gui`). They are unsigned until E7-1 to E7-3 land.

- `litepack-<version>-windows-<arch>-portable.zip` holds one top folder, `LitePack/`, with `lpk-gui.exe`, `lpk.exe`,
  `lpk-check.exe`, `lpk-worker.exe`, `lpk-worker-libarchive.exe`, the empty `portable` marker, `README.txt`,
  `THIRD-PARTY-NOTICES`, `LICENSE-APACHE` and `LICENSE-MIT`. It leaves out `lpk_shell.dll`: a portable copy registers
  no shell (R292), and its settings never show the "Open with" controls. With the marker, the GUI keeps its settings,
  recents, journal, opened entries and WebView2's data folder in `data\` beside the executable; when `data\` cannot be
  created or written it says so and exits non-zero (under `LPK_GUI_SMOKE_REPORT`, the error goes to the report instead
  of a message box), never falling back to `%APPDATA%` or `%LOCALAPPDATA%`.
- `litepack-<version>-windows-<arch>-cli.zip` holds the same files without `lpk-gui.exe` and the marker, with its own
  `README.txt` (no settings, no network).
- Both READMEs name the one footprint outside the folder: the `LitePack.Worker` AppContainer profile, created when a
  foreign archive is first opened and removed by `lpk-worker.exe --remove-profile`.
- Both are deterministic (R293): entries sorted, one timestamp for all of them (CI passes the commit's,
  `git log -1 --format=%ct`), fixed MS-DOS attributes, Deflate at level 9 through Python's `zipfile`. The bytes are
  identical for one Python build only, since its zlib and its `zipfile` shape them: CI pins Python 3.13.7 through
  `actions/setup-python`. A missing input fails before anything is written; both zips are written to temporary names
  and then renamed together, the old pair put back if a rename fails, so the folder never holds one old and one new
  zip. `python -I scripts/make-zips.py --self-test` builds each zip twice from dummy files and compares the bytes.

### The residue check

`scripts/e7-residue.py` (R291; E7-5 extends it with the installer's checks). `snapshot <out>` records every file,
folder and reparse point (junction or link, recorded and never followed) under `%APPDATA%` and `%LOCALAPPDATA%` (path,
size, modification time; walked through `\\?\` paths, so length never matters) and the keys and values of
`HKCU\Software`, `HKCU\Software\Classes` (the user's classes hive, exported on its own as well; the snapshot refuses
unless `Classes\Local Settings` is in it) and `HKCU\Environment` through `reg.exe export`. Every path it cannot read is
printed. **A snapshot records the user profile's registry**: it refuses to run unless `GITHUB_ACTIONS=true` or
`--on-ci` is given, and `--on-ci` is only for a disposable machine. LitePack's CI runs it on `windows-2025` only (R294);
the `--self-test` uses synthetic snapshots.

`diff <before> <after>` prints every added, removed or changed entry, and every path unreadable in either snapshot,
and exits 1 when any is not allowed. Allowed are the entries of its allow-list, which names Windows' own bookkeeping
one folder, key or value at a time with the reason beside each. That includes the AppContainer parent keys
(`AppContainer`, its `Mappings` and `Storage`) only when they are left empty (R301). With `--allow-worker-profile`, the
`LitePack.Worker` profile is allowed too, matched by its moniker and the SID its `Mappings` key names. With
`--require-worker-profile`, the profile is allowed, and the diff fails unless both its folder under
`%LOCALAPPDATA%\Packages` and its `Mappings` key were added. Key and value names are compared without case.

Windows and the WebView2 runtime keep a few records of their own, as they do for any app; LitePack's own data stays
in `data\`. The allow-list names them one at a time (R313, from the first full diff, CI run 42483731626):
- Records written while the GUI starts, by Windows' account broker (WAM), most likely for the WebView2 runtime (R315;
  their times fall inside the GUI step, and the CLI runs add none): its token cache
  (`%LOCALAPPDATA%\Microsoft\TokenBroker\Cache`, files of the shape `<40 hex>.tbres` directly in it), its credential
  file (`%LOCALAPPDATA%\Microsoft\Credentials`, names of 32 hex digits directly in it), the broker app's `PCT` and
  `PTT` values and the `AuthCookies\Live\Default\DIDC` `Data` value. So that the credential pattern cannot hide one
  of LitePack's own, the job fails if `cmdkey /list` shows a target containing `LitePack` or `lpk` after any run;
  any credential LitePack ever stores must use a target named after LitePack.
- The shell's display-string cache (MuiCache): `elscore.dll`'s strings, and the `Shell\MuiCache` key with
  `lpk-gui.exe`'s `ApplicationCompany` and `FriendlyAppName`, only under the run folder's path (`diff --run-folder`,
  which the job passes; by default `RUNNER_TEMP\portable-run\LitePack`), and `LangID`.
- Explorer's UserAssist session counter and its Windows Terminal entry, which change on the runner on their own.
- The WebView2 runtime's per-user records: the subtrees `HKCU\Software\Microsoft\EdgeWebView` and
  `HKCU\Software\Microsoft\EdgeUpdate\ClientState\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}` (the runtime's GUID), and
  nothing wider.

CI's `portable` job builds the binaries and both zips and takes a snapshot. It unzips both zips into fresh folders
under the runner's temp folder and makes a small `.lpk` and a `.zip` with the portable `lpk.exe`. It opens each in the
portable GUI (the smoke report, with `LOCALAPPDATA` and `APPDATA` left as they are), then requires the profile and
nothing else. It runs `lpk-worker.exe --remove-profile` and requires nothing left. It lists and extracts the `.zip`
with the CLI zip's `lpk.exe`, requires the profile again, removes it, and requires nothing left. Finally, with `data`
replaced by a file, it expects a non-zero exit, the error in the report and nothing new. The diffs in the job's log are
the evidence. The two zips are uploaded as the artifact
`zips-windows-x64`, kept three days.

E7-5 adds the desktop (`%USERPROFILE%\Desktop`, recorded as `%DESKTOP%`, for shortcuts; the Start menu is under
`%APPDATA%`) and `--allow-settings`: `%APPDATA%\LitePack\settings.json` and its folder, added and holding nothing
else, which an uninstall without the "delete the application data" box keeps (`docs/notes/e7-design.md` section 3).
Nothing else is allowed by it.

### The NSIS installer (E7-5)

The per-user installer is built through Tauri, on CI only: `crates/lpk-gui/tauri.nsis.conf.json` is a `--config`
overlay (a JSON merge patch over `tauri.conf.json`; tauri-cli 2.12.4 also hands it to the build in `TAURI_CONFIG`), so
the default `cargo tauri build` stays unbundled and never downloads NSIS:
`cargo tauri build --ci --config tauri.nsis.conf.json -- --locked` in `crates/lpk-gui`, after the release binaries of
`lpk-cli`, `lpk-check`, `lpk-worker-libarchive`, `lpk-shell` and the `workers/` workspace. It is unsigned until E7-1
to E7-3 (D-160). Part 2 (E6-3, E7-3) adds the sparse package; part 1 registers the shell through route C.

**Per user, no UAC.** `installMode` `currentUser`: `%LOCALAPPDATA%\LitePack`. The GUI's local state is in
`%LOCALAPPDATA%\org.litepack.gui`, the settings in `%APPDATA%\LitePack`.

**Layout.** `lpk-gui.exe` and `uninstall.exe` at the root; `bin\` with `lpk.exe`, `lpk-check.exe`, `lpk-worker.exe`
and `lpk-worker-libarchive.exe` (the GUI starts its workers from `bin\` when `bin\lpk-worker.exe` exists, R307; only
`bin\` goes on `Path`); `shell\<version>\lpk_shell.dll` (a DLL Explorer has loaded cannot be replaced, so each version
has its own folder, R308; the overlay's folder name is tested against `tauri.conf.json`'s version);
`THIRD-PARTY-NOTICES`, `LICENSE-APACHE`, `LICENSE-MIT`.

**Hooks** (`crates/lpk-gui/installer.nsh`):
- The refusals: below build 19045, a message naming Windows 10 22H2; the x64 installer on an ARM64 PC, a message
  naming the ARM64 build; and (R336) an install folder whose path holds `%` or `!`, which the next-logon cleanup's
  cmd line would expand. Each exits with error level 1 before any file is written and before an old version is
  touched: in the GUI-init function (`MUI_CUSTOMFUNCTION_GUIINIT`, before the first page, so before the reinstall
  page can run the old uninstaller) and in a hidden section the hooks file defines ahead of the template's, which
  runs in silent and interactive installs alike after the folder is chosen, before the WebView2 section (which could
  download Microsoft's bootstrapper) and before `SetOutPath` creates the install folder. PREINSTALL repeats them and
  then removes the install folder only when it is empty. The template sets no `ManifestSupportedOS`, so NSIS's
  default (every version up to Windows 10/11) is in the manifest and `${AtLeastBuild}` sees the real build. CI has no
  Windows 10 runner: the version refusal is checked by hand on a Windows 10 VM in E7-10; the ARM64 refusal runs on
  `windows-11-arm`. (`--register-shell` also refuses `%` and `;`, later.)
- PREINSTALL then runs the "close LitePack" check (as the uninstaller does, below), and sets `bin\`'s executables
  aside (R334, R336): Windows refuses to overwrite a running executable (an `lpk.exe` compressing during an upgrade;
  NSIS's `File` would then skip it in a silent install, leaving a half-updated `bin\`) but lets it be renamed. Each
  existing `bin\<name>.exe` is renamed to `<name>.exe.old-<n>` (the first free `n`) before any file is written. If a
  rename fails, the others are renamed back (each checked; a failed one is named in the message) and the installer
  exits with error level 2. The set-aside copies are deleted only in POSTINSTALL, once all four new executables
  exist; if one is missing (its `File` failed), its copy is renamed back and the installer exits with error level 2
  naming it: the GUI and the Uninstall key are then the new version while that file is the old one (mixed versions;
  reinstall to repair), and that exit level 2 is kept even if `--register-shell` then fails (it would set 3). An
  executable the uninstaller cannot rename aside is named in its log. A held copy stays until the next install or the uninstall deletes it (`bin\lpk*.exe.old-*`, a pattern
  no user file of a `/D=` folder's `bin\` is expected to match).
- PREINSTALL also cancels every `LitePack *` `RunOnce` value still pending from an earlier uninstall or upgrade, so
  none can touch a reinstall made before the next logon.
- POSTINSTALL: the set-aside copies (above), then `lpk-gui.exe --register-shell` (route C, the `.lpk` association,
  `GuiPath`, `bin\` on `Path`; docs/API.md), a `WM_SETTINGCHANGE` broadcast for `Environment` (NSIS's `SendMessage`
  with a 5-second timeout), and the removal of other versions' `shell\` folders.
- PREUNINSTALL, skipped under `/UPDATE`: the "close LitePack" check first (a cancelled prompt leaves LitePack installed
  and registered), then `--unregister-shell`, the broadcast, `bin\lpk-worker.exe --remove-profile`, and (R336) each
  `bin\` executable that cannot be deleted because it still runs is renamed aside as at an upgrade.
- POSTUNINSTALL, skipped under `/UPDATE`: `%LOCALAPPDATA%\org.litepack.gui` always; `%APPDATA%\LitePack` only with the
  box; Tauri's bookkeeping (`HKCU\Software\litepack\LitePack`, its `Installer Language`) always. The registry compares
  names without case, so that key is `Software\LitePack\LitePack`, under LitePack's own `Software\LitePack`: it is
  deleted, then `Software\LitePack` only when empty.
- The next-logon cleanup (R329, R335, R336). What a running process still holds is removed at the next logon by a
  per-user `RunOnce` value. Windows runs a Run/RunOnce command of at most 260 characters and documents neither the
  order of the values nor that one finishes before the next starts, so each cleanup is one value whose single cmd
  line (`cmd /d /v:off /c`) changes to the install folder once and runs its steps in sequence with relative paths;
  the design does not depend on the order of values. `LitePack uninstall` (after an uninstall: this version's shell
  DLL Explorer holds, set-aside executables still running): `cd /d "<install>" && (del /f /q
  shell\<version>\lpk_shell.dll & del /f /q "bin\lpk.exe.old-1" & ... & rmdir shell\<version> & rmdir shell & rmdir
  bin & cd .. & rmdir "<folder name>")`, one `del` per file that is there, by its exact name (R343: given several
  names, `del` stopped at a missing one and deleted none, CI run 42944220039); only when the names would pass 260
  characters is the set-aside group one `del /f /q bin\lpk*.exe.old-*`. `LitePack shell <version>` (after an upgrade: an old version's DLL Explorer holds): the DLL, then
  `rmdir shell\<version>`. Never recursively, so nothing else in a `/D=` folder can go; nothing runs if the folder is
  gone. The whole stored string is measured. The budget (R338): `LitePack uninstall` has a fixed part of 184
  characters at version 0.0.1, counting `cmd.exe`'s path and the install folder's last name `LitePack`, so the
  install path may be at most 76 characters; the version appears twice, so each character it has beyond `0.0.1`
  takes two from the path (a version like `1.10.0-beta.1` leaves 60). CI's path (43 characters) gives 227. Over 260,
  the value is retried with the install folder's short (8.3) path (`GetFullPathName /SHORT`); an install path long
  enough to push even that past 260, or a volume without short names, gets no value at all, and what is left stays until the next install, whose cleanup deletes `bin\lpk*.exe.old-*` and other
  versions' `shell\` folders (after an uninstall with no later install, it stays). CI holds the DLL open during an
  uninstall and runs an uninstall while `bin\lpk.exe` compresses; it lists the values with `scripts/e7-runonce.py`,
  requires each to be at most 260 characters, runs them as the next logon would (in a seeded random order), requires
  the install folder gone, and checks that a reinstall cancels them.

**Switches.** Tauri's: `/S` silent (installer and uninstaller), `/P` passive (a progress bar), `/NS` no shortcuts,
`/D=<dir>` (last and unquoted), `/R` relaunch after a silent or passive install; `/UPDATE` and `/ARGS` are the
updater's (under `/UPDATE` the old uninstaller does not run, the new files overwrite the old). LitePack's own:
`/DELETEAPPDATA` on the uninstaller sets what the confirm page's "delete the application data" box sets
(`$DeleteAppDataCheckboxState`, R309), so a silent uninstall can remove the settings.

**The CI job `installer`** (`windows-2025`) builds the installer at the workspace's version N and at N+1 (an overlay
with `version` 0.0.2 and `shell/0.0.2`), snapshots, installs N with `/S` and checks the layout, the notices, the
Uninstall key, `Path` and route C (`scripts/e7-installer-check.py`, read-only), opens a `.lpk` and a `.zip` in the
installed GUI (the profile required), runs `bin\lpk.exe` on the `.zip`, installs N+1 with `/S` while `bin\lpk.exe`
compresses about 256 MiB of generated text with `--balanced --threads 1` (the step fails if it has exited before the
upgrade begins; it then requires exit code 0, every installed binary N+1's by hash, the running one set aside as
`lpk.exe.old-1` while it still runs when the upgrade ends, and the running `lpk.exe`'s own exit code 0 and an
archive that `lpk t` accepts; the `.sha256` files are read by `scripts/e7-sha256.py`, which accepts Git Bash's `*`
marker and fails loudly on a missing name), checks N+1's registration and a single `Path` entry, uninstalls with
`/S`, and diffs
against the first snapshot with `--allow-settings` and the profile not allowed. A second install and uninstall with
`/DELETEAPPDATA` requires `%APPDATA%\LitePack` gone and nothing left but Windows' entries and the copy below.
`--run-folder` names the install folder, so the entries keyed by `lpk-gui.exe`'s path (MuiCache, and Explorer's
UserAssist record, ROT13 as Explorer stores it) match only there.

The uninstaller also removes the jump list Windows keeps for LitePack's AppUserModelID (`org.litepack.gui`):
`%APPDATA%\Microsoft\Windows\Recent\AutomaticDestinations\a233dcd0c3d36a7d.automaticDestinations-ms` (the name is the
CRC-64 of the upper-cased ID; a test checks it against `tauri.conf.json`), its `CustomDestinations` twin, and the
`org.litepack.gui` value of `HKCU\...\CurrentVersion\Search\JumplistData`.

**The plugin's crates (R272, R325).** CI's `notices pins` job checks that the crates compiled into Tauri's NSIS
plugin `nsis_tauri_utils.dll` are in `THIRD-PARTY-NOTICES` under their licences. The plugin's release tag publishes no
`Cargo.lock`, so the job takes the tag's source archive (SHA-256 pinned in `notices/nsis/pin.toml`) and resolves its
workspace with `cargo metadata --filter-platform i686-pc-windows-msvc` (nothing is built or run), following the
normal dependencies of `nsis_tauri_utils` and never a proc-macro crate. This resolves **today's** versions from
crates.io, not necessarily the ones the DLL was built with: crate names and licences rarely change, versions may.

**Known limitation (R312).** A silent uninstall leaves `%LOCALAPPDATA%\Temp\~nsu<n>.tmp\Un.exe`: NSIS runs the
uninstaller from that copy so that it can remove the install folder, and only a reboot can delete the copy, which a
per-user uninstaller cannot schedule (it would need `MoveFileEx` with `MOVEFILE_DELAY_UNTIL_REBOOT`, an administrator's
right). The residue check allows exactly that (`--allow-uninstaller-copy`: a `~nsu*.tmp` folder holding one `Un.exe`
or `Un_<letter>.exe` and nothing else); Windows' own temporary-folder cleanup removes it later.
