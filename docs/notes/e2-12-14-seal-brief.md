# E2-12 / E2-13 / E2-14 — the brief (saved from the 2026-10-04 session for the resume on 2026-10-07)

The dispatch brief as written; the implementer's design notes and the E2-14 blocker are in
`e2-12-14-seal-resume.md`.


PLAN Phase 1 tasks **E2-12** ("The `lpk-format` crypto from E1-12 driven by the pipeline: password, keyfile,
listable mode; per-chunk nonces from chunk indices. Acceptance: encrypted round-trip of `small`; the runner row
with `--password` shows the cost against the plain row; a wrong password is refused before any data is written
to disk"), **E2-13** ("Writing recovery frames at the configured percentage; repair on extraction. Acceptance: a
`small` archive damaged by the test's random overwrites up to the covered amount repairs and verifies; the
archive size overhead equals the configured percentage within the frame rounding") and **E2-14** ("Append a
changed directory to an existing archive with dedup against its chunks; rollback. Acceptance: `backup-versions`
as three appends (v1, then v2, then v3): the archive after three appends is within the frame overhead of the
one-shot archive of all three; rollback to v1 reproduces v1 bit-exactly"). The format already has all three
(D-38 recovery, D-39 encryption, D-40 journal; `crates/lpk-format/src/{crypto,writer,archive,journal}.rs`; the
format tool `lpk-decode` drives them with `--password`, `--password-file`, `--keyfile`, `check`, `repair`,
`rollback`); this brief wires them into `lpk-core`'s pipeline and the `lpk` tool, and proves the acceptances.
The orchestrator records the choices and runs the runner rows. Do not edit `docs/DECISIONS.md`, PLAN, STATUS,
`docs/spec/*`, `crates/lpk-check`, `crates/lpk-format` (its API is what it is; if one addition is needed, stop
and report NEEDS_CONTEXT).

## E2-12 — encryption
1. `lpk a --password <pw> | --password-file <file>` (and `--keyfile <file>`, `--listable`, `--suite
   aes-256-gcm|xchacha20-poly1305` with the format's default) build `SealOptions` from the credentials exactly
   as the format tool does (reuse its argument shapes and its password-file rule); the pipeline passes them to
   the writer (`WriterOptions::seal`); the Argon2id parameters are the format's defaults (D-39); never print or
   log a password; `--password` is documented as visible in the process list.
2. `lpk x`/`lpk t` with the same options; a wrong password is refused at the key slot — **before any output file
   or directory is created** (the plan opens the archive and the key slot first; test it: the output directory
   stays empty/absent after a wrong password); a missing password for an encrypted archive is refused the
   same way; listable archives list without the key.
3. Catalogue: the `lpk` tool gains the setting `fast-encrypted` (compress `["--fast", "--password-file",
   "{password_file}"]`?) — check what placeholders the runner offers (`bench/tools.toml` header); if no password
   placeholder exists, add one to the runner in the smallest honest way (`{password_file}` pointing at a file
   the runner writes into its scratch directory with a fixed test password, recorded in `run.json`), or use a
   fixed literal password in the setting with a note that it is a benchmark credential only. Extraction of the
   setting passes the same credential. The runner must verify the extracted tree as for any setting.
4. Tests: round trip through both tiers with a password, with a keyfile, listable and not; wrong password
   refused before any file exists; the peel and dedup unchanged under encryption; `lpk-check` (the independent
   decoder) verifies and extracts an encrypted archive made by `lpk a` with the password; the ignored corpus
   test archives every class of `small` encrypted (Fast tier), extracts bit-exactly, and prints the sizes and
   times next to the plain run (report only).

## E2-13 — recovery records
1. `lpk a --recovery <percent>` (0 = none, the format's bounds; default 0 for now — the owner's product default
   comes later) sets `WriterOptions::recovery` (D-38: interleaved per-group frames); `lpk t` reports damaged
   shards as `verify` does; `lpk repair <in> <out>` and `lpk check <archive>` call the format's `repair` and
   `check` (through `lpk_format::cli::run` or its functions — call, not copy).
2. Tests: an archive of a temp tree at 3% and at 10% recovery; the test overwrites random bytes within the
   covered range up to the covered amount (per group, as D-38 defines the bound — read the spec section 13 and
   the writer's `RecoveryOptions`), `repair` restores it and `verify` passes; damage beyond the bound is
   reported, not silently "repaired"; the size overhead equals the configured percentage within the frame
   rounding (compute the expected bytes from the data covered and assert within the rounding the spec
   states); the ignored corpus test does the same on `backup-versions` of `small`.

## E2-14 — journal (append and rollback)
1. `lpk append <archive> <dir>` adds a generation: `Writer::append` with dedup against the existing chunk table
   (D-40; the format already does it) through the pipeline (tiers, peel, fold, ordering all apply to the new
   generation); `lpk rollback <archive> <generation>` truncates to that generation after verifying the chain
   (the format's `rollback`); `lpk info` lists generations.
2. Tests: three appends of a temp tree's v1, v2, v3 (modified copies) versus the one-shot archive of all three
   — the appended archive's size is within the per-generation frame overhead (compute it: index + trailer +
   entry table per generation, as the spec sizes them) of the one-shot; rollback to generation 1 reproduces v1
   bit-exactly; a half-written append (truncate the file mid-append in the test) is recovered by rollback;
   dedup across generations counted in the summary. The ignored corpus test: `backup-versions` of `small` as
   three appends (`v1`, `v2`, `v3` folders) versus the one-shot archive of the class — sizes printed, the
   within-overhead relation asserted, rollback to v1 verified bit-exact.

## Constraints
- Branch `task/e2-12-14-seal` from `main` after E2-8 has landed (head given by the orchestrator).
- `#![forbid(unsafe_code)]`; no new dependency; no `unwrap` outside tests; no figures in docs, comments or
  commit messages; never a password in a test's committed fixture except an obvious test literal.
- Conventional Commits with the attribution line used so far; commit per task (E2-12, E2-13, E2-14) and per
  deliverable within them; if you near a turn limit, commit what works and reply with what is left.
- fmt, clippy `-D warnings`, nextest workspace, `cargo deny check` green.
- No PowerShell scripts, no `git stash`, no process killing, no `git reset --hard`; do not run `lpk-bench run`;
  the corpus tests read `bench/corpus/small` of the main checkout only, run once in release; do not edit
  `docs/DECISIONS.md`, `docs/PLAN.md`, `docs/STATUS.md`, `CLAUDE.md`, `bench/results/`, `docs/spec/`,
  `crates/lpk-format`, `crates/lpk-check`.
- Write your report to the file named in the dispatch: what you built per task, the three corpus tables, the
  test count, the four last lines. Return only status, commits, test count, the three acceptance verdicts in
  words, concerns.
