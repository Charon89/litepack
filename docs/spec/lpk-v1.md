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
| prior_list | variable | the prior list of section 10 (`prior_count`, then that many 32-byte IDs); follows the envelope |
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
| recovery_count | varint | number of `Recovery` frames (section 13); at most the bytes left after it divided by 2 |
| recovery_offset | varint | per recovery frame: absolute offset of the frame |
| recovery_len | varint | per recovery frame: whole encoded length of that frame |

The chunk table is not length-prefixed: a reader walks its declared records (under the count bound of
section 5) and continues after the last one. The six envelope varints follow the Merkle root; an input
that ends inside them is `Truncated` (`what` is `index`). The prior list (section 10) follows the envelope and
precedes `block_count`. The five block fields repeat `block_count` times. Bytes after
the last recovery location are `TrailingBytes` (`what` is `index`); an input that ends early is `Truncated`
(`what` is `index`, or `chunk table` inside the table). A `block_count` larger than the bytes left divided by 5
is `Truncated` and nothing is allocated for it; a `recovery_count` larger than the bytes left divided by 2 is
`Truncated` likewise. Each recovery location follows the same location rule as the records frame and overlaps no
block, no other recovery frame, the entry table and the records frame; a violation is `BadFrameLocation` with
`what` `recovery`.

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
`EnvelopeMismatch`, `Refused`, `BadPriorList`, `NoTrailer` (the last frame is not a trailer of the fixed shape), plus `Truncated` and `TrailingBytes` with the
`what` strings `index`, `trailer` and `archive`.

## 7. Decode envelope

Every archive states, in the index, the resources a decoder needs. A reader compares them with what the local
machine allows before it decodes anything, and refuses with a message that names the limit unless the caller
allows more. The envelope is six varints placed right after `merkle_root` and before the prior list and
`block_count` in the index payload (section 6):

| Field | Size | Meaning |
|---|---|---|
| max_window | varint | largest match-finder window (dictionary) any block needs, in bytes |
| max_bwt_block | varint | largest BWT block any block needs, in bytes; 0 when no BWT is used |
| max_block_plain | varint | largest `plain_len` of any block; must equal the maximum over the block table |
| max_frame_payload | varint | largest frame payload in the archive; must admit the index's own payload and every recorded frame (blocks, entry table, records, recovery) |
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
| 0x0002 | `lzma` | `dict_size: u32` LE, `lc: u8`, `lp: u8`, `pb: u8` (the LZMA1 properties; lc <= 8, lp <= 4, pb <= 4, lc + lp <= 4) | window = dict_size |
| 0x0003 | `bwt` | `block_size: u32` LE (bytes; not 0) | bwt block = block_size |
| 0x0004 | `bcj-x86` | none | none |
| 0x0005 | `bcj-arm64` | none | none |
| 0x0006 | `delta` | `base_chunk: u64` LE (the chunk the patch applies to), `patch_format: u8` (0 = zstd patch, 1 = suffix-array patch) | none |
| 0x0007 | `jpeg-reconstruct` | `record_id: varint` (the record in the `Records` frame, section 12) | memory per image, declared by `decode_memory` |
| 0x0008 | `deflate-reconstruct` | `record_id: varint` | none |
| 0x0009 | `png-filter` | `record_id: varint` | none |
| 0x000A | `base64` | `record_id: varint` | none |
| 0x000B | `utf16` | `record_id: varint` | none |
| 0x000C | `container-reconstruct` | `record_id: varint` | none |
| 0x000D..=0x7FFF | reserved for later versions of this spec | - | - |
| 0x8000..=0xFFFF | experimental; a conforming writer never emits them | - | - |

`params` is a byte string whose length is stated in the graph; its length must be exactly the layout's length
(0 for a primitive without parameters). The six reconstruction primitives (7 to 12) are the exception: their
`params` are one canonical varint, the `record_id` (1 to 10 bytes, nothing after it); the record itself holds
what earlier drafts put in the parameters of `base64` and `utf16` (section 12). Violating a layout's rules, such
as a `window_log` of 9 or 32, an `lc` of 9, a `patch_format` of 2, or a `record_id` that is empty, not a
canonical varint or followed by other bytes (reason `record_id`), is `BadParams` carrying the ID and a short
reason.

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
in exactly one of two groups:

- Implemented now: `store`, `zstd`, `lzma`.
- Requires the full reader: `bwt`, `bcj-x86`, `bcj-arm64`, `delta`, `jpeg-reconstruct`,
  `deflate-reconstruct`, `png-filter`, `base64`, `utf16`, `container-reconstruct`.

For a primitive without a decoder the reference decoder reports `UnimplementedPrimitive` with the ID, before it
runs any step of the graph.

### What the reference decoder enforces for `zstd`

The reference decoder decodes a `zstd` step's input as one zstd frame or a sequence of frames (skippable
frames are skipped) into at most the step's output bound, in steps of about 64 KiB, with a pure-Rust decoder. It
applies these rules, in this order:

1. The `window_log` must be 10..=31 (`BadParams`, `window_log`).
2. The window the step declares, 2^`window_log` bytes, must not exceed the reader's `max_window`; otherwise
   `WindowTooLarge` with the window needed and the window allowed. This is raised before the input is read.
3. When `dictionary` is not all zeros the reader's store of priors (section 10) must hold a prior of that ID,
   and the bytes it returns must hash to the ID; otherwise `MissingPrior`, raised before the input is read.
   The prior must be a zstd dictionary (the format that begins with the magic number 0xEC30A437); other
   bytes are a `ZstdError`.
4. The window a frame declares in its header must not exceed 2^`window_log` (`BadParams`, `frame window exceeds
   declared`); the decoder allocates nothing for a window before this check.
5. Output past the step's bound is `PayloadTooLarge` (`BlockLengthMismatch` on the last step, as above); a
   frame that is cut short, damaged, names a dictionary other than the prior, or whose content checksum (when
   it has one) does not match the output is `ZstdError` carrying the decoder's own text.

Errors of this section: `UnknownPrimitive`, `UnimplementedPrimitive`, `BadGraph`, `BadParams`,
`BlockLengthMismatch`, `PayloadTooLarge`, `WindowTooLarge`, `ZstdError`, `LzmaError`, and `Truncated` with the
`what` strings `graph` and `block header`.

### What the reference decoder enforces for `lzma`

The encoded bytes of an `lzma` step are a raw LZMA1 stream: no `.lzma` or `.xz` header (the properties are
the step's parameters, the output length is the step's output bound), decoded with a pure-Rust decoder. The
decoder stops when it has produced exactly the step's output bound. `lzma` parameters carry no prior ID: v1
has no priors for LZMA (section 10 is about `zstd` only). The rules, in this order:

1. `lc` <= 8, `lp` <= 4, `pb` <= 4 and `lc + lp` <= 4 (`BadParams`, `lc`, `lp`, `pb` or `lc + lp`). The
   last limit is liblzma's `LZMA_LCLP_MAX` and bounds the table of literal probabilities (3 * 2^(`lc` + `lp`)
   entries of 0x100 each in the reader); the LZMA SDK would accept up to 12. Under it `lc` 5 to 8 can
   never be used, although the parameter layout allows them.
2. `dict_size` must not exceed the reader's `max_window`; otherwise `WindowTooLarge` with the size needed and
   the size allowed, raised before the input is read.
3. A match distance greater than `dict_size`, or greater than the number of bytes produced so far, is
   `LzmaError` carrying the decoder's text. Whether a stream decodes never depends on the reader's
   `max_window` beyond rule 2: the reference decoder allocates a buffer of the smaller of `dict_size` and
   the output bound (at least 1 byte), which is exactly the reach a valid stream can use.
4. The first byte of the range coder stream must be 0, else `LzmaError` with reason `range coder`. The
   range coder's end condition (its code value being 0 after the last symbol) is checked only when the
   end-of-payload marker is present; a stream that ends at the output bound without a marker is not checked
   for it, and a decoder must not refuse a marker-less stream because its final code value is not 0.
5. The stream may end with the end-of-payload marker or without one: the encoder liblzma used for the test
   vectors always writes it, other writers (such as `.lzma` files of known size) do not, and a decoder that has
   produced the output bound accepts either. Any input after the stream (after the marker, when there is one)
   is `LzmaError` with reason `trailing input`; so is a stream whose symbols continue past the output bound
   when the bound falls between two symbols. Input that ends before the output bound is reached, or a
   marker before it, is `LzmaError` with reason `truncated`.
6. A match that crosses the output bound is `PayloadTooLarge` (`BlockLengthMismatch` on the last step); the
   decoder never writes past the bound. Any other damage is `LzmaError` carrying the decoder's own text.

## 9. Writing and reading an archive

### Writing an archive as a stream

An archive can be written without seeking and without reading back what was written: every offset the index
records is a count of the bytes already written. The frames follow in this order:

1. the header (section 2);
2. zero or more `ChunkData` frames (kind 2), each holding one block;
3. the `EntryTable` frame (kind 1);
4. optionally the `Records` frame;
   optionally one `Recovery` frame (section 13) after it;
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

A block is encoded by the writer's block encoder, which reports the decode graph of its blocks and the decoder
resources that graph needs (they feed the envelope); the writer records the graph in every block header, checks
it before the first block, and lists the priors it names in the index (section 10). The identity encoder
produces the graph `[store]`.

A writer that meets an I/O error, on its input or its output, or a chunker that breaks the rules above, refuses
every later call with that error: the archive being written is abandoned.

Entries are written in strictly ascending path order (section 4). A writer refuses a path that is invalid, equal
to the one before it or sorts before it.

### Reading one block at a time

A reader reads a chunk by finding its block in the chunk index, reading that block's frame, decoding the block
with its graph (section 8) and cutting the chunk out of the plain bytes at the offset its predecessors in the
block leave. Before decoding, the block's graph is checked against the envelope (section 7): a graph whose
resources need a window above `max_window` or a BWT block above `max_bwt_block` is `EnvelopeMismatch` with that
field, raised when the block header is parsed. It keeps the plain bytes of the block it read last, so chunks
read in order decode each block once.
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

The tool takes `--prior <file>` (repeatable) for the priors an archive needs (section 10); `info` lists the
IDs the index names.

## 10. Priors

A prior is a byte string a decoder needs besides the archive, for example a zstd dictionary (D-10). Its ID is
the BLAKE3-256 of its bytes. A step names a prior by ID in its parameters (for `zstd`, the `dictionary` field of
section 8; all zeros means none). The archive never contains the prior and the reader never fetches it: the
caller gives the reader a store that maps an ID to bytes, and the reader looks nothing up anywhere else (no
network, no file lookup).

The index lists every prior the archive's blocks name, right after the envelope (section 6):

| Field | Size | Meaning |
|---|---|---|
| prior_count | varint | number of priors the archive's blocks name; at most the bytes left after it divided by 32 |
| prior_id | 32 | per prior: the BLAKE3-256 of the prior's bytes; ascending, unique, never all zeros |

The list is the set of every non-zero ID that any block's graph names, in ascending byte order. A count larger
than the bytes left divided by 32 is `Truncated` (`what` is `index`) and nothing is allocated for it; an ID
that is all zeros, or that is not greater than the one before it, is `BadPriorList`. A reader can therefore say
which priors are needed before it decodes anything.

Errors of this section. `MissingPrior` with the ID: a block's graph names a prior the caller's store does not
hold, or holds under bytes that do not hash to the ID; it is raised before the step's input is read.
`UnlistedPrior` with the ID: a block's header names a prior the index does not list; it is raised when the
block header is parsed, before the block is decoded. `BadPriorList` as above.

## 11. Test vectors

The archives under `crates/lpk-format/tests/vectors/` are written by the reference writer with a zstd or an LZMA encoder
(zstd level 3 or LZMA preset 6, chunk size 4096, archive id 0x5A repeated, entry mtime 1000). Their plain contents are generated by
the tests from seeded patterns, so the repository holds only the archives and the dictionary:

- `zstd-basic.lpk`: three files in one block, `window_log` 20.
- `zstd-multiblock.lpk`: three files in several blocks (block size 32 KiB), a file spanning two blocks.
- `zstd-dict.lpk`: one block compressed with a dictionary; the dictionary is committed beside it as
  `zstd-dict.prior`, and the index lists its BLAKE3-256.
- `zstd-window.lpk`: one block whose parameters declare `window_log` 24.

- `lzma-basic.lpk`: three files in one block, a raw LZMA1 stream of preset 6 (8 MiB dictionary, lc 3, lp 0,
  pb 2).
- `lzma-multiblock.lpk`: three files in several blocks (block size 32 KiB), a file spanning two blocks, the
  same options.
- `lzma-props.lpk`: the non-default properties lc 0, lp 2, pb 0 with a 1 MiB dictionary.

The normal tests check that each archive decodes to its generated contents and that `lpk-decode verify` accepts
it (with `--prior tests/vectors/zstd-dict.prior` for the dictionary one). The vectors are not a normative
encoding: regenerating them reproduces the committed bytes only with the same zstd and liblzma library versions, which an
ignored test checks on purpose. To regenerate them: `cargo test -p lpk-format --test gen_vectors -- --ignored`.
The frame of `zstd-window.lpk` is written without a declared content size, so its header declares the 2^24
window.

## 12. Reconstruction records

A reconstruction primitive (7 to 12 of section 8) rebuilds the original bytes of a peeled stream (a JPEG, a Deflate
stream, a PNG's filter bytes, a Base64 or UTF-16 text, a container) from the decoded form. The side information it
needs is kept in one **record** per peeled stream, and all records of an archive live in one frame of kind 3, the
`Records` frame. The index (section 6) gives the frame's location; an archive with no peeled stream has no
`Records` frame and its record count is 0. Every peel is verified at compression time by re-encoding; a stream
whose peel does not reproduce the original bytes is stored as it was, so a record is never a guess.

### How a block refers to a record

The `params` of a reconstruction primitive are one varint, the `record_id` (section 8). A record's id is its
position in the frame, counted from 0. When a block header is parsed, every `record_id` a step names must be below
the archive's record count, otherwise `RecordOutOfRange` with the id and the count. An archive without a `Records`
frame has count 0, so every id is out of range. The check needs the count only: a reader that decodes a block whose
graph names no record never reads the `Records` frame.

### The `Records` frame (kind 3)

The payload is a `record_count` and the records in ascending id order. Each record carries the primitive it
belongs to, its body and the hash of the body, so a record verifies on its own, without the rest of the frame.
The payload's own hash is the frame hash of section 3.

Counts, byte-string lengths, `body_len`, `record_count` and chunk indices are varints; fields with a stated width
(`u8`, `u16`, `u32`, `u64`) are fixed-width little-endian. `bytes` is a varint length followed by that many bytes. `chunk list` is a varint count
followed by that many varint chunk indices (section 5).

| Kind | Name | Primitive |
|---|---|---|
| 7 | `jpeg` | `jpeg-reconstruct` (0x0007) |
| 8 | `deflate` | `deflate-reconstruct` (0x0008) |
| 9 | `png-filter` | `png-filter` (0x0009) |
| 10 | `base64` | `base64` (0x000A) |
| 11 | `utf16` | `utf16` (0x000B) |
| 12 | `container` | `container-reconstruct` (0x000C) |

**Records frame payload**

| Field | Size | Meaning |
|---|---|---|
| record_count | varint | number of records; at most the remaining bytes divided by 5 |
| records | variable | per record, in ascending id order (the id is the position, from 0): the fields below |
| kind | u16 LE | the primitive the record belongs to: 7 to 12 (section 12 kind table) |
| flags | u16 LE | reserved, must be 0 |
| body_len | varint | length of `body`; at most the bytes that remain |
| body | body_len bytes | the record body of the kind |
| body_hash | 32 | BLAKE3-256 of `body` |

**jpeg body (kind 7)**

| Field | Size | Meaning |
|---|---|---|
| original_len | u64 LE | byte length of the JPEG file |
| primary_len | u64 LE | bytes of the primary image up to and including its EOI; at most `original_len` |
| trailing | bytes | the data after the EOI as stored: the raw bytes, or empty when they were peeled as a nested stream |
| nested_trailing_chunks | chunk list | the chunks holding the peeled trailing data; empty when `trailing` is not empty |
| gainmap_count | varint | number of secondary images |
| offset | u64 LE | per secondary image: position inside the original file |
| len | u64 LE | per secondary image: length; `offset + len` is at most `original_len` |
| chunks | chunk list | per secondary image: the chunks holding it |
| lepton_version | u8 | the Lepton format revision the stream was written with (0 for the version lepton_jpeg 0.5 writes) |
| original_hash | 32 | BLAKE3-256 of the whole original file |

**deflate body (kind 8)**

| Field | Size | Meaning |
|---|---|---|
| original_len | u64 LE | compressed byte length of the Deflate stream |
| plain_len | u64 LE | its decompressed length |
| corrections | bytes | preflate-rs's correction data |
| library | u8 | 0 = preflate-rs 0.7 format; no other value |
| original_hash | 32 | BLAKE3-256 of the original compressed stream |

**png-filter body (kind 9)**

| Field | Size | Meaning |
|---|---|---|
| width | u32 LE | image width in pixels |
| height | u32 LE | image height in pixels |
| bit_depth | u8 | 1, 2, 4, 8 or 16 |
| color_type | u8 | 0, 2, 3, 4 or 6 |
| interlace | u8 | 0 none, 1 Adam7 |
| filters | bytes | one filter byte per scanline, in order; for interlaced images per pass in PNG's order |
| original_hash | 32 | BLAKE3-256 of the filtered scanline bytes (the Deflate-decoded IDAT data) |

**base64 body (kind 10)**

| Field | Size | Meaning |
|---|---|---|
| variant | u8 | 0 standard, 1 url-safe |
| line_len | u16 LE | characters per line; 0 = no line breaks |
| line_ending | u8 | 0 LF, 1 CRLF, 2 none |
| padding | u8 | 0 none, 1 `=` |
| original_len | u64 LE | length of the encoded text |
| original_hash | 32 | BLAKE3-256 of the encoded text |

**utf16 body (kind 11)**

| Field | Size | Meaning |
|---|---|---|
| endian | u8 | 0 little endian, 1 big endian |
| bom | u8 | 0 no byte order mark, 1 present |
| original_len | u64 LE | length of the UTF-16 text in bytes |
| original_hash | 32 | BLAKE3-256 of the UTF-16 text |

**container body (kind 12)**

| Field | Size | Meaning |
|---|---|---|
| format | u8 | 0 ZIP, 1 PDF, 2 gzip, 3 TAR; other values are reserved |
| original_len | u64 LE | byte length of the container |
| framing | bytes | the verbatim bytes of the container that are not member data (ZIP: every local header, extra field, data descriptor, the central directory and the end record; PDF: object headers and xref; gzip: header and trailer; TAR: the headers) |
| member_count | varint | number of members |
| offset | u64 LE | per member: position in the original |
| len | u64 LE | per member: length in the original; members ascend and do not overlap, and `offset + len` is at most `original_len` |
| chunks | chunk list | per member: the chunks whose plain bytes are the member's original bytes (a nested peel of a member is undone when those chunks are decoded, so their plain lengths add up to `len`) |
| original_hash | 32 | after the last member: BLAKE3-256 of the whole original container |

Rules. Records are decoded as a stream. A `record_count` larger than the bytes after it divided by 5 (rounded
down) is `Truncated` for "records", raised when the count is read. A payload that ends inside a record, a
`body_len` larger than the bytes that remain, a byte string or count that the bytes remaining of the body cannot
hold, are `Truncated` for "records". Bytes left after the last record are `TrailingBytes` for "records"; bytes
left in a body after its last field are `TrailingBytes` for "record body". A `kind` outside 7 to 12 is
`UnknownRecordKind` with the kind and the id, raised as soon as the kind is read; non-zero `flags` are
`ReservedRecordBits`. A body whose BLAKE3 differs from `body_hash` is `RecordHashMismatch` with the id, checked
before the body is looked at. Within a body, a `u8` that is outside its list of values (a `library` other than 0,
a `bit_depth` outside 1, 2, 4, 8, 16, a `color_type` outside 0, 2, 3, 4, 6, an `interlace`, `variant`, `padding`,
`endian` or `bom` above 1, a `line_ending` above 2, a container `format` above 3) is `BadRecord` with the id and
the field's name as `reason`. So are these inconsistencies: a JPEG `primary_len` above `original_len`
(`primary_len`); a JPEG with both raw `trailing` bytes and `nested_trailing_chunks` (`nested_trailing_chunks`);
a secondary image or member whose `offset + len` overflows or exceeds `original_len`, that is out of order, overlaps
another or (for a secondary image) starts before `primary_len` (`gainmaps`, `members`); the sums of the assembly rules
below that do not hold (`trailing`, `gainmaps`, `original_len`); a base64 `line_len` of 0 with a `line_ending`
other than 2, or a `line_len` above 0 with `line_ending` 2 (`line_ending`); a non-interlaced png-filter record
whose `filters` are not `height` bytes long (`filters`).
The first error ends the walk, and asking for record `n` walks from the start, so the first error among records
`0` to `n` is the one reported.

### What each body verifies

`original_hash` is the verification target of the inversion: after a reconstruction primitive rebuilds the
original bytes, they must hash to it (and have `original_len` bytes where the record states a length).

- jpeg: the whole original JPEG file, primary image, trailing data and secondary images included.
- deflate: the original compressed Deflate stream, not its decompressed form.
- png-filter: the filtered scanline bytes, that is the Deflate-decoded IDAT data, not the PNG file.
- base64, utf16: the original encoded text.
- container: the whole original container, framing and members.

### How a record's parts fit together

- container: the original is the `framing` bytes with each member's data inserted at its `offset`: reading the
  original from the start, the bytes of the members are the ranges `[offset, offset + len)`, and the framing
  bytes fill the rest in order. Members ascend and do not overlap, and `framing.len() + sum(len) == original_len`.
  A member's `chunks` give its data as stored, in order, and their `plain_len` add up to `len`.
- jpeg: the original is the primary image (`primary_len` bytes, ending with its EOI) followed by `original_len -
  primary_len` bytes. When `trailing` is not empty it is exactly those bytes, secondary images included (none is
  peeled separately, so `gainmap_count` is 0 and `nested_trailing_chunks` is empty). Otherwise they are made of
  the secondary images, which lie after `primary_len`, ascend, do not overlap and end at or before `original_len`
  (each one's `chunks` add up to its `len`), and the nested trailing data, which fills the remaining ranges in
  order: the `nested_trailing_chunks` add up to `original_len - primary_len - sum(secondary len)`. With no nested
  chunks the secondary images must cover every byte after the primary image.
- Chunk indices. Every chunk index a record names is below the chunk count (`ChunkIndexOutOfRange`). A chunk a
  record names lies in a block with a strictly lower index than every block whose graph names that record, so
  a record never needs the block that is being rebuilt from it and two blocks never need each other. The
  chunk sums above and this order are checked by whole-archive verification (`BadRecord` with `reason`
  `chunk lengths` or `chunk order`), which knows the blocks and the chunk table; parsing a record cannot.

### What the reference decoder does with records

The reference decoder parses, validates and hashes records (the frame hash, every `body_hash`, every field rule
above) and checks the record ids of every block header it parses; it applies none. A block whose graph names a
reconstruction primitive is reported as `UnimplementedPrimitive` (section 8), as before. Applying records needs the
JPEG and Deflate libraries of the full reader. Whole-archive verification (section 9), whenever the index lists a
`Records` frame, reads it and walks every record (a damaged frame is `HashMismatch`), checks the chunk indices,
chunk sums and block order above, and checks the record ids of every block graph, all before it decodes a block.

Writing. A writer given records writes them as one `Records` frame after the entry table and before the index, and
sets the index's `records_offset` and `records_len`; the envelope's `max_frame_payload` admits the frame (section 7).
The reference writer refuses an encoder whose graph names a record id it was not given (`RecordOutOfRange`), a
record that does not parse under the rules above, and a record whose `kind` differs from its body's (`BadOptions`).

Errors of this section: `UnknownRecordKind`, `ReservedRecordBits`, `RecordHashMismatch`, `RecordOutOfRange`,
`BadRecord`, and `Truncated` and `TrailingBytes` as above.

## 13. Recovery

A `Recovery` frame (kind 4) lets a reader rebuild damaged bytes of the archive: Reed-Solomon over fixed-size shards
in GF(2^16), the construction of the `reed-solomon-simd` library (Leopard-RS). An archive may have any number
of recovery frames; the index lists them (section 6).

### Coverage

A recovery frame covers one contiguous byte range of the archive, `[cover_offset, cover_offset + cover_len)`. The
reference writer makes it the range from the end of the header (offset 32) to the end of the last frame before
the recovery frame: every `ChunkData` frame, the `EntryTable` frame and the `Records` frame, if any. The index and
the trailer are not covered: the trailer holds the index hash, which authenticates the index, and a damaged index
is not repaired by this version.

### Shards

The covered range is cut into `data_shards` shards of `shard_len` bytes; the last shard is padded with zeros to
`shard_len` (the padding is never stored in the archive). `shard_len` is a multiple of 64 and not 0; the
reference writer's default is 65536 and it is a writer option. `data_shards` is `ceil(cover_len / shard_len)`.
`recovery_shards` is chosen by the writer, at least 1, and the reference writer takes `ceil(data_shards * percent /
100)` with `percent` from 1 to 20 (0 means no recovery frame). The writer refuses more than 32768 data shards
(`BadOptions`, `reason` `recovery shards`) and a `percent` above 20; a frame has at most 65535 shards in all
(`data_shards + recovery_shards`). The shards are coded in GF(2^16) as the `reed-solomon-simd` encoder does, with
`data_shards` original shards and `recovery_shards` recovery shards of `shard_len` bytes each.

### Payload

The payload is, in this order:

| Field | Size | Meaning |
|---|---|---|
| cover_offset | 8 | absolute offset of the first covered byte (little-endian); at least the header length |
| cover_len | 8 | number of covered bytes; the range ends at or before the index |
| shard_len | 4 | length of every shard in bytes; a multiple of 64, not 0 |
| data_shards | 4 | `ceil(cover_len / shard_len)` |
| recovery_shards | 4 | number of recovery shards; at least 1; `data_shards + recovery_shards` is at most 65535 |
| shard_hashes | data_shards * 32 | BLAKE3-256 of each data shard, the last one padded with zeros to `shard_len` |
| recovery | recovery_shards * shard_len | the Reed-Solomon recovery shards, in order |

Consistency rules, each a `BadRecovery` with the `reason` in brackets: `shard_len` is a multiple of 64 and not 0
[`shard_len`]; `cover_offset` is at least 32 and the range ends at or before the index [`cover range`]; the
cover is not empty and `data_shards` equals `ceil(cover_len / shard_len)` [`data_shards`]; `recovery_shards` is at
least 1 [`recovery_shards`]; `data_shards + recovery_shards` is at most 65535 [`shard count`]; the payload's
length is exactly 28 plus `data_shards * 32` plus `recovery_shards * shard_len` [`payload length`]. A payload
shorter than the 28 fixed bytes is `Truncated` (`what` is `recovery`). The length is checked before anything
is allocated, so a declared count never allocates more than the payload holds.

### Writing

A writer holds one shard buffer, hashes each shard when it completes, and feeds the coded shards to the encoder in
order. The encoder needs `data_shards` when it is created, and a stream writer learns it only when the covered
range ends, so the reference writer spools the completed shards to an anonymous temporary file and reads them back
once into the encoder at the end; memory is one shard buffer, 32 bytes per shard and the recovery shards
(`percent` of the covered length at most). The writer then writes the `Recovery` frame after the `Records` frame
(or the entry table) and before the index, and records its location in the index; `max_frame_payload` admits it
(section 7).

### Repair

A reader repairs an archive in these steps. It opens the archive; an archive whose index cannot be read is not
repaired and the error of the opening is the result. For each recovery frame the index lists:

1. Read the frame at its recorded location and check its hash. A frame that fails (its hash, its kind or its length)
   is unusable: it is counted and skipped, and its coverage is not protected by it. A frame that passes its hash but
   breaks a rule above is an error (`BadRecovery`).
2. Cut the covered range into shards and hash each one (the last padded with zeros). A shard whose hash differs
   from the frame's `shard_hashes` entry is damaged. This locates damage shard by shard without decoding.
3. When the number of damaged shards is at most `recovery_shards`, rebuild them: give the decoder every intact data
   shard and as many recovery shards as there are damaged data shards, and take the rebuilt shards back. Every
   rebuilt shard must match its `shard_hashes` entry, otherwise the result is `RecoveryError`.
4. Write the rebuilt bytes (without the padding) at their place in the copy.

When more shards are damaged than the frame can rebuild, nothing is rebuilt for that frame (`Unrepairable`, with
the frame's position in the index's list, the damaged count and the capacity). The reader goes on with the other
frames, the copy carries every repair that was possible, and the first `Unrepairable` is the result. A frame that is
itself damaged does not change the data it covers: no repair is attempted from a frame that fails its hash.

Detection without repair is the same scan without step 3 and 4: a count of the frames, of the unusable frames and
of the damaged shards. `lpk-decode check <archive>` prints these counts and exits 1 if a shard is damaged or a
frame is unusable; `lpk-decode repair <archive> <out>` writes the repaired copy to a file that must not exist and
exits 1 on an error, keeping the copy only when the error is `Unrepairable`.

### What is not covered

The header, the index frame and the trailer are not covered. Damage there is not repaired: damage to the index or
trailer makes the archive fail to open, and the error is the one that opening gives. Recovery frames do not cover
each other, and a frame lying inside another frame's range is repaired like any other bytes.

Errors of this section: `BadRecovery`, `Unrepairable`, `RecoveryError`, `BadFrameLocation` (with `what` `recovery`),
`Truncated`.
