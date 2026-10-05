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

## Measuring

```text
cargo run -p lpk-bench -- run --tools all --profile small [--corpus DIR] [--classes a,b]
    [--repeats 3] [--threads N] [--timeout-s N] [--long-run-s 120] [--results DIR] [--tmp DIR] [--allow-dirty-build]
cargo run -p lpk-bench -- run --compare <dirA> <dirB> [--max-diff-pct 3]
```

The defaults are the corpus `bench/corpus/<profile>`, three repeats, the machine's logical cores as
the thread count, results in a new `bench/results/<date>-<host>[-<n>]/` and temporary files under
`bench/tmp/<run>/`, which must be on the same volume as the corpus. The run refuses a build that
is dirty or of unknown origin unless `--allow-dirty-build` is given (recorded in `host.json`).
It exits non-zero when any combination failed or the written directory does not validate.

How a combination is measured:

- Before every tool and setting, outside any timing, every input file of the class is read and its
  BLAKE3 compared with the manifest (this also leaves the inputs in the file cache); a mismatch
  aborts the run and names the file.
- Each tool runs through `lpk-procstat-sys`, never through a shell. Creation runs in the parent of
  the class directory (the corpus root for a private corpus) with relative paths. Extraction, for
  every tool and for both steps of the tar-stream tools, runs in the combination's own scratch
  directory (`c1`, `c2`, ... under the run's temporary directory) with plain names for the archive
  and the output directory: WinRAR's long-path handling fails whenever its output argument contains
  `/` or `..`, and zpaqfranz extracts nothing when `-to` climbs two or more directories. Files that
  a later step reads (the archive, the tar file) are flushed to disk between steps, outside the
  timed intervals, so one step does not pay for the previous step's unwritten pages. Tools are measured with
  their output redirected, no progress display and no console, so every catalogue command line
  pins the tool's assume-yes and quiet switches; nothing may wait for a keypress.
- Tool-configuration environment variables are removed from the child (`XZ_OPT`, `XZ_DEFAULTS`,
  `ZSTD_CLEVEL`, `ZSTD_NBTHREADS`, `GZIP`, `RAR`, `TAR_OPTIONS`, `TAR_READER_OPTIONS`,
  `TAR_WRITER_OPTIONS`, `TAPE`); the names are recorded in every result's `measurement` object.
  WinRAR is also run with `-cfg-`, which makes it ignore `rar.ini`, `.rarrc` and the `RAR`
  variable. Other tools' configuration files are not touched.
- Tar-stream tools (zstd, xz) run in two sequential steps through a temporary file, never
  concurrently: tar writes the stream to a temporary tar file, then the compressor reads that file
  on its standard input and writes the archive; extraction is the decompressor writing a temporary
  tar, then tar extracting it. The published time is the sum of the two steps (each is also
  recorded as `tar_step` and `tool_step`), the published peak memory the larger of the two. The tar
  is the one the `store` tool resolved to. If a bsdtar step is ever flagged `descendants_killed`,
  tar started an external helper program: that is an adapter problem to report, not a tool fault.
- Every repeat compresses, extracts into an empty directory and verifies: each manifest file of the
  class must exist in the extraction with the same BLAKE3 and nothing else may be there. Tool
  output is never parsed (file names differ by code page); only the hashes decide.
- A combination fails, and is recorded as `failed` with the reason, when a step exits non-zero,
  times out, leaves descendants running, the archive is missing or empty, or verification finds a
  missing, extra or different file. A failed combination carries no median and the run goes on.
  The repeats completed before the failure stay in the file.
- After the loop deletes a tree of extracted files (between repeats and when a combination's
  scratch directory is removed) it pauses before the next timed step, by default 500 ms per 1000
  files removed (at least 250 ms, at most 30 s; `--settle-ms-per-1000-files`, 0 disables it). Deleting
  many just-written files leaves deferred work in the file system that would otherwise fall into
  the next timed step and penalise fast tools; the pause is the same for every tool, never inside a
  timed interval, and recorded in `run.json`.
- Medians are taken over the repeats for every measure. A combination whose first repeat (compress
  plus extract wall time) reaches `--long-run-s` is measured once; the result records the repeats
  requested, the repeats run and the reason (`repeats_short`).
- `run.json` is written last and lists every planned combination with its outcome, the repeats
  requested, `--long-run-s`, the thread count and the BLAKE3 of the catalogue file. A results
  directory without it is an aborted run and does not validate, and `run --validate` checks that
  every listed combination has its file with the listed outcome.
- On Windows the run warns before starting when the longest input path or the scratch archive
  path exceeds 259 characters (some tools, WinRAR among them, fail on such paths); use a shorter
  `--tmp` or checkout path.
- A private corpus (`corpus scan --private`) is supported for tools that take a list of files
  (`create_list` in the catalogue: tar, 7-Zip, WinRAR); the others are recorded as skipped with
  the reason. Their results carry `private: true`. Nothing derived from file names or tool output
  reaches a result file of a private corpus: failure reasons name a file by its position in the
  manifest. The system tar on Windows (bsdtar) cannot read non-ASCII names from a `-T` list, so a
  private class with any non-ASCII path is recorded as skipped for `store` and for the tar-stream
  tools there; classes with only ASCII paths run normally, and 7-Zip and WinRAR handle both.

`--compare` prints, for each tool and setting present in both directories, the sum over classes of
the median compress and of the median extract wall time, and the percentage difference
(`|B - A| / A`). It exits non-zero when a difference exceeds `--max-diff-pct` or when the two
directories differ in corpus, host, tool versions, thread count, requested repeats,
`--long-run-s` or catalogue file, or measured different classes, or a combination was measured in
only one of them or failed in both. This is the check behind the acceptance clause
"a second run differs by < 3% in time".

## Real-time scanners

Results include the cost of whatever real-time scanner is active (a scanner inspects every file an
extraction writes). `host.json` lists what Windows Security Center reports: every registered
antivirus product with its name, its raw `productState` (hex text) and the scanner state decoded
from bits 12-15 (`off`, `on`, `snoozed`, `expired`, `unknown`), and `antivirus_source` saying where
the list came from: `queried`, `query-failed`, or `not-applicable` (not Windows, or no Security
Center, as on Windows Server). The query runs once per run, before any timing, through Windows
PowerShell (`pwsh` as a fallback) with a time limit; a failure gives `query-failed`, never an
error. `defender_realtime` is an independent value read from the Defender registry key (`on`,
`off`, or `unknown`, which includes machines where the value is absent). A snoozed scanner can
resume in the middle of a run, so the products are queried again after the last combination (outside
all timing) and `run.json` records that list and `antivirus_changed`; the run prints a warning when
names or decoded states differ from the start. `--compare` prints a note, by name, when the product
names or decoded states differ between the two directories (start or end) or changed within either
run (names and decoded states only, not the raw value, whose low byte changes with definition
updates). For comparable runs, keep the scanner state the same.

## How a tool is found

1. The `path` of its entry in `bench/tools.local.toml` (untracked), if present.
2. The directories on `PATH`, using the executable names in the catalogue (`.exe`, `.cmd`,
   `.bat` and `.com` are tried on Windows).
3. The catalogue's install-location hints, written with environment variables such as
   `%ProgramFiles%`, or (the `lpk` row) a path relative to the current directory, which the runner
   records as an absolute path. `lpk` is this repository's own release build: `cargo build --release -p lpk-cli`
   first; the recorded version carries the build hash.

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
| `lpk`       | LitePack (this repository) | `cargo build --release -p lpk-cli` | same        | Apache-2.0 OR MIT                |
| `tsaur`     | t-saur              | release from github.com/iulianbondari/t-saur | same          | Apache-2.0 OR MIT                |
| `wzzip`     | WinZip command line | manual, see below              | not available               | paid licence                     |
| `pacl`      | PowerArchiver command line | manual, see below       | not available               | paid licence                     |

The `lpk` row has four settings (the fourth is the control of E2-8, below): `fast` (`--fast`, zstd with a long window), `balanced`
(`--balanced`: raw LZMA1 blocks with a 64 MiB dictionary, the dictionary the 7-Zip Ultra row uses,
or zstd `--ultra --long` for a block where a trial on a 4 MiB sample, taken as four stripes,
prefers it) and `store` (`--store`). The Fast tier closes a block at every cluster change; the
Balanced tier closes one only once it holds half the dictionary, and runs without the entropy
gate. JPEG files are peeled under both compressing settings. Balanced needs memory of about the
block size plus several times the dictionary to compress, and the block size plus the dictionary
to extract.

`balanced-unordered` (`--balanced --ordering none`) is the control row of the file-ordering task
(E2-8): the same as `balanced` except that the files of a cluster are written in path order, not in
the extension-and-similarity order (`lpk a --ordering similarity`, the default of every compressing
setting). The two rows are measured in one run on the same classes; the difference is what the
ordering buys, and the ordering's own cost shows in `lpk a -v`.

`lpk x --threads N` (E2-19) takes N as the extraction's total thread budget: decode workers plus
file writers never exceed N (the thread that only hands decoded blocks on is not counted). N = 1
decodes and writes on one thread; from N = 2 on, `clamp(N / 4, 1, 4)` threads write files and the
rest decode blocks, the decoders further capped so that the decoded blocks in flight stay within
the reader's decode memory. N above four times the logical cores is a usage error. The runner
passes the same N to `lpk x` as to 7-Zip, so the extraction comparison is thread for thread.

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

The catalogue flag `dedup = true` (default false; set on `zpaqfranz` and `tsaur`) marks a tool that
deduplicates across files. It is recorded per tool in `tools.json`. Since D-43 the report compares
gate G2 against the best tool without the flag and prints the flagged tools' best rows on the same
class as "reference, not compared".

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
