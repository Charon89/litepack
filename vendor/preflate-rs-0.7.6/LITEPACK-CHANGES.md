# LitePack changes to preflate-rs 0.7.6

This directory is a modified copy of `preflate-rs` 0.7.6 as published on crates.io (upstream
<https://github.com/microsoft/preflate-rs>, commit `3bcd33441849a8a25d2b26077128f4e3a88d071b`,
path `preflate`; Copyright (c) Microsoft Corporation; Apache License 2.0, see `LICENSE.txt`).
LitePack carries it under its decision D-66 and points its workspace at it with
`[patch.crates-io]` in the root `Cargo.toml`. This file states every change, as section 4(b) of
the Apache License 2.0 requires. Each changed place in the code carries a comment
`LitePack change N`.

## Why
LitePack's `deflate-reconstruct` decoder recreates Deflate streams from correction records read
from archives, which may be crafted. Upstream's `recreate_whole_deflate_stream` has no output
limit, loops until a decoded end-of-chunk flag, and takes its predictor parameters from the
record without checking them, so a small record can make it produce output (and hold token
lists) far beyond what the archive declares.

## Changes to the code
1. **An output-limited recreation.** New public function
   `recreate_whole_deflate_stream_limited(plain_text, prediction_corrections, limit)` in
   `src/stream_processor.rs` (exported from `src/lib.rs`). It runs the same steps as
   `recreate_whole_deflate_stream` (decode the `ReconstructionData`, build the `TokenPredictor`,
   recreate block by block, encode each block with `DeflateWriter`, stop at the decoded
   end-of-chunk flag) and returns an error as soon as the output would exceed `limit` bytes
   (change 5; the check after each block is kept as a backstop). It does not keep the recreated
   blocks (the upstream function returns them to an internal caller only). A failure to start
   the CABAC reader is an error (`ExitCode::InvalidPredictionData`) instead of a panic.
   To measure the output, `DeflateWriter` (`src/deflate/deflate_writer.rs`) gains
   `pub fn output_len(&self) -> usize`. The error is a new exit code,
   `ExitCode::OutputLimit` (30, in `src/preflate_error.rs`), which nothing else returns.
2. **A cap on the number of blocks.** The limited recreation refuses to start a block once it has
   produced `limit` blocks. Every block this crate writes takes at least 10 bits (a
   fixed-Huffman block with no tokens: 3 header bits and the 7-bit end-of-block code; a stored
   block of length 0 takes at least 35 bits; a dynamic block more), so a stream of at most
   `limit` bytes has at most `limit` blocks. The cap is a backstop to change 1.
3. **Predictor parameters checked.** `check_decoded_parameters` in
   `src/estimator/preflate_parameter_estimator.rs` refuses (`ExitCode::InvalidParameterHeader`)
   parameters that `estimate_preflate_parameters` cannot produce: the store and Huffman-only
   branch's fixed set; otherwise `window_bits` 9 to 15, `max_token_count` one less than a power
   of two from 2^7 to 2^15, `nice_length` and `matching_type` from the parser tables (or 258 and
   greedy), `AddFirst`/`AddFirstAndLast` limits at most 258, `max_dist_3_matches` at most 32768,
   `max_chain` 1 to 4096, `min_len` 3 to 258 or `u32::MAX`, and the hash algorithm among the
   candidates `estimate_preflate_comp_level` tries for that `min_len` (for `Zlib`, the shift and
   mask pairs it constructs). Only the limited recreation calls it.
4. **Token reservation bounded by the input.** In `TokenPredictor::recreate_block`
   (`src/token_predictor.rs`) the token list reserves `min(token count, input remaining)` entries
   instead of the decoded token count. The loop that fills it pushes at most that many tokens
   (each consumes at least one input byte), so the output is unchanged; the change only stops a
   decoded count from reserving gigabytes. It applies to both recreation functions.
5. **A capped writer.** `DeflateWriter::with_cap(limit)` reserves its buffer once,
   `limit + CAP_SLACK` bytes (`CAP_SLACK` = 8), and `DeflateWriter::encode_block_capped(block,
   limit)` writes the same bytes as `encode_block` but returns `OutputLimit` before any write
   step that could take the output past `limit`: each step (block header, one token, the
   end-of-block code, the last block's padding) appends at most 8 bytes and runs only while the
   output is at most `limit` bytes; the dynamic Huffman table goes to a scratch buffer and the
   stored payload is measured first, both appended only if the output stays within `limit`. So
   the buffer never grows past its reservation, and a valid stream of at most `limit` bytes is
   never refused. The limited recreation uses it. For tests and fuzzing,
   `recreate_whole_deflate_stream_limited_report` (hidden from the docs) also returns the
   buffer's capacity at the end; `DeflateWriter::capacity()` reads it.
6. **Hops bounded by `max_chain`.** In the limited recreation only,
   `TokenPredictor::limit_hops_to_max_chain` makes `recreate_block` refuse
   (`InvalidPredictionData`) a correction that asks for more hops along the hash chain
   (`DistAfterLenCorrection + 1` or `DistOnlyCorrection`) than the parameters' `max_chain`,
   before the hash chain is walked; the `+ 1` is checked in 64 bits first. Upstream walks up to
   the whole window per token for any decoded count. Analysis did not emit more hops than
   `max_chain` on any stream LitePack checked (its corpus test, see its tests).
   `src/stream_processor.rs` also gains a test module, `litepack_crafted`, whose ignored test
   writes the crafted record LitePack's tests use for this change.

7. **The coder from `lpk-codecs-vp8` instead of `cabac`** (LitePack LIC-2, 2026-10-25). Upstream codes
   its corrections with `cabac` 0.15.0 (LGPL-3.0-or-later). This copy depends on LitePack's
   `lpk-codecs-vp8` (`crates/lpk-codecs-vp8`, Apache-2.0: the VP8 bool coder of `lepton_jpeg` 0.5.8
   with the conventions of LitePack's spec section 8) instead: `Cargo.toml` names it in place of
   `cabac`; `src/cabac_codec.rs` bounds its generic encoder and decoder by `lpk_codecs_vp8::Vp8Encode`
   and `Vp8Decode` instead of `cabac`'s two traits, and its tests use `VPXBoolWriter`,
   `VPXBoolReader` and `Branch`; `src/stream_processor.rs` uses `VPXBoolWriter` and `VPXBoolReader`.
   The test-only helpers that used `cabac`'s debugging coder (`roundtree_cabac_correction`,
   `decompress_deflate_stream_assert`, `recompress_deflate_stream_assert`,
   `analyze_compressed_data_verify`) use the VP8 coder instead, so they still round-trip but no
   longer check that no context is mixed up. The bytes are unchanged: `lpk-codecs-vp8`'s replay of
   5,415 oracle streams (13 captured from this crate's analysis) and LitePack's vectors show the same
   corrections as with `cabac`. The coder decodes a stream's first (marker) symbol and discards
   its value, as `cabac`'s decoder does, so corrections whose first byte is 0x80 or more decode as
   upstream's.
8. **Correction values bounded to `u32`** (LitePack LIC-2, 2026-10-25), so that no crafted record
   gives different outcomes in debug and release builds. In `src/cabac_codec.rs`, `read_exp_value`
   decodes the unary exponent with `get_unary_encoded_max(context, 32)`: an exponent above 32 is
   refused at its 33rd one (upstream overflows `1 << (bits_found - 1)` on `u32`, which panics in a
   debug build and wraps in a release build); and `decode_correction_slow` adds the `+ 1` to a
   decoded value with `checked_add`, refusing a decoded 2^32 - 1 (upstream overflows there too).
   Both refusals are panics, which LitePack's callers catch and report as refused corrections.
   Corrections are `u32` values, so a valid record has exponents of at most 32 and decoded values
   below 2^32 - 1: no valid record is affected. For tests, `src/stream_processor.rs` gains the
   hidden function `litepack_record_with_coded_corrections(record, coded)`, exported from
   `src/lib.rs`: the record with its coded correction stream replaced.
9. **Prediction differences decoded with wrapping arithmetic** (LitePack E4-6, 2026-10-26). In
   `src/cabac_codec.rs` (this crate's own file; the coder it drives is unchanged),
   `decode_difference` subtracts and adds with `wrapping_sub`/`wrapping_add`: upstream's `-`
   and `+` overflow on a crafted correction, which panics in a build with overflow checks (the
   fuzz build) and wraps in a release build. The release result is unchanged, so no outcome
   differs from before in a release build and no valid record is affected (a valid record never
   overflows: the encoder computed the difference from two `u32` values). Found by LitePack's
   `deflate_recreate` fuzz target; the test `test_encode_decode_difference` checks the wrap.
10. **A decoded code length above 15 refused** (LitePack E4-6, 2026-10-26). In
   `src/tree_predictor.rs` (this crate's own file; the coder it drives is unchanged),
   `reconstruct_ld_trees` returns `ExitCode::RecompressFailed` when a decoded code-length code
   (`TreeCodeType::Code`) is above 15: upstream kept it, and `calc_codetree_freq` then indexed
   past its 19 frequencies (a panic). Deflate's code lengths are 0 to 15, so no valid record is
   affected. Found by LitePack's `deflate_recreate` fuzz target; its input is in
   `fuzz/seeds/deflate_recreate/`.
11. **A decoded repeat count past the remaining symbols refused** (LitePack E4-6, 2026-10-26). In
   `src/tree_predictor.rs` (this crate's own file; the coder it drives is unchanged),
   `reconstruct_ld_trees` takes the rest of the symbols with `get(n..)` and returns
   `ExitCode::RecompressFailed` when a decoded repeat count runs past them: upstream sliced
   `&symbols[n..]`, which panics out of range. A valid record's repeat counts stay within the
   symbols (the encoder derived them from them), so no valid record is affected. Found by the
   review of LitePack's E4-6.
12. **Tree counts bounded before they size a vector** (LitePack, 2026-10-26; found by the fuzz
   target `deflate_recreate`, run 40270390275). In `recreate_tree_for_block`
   (`src/tree_predictor.rs`) the decoded literal count, distance count and code-length count are
   checked against the ranges a Deflate header can carry (257 to 288, 1 to 32, 4 to 19, as the
   reader in `src/deflate/huffman_encoding.rs` reads HLIT, HDIST and HCLEN) and refused
   (`ExitCode::InvalidPredictionData`) otherwise, before the code-length vectors are resized.
   Upstream resized them to the decoded value: a crafted correction (one that wraps below zero,
   change 9, or simply adds 2^26) made a single `resize` allocate up to 4 GiB, past the limited
   recreation's `limit + 8` writer. Every count the analysis records comes from a header the
   reader accepted, so no valid record is affected. It applies to both recreation functions.
   `src/stream_processor.rs`'s `litepack_crafted` module gains the ignored test
   `write_crafted_counts_record`, which writes the crafted record of `lpk-core`'s test
   `tests/deflate_recreate_fuzz.rs` (with a counting allocator).
13. **Decoded table counts kept to what a Deflate header holds** (LitePack E4-6, 2026-10-26). Its
   `src/tree_predictor.rs` part (the code-length count outside 4 to 19 in
   `recreate_tree_for_block`) is covered by change 12, which checks before the resize; what
   remains of change 13 is in `src/deflate/huffman_encoding.rs`: `HuffmanOriginalEncoding::write` refuses a literal
   count outside 257 to 288, a distance count outside 1 to 32 or a code-length count outside 4
   to 19 (the subtractions that write HLIT, HDIST and HCLEN overflowed). These are the ranges
   the header's fields can hold, so no valid record is affected. Both files are this crate's
   own; the coder is unchanged. Found by the regression test below.
14. **Values kept to their fields' widths** (LitePack E4-6, 2026-10-26). In
   `src/deflate/huffman_encoding.rs`, `HuffmanOriginalEncoding::write` also refuses
   (`ExitCode::RecompressFailed`) a code-length code above 7 (written in 3 bits), a code length
   above 15, and a repeat count outside its code's range (3 to 6, 3 to 10, 11 to 138): a wider
   value spilled into the next field (`BitWriter::write`'s debug assertion; in a release build,
   other bytes). A valid header holds only such values, so no valid record is affected. Found
   by LitePack's `deflate_recreate` fuzz target (fuzz run 40272877553); its input is in
   `fuzz/seeds/deflate_recreate/`.

The workspace runs the regression test of changes 9 to 11, 13 and 14 (the vendored crate's own tests are not
run): `lpk-core`'s `crafted_corrections_do_not_panic` (`src/peel/deflate/decode.rs`) feeds the
`deflate_recreate` seed and a deterministic sweep of crafted correction streams to
`recreate_whole_deflate_stream_limited_report` and fails on any overflow or out-of-range panic.
Change 12's test is `lpk-core`'s `tests/deflate_recreate_fuzz.rs`.

Nothing else in the code changed: `recreate_whole_deflate_stream`, the analysis, the correction
format and every output for a valid record are upstream's.

## Changes to the files of the package
- `.cargo-ok` (a cargo extraction marker) removed.
- Upstream's `CLAUDE.md` (instructions for an AI coding assistant working in upstream's
  repository) renamed to `UPSTREAM-CLAUDE.md.txt`, so it is kept but not read as instructions
  for this repository.
- `LICENSE.txt` added: the Apache License 2.0 text that the source headers refer to (the
  published package does not include it). The package includes no `NOTICE.txt`.
- This file added.
- `Cargo.lock` (upstream's lockfile, unused: the LitePack workspace and `fuzz/` have their own)
  removed, since it listed `cabac` after change 7.
