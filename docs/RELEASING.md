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
  `git log -1 --format=%ct`), fixed MS-DOS attributes, Deflate at level 9 through Python's `zipfile`. A missing input
  fails before anything is written. `python -I scripts/make-zips.py --self-test` builds each zip twice from dummy files
  and compares the bytes.

### The residue check

`scripts/e7-residue.py` (R291; E7-5 extends it with the installer's checks). `snapshot <out>` records every file and
folder under `%APPDATA%` and `%LOCALAPPDATA%` (path, size, modification time) and the keys and values of
`HKCU\Software` (with `Classes`) and `HKCU\Environment` through `reg.exe export`. `diff <before> <after>` prints every
added, removed or changed entry and exits 1 when any is not allowed. Allowed are the entries of its allow-list, which
names Windows' own bookkeeping one folder, key or value at a time with the reason beside each, and, only with
`--allow-worker-profile`, the `LitePack.Worker` profile, matched by its moniker and the SID its `Mappings` key names.
The tool takes a snapshot of the real profile's registry, so it runs on CI's `windows-2025` runner only (R294); its
`--self-test` uses synthetic snapshots.

CI's `portable` job builds the binaries and both zips, takes a snapshot, unzips both into fresh folders under the
runner's temp folder, makes a small `.lpk` and a `.zip` with the portable `lpk.exe`, opens each in the portable GUI
(the smoke report, with `LOCALAPPDATA` and `APPDATA` left as they are), lists and extracts the `.zip` with the CLI
zip's `lpk.exe`, and then checks three diffs: after the runs, the worker profile alone (and it must be there); after
`lpk-worker.exe --remove-profile`, nothing; with `data` replaced by a file, a non-zero exit, the error in the report
and nothing new. The diffs in the job's log are the evidence. The two zips are uploaded as the artifact
`zips-windows-x64`, kept three days.
