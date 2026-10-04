# LitePack `.lpk` format, version 1 — freeze candidate

Status: freeze candidate. This revision applies the findings of two independent readings of the text and of an
independent decoder written from it (`docs/spec/CHANGES.md` lists every change); once frozen it is format v1.
Revision 1.1 (section 2) adds the decoding of `jpeg-reconstruct` (section 8) and nothing else.
The reference reader is the `lpk-format` crate; the tables below are checked against it by a test. Section 16 lists
every error class. `docs/spec/CONFORMANCE.md` states what an independent decoder must do with the test vectors.

## 1. Conventions

- Byte order: every fixed-width integer is little-endian.
- Varint: unsigned LEB128 for a `u64`, seven bits per byte, low group first, the high bit of a byte
  meaning "more bytes follow". At most 10 bytes. Readers require the canonical (shortest) form: the last
  byte of a multi-byte varint must not be `0x00` (so `0x80 0x00` is an error), and a tenth byte, if
  present, must be exactly `0x01`; a varint that continues past ten bytes is an error.
- Hash: BLAKE3-256 (32 bytes, default output).
- Sizes: all lengths are in bytes.
- Text: every string this specification quotes in backticks as a constant (context strings, associated-data
  prefixes, `info` strings) is ASCII and is used as its bytes, with no length prefix and no terminator. Paths are
  UTF-8 that is well-formed as RFC 3629 defines it: no surrogate code points (U+D800 to U+DFFF), no overlong
  forms, nothing above U+10FFFF.
- Normative terms: MUST, MUST NOT and MAY have their RFC 2119 meaning. Every rule a reader enforces names its
  error class; the class, with its `reason` or `what` string where one is given, is normative, and the message
  text is not. Section 16 lists every class with its fields and where it is raised. Remarks that name the
  reference implementation's libraries, defaults or internal choices are informative.
- Order of checks: where one input can break several rules, the section gives the order, and the first rule
  broken in that order is the error.

### What is authenticated

Each check below authenticates (detects any change to) exactly what it names:

- A frame's hash: its payload as stored (the sealed bytes when sealed). It is a plain hash, so it detects damage;
  it does not stop someone who rewrites the payload and its hash together.
- The trailer's `index_hash`: the index frame's payload as stored. The trailer is the root of a plain archive:
  everything the index records is authenticated through it.
- The Merkle root in the index: the chunk table's hashes; each chunk's bytes are then checked against their
  record (length and BLAKE3-256) before they are used.
- The index's `entry_table_hash`: the entry table's payload as stored.
- The index's frame locations: where every block, the entry table, the records frame and every recovery frame
  are, and their lengths. A record's `body_hash` authenticates its body; a recovery frame's `shard_hashes`
  authenticate the covered bytes shard by shard.
- In an encrypted archive (section 14): the header flags, through the key wrap's associated data; and every
  sealed payload, through its AEAD tag, whose associated data binds the archive id, the kind, the sequence and
  the sealed length.

Only checked for consistency, not authenticated: the header's `version_minor`; in a plain archive the header's
`archive_id` (compared with the trailer's) and the header flags; and the envelope (kind, flags, `payload_len`) of
every frame in a plain archive, and of every unsealed frame in an encrypted one. A changed envelope fails a
location, kind, length or hash check, with one exception: a MUST_UNDERSTAND bit set on a frame of a known kind is
ignored (section 3), so it changes nothing that is read.

## 2. Header

The header is fixed at 32 bytes at offset 0.

| Offset | Size | Field | Value / meaning |
|---|---|---|---|
| 0 | 8 | magic | `0x89 0x4C 0x50 0x4B 0x0D 0x0A 0x1A 0x0A` (`\x89LPK\r\n\x1a\n`) |
| 8 | 2 | version_major | 1 |
| 10 | 2 | version_minor | the revision the writer wrote under: 0, or 1 for revision 1.1 (see "Revisions") |
| 12 | 4 | flags | see below |
| 16 | 16 | archive_id | 16 random bytes chosen by the writer |

The magic follows the PNG pattern: a non-ASCII lead byte, the name, CR LF to catch line-ending
conversion, SUB to stop the DOS `type` command, and LF.

A reader checks the header in this order: the length (input shorter than 32 bytes is the error
`Truncated { what: "header" }`), the magic (a mismatch is the error `BadMagic`), the major version
(`UnsupportedMajor` with the value found), and only then the flags, because a future major version may redefine
them: a reserved bit set is `ReservedHeaderBits` with the reserved bits found, and then `LISTABLE` set without
`ENCRYPTED` is `BadHeaderFlags`. `version_minor` and `archive_id` are not checked here.

Version rule: a reader accepts `version_major` 1 and any `version_minor`; any other major version is
`UnsupportedMajor`.

Revisions. Version 1 grows by revisions that add the decoding of primitives the registry already lists (section
8); a revision never changes the meaning of a byte an earlier revision defines. Revision 1.0 decodes `store`,
`zstd` and `lzma`. Revision 1.1 (minor 1) adds the decoding of `jpeg-reconstruct` (0x0007, section 8 "Decoding
`jpeg-reconstruct`") and nothing else. `version_minor` is the revision the writer wrote under, a declaration and
not a fact about the archive's blocks: the archive may use any primitive of that revision or an earlier one, and
none of a later one (section 8). A reader does not infer from the minor which primitives occur; it checks the
primitives of each block as section 8 says. So a writer of revision 1.1 writes 1 whether or not any block names
a primitive of revision 1.1 (informative: the reference pipeline writes 1 whenever its JPEG peel is enabled). A
reader of revision 1.0 accepts an archive of minor 1 (any minor), lists it,
verifies its frames and records, and reports `UnimplementedPrimitive` with the ID 7 for every block whose graph
names `jpeg-reconstruct`, exactly as for any primitive it does not run.

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
| 1 | SEALED | the payload is sealed: the nonce, the ciphertext and the tag, in that order (section 14); the frame hash covers the sealed bytes |
| 2-15 | reserved | must be zero; a reader rejects the frame otherwise |

Frame kinds:

| Kind | Name |
|---|---|
| 1 | EntryTable |
| 2 | ChunkData |
| 3 | Records |
| 4 | Recovery |
| 5 | Index |
| 6 | Trailer |
| 7 | KeySlot |

Kind 0 is invalid (`InvalidKind`). Kinds 8 to 0x7FFF are reserved for later versions of this specification.
Kinds 0x8000 to 0xFFFF are experimental; a v1 writer MUST NOT write them.

Reading a frame, in this order:

1. Input that ends exactly at a frame boundary is a clean end. Input that ends inside the four bytes of kind and
   flags is `Truncated` (`frame header`).
2. A reserved flag bit set is `ReservedFrameBits` with the reserved bits found.
3. Kind 0 is `InvalidKind`.
4. `payload_len`: a non-canonical varint is `NonCanonicalVarint`, one that continues past ten bytes is
   `VarintTooLong`, and input that ends inside it is `Truncated` (`frame header`).
5. A `payload_len` above the reader's limit is `PayloadTooLarge` with the length and the limit, raised before any
   payload byte is read or allocated. The limit is the reader's `max_frame_payload` resource (section 7); once the
   index is read it is the smaller of that and the envelope's `max_frame_payload`.
6. A kind the reader does not know with MUST_UNDERSTAND set is `UnknownMustUnderstand` with the kind. Without
   the bit the frame is skipped: its payload is still read and hashed, and the kind, flags and payload length are
   reported to the caller.
7. Input that ends inside the payload is `Truncated` (`payload`); inside the hash, `Truncated` (`hash`).
8. A payload whose BLAKE3-256 differs from the stored hash is `HashMismatch` with the kind, for known and unknown
   kinds alike.

MUST_UNDERSTAND on a frame of a known kind is accepted and ignored; writers do not set it. The trailer is the
exception: its shape is fixed (section 6), so a trailer with any flag set is not a trailer.

No unaccounted frames: every frame of a v1 archive is the key slot (section 14), a frame that the index of the
latest generation or of an earlier one lists, or the index or trailer of a generation (section 15). A writer MUST
NOT write any other frame. A reader that opens an archive through its trailer never reads such a frame; when one
shifts a recorded frame the location checks of section 6 fail (`BadFrameLocation`), and the diagnosis walk
(section 6) reports what it finds as it would for any frame.

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

Entry flags (a reserved bit set is `ReservedEntryBits` with the bits and the entry index):

| Bit | Name | Meaning |
|---|---|---|
| 0 | EXECUTABLE | the file is executable |
| 1 | HIDDEN | the entry is hidden |
| 2 | READ_ONLY | the entry is read-only |
| 3 | SYSTEM | the entry is a system file |
| 4-15 | reserved | must be zero; a reader rejects the entry otherwise |

Path rules: a path is UTF-8 with `/` as the separator, 1 to 65535 bytes long. It has no leading or
trailing `/`, no empty component, no component equal to `.` or `..`, and contains no `\` and no NUL byte.
A violation is `InvalidPath` with the entry index and a reason, checked in this order: a `path_len` of 0
("empty"); a `path_len` above 65535 ("too long"); then, once the path bytes are read (a payload that ends inside
them is `Truncated`), bytes that are not well-formed UTF-8 ("not utf-8"); a leading `/` ("leading slash"); a `\`
anywhere ("backslash"); a NUL byte anywhere ("nul"); a trailing `/` ("trailing slash"); and then, component by
component from the left, an empty component ("empty component") or a component `.` or `..` ("dot component").
So `/a\b` is "leading slash" and `a//..` is "empty component".
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
is `Truncated` for "entry table", raised when the count is read. Likewise a file's `chunk_count` larger than the
bytes that remain in the payload after it (every index takes at least one byte) is `Truncated` for "entry
table", raised when the count is read.

Check order: a reader checks each entry in field order, so when several rules are broken the first of
these wins: kind, flags, path (length, then truncation, UTF-8, the path rules in the order above), sort order,
mtime, size
(and for a directory its size), then the symlink target or the chunk list. Entries are checked in order,
and after the last one the trailing-bytes check applies.

Names: paths are opaque to the table. They may contain `:` and names that Windows reserves (`CON`, a
component ending in a dot or a space). An extractor must map or refuse such names and must never let a component
replace or escape the target directory; that rule belongs to the extractor (section 9 states the reference
tool's), not to this format.

Reading: a reader first reads only `entry_count` (and applies the bound above); entries are then decoded one after another as a
stream, so a table of any size can be walked without holding all entries in memory.

## 5. Chunks and the Merkle tree

The archive's content is a list of chunks, numbered from 0 in table order. An entry's `chunks` field (section 4)
holds indices into this list. The chunk table has one record per chunk and is embedded by the index frame
(section 6); this section defines its encoding, the Merkle tree over it, and how files and byte ranges are
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
| block_count | varint | number of blocks; at most the bytes left after it divided by 6 |
| frame_offset | varint | per block: absolute offset of the block's `ChunkData` frame |
| frame_len | varint | per block: whole encoded length of that frame |
| first_chunk | varint | per block: index of the first chunk the block holds |
| chunk_count | varint | per block: number of chunks the block holds |
| plain_len | varint | per block: sum of the `plain_len` of its chunks |
| sequence | varint | per block: position of the block's frame among the archive's frames (section 14) |
| entry_table_offset | varint | absolute offset of the `EntryTable` frame |
| entry_table_len | varint | whole encoded length of that frame |
| entry_table_sequence | varint | position of that frame among the archive's frames |
| entry_table_hash | 32 | BLAKE3-256 of the entry table's payload as stored (the sealed bytes when sealed) |
| records_offset | varint | absolute offset of the `Records` frame; 0 when there is none |
| records_len | varint | whole encoded length of that frame; 0 when there is none |
| records_sequence | varint | position of that frame among the archive's frames; 0 when there is none |
| recovery_count | varint | number of `Recovery` frames (section 13); at most the bytes left after it divided by 3 |
| recovery_offset | varint | per recovery frame: absolute offset of the frame |
| recovery_len | varint | per recovery frame: whole encoded length of that frame |
| recovery_sequence | varint | per recovery frame: position of that frame among the archive's frames |
| generation_count | varint | number of generations (section 15); at most the bytes left after it divided by 19 |
| generation | varint | per generation: its number; the entries count 0, 1, 2, ... |
| start_offset | varint | per generation: absolute offset of its first frame; strictly ascending |
| first_sequence | varint | per generation: sequence of that first frame; strictly ascending |
| salt | 16 | per generation: the salt of its sealed frames' nonces (zeros when not encrypted) |

The chunk table is not length-prefixed: a reader walks its declared records (under the count bound of
section 5) and continues after the last one. The six envelope varints follow the Merkle root; an input
that ends inside them is `Truncated` (`what` is `index`). The prior list (section 10) follows the envelope and
precedes `block_count`. The six block fields repeat `block_count` times; `sequence` is the sixth. The entry table's location (offset, length, sequence) is followed by `entry_table_hash`, 32 raw bytes; a reader that has the index compares it with the BLAKE3-256 of the entry table's payload as stored (the sealed bytes when the table is sealed) and fails with `EntryTableMismatch` when they differ.
Every `sequence` is the position of the frame among the archive's frames, counted from 0 for the first frame after the header (section 14); plain archives write them too. "No records frame" is `records_offset`, `records_len` and `records_sequence` all 0. Positions count every frame, including the key slot and the indexes and trailers of earlier generations (sections 14 and 15). The reader does not otherwise check a sequence: a wrong one makes the frame fail its tag when it is opened (section 14), and in a plain archive it is not used. Bytes after
the generation table are `TrailingBytes` (`what` is `index`); an input that ends early is `Truncated`
(`what` is `index`, or `chunk table` inside the table). A `block_count` larger than the bytes left divided by 6
is `Truncated` and nothing is allocated for it; a `recovery_count` larger than the bytes left divided by 3 is
`Truncated` likewise. Each recovery location follows the same location rule as the records frame and overlaps no
block, no other recovery frame, the entry table and the records frame; the recovery frames are in ascending offset
order and, when there are any, the last one ends at the offset of the index frame (section 13); a violation is
`BadFrameLocation` with `what` `recovery`.

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

The generation table (section 15) is checked entry by entry as it is read, each a `BadGenerationTable` with the
`reason` in brackets: the entry's `generation` equals its position [`generation`]; the first entry's
`start_offset` is 32 [`first start_offset`] and its `first_sequence` is 0 [`first first_sequence`]; each later
entry's `start_offset` and `first_sequence` are greater than the previous entry's [`order`]. The rules that need
the trailer are checked when the archive is opened (below).

Parse order. A reader parses the index payload and applies the rules in this order; the first failure is the error:

1. The chunk table (section 5): its count bound, its records, `Truncated` (`chunk table`).
2. `merkle_root`, the six envelope varints (`threads_hint` above 4294967295 is `EnvelopeMismatch` here), the prior
   list (its count bound, then `BadPriorList` entry by entry), `block_count` (its bound) and the block fields, the
   entry table's location and hash, the records location, `recovery_count` (its bound) and the recovery locations,
   `generation_count` (at most the bytes left divided by 19, otherwise `Truncated`) and the generation entries
   with their rules above; any input that ends early is `Truncated` (`index`).
3. Bytes left after the generation table: `TrailingBytes` (`index`).
4. The block rules, block by block: the location (`BlockOutOfRange`), the frame order and the single empty block
   (`BlockCoverage`), the chunk range (`BlockCoverage`), and at the end blocks that stop short (`BlockCoverage`
   with the number of blocks).
5. The entry table's location, then the records location (`BadFrameLocation`).
6. The entry table and the records frame against the blocks and each other (`BadFrameLocation`).
7. Each recovery location (`BadFrameLocation`, `recovery`), then their overlaps with the blocks, the entry table,
   the records frame and each other, then their ascending order and where the last one ends (all
   `BadFrameLocation`, `recovery`).
8. One pass over the chunk records against the blocks (`BlockLengthMismatch`, `BlockCoverage`), then the Merkle
   root (`MerkleRootMismatch`).
9. The envelope rules of section 7, in the order given there.

### The trailer frame (kind 6)

The trailer is written with empty frame flags and has a fixed 96-byte payload:

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 8 | index_offset | absolute offset of the index frame (u64, little-endian) |
| 8 | 8 | index_len | whole encoded length of the index frame (u64) |
| 16 | 32 | index_hash | BLAKE3-256 of the index frame's payload |
| 48 | 8 | generation | 0 for the first write, one more for every append (u64) |
| 56 | 16 | archive_id | must equal the header's archive_id |
| 72 | 8 | previous_trailer_offset | absolute offset of the previous generation's trailer frame; 0 for generation 0 (u64) |
| 80 | 16 | salt | random per-generation salt for the sealed frames' nonces; zeros when not encrypted |

`index_hash` covers the index payload as stored: the sealed bytes when the index is sealed, as for
`entry_table_hash`.

The whole trailer frame is `kind` (2) + `flags` (2) + `payload_len` varint (1 byte, the value 96) + payload
(96) + hash (32) = 133 bytes, and it is the last thing in the archive. The index must lie at or after the
end of the header and end exactly where the trailer starts (no gap), otherwise `BadFrameLocation` (`index`). A
trailer of generation 0 whose `previous_trailer_offset` is not 0 is `BadTrailer` (`reason`
`previous_trailer_offset`).
An archive that has been appended to (section 15) holds the trailers of its earlier generations as ordinary frames
inside the body; `previous_trailer_offset` chains them, and the reader opens the last one.

### Opening an archive

Reading a recorded frame. Every read of a frame at a location the index (or the trailer) records is bounded
by the recorded length: nothing past `offset + len` is read. In this order: a recorded length too short to hold
the kind is `BadFrameLocation`; the kind must be the expected one (`WrongFrameKind` with the expected and the
found kind); then the frame is read as in section 3 under the bound. A frame that the bound cuts (its envelope
claims more than the recorded length) is `BadFrameLocation`, and its hash is never reached; a frame that ends
before the bound is read and hashed first (`HashMismatch`), and then its encoded size, which differs from the
recorded length, is `BadFrameLocation`. `BadFrameLocation` names the frame: `index`, `entry table`, `records`,
or, for a block or a recovery frame read this way, the kind's name `ChunkData` or `Recovery`. In an encrypted
archive the sealing rules and the tag (section 14) are checked after the frame is read.

Opening an archive is one procedure, in this order:

1. If the input is shorter than the header plus the trailer frame (165 bytes), go to the diagnosis below.
2. Read the 32-byte header (section 2).
3. Encrypted archive (header flag `ENCRYPTED`): read and parse the key slot that must follow the header
   (section 14, its own order of checks). With credentials: a key slot whose Argon2 memory exceeds the reader's
   `memory` resource is `Refused` (field `argon2_m`), then the key is unwrapped (`WrongKey`). Without
   credentials the archive opens keyless (section 14). Plain archive: a frame of kind 7 right after the header is
   `UnexpectedKeySlot`. The key slot is read before the trailer.
4. Read the last 133 bytes. They must be a frame of kind 6, empty flags, payload length 96, with a valid hash;
   if they are not (or they cannot be read), go to the diagnosis below. A trailer whose `archive_id` differs from
   the header's is `ArchiveIdMismatch`. A trailer whose `generation` exceeds the file length divided by 133 cannot
   be true (each generation holds at least a trailer frame) and is `BadTrailer` (`reason` `generation`). A
   generation-0 trailer with a non-zero `previous_trailer_offset` is `BadTrailer` (`previous_trailer_offset`).
5. A keyless archive stops here and walks the frame envelopes instead (section 14).
6. The index location: it must start at or after offset 32 and end exactly where the trailer starts, otherwise
   `BadFrameLocation` (`index`). Read the frame there as a recorded frame of kind 5 (above), under the reader's
   `max_frame_payload`.
7. The BLAKE3 of its payload as stored must equal the trailer's `index_hash` (`IndexHashMismatch`).
8. Encrypted archive: the index must be sealed (`UnsealedFrame`) and is opened with the trailer's salt and the
   sequence 2^64 - 1 - `generation` (`AuthenticationFailed`).
9. Parse the index (parse order above). The reader walks the chunk table once to find where it ends, then
   makes one pass over its records that checks the block lengths, gathers the hashes for the Merkle root and
   builds the chunk index.
10. The generation table against the trailer, each a `BadGenerationTable`: it has `generation + 1` entries
    [`count`]; the last entry's salt is the trailer's [`salt`]; in a generation above 0 the last entry's
    `start_offset` is `previous_trailer_offset + 133` [`start_offset`]; in an archive that is not encrypted every
    salt is zero [`salt not zero`].
11. Compare the envelope with the reader's resources (section 7); an archive that needs more is `Refused`
    before any block is read. From here the frame limit is the smaller of the envelope's and the reader's
    `max_frame_payload`.
12. The entry table is read only when asked, as a recorded frame of kind 1; then its payload as stored must hash
    to `entry_table_hash` (`EntryTableMismatch`).

### Truncated versus corrupt

When the tail is not a valid trailer, the reader walks the frames forward from the end of the header, under
the read limits of section 3, skipping unknown kinds as there. If every frame it reads verifies and the input
ends exactly at a frame boundary, or inside a frame, without a trailer frame having been read, the archive is
cut short: `Truncated` with `what` set to `trailer`. If a frame fails its hash, the error is that frame's
`HashMismatch` (or whatever error the frame grammar gives). A trailer frame does not end the walk when bytes follow it: they are the next
generation (section 15), and the walk goes on, so an append that was cut short ends in `Truncated` with `what` `trailer`
like any other cut. Only when the first frame after a trailer fails to read for any reason other than being cut short
(a reserved flag bit, kind 0, a bad varint, a payload above the limit, an unknown kind with MUST_UNDERSTAND, or a hash
that does not verify) is the error `TrailingBytes` with `what` set to `archive`; the sealing rules of section 14, which
the walk also applies to every frame it reads, keep their own errors there. The walk also reports how many frames verified and
the offset just after the last good frame, so a repair tool knows where the readable part ends. Two edge
cases: if the walk ends on a trailer frame that is not of the fixed shape (flags not empty, or a payload that is
not 96 bytes) with nothing after it, the error is `NoTrailer`; and a header that is shorter than 32 bytes or
invalid ends the walk at once with the header's own error (section 2), not `Truncated` with `what` `trailer`.

Errors of this section: `MerkleRootMismatch`, `IndexHashMismatch`, `ArchiveIdMismatch`,
`BlockLengthMismatch`, `BlockCoverage`, `BlockOutOfRange`, `BadFrameLocation`, `WrongFrameKind`,
`EnvelopeMismatch`, `Refused`, `BadPriorList`, `BadTrailer`, `BadGenerationTable`, `EntryTableMismatch`,
`UnexpectedKeySlot`, `NoTrailer` (the last frame is not a trailer of the fixed shape), plus `Truncated` and
`TrailingBytes` with the `what` strings `index`, `chunk table`, `trailer` and `archive`.

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
largest `plain_len` of any block. `max_frame_payload` is the largest frame payload in the archive: it covers every recorded frame (below).
`decode_memory`
is the writer's estimate of the peak memory of one decoding thread, in bytes, and `threads_hint` is the number
of independent blocks a reader may decode at once without exceeding `decode_memory` times `threads_hint`
(0 = no hint; a value above 4294967295 is `EnvelopeMismatch` with `field` `threads_hint`).

Order of the checks. A `threads_hint` above 4294967295 is raised while the envelope is read, before the block
rules and before a later truncation can be noticed. The other mismatches are checked after the block rules and
after `MerkleRootMismatch` of section 6, in the order below:

- `max_block_plain` must equal the maximum `plain_len` over the block table (0 when there are no blocks),
  otherwise `EnvelopeMismatch` with `field` `max_block_plain`.
- `max_frame_payload` is `M` below. The length of the index payload must be at most `M`, and every recorded
  frame length `L` must satisfy the bound below. The recorded frames are, in the order they are checked: every
  block's `frame_len`, the entry table's, the records frame's when there is one, and every recovery frame's. The
  bound is `L <= M + 36 + varint_len(M)`, where `varint_len(M)` is the encoded length of `M` as a varint (36
  bytes are the kind, flags and hash); otherwise `EnvelopeMismatch` with `field` `max_frame_payload`. The right
  side grows with `M`, so a larger `M` than needed is allowed. A recorded length that is the length of no
  frame, such as 165 (payloads 127 and 128 give 164 and 166) or 16422, is admitted by the first `M` whose bound
  reaches it (128 and 16384).
- The index payload contains the envelope's own varints, so `max_frame_payload` depends on the size it is part
  of: a writer repeats the computation until the value is stable. An index payload larger than the reader's own
  `max_frame_payload` is not `Refused`: reading the frame fails with `PayloadTooLarge` (section 3), because the
  reader's limit applies while the index is read.

The format itself does not limit any value of the envelope; the limits are the reader's. The reader's resources
are `max_window`, `max_bwt_block`, `max_block_plain`, `max_frame_payload` and `memory`, all in bytes. The `memory`
resource also bounds the Argon2 memory of a key slot (section 14) and the decoder buffer of a recovery group
(section 13). The reference reader's defaults (informative):

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
`params` are one canonical varint, the `record_id` (1 to 10 bytes, nothing after it); the record (section 12)
holds the rest of what the primitive needs. Violating a layout's rules, such
as a `window_log` of 9 or 32, an `lc` of 9, a `patch_format` of 2, or a `record_id` that is empty, not a
canonical varint or followed by other bytes (reason `record_id`), is `BadParams` carrying the ID and a short
reason. A `params` whose length differs from the layout's is `BadParams` with the reason `length`.

Primitives 3 to 12. Revision 1.0 gives `bwt`, `bcj-x86`, `bcj-arm64`, `delta` and the six reconstruction
primitives an ID, a parameter layout and their resources, but no decoding. Revision 1.1 specifies the decoding of
`jpeg-reconstruct` (below). A writer MUST NOT emit a primitive whose decoding no revision up to the archive's
`version_minor` specifies, and a reader MUST report `UnimplementedPrimitive` with the ID for a graph that names a
primitive it does not run, before it runs any step (it still validates their parameters as above). The decoding
of the others is specified by later revisions.

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

Bounds. Every intermediate output is bounded by the archive's envelope `max_block_plain` (section 7), which the
reader has already compared with its own resource; a larger one is `PayloadTooLarge`. So whether a block decodes
never depends on the reader's resources beyond the checks that refuse the archive. The last step must produce
exactly `plain_len` bytes; any other length, including a decoder
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

Order of checks for a block, from its frame to its first step:

1. The block's frame is read at its recorded location as a frame of kind 2 (section 6; in an encrypted archive
   the sealing rules and the tag, section 14).
2. The graph is parsed: `step_count` (`BadGraph`, `step count`), then per step the primitive ID
   (`UnknownPrimitive`), the step flags (`BadGraph`, `step flags`), `params_len` (`BadGraph`, `params length`),
   the params bytes and their layout (`BadParams`); input that ends inside the graph is `Truncated` (`graph`).
3. When the graph names a reconstruction primitive, the archive's record count is read (the `Records` frame, with
   its own errors, section 12) and every `record_id` must be below it (`RecordOutOfRange`).
4. `plain_len` and `encoded_len` (`Truncated`, `block header`); `encoded_len` must equal the bytes that follow
   (`BlockLengthMismatch`).
5. The graph's resources against the envelope: a window above `max_window`, then a BWT block above
   `max_bwt_block` (`EnvelopeMismatch` with that field).
6. Every prior the graph names must be in the index's prior list (`UnlistedPrior`).
7. `plain_len` must equal the block table's (`BlockLengthMismatch`) and be at most the envelope's
   `max_block_plain` (`PayloadTooLarge`).
8. Every primitive of the graph must be one the reader runs (`UnimplementedPrimitive`), and only then does the
   first step run; each step then applies its own rules (below).

### What the reference decoder runs

The reference decoder knows the name, the parameter layout and the validation of all 13 primitives. Each ID is
in exactly one of two groups (informative; the normative rule for 3 to 12 is above):

- Implemented now: `store`, `zstd`, `lzma`.
- Requires the full reader: `bwt`, `bcj-x86`, `bcj-arm64`, `delta`, `jpeg-reconstruct`,
  `deflate-reconstruct`, `png-filter`, `base64`, `utf16`, `container-reconstruct`.

For a primitive without a decoder the reference decoder reports `UnimplementedPrimitive` with the ID, before it
runs any step of the graph. The reference decoder (`lpk-format`, `lpk-decode`) stays a revision 1.0 reader; the
full reader of revision 1.1 is `lpk-core` (informative: it registers its `jpeg-reconstruct` decoder on an opened
archive, and the `lpk` tool extracts and tests through it).

### Decoding `jpeg-reconstruct` (revision 1.1)

The step's input is one Lepton stream and its output is the primary image of the JPEG file its record describes
(section 12): `primary_len` bytes ending with the primary image's EOI marker.

- The codec is defined by the library: the stream format is the one `lepton_jpeg`, the Rust port of Dropbox's
  Lepton (<https://github.com/microsoft/lepton_jpeg_rust>, Apache-2.0), versions 0.5.x, writes and decodes with its
  `compat_lepton_vector_write` settings (16-bit DC and predictor arithmetic). The upstream project
  (<https://github.com/dropbox/lepton>) is its origin but publishes no standalone format document, so the library
  version range is the definition, normatively by citation as section 8 cites RFC 8878 for `zstd`; a conforming
  decoder may use that library to decode the stream (D-49).
- `lepton_version` is the record's own field, not a byte of the stream: it names the revision of the codec, and 0
  (what `lepton_jpeg` 0.5 writes) is the only value of revision 1.1. The stream carries its own header (magic,
  version byte, sizes), which the library checks when it decodes; a stream whose header it refuses is
  `lepton stream`.
- One block holds exactly one Lepton stream, because a graph applies to the whole block: the block's `plain_len`
  is the record's `primary_len` and its chunk records are those of the primary image's bytes.
- After decoding, the decoder MUST assemble the original file as section 12 states (the primary image; then either
  the raw `trailing` bytes, or the secondary images at their offsets with the nested trailing data filling the
  ranges between and after them, in order) and check it: its length is `original_len` and its BLAKE3-256 is
  `original_hash`. Every chunk the record names lies in a block with a lower index than the block being decoded
  (`BadRecord`, `chunk order`) whose graph names no reconstruction primitive (`BadRecord`, `nested record`; section
  12, "Nesting"). The step's output is the primary image only.
- Order of checks inside the step, exactly as the reference performs them: (1) the record is looked up by its id
  (the errors of the `Records` frame, section 12); (2) its kind is `jpeg` (`BadRecord`, `kind`); (3) its
  `lepton_version` is 0 (`BadRecord`, `lepton_version`); (4) `primary_len` against the step's output bound
  (`PayloadTooLarge`; on a one-step graph the bound is the block's `plain_len`, and a `primary_len` that differs
  from it is observed as `BlockLengthMismatch` when the step's output length is compared, section 8 "Bounds");
  (5) resources: the frame header of the JPEG is read from the stream's own header and the image's memory term is
  compared with the decode memory left after `max_block_plain` (`Refused`, field `decode_memory`; a stream whose
  header cannot be read is `lepton stream`); (6) the stream is decoded (`BadRecord`, `lepton stream` when the
  library refuses it or its output would exceed `primary_len`; `primary_len` when the output is shorter);
  (7) assembly, in file order: per secondary image, the nested trailing bytes before it (`BadRecord`, `gainmaps`
  when its offset lies before the bytes already assembled; `trailing` when the nested trailing data ends before
  that offset), then its chunks (`gainmaps` when their lengths do not add up to its `len`); then the rest of the
  nested trailing data. Each chunk is fetched in that order with the chunk order and nesting checks above, then
  the chunk's own checks (section 9: `ChunkIndexOutOfRange`, the block's errors, `ChunkMismatch`); (8) the
  assembled length is `original_len` and its BLAKE3-256 `original_hash` (`BadRecord`, `original_hash`, also for a
  length that would overflow 64 bits).
- Resources: memory per image, declared by the envelope's `decode_memory` (section 7). The reference writer
  declares `max_block_plain` plus, for the largest peeled image, an allowance: a coefficient term computed from the
  image's frame header over the library's data layout (each component's 8x8 blocks, padded to whole MCUs, times
  128 bytes: 64 coefficients of 2 bytes) plus a fixed term of 67108864 bytes (64 MiB) for the library's models and
  thread buffers. Revision 1.1 states no bound on the library's working set; the fixed term is not measured (task
  E2-5b measures it). A decoder compares the image's term (the same computation) with the archive's
  `decode_memory`, never above its own memory resource, minus `max_block_plain`, and refuses an image whose term
  exceeds it with `Refused` (field `decode_memory`, the class `Archive::open` uses for an envelope the reader
  cannot meet, section 7), without decoding it.

### Normative decoders

The decoding of a `zstd` step is that of RFC 8878 (Zstandard frames), and the decoding of an `lzma` step is that of
the LZMA SDK's `lzma-specification.txt` (LZMA1, its reference decoder `LzmaSpec`), including how much input each
consumes. For an `lzma` step, the end of the stream is the input position of that decoder after the end-of-payload
marker, or, for a stream without the marker (allowed only in the last step of a graph), its input position when the
output bound is reached; "trailing input" is any byte after that position. For a `zstd` step the input is a sequence
of whole frames and nothing else.

### What the reference decoder enforces for `zstd`

A decoder decodes a `zstd` step's input as one zstd frame or a sequence of frames (skippable frames are skipped)
into at most the step's output bound. It applies these rules, in this order:

1. The `window_log` must be 10..=31 (`BadParams`, `window_log`).
2. The window the step declares, 2^`window_log` bytes, must not exceed the reader's `max_window`; otherwise
   `WindowTooLarge` with the window needed and the window allowed. This is raised before the input is read.
3. When `dictionary` is not all zeros the reader's store of priors (section 10) must hold a prior of that ID,
   and the bytes it returns must hash to the ID; otherwise `MissingPrior`, raised before the input is read.
   The prior must be a zstd dictionary as RFC 8878 section 5 defines it (it begins with the magic number
   0xEC30A437, stored little-endian as the bytes `37 A4 30 EC`); other bytes are a `ZstdError`.
4. Input of zero bytes with a non-zero output bound is `ZstdError` with the reason `truncated`.
5. Each frame's `Window_Size`, as RFC 8878 defines it (for a single-segment frame, its `Frame_Content_Size`), must
   not exceed 2^`window_log` (`BadParams`, `frame window exceeds
   declared`); the decoder allocates nothing for a window before this check.
6. Dictionaries: when the step names a prior, every frame is decoded with that prior, whether its header carries
   no `Dictionary_ID` or the prior's; a frame whose `Dictionary_ID` is another one is `ZstdError`. When the step's
   `dictionary` is all zeros, a frame that carries a `Dictionary_ID` is `ZstdError`.
7. Output past the step's bound is `PayloadTooLarge` (`BlockLengthMismatch` on the last step, as above); a
   frame that is cut short or damaged, or whose content checksum (when it has one) does not match the output,
   is `ZstdError`; the `reason` is the decoder's own text and is not normative.

Errors of this section: `UnknownPrimitive`, `UnimplementedPrimitive`, `BadGraph`, `BadParams`,
`BlockLengthMismatch`, `PayloadTooLarge`, `WindowTooLarge`, `ZstdError`, `LzmaError`, and `Truncated` with the
`what` strings `graph` and `block header`.

### What the reference decoder enforces for `lzma`

The encoded bytes of an `lzma` step are a raw LZMA1 stream: no `.lzma` or `.xz` header (the properties are
the step's parameters, the output length is the step's output bound). In the last step of a graph the output
bound is the block's `plain_len` and the decoder stops when it has produced exactly that many bytes. A step that
is not the last has only a bound (the envelope's `max_block_plain`), so its stream MUST end with the
end-of-payload marker: a stream without it that reaches the bound is `LzmaError` with the reason
`marker required`, and one that ends early without it is `LzmaError` with `truncated`. `lzma` parameters carry no prior ID: v1
has no priors for LZMA (section 10 is about `zstd` only). The rules, in this order:

1. `lc` <= 8, `lp` <= 4, `pb` <= 4 and `lc + lp` <= 4 (`BadParams`, `lc`, `lp`, `pb` or `lc + lp`), in that
   order. The last limit bounds the table of literal probabilities (0x300 * 2^(`lc` + `lp`) entries); under it
   `lc` 5 to 8 can never be used, although the parameter layout allows them. (Informative: it is liblzma's
   `LZMA_LCLP_MAX`; the LZMA SDK would accept up to 12.)
2. `dict_size` must not exceed the reader's `max_window`; otherwise `WindowTooLarge` with the size needed and
   the size allowed, raised before the input is read.
3. A match distance greater than `dict_size`, or greater than the number of bytes produced so far, is
   `LzmaError`. Whether a stream decodes never depends on the reader's
   `max_window` beyond rule 2: a decoder needs a buffer of the smaller of `dict_size` and
   the output bound (at least 1 byte), which is exactly the reach a valid stream can use.
4. The first byte of the range coder stream must be 0, else `LzmaError` with reason `range coder`. The
   range coder's end condition (its code value being 0 after the last symbol) is checked only when the
   end-of-payload marker is present; a stream that ends at the output bound without a marker is not checked
   for it, and a decoder must not refuse a marker-less stream because its final code value is not 0.
5. In the last step the stream may end with the end-of-payload marker or without one (the test vectors carry
   it; `.lzma` files of known size do not), and a decoder that has produced the output bound accepts either. Any input after the stream (after the marker, when there is one)
   is `LzmaError` with reason `trailing input`; so is a stream whose symbols continue past the output bound
   when the bound falls between two symbols. Input that ends before the output bound is reached, or a
   marker before it, is `LzmaError` with reason `truncated`.
6. A match that crosses the output bound is `PayloadTooLarge` (`BlockLengthMismatch` on the last step); the
   decoder never writes past the bound. Any other damage is `LzmaError`; its `reason` is the decoder's own text
   and is not normative, except the reasons `range coder`, `trailing input`, `truncated` and `marker required`
   named above.

## 9. Writing and reading an archive

### Writing an archive as a stream

An archive can be written without seeking and without reading back what was written: every offset the index
records is a count of the bytes already written. The frames follow in this order:

1. the header (section 2);
   in an encrypted archive, the `KeySlot` frame (kind 7) right after it (section 14);
2. zero or more `ChunkData` frames (kind 2), each holding one block;
3. the `EntryTable` frame (kind 1);
4. optionally the `Records` frame;
   (with recovery on, a `Recovery` frame (kind 4) follows each group of data frames, ahead of the next frame, and
   the last one closes the data frames, as section 13 says);
5. the `Index` frame (kind 5), which records the location of every frame above, the chunk table and the Merkle
   root over its hashes, and the decode envelope (section 7);
6. the `Trailer` frame (kind 6), which locates the index.

In a plain archive the header flags are zero and no frame has a flag set; an encrypted archive sets the header
flags and the `SEALED` frame flag as section 14 says, and nothing else. An archive without file content has no blocks
and an empty chunk table.

The index payload states its own length through `max_frame_payload`, whose varint width depends on the value.
A writer settles this by computing the index again with the length it just produced until the declared
`max_frame_payload` equals the larger of the index payload length and, over every other recorded frame of length
`L`, the smallest `M` with `L <= M + 36 + varint_len(M)` (section 7).

### Chunks and blocks

A file's bytes are cut into chunks of exactly the chunk size, the last one shorter; an empty file has no chunks.
Chunks are numbered in the order they are written; this writer gives each file a run of consecutive numbers
(files may be added in any order, so the numbers need not follow the entry order), but a reader accepts any
chunk list whose indices are in range and whose lengths add up to the file size. The chunk table has one record
per chunk (section 5). The rule that chooses the cut is a property of the writer, not of the format: a reader
finds each chunk's length in the chunk table and never assumes a size, so another way of cutting changes only
the chunk boundaries. A
writer's chunker is fed the bytes of one file as a stream and may hold back a tail of at most one chunk; the
chunker starts afresh for each file, so a chunk never holds bytes of two files, and no chunk is longer than the
chunk size.

Chunks fill blocks in order. A writer closes the current block before the chunk that would take it past its
size limit, so a block holds whole chunks and a chunk never spans two blocks. A block of a store-only archive
has the graph `[store]`; for such an archive the envelope declares `max_window` 0, `max_bwt_block` 0,
`threads_hint` 0 and `decode_memory` equal to `max_block_plain`, the space of one block buffer; the encoded
input a decoder reads beside it is not counted in `decode_memory`.

A block is encoded by the writer's block encoder, which reports, for each block, its decode graph and the decoder
resources that graph needs (they feed the envelope); the writer checks each graph when its block closes, records
it in that block's header, and lists the priors the graphs name in the index (section 10). The identity encoder
produces the graph `[store]`. A writer may also close a block early, before it is full.

A writer that meets an I/O error, on its input or its output, a chunker that breaks the rules above, an encoder
error, or a graph that fails its check, refuses every later call: the archive being written is abandoned.

Entries are written in strictly ascending path order (section 4). A writer refuses an invalid path and a path it
was already given (`DuplicateEntry`); it accepts entries in any order and writes the table sorted.

### How a writer lays out a peeled JPEG (revision 1.1, informative but exact)

The reference writer writes a file whose primary image it peeled (section 8, "Decoding `jpeg-reconstruct`") as one
entry whose parts it writes in this order, while the entry's chunk list is in file order:

1. The bytes after the primary image, as nested parts: each secondary image (a complete JPEG inside the trailing
   data of a file whose primary image carries an MPF or gain-map marker) and each run of trailing data between and
   after them, written plainly or through the model like any chunks. Their chunks lie in blocks with lower indices
   than the primary's block, because the primary's block is closed after them.
2. The record (section 12), added to the `Records` frame once those chunk indices are known: `trailing` empty,
   `nested_trailing_chunks` the trailing runs' chunks in order, one secondary image entry per secondary image, and
   `original_hash` of the whole file. The `Records` frame is still one frame before the index.
3. The primary image as a block of its own: the graph `[jpeg-reconstruct {record_id}]`, the Lepton stream as its
   encoded bytes, and `plain_len` and chunk records those of the primary image's bytes, so the chunk table hashes
   the original bytes and verification is unchanged.

A file whose peel fails (the library refuses it, the decoded stream differs from the primary image, there is no
EOI, or the stream is not smaller than the primary image) is written as it is, through the normal path.

### Reading one block at a time

A reader reads a chunk by finding its block in the chunk index, reading that block's frame, decoding the block
with its graph (section 8) and cutting the chunk out of the plain bytes at the offset its predecessors in the
block leave. Before decoding, the block's graph is checked against the envelope (section 7): a graph whose
resources need a window above `max_window` or a BWT block above `max_bwt_block` is `EnvelopeMismatch` with that
field, raised when the block header is parsed. It keeps the plain bytes of the block it read last, so chunks
read in order decode each block once.
Every chunk is compared with its record in the chunk table (length and BLAKE3) before its bytes are used; a
mismatch is `ChunkMismatch` with the chunk's number. A block whose frame hash fails is that frame's `HashMismatch`
(kind 2), whether a file of the block is extracted or the archive is verified: the error names the damaged frame,
not a chunk. `ChunkMismatch` is reported only when the block's frame is intact and decodes, and a chunk cut from it
differs from its record.

Whole-archive verification first walks the `Records` frame when the index lists one (section 12), then decodes
every block and compares every chunk with its record, so a chunk no entry uses is checked too; a block whose frame
hash fails is that frame's `HashMismatch`. It then checks every file entry's chunk list and total size against the
chunk table, which needs no block reads.

Verification and recovery. `verify` leaves recovery to `check` in every mode: with the key it checks the frame hashes
of the frames it reads, the chunks, the entries and the records; without the key it checks the frame hashes of every
frame but the recovery frames and the sealing rules (the key slot's place and shape), and reports that the chunks were
not checked (section 14). Neither reads a recovery payload, and neither fails on a damaged recovery frame or shard.
`check` and `repair` (section 13) are the recovery commands.

Only the block read last is kept, so a chunk list that alternates between chunks of two blocks makes every
reference read and decode a whole block; a reader of untrusted archives needs a decode budget.

### Extraction by the reference tool

The reference tool (`lpk-decode`) extracts files and directories and applies these refusals before it writes
anything:

- a symlink entry is refused (`SymlinkRefused`); what an extractor does with links is a policy outside this
  format;
- a path component that is a Windows device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1` to `COM9`, `COM` followed
  by a superscript 1, 2 or 3 (U+00B9, U+00B2, U+00B3), `LPT1` to `LPT9`, `LPT` followed by one of the same
  superscripts, `CONIN$`, `CONOUT$`),
  compared in any letter case on the part before the first dot with its trailing spaces removed, so that
  `con .txt` is refused as well as `CON.txt`; a component that contains `:`; or one that ends in a dot or a space
  is refused (`UnsafePath`);
- an entry one of whose parent paths is an entry that is not a directory, such as a file `a` beside `a/b`, is
  refused (`UnsafePath`, reason `conflicting name`).

A file whose parent directories have no entry of their own is extracted, its missing parents created. The tool
does not overwrite an existing file and reuses existing directories. Each file is created with
"create new" semantics at its final path; a file the tool created and could not finish is removed, and a file
that was already there is left untouched. Paths in the tool's listing have control characters escaped.

Errors of this section: `UnsortedEntries`, `InvalidPath`, `ChunkMismatch`, `HashMismatch`, `FileSizeMismatch`,
`ChunkIndexOutOfRange`, `SymlinkRefused`, `UnsafePath`; and for writers `BadChunk` (a chunker that breaks the
cutting rules) and `BadOptions`.

The tool takes `--prior <file>` (repeatable) for the priors an archive needs (section 10); `info` lists the
IDs the index names.

## 10. Priors

A prior is a byte string a decoder needs besides the archive, for example a zstd dictionary. Its ID is
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
block header is parsed, before the block is decoded. The index may list a prior that no block names; a reader
does not check that. `BadPriorList` as above.

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
- `jpeg-peel.lpk` (revision 1.1): one JPEG with an MPF marker, a secondary image and trailing data, peeled; written
  by `lpk-core`'s pipeline (`cargo test -p lpk-core --test jpeg_vector -- --ignored` regenerates it), and the same
  `lepton_jpeg` version and the same `flate2` backend (the library deflates its stream header; the workspace
  build, which the committed bytes come from, uses the zlib backend) are needed to reproduce its bytes.

The normal tests check that each archive decodes to its generated contents and that `lpk-decode verify` accepts
it (with `--prior tests/vectors/zstd-dict.prior` for the dictionary one). The full list of vectors, including the
encrypted, journal, recovery and malformed ones, and what an independent decoder must do with each, is in
`docs/spec/CONFORMANCE.md`. The vectors are not a normative
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
- Nesting. Every chunk a record names lies in a block whose graph names no reconstruction primitive, so a
  reconstruction step reads plain blocks only and never recurses. A violation is `BadRecord` with `reason`
  `nested record`, raised at the reconstruction step when it reads that chunk (section 8). A writer refuses to
  close a block whose graph names a record that breaks the chunk order or this rule, as it refuses any graph
  a reader would refuse.

### What the reference decoder does with records

The reference decoder parses, validates and hashes records (the frame hash, every `body_hash`, every field rule
above) and checks the record ids of every block header it parses; it applies none. A block whose graph names a
reconstruction primitive is reported as `UnimplementedPrimitive` (section 8), as before. Applying records needs the
JPEG and Deflate libraries of the full reader; revision 1.1's full reader applies `jpeg` records (section 8,
"Decoding `jpeg-reconstruct`"), reading the chunks a record names through the archive's blocks, which by the
nesting rule above are plain blocks (the reference full reader refuses any other with `nested record`). Whole-archive
verification (section 9), whenever the index lists a
`Records` frame, reads it and walks every record (a damaged frame is `HashMismatch`), checks the chunk indices,
chunk sums and block order above, and checks the record ids of every block graph, all before it decodes a block.

Writing. A writer given records writes them as one `Records` frame after the entry table and before the index, and
sets the index's `records_offset` and `records_len`; the envelope's `max_frame_payload` admits the frame (section 7).
The reference writer refuses an encoder whose graph names a record id it was not given (`RecordOutOfRange`), a
record that does not parse under the rules above, and a record whose `kind` differs from its body's (`BadOptions`).

Errors of this section: `UnknownRecordKind`, `ReservedRecordBits`, `RecordHashMismatch`, `RecordOutOfRange`,
`BadRecord`, and `Truncated` and `TrailingBytes` as above.

## 13. Recovery

`Recovery` frames (kind 4) let a reader rebuild damaged bytes of the archive. The covered bytes are cut into
groups of data shards and each group has its own frame, so the memory a writer or a repairing reader needs depends
on the group, not on the archive. The index lists every frame (section 6).

The code. Version 1 defines the Reed-Solomon code by reference: a systematic erasure code over GF(2^16), the
Leopard-RS construction as implemented by the `reed-solomon-simd` library, version 3.x, with its 64-byte shard
interleave (each shard's bytes are coded in 64-byte units, which is why `shard_len` is a multiple of 64). The
recovery shards of a frame are those that library's encoder produces for the frame's `group_shards` data shards
(the real ones followed by the implicit zero shards) and its `recovery_shards`. Which pairs of counts are allowed is
that library's `ReedSolomonDecoder::supports(group_shards, recovery_shards)`. A normative description of the field,
its basis and the shard layout, independent of the library, is planned for a later revision; until then an
independent decoder MUST report damage from the shard hashes (which need no Reed-Solomon code) and MAY leave repair
out.

### Coverage and groups

A group is a run of whole data frames (`ChunkData`, `EntryTable`, `Records`) written one after another. Its
`Recovery` frame is written immediately after them and covers exactly their bytes: `cover_offset` is the end of
the previous recovery frame (offset 32, the end of the header, for the first) and `cover_offset + cover_len` is
the offset of the recovery frame itself. So each recovery frame covers the frames written since the previous
recovery frame (or the header), and no recovery frame is covered by any group. In an encrypted archive the key slot
is the first frame after the header, so the first group covers it and its bytes count towards that group. The index and the trailer are not
covered: the trailer holds the index hash, which authenticates the index, and a damaged index is not repaired by
this version. A writer closes a group, and writes its frame, when the next data frame would make the group longer
than `group_shards * shard_len` bytes; the last group closes before the index, so every data frame is covered
when recovery is on. A single frame longer than a group cannot be written (`BadOptions`, `reason` `group smaller
than a block`; the reference writer also refuses at the start a `block_size` plus 4096 above that length).
The group's bytes are cut into shards of `shard_len` bytes (the last padded with zeros; the padding is never
stored). The frame's `group_shards` is the number of data shards the group is coded with. The reference writer
sizes it by the data actually written (`group_shards` equals `data_shards`, no padding); a reader accepts a frame
whose `group_shards` is larger, in which case the shards after the real ones are implicit all-zero shards, never
written, and the reader pads in the same way.

The tiling has two parts, checked at different times. When the index is parsed (section 6): the recovery locations
ascend and the last one ends where the index starts, or, when the latest generation wrote no recovery frame, it lies
before that generation's `start_offset` and ends at or before the previous trailer's offset (`BadFrameLocation`,
`what` `recovery`). Opening an archive reads no recovery payload. When `check` or `repair` reads a frame (below),
the frame's range must end where the frame itself starts and start where the previous recovery frame ends (offset 32
for the first); a frame that breaks this is not an error: it is counted as unusable, with the reason `coverage`. The
one exception is the first frame of a later generation (section 15): its range starts at a `start_offset` of the
index's generation table that lies after the previous recovery frame's end, because the old index and trailer are
not covered. A frame belongs to the generation in whose range of the generation table its offset lies; no byte of
an uncovered old trailer is read to decide this. `BadRecovery` is the class of a recovery payload that breaks the
rules of the payload below, and inside `check` and `repair` it too only makes the frame unusable.

### Geometry and bounds

`shard_len` is a multiple of 64, not 0 and at most 16777216 (16 MiB). `group_shards` is 1 to 32768 and
`group_shards * shard_len` is at most 1073741824 (1 GiB). `data_shards`, the real shards of a group, is
`ceil(cover_len / shard_len)` and between 1 and `group_shards`. `recovery_shards` is at least 1 and
`group_shards + recovery_shards` is at most 65535, and the pair must be one the code supports (above; a frame
that is not is `BadRecovery`, `reason` `shard count`). The reference writer takes `recovery_shards =
ceil(group_shards * percent / 100)` (at least 1) from the frame's own `group_shards`, with `percent` from 1 to 20
(0 means no recovery frames). Its options are `percent`, `shard_len` (default 65536) and `group_shards`, the most
shards a group may take (default 2048: 128 MiB, so that the default 64 MiB block fits). It refuses options outside
these bounds, and options whose full group would need more than the default `memory` resource of section 7
(2147483648 bytes) to repair (`BadOptions`, `reason` `repair memory`).

### Payload

The payload of a frame is, in this order:

| Field | Size | Meaning |
|---|---|---|
| cover_offset | 8 | absolute offset of the first covered byte (little-endian); at least the header length |
| cover_len | 8 | number of covered bytes; the range ends at or before the index and overlaps no recovery frame |
| shard_len | 4 | length of every shard in bytes; a multiple of 64, not 0, at most 16777216 |
| data_shards | 4 | real shards in this group: `ceil(cover_len / shard_len)`, at least 1, at most `group_shards` |
| group_shards | 4 | data shards the group is coded with, 1 to 32768; shards after `data_shards` are implicit zero shards; `group_shards * shard_len` is at most 1073741824 |
| recovery_shards | 4 | number of recovery shards; at least 1; `group_shards + recovery_shards` is at most 65535 |
| shard_hashes | data_shards * 32 | BLAKE3-256 of each real data shard, the last one padded with zeros to `shard_len` |
| recovery | recovery_shards * shard_len | the Reed-Solomon recovery shards, in order |

Consistency rules, each a `BadRecovery` with the `reason` in brackets: `shard_len` [`shard_len`]; `cover_offset` at
least 32 and the range ends at or before the index [`cover range`]; `group_shards` in range [`group_shards`] and
`group_shards * shard_len` within 1 GiB [`group size`]; `data_shards` as above [`data_shards`]; `recovery_shards` at
least 1 [`recovery_shards`]; the sum at most 65535 [`shard count`]; the payload's length exactly 32 plus
`data_shards * 32` plus `recovery_shards * shard_len` [`payload length`]. A payload shorter than the 32 fixed
bytes is `Truncated` (`what` is `recovery`). The rules are checked in the order listed, and the length before
anything is allocated, so a declared count never allocates more than the payload holds. That a frame's coverage overlaps no recovery frame needs the
index's list; the repairing reader checks it and counts a frame that breaks it as unusable.

### Writing and memory

A writer keeps the bytes of the open group (at most `group_shards * shard_len`). When the group closes it cuts
them into shards, hashes each, builds the encoder for exactly that many data shards and the group's
`recovery_shards`, encodes, and writes the frame at once; nothing is kept after that and no temporary file is used.
The index lists the frames and `max_frame_payload` admits them (section 7). Memory rule: the open group's bytes;
the encoder's work buffer for that group, `work_count * shard_len` bytes where `work_count` is the group's shards
rounded up to a multiple of `next_pow2(recovery_shards)`; and the group's recovery shards, `recovery_shards *
shard_len` bytes, held until the frame is written. The three coexist at the moment a group is encoded, so the
writer's peak is their sum for one group. A small archive therefore costs in proportion to its size, and no
term grows with the archive.

### Repair

A reader repairs an archive in these steps. It opens the archive; an archive whose index cannot be read is not
repaired and the error of the opening is the result. For each recovery frame the index lists:

1. Read the frame at its recorded location and check its hash. A frame that fails (its hash, its kind or its
   length), or that passes its hash but breaks a rule above or the coverage rule, is unusable: it is counted
   and skipped, and its coverage is not protected by it.
2. Cut the group's range into shards and hash each one (the last padded with zeros). A shard whose hash differs
   from the frame's `shard_hashes` entry is damaged. This locates damage shard by shard without decoding.
3. When the number of damaged shards is at most `recovery_shards`, rebuild them: give the decoder every intact
   data shard, the implicit zero shards and at least as many recovery shards as there are damaged data shards, and
   take the rebuilt shards back. Any such set of recovery shards gives the same result; a decoder may give all of
   them (the reference reader gives the first ones, as many as there are damaged shards). Recovery shards carry no
   hash of their own: a damaged recovery shard makes the frame fail its frame hash, and the frame is unusable.
   Every rebuilt shard must match its `shard_hashes` entry, otherwise the result is `RecoveryError`.
4. Write the rebuilt bytes (without the padding) at their place in the copy.

When more shards are damaged than a frame can rebuild, nothing is rebuilt for that frame (`Unrepairable`, with the
frame's position in the index's list, the damaged count and the capacity). The reader goes on with the other
frames, the copy carries every repair that was possible, and the first `Unrepairable` is the result. A frame that
is itself damaged does not change the data it covers.

Memory rule: one shard while scanning; for a group with damage, the decoder's buffer for that group,
`(next_pow2(recovery_shards) + group_shards).next_pow2() * shard_len` bytes, independent of the archive's size. A
reader refuses a group whose buffer would exceed its memory limit before rebuilding it (`Refused`, field `recovery
group`).

Detection without repair is the same scan without steps 3 and 4: counts of the frames, of the unusable frames and
of the damaged shards. `lpk-decode check <archive>` prints these counts and exits 1 (`DamageFound`) if a shard is
damaged or a frame is unusable; `lpk-decode repair <archive> <out>` writes the repaired copy to a file that must not
exist, prints the report and exits 1 on an error, keeping the copy when the error is `Unrepairable`. The exact output
lines are given in `docs/spec/CONFORMANCE.md`. `verify` never reads a recovery payload, with or without the key
(section 9), so a damaged recovery frame does not make it fail; `check` reports it.

### What is not covered

The header, the index frame and the trailer are not covered. Damage there is not repaired: damage to the index or
trailer makes the archive fail to open, and the error is the one that opening gives.

Errors of this section: `BadRecovery`, `Unrepairable`, `RecoveryError`, `DamageFound` (the tool's `check`), `Refused` (field `recovery group`), `BadFrameLocation` (with `what` `recovery`),
`Truncated` (`recovery`).

## 14. Encryption

An archive is encrypted when the header flag `ENCRYPTED` is set. Encryption seals the payloads of frames; the
frame envelope (kind, flags, length, hash) stays in clear, and the frame hash is the BLAKE3-256 of the payload as
stored, that is, of the sealed bytes. A reader therefore verifies integrity and skips frames without a key, and
recovery (section 13) works on the sealed bytes.

### Cipher suites

| Suite | Cipher | Key | Nonce | Tag |
|---|---|---|---|---|
| 1 | AES-256-GCM (default) | 32 | 12 | 16 |
| 2 | XChaCha20-Poly1305 | 32 | 24 | 16 |

### What is sealed

Which frames are sealed, by kind:

| Kind | Name | Sealed in an encrypted archive |
|---|---|---|
| 1 | EntryTable | yes; no in a listable archive |
| 2 | ChunkData | yes |
| 3 | Records | yes |
| 4 | Recovery | no |
| 5 | Index | yes |
| 6 | Trailer | no |
| 7 | KeySlot | no |

In a listable archive (header flag `LISTABLE`) the `EntryTable` frame is written in clear and everything else is as in
the table. The header, the key slot, the recovery frames and the trailer are never sealed.

Frame flag bit 1, `SEALED`, is set on exactly the frames that are sealed. A sealed payload is
`nonce | ciphertext | tag`: the nonce (12 or 24 bytes), the ciphertext (as long as the plain payload) and the 16-byte
tag. Opening a sealed payload, in this order: a payload shorter than the nonce plus the tag is `AuthenticationFailed`;
a stored nonce that differs from the derived one (below) is `AuthenticationFailed`; then the tag is checked
(`AuthenticationFailed`). The error names the kind and the sequence. The reader checks the flag against the table before it opens anything: a sealed frame where the table says no
(and any sealed frame of an archive that is not encrypted) is `UnexpectedSealedFrame`; an unsealed frame where the table
says yes is `UnsealedFrame`. A frame of unknown kind is skipped as in section 3 and is not subject to the table.

### The archive key, nonces and associated data

The writer draws a random 32-byte archive key `K`. Every sealed frame is encrypted with `K` directly. Nonces are
derived, never random:

`nonce = HKDF-SHA256(ikm = K, salt = archive_id, info = "LitePack lpk v1 nonce" || kind (u16 LE) || sequence (u64 LE) || salt (16))`,
cut to the nonce length of the suite (the first 12 or 24 bytes of the output). `salt` is the salt of the generation
that wrote the frame (section 15): a random value drawn for each generation, so that a generation written again after
a rollback never reuses a nonce of the generation it replaced. A reader finds the generation of a frame from its
sequence and the index's generation table (`first_sequence` ranges); the index itself uses the salt of the trailer it
was read with. An archive that is not encrypted has zero salts.

`sequence` is the position of the frame among the frames of the archive, counting every frame (sealed or not, the key
slot included) from 0 for the first frame after the header. The index frame is the exception (and the sequence below is reserved for it): the trailer cannot
name its position, so the index is sealed under the constant sequence 2^64 - 1, which no counted frame reaches. The index of generation `g` is sealed under `2^64 - 1 - g` (section 15), so no two generations of one archive seal an index under the same sequence. Each
(kind, sequence, generation salt) triple is unique, so no nonce is used twice under `K`; a random nonce could repeat across
the very large number of frames an archive may hold, and a repeat under GCM or Poly1305 breaks both secrecy and
authenticity. The sealed payload still carries the nonce; a reader recomputes it, and a payload whose stored nonce
differs is `AuthenticationFailed`.

The associated data of every sealed frame is `"LitePack lpk v1 AD" || archive_id (16) || kind (u16 LE) || sequence
(u64 LE) || payload_len (u64 LE)`, where `payload_len` is the frame's own `payload_len` field, the length of the
sealed payload. A sealed frame moved to another position, another archive or another kind therefore fails its tag. The
index records the sequence of every frame it locates (section 6): the block table, the entry table, the records frame and
the recovery frames each carry a `sequence`.

### The key slot (kind 7)

An encrypted archive's first frame after the header is the key slot, in clear; no other key slot may appear
(`MissingKeySlot` when the first frame is another kind, `UnexpectedKeySlot` for a key slot in an archive that is not
encrypted or a second one). Its payload is exactly 111 bytes:

| Field | Size | Meaning |
|---|---|---|
| suite | 1 | cipher suite: 1 = AES-256-GCM, 2 = XChaCha20-Poly1305 |
| kdf | 1 | key derivation: 1 = Argon2id |
| argon2_t | 4 | Argon2 passes, little-endian; 1 to 64 |
| argon2_m_kib | 4 | Argon2 memory in KiB, little-endian; at least 8192 |
| argon2_p | 4 | Argon2 lanes, little-endian; 1 to 64 |
| salt | 16 | Argon2 salt |
| keyfile_required | 1 | 0 or 1: a keyfile is mixed into the key derivation |
| wrapped_key | 48 | the archive key encrypted under the KEK (32 bytes of ciphertext, then the 16-byte tag) |
| check | 32 | BLAKE3 keyed with the archive key, of the archive id |

Reading the key slot, in this order: the frame is read under a payload limit of 111 bytes (a larger `payload_len`
is `BadKeySlot`, `length`; the other errors of section 3 apply); a slot with the `SEALED` flag is
`UnexpectedSealedFrame`; a payload that is not exactly 111 bytes is `BadKeySlot` (`length`); `suite` not 1 or 2 is
`BadKeySlot` (`suite`); `kdf` not 1 is `BadKeySlot` (`kdf`); `argon2_t`, then `argon2_m_kib`, then `argon2_p`
outside their bounds is `BadArgon2` (`t`, `m`, `p`); `keyfile_required` not 0 or 1 is `BadKeySlot`
(`keyfile_required`).

The key encryption key, the KEK, is Argon2id (RFC 9106, version 0x13) of the password bytes with the slot's 16-byte
salt, `t` passes, `m` KiB of memory and `p` lanes, no secret and no associated data, and a 32-byte output. With a
keyfile (`keyfile_required` is 1) it is `HKDF-SHA256(ikm = argon2_output || BLAKE3-256(keyfile bytes), salt = salt,
info = "LitePack lpk v1 keyfile")` with a 32-byte output. A keyfile given when `keyfile_required` is 0 is ignored. `wrapped_key` is `K` encrypted with the suite under the
KEK, the all-zero nonce of the suite's length (12 or 24 zero bytes; safe because every KEK is used once: the salt
is fresh per archive) and the associated data `"LitePack lpk v1 keywrap" ||
archive_id || header_flags (u32 LE)`: the header flags are bound, so a flipped `ENCRYPTED` or `LISTABLE` bit fails at the key slot. `check` is `BLAKE3-256` keyed with `K` of the `archive_id`, a second and cheap way to tell a right key from a
wrong one.

Argon2 parameters: the writer's default is `t = 3`, `m = 65536` KiB, `p = 4` (RFC 9106's second recommendation) and it
accepts others within `1 <= t <= 64`, `m >= 8192` and `1 <= p <= 64`; a reader refuses parameters outside these bounds (`BadArgon2`),
and a key slot whose `m` times 1024 exceeds the reader's memory resource is `Refused` (field `argon2_m`), before the
password is derived. The password is the raw bytes given, with no normalisation (the reference tool strips one trailing CR LF or LF from a password file).

### Order of checks and outcomes

1. The header is read; with `ENCRYPTED` set the key slot is read next, before any other frame. A damaged key slot is
   `BadKeySlot` or `BadArgon2`.
2. The key slot is opened with the credentials. A wrong password, a wrong or missing keyfile and a damaged wrapped key
   are all `WrongKey`; after unwrapping, the reader also compares `check` with the BLAKE3-256 of the archive id keyed
   with the unwrapped key, and a mismatch is `WrongKey`. No frame after the key slot has been read.
   Without credentials the archive opens keyless, in these steps: the trailer is read and checked as in section 6
   (step 4); then, from the end of the key slot (sequence 1), the frame envelopes are walked: each
   frame's kind, flags and `payload_len` are read and its payload is skipped by its declared length, without being
   read or hashed, so the sequence count continues past a damaged frame. A frame that would end past the file is
   `Truncated` (`frames`); a key slot met again is `UnexpectedKeySlot`. The walk records every `Recovery` frame (its
   location and sequence), takes the last `EntryTable` frame as the entry table, records the end of every trailer that
   is not the last frame as a generation start, and stops at the trailer that ends the file. A listable archive
   without an `EntryTable` frame is `BadFrameLocation` (`entry table`). A keyless reader can check and repair the
   archive with the recovery frames (section 13), which work on the sealed bytes, assigning frames to generations by
   offset against the generation starts it found. Its `verify` walks every frame as the diagnosis walk does (section
   6; the frame hashes and the sealing rules), except that a recovery frame whose hash fails is passed over, reads no
   recovery payload, and reports that the chunks were not checked (section 9); a listable
   archive's entry table can be listed (it is not authenticated without the index); everything that needs the index or
   a sealed frame (`extract`, a sealed entry table) is `PasswordRequired`.
3. Only then are the trailer, the index and the blocks read. A modified frame envelope or payload fails the frame hash
   first (`HashMismatch`); a payload modified together with its frame hash fails the tag: `AuthenticationFailed` naming the
   kind and the sequence. The entry table, whether sealed or in clear, is compared with the index's `entry_table_hash`
   (`EntryTableMismatch`), so a clear table of a listable archive cannot be rewritten undetected by a reader that
   has the key.

The reference tool takes the password with `--password-file` (one trailing newline is not part of it) or `--password`
(for tests), and a keyfile with `--keyfile`; `list` works on a listable archive without either.

Errors of this section: `PasswordRequired`, `WrongKey`, `AuthenticationFailed`, `UnexpectedSealedFrame`, `UnsealedFrame`,
`UnexpectedKeySlot`, `MissingKeySlot`, `BadKeySlot`, `BadArgon2`, `Refused` (field `argon2_m`), `EntryTableMismatch`,
`Truncated` (`frames`).

### Test vectors

`sealed-aes.lpk`, `sealed-xchacha.lpk`, `sealed-listable.lpk` and `sealed-keyfile.lpk` (with `sealed-keyfile.key`) are
committed under `crates/lpk-format/tests/vectors/`; their test passwords are in `vectors.toml`. They use Argon2 `t = 1`,
`m = 8192`, `p = 1` and are written with a seeded random generator, so regenerating them gives the same bytes.

## 15. Generations (journal)

An archive grows by appending. Each write is a generation: generation 0 is the archive as first written (sections 6 and
9), and an append adds generation `g + 1` after the trailer of generation `g`. Nothing before that trailer is
rewritten, so every earlier trailer stays valid and "undo the append" is a truncation: append-only updates with
deduplication against what is already stored.

| Rule | Value |
|---|---|
| generation number | 0 for the first write; an append writes the previous number plus one |
| trailer length | 133 bytes (payload 96), the same for every generation |
| previous_trailer_offset | offset of the previous generation's trailer frame; 0 in generation 0 |
| generation salt | 16 random bytes per generation (zeros when not encrypted), in the trailer and in the index's generation table |
| index | complete: every chunk, block, recovery frame and prior of every generation, and the generation table |
| entry table | complete: the entries of the new generation, in sorted order |
| frame sequence | the position of the frame in the file, counting every frame; a new generation continues after the previous trailer |
| index sequence | 2^64 - 1 - generation |
| nonce | derived from the key, the archive id, kind, sequence and the salt of the generation that wrote the frame |
| deduplication | a new chunk with the BLAKE3 and length of a chunk of the old table is referenced, not written |
| recovery | a generation's recovery frames cover only its own data frames |
| commit | the append is committed when the last byte of the new trailer is written, after the data is synced |
| rollback | truncate the file to the end of an earlier generation's trailer |

### Layout of an append

After the trailer of generation `g` the writer appends, in this order: the new `ChunkData` frames (with their
recovery frames when recovery is on); the `EntryTable` frame; a `Records` frame when the append brings its own; the
last recovery frame; the `Index` frame; the `Trailer` frame with `generation = g + 1`, `previous_trailer_offset` set
to the offset of the old trailer and a fresh `salt`. The new index describes the whole archive, so a reader opens the
latest generation exactly as it opens a one-shot archive; the open path (section 6) only gains the generation table
checks below:

- The chunk table lists every chunk of every generation, in order: the old chunks keep their numbers, the new ones
  follow. Chunks that only deleted entries referenced stay in the table, unreferenced, so the table only grows.
- The block table lists every block of every generation, in ascending offset order; new blocks cover the new chunks
  only, so the blocks still partition the table.
- The entry table is complete: the old entries that still exist, the new and the replaced ones, in sorted order. A
  replaced path has its new chunk list; a deleted path is absent.
- The prior list and the recovery list are the unions over all generations. The envelope is recomputed over the
  whole archive. The `Records` location is the old frame's unless the append wrote a new `Records` frame, which then
  replaces it; every old record must be in it at its old position and unchanged (the writer compares them in order:
  `BadRecord` for a changed one, `RecordOutOfRange` for a missing one), because old blocks name records by position.
- The generation table (after the recovery list) has one entry per generation, 0 to the latest, in order: the
  generation number, the `start_offset` of its first frame (the end of the header for generation 0, the end of the
  previous trailer for the others), the `first_sequence` of that frame and the generation's `salt`. The rules and
  their `BadGenerationTable` reasons are in section 6: when the index is parsed, `generation` equals the position,
  the first entry starts at offset 32 with `first_sequence` 0, and offsets and sequences ascend strictly; when the
  archive is opened at the trailer of generation `g`, the table has `g + 1` entries, the last one carries the
  trailer's salt and (for `g` above 0) starts at `previous_trailer_offset + 133`, and a plain archive's salts are
  all zero. The last entry's `generation` is then `g`.

A trailer whose `generation` is larger than the file length divided by 133 is refused (`BadTrailer`); a writer that
would pass the largest `u64` refuses too.

### Sequence numbers, salts and nonces

The sequence of a frame is its position in the file, counting every frame: the key slot is 0 in an encrypted archive,
and the indexes and trailers of earlier generations count like any other frame. This is the rule; the reference
writer finds the first sequence of an append as the largest sequence the old index lists plus 3 (the old index, the
old trailer, then the new frame), which is the same number because nothing but the old index and trailer follows the
last listed frame. The index is the exception: it is sealed under `2^64 - 1 - g` in generation `g`.

A nonce is a function of the archive key, the archive id, the frame kind, the sequence and the salt of the generation
(section 14). After a rollback to generation `g` and a new append, the new generation `g + 1` has the same sequences
as the one it replaces, and its index the same index sequence: only the fresh random salt keeps the nonces apart, and
the writer draws it from a cryptographic RNG for every generation of an encrypted archive. The key slot is written once,
in generation 0, and reused. Appending to an encrypted archive needs the credentials: the writer unwraps the key slot
with them (`WrongKey` for a wrong password, `AppendNeedsCredentials` when none were given or the archive was opened
without them) and seals with that key.

### The trailer chain

`previous_trailer_offset` is 0 in generation 0. Opening checks only the last trailer (section 6), with the generation
table tying it to the start of its generation. Walking the chain back (the reference reader's history, and rollback)
applies these checks to each trailer from the last one down, in this order: its index must end exactly where it starts
(`BadFrameLocation`, `index`); at generation 0 its `previous_trailer_offset` must be 0 (`BadTrailer`,
`previous_trailer_offset`) and the walk ends; otherwise the previous trailer frame (133 bytes) must end at or before
its index (`BadFrameLocation`, `trailer`) — that bound is also the bound of the read: the 133 bytes at the offset are
read as a recorded frame location, so a frame that would run past the newer index is `BadFrameLocation` before its
shape is judged; it is read as a trailer of the fixed shape (`NoTrailer`), with a valid hash (`HashMismatch`), not
cut short (`Truncated`, `trailer`); it must carry the header's archive id (`ArchiveIdMismatch`) and
the generation number one lower (`GenerationMismatch` with the expected and the found number). The chain lists, newest
first, each generation's number, trailer offset, index offset and index hash. `lpk-decode info` prints the chain length,
or the chain error when an old trailer is damaged (the archive itself still opens at its last trailer).

### Deduplication against the old table

The reference writer's append builds a map from BLAKE3 to chunk number over the old chunk table (the first chunk wins
for a repeated hash; the map is held until the append finishes). A new chunk whose BLAKE3 and length
match an old chunk is referenced by its old number and not written; any other chunk goes to a new block. Chunks inside
the appended data are not deduplicated against each other, and a one-shot writer does not deduplicate at all; content
defined chunking and full deduplication come with the writer's later tasks.

### Recovery

The recovery frames of generation `g + 1` cover only its own data frames: the first group starts right after the old
trailer, at the generation's `start_offset` (the old index and trailer stay uncovered, as in section 13), and the
interleaving rule of section 13 goes on from there. Earlier generations keep their own frames, which the new index
still lists, so damage in any generation is repaired as before. The recovery list may end before the index when the
latest generation wrote no recovery frames (section 13).

### Without the key

A reader without credentials finds the frames by walking their envelopes, step by step as section 14 describes. The
walk passes every trailer that is not the last one (a trailer ends a generation, not the file), records the end of each
as a generation start, takes the last `EntryTable` frame as the current table and lists the recovery frames of every
generation.

### Rollback

`lpk-decode rollback <archive> <generation>` truncates the file to the end of the
named generation's trailer. The last complete trailer is found from the end of the file, or, when the end is not a
trailer, by walking the frames (with a frame length limit of the file's length); the chain is verified back to
generation 0 and the named generation must be on it (`NoSuchGeneration` with the requested and the latest number
otherwise). No credentials are needed. The file after a rollback is byte-identical to the archive as it was when that
generation was written. After a cut append the diagnosis (`Diagnosis.last_trailer_end`) and `rollback` name the last
complete generation (the diagnosis reports the offset after the last complete trailer): rolling back to the latest
complete generation removes the broken tail.

### The commit rule and a half-written append

An append is committed when the last byte of the new trailer is written, and the data it commits must be on disk first:
a writer flushes and syncs everything before the trailer to stable storage and syncs again after it. Until the
trailer is complete the last valid trailer is the old one, but it is no longer at the end of the file, so opening fails
through the diagnosis walk of section 6 with `Truncated` (`what` `trailer`). The walk continues past trailers, so it
reports the cut and the offset after the last complete trailer. `lpk-decode info` prints the generation and the length
of the chain.

Errors of this section: `NoSuchGeneration`, `AppendNeedsCredentials`, `GenerationMismatch`, `BadGenerationTable`,
`BadTrailer`, plus those of sections 6, 12, 13 and 14.

## 16. Error catalogue

Every error class, its fields and where it is raised. The class (with the `reason`, `what` or `field` string where the
text names one) is normative; the message text is not. "Reader" classes come from reading an archive; "writer" and
"tool" classes are given for completeness.

| Class | Fields | Raised by (section) |
|---|---|---|
| `BadMagic` | - | header: the first 8 bytes are not the magic (2) |
| `UnsupportedMajor` | `found` | header: `version_major` is not 1 (2) |
| `ReservedHeaderBits` | `bits` | header: a reserved flag bit is set (2) |
| `BadHeaderFlags` | `bits` | header: `LISTABLE` without `ENCRYPTED` (2) |
| `ReservedFrameBits` | `bits` | frame: a reserved flag bit is set (3) |
| `InvalidKind` | - | frame: kind 0 (3) |
| `NonCanonicalVarint` | - | any varint not in its shortest form (1, 3, 5) |
| `VarintTooLong` | - | any varint that continues past ten bytes (1, 3) |
| `Truncated` | `what`: `header`, `frame header`, `payload`, `hash`, `entry table`, `chunk table`, `index`, `trailer`, `graph`, `block header`, `records`, `record body`, `recovery`, `frames` | input or a payload ends inside the named part (2 to 15) |
| `HashMismatch` | `kind` | a frame's payload does not match its hash (3, 6, 9) |
| `UnknownMustUnderstand` | `kind` | an unknown kind with MUST_UNDERSTAND (3) |
| `PayloadTooLarge` | `len`, `max` | a `payload_len` above the frame limit (3); a block `plain_len` or an intermediate output above `max_block_plain`, a non-final step's output past its bound (8) |
| `UnsortedEntries` | `index` | entry table: a path not greater than the previous one (4) |
| `UnsupportedEntryKind` | `kind`, `index` | entry table: an unknown entry kind (4) |
| `ReservedEntryBits` | `bits`, `index` | entry table: a reserved entry flag bit (4) |
| `InvalidPath` | `index`, `reason` | entry table: the path rules and the symlink target (4) |
| `InconsistentEntry` | `index`, `reason` | entry table: "directory size" (reader); other reasons are writer refusals (4) |
| `TrailingBytes` | `what`: `entry table`, `chunk table`, `index`, `records`, `record body`, `archive` | bytes left after the last item of a payload, or broken bytes after a trailer (4 to 6, 12) |
| `ChunkMismatch` | `chunk` | an intact block yields a chunk that differs from its record (5, 9) |
| `ChunkIndexOutOfRange` | `chunk`, `len` | a chunk index not below the chunk count (5, 12) |
| `FileSizeMismatch` | `expected`, `found` | a file's chunks do not add up to its size (5) |
| `RangeOutOfFile` | `offset`, `len`, `file_len` | a byte range outside the file (5) |
| `MerkleRootMismatch` | - | index: stored root differs from the recomputed one (6) |
| `IndexHashMismatch` | - | the index payload does not match the trailer's `index_hash` (6) |
| `ArchiveIdMismatch` | - | a trailer's `archive_id` differs from the header's (6, 15) |
| `BlockLengthMismatch` | `block` | block lengths: index records against the chunk table, the block header, the last step's output (6, 8) |
| `BlockCoverage` | `block` | the blocks do not partition the chunk table, or frames out of order (6) |
| `BlockOutOfRange` | `block` | a block frame outside the body (6) |
| `WrongFrameKind` | `expected`, `found` | a recorded location holds another kind (6) |
| `NoTrailer` | - | the walk ends on a trailer of the wrong shape; an old trailer of the wrong shape (6, 15) |
| `BadGenerationTable` | `reason`: `generation`, `first start_offset`, `first first_sequence`, `order`, `count`, `salt`, `start_offset`, `salt not zero` | the generation table (6, 15) |
| `BadTrailer` | `reason`: `generation`, `previous_trailer_offset` | trailer fields out of range (6, 15) |
| `GenerationMismatch` | `expected`, `found` | trailer chain: a generation number not one lower (15) |
| `BadFrameLocation` | `what`: `index`, `entry table`, `records`, `recovery`, `trailer`, `ChunkData`, `Recovery`, `frames` | a recorded location out of range, overlapping, or not matching the frame there (6, 13, 14, 15) |
| `EnvelopeMismatch` | `field` | the envelope against the archive, or a block graph against the envelope (7, 9) |
| `Refused` | `field`, `needed`, `allowed` | the archive needs more than the reader allows: an envelope field, `argon2_m`, `recovery group` (7, 13, 14) |
| `UnknownPrimitive` | `id` | a primitive ID not in the registry (8) |
| `UnimplementedPrimitive` | `id` | a primitive the reader does not run (8) |
| `BadGraph` | `reason`: `step count`, `step flags`, `params length` | the decode graph (8) |
| `BadParams` | `id`, `reason` | a primitive's parameters break its layout; `frame window exceeds declared` (8) |
| `WindowTooLarge` | `needed`, `allowed` | a step's window above the reader's `max_window` (8) |
| `ZstdError` | `reason` | a `zstd` step's input (8) |
| `LzmaError` | `reason` | an `lzma` step's input (8) |
| `MissingPrior` | `id` | a prior the caller's store does not hold (8, 10) |
| `UnlistedPrior` | `id` | a block names a prior the index does not list (10) |
| `BadPriorList` | `reason`: `zero id`, `not ascending and unique` | the index's prior list (10) |
| `UnknownRecordKind` | `kind`, `record` | records: a kind outside 7 to 12 (12) |
| `ReservedRecordBits` | `bits`, `record` | records: non-zero record flags (12) |
| `RecordHashMismatch` | `record` | records: a body that does not match `body_hash` (12) |
| `RecordOutOfRange` | `record`, `count` | a block names a record the archive does not have (12) |
| `BadRecord` | `record`, `reason` | a record body breaks its rules (12) |
| `BadRecovery` | `reason` | a recovery payload breaks its rules (13) |
| `Unrepairable` | `frame`, `damaged`, `capacity` | repair: more damaged shards than a frame can rebuild (13) |
| `RecoveryError` | `reason` | repair: a rebuilt shard does not match its hash (13) |
| `DamageFound` | `damaged`, `unusable` | tool: `check` found damage (13) |
| `PasswordRequired` | - | a sealed frame or the index is needed without credentials (14) |
| `WrongKey` | - | the key slot does not open with the credentials (14) |
| `AuthenticationFailed` | `kind`, `sequence` | a sealed payload: too short, wrong nonce, or wrong tag (14) |
| `UnexpectedSealedFrame` | `kind` | a sealed frame where the rules say clear (14) |
| `UnsealedFrame` | `kind` | a clear frame where the rules say sealed (14) |
| `UnexpectedKeySlot` | - | a key slot in a plain archive, or a second one (6, 14) |
| `MissingKeySlot` | - | an encrypted archive whose first frame is not the key slot (14) |
| `BadKeySlot` | `reason`: `length`, `suite`, `kdf`, `keyfile_required` | the key slot payload (14) |
| `BadArgon2` | `reason`: `t`, `m`, `p` | Argon2 parameters out of bounds (14) |
| `EntryTableMismatch` | - | the entry table does not match `entry_table_hash` (6, 14) |
| `NoSuchGeneration` | `requested`, `latest` | rollback to a generation that is not on the chain (15) |
| `SymlinkRefused` | `path` | tool: extraction of a symlink entry (9) |
| `UnsafePath` | `path`, `reason` | tool: extraction of a path it refuses (9) |
| `BadOptions` | `reason` | writer: options it cannot honour (9, 13) |
| `DuplicateEntry` | `path` | writer: the same path added twice (9) |
| `BadChunk` | `reason` | writer: a chunker that breaks the cutting rules (9) |
| `AppendNeedsCredentials` | - | writer: append to an encrypted archive without credentials (15) |
| `SealFailed` | `kind`, `sequence` | writer: the cipher refused to seal (14) |
| `Io` | the I/O error | any read or write that fails below the format |

