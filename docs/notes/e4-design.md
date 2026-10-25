# E4 design: sandboxed workers for every foreign-format parser

2026-10-25, revised; for E4-1 to E4-8 (`docs/PLAN.md:166–183`), checked on `origin/main` 9006c5d. Numbers are
design defaults or format limits, never measurements; each choice becomes a `D-NN` (§7).

## 1. Process model (E4-1, E4-4)

**Workers.** Two binaries started from the host's own directory: `lpk-worker` (Rust readers, no C code) and
`lpk-worker-libarchive`; one per opened archive, never reused (a password and parser state die with it); neither
sees the destination.

**LPAC is the only default.** `CreateProcessW` with `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` (no capability)
and `PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY` = `PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT`
[LAC]: with no network capability it cannot reach the network [LAC]; a restricted token cannot protect TCP/IP
sockets [CHR].

**Restricted token: opt-in only** (`lpk x --sandbox restricted`, a GUI setting; warned on every use; never for
libarchive), for machines where the AppContainer launch fails. `CreateRestrictedToken` on the caller's token:
`DISABLE_MAX_PRIVILEGE`, every group deny-only, restricting SIDs {Everyone, Users, RESTRICTED}, both access checks
required [CRT]; Low integrity [STI]; `CreateProcessAsUserW` needs no `SE_ASSIGNPRIMARYTOKEN_NAME` for it [CPAU].
Residual, for `docs/SECURITY.md`: reading whatever those SIDs may read (E4-1 pins a Users-readable file), such
named objects, the network. Rejected: an automatic fallback; Chromium's NULL-SID lockdown (an impersonation
hand-off after DLL loading [CHR]).

**Spawn.** Suspended and `DETACHED_PROCESS` [PCF], so there is no console: from the console-less GUI a console
program would get a new one [CC], and `CREATE_NO_WINDOW` still puts a `conhost.exe` in the job
(`lpk-procstat-sys/src/windows_impl.rs:161–163`), breaking the one-process limit. The job comes from
`PROC_THREAD_ATTRIBUTE_JOB_LIST` [UPTA]; nothing is inherited. The broker duplicates three pipe ends (requests,
responses, stderr) and the archive handle(s), read-only, into the child [DH], writes their values to a page at the
first free of four random addresses fixed before creation and named on its command line (`VirtualAllocEx` retried
at the next [VAE][WPM]), then resumes it; the child takes the first candidate `VirtualQuery` shows committed [VQ]
whose block validates (magic, version, length, checksum), else refuses to start. No handle is ever inheritable,
so a concurrent `std::process::Command` [RSTD] takes none.

| Both modes | Setting | Source |
|---|---|---|
| One process | `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` = 1 (excess terminated) and `PROCESS_CREATION_CHILD_PROCESS_RESTRICTED` | [JBLI], [UPTA] |
| Memory | `…_PROCESS_MEMORY` = `…_JOB_MEMORY` = D-67's extraction budget (`--memory`); a commit past it fails | [JBLI] |
| CPU | `…_PROCESS_TIME` (user mode; terminated past it) = min(`cpu_max`, user time at the last host-observed progress + 60 s); `cpu_max` default 1 h, `--max-cpu <minutes\|off>` | [JBLI] |
| No UI | all eight `JOB_OBJECT_UILIMIT_*`; `WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON` (answers [CRT]'s `SendMessage` warning); `EXTENSION_POINT_DISABLE`, `PROHIBIT_DYNAMIC_CODE`, `IMAGE_LOAD_NO_REMOTE`, `IMAGE_LOAD_NO_LOW_LABEL`, `STRICT_HANDLE_CHECKS` | [JUI], [UPTA] |
| Lifetime | `KILL_ON_JOB_CLOSE`; `DIE_ON_UNHANDLED_EXCEPTION` (exception code as exit status); no `BREAKAWAY_OK` | [JBLI] |

| Reach | Restricted (opt-in) | LPAC |
|---|---|---|
| Files | anything granted to Everyone, Users or RESTRICTED [CRT]; no write to Medium or unlabeled objects [MIC] | only objects granted to ALL RESTRICTED APPLICATION PACKAGES or its package SID [CHR] |
| Registry | the same rule, no writes [MIC] | none without `registryRead` [LAC] |
| Named objects | the session's, where those SIDs are granted | its own namespace [ACI] |
| Network | **open** [CHR] | none [LAC] |
| Other processes | no write-up into Medium [MIC] | isolated [ACI] |
| Clipboard, desktop | none (Win32k disabled, UI limits) | the same, plus window isolation [ACI] |

**LPAC identity.** `LitePack.Worker` [CACP]. E4-1 checks whether its SID from
`DeriveAppContainerSidFromAppContainerName` [DACS] launches without a profile; if so none is made, else one is
created once, reused (as [LAC]'s sample) and removed by the uninstaller and `lpk-worker --remove-profile`, never
per launch (a profile is per-user folders and registry [CACP]). Both worker images grant the package SID read and
execute [CHR].

**`rappct` (decision (c)).** Audit: the published licence text is MIT (GitHub does not detect it as MIT [RAPG]);
maintenance (0.11.1–0.13.3 shipped 2025-10-21..23, none since, one owner [RAPC]); each unsafe block argued;
dependencies `windows` 0.62, `thiserror` [RAPS]; fit. Expected outcome: direct `windows` 0.62.2 calls in
`lpk-sandbox-sys`.

**Linux (decision (d)).** A plain child under `setrlimit` (`RLIMIT_AS`, `RLIMIT_CPU`, `RLIMIT_CORE` 0,
`RLIMIT_FSIZE` 0, `RLIMIT_NOFILE`) [RLIM] with `PR_SET_PDEATHSIG` [PRCTL], archive on fd 3; unsandboxed, CI only.

**Unsafe.** Only `lpk-sandbox-sys` (token, job, launch, AppContainer, ACL, bootstrap read, Linux `pre_exec`)
and `lpk-libarchive-sys` (FFI, safe wrapper). The job code of `windows_impl.rs` (`:31–64`, `:110–156`,
`:233–244`) moves to a public `job` module both call. Lint tables copy `lpk-procstat-sys/Cargo.toml:27–28` but
deny `clippy::undocumented_unsafe_blocks` (warn, `:34`). Rule: one operation per `unsafe` block, under a
`// SAFETY:` comment naming the invariant. E4-5's null dereference is a test-only binary there; every other new
crate keeps `unsafe_code = "forbid"` (`Cargo.toml:24`).

## 2. The broker protocol (E4-2)

Crates: `lpk-proto` (frames, the `ForeignReader` trait and the worker loop; no parser, no dependency,
`forbid(unsafe)`), the host's `lpk_core::foreign`, both binaries.

- **Framing.** `u32` LE length, `u16` LE kind, payload; control frames ≤ 64 KiB, data and list pages ≤ 1 MiB; each
  count field is checked against the bytes left in the frame before allocating. The worker's first frame,
  `Hello { proto, build }`, must equal the host's within 10 s.
- **Requests.** `Open { password? }` → `Opened { format }` | `NeedPassword`; `List { from, max }` →
  `Entries { records, more }`; `ReadMember { indices }` (ascending, so a solid block decodes once) → per member
  `MemberStart`, `Data`…, `MemberEnd { checked }`; `Cancel` → `Cancelled`; `Close` → exit; `Error`; `Fatal` from a
  panic hook.
- **Credentials.** In `Open` only, `Zeroizing` on both sides; `--keyfile` refused as `UnneededKeyfile`
  (`lpk-cli/src/lib.rs:290`). `NeedPassword` → `FormatError::PasswordRequired` (`lpk-format/src/error.rs:402`), a
  wrong one → `WrongKey` (`:405`), so `with_credentials` (`lpk-cli/src/lib.rs:390–416`) retries unchanged.
- **Limits, never from worker reports.** Progress is what the host sees: the job's read bytes [IOC], credited up to
  the archive length, plus `Data` bytes, up to the declared total. 60 s without progress, or the CPU limit (§1) →
  `TerminateJobObject`; cancel grace 2 s. Entries ≤ archive length ÷ the format's smallest header (ZIP: 46 bytes),
  times the expansion limit where headers are compressed (7z, compressed tar), and ≤ 10 million unless
  `--max-entries <n|off>`; the listing cache is charged to D-67's budget (each entry's fixed size plus its name);
  `more = true` past a cap is refused.
- **Output.** Bytes written ≤ the expansion limit × archive length: default 10,000, a refusal threshold (the zlib
  format tops out at 1032:1 [ZT]), `--max-expansion <factor|off>`. Free space (`GetDiskFreeSpaceExW` [GDF],
  `statvfs`, beside `lpk-procstat-sys/src/lib.rs:229`) is checked against the declared total before writing and
  every 64 MiB written. The host checks each record's packed range (offset, length, solid block): inside the
  archive, no overlap between blocks (`OverlappingEntries`).
- **Cancellation.** E3-3's token (`lpk-core/src/progress.rs:124`) is polled between frames: `Cancel`, a kill after
  the grace, D-54's cleanup, `CoreError::Cancelled`. Progress events (`:111`) come from frames.
- **Errors (D-77).** `CoreError::Foreign` (format, method, corrupt, size, overlap, limit, space) and
  `CoreError::Worker` (spawn, sandbox, denied, crash, panic, memory, CPU, timeout, protocol, pipe, build): exit 2,
  `kind` the inner variant as for `Format` (`lpk-cli/src/failure.rs:6–8`), placed by the exhaustive match
  (`:14–15`). No worker output can panic the host.
- **Stderr.** Drained by a thread, first and last 32 KiB kept and shown escaped, never parsed.
  **Back-pressure:** pull only; a full pipe blocks the worker.
- **Policy before any write.** Each record becomes an `lpk_format::Entry` (`lpk-format/src/entry.rs:88`), passes
  `validate_path` (`:106`) and E3-2's pass unchanged (`lpk-core/src/extract.rs:738–748`, `OutDir` `:612–722`, moved
  out and shared). Files are created create-new as data arrives, never pre-sized (unlike `:83–84`); unfinished ones
  are removed (`Shared`, `:333–379`); received bytes must equal the declared size. Tar hard links become copies of
  an extracted member, else refused. `lpk x` and `lpk l` (`lpk-cli/src/lib.rs:1284`, `:1645`) route by detection.
- **Fuzz target `worker_proto`**: arbitrary worker output through the decoder, the limits, the policy pass and the
  writer over an in-memory sink, plus detection; no panic, memory under a counting-allocator bound
  (`fuzz/Cargo.toml:18–19`).

## 3. Readers (E4-3, E4-4)

**Rust family** (`lpk-foreign`, `forbid(unsafe)`), no C codec. ZIP: `zip` 9.0.4 with `deflate` (zlib-rs),
`deflate64`, `lzma`, `xz`, `ppmd`, `aes-crypto`, `legacy-zip` [ZIPF]; bzip2 and zstd members read raw
(`by_index_raw`), decoded by `bzip2-rs` [B2R] and `ruzstd` (`zip`'s `bzip2-rs` feature pulls C `bzip2-sys`);
backslashes read as separators [APN]. 7z: `sevenz-rust2` 0.23.0 with `aes256`, `ppmd`, `deflate`, `brotli`, `lz4`
[SZ7]. tar: `tar` 0.4.46 over `flate2` (zlib-rs), `bzip2-rs`, `lzma-rust2` (`xz`, `lzip`, `.lzma`), `ruzstd`.
Cargo unifies features across a build, `--workspace` included [CR], and the host enables `zip`'s `zstd` and
flate2's C `zlib` (`crates/lpk-core/Cargo.toml:15`), so `lpk-foreign` and `lpk-worker` form their own workspace,
`workers/` (excluded like `fuzz/`; own lockfile, `deny.toml` and CI job; built into the shared `target/`), and the
release job fails if the shipped `lpk-worker.exe`'s linker map (`/MAP`) holds an unmangled `inflate*`, `ZSTD_*`
or `BZ2_*` symbol.

**libarchive** (`lpk-libarchive-sys`). Recommended: 3.8.9 (`docs/LICENSING.md:61`) vendored in
`vendor/libarchive-3.8.9/` (sources, `COPYING`, file list), built with `cc` against a committed `config.h` per
target (x86_64 and aarch64 MSVC, x86_64 Linux) generated once by libarchive's CMake [LAI]; hand-written bindings.
In: the read core; formats `rar`, `rar5`, `iso9660`, `cab`, `lha`, `ar`, `cpio`, plus `raw` and `tar` only behind
`compress` or `uu`; filters `gzip`, `xz`, `zstd`, `compress`, `rpm`, `uu`; registered by name, never `…_all`. Not
compiled: writers, `*_disk*`, `archive_read_support_filter_program.c` (so no external-program fallback can link),
`bzip2`, `lrzip` (always a program [LALZ]), `lz4`, `lzop`, `grzip`, `mtree` (reads files it names [LAF]), `zip`,
`7zip`, `xar`, `warc`. Codecs: zlib from `libz-sys` (in `Cargo.lock`, `links = "z"`, static), liblzma from
`liblzma-sys` (0BSD), libzstd from `zstd-sys` (BSD-3-Clause). Rejected: the `cmake` crate (CMake on every builder,
everything compiled); a prebuilt (unauditable, not reproducible for E8-5); system libarchive (drift).

**RAR licence.** `COPYING` [LACOPY] keeps every C source on its two-clause terms except UC Regents files,
`archive_parse_date.c` and BLAKE2 (taken under CC0-1.0); `libarchive/archive_read_support_format_rar.c` has a
two-clause BSD header (Tim Kientzle, Andres Mejia) [LAR4], `…_rar5.c` one by Grzegorz Antoniak [LAR5];
`archive_ppmd7.c` is Igor Pavlov's public-domain PPMd [LAP7]. No unRAR code.

**Detection by magic, in the host**, byte comparisons only: `.lpk`, 7z, RAR4, RAR5, ZIP (local header or end
record, which finds SFX), gzip, bzip2, xz, zstd, lzip, `.Z`, `MSCF`, `CD001` at 0x8001, `ustar` at 257,
`!<arch>`, cpio, RPM, LHA; PE files scanned 4 MiB for 7z or RAR. Volume sets (`.partN.rar`, `.rNN`, `.7z.NNN`,
`.zip.NNN`): one handle per volume, at most 1,000.

| Formats ([7Z] 26.05, [RL]) | Route | Named gaps |
|---|---|---|
| ZIP, JAR | Rust | encrypted bzip2/zstd members; spanned `.z01` sets; WinZip JPEG, WavPack |
| 7z | Rust | BZIP2, zstd, Deflate64, LZS, Lizard coders (`decoder.rs:206` [SZD]) |
| TAR, GZ, BZ2, XZ, LZMA | Rust | |
| RAR | libarchive (RAR 2.9–4.x, RAR5, volumes, SFX) | encrypted: main header fatal (`rar.c:998–1004`), data (`:1466`), RAR5 headers (`rar5.c:2408`) and data (`:4284`) [LAR4][LAR5]; solid RAR4 (`rar.c:1409–1433`); filters inside PPMd (`:2189–2191`); RAR 1.5/2.x (`unp_ver` never read, `:185`); RAR5 dictionaries above 64 MiB (`rar5.c:1851–1855`) |
| ISO | libarchive (ISO 9660, Rock Ridge, Joliet [LAF]) | UDF, UDF-only images [LAT] |
| CAB | libarchive (stored, MSZIP, LZX) | Quantum; multi-cabinet sets [LACB] |
| LZH, AR, CPIO, RPM, Z, UUE | libarchive | bzip2 payloads |
| XAR, ARJ, WIM, CHM, MSI, NSIS, IHEX; 7-Zip's disk images and file systems (APFS to VMDK) | | none built: libarchive 3.8.9 reads only XAR of these, needing an XML parser [LAT][LAX]; unRAR and 7-Zip handlers excluded (D-89 (a)) |

Follow-up tasks: our own ZipCrypto and AES over the tree's RustCrypto crates, so encrypted bzip2/zstd members
decrypt, then decode with `bzip2-rs`/`ruzstd`; a vendored patch (as D-66) adding BZIP2 and zstd to
`sevenz-rust2`'s coder match (`decoder.rs:206`).

## 4. Crash containment and the hostile set (E4-5, E4-7)

The cause comes from the broker's actions (timeout, cancel), then the exit status and job accounting (user time,
peak commit, read as `windows_impl.rs:233–244` does); job messages only confirm, delivery not guaranteed [JCP].

| E4-5 case | Stopped by | Host observes → cause |
|---|---|---|
| abort | | exit status, no `MemberEnd` → `Crashed` |
| null dereference | `DIE_ON_UNHANDLED_EXCEPTION` | exception code → `Crashed` |
| allocation past the limit | the commit fails [JBLI] | peak commit at the limit → `MemoryLimit` |
| spin | `PROCESS_TIME` [JBLI] | user time at the limit → `CpuLimit` |
| spawn | child-process policy, process limit | the parser's report → `Denied(process)` |
| network | no capability [LAC] | `Denied(network)`; under the opt-in, the residual |
| file readable by Users | package SID (LPAC) | `Denied(file)`; under the opt-in, the residual |

Each test asserts the cause, a live host and a working next extraction, on `windows-2025`.

E4-7, generated fixtures, refused before the first data frame unless stated: overlapping-entry ZIP →
`OverlappingEntries` (host check); nested archives → one level, the inner one written as a file; 4 GiB declared
over 1 KiB → `SizeMismatch` after at most the body, file removed; high-ratio bomb → the expansion limit; `..`,
absolute, UNC, device and `:` names → `UnsafePath` (D-79); tar symlinks out → skipped without `--symlinks`, and an
entry below a symlink entry is a conflicting name (`check_conflicting_names`, `lpk-core/src/policy.rs:405`); hard
links out → refused; case-only pairs → `CaseCollision` (`:450`); 1,000,000 members → pages, host memory under the
listing charge. Time bounds: the timeouts, set short.

## 5. Fuzzing (E4-6)

New targets: `jpeg_reconstruct`; `jpeg_peel` and `deflate_analyse`, the compress-side Peel parsers, for the gate's
"every Peel parser" (the walker has `deflate_walk`); `foreign_zip`, `foreign_7z`, `foreign_tar`,
`foreign_libarchive`; `worker_proto`; `png_filter_undo` with revision 1.4: 28 targets, 29 with PNG.

`.github/workflows/fuzz.yml` today is one weekly job (`:12–13`) of 360 minutes at ≤ 1,200 s per target
(`:27–38`). New: a `build` job compiles every target once; a `targets` job turns `fuzz/fuzz_targets/*.rs` into the
matrix and fails when `fuzz/Cargo.toml`'s `[[bin]]` set or `fuzz/run.sh:18–20` differs; one job per target, 600 s
nightly, 3,600 s on Mondays (decision (e)), flags as `fuzz/run.sh:37`. `foreign_libarchive` builds libarchive with
clang and `-fsanitize=fuzzer-no-link,address`. Corpus: `actions/cache` per target, restored by prefix (unused
entries expire after 7 days, 10 GB per repository [GHC]), minimised on Mondays; seeds from
`fuzz/seeds/<target>/`, the vectors and foreign fixtures; a crash input joins the seeds with its fix.

## 6. Consumption by E5 and E6

E5-1's `Session` is an enum, `Lpk(lpk_format::Archive)` or `Foreign(foreign::Broker)`, with
`open(path, &OpenOptions)`, `page(at, len, &Order)`,
`extract(&Selection, dir, &dyn ExtractPolicy, &Monitor) -> ExtractSummary`, `read_entry(index, cap)`,
`test(&Monitor)` and, foreign only, `convert(out, &Pipeline, &Monitor) -> RunSummary`; `OpenOptions` carries
credentials, `memory` and `foreign::Limits`. A foreign session keeps one worker (restarted after a failure, ended
on drop) and the validated listing; `check`, `repair`, `history`, `rollback`, `append` stay `.lpk`-only. E6 never
loads the engine (E6-1); its verbs reach `Session` through E5-7.

**`--convert`.** Members are extracted under E3-2's policy into a private spool beside the output; `create` runs
over inputs built from the validated listing (symlink targets carried: `lpk-core/src/ingest.rs:52–69`) with spool
files as sources; the spool is always removed. Rejected: streaming from the worker, since inputs are opened by path
(`lpk-core/src/source.rs:30`) and ordered across the set first (`FileOrder`, D-12).

## 7. PLAN edits, decisions, order, scope, risks

PLAN edits: **E4-1** builds both launch modes, owns decision (c)'s audit, inherits nothing (three pipe ends and
the archive handle(s) duplicated into the suspended child, not "two pipe handles inherited") and tests a
BUILTIN\Users-readable file. **E4-2/E4-3**: `lpk-worker` and `lpk-foreign` live in the `workers/` workspace, no C
codec. **E4-4**: "ISO9660/UDF" becomes ISO 9660, UDF a named gap. **E4-5**: the network case is tested blocked
under LPAC and documented as the opt-in's residual (a test-case change, not a gate threshold). **E4-6**: adds
`jpeg_peel`, `deflate_analyse`.

| Task | Acceptance additions | `D-NN` records |
|---|---|---|
| E4-1 | `crates/lpk-sandbox-sys/tests/`, both modes, `windows-2025`: token shape; memory-limit kill; dies with the job; no spawn; only the duplicated handles; launched from a console-less host, one process in the job; the Users-readable file denied under LPAC, readable under the opt-in (pinned); Linux `setrlimit`; the manifest grep | modes, identity, residual |
| E4-2 | a `faults` feature (absent from release: `cargo tree -e features`) sends garbage, hangs, closes the pipe, repeats `more = true`: each a named error within its limit plus a stated slack | framing, limits, errors |
| E4-3 | BLAKE3 manifests for the fixtures; three reader targets; `workers/` CI green, the map check | crates, workspace, detection |
| E4-4 | RAR4/RAR5 (volumes too) by `rar`, ISO, CAB by `makecab`; the table as `docs/FORMATS.md`; `foreign_libarchive` | `rappct`, libarchive build |
| E4-5 | §4 | cause table |
| E4-6 | §5; seven consecutive green nights after the last target lands | matrix, corpus |
| E4-7, E4-8 | §4's bounds; PLAN's checks | |

One more `D-NN`, recommended: the Deflate peel's container walkers (`lpk-core/src/peel/deflate/walk.rs:209–210`
parses ZIP during `lpk a`) stay in-process. They only locate Deflate streams in files the user chose to archive,
bounded by D-66 and D-68 and fuzzed (`deflate_walk`); a worker would pipe every peeled file.

Order: E4-1 → E4-2 → E4-3 → E4-7; E4-6's workflow early, ticked last; `lpk-libarchive-sys` beside E4-2; then
E4-4 → E4-5 → E4-8.

**Out of Phase 1.** Linux and macOS sandboxing; the Peel parsers in a worker; the gap formats; recursive
extraction; broker-mediated reads; foreign writers beyond E3-6.

**Risks.** (1) Windows 10 22H2 against 11: every API predates 22H2 (XP: tokens, jobs; Vista: attribute lists; 8:
`SECURITY_CAPABILITIES`, profiles; 10: job list, child-process policy [UPTA][CACP]; LPAC: RS2 [CHR]); what an LPAC
may read differs by build and CI has no Windows 10 runner, so E4-5's tests also run on E5-10's 22H2 machine.
(2) Profile creation refused (`E_ACCESSDENIED` [CACP]) where one is needed: foreign archives refused unless the
user opts in. (3) Raising `PROCESS_TIME` on a live process: E4-1 proves it, else the watchdog kills. (4)
`bzip2-rs` has had no release since 0.1.2 (2021-02-16) [B2R]: fuzzed; replacing it is a `D-NN`. (5) libarchive
security releases: each bump a `D-NN`.

[CRT]: https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken
[STI]: https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-settokeninformation
[CPAU]: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw
[UPTA]: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute
[PCF]: https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
[CC]: https://learn.microsoft.com/en-us/windows/console/creation-of-a-console
[DH]: https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle
[VAE]: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualallocex
[WPM]: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-writeprocessmemory
[VQ]: https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualquery
[JBLI]: https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information
[JUI]: https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_ui_restrictions
[JCP]: https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_associate_completion_port
[IOC]: https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-io_counters
[GDF]: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getdiskfreespaceexw
[MIC]: https://learn.microsoft.com/en-us/windows/win32/secauthz/mandatory-integrity-control
[LAC]: https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer
[ACI]: https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation
[CACP]: https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-createappcontainerprofile
[DACS]: https://learn.microsoft.com/en-us/windows/win32/api/userenv/nf-userenv-deriveappcontainersidfromappcontainername
[CHR]: https://chromium.googlesource.com/chromium/src/+/HEAD/docs/design/sandbox.md
[RSTD]: https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html
[CR]: https://doc.rust-lang.org/cargo/reference/resolver.html#features
[RLIM]: https://man7.org/linux/man-pages/man2/getrlimit.2.html
[PRCTL]: https://man7.org/linux/man-pages/man2/prctl.2.html
[RAPC]: https://crates.io/crates/rappct/versions
[RAPG]: https://github.com/cpjet64/rappct
[RAPS]: https://docs.rs/crate/rappct/0.13.3/source/Cargo.toml.orig
[ZIPF]: https://docs.rs/crate/zip/9.0.4/source/Cargo.toml
[B2R]: https://crates.io/crates/bzip2-rs
[SZ7]: https://docs.rs/crate/sevenz-rust2/0.23.0/source/src/lib.rs
[SZD]: https://docs.rs/crate/sevenz-rust2/0.23.0/source/src/decoder.rs
[APN]: https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT
[ZT]: https://www.zlib.net/zlib_tech.html
[LAI]: https://github.com/libarchive/libarchive/blob/v3.8.9/INSTALL
[LACOPY]: https://github.com/libarchive/libarchive/blob/v3.8.9/COPYING
[LAR4]: https://github.com/libarchive/libarchive/blob/v3.8.9/libarchive/archive_read_support_format_rar.c
[LAR5]: https://github.com/libarchive/libarchive/blob/v3.8.9/libarchive/archive_read_support_format_rar5.c
[LAP7]: https://github.com/libarchive/libarchive/blob/v3.8.9/libarchive/archive_ppmd7.c
[LACB]: https://github.com/libarchive/libarchive/blob/v3.8.9/libarchive/archive_read_support_format_cab.c
[LALZ]: https://github.com/libarchive/libarchive/blob/v3.8.9/libarchive/archive_read_support_filter_lrzip.c
[LAX]: https://github.com/libarchive/libarchive/blob/v3.8.9/libarchive/archive_read_support_format_xar.c
[LAF]: https://github.com/libarchive/libarchive/blob/v3.8.9/libarchive/libarchive-formats.5
[LAT]: https://github.com/libarchive/libarchive/tree/v3.8.9/libarchive
[7Z]: https://www.7-zip.org/
[RL]: https://www.rarlab.com/otherfmt.htm
[GHC]: https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching
