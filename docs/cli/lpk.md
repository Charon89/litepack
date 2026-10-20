# Command-Line Help for `lpk`

This document contains the help content for the `lpk` command-line program.

**Command Overview:**

* [`lpk`↴](#lpk)
* [`lpk a`↴](#lpk-a)
* [`lpk l`↴](#lpk-l)
* [`lpk x`↴](#lpk-x)
* [`lpk t`↴](#lpk-t)
* [`lpk check`↴](#lpk-check)
* [`lpk repair`↴](#lpk-repair)
* [`lpk append`↴](#lpk-append)
* [`lpk rollback`↴](#lpk-rollback)
* [`lpk info`↴](#lpk-info)
* [`lpk export`↴](#lpk-export)

## `lpk`

LitePack archiver: create, extract and test .lpk archives

**Usage:** `lpk [OPTIONS] <COMMAND>`

Exit codes: 0 ok, 1 usage error, 2 an operation was refused or failed. A failure is one line on stderr, `error: <message>`; with --json it is one JSON object line instead: {"error":{"code":1|2,"kind":"<the error's variant name; Usage for a command-line error; UnneededKeyfile for a --keyfile the archive does not take>","message":"...","path":"<the path the error names, else the archive>","entries":[{"path","kind","message"}, the per-entry errors of a partial extraction, when there are some]}}. A password is asked for on the terminal only with --password-prompt, or when stdin and stderr are terminals and the archive needs one that was not given; never through a pipe.

###### **Subcommands:**

* `a` — Create an archive from the contents of a directory (not the directory itself)
* `l` — List an archive, one line per entry: kind, size, mtime (UTC), path (tab-separated)
* `x` — Extract every entry of an archive into a directory; an existing file is refused unless --overwrite says otherwise, and symlink entries are skipped unless --symlinks is given
* `t` — Test an archive: frame hashes, chunk hashes, Merkle root, records, recovery shards
* `check` — Hash the data shards the recovery frames cover and report the damage (exit 2 if any)
* `repair` — Write a copy of an archive with its damaged shards rebuilt from the recovery frames
* `append` — Append the contents of a directory to an archive as its next generation: a snapshot (paths the directory no longer has are removed), deduplicated against the archive's chunks. A failed append leaves the archive as it was
* `rollback` — Truncate an archive to the end of an earlier generation (in place; `info` lists them). Also recovers an archive whose last append was cut short. No key is needed: the credential flags, --password-prompt included, are accepted and ignored
* `info` — Print an archive's header, envelope, counts and generations
* `export` — Write every entry of an archive into a ZIP or 7z file (by the output's extension), streamed in entry order: no tree on disk, the blocks decoded as for `x`

###### **Options:**

* `--json` — Machine-readable output: `l` prints one JSON object per entry per line, and any failing command prints exactly one JSON object line on stderr instead of `error: ...`



## `lpk a`

Create an archive from the contents of a directory (not the directory itself)

**Usage:** `lpk a [OPTIONS] <ARCHIVE> <INPUT>`

###### **Arguments:**

* `<ARCHIVE>` — The archive to create; must not exist
* `<INPUT>` — The directory whose contents are archived

###### **Options:**

* `--tier <TIER>` — The tier by name: the long form of --fast, --balanced and --store (one or the other; `strong` is reserved for the Strong tier, Phase 2, and refused)

  Possible values:
  - `fast`
  - `balanced`
  - `store`
  - `strong`:
    Reserved (Phase 2): refused

* `--fast` — The Fast tier (zstd with a long window; the default)
* `--balanced` — The Balanced tier (LZMA with a large dictionary, or zstd --ultra --long per block by a trial on a sample). Memory: about the block size plus several times the dictionary to compress; the block size plus the dictionary to extract
* `--store` — Store every block without compression
* `--strong` — Reserved for the Strong tier (Phase 2): refused with exit 1 until it arrives
* `--threads <N>` — Blocks encoded at once at most (default: the machine's logical cores; 1 is sequential). The archive is the same whatever the count. The memory budget may allow fewer (shown with -v)
* `--memory <SIZE>` — The memory budget of the whole run, for example 512M or 2G (binary units) or plain bytes. Default: half of the physical memory, at most 1 GiB (--store), 2 GiB (--fast) or 4 GiB (--balanced). The blocks in flight are derived from it; a budget below what one block in flight needs is refused with that figure
* `--level <L>` — zstd level (of the Fast tier, or of the Balanced tier's zstd candidate; default 22 there)
* `--window-log <W>` — log2 of the zstd match window, 10 to 28 (the reader's default limit caps it)
* `--dict-size <BYTES>` — The Balanced tier's LZMA dictionary in bytes (default 64 MiB)
* `--no-dictionaries` — Use no dictionaries (what the Fast tier does today: none is bundled)
* `--no-dedup` — Do not deduplicate chunks (the Fast and Balanced tiers cut files with content-defined chunking and store each distinct chunk once by default, across files and within a file). This also returns to the fixed 1 MiB cut
* `--ordering <ORDER>` — How files are ordered inside a cluster before they are written: `none` (the default: path order, which keeps directory locality) or `extension` (by extension, then name, the order 7-Zip uses for solid archives). Ignored with --no-dedup and --store

  Default value: `none`

  Possible values: `none`, `extension`

* `--deflate-peel <ON|OFF>` — The Deflate peel: Deflate streams inside ZIP, Office, JAR, PNG, gzip and PDF files (and zlib streams found in other binary files) stored as their plain text plus a reconstruction record that restores them bit for bit (revision 1.2). Default: on with --balanced, off with --fast. An append runs it only on a revision 1.2 archive

  Possible values: `on`, `off`

* `--bwt <ON|OFF>` — The Balanced tier's BWT stage (revision 1.3): a block of text is also tried as a Burrows-Wheeler transform followed by LZMA and written so when that is smaller. Default: on with --balanced; the Fast tier never uses it. An append runs it only on a revision 1.3 archive

  Possible values: `on`, `off`

* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none
* `--suite <SUITE>` — The cipher suite of an encrypted archive (default aes-256-gcm)

  Possible values: `aes-256-gcm`, `xchacha20-poly1305`

* `--listable` — Leave the entry table in clear, so the encrypted archive lists without the key
* `--recovery <PERCENT>` — Recovery records as a percentage of the archived data, 0 (none, the default) to 20: Reed-Solomon frames interleaved with the data they protect; `lpk repair` rebuilds damaged shards up to that share of each group

  Default value: `0`
* `-v`, `--verbose` — Print the counts and the stage times to stderr



## `lpk l`

List an archive, one line per entry, streamed in table order: kind (File, Directory or Symlink), size (bytes; a symlink's target length), mtime (UTC, ISO 8601 to the second; `-` when the archive records none) and path, tab-separated, control characters in the path escaped as the reference tool escapes them (`\n`, `\t`, `\u{1}`). --long adds the flags column (`xhrs`: executable, hidden, read-only, system; `-` for a flag not set) before the path, a symlink's target after ` -> `, and a first line `generation<TAB>N` with the archive's generation (the format records one per archive, not per entry).

With --json each line is one object: "kind", "size", "mtime" (the string above, or null), "mtime_ns" (nanoseconds since the Unix epoch, or null), "flags" (an array of "executable", "hidden", "read-only", "system"), "flag_bits" (the raw flag word), "path" (unescaped: a JSON string), "symlink_target" (a symlink's target, else null), "chunks" (the number of chunks the file references) and "generation" (the archive's).

A sealed archive written with --listable lists without its key; one that was not is refused with PasswordRequired (exit 2) unless a credential is given.

**Usage:** `lpk l [OPTIONS] <ARCHIVE>`

###### **Arguments:**

* `<ARCHIVE>` — The archive

###### **Options:**

* `--long` — Add the flags column and the symlink targets, after a first line with the archive's generation
* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none



## `lpk x`

Extract every entry of an archive into a directory; an existing file is refused unless --overwrite says otherwise, and symlink entries are skipped unless --symlinks is given

**Usage:** `lpk x [OPTIONS] <ARCHIVE> <OUTDIR>`

###### **Arguments:**

* `<ARCHIVE>` — The archive
* `<OUTDIR>` — The directory to extract into; created if missing

###### **Options:**

* `--threads <N>` — The total thread budget, decoders plus file writers (default: the machine's logical cores; at most four times that). 1 decodes and writes on one thread; from 2 on, clamp(N/4, 1, 4) threads write files and the rest decode blocks, the decoders capped so that the extraction stays within its memory budget (see --memory)
* `--prior <FILE>` — A prior file (for example a zstd dictionary) the archive needs; may be repeated
* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none
* `--memory <SIZE>` — The memory budget of the extraction, for example 512M or 2G or plain bytes; also the reader's decode memory limit (an archive whose envelope needs more is refused before anything is written). Default: half of the physical memory, at most the ceiling of the tier the archive's envelope shows: 1 GiB (stored blocks only), 2 GiB (Fast-sized blocks and windows) or 4 GiB (larger); the reader is then limited to half of the physical memory, at most 4 GiB. The decoders in flight are derived from the budget (shown with -v); a budget below what one block in flight needs is refused with that figure
* `--overwrite <MODE>` — What an existing file at a target gets. refuse: the extraction is refused before any output (the default). skip: the file is left as it is and counted (`skipped <n> existing entries` on stderr; with --json one `{"summary":{...}}` line). rename: the entry is written beside it as `<stem> (<k>)<.ext>` with the smallest free k, and counted. replace: the file is replaced (a link at the target is removed as a link, never followed). A directory where a file goes, or a file where a directory goes, is refused in every mode but skip, which skips it (and everything below a skipped directory). Directories are created or reused whatever the mode

  Default value: `refuse`

  Possible values: `refuse`, `skip`, `rename`, `replace`

* `--symlinks` — Create symlink entries with the target the archive holds. Without this flag every symlink entry is skipped with a warning on stderr (with --json one `{"warning":{"kind":"SymlinkSkipped","path":...}}` line). A link that cannot be created (for example without the privilege on Windows) fails the extraction
* `-v`, `--verbose` — Print the plan's counts, the memory budget and its terms, and the pool size to stderr



## `lpk t`

Test an archive: frame hashes, chunk hashes, Merkle root, records, recovery shards

**Usage:** `lpk t [OPTIONS] <ARCHIVE>`

###### **Arguments:**

* `<ARCHIVE>` — The archive

###### **Options:**

* `--prior <FILE>` — A prior file (for example a zstd dictionary) the archive needs; may be repeated
* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none
* `--memory <SIZE>` — The decode memory limit, as for `x`: an archive whose envelope needs more is refused



## `lpk check`

Hash the data shards the recovery frames cover and report the damage (exit 2 if any)

**Usage:** `lpk check [OPTIONS] <ARCHIVE>`

###### **Arguments:**

* `<ARCHIVE>` — The archive

###### **Options:**

* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none



## `lpk repair`

Write a copy of an archive with its damaged shards rebuilt from the recovery frames

**Usage:** `lpk repair [OPTIONS] <ARCHIVE> <OUT>`

###### **Arguments:**

* `<ARCHIVE>` — The archive
* `<OUT>` — The repaired copy; must not exist. Kept with every repair that was possible when a frame had more damage than it can rebuild (exit 2 then)

###### **Options:**

* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none



## `lpk append`

Append the contents of a directory to an archive as its next generation: a snapshot (paths the directory no longer has are removed), deduplicated against the archive's chunks. A failed append leaves the archive as it was

**Usage:** `lpk append [OPTIONS] <ARCHIVE> <INPUT>`

###### **Arguments:**

* `<ARCHIVE>` — The archive; must exist
* `<INPUT>` — The directory whose contents become the new generation

###### **Options:**

* `--tier <TIER>` — The tier by name: the long form of --fast, --balanced and --store (one or the other; `strong` is reserved for the Strong tier, Phase 2, and refused)

  Possible values:
  - `fast`
  - `balanced`
  - `store`
  - `strong`:
    Reserved (Phase 2): refused

* `--fast` — The Fast tier (zstd with a long window; the default)
* `--balanced` — The Balanced tier (LZMA with a large dictionary, or zstd --ultra --long per block by a trial on a sample). Memory: about the block size plus several times the dictionary to compress; the block size plus the dictionary to extract
* `--store` — Store every block without compression
* `--strong` — Reserved for the Strong tier (Phase 2): refused with exit 1 until it arrives
* `--threads <N>` — Blocks encoded at once at most (default: the machine's logical cores; 1 is sequential). The archive is the same whatever the count. The memory budget may allow fewer (shown with -v)
* `--memory <SIZE>` — The memory budget of the whole run, for example 512M or 2G (binary units) or plain bytes. Default: half of the physical memory, at most 1 GiB (--store), 2 GiB (--fast) or 4 GiB (--balanced). The blocks in flight are derived from it; a budget below what one block in flight needs is refused with that figure
* `--level <L>` — zstd level (of the Fast tier, or of the Balanced tier's zstd candidate; default 22 there)
* `--window-log <W>` — log2 of the zstd match window, 10 to 28 (the reader's default limit caps it)
* `--dict-size <BYTES>` — The Balanced tier's LZMA dictionary in bytes (default 64 MiB)
* `--no-dictionaries` — Use no dictionaries (what the Fast tier does today: none is bundled)
* `--no-dedup` — Do not deduplicate chunks (the Fast and Balanced tiers cut files with content-defined chunking and store each distinct chunk once by default, across files and within a file). This also returns to the fixed 1 MiB cut
* `--ordering <ORDER>` — How files are ordered inside a cluster before they are written: `none` (the default: path order, which keeps directory locality) or `extension` (by extension, then name, the order 7-Zip uses for solid archives). Ignored with --no-dedup and --store

  Default value: `none`

  Possible values: `none`, `extension`

* `--deflate-peel <ON|OFF>` — The Deflate peel: Deflate streams inside ZIP, Office, JAR, PNG, gzip and PDF files (and zlib streams found in other binary files) stored as their plain text plus a reconstruction record that restores them bit for bit (revision 1.2). Default: on with --balanced, off with --fast. An append runs it only on a revision 1.2 archive

  Possible values: `on`, `off`

* `--bwt <ON|OFF>` — The Balanced tier's BWT stage (revision 1.3): a block of text is also tried as a Burrows-Wheeler transform followed by LZMA and written so when that is smaller. Default: on with --balanced; the Fast tier never uses it. An append runs it only on a revision 1.3 archive

  Possible values: `on`, `off`

* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none
* `--recovery <PERCENT>` — Recovery records for the new generation as a percentage of its data, 0 (none, the default) to 20, as for `a`

  Default value: `0`
* `-v`, `--verbose` — Print the counts and the stage times to stderr



## `lpk rollback`

Truncate an archive to the end of an earlier generation (in place; `info` lists them). Also recovers an archive whose last append was cut short. No key is needed: the credential flags, --password-prompt included, are accepted and ignored

**Usage:** `lpk rollback [OPTIONS] <ARCHIVE> <GENERATION>`

###### **Arguments:**

* `<ARCHIVE>` — The archive; changed in place
* `<GENERATION>` — The generation to keep (0 is the first write; `info` shows the latest)

###### **Options:**

* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none



## `lpk info`

Print an archive's header, envelope, counts and generations

**Usage:** `lpk info [OPTIONS] <ARCHIVE>`

###### **Arguments:**

* `<ARCHIVE>` — The archive

###### **Options:**

* `--prior <FILE>` — A prior file (for example a zstd dictionary) the archive needs; may be repeated
* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none



## `lpk export`

Write every entry of an archive into a ZIP or a 7z file, chosen by the output's extension (.zip or .7z; anything else is a usage error). The entries are streamed in entry order through the extraction engine (the same plan, thread budget and memory budget as `x`; the decoded blocks are cached under the budget by a schedule computed from the whole plan, so a deduplicated chunk referring back to an earlier block is decoded again only when the cache cannot hold it): no tree is written on disk and no whole file is held in memory.

ZIP: one entry per archive entry, Zip64 where an entry or the directory needs it, UTF-8 names, the modification time as DOS time and as the extended-timestamp field, a Unix mode with the executable flag and the DOS read-only, hidden and system bits; a symlink entry is a ZIP symlink (its target as the stored content); --method deflate (default), zstd (method 93) or store; --encrypt writes AES-256 (AE-2) entries with the archive's password.

7z: one entry per archive entry, each in its own stream (an empty file as a stream-less entry); --method lzma2 (default), deflate or store (zstd inside 7z is a fork extension 7-Zip does not read: refused); --encrypt writes AES-256 with an encrypted header.

Names are written as the archive stores them: the format's reader already refuses absolute paths and `..`; a colon, a device name or a trailing dot or space goes through as-is for the consumer's extractor to sanitise. Both writers run in process. The archive, its key slot and the plan are checked before the output exists; the output is refused when it is the archive itself, and when it exists unless --force replaces it (touched only once the export writes). A failed export removes the output; a panic leaves it in place. Outside the memory budget: the writer's directory, kept until the end (ZIP's central directory about 200 bytes plus the name per entry, 7z's entry table about 100 plus the name), and its compressor (Deflate under 1 MiB, zstd a few MiB, LZMA2 about eleven times its 16 MiB dictionary); -v prints the figures.

**Usage:** `lpk export [OPTIONS] <ARCHIVE> <OUT>`

###### **Arguments:**

* `<ARCHIVE>` — The archive
* `<OUT>` — The file to write: `.zip` or `.7z` by its extension; must not exist unless --force

###### **Options:**

* `--method <METHOD>` — The compression method of the entries: deflate (ZIP's default), zstd (ZIP only), store, or lzma2 (7z's default; 7z only)

  Possible values:
  - `deflate`
  - `zstd`
  - `store`
  - `lzma2`:
    7z only

* `--encrypt` — Encrypt every entry with the archive's password (AES-256: ZIP's AE-2, 7z's with the header encrypted). Needs a credential with a password; a keyfile alone is refused
* `--force` — Replace an existing output file
* `--threads <N>` — The thread budget: 1 decodes on the calling thread; from 2 on, N - 1 threads decode blocks ahead of the writer (capped by the memory budget), as for `x`
* `--prior <FILE>` — A prior file (for example a zstd dictionary) the archive needs; may be repeated
* `--password <STR>` — The password. Visible in the process list and the shell history; prefer --password-file or --password-prompt
* `--password-file <PATH>` — A file holding the password; one trailing newline is not part of it
* `--password-prompt` — Ask for the password on the terminal, typed without echo (twice for `a`). Needs stdin and stderr to be terminals, else refused (exit 1). Without this flag the password is asked for only when the archive needs one that was not given and stdin and stderr are terminals; through a pipe the command fails with PasswordRequired instead
* `--keyfile <PATH>` — A keyfile as a second factor (alone: the password is empty). Refused on an archive whose key slot takes none
* `--memory <SIZE>` — The memory budget, as for `x`: the decoded blocks held at once (cached or in flight) are derived from it; the writer's own memory (its compressor) is outside the count
* `-v`, `--verbose` — Print the plan's counts, the cache schedule's outcome and the memory terms to stderr



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>
