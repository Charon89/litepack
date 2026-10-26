# LitePack engine API v0 (`lpk-api`)

E5-1, 2026-10-25. The crate `crates/lpk-api` is the only engine surface of the GUI (E5-2): a
manifest test (`crates/lpk-api/tests/gui_seam.rs`) fails when `lpk-gui` depends on any `lpk-*`
crate but `lpk-api` and `lpk-gui-sys`, or names `lpk_core::` or `lpk_format::` in its sources.
The `lpk` command line runs `l`, `x`, `a`, `append`, `export`, `check`, `repair` and `rollback`
through it; `t` and `info` stay on the reference tool (design §2). Rustdoc is the reference;
this page is the overview. v0 is unstable: it changes with the GUI tasks.

## Public surface

| Item | What it does |
|---|---|
| `Session::open(path, &OpenOptions)` | Opens an `.lpk` in process (credentials, decode-memory limit, priors, an earlier `generation` read-only) or a foreign archive through E4's sandboxed broker (by magic; listed only when first asked). |
| `Session::open_file(File, path, &OpenOptions)` | The same for an `.lpk` the caller opened (its open error stays the caller's). |
| `Session::unlock(&mut self, Credentials)` | Re-opens with credentials. |
| `Session::info(&self)` | Format, generation (open and latest), encrypted, listable, keyless, length. |
| `Session::for_each(&self, &Selection, f)` | Streams the entries, none collected (E3-1's bound); a glob matching nothing is `NoMatch` before `f` runs. |
| `Session::model(&self, &CancelToken)` | The listing model, built on first call and shared: `ListModel::view(scope, order, filter, c)`, `rows(view, first, n)`, `totals(view, picks)`, `children(id)`, `new_selection()`. |
| `Session::read_entry(&self, id, cap, &CancelToken)` | One file's bytes, refused past `cap` (`Refused`, field `preview cap`); through the export engine, cancellable per block. |
| `Session::preflight(&self, &Selection, &Destination, &CancelToken)` | The destination, entry and byte counts, existing targets, before any write. |
| `Session::extract(&self, &Selection, &Destination, &ExtractOptions, &Monitor)` | E3-2's policy, overwrite and symlink modes; `.lpk` or foreign. |
| `Session::export(&self, &Selection, out, &ExportOptions, &Monitor)` | ZIP or 7z (E3-6); `check_export_target` refuses the archive itself and an existing output without `force`. |
| `Session::test` / `check` / `repair(out)` | Verify, the recovery scan, a repaired copy; per-block progress and cancel (the `lpk-format` hooks `verify_with`, `check_recovery_with`, `repair_into`). |
| `Session::history(&self, &CancelToken)` | The trailer chain. Exclusive: cancels the reads in flight and waits for them. |
| `Session::rollback(self, g)` / `append(self, dir, &CreateSettings, &Monitor)` | Exclusive: cancel and wait for the reads in flight, close, then run `rollback` / `append` (append returns the re-opened session). |
| `Session::convert` | E5-7's `--convert`: `Unsupported` in v0 (needs E4-3's readers). |
| `create(&Sources, out, &CreateSettings, &Monitor)` | A new archive; `CreateSettings` holds the tier flags (`Tier` cannot name Strong), `seal`, `recovery`. |
| `append(archive, dir, …)`, `rollback(path, g)` | The same without a session (an archive whose last append was cut short does not open). |
| `calibrate(&Sources, &CreateSettings, sample, &Monitor)` | Runs the pipeline in memory on the first `sample` bytes of files: a `Rate`, E5-5's only source of estimates. |

`Selection` is `All`, `Globs` (D-90) or `Ids(Arc<IdSet>)` (picks as a bit set; an extraction
of ids spells each path as an exact glob in v0). `Destination` is `Dir`, `Smart { beside }` or
`NewFolder { beside }`. A `Session` is `Send + Sync` (a compile-time test asserts it).

**Model memory bound** over the reader's open, for `N` entries with `P` path bytes:
`N·(size_of::<Row>() + 4·size_of::<u32>()) + P + ⌈N/8⌉ + 1 MiB`, asserted on E3-1's
million-entry archive by `crates/lpk-api/tests/model_memory.rs`.

## Errors

`Error { class, kind, message, path, entries }` plus `refusal()` (an envelope refusal's
figures). `kind` is D-77's (the engine variant's name, what `lpk --json` prints); `class` keys
the message. `Class::Usage` is exit 1, every other class exit 2. The enum is `#[non_exhaustive]`.

| Class | Message |
|---|---|
| Usage | The request is not valid. |
| NotFound | A file or entry was not found. |
| AccessDenied | Access was denied. |
| DiskFull | The disk is full. |
| PasswordRequired | This archive needs a password. |
| WrongPassword | The password is wrong. |
| Damaged | The archive is damaged. |
| Unsupported | This archive needs a feature this version does not have. |
| TooLarge | This exceeds a limit. |
| Unsafe | A path was refused as unsafe. |
| Exists | Something is in the way. |
| Cancelled | The operation was cancelled. |
| Sandbox | The sandboxed reader failed. |
| Io | A read or write failed. |
| Internal | Something went wrong inside LitePack. |

## Activation verbs (E5-7)

The verbs `lpk-gui.exe` accepts (design §5); E6's menu sends them, and a test of E5-7 compares
the binary's parser with this table.

| Verb | Arguments | Menu entry | Action |
|---|---|---|---|
| `--open` (or a bare path) | `<archive>` | Open | Opens the archive in the browser. |
| `--extract-here` | `<archive>...` | Extract here | Smart destination beside each archive. |
| `--extract-to` | `<archive>...` | Extract to "&lt;name&gt;\" | Into `<name>\` beside each, `<name> (k)` if taken. |
| `--create` | `<path>...` | Add to archive… | Opens the create wizard with these inputs. |
| `--create … --name` | `<path>... --name <file>.lpk` | Add to "&lt;name&gt;.lpk" | Creates at once with the defaults. |
| `--test` | `<archive>...` | Test | Tests each archive. |
| `--convert` | `<archive>` | Convert to… | Re-creates a foreign archive as `.lpk` (E4 §6). |

`@file` response files carry longer argument lists.
