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
