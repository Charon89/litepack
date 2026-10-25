# `.lpk` v1 conformance: what an independent decoder must do

For an implementer who has only `docs/spec/lpk-v1.md` and `crates/lpk-format/tests/vectors/`. This is the
checklist an independent decoder is held to before the format is frozen. It states no performance figures.

## What "decodes all vectors" means

An independent decoder passes the gate when, for every archive in the table below:

1. it opens the archive (with the password, keyfile and prior the table names, and no other input);
2. it extracts every entry bit-exactly: for each line of `files` in `tests/vectors/expected.toml` the path,
   the byte length and the BLAKE3-256 of the extracted bytes are equal, in entry-table order;
3. its `verify` is clean: every frame hash, every block's decoded length, every chunk hash, the index's Merkle
   root and its `entry_table_hash` check out, for a revision 1.1, 1.2, 1.3 or 1.4 vector every record's `original_hash`
   too (spec section 8), the extracted entries match the hashes of `files` in `expected.toml` (`expected-1.1.toml`
   for the revision 1.1 vectors, `expected-1.2.toml` for the revision 1.2 vector, `expected-1.3.toml` for the
   revision 1.3 vector, `expected-1.4.toml` for the revision 1.4 vector), and it reports no error;
4. its `list` output, if it has one in the reference format, is `list` of `expected.toml`; its `verify` summary,
   likewise, is `verify` (`ok: <entries> entries, <chunks> chunks, <blocks> blocks`). Only the facts matter (entry
   count, chunk count, block count); the wording of a different tool is free;
5. for `journal-3gen.lpk`: it reads the latest generation (`latest_generation`, the `files` list), and after a
   rollback to generation 0 and to generation 1 (spec section 15: truncate the file to the end of that
   generation's trailer) it reads exactly `files_at_generation_0` and `files_at_generation_1`; the rollback is
   done on a copy;
6. for `malformed-truncated.lpk` and `malformed-hashflip.lpk` it fails with the error class below and extracts
   nothing from them (the negative cases are gate items, as binding as the positive ones);
7. recovery (spec section 13) is part of the gate: on `recovery-groups.lpk` its recovery check reports no damage;
   on `malformed-recovery-damaged.lpk` it reports exactly one damaged shard, extracts every file that no damaged
   chunk touches, and its repair writes a copy whose bytes equal `recovery-groups.lpk` — a decoder without
   repair may skip the repair clause but must still report the damage. Repair stays optional; the Reed-Solomon code
   it needs is defined in spec section 13 ("The code"), so it can be implemented from the text without any library
   (specification revision of 2026-10-16). This item is about the recovery check:
   `verify` leaves recovery to `check` in every mode (spec section 9) and does not fail on a damaged recovery
   frame;
8. the negative cases on the positive vectors are gate items too: `zstd-dict.lpk` without its prior fails with
   `MissingPrior`; a sealed vector with a wrong password, and `sealed-keyfile.lpk` without its keyfile, fail with
   `WrongKey`; `sealed-listable.lpk` is listed without any password (its `list` of `expected.toml`), while
   extracting from it without the password is `PasswordRequired`.

The plain contents of the vectors are not committed; `expected.toml` gives their hashes. (They are generated
from a xorshift pattern by `tests/common/mod.rs::pattern`; the hashes make that generator unnecessary.)
`expected.toml` is checked against the reference reader by `cargo test -p lpk-format --test conformance`.

### The schema of `expected.toml`

One TOML table per vector, named by the file name in quotes (`["zstd-basic.lpk"]`). The keys:

| Key | Type | Present for | Meaning |
|---|---|---|---|
| `files` | array of strings | every vector that opens | one string per entry, in entry-table order: `<path> <size> <blake3>`, the path as stored, the extracted byte length in decimal and the BLAKE3-256 of the extracted bytes in lowercase hex; a directory has size 0 and the hash of no bytes |
| `list` | string | the same | the stdout of `lpk-decode list` (see below) |
| `verify` | string | the same | the stdout of `lpk-decode verify` |
| `check` | string | `recovery-groups.lpk` | the stdout of `lpk-decode check` |
| `latest_generation` | integer | `journal-3gen.lpk` | the generation of the last trailer |
| `files_at_generation_<g>` | array of strings | `journal-3gen.lpk` | `files` after a rollback to generation `<g>`, for each `<g>` below `latest_generation` |
| `error` | string | `malformed-*` without `recovery` (for `malformed-deflate-bound.lpk`, in `expected-1.2.toml`; for `malformed-bwt-bound.lpk` and `malformed-bwt-inverse.lpk`, in `expected-1.3.toml`; for the ten `malformed-png-filter-*.lpk`, in `expected-1.4.toml`) | the error class |
| `reason` | string | the `malformed-png-filter-*.lpk` tables of `expected-1.4.toml` whose `error` is `BadRecord` | the `reason` of the error (normative, spec section 16) |
| `message` | string | the same | the reference reader's message (informative) |
| `recovery_frames` | integer | `malformed-recovery-damaged.lpk` | recovery frames the index lists |
| `shards_damaged` | integer | the same | damaged data shards `check` reports |
| `shards_repaired_by_repair` | integer | the same | shards `repair` rebuilds |
| `repaired_copy_equals` | string | the same | the vector whose bytes the repaired copy equals |
| `check_stdout`, `check_stderr`, `check_exit` | string, string, integer | the same | the stdout, stderr and exit code of `lpk-decode check` |
| `info` | string | `jpeg-peel.lpk` | the stdout of `lpk-decode info` |
| `error_1_0`, `message_1_0` | string | `jpeg-peel.lpk` | the error class a revision 1.0 reader's `verify` reports (`UnimplementedPrimitive`) and the reference reader's message (informative) |
| `verify_exit_1_0`, `verify_stderr_1_0` | integer, string | `jpeg-peel.lpk` | the exit code and stderr of `lpk-decode verify` (a 1.0 reader) |
| `info`, `error_1_0`, `message_1_0`, `verify_exit_1_0`, `verify_stderr_1_0` | as above | `deflate-peel.lpk`, `malformed-deflate-bound.lpk` | the same keys for every reader before revision 1.2 (`UnimplementedPrimitive`, ID 8) |
| `files_1_0` | array of strings | `deflate-peel.lpk`, `malformed-deflate-bound.lpk` | the lines of `files` (same format) for the entries a reader before revision 1.2 extracts: those with no chunk in a `deflate-reconstruct` block |
| `info`, `error_1_0`, `message_1_0`, `verify_exit_1_0`, `verify_stderr_1_0`, `files_1_0` | as above | `bwt-text.lpk`, `malformed-bwt-bound.lpk`, `malformed-bwt-inverse.lpk` | the same keys for every reader before revision 1.3 (`UnimplementedPrimitive`, ID 3; `files_1_0` the entries with no chunk in a `bwt` block) |
| `info`, `error_1_0`, `message_1_0`, `verify_exit_1_0`, `verify_stderr_1_0`, `files_1_0` | as above | `png-filter.lpk` and the ten `malformed-png-filter-*.lpk` | the same keys for every reader before revision 1.4 (`UnimplementedPrimitive`, ID 9; `files_1_0` the entries with no chunk in a `png-filter` block) |

A decoder needs only `files`, `latest_generation`, `files_at_generation_<g>`, `error` and the recovery counts; the
stdout strings matter only to a decoder that prints the reference formats.

### Output formats of the reference tool

Every line ends with LF. A failing command prints `error: <message>` on stderr and exits 1; a usage error exits 2.

- `list`: one line per entry, `<Kind>\t<size>\t<path>` with `Kind` one of `File`, `Directory`, `Symlink`, control
  characters in the path escaped.
- `verify`: `ok: <entries> entries, <chunks> chunks, <blocks> blocks`; without the key,
  `ok (frame hashes only, chunks not checked without the password): <entries> entries`, where `<entries>` is the
  entry count when the entry table is readable (a listable archive) and `0` when it is sealed.
- `check` and `repair`: `recovery frames: <n>, unusable: <n>, damaged shards: <n>, repaired shards: <n>`; `check`
  exits 1 with `error: damage found: <n> shards damaged, <n> recovery frames unusable` on stderr when it finds
  damage.
- `info`: `key: value` lines; among them `format: <major>.<minor>`, `generation: <g>`, `chain length: <n>` (or
  `chain: error: <message>`), `entries: <n>`, `chunks: <n>`, `blocks: <n>`, `recovery frames: <n>`, `prior: <hex>`
  per prior, and the envelope as `envelope <field>: <value>`. Only the facts matter to another tool.

## The vectors

Graph = the block graph of the data blocks. Recovery = recovery frames present. Generations = more than one.

| Archive | Graph | Recovery | Generations | Needs | What it proves |
|---|---|---|---|---|---|
| `zstd-basic.lpk` | zstd | no | 1 | nothing | one block (window_log 20); chunk table, entry table, Merkle roots |
| `zstd-multiblock.lpk` | zstd | no | 1 | nothing | several blocks (32 KiB), a file whose chunks span two blocks |
| `zstd-dict.lpk` | zstd | no | 1 | prior `zstd-dict.prior` | a zstd step with a dictionary named by BLAKE3 in the index; without the prior the error is `MissingPrior` and the archive does not verify |
| `zstd-window.lpk` | zstd | no | 1 | nothing | a frame that declares window 2^24 (no content size); the envelope's `max_window` |
| `lzma-basic.lpk` | lzma | no | 1 | nothing | raw LZMA1, 8 MiB dictionary, lc 3, lp 0, pb 2 |
| `lzma-multiblock.lpk` | lzma | no | 1 | nothing | LZMA over several blocks, a file across a block boundary |
| `lzma-props.lpk` | lzma | no | 1 | nothing | LZMA with lc 0, lp 2, pb 0 and a 1 MiB dictionary |
| `sealed-aes.lpk` | store | no | 1 | password `correct horse` | an encrypted archive, AES-256-GCM suite, sealed frames, key slot |
| `sealed-xchacha.lpk` | store | no | 1 | password `battery staple` | the XChaCha20-Poly1305 suite |
| `sealed-listable.lpk` | store | no | 1 | password `list me` | an encrypted archive whose entry table is readable without the password (a listing needs none; extraction does) |
| `sealed-keyfile.lpk` | store | no | 1 | password `two factors` and keyfile `sealed-keyfile.key` | a key slot that needs a second factor; a missing keyfile or a wrong password is `WrongKey` |
| `journal-3gen.lpk` | store | no | 3 | nothing | append-only journal: a deleted file, a replaced file, a copy; rollback |
| `recovery-groups.lpk` | store | yes: 3 frames (percent 20, shard_len 1024, group_shards 12) | 1 | nothing | recovery frames over several groups; `check` is clean (see below) |
| `malformed-truncated.lpk` | zstd | no | 1 | nothing | not a valid archive: see below |
| `malformed-hashflip.lpk` | zstd | no | 1 | nothing | not a valid archive: see below |
| `malformed-recovery-damaged.lpk` | store | yes | 1 | nothing | one damaged data shard: `check` reports it, `repair` restores `recovery-groups.lpk` |
| `jpeg-peel.lpk` | store and `jpeg-reconstruct` (revision 1.1, header minor 1) | no | 1 | a revision 1.1 reader | a peeled JPEG: the primary image as a Lepton stream in a block of its own, an MPF secondary image and trailing data as nested chunks in a lower block, one `jpeg` record; see below |
| `deflate-peel.lpk` | zstd, `[lzma, deflate-reconstruct]` and `[zstd, deflate-reconstruct]` (revision 1.2, header minor 2) | no | 1 | a revision 1.2 reader | four peeled Deflate streams (two ZIP members, a PNG's IDAT stream over three spans, a gzip member) as section 9 lays them out: both model steps as non-final steps, one inflated text above `max_block_plain` (the bound of section 8), ZIP, PNG and gzip framing as plain chunks, four `deflate` records; a plain text file every reader extracts; see below |
| `journal-record.lpk` | store and `jpeg-reconstruct` (revision 1.1, header minor 1) | no | 2 | a revision 1.1 reader | generation 0 holds a store-encoded text file; generation 1 is an append that adds a peeled JPEG (as in `jpeg-peel.lpk`): an appended generation's `Records` frame restates the old records (none here) and adds one, and the appended blocks name a record by its position in that frame |
| `bwt-text.lpk` | zstd, `[lzma, bwt]`, `[bwt]`, `[zstd, bwt]` and `[lzma, bwt, deflate-reconstruct]` (revision 1.3, header minor 3) | no | 1 | a revision 1.3 reader | text through the Burrows-Wheeler transform before LZMA or zstd, in BWT blocks of 4096, 16384, 1 and 8192 bytes: blocks exactly `max_block_plain` long whose `lzma` output is longer (the bound I(O, B) of spec section 8, "Bounds"), a short last BWT block, a first-step `bwt` of one byte, a long run and every byte value, and a peeled gzip stream whose inflated text is above `max_block_plain` and transformed (the look-ahead); a plain text file every reader extracts; see below |
| `png-filter.lpk` | zstd, `[lzma, png-filter, deflate-reconstruct]` and `[zstd, png-filter, deflate-reconstruct]` (revision 1.4, header minor 4) | no | 1 | a revision 1.4 reader | four PNG files whose IDAT streams are peeled and whose row filters are re-encoded (spec section 9): RGB 8-bit with filter None and three IDAT chunks, its inflated text above `max_block_plain` (the bound of section 8); RGB 8-bit with all five filter types over two IDAT chunks; grey 2-bit Adam7 with an empty pass, its step input filtered with Paeth instead of Up; RGBA 16-bit with Average and Paeth rows; eight records (`deflate` then `png-filter` per image); a plain text file every reader extracts; see below |

The passwords and the keyfile are also in `tests/vectors/vectors.toml` (test values, not secrets). The four sealed
vectors (`sealed-*`) decode to the same three files.

Recovery expectations (`expected.toml`): for `recovery-groups.lpk` `check` prints
`recovery frames: 3, unusable: 0, damaged shards: 0, repaired shards: 0` and exits 0. For
`malformed-recovery-damaged.lpk` (one bit flipped in the middle of the first block's frame) the archive opens, `check`
reports 1 damaged shard and exits 1, and `repair` writes a copy that is byte-identical to `recovery-groups.lpk`
(1 shard repaired). The other vectors carry no recovery frames, so `check` finds nothing to scan on them.

Apart from `jpeg-peel.lpk`, `journal-record.lpk`, `deflate-peel.lpk`, `bwt-text.lpk` and `png-filter.lpk` (and their malformed companions), the vectors do not exercise the
primitives 3 to 12; a decoder that meets one it does not run reports `UnimplementedPrimitive` and does not guess.

`jpeg-peel.lpk` and `journal-record.lpk` (revision 1.1). Which expectations apply depends on the decoder's
revision (`expected.toml` has a 1.0 table for `jpeg-peel.lpk` only; `journal-record.lpk` is measured through
`expected-1.1.toml` alone, one table per vector):

- A revision 1.0 decoder reads the `["jpeg-peel.lpk"]` table of `expected.toml`. It MUST open the vector, list it
  (key `list`), print `info` (key `info`: `format: 1.1`, `records: true`), and report `UnimplementedPrimitive`
  with the ID 7 from `verify` and from extracting `photo.jpg`: the class is the key `error_1_0` (not `error`,
  which the table does not have, because the vector is valid), the message `message_1_0`, and the reference
  tool's `verify` exit code and stderr `verify_exit_1_0` and `verify_stderr_1_0`. The vector does not exercise
  whether a failed extraction leaves a partial file (that is the extraction policy of spec section 9); no key
  checks it.
- A revision 1.1 decoder reads `expected-1.1.toml` for what differs from a revision 1.0 reader, the keys `files`
  and `verify`, which is all that file holds; the keys `list` and `info` of `expected.toml` apply to it as well
  (they are facts of the archive, the same for every revision). It MUST extract `photo.jpg` to the bytes whose path,
  size and BLAKE3-256 are the key `files` (in entry-table order) and `verify` the vector (key `verify`, the stdout
  of the reference full reader's `verify`, exit code 0); the primary image is the Lepton stream's decoding, and the
  record's `original_hash` is checked over the assembled file (spec section 8). `expected-1.1.toml` is checked by
  `cargo test -p lpk-core --test jpeg_vector`.

`deflate-peel.lpk` (revision 1.2). For this vector the `_1_0` keys of `expected.toml` describe every reader before
revision 1.2, a revision 1.1 reader included (it does not run `deflate-reconstruct` either):

- A reader before revision 1.2 reads the `["deflate-peel.lpk"]` table of `expected.toml`. It MUST open the vector,
  list it (key `list`), print `info` (key `info`: `format: 1.2`, `records: true`), verify its frames and records,
  and report `UnimplementedPrimitive` with the ID 8 from `verify` (keys `error_1_0`, `message_1_0`,
  `verify_exit_1_0`, `verify_stderr_1_0`) and from extracting any entry with a chunk in a `deflate-reconstruct`
  block; it MUST extract the entries of the key `files_1_0` (`readme.txt`) bit-exactly.
- A revision 1.2 decoder reads `expected-1.2.toml` (keys `files` and `verify`); `list` and `info` of
  `expected.toml` apply to it as well. It MUST extract every entry to the bytes of `files` and `verify` the vector
  (exit code 0): each Deflate stream is recreated from its plain text, the output of the block's `lzma` or `zstd`
  step, whose bound is the record's `plain_len` (spec section 8, "Bounds"), and checked against the record's
  `original_len` and `original_hash`. `expected-1.2.toml` is checked by
  `cargo test -p lpk-core --test deflate_vector`.

To regenerate the vector: `cargo test -p lpk-core --test deflate_vector -- --ignored regenerate`, then
`cargo test -p lpk-format --test conformance -- --ignored`; byte identity needs the same liblzma, libzstd,
`preflate-rs` and `bitcode` versions and `lpk-codecs-vp8`. Its inputs are committed fixtures (spec section 11).

`bwt-text.lpk` (revision 1.3). For this vector and its two malformed companions the `_1_0` keys of `expected.toml`
describe every reader before revision 1.3 (a revision 1.1 or 1.2 reader included: neither runs `bwt`):

- A reader before revision 1.3 reads the `["bwt-text.lpk"]` table of `expected.toml`. It MUST open the vector,
  list it (key `list`), print `info` (key `info`: `format: 1.3`, `records: true`), verify its frames and records,
  and report `UnimplementedPrimitive` with the ID 3 from `verify` (keys `error_1_0`, `message_1_0`,
  `verify_exit_1_0`, `verify_stderr_1_0`) and from extracting any entry with a chunk in a `bwt` block; it MUST
  extract the entries of the key `files_1_0` (`readme.txt`) bit-exactly.
- A revision 1.3 decoder reads `expected-1.3.toml` (keys `files` and `verify`); `list` and `info` of
  `expected.toml` apply to it as well. It MUST extract every entry to the bytes of `files` and `verify` the vector
  (exit code 0): each `bwt` step's input is inverted block by block as spec section 8 ("Decoding `bwt`") defines
  it, with every index checked; the `lzma` or `zstd` step before it is bounded by I(O, B) (spec section 8,
  "Bounds"), and in the `[lzma, bwt, deflate-reconstruct]` block O is the record's `plain_len` (the look-ahead).
  `expected-1.3.toml` is checked by `cargo test -p lpk-core --test bwt_vector`.

To regenerate the vector and its malformed companions: `cargo test -p lpk-core --test bwt_vector -- --ignored
regenerate`, then `cargo test -p lpk-format --test conformance -- --ignored`; byte identity needs the same
liblzma, libzstd, `preflate-rs` and `bitcode` versions and `lpk-codecs-vp8` (the transform is unique by definition). Its
contents are generated by the test, except `notes.txt.gz`, a committed fixture of the Deflate vector.

`png-filter.lpk` (revision 1.4). For this vector and its malformed companions the `_1_0` keys of
`expected.toml` describe every reader before revision 1.4 (a revision 1.1, 1.2 or 1.3 reader included: none runs
`png-filter`):

- A reader before revision 1.4 reads the `["png-filter.lpk"]` table of `expected.toml`. It MUST open the vector,
  list it (key `list`), print `info` (key `info`: `format: 1.4`, `records: true`), verify its frames and records,
  and report `UnimplementedPrimitive` with the ID 9 from `verify` (keys `error_1_0`, `message_1_0`,
  `verify_exit_1_0`, `verify_stderr_1_0`) and from extracting any entry with a chunk in a `png-filter` block (the
  first unrun primitive in step order: `png-filter` comes before `deflate-reconstruct`); it MUST extract the
  entries of the key `files_1_0` (`readme.txt`) bit-exactly.
- A revision 1.4 decoder reads `expected-1.4.toml` (keys `files` and `verify`); `list` and `info` of
  `expected.toml` apply to it as well. It MUST extract every entry to the bytes of `files` and `verify` the vector
  (exit code 0): each `png-filter` step undoes its input's own filter bytes and filters the rows again with the
  record's, as spec section 8 ("Decoding `png-filter`") defines it; the `lzma` or `zstd` step before it is bounded
  by the image's filtered length, learnt before that step runs from the `deflate` record after it (spec section 8,
  "Bounds": `image.png`'s inflated text is above `max_block_plain`). `expected-1.4.toml` is checked by
  `cargo test -p lpk-core --test png_filter_vector`.

To regenerate the vector and its malformed companions: `cargo test -p lpk-core --test png_filter_vector --
--ignored regenerate`, then `cargo test -p lpk-format --test conformance -- --ignored`; byte identity needs the
same zlib (`flate2`), liblzma, libzstd, `preflate-rs`, `bitcode` and `lpk-codecs-vp8` versions. `image.png` is a committed
fixture of the Deflate vector; the other contents are generated by the test.

## Malformed vectors

The first two are derived from `zstd-basic.lpk` (the third from the recovery vector, see above; the others are built by lpk-core, see their rows) by `tests/conformance.rs` (the test checks the committed bytes against
the derivation, so they cannot drift).

| Archive | Derivation | Required outcome |
|---|---|---|
| `malformed-truncated.lpk` | the last 100 bytes removed | open fails: `Truncated` ("input truncated in trailer"); nothing is extracted |
| `malformed-deflate-bound.lpk` | built by lpk-core's `tests/deflate_vector.rs` (revision 1.2, header minor 2): `notes.txt.gz` with its Deflate stream as `[lzma, deflate-reconstruct]` whose record's `plain_len` is 10000 below the inflated text, and `readme.txt` | a revision 1.2 reader: open succeeds; `verify`, and extracting `notes.txt.gz`, fail with `PayloadTooLarge` (the `lzma` step crosses its bound, the record's `plain_len`, spec section 8 "Bounds" and lzma rule 6; keys `error` and `message` of its table in `expected-1.2.toml`; only the class is normative; the reference reports `max` = the record's `plain_len`, and the `len` it reports depends on how far its decoder got); `readme.txt` extracts. A reader before revision 1.2: the `_1_0` keys of its `expected.toml` table, as for `deflate-peel.lpk` (`UnimplementedPrimitive`, ID 8; `files_1_0` is `readme.txt`) |
| `malformed-hashflip.lpk` | one bit flipped in the middle of the first block's frame | open succeeds (the damage is in the body); `verify`, and extracting any file of that block, fails with `HashMismatch` of kind 2 ("payload hash mismatch in frame kind 2", the `ChunkData` frame), never `ChunkMismatch` (spec section 9) |
| `malformed-bwt-bound.lpk` | built by lpk-core's `tests/bwt_vector.rs` (revision 1.3, header minor 3): the first block of `prose.txt` as `[lzma, bwt {4096}]` with 64 junk bytes appended to the `bwt` step's input before the `lzma` step encodes it, and `readme.txt` | a revision 1.3 reader: open succeeds; `verify`, and extracting `prose.txt`, fail with `PayloadTooLarge` (the `lzma` step's output crosses its bound I(O, B), spec section 8 "Bounds" and lzma rule 6, before the `bwt` step runs; keys `error` and `message` of its table in `expected-1.3.toml`; only the class is normative; the reference reports `max` = I(O, B) = 16904); `readme.txt` extracts. A reader before revision 1.3: the `_1_0` keys of its `expected.toml` table, as for `bwt-text.lpk` (`UnimplementedPrimitive`, ID 3; `files_1_0` is `readme.txt`) |
| `malformed-bwt-inverse.lpk` | built by the same test: the first block of `prose.txt` as `[lzma, bwt {16384}]`, one BWT block of 32 indices whose second index (index[1]) is replaced by the third (in range), and `readme.txt` | a revision 1.3 reader: open succeeds; `verify`, and extracting `prose.txt`, fail with `BwtError` `inverse` (check (5) of spec section 8 "Decoding `bwt`": the chain from index[1] does not end on index[2]; a decoder that walked from the primary index alone would produce the text and does not conform); `readme.txt` extracts. A reader before revision 1.3: the `_1_0` keys, as above |
| `malformed-png-filter-bound.lpk` | built by lpk-core's `tests/png_filter_vector.rs` (revision 1.4, header minor 4): `sprite.png` as `[lzma, png-filter, deflate-reconstruct]` whose `deflate` record's `plain_len` is one below the image's filtered length, and `readme.txt` | a revision 1.4 reader: open succeeds; `verify`, and extracting `sprite.png`, fail with `PayloadTooLarge` (check (4) of "Decoding `png-filter`" against the bound the look-ahead gives, before the `lzma` step runs; spec section 8 "Bounds"; keys `error` and `message` of its table in `expected-1.4.toml`; the reference reports `len` = the filtered length, 3630, and `max` = the record's `plain_len`, 3629); `readme.txt` extracts. A reader before revision 1.4: the `_1_0` keys of its `expected.toml` table, as for `png-filter.lpk` (`UnimplementedPrimitive`, ID 9; `files_1_0` is `readme.txt`) |
| `malformed-png-filter-width.lpk`, `-height`, `-bit-depth`, `-filters` | built by the same test: `sprite.png`'s `png-filter` record with `width` 0; `height` 0 (and no filter byte, so the record parses); `bit_depth` 4 with colour type 2; its fourth filter byte 5 | a revision 1.4 reader: `verify`, and extracting `sprite.png`, fail with `BadRecord` and the reason `width`, `height`, `bit_depth` or `filters` (check (3), before the `lzma` step runs; keys `error` and `reason`); `readme.txt` extracts. A reader before revision 1.4: the `_1_0` keys, as above |
| `malformed-png-filter-scanlines.lpk` | built by the same test: `sprite.png`'s step input one byte short (the `lzma` step encodes the Up text less its last byte) | a revision 1.4 reader: `BadRecord` `scanlines` (check (5)); `readme.txt` extracts. A reader before revision 1.4: the `_1_0` keys, as above |
| `malformed-png-filter-hash.lpk` | built by the same test: `sprite.png`'s `png-filter` record with one bit of `original_hash` flipped | a revision 1.4 reader: `BadRecord` `original_hash` (check (7)); `readme.txt` extracts. A reader before revision 1.4: the `_1_0` keys, as above |
| `malformed-png-filter-lookahead.lpk` | built by the same test: `notes.txt` (100 bytes) as `[lzma, png-filter, bwt {4096}]` whose `png-filter` record is `sprite.png`'s (filtered length 3630) and whose `lzma` step encodes `sprite.png`'s Up text; `filler.bin` (5000 bytes, plain) makes `max_block_plain` larger than 3630; and `readme.txt` | a revision 1.4 reader: `verify`, and extracting `notes.txt`, fail with `PayloadTooLarge` before the `lzma` step runs: the `png-filter` step's output bound is the input bound the `bwt` step after it gives, I(100, 4096) = 208 (spec section 8, "Bounds", revision 1.4, and the look-ahead), not `max_block_plain`; a decoder that bounded it by `max_block_plain` would run the steps and report `BwtError` `length` instead, and does not conform; `filler.bin` and `readme.txt` extract. A reader before revision 1.4: the `_1_0` keys (`UnimplementedPrimitive`, ID 9, the first unrun primitive in step order; `files_1_0` is `filler.bin` and `readme.txt`) |
| `malformed-png-filter-kind.lpk` | built by the same test: `sprite.png` as in `png-filter.lpk`, its `png-filter` step naming the `deflate` record (record 0) instead of its `png-filter` record | a revision 1.4 reader: `BadRecord` `kind` (check (2), before the `lzma` step runs, after the `deflate-reconstruct` step's own checks passed); `readme.txt` extracts. A reader before revision 1.4: the `_1_0` keys, as above |
| `malformed-png-filter-input-filter.lpk` | built by the same test: `sprite.png`'s step input with its first scanline's filter byte set to 7 before the `lzma` step encodes it | a revision 1.4 reader: `BadRecord` `scanlines` (check (6)): an input filter byte above 4 is never read as None or any other type; `readme.txt` extracts. A reader before revision 1.4: the `_1_0` keys, as above |

A decoder may name the errors differently; the class matters: a cut-off file is reported as truncated, a changed
byte as a hash mismatch, and never as success or as a crash.

Old generations are a gate item too: reading `journal-3gen.lpk` after a rollback (item 5) proves that a decoder opens
an earlier generation exactly as it opens a one-shot archive.

## Priors, passwords, keys

- Prior: `zstd-dict.prior` (8192 bytes) for `zstd-dict.lpk`. Its identity is its BLAKE3-256, which the archive's index
  lists; a decoder matches by hash, not by file name.
- Passwords and the keyfile are used as raw bytes (no normalisation); the keyfile is `sealed-keyfile.key`, 200 bytes.
- Argon2id parameters are in each key slot; the vectors use t 1, m 8192 KiB, p 1.

## Fuzz and robustness

The reference reader is fuzzed (`fuzz/`); an independent decoder is expected to refuse the malformed vectors
without panicking, hanging or allocating beyond what the envelope declares.
