/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/

/*
 *  Copyright (c) 2010 The WebM project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE banner below
 *  An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the VPX_AUTHORS file in this directory
 */
/*
Copyright (c) 2010, Google Inc. All rights reserved.
Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:
Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.
Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the documentation and/or other materials provided with the distribution.
Neither the name of Google nor the names of its contributors may be used to endorse or promote products derived from this software without specific prior written permission.
THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS “AS IS” AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*/

// Modified by LitePack (2026-10-25) from lepton_jpeg 0.5.8, `src/structs/vpx_bool_reader.rs`
// (LIC-2; spec section 8):
// - the metrics, the tracing hash, the model-component argument and `get_grid` removed; errors
//   are `std::io::Error` (lepton_jpeg's error type is not carried);
// - the per-symbol step renamed `decode_bit` (the public `get` is the one-symbol method);
// - `get_n_bits` takes a context slice of any length, decodes bit `i` with context
//   `min(len - 1, i)` and returns `u64` (up to 64 bits);
// - `get_unary_encoded` takes a slice, decodes ones until a zero with context `min(len - 1, i)`
//   (the writer's code always ends with a zero), with no bound on the value; it refills before every symbol instead of every seventh, since `len` may exceed 14.

use std::io::{Error, ErrorKind, Read, Result};

use crate::branch::Branch;

const BITS_IN_BYTE: u32 = 8;
const BITS_IN_VALUE: u32 = 64;
const BITS_IN_VALUE_MINUS_LAST_BYTE: u32 = BITS_IN_VALUE - BITS_IN_BYTE;
const VALUE_MASK: u64 = (1 << BITS_IN_VALUE_MINUS_LAST_BYTE) - 1;

/// The reader of the VP8 bool coder: decodes binary symbols with adaptive [`Branch`] contexts
/// from a stream written by [`VPXBoolWriter`](crate::VPXBoolWriter). Bytes past the end of the
/// stream read as zero.
#[derive(Debug)]
pub struct VPXBoolReader<R> {
    value: u64,
    range: u64, // 128 << BITS_IN_VALUE_MINUS_LAST_BYTE <= range <= 255 << BITS_IN_VALUE_MINUS_LAST_BYTE
    upstream_reader: R,
}

impl<R: Read> VPXBoolReader<R> {
    /// A reader at the start of a stream; fails if the stream's first (marker) symbol is not a
    /// zero.
    pub fn new(reader: R) -> Result<Self> {
        let mut r = VPXBoolReader {
            upstream_reader: reader,
            value: 1 << (BITS_IN_VALUE - 1), // guard bit
            range: 255 << BITS_IN_VALUE_MINUS_LAST_BYTE,
        };

        let mut dummy_branch = Branch::new();
        let bit = r.get_bit(&mut dummy_branch)?; // marker false bit
        if bit {
            return Err(Error::new(ErrorKind::InvalidData, "StreamInconsistent"));
        }

        Ok(r)
    }

    // Lepton uses VP8 adaptive arithmetic coding scheme, where bits are extracted from file stream
    // by "division" of current 8-bit stream `value` by adaptive 8-bit `split`. Adaptation is achieved by
    // combination of predicted probability to get false bit (`1 <= probability <= 255`, in 1/256 units),
    // and `range` that represents maximum possible value of yet-not-decoded stream part (so that
    // `range > value`, `128 <= range <= 256` in units of $2^{-n-8}$ for the `n` bits already consumed)
    // by forming predictor `split = 1 + (((range - 1) * probability) >> BITS_IN_BYTE)`,
    // `1 <= split <= range - 1`. Comparison of predictor with stream gives the next decoded bit:
    // true for `value >= split` and false otherwise - this is effectively division step.
    // After this we shrink `value` and `range` by `split` for true or shrink `range` to `split`
    // for false and update `probability`. Now `range` can get out of allowable range and we restore it
    // by shifting left both `range` and `value` with corresponding filling of `value` by further
    // stream bits (it corresponds to bring down new digit in division, and since `range > value` is invariant
    // of the operations, shifted out `value` bits are guaranteed to be 0). Repeat until stream ends.
    //
    // Reference: https://datatracker.ietf.org/doc/html/rfc6386#section-7.
    //
    // Here some improvements to the basic scheme are implemented. First, we store more stream bits
    // in `value` to reduce refill rate, so that 8 MSBs of `value` represent `value` of the scheme
    // (it was already implemented in DropBox version, however, with shorter 16-bit `value`).
    // Second, `range` and `split` are also stored in 8 MSBs of the same size variables (it is new
    // and it allows to reduce number of operations to compute `split` - previously `big_split` -
    // and to update `range` and `shift`). Third, we use local values for all stream state variables
    // to reduce number of memory load/store operations in decoding of many-bit values. Fourth,
    // we use in `value` a set bit after the stream bits as a guard - completely getting rid
    // of bit counter and not changing comparison result `value >= split`.
    #[inline(always)]
    fn decode_bit(branch: &mut Branch, tmp_value: &mut u64, tmp_range: &mut u64) -> bool {
        let probability = branch.get_probability() as u64;

        let split = mul_prob(*tmp_range, probability);

        let bit = *tmp_value >= split;

        branch.record_and_update_bit(bit);

        if bit {
            *tmp_range -= split;
            *tmp_value -= split;
        } else {
            *tmp_range = split;
        }

        let shift = (*tmp_range).leading_zeros();

        *tmp_value <<= shift;
        *tmp_range <<= shift;

        bit
    }

    /// Decodes a value written unary with `branches`: ones until a zero, symbol `i` with context
    /// `branches[min(branches.len() - 1, i)]`. The value has no bound (the loop ends: past the
    /// end of the stream every symbol reads as zero once the code value is exhausted).
    #[inline(always)]
    pub fn get_unary_encoded(&mut self, branches: &mut [Branch]) -> Result<usize> {
        let a = branches.len();
        if a == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "get_unary_encoded: no context",
            ));
        }

        let mut tmp_value = self.value;
        let mut tmp_range = self.range;

        let mut value = 0usize;
        loop {
            if tmp_value & VALUE_MASK == 0 {
                tmp_value = Self::vpx_reader_fill(tmp_value, &mut self.upstream_reader)?;
            }
            if !Self::decode_bit(
                &mut branches[value.min(a - 1)],
                &mut tmp_value,
                &mut tmp_range,
            ) {
                break;
            }
            value += 1;
        }

        self.value = tmp_value;
        self.range = tmp_range;

        Ok(value)
    }

    /// Decodes a field of `n` bits written by
    /// [`put_n_bits`](crate::VPXBoolWriter::put_n_bits), most significant first, bit `i` with
    /// context `branches[min(branches.len() - 1, i)]`.
    #[inline(always)]
    pub fn get_n_bits(&mut self, n: usize, branches: &mut [Branch]) -> Result<u64> {
        if n == 0 {
            return Ok(0);
        }
        if branches.is_empty() || n > 64 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "get_n_bits: no context or more than 64 bits",
            ));
        }
        let last = branches.len() - 1;

        let mut tmp_value = self.value;
        let mut tmp_range = self.range;

        let mut coef = 0u64;
        for i in (0..n).rev() {
            // Here the fastest way is to use condition of `get_bit`, presumably as
            // this loop cannot be unrolled due to vaiable iterations number.
            // Moreover, this condition holds very rarely as `value` is usually already filled
            // by previous `get_bit` sign reading.
            if tmp_value & VALUE_MASK == 0 {
                tmp_value = Self::vpx_reader_fill(tmp_value, &mut self.upstream_reader)?;
            }

            coef |= (Self::decode_bit(&mut branches[i.min(last)], &mut tmp_value, &mut tmp_range)
                as u64)
                << i;
        }

        self.value = tmp_value;
        self.range = tmp_range;

        Ok(coef)
    }

    /// Decodes one binary symbol with `branch`.
    #[inline(always)]
    pub fn get_bit(&mut self, branch: &mut Branch) -> Result<bool> {
        let mut tmp_value = self.value;
        let mut tmp_range = self.range;

        // We ensure that the guard bit never comes into the first byte,
        // thus having in `value` at least 8 stream bits.
        if tmp_value & VALUE_MASK == 0 {
            tmp_value = Self::vpx_reader_fill(tmp_value, &mut self.upstream_reader)?;
        }

        let bit = Self::decode_bit(branch, &mut tmp_value, &mut tmp_range);

        self.value = tmp_value;
        self.range = tmp_range;

        Ok(bit)
    }

    // Fill `tmp_value` maximally still preserving space for the guard bit,
    // after this returned value has `56 | (63 - shift)` stream bits
    #[inline(always)]
    fn vpx_reader_fill(mut tmp_value: u64, upstream_reader: &mut R) -> Result<u64> {
        // This `if` does not change performance but drops down instructions count by 3 %
        if tmp_value & 0xFF == 0 {
            let mut shift: i32 = tmp_value.trailing_zeros() as i32;
            // Unset the last guard bit and set a new one
            tmp_value &= tmp_value - 1;
            tmp_value |= 1 << (shift & 7);

            // BufReader is already pretty efficient handling small reads, so optimization doesn't help that much
            let mut v = [0u8; 1];
            shift -= 7;

            while shift > 0 {
                let bytes_read = upstream_reader.read(&mut v)?;
                if bytes_read == 0 {
                    break;
                }

                tmp_value |= (v[0] as u64) << shift;
                shift -= 8;
            }
        }

        Ok(tmp_value)
    }
}

fn mul_prob(tmp_range: u64, probability: u64) -> u64 {
    ((((tmp_range - (1 << BITS_IN_VALUE_MINUS_LAST_BYTE)) >> 8) * probability)
        & (0xFF << BITS_IN_VALUE_MINUS_LAST_BYTE))
        + (1 << BITS_IN_VALUE_MINUS_LAST_BYTE)
}
