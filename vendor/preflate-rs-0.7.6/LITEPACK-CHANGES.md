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
   end-of-chunk flag) and returns an error (`ExitCode::InvalidDeflate`) as soon as the output,
   measured after each block, exceeds `limit` bytes. It does not keep the recreated blocks (the
   upstream function returns them to an internal caller only). A failure to start the CABAC
   reader is an error (`ExitCode::InvalidPredictionData`) instead of a panic.
   To measure the output, `DeflateWriter` (`src/deflate/deflate_writer.rs`) gains
   `pub fn output_len(&self) -> usize`.
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
