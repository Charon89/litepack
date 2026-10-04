# `.lpk` v1 conformance: what an independent decoder must do

For an implementer who has only `docs/spec/lpk-v1.md` and `crates/lpk-format/tests/vectors/`. This is the
checklist an independent decoder is held to before the format is frozen. It states no performance figures.

## What "decodes all vectors" means

An independent decoder passes the gate when, for every archive in the table below:

1. it opens the archive (with the password, keyfile and prior the table names, and no other input);
2. it extracts every entry bit-exactly: for each line of `files` in `tests/vectors/expected.toml` the path,
   the byte length and the BLAKE3-256 of the extracted bytes are equal, in entry-table order;
3. its `verify` is clean: every frame hash, every block's decoded length, every chunk hash and the index's
   Merkle root check out, the extracted entries match the hashes of `files` in `expected.toml`, and it reports
   no error;
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
   repair may skip the repair clause but must still report the damage. This item is about the recovery check:
   `verify` leaves recovery to `check` in every mode (spec section 9) and does not fail on a damaged recovery
   frame. Spec section 13's "`reed-solomon-simd` 3.x" means that library's 3.x series as pinned in the repository's
   `Cargo.lock` when the vectors were written;
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
| `error` | string | `malformed-*` without `recovery` | the error class |
| `message` | string | the same | the reference reader's message (informative) |
| `recovery_frames` | integer | `malformed-recovery-damaged.lpk` | recovery frames the index lists |
| `shards_damaged` | integer | the same | damaged data shards `check` reports |
| `shards_repaired_by_repair` | integer | the same | shards `repair` rebuilds |
| `repaired_copy_equals` | string | the same | the vector whose bytes the repaired copy equals |
| `check_stdout`, `check_stderr`, `check_exit` | string, string, integer | the same | the stdout, stderr and exit code of `lpk-decode check` |

A decoder needs only `files`, `latest_generation`, `files_at_generation_<g>`, `error` and the recovery counts; the
stdout strings matter only to a decoder that prints the reference formats.

### Output formats of the reference tool

Every line ends with LF. A failing command prints `error: <message>` on stderr and exits 1; a usage error exits 2.

- `list`: one line per entry, `<Kind>\t<size>\t<path>` with `Kind` one of `File`, `Directory`, `Symlink`, control
  characters in the path escaped.
- `verify`: `ok: <entries> entries, <chunks> chunks, <blocks> blocks`; without the key,
  `ok (frame hashes only, chunks not checked without the password): <entries> entries`.
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

The passwords and the keyfile are also in `tests/vectors/vectors.toml` (test values, not secrets). The four sealed
vectors (`sealed-*`) decode to the same three files.

Recovery expectations (`expected.toml`): for `recovery-groups.lpk` `check` prints
`recovery frames: 3, unusable: 0, damaged shards: 0, repaired shards: 0` and exits 0. For
`malformed-recovery-damaged.lpk` (one bit flipped in the middle of the first block's frame) the archive opens, `check`
reports 1 damaged shard and exits 1, and `repair` writes a copy that is byte-identical to `recovery-groups.lpk`
(1 shard repaired). The other vectors carry no recovery frames, so `check` finds nothing to scan on them.

The vectors do not exercise the reconstruction primitives (7 to 12), which have no decoder in v1 yet; a decoder that
meets one reports `UnimplementedPrimitive` and does not guess.

## Malformed vectors

The first two are derived from `zstd-basic.lpk` (the third from the recovery vector, see above) by `tests/conformance.rs` (the test checks the committed bytes against
the derivation, so they cannot drift).

| Archive | Derivation | Required outcome |
|---|---|---|
| `malformed-truncated.lpk` | the last 100 bytes removed | open fails: `Truncated` ("input truncated in trailer"); nothing is extracted |
| `malformed-hashflip.lpk` | one bit flipped in the middle of the first block's frame | open succeeds (the damage is in the body); `verify`, and extracting any file of that block, fails with `HashMismatch` of kind 2 ("payload hash mismatch in frame kind 2", the `ChunkData` frame), never `ChunkMismatch` (spec section 9) |

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
