# LitePack `.lpk` format, version 1 — working draft

Status: this is a working draft. The format is not frozen until the independent-decoder gate (task E1-14).
Sections are added task by task; this revision covers conventions, the header, the frame grammar and the entry table.
The reference reader is the `lpk-format` crate; the tables below are checked against it by a test.

## 1. Conventions

- Byte order: every fixed-width integer is little-endian.
- Varint: unsigned LEB128 for a `u64`, seven bits per byte, low group first, the high bit of a byte
  meaning "more bytes follow". At most 10 bytes. Readers require the canonical (shortest) form: the last
  byte of a multi-byte varint must not be `0x00` (so `0x80 0x00` is an error), and a tenth byte, if
  present, must be exactly `0x01`; a varint that continues past ten bytes is an error.
- Hash: BLAKE3-256 (32 bytes, default output).
- Sizes: all lengths are in bytes.

## 2. Header

The header is fixed at 32 bytes at offset 0.

| Offset | Size | Field | Value / meaning |
|---|---|---|---|
| 0 | 8 | magic | `0x89 0x4C 0x50 0x4B 0x0D 0x0A 0x1A 0x0A` (`\x89LPK\r\n\x1a\n`) |
| 8 | 2 | version_major | 1 |
| 10 | 2 | version_minor | 0 for this draft |
| 12 | 4 | flags | see below |
| 16 | 16 | archive_id | 16 random bytes chosen by the writer |

The magic follows the PNG pattern: a non-ASCII lead byte, the name, CR LF to catch line-ending
conversion, SUB to stop the DOS `type` command, and LF.

A reader checks the header in this order: the length (input shorter than 32 bytes is the error
`Truncated { what: "header" }`), the magic (a mismatch is the error `BadMagic`), the major version, and
only then the flags, because a future major version may redefine them.

Version rule: a reader accepts `version_major` 1 and any `version_minor`; any other major version is
an error.

Header flags:

| Bit | Name | Meaning |
|---|---|---|
| 0 | ENCRYPTED | the archive content is encrypted |
| 1 | LISTABLE | the archive can be listed without the key |
| 2-31 | reserved | must be zero; a reader rejects the header otherwise |

## 3. Frames

After the header the archive is a sequence of frames.

| Field | Size | Meaning |
|---|---|---|
| kind | 2 | frame kind (see below) |
| flags | 2 | frame flags (see below) |
| payload_len | varint | length of the payload in bytes |
| payload | payload_len | the frame content |
| hash | 32 | BLAKE3-256 of the payload bytes only |

Frame flags:

| Bit | Name | Meaning |
|---|---|---|
| 0 | MUST_UNDERSTAND | a reader that does not know the kind must fail instead of skipping |
| 1-15 | reserved | must be zero; a reader rejects the frame otherwise |

Frame kinds:

| Kind | Name |
|---|---|
| 1 | EntryTable |
| 2 | ChunkData |
| 3 | Records |
| 4 | Recovery |
| 5 | Index |
| 6 | Trailer |

Kind 0 is invalid. Kinds 7 to 0x7FFF are reserved for later versions of this specification. Kinds 0x8000 to
0xFFFF are experimental and are never written by the reference writer.

Unknown kinds: a reader meeting a kind it does not know fails if MUST_UNDERSTAND is set. Otherwise it
skips the frame: the payload is still read and its hash verified, and the kind, flags and payload length
are reported to the caller.

Hash rule: a payload whose BLAKE3-256 differs from the stored hash is an error, for known and unknown
kinds alike.

Truncation: input that ends inside a frame header, a payload or a hash is an error naming that part.
Input that ends exactly at a frame boundary is a clean end.

Reader limits: a reader enforces a maximum payload length (the reference default is 1 GiB for now). A
`payload_len` above the limit is an error, raised before any payload byte is read or allocated.

## 4. Entry table

The payload of a frame of kind EntryTable lists the archive's entries.

| Field | Size | Meaning |
|---|---|---|
| entry_count | varint | number of entries that follow |
| entries | | `entry_count` entries, sorted by path |

Order: entries are sorted by path bytes ascending, comparing unsigned bytes (so UTF-8 byte order, not
locale order), with no duplicate paths. A reader that meets a path that is not strictly greater than the
previous one fails with `UnsortedEntries`, naming the entry index (counted from 0).

One entry:

| Field | Size | Present | Meaning |
|---|---|---|---|
| kind | 1 | always | entry kind (see below); an unknown value is `UnsupportedEntryKind` |
| flags | 2 | always | entry flags (see below) |
| path_len | varint | always | length of the path in bytes, 1 to 65535 |
| path | path_len | always | the path, see the path rules |
| mtime_ns | 8 | always | signed 64-bit nanoseconds since 1970-01-01T00:00:00Z; the minimum `i64` value means unknown |
| size | varint | always | file: byte length; directory: 0; symlink: length of the target in bytes |
| target_len | varint | symlink only | must equal size |
| target | target_len | symlink only | the link target: any bytes except NUL, 1 to 65535 bytes |
| chunk_count | varint | file only | number of chunk indices |
| chunks | chunk_count varints | file only | indices into the archive chunk list (defined with the chunk frames) |

A directory or a symlink has no chunk list. Whether a file's `size` agrees with its chunk list is defined
with the chunk frames (task E1-3) and is not checked by the entry table.

Entry kinds:

| Value | Name |
|---|---|
| 0 | File |
| 1 | Directory |
| 2 | Symlink |

Entry flags:

| Bit | Name | Meaning |
|---|---|---|
| 0 | EXECUTABLE | the file is executable |
| 1 | HIDDEN | the entry is hidden |
| 2 | READ_ONLY | the entry is read-only |
| 3 | SYSTEM | the entry is a system file |
| 4-15 | reserved | must be zero; a reader rejects the entry otherwise |

Path rules: a path is UTF-8 with `/` as the separator, 1 to 65535 bytes long. It has no leading or
trailing `/`, no empty component, no component equal to `.` or `..`, and contains no `\` and no NUL byte.
A violation is `InvalidPath` with the entry index and one of these reasons: "empty", "not utf-8",
"leading slash", "backslash", "nul", "dot component", "empty component", "trailing slash", "too long".
A bad symlink target (empty, longer than 65535 bytes, containing NUL, or `target_len` different from
`size`) is `InvalidPath` with the reason "symlink target". Paths are compared as bytes and are not
normalised; a reader does not alter case or Unicode form.

Consistency: an entry whose fields contradict each other is `InconsistentEntry` with the entry index and
a reason. The reader raises one reason: "directory size" (a directory whose `size` is not 0). A writer
also refuses, with the same error, "directory has chunks", "symlink has chunks", "symlink without
target" and "target on non-symlink"; those cannot occur on the wire.

Errors: the payload ending inside the table is `Truncated` for "entry table"; bytes left after the last
entry are `TrailingBytes` for "entry table". Counts and lengths are checked against the bytes that remain
before anything is allocated. The smallest possible entry is 14 bytes (a directory with a one-byte path:
1 + 2 + 1 + 1 + 8 + 1), so an `entry_count` larger than the bytes after it divided by 14 (rounded down)
is `Truncated` for "entry table", raised when the count is read.

Check order: a reader checks each entry in field order, so when several rules are broken the first of
these wins: kind, flags, path (length, then truncation, UTF-8, path rules), sort order, mtime, size
(and for a directory its size), then the symlink target or the chunk list. Entries are checked in order,
and after the last one the trailing-bytes check applies.

Names: paths are opaque to the table. They may contain `:` and names that Windows reserves (`CON`, a
component ending in a dot or a space). An extractor must map such names and must never let a component
replace or escape the target directory; that rule belongs to the extraction task, not to this format.

Reading: a reader first reads only `entry_count` (and applies the bound above); entries are then decoded one after another as a
stream, so a table of any size can be walked without holding all entries in memory.
