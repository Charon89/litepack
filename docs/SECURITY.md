# LitePack security notes: the worker model

Status: the worker-model section of PLAN E4-8, written 2026-10-27 against `main` and the decisions it cites. Every
statement names the decision (`D-NN`) or the code that carries it; nothing here describes planned work as done.
The full design and its sources are in `docs/notes/e4-design.md`. The root `SECURITY.md` (policy and contact) is
E9-7's task.

## 1. What runs where

- **The host** (`lpk`, the GUI through `lpk-api`) opens `.lpk` archives in process: their decoders are LitePack's
  own, bounded by the format's memory rules (D-66, D-67, D-68) and fuzzed (section 6). The host also detects the
  format of every other archive by magic bytes only (`lpk_core::foreign::detect`, D-106) and owns every file
  write (D-99 (4)).
- **The worker** (`lpk-worker`, built in the separate `workers/` cargo workspace) runs every foreign-format
  parser: ZIP, 7z, tar and the single compressed streams, all pure Rust with no C codec (D-106). The workers'
  `deny.toml` bans the C codec crates and `scripts/check-worker-symbols.sh` fails a release worker whose map holds
  C codec symbols or the test-only `faults` feature (D-99 (1)). One worker per opened archive, never reused, so a
  password and parser state die with it (`docs/notes/e4-design.md` section 1).
- **The broker protocol** (`lpk-proto`, `forbid(unsafe)`) connects them: length-prefixed frames (a 6-byte header,
  control frames at most 64 KiB, `Entries` and `Data` at most 1 MiB), every length and count checked against the
  bytes left before allocating, the worker's `Hello` (protocol and build) required within 10 s (D-99 (2)).
  Requests are `Open` (with the password, held in `Zeroizing` buffers), `List`, `ReadMember`, `Cancel`, `Close`.
- The libarchive worker for RAR, ISO, CAB and the rest (E4-4) is not on `main`; until it lands RAR is refused
  before any worker starts (D-106 (2)).

## 2. What the worker can and cannot reach (Windows)

`lpk-sandbox-sys` launches the worker (D-97); it is one of the crates allowed `unsafe` (`docs/UNSAFE.md`).

- **Default: an LPAC AppContainer** (`LitePack.Worker`, no capability, `ALL_APPLICATION_PACKAGES` opt-out). It
  reaches only objects granted to the package SID or to ALL RESTRICTED APPLICATION PACKAGES; no network; no HKCU
  (pinned by tests, D-97 (4)).
- **Job object, both modes**: one process, a commit limit per process and job, a per-process user-time limit
  raised only on host-observed progress, all eight UI restrictions, kill-on-close, die-on-unhandled-exception
  (D-97 (1)).
- **No inherited handle**: the pipe ends (each with a one-way access mask) and the read-only archive handle are
  duplicated into the suspended child and named in a checksummed page; the child refuses to start without a
  valid page (D-97 (1)). The worker never receives the destination.
- **Isolation between workers**: the process and thread carry a broker-only security descriptor and the child's
  default DACL is replaced; a test has one worker try sixteen `OpenProcess`/`OpenThread` rights on another and get
  access denied in both modes (D-97 (2)).
- **The worker writes nothing**: it never uses `TEMP` or writes a file, and the broker empties the AppContainer's
  `AC\Temp` before each launch without following reparse points (D-99 (6)).
- **Limits come from what the host observes, never from worker reports**: progress is the job's read bytes plus
  `Data` bytes; 60 s without progress ends the worker; entries are capped by the archive length over the format's
  smallest header and by `--max-entries` (default 10 million); output by `--max-expansion` (default 10,000) times
  the archive length, with free space checked before writing and every 64 MiB (D-99 (3)).
- **Policy before any write**: each record becomes an `Entry` through `validate_path` and E3-2's pass; files are
  created create-new as data arrives and removed if unfinished; received bytes must equal the declared size;
  overlapping packed ranges are refused, and packed ranges are required for every format that has them (D-99 (4),
  D-106 (3)).
- **The restricted-token mode** (Low integrity, restricting SIDs Everyone, Users and a per-launch unique SID) is an
  opt-in of the engine (`lpk_core::foreign::Sandbox::Restricted`) for machines where the AppContainer launch fails;
  the CLI does not expose it today (D-97 (1)).
- **Linux** runs the worker as a plain child under `setrlimit`, unsandboxed by decision (d); it is used by CI only
  (D-97).

## 3. Residual risks

Stated by the decisions that built the sandbox; none is closed by this note.

1. Under the restricted-token opt-in the worker can read whatever Everyone or Users may read (a BUILTIN\Users
   readable file and HKLM\SOFTWARE are read, both pinned by tests) and can reach the network (D-97 (4), D-105 (1)).
2. Every LPAC worker shares the persistent `Packages\LitePack.Worker\AC` folder. The broker empties `AC\Temp`
   before each launch, but one host's cleanup can run while another host's worker is alive and can race a
   junction swap: narrowed, not closed (D-99 (6)).
3. The job's CPU limit counts user time only and is enforced with a lag of a few seconds, so the no-progress
   watchdog stays required (D-97 (4), D-105 (3)).
4. The `Denied` causes of crash containment are reported by the worker itself: the tests prove the refusals, but a
   hostile parser would stay silent; the LPAC network test proves that loopback is blocked and Winsock cannot
   start, not that internet traffic is blocked as such (D-105 (3)).
5. A worker may exit with STATUS_QUOTA_EXCEEDED to be reported as `CpuLimit`; harmless, since a hostile worker
   chooses its exit code anyway (D-105 (2)).
6. Overlapping-range checks are advisory against a lying worker; the expansion cap and the free-space checks are
   what bound the output (D-99 (4)). A slow-drip worker can live up to the read credit (three times the archive
   length) (D-106).
7. A foreign archive is reopened by path for the worker after its zone was read from the held handle: a window
   the code documents (D-119 (1)).
8. The tests run on Windows 11 and `windows-2025` CI; the run on Windows 10 22H2 is by hand on E5-10's machine and
   has not happened yet (D-97, D-105).

## 4. Crash containment (E4-5)

A test-only crash parser runs under LPAC and under the restricted token: abort and null dereference end as
`Crashed`, allocation past the commit limit as `MemoryLimit`, spinning past the CPU limit as `CpuLimit`, and
spawning a process, opening the network or opening a file outside the allowance as `Denied` with the resource
named. Each of the 14 tests asserts the named cause, the host alive and the next extraction in the same session
succeeding (D-105). A panic in the worker is reported as `Panicked`; no worker output can panic the host (D-99 (5)).

## 5. The hostile-archive set (E4-7)

Generated fixtures, run through the real `lpk x` and the sandboxed worker, each refused or bounded with its named
reason and a stated time and memory bound (D-111): overlapping ZIP entries (`OverlappingEntries`); a nested
42.zip-shaped archive (one level, inner archives written as files); a 4 GiB declared size over a 1 KiB body (the
expansion limit, or `SizeMismatch` with the file removed under `--max-expansion off`); a high-ratio bomb
(`LimitExceeded`); sixteen unsafe names in ZIP, tar and 7z (`UnsafePath`); tar symlinks pointing outside
(skipped); tar hard links outside (`Unsupported`); case-only pairs (`CaseCollision` on Windows); a million-member
ZIP64 central directory (host memory within the listing charge). Every test asserts that nothing is written outside
the destination and that no partial file remains.

## 6. Fuzzing (E4-6)

29 targets cover every parser: the `.lpk` decoders and peels, the foreign readers (`foreign_zip`, `foreign_7z`,
`foreign_tar`) and the worker protocol (`worker_proto`). `fuzz.yml` runs one job per target, 600 s nightly and
3,600 s on Mondays, with a persistent corpus; a check fails the workflow when the matrix and `fuzz/fuzz_targets/`
differ; every build uses `--no-cfg-fuzzing` so the fuzzer sees the shipped checks (D-114). Findings so far were
fixed with their inputs kept as seeds (D-115, D-116). `foreign_libarchive` waits for E4-4.

## 7. Extraction policy on Windows (E6-6, E6-0)

Every write is relative to a held directory handle (`NtCreateFile` with `OBJ_DONT_REPARSE`); each reopen, cleanup
or hard-link copy compares the 128-bit file ID; a parent that is a link is always refused; only name-surrogate
reparse points are treated as links when a name is replaced; names over 255 UTF-16 units and paths over 32,767 are
refused (D-113). Symlinks are skipped by default; with `--symlinks` a link is created only when its target cannot
leave the destination (D-108) and the token holds `SeCreateSymbolicLinkPrivilege`, else it is skipped with a
warning (D-113 (3)). The same policy covers `.lpk` and foreign archives.

## 8. Mark of the Web (E6-5)

When an archive's `Zone.Identifier` gives zone 3 or 4 (or exists and cannot be read, counted as 3), every file
`lpk x` writes, the `export` output and the `repair` copy get a `Zone.Identifier` stream with only `ZoneId`, written
after the create-new and before the first byte through the file's verified handle; `--motw off` opts out; a volume
without named streams gives one warning (D-119).

## 9. Reporting a vulnerability

**PLACEHOLDER: the disclosure address and policy are the owner's to set (E9-7). Until then, do not file security
reports in public issues.**
