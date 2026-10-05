# E2-12 / E2-13 / E2-14 — resume notes (the implementer stopped before writing code on 2026-10-04)


Branch `task/e2-12-14-seal`, pushed to origin at `0fb0118` (no change from `main` yet). I wrote no code before
the stop, so there was nothing to commit. On the clean tree, `cargo fmt --all` changed nothing and
`cargo build --workspace` passed. Clippy, nextest and deny were not run (the coordinator said not to). There are
no new tests and no corpus tables.

## Per task
- E2-12 (encryption): untouched. Only the design below exists.
- E2-13 (recovery records): untouched. Only the design below exists.
- E2-14 (journal): untouched. Investigating it turned up a format blocker (below) that needs an owner decision.

## BLOCKER for E2-14 (needs a decision: NEEDS_CONTEXT)
`lpk_format::Writer::add_record` refuses in an append: `if self.base.is_some() { return Err(bad_options("add_record in an append")) }`
(crates/lpk-format/src/writer.rs, around line 1592). The JPEG peel creates its record while writing
(`pipeline.rs::write_peeled` calls `writer.add_record(stage.record(&plan, &lists))`, and the record holds the
chunk indices of the nested parts). So the peel cannot run inside an appended generation unless the format
changes. The brief forbids editing `crates/lpk-format`. Options:
1. A small format addition: allow `add_record` in an append when it adds after the old records. Old blocks name
   records by position, so keeping the old records first is safe. This means pre-loading the old records into
   `options.records` (the append constructor already checks that a non-empty `options.records` keeps them
   unchanged) and dropping the guard.
2. Without a format change: in an append, files of peel classes go through `add_file` unpeeled. This needs to be
   documented and reported as a concern. The E2-14 acceptance (size within the frame overhead, rollback
   bit-exact) can probably still pass this way, but the brief's "peel applies to the new generation" does not.

## Design worked out (for whoever resumes)

### E2-12
- `lpk a` gets `--password STR` (conflicts with `--password-file`; its help says it is visible in the process
  list), `--password-file PATH`, `--keyfile PATH`, `--listable` and `--suite aes-256-gcm|xchacha20-poly1305` (the
  default is `Suite`'s default, see crypto.rs). Build `lpk_format::SealOptions { suite, argon2:
  Argon2Params::default(), listable, credentials }` and put it into `pipeline.seal.writer.seal`.
  `WriterOptions::seal` already exists, and the pipeline passes `seal.writer` into the writer untouched, so
  `run_file` needs no change.
- Password-file rule: copy it from `lpk_format::cli::credentials`, which is private: one trailing `\n` (and a
  `\r` before it) is not part of the password. A keyfile alone means an empty password. The password string
  should be held in `zeroize::Zeroizing` if that crate is reachable, but lpk-cli does not depend on it and no
  new dependency is allowed. Otherwise move it straight into `Credentials` (that type wipes itself on drop).
- `lpk x`: `lpk_core::extract_file` opens with `Archive::open` (extract.rs, around line 1035). Add a
  `credentials: Option<&Credentials>` parameter, or an `ExtractOptions` field (but `ExtractOptions` is `Copy`,
  so a parameter or a new `extract_file_with` is simpler). Open with `Archive::open_with(file,
  &Resources::default(), creds)`. A wrong password is `FormatError::WrongKey` from `open_with`, before `plan()`,
  which is where the output directory is created (extract.rs around line 542). Also refuse `a.is_keyless()`
  explicitly with `FormatError::PasswordRequired` before `plan()`. Workers use `archive.fork(reader)`, which
  carries the sealer.
- `lpk t`, `check`, `repair`, `rollback`, `info`: pass `--password`, `--password-file`, `--keyfile` (and
  `--prior`) as global arguments before the subcommand to `lpk_format::cli::run_with` (see `reference()` in
  crates/lpk-cli/src/lib.rs). The format tool already handles credentials, keyless listing, check, repair,
  rollback and info (which lists generations and the chain length).
- Runner: check the placeholders at the top of `bench/tools.toml` and in `crates/lpk-bench/src/run/`. If there
  is no `{password_file}`, add one that writes a fixed test password into the scratch directory and records it
  in `run.json`. Then add the setting `fast-encrypted` = `["--fast", "--password-file", "{password_file}"]`
  with the same credential for extraction.

### E2-13
- `lpk a --recovery <0..=20>` sets `pipeline.seal.writer.recovery.percent` (shard 64 KiB and group 2048 shards
  stay at their defaults). The Fast block size (64 MiB) fits the default 128 MiB group.
- `lpk check <archive>` and `lpk repair <in> <out>` go through `run_with` (`check` and `repair` subcommands).
- Test damage bound (spec section 13): per group, at most `recovery_shards = ceil(data_shards*p/100)` (at least
  1) damaged shards can be repaired. Geometry: read each `a.recovery_frames()` location with
  `lpk_format::ReadFrame::read` and parse it with `RecoveryFrame::parse(payload, index_offset)` to get
  `cover_offset`, `cover_len`, `shard_len` and `data_shards`. Damage `recovery_shards` distinct shards inside the
  cover, repair, then verify. Damage `recovery_shards+1` shards in one group, and repair must report it as
  unrepairable (repair_cmd returns Err).
- Overhead check: the plain archive is deterministic (`archive_id` is zero by default), so overhead = len(with
  recovery) - len(plain), and it should equal the sum over frames of (frame header + 32 + 32*data_shards +
  recovery_shards*shard_len) plus the index entries. The parity is `p%` of the covered bytes within one shard
  plus one shard of rounding per group.

### E2-14
- New `Pipeline::append_file(self, root, archive_path, credentials)` in pipeline.rs:
  1. prepare;
  2. walk (leave out the archive by `file_identity`, as `create_new_and_run` does);
  3. `Archive::open_with`, refusing a keyless archive;
  4. remember the file length;
  5. open the file for append;
  6. `Writer::append_with_chunker(existing, BufWriter(out), wopts, creds, fold.install(&mut wopts)?)
     .with_sync(...)`;
  7. run the same stage loop;
  8. for snapshot semantics, `delete_path` every old path that is absent from the new tree;
  9. on any error, `file.set_len(original_len)`.

  Refactor `run_inputs` to take a target enum (new or append) instead of `out` and `sync`, so the stage loop is
  shared. `store.rs:62` also calls `run_inputs`. `dedup` works in an append (writer.rs `own` map and old
  `by_hash`), and `RunSummary.writer.reused_chunks` counts the cross-generation dedup.
- Generation numbering choice for the tests: `lpk a` of an empty directory is generation 0, then three appends
  (v1, v2, v3) are generations 1 to 3, so "rollback to generation 1 reproduces v1" holds literally. Then
  compare against the one-shot archive of the tree holding v1/, v2/ and v3/.
- `lpk append <archive> <dir>` (with the credential flags) and `lpk rollback <archive> <gen>` (through
  `run_with`). `lpk info` lists generations through `run_with` `info`.

## Merge note
E2-8 touches `fold/ordering.rs` and `pipeline.rs`. The `run_inputs` refactor above touches pipeline.rs around
`open_writer`, so expect a small conflict there.
