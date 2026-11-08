# LitePack

LitePack is a Windows-first archiver with its own format, `.lpk`. It aims to make smaller archives than 7-Zip,
WinRAR and WinZip on real user data, in three ways:

- It recompresses files that are already compressed, bit for bit: JPEG photos, and the Deflate streams inside PDF,
  Office, PNG and APK files.
- It finds duplicate and near-duplicate data across files.
- It keeps everything in a modern container, with recovery records, authenticated encryption, integrity checks on
  every chunk, random access, and new generations appended to an existing archive.

It also opens ZIP, 7z, RAR, tar, ISO and other formats in a sandboxed worker process, and exports to ZIP and 7z. It
has a command line, `lpk`, and a Windows GUI with an Explorer context menu.

**Status: in development. There is no release yet.** The results measured so far are on the
[benchmark page](https://charon89.github.io/litepack/bench/results.html).

## Documentation

The documentation site is <https://charon89.github.io/litepack/>. It has:

- the [`lpk` command reference](https://charon89.github.io/litepack/cli/lpk.html);
- the [benchmark results](https://charon89.github.io/litepack/bench/results.html), and the
  [corpus](https://charon89.github.io/litepack/bench/corpus.html) they were measured on;
- the [`.lpk` v1 format specification](https://charon89.github.io/litepack/spec/lpk-v1.html), with
  [conformance notes for independent decoders](https://charon89.github.io/litepack/spec/CONFORMANCE.html);
- the [security model](https://charon89.github.io/litepack/security.html) and the
  [privacy policy](https://charon89.github.io/litepack/privacy.html);
- the [licences and dependency policy](https://charon89.github.io/litepack/licensing.html), and the
  [third-party notices](https://charon89.github.io/litepack/notices.html).

## Building from source

LitePack targets Windows 10 22H2 and later, on x64 and ARM64. The engine and the command line also build on Linux,
where CI tests them.

You need two things:
- **Rust:** run `rustup toolchain install` in the repository to get the version pinned in `rust-toolchain.toml`.
- **On Windows:** Visual Studio 2022 Build Tools, with the C++ workload.

`scripts/setup-windows.ps1` installs both. Then:

```
cargo build --release --workspace
cargo build --release --manifest-path workers/Cargo.toml
```

The first command builds `lpk.exe` and the libarchive worker in `target/release/`. The second builds the Rust worker,
`workers/target/release/lpk-worker.exe`. `lpk` looks for its workers beside itself, so to open other formats, copy
`lpk-worker.exe` into `target/release/`. `.lpk` archives need no worker.

The GUI is built with the Tauri CLI. Install it with `cargo install tauri-cli --version 2.12.4 --locked`, then run
`cargo tauri build --ci -- --locked` in `crates/lpk-gui`.

## Repository layout

| Path | What it holds |
|---|---|
| `crates/` | The Rust workspace: the format (`lpk-format`), the engine (`lpk-core`, `lpk-api`), the command line (`lpk-cli`), the GUI host (`lpk-gui`), the Explorer menu handler (`lpk-shell`), the libarchive worker, an independent decoder (`lpk-check`) and the benchmark harness (`lpk-bench`) |
| `workers/` | The sandboxed Rust worker and its format readers, in a workspace of their own |
| `apps/gui/` | The GUI's HTML, CSS and JavaScript |
| `book/` | The documentation site (mdBook) |
| `docs/` | The plan, the decisions, the status, the format specification, the CLI reference, the security model and the licensing policy |
| `bench/` | The corpus lock file, the committed measurements (`bench/results/`) and the reports |
| `fuzz/` | The fuzz targets |
| `scripts/` | Machine setup, and the checks CI runs |

## How it is developed

- **The plan:** the work follows the ordered task list in [`docs/PLAN.md`](docs/PLAN.md).
- **The decisions:** each one is recorded in [`docs/DECISIONS.md`](docs/DECISIONS.md).
- **The status:** [`docs/STATUS.md`](docs/STATUS.md) says where the work stands.
- **The method:** its sources and the competitor analysis are in
  [`docs/LitePack-Method-v2.md`](docs/LitePack-Method-v2.md).
- **The tools:** LitePack is developed with Claude Code; the working notes are in
  [`docs/CLAUDE-CODE.md`](docs/CLAUDE-CODE.md).

## Security

Please report vulnerabilities privately, as [`SECURITY.md`](SECURITY.md) describes, and never in a public issue.

## Licence

LitePack is licensed under either the Apache License 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE)) or the MIT licence
([`LICENSE-MIT`](LICENSE-MIT)), at your option. The licences of the code it is built from are in
[`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES).
