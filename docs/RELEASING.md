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

## The updater (E7-7)

`tauri-plugin-updater` =2.14.1, called from Rust alone (`crates/lpk-gui/src/updates.rs`; the plain-Rust parts in
`crates/lpk-gui/src/update.rs`). Design: `docs/notes/e7-design.md` section 6.

- **Endpoint and key.** `tauri.conf.json`'s `plugins.updater`: one HTTPS endpoint,
  `https://github.com/Charon89/litepack/releases/latest/download/latest.json`, `requireSignedVersion: true`,
  `windows.installMode: passive`, no `allowDowngrades` and no `dangerous*` option (a config test asserts all of
  this). The `pubkey` stays a placeholder until E7-1 supplies the owner's minisign public key; until then the gate
  makes no request at all.
- **Flow.** With **Check for updates** on, the check runs once the window is up, at most once a day: the day is
  recorded in `update.json` in the local state folder before the request, so a failed check waits for tomorrow;
  when the day cannot be recorded (no state folder, a failed write) no check is made. The day is the UTC day of the
  system clock, so a clock set back (or forward) to another day can check again that day.
  A newer version shows its notes with **Install and restart**, **Later** (dismiss) and **Skip this version** (the
  version the check found, never one the page names, is stored in `update.json`; it is not offered again, a newer one
  is). Nothing installs without the first, which runs in this order (`updates::install_flow`, R323): the download
  and its verification first, so a failed or timed-out download leaves the GUI as it was; then the quit path (asks
  while an operation runs, then cancels; if the user keeps the operation, nothing installs and nothing is
  cancelled); then the plugin's `on_before_exit` closes the sessions and `install` starts the NSIS installer with
  `/P /UPDATE`, plus `/R` (restart) only when LitePack was started without arguments, since `/R` passes the launch's
  arguments on (`/ARGS`) and would replay a verb such as `--extract-here`; after a launch with arguments the user
  starts LitePack again. Then the process exits. If the installer does not start once the quit path has run, the GUI
  says so and exits rather than stay half-closed. Per user, no UAC.
  With the setting off, nothing is requested and `update.json` is not written.
- **The release prefix.** Before any download, a `url` outside `https://github.com/Charon89/litepack/releases/download/`
  is refused (`lpk_gui::update::RELEASE_DOWNLOAD_PREFIX`, built from the one `REPOSITORY` constant). The comparison is
  byte for byte on the parsed URL: no case folding of the path, and nothing but a path may follow the prefix.
- **Verification.** The plugin checks the downloaded installer's minisign signature against the compiled-in key, and
  with `requireSignedVersion` that the signature's trusted comment names the announced version (CVE-2026-95625).
- **Timeouts.** The check: 30 s (`CHECK_TIMEOUT`). The download: 10 minutes for the whole installer
  (`DOWNLOAD_TIMEOUT`).
- **Portable copies** (the `portable` marker) only notify: the notice links to
  `https://github.com/Charon89/litepack/releases/latest`; they never download or install, since the installer would
  turn the portable copy into an installed one.
- **The Store build** (E7-4) turns the `updater` cargo feature off (`--no-default-features`): its tree has no
  `tauri-plugin-updater` (`crates/lpk-gui/tests/updater_feature.rs` runs `cargo tree`), and the GUI shows no update
  setting. The Store updates it.
- **Test keys.** The tests (`updates.rs`) generate a minisign key pair at test time with the `minisign` dev-dependency
  (MIT), sign their manifests and payloads in memory, and serve them from a local server on `127.0.0.1` with plain
  HTTP allowed by the test configuration alone. No key, public or secret, of the tests is committed, so E7-1's
  `git log --all -S` for the secret-key header finds nothing. The tests cover: a correctly signed newer release
  verifies; a bad signature in the manifest, a tampered payload, and a validly signed older release announced as a
  newer version are each refused; a `url` off the release prefix is refused before any download (the server sees
  only `latest.json`); with the check off, the server sees no request; and the check runs once a day and does not
  offer a skipped version.
- **End to end (test 6, CI job `updater-e2e`).** Two test installers, N (0.0.1) and N+1 (0.0.2), built with the
  `update-e2e` feature and `tauri.update-e2e.conf.json` chained after `tauri.nsis.conf.json` (tauri-cli 2.12.4 takes
  `--config` more than once), hold a key pair the job generates with `tauri signer generate` and deletes at its end.
  N+1 is signed with `tauri signer sign --app-version 0.0.2` and served with `latest.json` from a Python server on
  `127.0.0.1:8737`. A payload with one byte flipped is refused first (nothing installs); then the hook
  (`LPK_GUI_UPDATE_E2E`) presses Install, and the job requires the steps check, download, verified, quit, install in
  that order, N+1's binaries, the registration and `Path`, the settings kept, and no residue after the uninstall.
  No shipped build holds the hook or the overlay: the `gui` job greps the release exe.
- **Startup after the update (R342).** tauri-bundler patches the bundle type into the packed `lpk-gui.exe`, so the job
  compares its hash with the token turned back (`scripts/e7-bundled-hash.py`), and it tracks the installer by process
  audit (started by `lpk-gui.exe` with `/P /UPDATE`, exit 0). Before the hand-off the GUI destroys its window (bounded,
  1.5 s), so its WebView2 releases the data folder; the new GUI retries its webview every 500 ms for 10 s, and a
  setup failure shows "LitePack could not start" and exits 1 instead of Tauri's panic (exit 101). When the data folder
  stays in use for 2 s (the browser process of a LitePack that ended abruptly still holds it), the GUI uses a second
  folder beside it, `WebView2-2` in the same local state folder, alternating between the two while both are in use
  (R362); a later start whose webview opens on the first folder removes the second in the background (R364). A panic,
  a failed start or a failed hand-off to the installer (R348: the process is held until the installer
  starts) appends a bounded local note to `crash.log` in the local state folder (portable: `data\`): the error, which can name a file
  path, and where in the code it happened; the last 8 at most, under 64 KiB; nothing is sent, and the uninstaller
  removes the folder. Test 6 fails on such a note, and on any non-zero exit of the installed `lpk-gui.exe` (4689).
- **Signing `latest.json`.** The owner's offline step (`scripts/updater-sign.sh`) belongs to E7-2.

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
  foreign archive is first opened and removed by `lpk-worker.exe --remove-profile`; the portable README also names
  the GUI's About box button "Remove the sandbox profile" (Settings, then About LitePack), which calls the same
  delete in process and is refused while an operation runs.
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
- Explorer's own icon and thumbnail caches, `%LOCALAPPDATA%\Microsoft\Windows\Explorer\iconcache_*.db` and
  `thumbcache_*.db`, changed only (never added), which it rewrites when it refreshes icons.
- Windows' cache of the display strings it loads from its own DLLs (MuiCache), filled when a program starts: the
  values of `Local Settings\MuiCache\<hex>\52C64B7E` (any hex subkey) named `@%SystemRoot%\System32\<name>.dll,-<n>`
  or `C:\Windows\system32,@<name>.dll,-<n>`, never a name pointing anywhere else (`urlmon.dll`, `elscore.dll`).
- The shell's display-string cache (MuiCache): the `Shell\MuiCache` key with
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
  gone. The whole stored string is measured (R338, R343). Its fixed part counts `cmd.exe`'s path, the install
  folder's last name `LitePack`, the version (written twice) and, in the exact-name form, each set-aside file's
  name, so a longer version or more executables still running leave less room for the install path; the wildcard
  form's fixed part does not grow with the number of files. Over 260,
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

## The release workflow (E7-2, part 1)

`.github/workflows/release.yml` builds what a release would ship, without the signing identity (E7-1, E7-3) and
without publishing anything. Its permissions are `contents: read`, and it uses no deployment target, no stored
credential, no OIDC token and no cache. Every action is pinned by its 40-hex commit SHA, with the tag in a comment.

**Triggers.** A pull request or a push to `main` that touches the workflow, `scripts/repro-build.sh`,
`scripts/make-zips.py`, `scripts/release-tools.env`, `scripts/cargo-auditable.cmd`, `scripts/e7-*`, `crates/lpk-gui/installer*.nsh`,
`crates/lpk-gui/tauri*.conf.json` or `rust-toolchain.toml`; and `workflow_dispatch`.

**Jobs.**
- `preflight` (ubuntu-24.04): `scripts/e7-preflight.py version` (Cargo.toml, `workers/Cargo.toml`,
  `tauri.conf.json` and the overlay's `shell/<v>/` agree; it prints `releasable: no (unsigned; the updater key is a
  placeholder)`), then `e7-preflight.py workflow` on this workflow (SHA pins, permissions, no deployment target, no
  credential, no OIDC). Outputs the version and the commit time.
- `tools` (windows-2025): `cargo install --locked --root` of tauri-cli, cargo-auditable and cargo-cyclonedx at the
  versions in `scripts/release-tools.env`, uploaded with `tools.sha256`; the list's own hash is the job's output.
- `build-x64`, `build-arm64` (windows-2025): `scripts/e7-release-build.sh <arch> <out>` runs `repro-build.sh
  --auditable --gui` for the target (x64 also runs `check-worker-symbols.sh`) and writes `unsigned.sha256` for the six
  PEs. Each job checks the tools against their list and the list against `tools`' output first. Then (part 1d) it runs
  `cargo cyclonedx --describe binaries --format json --spec-version 1.5 --target <triple>` three times, with
  `SOURCE_DATE_EPOCH` the commit time (so no serial number and a fixed timestamp): in the root workspace with its
  default features, for `lpk-gui` with the features tauri-cli builds it with (`--no-default-features --features
  "updater tauri/custom-protocol"`; the script stops if the GUI's default features change), and in `workers/` with its
  default features (never `faults`). cargo-cyclonedx has no `--locked`, so the script then requires `Cargo.lock` and
  `workers/Cargo.lock` unchanged. `scripts/e7-sboms.py collect` keeps one SBOM per shipped binary as
  `sboms/<binary>.cdx.json` (`lpk`, `lpk-check`, `lpk-worker`, `lpk-worker-libarchive`, `lpk_shell`, `lpk-gui`),
  leaves the unshipped binaries' out by name, and fails on a missing SBOM, an extra one (of any other binary, or a
  shipped one twice), or one that is not CycloneDX JSON of spec 1.5 whose `metadata.component` names its binary and
  whose target-triple property is the job's. `sboms.sha256` lists the six; its hash is the job's second output
  (`unsigned.sha256` stays the list of the six PEs that the later jobs read line by line).
- `package` (windows-2025): checks every download against its list and every list against its job's output. For each
  architecture it copies the built GUI to `target\<triple>\release\`, writes the overlay with
  `scripts/e7-nsis-overlay.py`, and runs `cargo tauri bundle --ci -v --bundles nsis --target <triple>
  --no-binary-patching --no-sign --config <overlay>`. It fails without makensis's "Processing script file" or with a
  warning 6000. Then 7-Zip lists and unpacks each installer, and every shipped PE inside must equal the build job's
  (`lpk-gui.exe` exactly once): this is the `--no-binary-patching` check. `e7-artefacts.py nsis-parts` then lists
  every PE of the NSIS installation the bundler used (`%LOCALAPPDATA%\tauri\NSIS`, found by content, paths relative
  to it) in `nsis-parts.sha256`. It uploads `installers`: the two setups and `nsis-parts.sha256` with
  `installers.sha256` over all three.
- `refuse-arm64-on-x64` (windows-2025): the ARM64 installer run with `/S` exits non-zero within 300 s and writes
  nothing under `%LOCALAPPDATA%\LitePack`.
- `install-arm64` (windows-11-arm): the ARM64 installer run with `/S` installs. Each installed PE is machine 0xAA64
  (the COFF Machine field read with `od`, as `arm64-smoke` does: the job has no Python) and byte-equal to the build's. `bin\lpk.exe --version` runs, and `bin\lpk.exe`
  round-trips a small folder. A silent uninstall (waited on with `e7-wait-uninstaller.sh`) leaves no install folder.
- `assemble` (windows-2025, part 1c): checks `unsigned-x64`, `unsigned-arm64` (their `unsigned.sha256` and
  `sboms.sha256`) and `installers` against their lists and the lists against the build and `package` jobs' outputs.
  It runs the self-tests of `make-zips.py`, `scripts/e7-sboms.py`, `scripts/e7-assets.py` and
  `scripts/e7-artefacts.py`, then `make-zips.py` per architecture (`--bin` the build job's artifact, the version and
  the commit time from `preflight`) and copies the two installers beside the zips. `e7-sboms.py pack` writes the SBOM
  zip from both build jobs' `sboms/` (part 1d) and `e7-sboms.py check` reads it back. `e7-assets.py sums` writes
  `SHA256SUMS` over exactly the seven expected assets and `check` reads it back. Both `e7-artefacts.py` commands take
  `--version` and refuse a folder holding anything but those seven and `SHA256SUMS`, and then look inside the six
  artefacts only (`e7-assets.py`'s `artefacts`): the SBOM zip holds no PE and no notices.
  `e7-artefacts.py notices` is E7-8 part 2's finder: every artefact holds `THIRD-PARTY-NOTICES`, `LICENSE-APACHE`
  and `LICENSE-MIT` exactly once each, at its top folder (`LitePack/` in a zip, the root in an installer), byte-equal
  to the repository's. Names are compared without regard to case or separator, so a second copy anywhere fails (the
  zips read with `zipfile`, the installers through the runner's 7-Zip). And `e7-artefacts.py pes`: every PE inside
  each artefact is classified. A `shipped` PE is one of the six names, at the artefact's machine (0x8664 or 0xAA64),
  carries `.dep-v0` and is byte-equal to the build's, each name the artefact should hold exactly once. An
  `nsis-plugin` is an x86 (0x14C) PE directly under `$PLUGINSDIR` in an installer, each name at most once, byte-equal
  to a file of that name in `nsis-parts.sha256`. The `nsis-uninstaller` is the `uninstall.exe` NSIS writes at install
  time (x86, at the installer's root, exactly one), which 7-Zip rebuilds from this installer's own stub, its bytes
  `[0, S)` up to its first header (S the first 512-byte boundary with NSIS's `EF BE AD DE` `NullsoftInst`, at least
  0x200, the stub an MZ PE). It must carry its own first header at the same S with NSIS's `FH_FLAGS_UNINSTALL` bit set
  (clear in the installer's), have the installer's section table, and equal the installer's `[0, S)` everywhere but
  inside `.rsrc` (the uninstaller's icon is patched in there; R391) and the PE `CheckSum` field. Anything else fails.
  It prints each PE with its class, machine and SHA-256, both flags words, one line per part of the stub where the
  uninstaller differs (its count of ranges and bytes, its first and last range) and every range outside `.rsrc` and
  the `CheckSum` in full (R393), a count per class, and 7-Zip's full listing of each installer. The job uploads
  `release-assets` (the seven assets and `SHA256SUMS`, kept 14 days) and outputs the SHA-256 of `SHA256SUMS`.
- `smoke-x64` (windows-2025) and `smoke-arm64` (windows-11-arm): `SHA256SUMS` checked against `assemble`'s output,
  then `sha256sum -c`. The CLI and portable zips of the runner's architecture are unzipped with Windows' own
  `tar.exe`. From the CLI zip: each PE's machine is the runner's (`e7-pe-sections.py --machine` on x64, `od` on ARM64,
  which has no Python), `lpk.exe --version` names the version, a small folder round-trips through `a` and `x` (each
  file compared with `cmp`), `lpk-check.exe verify` passes the archive, and `lpk export` writes a `.zip` that `lpk x`
  extracts through the sandboxed worker, compared again. From the portable zip, the GUI opens the `.lpk` and the
  `.zip` under `LPK_GUI_SMOKE_REPORT`, each report with no problems and rows rendered (as `ci.yml`'s `portable` job),
  then `lpk-worker.exe --remove-profile` removes the worker's profile.

**The assets.** For version `<v>`: `LitePack_<v>_x64-setup.exe` and `LitePack_<v>_arm64-setup.exe` (the bundler's
names, from `tauri.conf.json`'s `productName`), `litepack-<v>-windows-x64-portable.zip`,
`litepack-<v>-windows-x64-cli.zip`, `litepack-<v>-windows-arm64-portable.zip` and
`litepack-<v>-windows-arm64-cli.zip` (`make-zips.py`'s), `litepack-<v>-sbom.zip` (part 1d, `e7-sboms.py`'s), and
`SHA256SUMS` (`sha256sum`'s text format, `<hex>  <name>`, LF line ends, sorted by name).
`python -I scripts/e7-assets.py expected --version <v>` prints the seven names; its self-test checks them against
`make-zips.py`, `e7-sboms.py` and `productName`. A user checks a download with `sha256sum -c SHA256SUMS
--ignore-missing`.

**The SBOM asset.** `litepack-<v>-sbom.zip` holds the folders `x64/` and `arm64/`, each with one CycloneDX 1.5 JSON
SBOM per shipped binary of that architecture: `lpk.cdx.json`, `lpk-check.cdx.json`, `lpk-worker.cdx.json`,
`lpk-worker-libarchive.cdx.json`, `lpk_shell.cdx.json` and `lpk-gui.cdx.json`, twelve in all. Each is cargo-cyclonedx's
output unchanged (`--describe binaries`, the architecture's `--target`, the build's features and lockfiles): the
binary as `metadata.component`, every crate it depends on for that target as a component with its version, licence
and package URL, and the dependency graph. The zip is deterministic as the other zips are (sorted entries, the commit
time, fixed attributes). Until part 2, the SBOMs describe the unsigned build: part 2 attests them with the signed
artefacts (`docs/notes/e7-design.md` section 1). Each SBOM's `bom-ref`s carry the path of the checkout cargo-cyclonedx
ran in, as it writes them.

**The ARM64 installer.** The hooks cannot see the template's `${ARCH}`, so the ARM64 overlay names
`crates/lpk-gui/installer-arm64.nsh` as its hooks file. That file defines `LPK_INSTALLER_ARCH` as `arm64` and includes
`installer.nsh`, whose own definition (`x64`) applies only when nothing is defined. The ARM64 installer refuses on
any PC that is not ARM64. The x64 installer refuses on ARM64, as before, and on 32-bit Windows.

**Unsigned, and deferred to part 2.** Every PE and both installers are unsigned. Part 2 adds signing (with
`signtool verify`), its deployment target, attestation, the draft release and the tag trigger, the updater's
`latest.json` signature, and the proof that the bundler downloads nothing (here it may fetch NSIS itself). The later
steps of part 1 add the rest: part 1d the SBOMs (E8-6's half; done, above), and part 1e the install checks on both architectures (a silent install of each
architecture's installer, its layout and notices, the GUI and `bin\lpk.exe` on a `.lpk` and a `.zip`, a silent
uninstall and the residue diff). Part 2 then signs the PEs and both installers, and `assemble` checks the
artefacts against the signed files instead (`docs/notes/e7-design.md` section 1). Signing also changes the bytes of
the NSIS parts inside each installer: when it can sign, tauri-bundler 2.10.4 signs its own copies of the
`Plugins/x86-unicode` plugins before makensis packs them, and the stub the uninstaller is rebuilt from is the signed
installer's. So `nsis-parts` must then hash the signed plugin copies of each architecture's bundle, not the NSIS
cache, and the uninstaller rule (its stub equal to the installer's apart from `.rsrc` and `CheckSum`) must be
confirmed again on a signed installer.

**A rehearsal.** Once the workflow is on `main`: `gh workflow run release.yml --ref main`, then
`gh run list --workflow release.yml` and `gh run view <id>`. The artifacts (`unsigned-x64`, `unsigned-arm64`,
`installers`) are kept for 7 days, `release-assets` for 14. They are unsigned test builds, not releases.
