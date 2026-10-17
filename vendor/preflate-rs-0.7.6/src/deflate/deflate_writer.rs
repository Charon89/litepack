/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/

use crate::preflate_error::{ExitCode, Result, err_exit_code};

/// LitePack change 5 (LITEPACK-CHANGES.md): the most bytes one capped write step appends: a
/// reference token is at most 48 bits (15 + 5 + 15 + 13), written while at most 32 bits are
/// buffered, so at most two 4-byte flushes; the padding of a last block flushes at most 8.
pub const CAP_SLACK: usize = 8;

use super::deflate_token::{DeflateHuffmanType, DeflateTokenBlockType};
use super::{
    deflate_constants::{
        DIST_BASE_TABLE, DIST_EXTRA_TABLE, LENGTH_BASE_TABLE, LENGTH_EXTRA_TABLE, MIN_MATCH,
        NONLEN_CODE_COUNT, quantize_distance, quantize_length,
    },
    deflate_token::{DeflateToken, DeflateTokenBlock},
};

use super::bit_writer::BitWriter;
use super::huffman_encoding::HuffmanWriter;

/// Takes a tokenized block and writes it to the original compressed output.
pub struct DeflateWriter {
    /// bit writer to write partial bits to output
    bitwriter: BitWriter,

    /// compressed output
    output: Vec<u8>,
}

impl std::fmt::Debug for DeflateWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DeflateWriter {{ bitwriter: {:?}, output: len={} }}",
            self.bitwriter,
            self.output.len()
        )
    }
}

impl DeflateWriter {
    pub fn new() -> Self {
        Self {
            output: Vec::new(),
            bitwriter: BitWriter::default(),
        }
    }

    pub fn detach_output(&mut self) -> Vec<u8> {
        let mut o = Vec::new();
        o.append(&mut self.output);
        o
    }

    pub fn encode_block(&mut self, block: &DeflateTokenBlock) -> Result<()> {
        match &block.block_type {
            DeflateTokenBlockType::Stored { uncompressed, .. } => {
                self.bitwriter.write(block.last as u32, 1, &mut self.output);
                self.bitwriter.write(0, 2, &mut self.output);
                self.bitwriter.pad(0, &mut self.output);
                self.bitwriter.flush_whole_bytes(&mut self.output);

                self.output
                    .extend_from_slice(&(uncompressed.len() as u16).to_le_bytes());
                self.output
                    .extend_from_slice(&(!uncompressed.len() as u16).to_le_bytes());

                self.output.extend_from_slice(&uncompressed);
            }
            DeflateTokenBlockType::Huffman {
                tokens,
                huffman_type,
            } => {
                match huffman_type {
                    DeflateHuffmanType::Static => {
                        self.bitwriter.write(block.last as u32, 1, &mut self.output);
                        self.bitwriter.write(1, 2, &mut self.output);
                        let huffman_writer = HuffmanWriter::start_fixed_huffman_table();
                        self.encode_huffman(tokens, &huffman_writer);
                    }
                    DeflateHuffmanType::Dynamic {
                        huffman_encoding, ..
                    } => {
                        self.bitwriter.write(block.last as u32, 1, &mut self.output);
                        self.bitwriter.write(2, 2, &mut self.output);

                        let huffman_writer = HuffmanWriter::start_dynamic_huffman_table(
                            &mut self.bitwriter,
                            &huffman_encoding,
                            &mut self.output,
                        )?;

                        self.encode_huffman(tokens, &huffman_writer);
                    }
                }

                if block.last {
                    self.bitwriter.pad(0, &mut self.output);
                }
            }
        }

        Ok(())
    }

    pub fn flush(&mut self) {
        self.bitwriter.flush_whole_bytes(&mut self.output);
    }

    /// LitePack change 1 (LITEPACK-CHANGES.md): the number of whole bytes written so far, so a
    /// limited recreation can stop once its output exceeds the limit.
    pub fn output_len(&self) -> usize {
        self.output.len()
    }

    /// LitePack change 5 (LITEPACK-CHANGES.md): a writer for a recreation limited to `limit`
    /// bytes. Its buffer is reserved once, `limit + CAP_SLACK` bytes, and
    /// [`DeflateWriter::encode_block_capped`] never grows it past that.
    pub fn with_cap(limit: usize) -> Result<Self> {
        let mut output = Vec::new();
        if output
            .try_reserve_exact(limit.saturating_add(CAP_SLACK))
            .is_err()
        {
            return err_exit_code(ExitCode::OutOfMemory, "Out of memory");
        }
        Ok(Self {
            output,
            bitwriter: BitWriter::default(),
        })
    }

    /// LitePack change 5: the capacity of the output buffer.
    pub fn capacity(&self) -> usize {
        self.output.capacity()
    }

    /// LitePack change 5: [`DeflateWriter::encode_block`] for a writer made by
    /// [`DeflateWriter::with_cap`], writing the same bytes, that returns `OutputLimit` as soon as
    /// the output would pass `limit`. Every write of the bit writer appends at most 4 bytes and
    /// every primitive step below (block header, one token, the end-of-block code, the final
    /// padding) at most `CAP_SLACK`, so each step runs only while the output is at most `limit`
    /// bytes; the dynamic Huffman table is written to a scratch buffer and the stored payload is
    /// measured first, both appended only if they keep the output within `limit`. A valid stream
    /// of at most `limit` bytes is never refused (its output only grows), and the buffer never
    /// exceeds `limit + CAP_SLACK` bytes, its reserved capacity.
    pub fn encode_block_capped(&mut self, block: &DeflateTokenBlock, limit: usize) -> Result<()> {
        self.room(limit, 0)?;
        match &block.block_type {
            DeflateTokenBlockType::Stored { uncompressed, .. } => {
                self.bitwriter.write(block.last as u32, 1, &mut self.output);
                self.bitwriter.write(0, 2, &mut self.output);
                self.bitwriter.pad(0, &mut self.output);
                self.bitwriter.flush_whole_bytes(&mut self.output);

                self.room(limit, 4 + uncompressed.len())?;
                self.output
                    .extend_from_slice(&(uncompressed.len() as u16).to_le_bytes());
                self.output
                    .extend_from_slice(&(!uncompressed.len() as u16).to_le_bytes());

                self.output.extend_from_slice(&uncompressed);
            }
            DeflateTokenBlockType::Huffman {
                tokens,
                huffman_type,
            } => {
                let huffman_writer = match huffman_type {
                    DeflateHuffmanType::Static => {
                        self.bitwriter.write(block.last as u32, 1, &mut self.output);
                        self.bitwriter.write(1, 2, &mut self.output);
                        HuffmanWriter::start_fixed_huffman_table()
                    }
                    DeflateHuffmanType::Dynamic {
                        huffman_encoding, ..
                    } => {
                        self.bitwriter.write(block.last as u32, 1, &mut self.output);
                        self.bitwriter.write(2, 2, &mut self.output);

                        let mut table = Vec::new();
                        let huffman_writer = HuffmanWriter::start_dynamic_huffman_table(
                            &mut self.bitwriter,
                            &huffman_encoding,
                            &mut table,
                        )?;
                        self.room(limit, table.len())?;
                        self.output.extend_from_slice(&table);
                        huffman_writer
                    }
                };

                for token in tokens {
                    self.room(limit, 0)?;
                    self.encode_token(token, &huffman_writer);
                }
                self.room(limit, 0)?;
                huffman_writer.write_literal(&mut self.bitwriter, &mut self.output, 256);

                if block.last {
                    self.room(limit, 0)?;
                    self.bitwriter.pad(0, &mut self.output);
                }
            }
        }
        self.room(limit, 0)
    }

    /// LitePack change 5: `OutputLimit` unless `n` more bytes keep the output within `limit`.
    fn room(&self, limit: usize, n: usize) -> Result<()> {
        if self.output.len().saturating_add(n) > limit {
            return err_exit_code(
                ExitCode::OutputLimit,
                "recreation exceeds the output limit",
            );
        }
        Ok(())
    }

    /// LitePack change 5: one token, as [`DeflateWriter::encode_huffman`] writes it.
    fn encode_token(&mut self, token: &DeflateToken, huffman_writer: &HuffmanWriter) {
        match token {
            DeflateToken::Literal(lit) => {
                huffman_writer.write_literal(&mut self.bitwriter, &mut self.output, u16::from(*lit));
            }
            DeflateToken::Reference(reference) => {
                let lencode = quantize_length(reference.len());
                huffman_writer.write_literal(
                    &mut self.bitwriter,
                    &mut self.output,
                    NONLEN_CODE_COUNT as u16 + u16::from(lencode.get()),
                );

                let lenextra: u8 = LENGTH_EXTRA_TABLE[usize::from(lencode.get())];
                if lenextra > 0 {
                    self.bitwriter.write(
                        reference.len() - MIN_MATCH - LENGTH_BASE_TABLE[usize::from(lencode.get())] as u32,
                        lenextra.into(),
                        &mut self.output,
                    );
                }

                let distcode = quantize_distance(reference.dist());
                huffman_writer.write_distance(&mut self.bitwriter, &mut self.output, distcode);

                let distextra = DIST_EXTRA_TABLE[usize::from(distcode.get())];
                if distextra > 0 {
                    self.bitwriter.write(
                        reference.dist() - 1 - DIST_BASE_TABLE[usize::from(distcode.get())] as u32,
                        distextra.into(),
                        &mut self.output,
                    );
                }
            }
        }
    }

    fn encode_huffman(&mut self, tokens: &Vec<DeflateToken>, huffman_writer: &HuffmanWriter) {
        for token in tokens {
            match token {
                DeflateToken::Literal(lit) => {
                    huffman_writer.write_literal(
                        &mut self.bitwriter,
                        &mut self.output,
                        u16::from(*lit),
                    );
                }
                DeflateToken::Reference(reference) => {
                    let lencode = quantize_length(reference.len());
                    huffman_writer.write_literal(
                        &mut self.bitwriter,
                        &mut self.output,
                        NONLEN_CODE_COUNT as u16 + u16::from(lencode.get()),
                    );

                    let lenextra: u8 = LENGTH_EXTRA_TABLE[usize::from(lencode.get())];
                    if lenextra > 0 {
                        self.bitwriter.write(
                            reference.len()
                                - MIN_MATCH
                                - LENGTH_BASE_TABLE[usize::from(lencode.get())] as u32,
                            lenextra.into(),
                            &mut self.output,
                        );
                    }

                    let distcode = quantize_distance(reference.dist());
                    huffman_writer.write_distance(&mut self.bitwriter, &mut self.output, distcode);

                    let distextra = DIST_EXTRA_TABLE[usize::from(distcode.get())];
                    if distextra > 0 {
                        self.bitwriter.write(
                            reference.dist()
                                - 1
                                - DIST_BASE_TABLE[usize::from(distcode.get())] as u32,
                            distextra.into(),
                            &mut self.output,
                        );
                    }
                }
            }
        }

        huffman_writer.write_literal(&mut self.bitwriter, &mut self.output, 256);
    }
}

/// Create a set of blocks and read them back to see if they are identical
#[test]
fn roundtrip_deflate_writer() {
    use super::deflate_reader::parse_deflate_whole;

    let mut w = DeflateWriter::new();

    let blocks = [
        DeflateTokenBlock {
            block_type: DeflateTokenBlockType::Huffman {
                tokens: vec![
                    DeflateToken::Literal(0),
                    DeflateToken::Literal(2),
                    DeflateToken::Literal(3),
                ],
                huffman_type: DeflateHuffmanType::Static,
            },
            last: false,
        },
        DeflateTokenBlock {
            block_type: DeflateTokenBlockType::Huffman {
                tokens: vec![
                    DeflateToken::Literal(0),
                    DeflateToken::Literal(2),
                    DeflateToken::Literal(3),
                ],
                huffman_type: DeflateHuffmanType::Static,
            },
            last: false,
        },
        DeflateTokenBlock {
            block_type: DeflateTokenBlockType::Stored {
                uncompressed: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            },
            last: false,
        },
        DeflateTokenBlock {
            block_type: DeflateTokenBlockType::Huffman {
                tokens: vec![
                    DeflateToken::Literal(0),
                    DeflateToken::Literal(1),
                    DeflateToken::new_ref(100, 1),
                    DeflateToken::new_ref(258, 1),
                    DeflateToken::Literal(3),
                ],
                huffman_type: DeflateHuffmanType::Static,
            },
            last: true,
        },
    ];

    for i in 0..blocks.len() {
        w.encode_block(&blocks[i]).unwrap();
    }
    w.flush();

    let output = w.detach_output();

    let (r, _) = parse_deflate_whole(&output).unwrap();

    assert_eq!(blocks.len(), r.blocks.len());
    for i in 0..blocks.len() {
        assert_eq!(blocks[i], r.blocks[i], "block {}", i);
    }
}

/// rountrips all deflate files in the sample directory
#[test]
fn roundtrip_full_file() {
    crate::utils::test_on_all_deflate_files(|buffer| {
        let (r, _plain_text) = super::deflate_reader::parse_deflate_whole(&buffer).unwrap();

        let output = write_deflate_blocks(&r.blocks);

        if r.compressed_size != output.len() || buffer[0..r.compressed_size] != output[..] {
            println!("mismatch");
        }
        //assert_eq!(buffer.len(), output.len());
        //assert_eq!(buffer, output);
    });
}

#[cfg(test)]
pub fn write_deflate_blocks(blocks: &[DeflateTokenBlock]) -> Vec<u8> {
    let mut w = DeflateWriter::new();
    for i in 0..blocks.len() {
        w.encode_block(&blocks[i]).unwrap();
    }
    w.flush();

    let output = w.detach_output();
    output
}
