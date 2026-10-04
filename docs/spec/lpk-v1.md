# LitePack `.lpk` format, version 1 — working draft

Status: this is a working draft. The format is not frozen until the independent-decoder gate (task E1-14).
Sections are added task by task; this revision covers conventions, the header, the frame grammar, the entry table, chunks and the Merkle tree, the index and trailer, and writing and reading an archive.
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
in section 5 and is not checked by the entry table.

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

## 5. Chunks and the Merkle tree

The archive's content is a list of chunks, numbered from 0 in table order. An entry's `chunks` field (section 4)
holds indices into this list. The chunk table has one record per chunk and is embedded by the index frame
(task E1-4); this section defines its encoding, the Merkle tree over it, and how files and byte ranges are
verified against it. Where the chunk data itself is stored is defined with the chunk frames.

The chunk table is a varint `chunk_count` followed by `chunk_count` records. One record:

| Field | Size | Meaning |
|---|---|---|
| plain_len | varint | length in bytes of the chunk's original data; 0 is allowed |
| hash | 32 | BLAKE3-256 of the chunk's original data (the bytes extraction returns, not the stored form) |

The smallest record is 33 bytes (a one-byte `plain_len` and the hash). A `chunk_count` larger than the bytes
after it divided by 33 (rounded down) is `Truncated` for "chunk table", raised when the count is read. Records
are decoded as a stream; a payload that ends inside a record is `Truncated` for "chunk table", and bytes left
after the last record are `TrailingBytes` for "chunk table". A `plain_len` that is not a canonical varint is
`NonCanonicalVarint`.

Merkle tree: the leaves are the chunk hashes in table order, and the leaf value is the chunk hash itself, with
no further hashing. An internal node over a left child and a right child is the 32-byte BLAKE3 key derivation
`derive_key(context, left || right)` with the context string `LitePack lpk v1 Merkle node` and the 64-byte
concatenation as key material. For `n` leaves: if `n` is 1 the root is the leaf; otherwise let `k` be the
largest power of two strictly less than `n`, and the root is the node over the root of the first `k` leaves
and the root of the remaining `n - k` leaves. There is no padding, and an unpaired node is never duplicated.
The root of a table with no records is `derive_key(context, [])` with the context string
`LitePack lpk v1 Merkle empty` and empty key material. Roots are 32 bytes.

Shape for five leaves (`N(a, b)` is the node over `a` and `b`; `L0` to `L4` are the leaf values):

```
root = N( N( N(L0, L1), N(L2, L3) ), L4 )
```

Inclusion proof: the proof of leaf `i` is the list of sibling values met on the way from the leaf to the root,
nearest the leaf first, and a level where the node has no sibling contributes nothing. In the five-leaf tree
the proof of leaf 2 is `[L3, N(L0, L1), L4]` and the proof of leaf 4 is `[N( N(L0, L1), N(L2, L3) )]`. A proof
has at most `ceil(log2 n)` entries. A verifier is given the root, the index `i`, the leaf count `n`, the leaf
value and the proof; it recomputes the root by the split rule above (at each split the last proof entry is the
sibling of the half that holds `i`, and a proof that is too short or too long fails) and accepts when the result
equals the root. An index not below `n` never verifies. The proof of a leaf in a left half holds the right half
only as a hash, so a proof alone does not pin `n` for every leaf; the verifier takes `n` from the chunk table.

File verification: a file is the chunks named by its entry, in order. Verification checks, in this order, that
every index is below the table length (otherwise `ChunkIndexOutOfRange` with the index and the table length),
that the sum of the chunks' `plain_len` equals the file length (otherwise `FileSizeMismatch` with the expected
and the found length; a chunk list whose lengths add up to more than the largest `u64` is malformed and is
reported the same way, with the found length set to the largest `u64`), and then, for each chunk in order, that the original bytes have length `plain_len` and
hash to the table's hash (otherwise `ChunkMismatch` with the chunk index). A chunk may appear in several files
and several times in one file.

Range verification: verifying bytes `[offset, offset + len)` of a file first checks that the range lies inside
the file (otherwise `RangeOutOfFile` with the offset, the length and the file length; a sum that overflows is
also out of the file), then the index and size checks above, and then reads and checks only the chunks that
overlap the range, found from the running sum of `plain_len`. A chunk of length 0 holds no byte and is not
read, and an empty range reads no chunk. A chunk that is read is checked whole.

Test vectors (checked by the crate's tests, in hexadecimal): the root of an empty table is
`f986dffb57677490beca54d2e3583730ff1b3e102731e6b019a8361c1a4efa0c`. The root of the two leaves
`1111111111111111111111111111111111111111111111111111111111111111` and
`2222222222222222222222222222222222222222222222222222222222222222` (each 32 bytes of 0x11 and 0x22) is
`2c3cddfa1e3c1c08f0088621676bc301fa3b581d7aa895c340239273754da9d3`.

Errors of this section: `ChunkMismatch`, `ChunkIndexOutOfRange`, `FileSizeMismatch`, `RangeOutOfFile`, plus
`Truncated`, `TrailingBytes` and `NonCanonicalVarint` for the table.

## 6. Index and trailer

The index locates every block of chunk data and authenticates the chunk table; the trailer is a fixed-size
frame at the very end of the archive that finds and authenticates the index. A reader opens an archive from
its head and its tail without reading the body.

### The index frame (kind 5)

The payload, in this order:

| Field | Size | Meaning |
|---|---|---|
| chunk_table | variable | the chunk table of section 5 (count, then the records) |
| merkle_root | 32 | the Merkle root over the chunk table's hashes |
| max_window | varint | the decode envelope of section 7: largest match-finder window, in bytes |
| max_bwt_block | varint | envelope: largest BWT block, in bytes (0 when none) |
| max_block_plain | varint | envelope: largest block `plain_len`; must equal the maximum over the block table |
| max_frame_payload | varint | envelope: largest frame payload; must admit the index's own payload and every recorded frame (section 7) |
| decode_memory | varint | envelope: the writer's estimate of peak decoder memory per thread, in bytes |
| threads_hint | varint | envelope: independent blocks decodable at once; 0 = no hint |
| block_count | varint | number of blocks; at most the bytes left after it divided by 5 |
| frame_offset | varint | per block: absolute offset of the block's `ChunkData` frame |
| frame_len | varint | per block: whole encoded length of that frame |
| first_chunk | varint | per block: index of the first chunk the block holds |
| chunk_count | varint | per block: number of chunks the block holds |
| plain_len | varint | per block: sum of the `plain_len` of its chunks |
| entry_table_offset | varint | absolute offset of the `EntryTable` frame |
| entry_table_len | varint | whole encoded length of that frame |
| records_offset | varint | absolute offset of the `Records` frame; 0 when there is none |
| records_len | varint | whole encoded length of that frame; 0 when there is none |

The chunk table is not length-prefixed: a reader walks its declared records (under the count bound of
section 5) and continues after the last one. The six envelope varints follow the Merkle root; an input
that ends inside them is `Truncated` (`what` is `index`). The five block fields repeat `block_count` times. Bytes after
`records_len` are `TrailingBytes` (`what` is `index`); an input that ends early is `Truncated` (`what` is
`index`, or `chunk table` inside the table). A `block_count` larger than the bytes left divided by 5 is
`Truncated` and nothing is allocated for it.

Block rules. Blocks are in ascending `first_chunk` order and partition the chunk indices `0..n` exactly,
contiguously and without overlap, where `n` is the chunk table's count: the first block starts at 0, each
next block starts where the previous one ended, and the last ends at `n`; anything else is `BlockCoverage`
with the number of the offending block (the number of blocks when they stop short of `n`). A block may have
`chunk_count` 0 only when the archive has no chunks. Each block's `plain_len` must equal the sum of its
chunks' `plain_len` in the chunk table, otherwise `BlockLengthMismatch` with the block number. A block's
frame must start at or after the end of the header (offset 32), be at least as long as the smallest frame,
and end no later than the offset of the index frame itself, otherwise `BlockOutOfRange` with the block
number; a sum that overflows is out of range. The `EntryTable` frame and the `Records` frame (when
`records_offset` and `records_len` are not both 0) follow the same location rule; a violation is
`BadFrameLocation` naming the frame. The smallest frame is 37 bytes (4 for kind and flags, 1 for the shortest
`payload_len`, 32 for the hash): every recorded location, including the index's, must be at least that long.
The envelope must agree with the block table (section 7). Frame locations do not overlap: the blocks' frames are in strictly ascending `frame_offset` order, each
starting at or after the end of the previous one (otherwise `BlockCoverage` with the later block's number),
and the `EntryTable` and `Records` frames overlap no block and each other (otherwise `BadFrameLocation`
naming that frame). An empty block is the only block of an archive without chunks; a second block of such an
archive is `BlockCoverage`. The stored `merkle_root` must equal the root recomputed from the chunk
table's hashes, otherwise `MerkleRootMismatch`. A chunk's place is its block and the sum of the `plain_len`
of the chunks before it in that block.

### The trailer frame (kind 6)

The trailer is written with empty frame flags and has a fixed 72-byte payload:

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 8 | index_offset | absolute offset of the index frame (u64, little-endian) |
| 8 | 8 | index_len | whole encoded length of the index frame (u64) |
| 16 | 32 | index_hash | BLAKE3-256 of the index frame's payload |
| 48 | 8 | generation | 0 for an archive written in one go (u64) |
| 56 | 16 | archive_id | must equal the header's archive_id |

The whole trailer frame is `kind` (2) + `flags` (2) + `payload_len` varint (1 byte, the value 72) + payload
(72) + hash (32) = 109 bytes, and it is the last thing in the archive. The index must lie at or after the
end of the header and end no later than the start of the trailer, otherwise `BadFrameLocation` (`index`).

### Opening an archive

Every read of a recorded frame is bounded by the recorded length: nothing past `offset + len` is read, and a
frame that is cut by that bound, or whose own encoded size differs from the recorded length, is
`BadFrameLocation` naming the frame (`index`, `entry table` or `records`). The kind is checked first.

1. If the input is shorter than the header plus the trailer frame (141 bytes), go to the diagnosis below.
2. Read the 32-byte header (errors of section 2 apply).
3. Read the last 109 bytes. They must be a frame of kind 6, empty flags, payload length 72, with a valid hash;
   if they are not, go to the diagnosis below. A trailer whose `archive_id` differs from the header's is
   `ArchiveIdMismatch`.
4. Read the frame at `index_offset`. Its kind must be 5 (otherwise `WrongFrameKind` with the expected and the
   found kind), its hash must verify (otherwise `HashMismatch`), its encoded length must equal `index_len`
   (otherwise `BadFrameLocation`), and the BLAKE3 of its payload must equal the trailer's `index_hash`
   (otherwise `IndexHashMismatch`).
5. Parse the index under the rules above (including the envelope's consistency rules of section 7). The reader walks the chunk table once to find where it ends, then
   makes one pass over its records that checks the block lengths, gathers the hashes for the Merkle root and
   builds the chunk index (record offsets and cumulative plain offsets).
6. Compare the envelope with the reader's resources (section 7); an archive that needs more is refused before
   any block is read.
7. The entry table is read only when asked: the frame at the recorded location must be of kind 1 and have
   the recorded length, and its hash must verify.

### Truncated versus corrupt

When the tail is not a valid trailer, the reader walks the frames forward from the end of the header, under
the read limits of section 3, skipping unknown kinds as there. If every frame it reads verifies and the input
ends exactly at a frame boundary, or inside a frame, without a trailer frame having been read, the archive is
cut short: `Truncated` with `what` set to `trailer`. If a frame fails its hash, the error is that frame's
`HashMismatch` (or whatever error the frame grammar gives). If a trailer frame is read and bytes follow it,
the error is `TrailingBytes` with `what` set to `archive`. The walk also reports how many frames verified and
the offset just after the last good frame, so a repair tool knows where the readable part ends. Two edge
cases: if the walk ends on a trailer frame that is not of the fixed shape (flags not empty, or a payload that is
not 72 bytes) with nothing after it, the error is `NoTrailer`; and a header that is shorter than 32 bytes or
invalid ends the walk at once with the header's own error (section 2), not `Truncated` with `what` `trailer`.

Errors of this section: `MerkleRootMismatch`, `IndexHashMismatch`, `ArchiveIdMismatch`,
`BlockLengthMismatch`, `BlockCoverage`, `BlockOutOfRange`, `BadFrameLocation`, `WrongFrameKind`,
`EnvelopeMismatch`, `Refused`, `NoTrailer` (the last frame is not a trailer of the fixed shape), plus `Truncated` and `TrailingBytes` with the
`what` strings `index`, `trailer` and `archive`.

## 7. Decode envelope

Every archive states, in the index, the resources a decoder needs. A reader compares them with what the local
machine allows before it decodes anything, and refuses with a message that names the limit unless the caller
allows more. The envelope is six varints placed right after `merkle_root` and before `block_count` in the
index payload (section 6):

| Field | Size | Meaning |
|---|---|---|
| max_window | varint | largest match-finder window (dictionary) any block needs, in bytes |
| max_bwt_block | varint | largest BWT block any block needs, in bytes; 0 when no BWT is used |
| max_block_plain | varint | largest `plain_len` of any block; must equal the maximum over the block table |
| max_frame_payload | varint | largest frame payload in the archive; must admit the index's own payload and every recorded frame (blocks, entry table, records) |
| decode_memory | varint | the writer's estimate of peak decoder memory for one decoding thread, in bytes |
| threads_hint | varint | independent blocks a reader may decode at once within `decode_memory` times this; 0 = no hint; at most 4294967295 |

`max_window` is the largest match-finder window (dictionary) any block needs; `max_bwt_block` is the largest
Burrows-Wheeler block any block needs (0 when none is used); both are in bytes. `max_block_plain` is the
largest `plain_len` of any block. `max_frame_payload` is the largest frame payload in the archive: it covers the index, every block, the entry
table and the records frame. `decode_memory`
is the writer's estimate of the peak memory of one decoding thread, in bytes, and `threads_hint` is the number
of independent blocks a reader may decode at once without exceeding `decode_memory` times `threads_hint`
(0 = no hint; a value above 4294967295 is `EnvelopeMismatch` with `field` `threads_hint`).

Order of the checks. A `threads_hint` above 4294967295 is raised while the envelope is read, before the block
rules and before a later truncation can be noticed. The other mismatches are checked after the block rules and
after `MerkleRootMismatch` of section 6, in the order below:

- `max_block_plain` must equal the maximum `plain_len` over the block table (0 when there are no blocks),
  otherwise `EnvelopeMismatch` with `field` `max_block_plain`.
- `max_frame_payload` is `M` below. The length of the index payload must be at most `M`, and every recorded
  frame length `L` (each block's `frame_len`, the entry table's and, when present, the records frame's) must
  satisfy `L <= M + 36 + varint_len(M)`, where `varint_len(M)` is the encoded length of `M` as a varint (36
  bytes are the kind, flags and hash); otherwise `EnvelopeMismatch` with `field` `max_frame_payload`. The right
  side grows with `M`, so a larger `M` than needed is allowed. A recorded length that is the length of no
  frame, such as 165 (payloads 127 and 128 give 164 and 166) or 16422, is admitted by the first `M` whose bound
  reaches it (128 and 16384).
- The index payload contains the envelope's own varints, so `max_frame_payload` depends on the size it is part
  of: a writer repeats the computation until the value is stable. An index payload larger than the reader's own
  `max_frame_payload` is not `Refused`: reading the frame fails with `PayloadTooLarge` (section 3), because the
  reader's limit applies while the index is read.

The format itself does not limit any value of the envelope; the limits are the reader's. The reader's resources
are `max_window`, `max_bwt_block`, `max_block_plain`, `max_frame_payload` and `memory`, all in bytes, with these
defaults:

| Resource | Default (bytes) | Bounds the envelope field |
|---|---|---|
| max_window | 268435456 (256 MiB) | max_window |
| max_bwt_block | 67108864 (64 MiB) | max_bwt_block |
| max_block_plain | 1073741824 (1 GiB) | max_block_plain |
| max_frame_payload | 1073741824 (1 GiB) | max_frame_payload |
| memory | 2147483648 (2 GiB) | decode_memory |

A writer's default envelope stays within two caps: `max_window` at most 268435456 bytes (256 MiB) and
`max_bwt_block` at most 67108864 bytes (64 MiB). The caps are policy of writers and of the default resources,
not a rule of the parser.

Refusal rule. Each of `max_window`, `max_bwt_block`, `max_block_plain` and `max_frame_payload` must be at most
the reader's resource of the same name, and `decode_memory` at most `memory`. The first field in that order that
exceeds its limit is the refusal: `Refused`, carrying the field name, the value the archive needs and the value
the reader allows, with the message
`the archive needs <field> of <needed> bytes; this reader allows <allowed>`. `threads_hint` never refuses. The check happens when the archive is opened, after the index has
been read and before any block is read. Allowing more is the caller passing larger resources; there is no
switch that skips the check.

Frame limit. While the index is read, the largest accepted `payload_len` (section 3) is the reader's
`max_frame_payload`; once the index is read it is the smaller of the envelope's `max_frame_payload` and the
reader's.

Errors of this section: `EnvelopeMismatch`, `Refused`, and `Truncated` with `what` `index`.

## 8. Primitives and the decode graph

Nothing executable is stored in an archive. A block's decoding is described by a short graph of primitives
taken from a fixed registry; a reader that meets a primitive ID it does not know stops before it reads any
data. The registry gives each primitive a 16-bit ID, a name, a parameter layout and the resources it needs
(they feed the envelope of section 7):

| ID | Name | Parameters | Resources |
|---|---|---|---|
| 0x0000 | `store` | none (length 0) | none |
| 0x0001 | `zstd` | `window_log: u8` (window = 2^window_log bytes; 10..=31), `dictionary: [u8; 32]` (BLAKE3 id of a prior, all zeros = none) | window = 2^window_log |
| 0x0002 | `lzma` | `dict_size: u32` LE, `lc: u8`, `lp: u8`, `pb: u8` (the LZMA1 properties; lc <= 8, lp <= 4, pb <= 4) | window = dict_size |
| 0x0003 | `bwt` | `block_size: u32` LE (bytes; not 0) | bwt block = block_size |
| 0x0004 | `bcj-x86` | none | none |
| 0x0005 | `bcj-arm64` | none | none |
| 0x0006 | `delta` | `base_chunk: u64` LE (the chunk the patch applies to), `patch_format: u8` (0 = zstd patch, 1 = suffix-array patch) | none |
| 0x0007 | `jpeg-reconstruct` | none (the record is in the `Records` frame) | memory per image, declared by `decode_memory` |
| 0x0008 | `deflate-reconstruct` | none | none |
| 0x0009 | `png-filter` | none | none |
| 0x000A | `base64` | `variant: u8` (0 standard, 1 url-safe), `line_len: u16` LE (0 = no line breaks) | none |
| 0x000B | `utf16` | `endian: u8` (0 LE, 1 BE), `bom: u8` (0 none, 1 present) | none |
| 0x000C | `container-reconstruct` | none | none |
| 0x000D..=0x7FFF | reserved for later versions of this spec | - | - |
| 0x8000..=0xFFFF | experimental; a conforming writer never emits them | - | - |

`params` is a byte string whose length is stated in the graph; its length must be exactly the layout's length
(0 for a primitive without parameters). Violating a layout's rules, such as a `window_log` of 9 or 32, an `lc`
of 9 or a `variant` of 2, is `BadParams` carrying the ID and a short reason.

### The decode graph

A graph is a linear chain in version 1; the encoding leaves room for a graph with fan-in in a later version.

| Field | Size | Meaning |
|---|---|---|
| step_count | varint | number of steps, 1..=16 |
| primitive | u16 LE | per step: the primitive ID (section 8 registry) |
| flags | u8 | per step: bit 0 `MUST_UNDERSTAND` is reserved and must be 0 in v1; all other bits must be 0 |
| params_len | varint | per step: length of `params`, at most 256 |
| params | params_len bytes | per step: the parameters, laid out as the primitive defines |

A step count of 0 or above 16 is `BadGraph` with `reason` `step count`; a non-zero `flags` is `BadGraph` with
`step flags`; a `params_len` above 256 is `BadGraph` with `params length`.

Order of application. Steps are applied in the order written, to decode: the encoded bytes go into the first
step's decoder, its output into the second step's, and the last output is the block's plain bytes. A writer that
compressed with `bcj-x86` and then with `lzma` therefore writes the graph `[lzma, bcj-x86]`.

Bounds. Every intermediate output is bounded by the reader's `max_block_plain`; a larger one is
`PayloadTooLarge`. The last step must produce exactly `plain_len` bytes; any other length, including a decoder
that would produce more, is `BlockLengthMismatch`. A block whose `plain_len` exceeds `max_block_plain` is
`PayloadTooLarge`, raised before any decoding. Before the first step runs, a reader checks the whole graph:
the step count, every step's parameters, and that every primitive is one it can run (otherwise
`UnimplementedPrimitive`); no step runs when a later one cannot.

Unknown IDs. A primitive ID that is not in the table is `UnknownPrimitive`, raised as soon as the ID is read
while the graph is parsed, before any later byte of the graph or any encoded byte is looked at; a graph that is
truncated right after an unknown ID still reports the unknown ID. A known primitive whose parameters fail their
layout is `BadParams`, raised at the same point of the parse.

### The `ChunkData` block header

The payload of a `ChunkData` frame (kind 2) starts with this header, followed by the encoded bytes:

| Field | Size | Meaning |
|---|---|---|
| graph | variable | the decode graph (step_count and the steps) |
| plain_len | varint | length of the block's plain bytes; must equal the block table's `plain_len` |
| encoded_len | varint | number of bytes that follow; must equal the payload length minus the header |
| encoded | encoded_len bytes | the encoded bytes |

A block with the graph `[store]` has `encoded_len == plain_len` and its encoded bytes are its plain bytes. An
`encoded_len` that differs from the bytes present, a `plain_len` that differs from the block table's, or a last
step whose output length differs from `plain_len` is `BlockLengthMismatch`.

### What the reference decoder runs

The reference decoder knows the name, the parameter layout and the validation of all 13 primitives. Each ID is
in exactly one of three groups:

- Implemented now: `store`.
- Added to the reference decoder by later revisions of this specification's reference implementation: `zstd`, `lzma`.
- Requires the full reader: `bwt`, `bcj-x86`, `bcj-arm64`, `delta`, `jpeg-reconstruct`,
  `deflate-reconstruct`, `png-filter`, `base64`, `utf16`, `container-reconstruct`.

For a primitive without a decoder the reference decoder reports `UnimplementedPrimitive` with the ID, before it
runs any step of the graph.

Errors of this section: `UnknownPrimitive`, `UnimplementedPrimitive`, `BadGraph`, `BadParams`,
`BlockLengthMismatch`, `PayloadTooLarge`, and `Truncated` with the `what` strings `graph` and `block header`.

## 9. Writing and reading an archive

### Writing an archive as a stream

An archive can be written without seeking and without reading back what was written: every offset the index
records is a count of the bytes already written. The frames follow in this order:

1. the header (section 2);
2. zero or more `ChunkData` frames (kind 2), each holding one block;
3. the `EntryTable` frame (kind 1);
4. optionally the `Records` frame;
5. the `Index` frame (kind 5), which records the location of every frame above, the chunk table and the Merkle
   root over its hashes, and the decode envelope (section 7);
6. the `Trailer` frame (kind 6), which locates the index.

The frames of a stream-written archive have no flags set and the header flags are zero. An archive without file
content has no blocks and an empty chunk table.

The index payload states its own length through `max_frame_payload`, whose varint width depends on the value.
A writer settles this by computing the index again with the length it just produced until the declared
`max_frame_payload` equals the larger of the longest other frame's smallest admissible payload and the index
payload length.

### Chunks and blocks

A file's bytes are cut into chunks of exactly the chunk size, the last one shorter; an empty file has no chunks.
Chunks are numbered in the order they are written, which is the order of the entries that own them; this
writer gives each file a run of consecutive numbers, but a reader accepts any chunk list whose indices are in
range and whose lengths add up to the file size. The chunk table has one record per chunk (section 5). The
rule that chooses the cut is a property of the writer, not of the format: a reader finds each chunk's length in
the chunk table and never assumes a size, so another way of cutting changes only the chunk boundaries. A
writer's chunker is fed the bytes of one file as a stream and may hold back a tail of at most one chunk; the
chunker starts afresh for each file, so a chunk never holds bytes of two files, and no chunk is longer than the
chunk size.

Chunks fill blocks in order. A writer closes the current block before the chunk that would take it past its
size limit, so a block holds whole chunks and a chunk never spans two blocks. A block of a store-only archive
has the graph `[store]`; for such an archive the envelope declares `max_window` 0, `max_bwt_block` 0,
`threads_hint` 0 and `decode_memory` equal to `max_block_plain`, the space of one block buffer; the encoded
input a decoder reads beside it is not counted in `decode_memory`.

A writer that meets an I/O error, on its input or its output, or a chunker that breaks the rules above, refuses
every later call with that error: the archive being written is abandoned.

Entries are written in strictly ascending path order (section 4). A writer refuses a path that is invalid, equal
to the one before it or sorts before it.

### Reading one block at a time

A reader reads a chunk by finding its block in the chunk index, reading that block's frame, decoding the block
with its graph (section 8) and cutting the chunk out of the plain bytes at the offset its predecessors in the
block leave. It keeps the plain bytes of the block it read last, so chunks read in order decode each block once.
Every chunk is compared with its record in the chunk table (length and BLAKE3) before its bytes are used; a
mismatch is `ChunkMismatch` with the chunk's number. A block whose frame hash fails cannot vouch for any chunk it
holds, so when a file is extracted, reading a chunk of that block is reported as a `ChunkMismatch` of that
chunk.

Whole-archive verification decodes every block and compares every chunk with its record, so a chunk no entry
uses is checked too; a block whose frame hash fails is reported as that frame's `HashMismatch`. It then checks
every file entry's chunk list and total size against the chunk table, which needs no block reads.

Only the block read last is kept, so a chunk list that alternates between chunks of two blocks makes every
reference read and decode a whole block; a reader of untrusted archives needs a decode budget.

### Extraction by the reference tool

The reference tool (`lpk-decode`) extracts files and directories and applies these refusals before it writes
anything:

- a symlink entry is refused (`SymlinkRefused`); what an extractor does with links is a policy outside this
  format;
- a path component that is a Windows device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1` to `COM9`, `COM` followed
  by a superscript 1, 2 or 3, `LPT1` to `LPT9`, `LPT` followed by a superscript 1, 2 or 3, `CONIN$`, `CONOUT$`),
  compared in any letter case on the part before the first dot with its trailing spaces removed, so that
  `con .txt` is refused as well as `CON.txt`; a component that contains `:`; or one that ends in a dot or a space
  is refused (`UnsafePath`).

The tool does not overwrite an existing file and reuses existing directories. Each file is created with
"create new" semantics at its final path; a file the tool created and could not finish is removed, and a file
that was already there is left untouched. Paths in the tool's listing have control characters escaped.

Errors of this section: `UnsortedEntries`, `InvalidPath`, `BadChunk`, `ChunkMismatch`, `FileSizeMismatch`,
`ChunkIndexOutOfRange`, `SymlinkRefused`, `UnsafePath`.
