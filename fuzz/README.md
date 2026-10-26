# Fuzzing lpk-format

One libFuzzer target per reader entry point of `crates/lpk-format`. Each asserts only that nothing
panics, hangs or exhausts memory; every error result is fine. This crate is excluded from the
workspace (`exclude = ["fuzz"]` in the root `Cargo.toml`), so Windows builds, `cargo deny` and CI
are untouched. `cargo-fuzz` is Unix-only: use Linux, or WSL on Windows.

## Targets

| Target | Entry point |
|---|---|
| `frame_read`, `header_read`, `varint_read` | `Frame::read`, `Header::read`, `varint::read` |
| `entry_table`, `chunk_table`, `records_table` | `parse` + iterate + `get` + `validate` |
| `index_parse`, `trailer_read_tail` | `Index::parse`, `Trailer::read_tail` |
| `block_header_and_decode` | `BlockHeader::parse` + `decode_block` (`Registry::v1` and the full reader's decoders, tight `Resources`) |
| `zstd_decode`, `lzma_decode` | the decoders directly, random `expected_len` |
| `recovery_frame`, `key_slot` | `RecoveryFrame::parse` (runs the section 13 field checks), `KeySlot::parse` + `unwrap` |
| `archive_open` | `Archive::open_with` (keyless and with a password) + `verify` + extract every entry |
| `archive_mutate` | structure-aware: byte mutations of a committed vector (every family, including sealed and journal), then open, verify, extract, `check_recovery`, `repair` |
| `deflate_walk`, `deflate_recreate` | the Deflate peel's stream walk; the bounded recreation of D-66 with arbitrary corrections |
| `bwt_decode` | revision 1.3's `bwt` step (`lpk_core::BwtDecoder`) over arbitrary input with a small block size and bounds; a counting allocator asserts nothing is allocated past the output bound plus the memory term |
| `delta_patch_decode` | the delta stage's format-1 patch decoder (`lpk_core::delta::apply`, E2-9) over an arbitrary base and patch; a counting allocator asserts nothing is allocated past the output (no cover table) and nothing at all before an error |
| `bwt_roundtrip` | libsais's forward transform of arbitrary text in arbitrary block sizes, then the layout and the `u32` inverse: the text comes back; one index or byte damaged: an error or other bytes, never a panic |
| `png_filter_decode` | revision 1.4's `png-filter` step (`lpk_core::PngFilterDecoder`) over arbitrary records (width and height below 2^16, any depth, colour type, interlace and filter bytes) and input, optionally with the record's hash made the correct output's; an output is the record's filtered length within the bound and hashes as recorded; a counting allocator bounds the allocations by the output, two rows and the record |

Seeds: `seeds/<target>/` (real payloads cut from the vectors by `cargo test -p lpk-format --test gen_vectors --
--ignored regenerate_fuzz_seeds`, plus a few hand-made minimal inputs; `archive_mutate` has one unmutated
1-byte seed per vector); full logs of a run: `artifacts/logs/<target>.log`; the committed vectors under
`crates/lpk-format/tests/vectors/` are read as an extra seed directory by `run.sh`
(`archive_mutate` embeds them). The corpus that grows lives in `corpus/<target>/` (git-ignored).

## Run locally (Linux or WSL)

```sh
rustup toolchain install nightly          # cargo-fuzz needs nightly
cargo install cargo-fuzz --locked --version 0.13.2
export CARGO_TARGET_DIR=~/lpk-fuzz-target # under WSL: keep the build off /mnt (slow)
bash fuzz/run.sh -t 60                    # every target for 60 s
bash fuzz/run.sh -t 600 archive_mutate    # one target for 10 min
cargo +nightly fuzz run --no-cfg-fuzzing archive_mutate    # by hand, from fuzz/
```

Every build passes `--no-cfg-fuzzing`, so the targets run the code we ship. With `cfg(fuzzing)` set
(cargo-fuzz's default), dependencies relax checks: bitcode accepts trailing bytes after a record
(`expect_eof`) and miniz_oxide skips the zlib Adler-32 check. A record the shipped decoder refuses
then reached code no shipped build reaches (fuzz run 40270390275, `deflate_recreate`). The fuzz
build is a release build without overflow checks, as the shipped binaries are; the workspace's
debug tests have overflow checks, so a crafted value can panic there (caught, `corrections`) where
it wraps in the fuzz and shipped builds: reproduce a fuzz finding with
`RUSTFLAGS="--cfg fuzzing" cargo test --release` only to match the old builds; with this flag, a
plain `cargo test --release` matches.

What the fuzzer gives up for this: code after a successful zlib decode is reached only by inputs
with a valid Adler-32 checksum (and bitcode records only without trailing bytes), so seeds should
carry valid checksums for mutations to get past them. And as the fuzz builds have no overflow
checks, a wrap is found only through what it causes later; an occasional separate run built with
`-a` (`cargo fuzz build -a --no-cfg-fuzzing`, debug assertions and overflow checks) would catch
wraps directly (a suggestion; no CI job runs it).

A crash is saved under `fuzz/artifacts/<target>/`. Minimise it with
`cargo +nightly fuzz tmin --no-cfg-fuzzing <target> <file>`, add the bytes as a regression test in `lpk-format`, fix the
bug in its own commit.

## CI

`.github/workflows/fuzz.yml`: manual (`workflow_dispatch`, input `seconds` per target) and weekly.
It uploads `fuzz/artifacts` when a target crashes.
