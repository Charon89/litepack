# LitePack `.lpk` format, version 1 — working draft

Status: this is a working draft. The format is not frozen until the independent-decoder gate (task E1-14).
Sections are added task by task; this revision covers conventions, the header and the frame grammar.
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
