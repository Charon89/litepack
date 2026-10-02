# Baseline tools

The Phase 0 runner (`lpk-bench run`, PLAN P0-3) compares LitePack with the archivers below. The
committed catalogue is `bench/tools.toml`; it holds no machine-specific data. Each results
directory `bench/results/<date>-<host>/` records, in `tools.json`, which tools were found on that
machine, their versions, and why any were skipped.

Check what the runner sees on this machine:

```text
cargo run -p lpk-bench -- run --list-tools [--tools 7z,xz]
```

One line per tool: id, `found` with version and path, or `skipped: <reason>`. Reasons are
`not installed` and `manual (see docs/BASELINES.md)`. The path is printed for you only; it is
never written to a tracked file or a result file.

Check a results directory against `bench/results/schema.json`:

```text
cargo run -p lpk-bench -- run --validate bench/results/<date>-<host>
```

## How a tool is found

1. The `path` of its entry in `bench/tools.local.toml` (untracked), if present.
2. The directories on `PATH`, using the executable names in the catalogue (`.exe`, `.cmd`,
   `.bat` and `.com` are tried on Windows).
3. The catalogue's install-location hints, written with environment variables such as
   `%ProgramFiles%`.

Then the tool is run once with its version arguments and the version is read from the output.

## Installing

| id          | tool                | Windows                        | Linux                       | licence note                     |
|-------------|---------------------|--------------------------------|-----------------------------|----------------------------------|
| `store`     | tar (the control)   | bundled with Windows 10+       | `apt install tar`           | external program only            |
| `7z`        | 7-Zip               | `winget install 7zip.7zip`     | `apt install 7zip` (`7zz`)  | external program only            |
| `rar`       | WinRAR (`rar`)      | `winget install RARLab.WinRAR` | `apt install rar` (multiverse) | 40-day trial, then a paid licence |
| `zstd`      | Zstandard           | `winget install Meta.Zstandard`| `apt install zstd`          | external program only            |
| `xz`        | XZ Utils            | `winget install TukaaniProject.XZUtils` | `apt install xz-utils` | external program only      |
| `zpaqfranz` | zpaqfranz           | release from github.com/fcorbelli/zpaqfranz | same           | MIT                              |
| `tsaur`     | t-saur              | release from github.com/iulianbondari/t-saur | same          | Apache-2.0 OR MIT                |
| `wzzip`     | WinZip command line | manual, see below              | not available               | paid licence                     |
| `pacl`      | PowerArchiver command line | manual, see below       | not available               | paid licence                     |

`zpaqfranz` and `tsaur` are marked `verified = false` in the catalogue: their command lines come
from the projects' READMEs and have not been run by us. t-saur's lite builds need `--no-lepton`;
add it in `bench/tools.local.toml` (below). If a tool's help disagrees with the catalogue, fix the
catalogue in a commit and say so in the commit message.

## Pointing the runner at a tool: `bench/tools.local.toml`

This file is git-ignored and optional. It holds `[[tool]]` entries matched by `id`; any field
present replaces the catalogue's field for that tool, and settings are merged by setting `id`.
Use it for a tool installed in an unusual place:

```toml
[[tool]]
id = "7z"
path = "D:/portable/7-Zip/7z.exe"   # any path; used on this machine only
```

or for a variant command line:

```toml
[[tool]]
id = "tsaur"
create = ["pack", "{archive}", "{input}", "--no-lepton", "{settings}", "{threads}"]
```

Templates may use `{archive}`, `{input}`, `{outdir}`, `{settings}` and `{threads}` (see the header
of `bench/tools.toml`). The runner executes every directory-mode tool with its working directory
set to the parent of the class directory and passes `{input}`, `{archive}` and `{outdir}` as
relative paths, so no archive contains an absolute path. Result files record the exact argument
list as executed (thread argument filled in, paths shown relative) plus the setting id. All
settings run at the machine's thread count, as a user would run them; each result records
whether the thread count changes the archive size for that tool (`ratio_depends_on_threads`).

WinRAR's settings keep its default non-solid mode (`m3`, `best`, `best-rr3`) and add `best-solid`
(`-s`) as its strongest sensible configuration; `best-rr3` differs from `best` only by the
recovery record. The `store` tool uses the Windows System32 bsdtar (its hint is tried before
PATH) because GNU tar, as shipped with Git for Windows, reads `D:` in an archive path as a remote
host; the recorded version names the flavour.

## Manual tools: WinZip and PowerArchiver

`wzzip` and `pacl` need a paid licence and their command-line flags for the strongest setting
have not been verified, so the catalogue lists them without guessed flags. Without an entry in
`bench/tools.local.toml` they are reported as `skipped: manual (see docs/BASELINES.md)` and
excluded from every comparison.

To include one:

1. Install the product and the command-line component:
   WinZip with its Command Line add-on (`wzzip.exe`), or PowerArchiver with its command-line
   executable (`pacl.exe`). Use a licence you own.
2. Open the tool's own help (`wzzip -h`, `pacl` with no arguments, or the vendor's command-line
   manual) and take the flags for its strongest compression, its multithreading, and for
   extracting into a directory. Do not copy flags from the web without checking them against the
   installed version.
3. Add an entry to `bench/tools.local.toml`: `path` (if not on `PATH` or in the usual install
   folder), `extension` if the format is not `.zip`, `create` and `extract` templates, `threads`
   if the tool has a thread option, and at least one `[[tool.setting]]` with `id`, `compress`
   and `extract` arguments. Example shape (the flags are placeholders, take yours from the help):

   ```toml
   [[tool]]
   id = "wzzip"
   create = ["-a", "{settings}", "{archive}", "{input}/*"]
   extract = ["-e", "{archive}", "{outdir}"]
   [[tool.setting]]
   id = "strongest"
   compress = ["<flags from wzzip help>"]
   extract = []
   ```
4. Run `lpk-bench run --list-tools --tools wzzip`; it should show `found` with a version.
   The exact flags are recorded in each result file, so a reader can see what was compared.

The entry stays on your machine. If the comparison is published, say in the report that the
WinZip or PowerArchiver command line was supplied by the owner.
