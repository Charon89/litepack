//! The VP8 adaptive binary arithmetic coder ("bool coder") that the corrections of the
//! `deflate-reconstruct` step are coded with (spec `docs/spec/lpk-v1.md`, section 8; PLAN LIC-2).
//!
//! The coder files are a modified copy of `lepton_jpeg` 0.5.8's (`src/structs/branch.rs`,
//! `vpx_bool_writer.rs`, `vpx_bool_reader.rs`; Copyright (c) Microsoft Corporation, Apache-2.0;
//! the writer and reader also carry the WebM project's BSD-3-Clause banner), each with its
//! headers intact and a statement of LitePack's changes; `NOTICE.txt` is `lepton_jpeg`'s
//! repository notice file. On top of `lepton_jpeg`'s coder this crate fixes the two conventions
//! spec section 8 states:
//!
//! - the stream tail: after the last symbol the writer codes zero bits with probability 1/2 until
//!   its low register is zero, then flushes (see [`VPXBoolWriter::finish`]);
//! - the composite methods: an n-bit field is coded most significant bit first and a unary value
//!   as ones then a zero, symbol `i` of either with context `min(A - 1, i)` of an array of `A`
//!   contexts.
//!
//! The vendored `preflate-rs` (`vendor/preflate-rs-0.7.6`) codes its corrections through the
//! [`Vp8Encode`] and [`Vp8Decode`] traits.
#![forbid(unsafe_code)]

pub mod branch;
pub mod vpx_bool_reader;
pub mod vpx_bool_writer;

use std::io::{Read, Result, Write};

pub use branch::Branch;
pub use vpx_bool_reader::VPXBoolReader;
pub use vpx_bool_writer::VPXBoolWriter;

/// The encoding side of the coder as preflate uses it, generic over the context type.
pub trait Vp8Encode<C> {
    /// Codes one binary symbol with `context`.
    fn put(&mut self, bit: bool, context: &mut C) -> Result<()>;
    /// Codes the low `num_bits` bits of `bits` (1 to 64), most significant first, bit `i` with
    /// `contexts[min(contexts.len() - 1, i)]`.
    fn put_n_bits(&mut self, bits: u64, num_bits: usize, contexts: &mut [C]) -> Result<()>;
    /// Codes `value` (at most `contexts.len()`) as ones then a zero, symbol `i` with
    /// `contexts[min(contexts.len() - 1, i)]`.
    fn put_unary_encoded(&mut self, value: usize, contexts: &mut [C]) -> Result<()>;
    /// Ends the stream and writes it.
    fn finish(&mut self) -> Result<()>;
}

/// The decoding side of the coder as preflate uses it, generic over the context type.
pub trait Vp8Decode<C> {
    /// Decodes one binary symbol with `context`.
    fn get(&mut self, context: &mut C) -> Result<bool>;
    /// Decodes a field of `num_bits` bits (0 to 64) coded by [`Vp8Encode::put_n_bits`].
    fn get_n_bits(&mut self, num_bits: usize, contexts: &mut [C]) -> Result<u64>;
    /// Decodes a value coded by [`Vp8Encode::put_unary_encoded`].
    fn get_unary_encoded(&mut self, contexts: &mut [C]) -> Result<usize>;
}

impl<W: Write> Vp8Encode<Branch> for VPXBoolWriter<W> {
    #[inline(always)]
    fn put(&mut self, bit: bool, context: &mut Branch) -> Result<()> {
        self.put_bit(bit, context)
    }

    #[inline(always)]
    fn put_n_bits(&mut self, bits: u64, num_bits: usize, contexts: &mut [Branch]) -> Result<()> {
        VPXBoolWriter::put_n_bits(self, bits, num_bits, contexts)
    }

    #[inline(always)]
    fn put_unary_encoded(&mut self, value: usize, contexts: &mut [Branch]) -> Result<()> {
        VPXBoolWriter::put_unary_encoded(self, value, contexts)
    }

    fn finish(&mut self) -> Result<()> {
        VPXBoolWriter::finish(self)
    }
}

impl<R: Read> Vp8Decode<Branch> for VPXBoolReader<R> {
    #[inline(always)]
    fn get(&mut self, context: &mut Branch) -> Result<bool> {
        self.get_bit(context)
    }

    #[inline(always)]
    fn get_n_bits(&mut self, num_bits: usize, contexts: &mut [Branch]) -> Result<u64> {
        VPXBoolReader::get_n_bits(self, num_bits, contexts)
    }

    #[inline(always)]
    fn get_unary_encoded(&mut self, contexts: &mut [Branch]) -> Result<usize> {
        VPXBoolReader::get_unary_encoded(self, contexts)
    }
}

#[cfg(test)]
mod tests;
