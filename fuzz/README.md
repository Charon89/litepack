# Fuzzing the readers and the peel parsers

One libFuzzer target per reader entry point of `crates/lpk-format`, per peel parser of `crates/lpk-core` and per foreign reader. Each asserts that nothing
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
| `worker_proto` | E4-2's broker over arbitrary worker output (`lpk_core::foreign::replay`) and detection; a counting allocator bounds the listing |
| `foreign_zip`, `foreign_7z`, `foreign_tar` | E4-3's pure-Rust readers (`lpk-foreign`): open, every listing page, the first 256 file members to a sink (`src/lib.rs`, `foreign`); seeds: the readers' fixtures (`workers/lpk-foreign/tests/fixtures`, read by `run.sh`) |
| `jpeg_reconstruct` | revision 1.1's `jpeg-reconstruct` step (`lpk_core::JpegDecoder::decode_in`) over an arbitrary Lepton stream and record (version, nested trailing chunks and a secondary image, a short bound, starved memory), one decode thread; a counting allocator bounds the allocations by the decode memory; seeds: the streams `JpegPeel::peel` writes for `crates/lpk-core/tests/fixtures/*.jpg` |
| `jpeg_peel` | the JPEG peel's compress side (`JpegPeel::peel`: marker scan, memory gate, Lepton encode, verifying decode, secondary images); a plan covers the file exactly; a counting allocator bounds the allocations by the peel's memory limit; seeds: the JPEG fixtures with a flag byte |
| `deflate_analyse` | the Deflate peel's analysis (`lpk_core::peel::deflate::examine`: walker, preflate analysis, bounded recreation) with a 1 MiB plain-text limit; every candidate counted once, no caught panic, spans inside the file; seeds: `crates/lpk-core/tests/fixtures/deflate` with an extension byte (as `deflate_walk`'s) |
| `png_filter_undo` | revision 1.4's PNG filter undo on the compress side (`png_candidate` of the Deflate peel: IHDR, layout, filter bytes, the Up transcode checked by re-filtering); `png_filter_decode` covers the decoder's side only; a counting allocator bounds the allocations by five times the text; seeds: the inflated IDAT text of the PNG fixtures with their headers |

`foreign_libarchive` (E4-4's libarchive reader) is not here yet: `crates/lpk-libarchive-sys` is not on `main`;
it lands with E4-4, built with clang and `-fsanitize=fuzzer-no-link,address` (`docs/notes/e4-design.md`, section 5).

A target added here is added to `Cargo.toml`'s `[[bin]]` set and to `run.sh`'s list `all` too;
`bash fuzz/check-targets.sh` (run by the workflow) fails when the three differ.

Seeds: `seeds/<target>/` (real payloads cut from the vectors by `cargo test -p lpk-format --test gen_vectors --
--ignored regenerate_fuzz_seeds`, plus a few hand-made minimal inputs; `archive_mutate` has one unmutated
1-byte seed per vector; E4-6's `jpeg_*`, `deflate_*` and `png_filter_undo` seeds were cut from `lpk-core`'s fixtures
once, by a scratch program calling `JpegPeel::peel`, `examine` and `png::Header::parse`; a crash input joins its
target's seeds with its fix); full logs of a run: `artifacts/logs/<target>.log`; the committed vectors under
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

Every build passes `--no-cfg-fuzzing` (`run.sh`, the workflow's `cargo fuzz build`), so the targets run
the code we ship. With `cfg(fuzzing)` set (cargo-fuzz's default), dependencies relax checks: bitcode
accepts trailing bytes after a record (`expect_eof`) and miniz_oxide skips the zlib Adler-32 check, so
a record the shipped decoder refuses reached code no shipped build reaches (fuzz run 40270390275,
`deflate_recreate`). cargo-fuzz's build keeps debug assertions and overflow checks on (the crashes of
E4-6's runs were overflow panics), as the workspace's debug tests do; a shipped release build wraps
instead.

A crash is saved under `fuzz/artifacts/<target>/`. Minimise it with
`cargo +nightly fuzz tmin --no-cfg-fuzzing <target> <file>`, add the bytes as a regression test in `lpk-format`, fix the
bug in its own commit.

## CI

`.github/workflows/fuzz.yml` (E4-6, kept running by E8-1): nightly at 03:17 UTC and by hand
(`workflow_dispatch`, input `seconds` per target, 1..3600). A `targets` job checks the target lists
(`check-targets.sh`) and makes the matrix; a `build` job compiles every target once (`cargo fuzz build`) and
passes the binaries on; one `fuzz` job per target runs `run.sh` on them (`LPK_FUZZ_BIN_DIR`) for 600 s, or
3,600 s on Mondays (PLAN E4 decision (e), a budget the owner may change). Each target's corpus persists in the
Actions cache (`fuzz-corpus-<target>-<run>`, restored by prefix; unused entries expire after 7 days) and is
minimised (`run.sh -m`, libFuzzer's `-merge=1`) by the scheduled Monday run. A crash fails the job and
uploads `fuzz/artifacts` (the input and the log) as `fuzz-artifacts-<target>`.

Open findings that fail a target until their fix lands are listed in `KNOWN.md` with their SUMMARY kind and signature; the workflow labels a failing job known only when `classify-failure.sh` matches both, else new.
