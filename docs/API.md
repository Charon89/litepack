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
| `OpenOptions::motw_off` | Mark of the Web opted out (E6-5, `--motw off`): files the session derives from the archive (extraction, export, repair) do not get its zone. Default `false`: an archive whose `Zone.Identifier` gives zone 3 or 4 (or cannot be parsed or read: 3) passes `ZoneId` only to each output, written before its first byte; `ExtractReport::zone` and `motw_unmarked` report it. |
| `Session::zone(&self)` / `zone_warning(&self)` | The archive's propagating zone, read once at open through the archive's handle (reported whatever `motw_off` says: an opened entry is zoned regardless), and the one warning when the stream could not be read and was taken as zone 3. |
| `Session::unlock(&mut self, Credentials)` | Re-opens with credentials. |
| `Session::info(&self)` | Format, generation (open and latest), encrypted, listable, keyless, length. |
| `Session::for_each(&self, &Selection, f)` | Streams the entries, none collected (E3-1's bound); a glob matching nothing is `NoMatch` before `f` runs. |
| `Session::model(&self, &CancelToken)` | The listing model, built on first call and shared: `ListModel::view(scope, order, filter, c)`, `rows(view, first, n)`, `totals(view, picks)`, `children(id)`, `new_selection()`. |
| `Session::read_entry(&self, id, cap, &CancelToken)` | One file's bytes, refused past `cap` (`Refused`, field `preview cap`, `TooLarge`; by the declared size before any data is read, and again on the streamed bytes). An `.lpk` entry goes through the export engine; a foreign (ZIP, 7z) member, chosen by its listing position with the listing fingerprint checked (`ArchiveChanged` on a mismatch), is decompressed by E4's sandboxed worker into memory, nothing written to disk. Cancellable; only a regular file is read (a directory, link or hard link is refused). |
| `Session::preflight(&self, &Selection, &Destination, &CancelToken)` | The destination, entry and byte counts, existing targets, before any write. |
| `Session::extract(&self, &Selection, &Destination, &ExtractOptions, &Monitor)` | E3-2's policy, overwrite and symlink modes; `.lpk` or foreign. |
| `Session::export(&self, &Selection, out, &ExportOptions, &Monitor)` | ZIP or 7z (E3-6); `check_export_target` refuses the archive itself and an existing output without `force`. |
| `Session::test` / `check` / `repair(out)` | Verify, the recovery scan, a repaired copy; per-block progress and cancel (the `lpk-format` hooks `verify_with`, `check_recovery_with`, `repair_into`). |
| `Session::history(&self, &CancelToken)` | The trailer chain. Exclusive: cancels the reads in flight and waits for them. |
| `Session::close(&self)` | Exclusive: cancels the reads in flight (their callers' tokens untouched), waits for them, and closes the session; every later call is `Usage`, kind `SessionClosed`. |
| `Session::rollback(self, g)` / `append(self, dir, &CreateSettings, &Monitor)` | Exclusive: close, then run the free `rollback` / `append` (append returns the re-opened session). Taking `self`, they suit a sole owner. |
| `Session::convert` | E5-7's `--convert`: `Unsupported` in v0 (needs E4-3's readers). |
| `create(&Sources, out, &CreateSettings, &Monitor)` | A new archive; `CreateSettings` holds the tier flags (`Tier` cannot name Strong), `seal`, `recovery`. Since E2-26 (v0 change) `ordering` is an `Option<FileOrder>`: `ordering: None` means the tier's default (`Tier::default_ordering`: Balanced `versions`, Fast and Store `none`); read the resolved value with `CreateSettings::ordering()`. |
| `append(archive, dir, …)`, `rollback(path, g)` | The same without a session (an archive whose last append was cut short does not open). |
| `committed_generation(path)` | The latest committed generation from the trailer chain, read-only (a torn append's tail skipped): how the GUI's recovery tells a committed append from a torn one (E5-2). |
| `calibrate(&Sources, &CreateSettings, sample, &Monitor)` | Runs the pipeline in memory on the first `sample` bytes of files: a `Rate`, E5-5's only source of estimates. |

`Selection` is `All`, `Globs` (D-90) or `Ids(Arc<IdSet>)` (picks as a bit set). For an
extraction or export, ids follow D-90's directory rule: **a picked directory is its whole
subtree** (the entries under it need no pick of their own, whatever the table order, and
unpicked ones under it are included); every entry picked is the whole archive; no entry picked is
a usage error. The picks reach the engine as an id set (`lpk_core::Selection::from_ids`, the index
in the entry table; for a foreign archive the index in its listing): no cap on the picks and no
restriction on names. `Destination` is `Dir`, `Smart { beside }` or
`NewFolder { beside }`.

**Sharing.** A `Session` is `Send + Sync` (a compile-time test asserts it). Each read is
registered before it starts, with its own cancel token, a child of the caller's
(`CancelToken::child`): the caller's token cancels it, and an exclusive operation (`history`,
`close`, `rollback`, `append`) cancels it without cancelling the caller's token. An exclusive
operation still waiting when `close` takes the session fails with `SessionClosed`; every call
after `close` (a cached model included) is refused. Once an
exclusive operation is pending no new read starts; it waits for the reads in flight to end.
Callbacks (`for_each`'s `f`) must not call back into the `Session`: a nested read would wait
behind a pending exclusive operation that waits for the outer read.

**A GUI holding `Arc<Session>`** (shared by its command threads) rolls back or appends with
`session.close()` — the reads in flight end `Cancelled` and every later call on that session is
refused — then the free `lpk_api::rollback(path, g)` or `lpk_api::append(path, dir, …)`, then
`Session::open` again for the new state.

**Model memory bound** over the reader's open, for `N` entries with `P` path bytes:
`N·(size_of::<Row>() + 4·size_of::<u32>()) + P + ⌈N/8⌉ + 1 MiB`, asserted on E3-1's
million-entry shape (names and sizes shuffled; paged, filtered and re-sorted) by
`crates/lpk-api/tests/model_memory.rs`, which also holds `for_each` to E3-1's listing bound.

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
| Unsupported | This is not supported (see the detail): an archive feature this version does not have, or an operation this archive does not allow. |
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
| `--open` (or a bare path) | `<archive>` | Open | Opens the archive in the browser. `--open=<archive>` is the same (the form the Edge WebDriver can pass, E5-11). |
| `--extract-here` | `<archive>...` | Extract here | Smart destination beside each archive (E5-4), queued one archive at a time; no destination question. The password prompt, an envelope refusal ("Allow X"), progress with Cancel and a result per archive appear as in the wizard. |
| `--extract-to` | `<archive>...` | Extract to "&lt;name&gt;\" | Into `<name>\` beside each, `<name> (k)` if taken (E5-4); same queue, prompts and results as `--extract-here`. |
| `--create` | `<path>...` | Add to archive… | Opens the create wizard with these inputs. `--create=<path>` (one input) is the same (the form the Edge WebDriver can pass, E5-5). |
| `--create … --name` | `<path>... --name <file>.lpk` | Add to "&lt;name&gt;.lpk" | Creates at once with the wizard's defaults (Balanced, recovery 5, test when done: one set of GUI defaults for every GUI entry point), through the wizard's progress dialog and summary. |
| `--test` | `<archive>...` | Test | Tests each archive. `--test=<archive>` (one archive) is the same (the form the Edge WebDriver can pass, E5-8). |
| `--convert` | `<archive>` | Convert to… | Re-creates a foreign archive as `.lpk` (E4 §6). |

`@file` response files carry longer argument lists: UTF-8 text (an optional byte-order mark), one argument
per line, blank lines skipped, not recursive (a line starting with `@` is a path), at most 16 MiB across a
command line's files. They are for users and scripts; E6's menus pass their paths in the binary `.verbs` files
of `docs/notes/e6-design.md` ("Invoke: the contract with the GUI"), read by `lpk-gui`'s `shellfile` (E6-2).

**The menu's response file (format v1).** `lpk_shell.dll` writes `shell-<32 lowercase hex>.verbs` (create-new)
into `%LOCALAPPDATA%\org.litepack.gui\Verbs` (the installed GUI's local state, whatever a portable copy's own
folder) and starts `"<GuiPath>" @<file>` (the `@` argument quoted when the path holds a space), `GuiPath` being
the string value `HKCU\Software\LitePack\GuiPath`; with none, or not an existing file, it starts nothing and
writes nothing. Little-endian: the 8 ASCII bytes `LPKVERBS`; a `u16` version (1); the verb as a `u16` count of
UTF-16 code units and the units (`--open`, `--create`, `--extract-here`, `--extract-to`, `--test` or `--convert`);
the `--name` value as a `u32` count and the units (count 0 when the verb has none); a `u32` path count; per path a
`u32` count of UTF-16LE code units and the units, unpaired surrogates kept; nothing follows the last path. The GUI
reads such a file only when it is in that folder and so named (any other `@file` is the text format above), accepts
version 1, refuses a wrong magic or version, a truncated file or one with trailing bytes, more than 16 MiB, more than
1,048,576 paths, a path over 32,767 units or an empty one, rebuilds each path losslessly (`OsString::from_wide`),
and deletes that file, never another `@file`, once the instance that acts on the verb has it (a second instance
forwards the argument and leaves the file to the first); at start-up it removes such files older than 24 hours.
`crates/lpk-gui/tests/data/shell-v1.verbs` is the one test vector: the writer's test produces exactly its bytes and
the reader's test parses it.

## Shell registration verbs (E6-4)

`lpk-gui.exe` registers itself with Explorer for the current user, and removes that again, from the command line
alone (the installer, the uninstaller and the settings page run them). They are handled before the window, the
webview and the single-instance plugin start: no window, never forwarded to a running instance, and only from the
literal command line (not from an `@file`, not mixed with the verbs above or with any other argument).

- `--register-shell`: undoes the previous registration, then writes the keys below.
- `--register-shell --open-with`: the same, and also lists the foreign archive formats in "Open with".
- `--unregister-shell`: removes what the record names, and the record.

Exit codes: 0 done; 1 failed (what was written is in the record, `--unregister-shell` undoes it); 2 refused, with
nothing written: a portable copy (the `portable` marker beside the exe) never registers, unknown or extra
arguments (also one that is not valid Unicode), `lpk_shell.dll` missing beside the exe, or an install path
containing `%` (Explorer would substitute it in the open command). Both verbs end with `SHChangeNotify(SHCNE_ASSOCCHANGED)`.

What `--register-shell` writes, all under `HKEY_CURRENT_USER`, values `REG_SZ` unless noted:

- `Software\LitePack\GuiPath`: the path of the running `lpk-gui.exe` (what `lpk_shell.dll` starts).
- Route C, Explorer's classic menu: `Software\Classes\CLSID\{01E42821-7D51-4B9B-AD9F-FFCA769A6025}` (default
  `LitePack`; `InProcServer32` default = `lpk_shell.dll` beside the exe, `ThreadingModel` = `Apartment`), and under
  each of `*`, `Directory` and `Directory\Background` the key `Software\Classes\<type>\shell\LitePack` with
  `ExplorerCommandHandler` = that class ID and `MultiSelectModel` = `Player`.
- `.lpk`: the ProgID `LitePack.lpk` (`shell\open\command` = `"<exe>" --open "%1"`, `DefaultIcon`), `.lpk`'s default
  value set to it and `PerceivedType` = `compressed` only when absent, and `.lpk\OpenWithProgids` listing it (a
  `REG_NONE` value without data).
- With `--open-with`: a `LitePack.<ext>` ProgID of the same shape for each of `zip`, `7z`, `tar`, `tgz` and `rar`
  (E4-4), listed in that extension's `OpenWithProgids`, never as its default.
- Default Programs: `Software\LitePack\Capabilities` and `Software\RegisteredApplications\LitePack`.
- `Software\LitePack\Registration`: the record of every key created and every value written, written ahead of each
  change. Unregistering deletes each recorded value whose data is still the recorded one, then each recorded key
  that is empty, deepest first, then the record; a value someone else changed since stays.

`UserChoice` and everything under `FileExts` are never written: the user's default handlers do not change.
