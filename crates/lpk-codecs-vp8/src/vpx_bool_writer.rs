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

// Modified by LitePack (2026-10-25) from lepton_jpeg 0.5.8, `src/structs/vpx_bool_writer.rs`
// (LIC-2; spec section 8):
// - the metrics, the tracing hash, the model-component argument and `put_grid` removed;
//   `drain_stats` and `flush_non_final_data` removed (the whole stream is buffered);
// - the per-symbol step renamed `code_bit` (the public `put` is the one-symbol method);
// - `put_n_bits` takes the value as `u64` and a context slice of any length, coding bit `i` with
//   context `min(len - 1, i)`; `put_unary_encoded` takes a slice and checks `v <= len`
//   the unary code always ends with a zero, coded with context `min(len - 1, i)` (so a value equal
//   to the number of contexts takes `len + 1` symbols; lepton_jpeg's stops after `len` ones);
// - `finish` implements the stream tail of spec section 8 instead of the minimal flush;
// - `carry` returns instead of asserting when it runs past the first byte.

use std::io::{Result, Write};

use crate::branch::Branch;

/// The writer of the VP8 bool coder: codes binary symbols with adaptive [`Branch`] contexts and
/// writes the stream to `W` on [`finish`](VPXBoolWriter::finish).
#[derive(Debug)]
pub struct VPXBoolWriter<W> {
    low_value: u64,
    range: u32,
    writer: W,
    buffer: Vec<u8>,
}

impl<W: Write> VPXBoolWriter<W> {
    /// A writer at the start of a stream.
    pub fn new(writer: W) -> Result<Self> {
        let mut retval = VPXBoolWriter {
            low_value: 1 << 9, // this divider bit keeps track of stream bits number
            range: 255,
            buffer: Vec::new(),
            writer,
        };

        let mut dummy_branch = Branch::new();
        // initial false bit is put to not get carry out of stream bits
        retval.put_bit(false, &mut dummy_branch)?;

        Ok(retval)
    }

    #[inline(always)]
    fn code_bit(
        &mut self,
        bit: bool,
        branch: &mut Branch,
        mut tmp_value: u64,
        mut tmp_range: u32,
    ) -> (u64, u32) {
        let probability = branch.get_probability() as u32;

        let split = 1 + (((tmp_range - 1) * probability) >> 8);

        branch.record_and_update_bit(bit);

        if bit {
            tmp_value += split as u64;
            tmp_range -= split;
        } else {
            tmp_range = split;
        }

        let shift = (tmp_range as u8).leading_zeros();

        tmp_range <<= shift;
        tmp_value <<= shift;

        // check whether we cannot put next bit into stream
        if tmp_value & (u64::MAX << 57) != 0 {
            // calculate the number odd bits left over after we remove:
            // - 48 bits (6 bytes) flushed to buffer
            // - 8 bits need to keep for coding accuracy (since probability resolution is 8 bits)
            // - 1 bit for marker
            // - 1 bit for overflow
            //
            // leftover_bits will always be <= 8
            let leftover_bits = tmp_value.leading_zeros() + 2;

            // shift align so that the top 6 bytes are ones we want to write, if there
            // was an overflow it gets rotated down to the bottom bit
            let v_aligned = tmp_value.rotate_left(leftover_bits);

            if (v_aligned & 1) != 0 {
                self.carry();
            }

            // Append the top six bytes of the u64 into buffer in big endian so that the top byte goes first.
            let b = v_aligned.to_be_bytes();
            self.buffer.extend_from_slice(&b[0..6]);

            // mask the remaining bits (between 8 and 16) and put them back to where they were
            // adding the marker bit to the top
            tmp_value = ((v_aligned & 0xffff) | 0x20000/*marker bit*/) >> leftover_bits;
        }

        (tmp_value, tmp_range)
    }

    /// Safe as: at the stream beginning initially put `false` ensure that carry cannot get out
    /// of the first stream byte - then `carry` cannot be invoked on empty `buffer`,
    /// and after the stream beginning `flush_non_final_data` keeps carry-terminating
    /// byte sequence (one non-255-byte before any number of 255-bytes) inside the `buffer`.
    ///
    /// Cold to keep this out of the inner loop since carries are pretty rare
    #[cold]
    #[inline(never)]
    fn carry(&mut self) {
        let mut x = self.buffer.len();

        while x > 0 {
            x -= 1;
            if self.buffer[x] == 0xFF {
                self.buffer[x] = 0;
            } else {
                self.buffer[x] += 1;
                return;
            }
        }
    }

    /// Codes the low `num_bits` bits of `bits`, most significant first, bit `i` with context
    /// `branches[min(branches.len() - 1, i)]`. Bits of `bits` at or above `num_bits` are ignored.
    #[inline(always)]
    pub fn put_n_bits(
        &mut self,
        bits: u64,
        num_bits: usize,
        branches: &mut [Branch],
    ) -> Result<()> {
        if num_bits == 0 {
            return Ok(());
        }
        if branches.is_empty() || num_bits > 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "put_n_bits: no context or more than 64 bits",
            ));
        }
        let last = branches.len() - 1;
        let mut tmp_value = self.low_value;
        let mut tmp_range = self.range;

        let mut i = num_bits;
        while i > 0 {
            i -= 1;
            (tmp_value, tmp_range) = self.code_bit(
                (bits >> i) & 1 != 0,
                &mut branches[i.min(last)],
                tmp_value,
                tmp_range,
            );
        }

        self.low_value = tmp_value;
        self.range = tmp_range;

        Ok(())
    }

    /// Codes `v` unary with `branches`: `v` ones then a zero, symbol `i` with context
    /// `branches[min(branches.len() - 1, i)]`, so `v == branches.len()` ends with a zero coded
    /// with the last context. `v` above `branches.len()` is refused.
    #[inline(always)]
    pub fn put_unary_encoded(&mut self, v: usize, branches: &mut [Branch]) -> Result<()> {
        let a = branches.len();
        if v > a || a == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "put_unary_encoded: value above the number of contexts",
            ));
        }

        let mut tmp_value = self.low_value;
        let mut tmp_range = self.range;

        // `v` ones then a zero; the zero after `a` ones uses the last context
        for i in 0..=a {
            let cur_bit = v != i;

            (tmp_value, tmp_range) =
                self.code_bit(cur_bit, &mut branches[i.min(a - 1)], tmp_value, tmp_range);
            if !cur_bit {
                break;
            }
        }

        self.low_value = tmp_value;
        self.range = tmp_range;

        Ok(())
    }

    /// Codes one binary symbol with `branch`.
    #[inline(always)]
    pub fn put_bit(&mut self, value: bool, branch: &mut Branch) -> Result<()> {
        let mut tmp_value = self.low_value;
        let mut tmp_range = self.range;

        (tmp_value, tmp_range) = self.code_bit(value, branch, tmp_value, tmp_range);

        self.low_value = tmp_value;
        self.range = tmp_range;

        Ok(())
    }

    /// The number of renormalisation shifts since the start of the stream: the bits of the code
    /// value that no later symbol can change, apart from a carry.
    fn position(&self) -> u64 {
        // `low_value` holds a marker bit, an overflow bit, and `stream_bits` bits of the code
        // value of which the last 8 are the range's precision; the buffer holds the rest.
        let stream_bits = 64 - u64::from(self.low_value.leading_zeros()) - 2;
        self.buffer.len() as u64 * 8 + stream_bits - 8
    }

    /// Ends the stream and writes it (the stream tail of spec section 8).
    ///
    /// The tail is defined on a reference coder whose low register holds the code value's bits
    /// that are not yet in an emitted byte, a byte being emitted once the coding position is 16
    /// bits past its last bit: after the last symbol it codes zero bits with probability 1/2
    /// until the low register is zero, then writes the emitted bytes and nothing more. Coding a
    /// zero bit only moves the position, so the result is the code value's bytes up to its last
    /// nonzero byte, or up to the last byte emitted when the last symbol was coded if that is
    /// longer: the first `max(m, (position - 16) / 8)` bytes of the code value, `m` the length up
    /// to its last nonzero byte. That is what this function writes.
    pub fn finish(&mut self) -> Result<()> {
        let emitted = (self.position().saturating_sub(16) / 8) as usize;

        let mut tmp_value = self.low_value;
        let stream_bits = 64 - tmp_value.leading_zeros() - 2;
        // 55 >= stream_bits >= 8

        tmp_value <<= 63 - stream_bits;
        if tmp_value & (1 << 63) != 0 {
            self.carry();
        }

        let mut shift = 63;
        for _stream_bytes in 0..(stream_bits + 7) >> 3 {
            shift -= 8;
            self.buffer.push((tmp_value >> shift) as u8);
        }

        // the whole code value is in `buffer`; keep what the reference coder writes
        let mut m = self.buffer.len();
        while m > 0 && self.buffer[m - 1] == 0 {
            m -= 1;
        }
        let len = m.max(emitted).min(self.buffer.len());

        self.writer.write_all(&self.buffer[..len])?;
        Ok(())
    }
}
