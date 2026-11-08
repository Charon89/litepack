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
  recorded in `update.json` in the local state folder before the request, so a failed check waits for tomorrow.
  A newer version shows its notes with **Install and restart**, **Later** (dismiss) and **Skip this version** (stored
  in `update.json`; that version is not offered again, a newer one is). Nothing installs without the first. It runs
  the quit path (asks while an operation runs, then cancels), downloads, and the plugin's `on_before_exit` closes the
  sessions before `install` starts the NSIS installer with `/P /UPDATE` and the process exits. Per user, no UAC.
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
- **Signing `latest.json`.** The owner's offline step (`scripts/updater-sign.sh`) belongs to E7-2.

## Licence checks of the GUI

`cargo deny check` excludes `lpk-gui`, whose tree `crates/lpk-gui/deny.toml` checks for the Windows triples
it ships on: `cargo deny --manifest-path crates/lpk-gui/Cargo.toml check`. `THIRD-PARTY-NOTICES` covers the GUI
(`about-gui.toml`, `about-gui.hbs`, with the WebView2 loader's notice). The release exe must hold no symbol of
the MPL-2.0 crate `option-ext` (only `dirs-sys`'s Unix module uses it): CI greps the PDB.
