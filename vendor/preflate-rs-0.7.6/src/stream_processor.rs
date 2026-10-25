/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/

//! Responsible for performing preflate and recreation on a chunk by chunk basis

use std::io::{BufRead, Cursor};

use bitcode::{Decode, Encode};
// LitePack change 7 (LITEPACK-CHANGES.md): the VP8 coder of lpk-codecs-vp8 instead of cabac's.
use lpk_codecs_vp8::{VPXBoolReader, VPXBoolWriter};

use crate::{
    PreflateConfig, Result,
    cabac_codec::{PredictionDecoderCabac, PredictionEncoderCabac},
    deflate::{
        deflate_reader::DeflateParser, deflate_token::DeflateTokenBlock,
        deflate_writer::DeflateWriter,
    },
    estimator::preflate_parameter_estimator::{
        TokenPredictorParameters, estimate_preflate_parameters,
    },
    preflate_error::{AddContext, ExitCode, PreflateError},
    preflate_input::{PlainText, PreflateInput},
    statistical_codec::{CodecCorrection, PredictionDecoder, PredictionEncoder},
    token_predictor::TokenPredictor,
};

/// the data required to reconstruct the deflate stream exactly the way that it was
#[derive(Encode, Decode)]
struct ReconstructionData {
    pub parameters: TokenPredictorParameters,
    pub corrections: Vec<u8>,
}

impl ReconstructionData {
    pub fn read(data: &[u8]) -> Result<Self> {
        bitcode::decode(data).map_err(|e| {
            PreflateError::new(
                ExitCode::InvalidCompressedWrapper,
                format!("{:?}", e).as_str(),
            )
        })
    }
}

/// Result of a call to PreflateStreamProcessor::decompress
pub struct PreflateStreamChunkResult {
    /// the extra data that is needed to reconstruct the deflate stream exactly as it was written
    pub corrections: Vec<u8>,

    /// the number of bytes that were processed from the compressed stream (this will be exactly the
    /// data that will be recreated using the cabac_encoded data)
    pub compressed_size: usize,

    /// the parameters that were used to compress the stream. Only returned for the first
    /// chunk that is passed in.
    pub parameters: Option<TokenPredictorParameters>,

    pub blocks: Vec<DeflateTokenBlock>,
}

/// Takes a stream of deflate compressed data removes the deflate compression, recording the data
/// that can be used to reconstruct it along with the plain-text.
#[derive(Debug)]
pub struct PreflateStreamProcessor {
    predictor: Option<TokenPredictor>,
    validator: Option<RecreateStreamProcessor>,
    parser: DeflateParser,
    max_chain_length: u32,
}

impl PreflateStreamProcessor {
    /// Creates a new PreflateStreamProcessor
    /// plain_text_limit: the maximum size of the plain text that will decompressed to memory
    /// verify: if true, the decompressed data will be recompressed and compared to the original as it is run
    pub fn new(config: &PreflateConfig) -> Self {
        Self {
            predictor: None,
            parser: DeflateParser::new(config.plain_text_limit),
            max_chain_length: config.max_chain_length,
            validator: if config.verify_compression {
                Some(RecreateStreamProcessor::new())
            } else {
                None
            },
        }
    }

    pub fn is_done(&self) -> bool {
        self.parser.is_done()
    }

    pub fn plain_text(&self) -> &PlainText {
        &self.parser.plain_text()
    }

    pub fn shrink_to_dictionary(&mut self) {
        self.parser.shrink_to_dictionary();
    }

    pub fn detach_plain_text(self) -> PlainText {
        self.parser.detach_plain_text()
    }

    /// decompresses a deflate stream and returns the plaintext and cabac_encoded data that can be used to reconstruct it
    pub fn decompress(&mut self, compressed_data: &[u8]) -> Result<PreflateStreamChunkResult> {
        let contents = self.parser.parse(compressed_data)?;

        let mut cabac_encoded = Vec::new();

        let mut cabac_encoder =
            PredictionEncoderCabac::new(VPXBoolWriter::new(&mut cabac_encoded).unwrap());

        if let Some(predictor) = &mut self.predictor {
            let mut input = PreflateInput::new(&self.parser.plain_text());

            // we are missing the last couple hashes in the dictionary since we didn't
            // have the full plaintext yet.
            predictor.add_missing_previous_hash(&input);

            predict_blocks(&contents.blocks, predictor, &mut cabac_encoder, &mut input)?;

            cabac_encoder.finish();

            if let Some(validator) = &mut self.validator {
                let (recompressed, _rec_blocks) = validator.recompress(
                    &mut Cursor::new(self.parser.plain_text().text()),
                    &cabac_encoded,
                )?;

                #[cfg(test)]
                for i in 0..contents.blocks.len() {
                    crate::utils::assert_block_eq(&contents.blocks[i], &_rec_blocks[i]);
                }

                // we should always succeed here in test code
                #[cfg(test)]
                crate::utils::assert_eq_array(
                    &recompressed,
                    &compressed_data[..contents.compressed_size],
                );

                if recompressed[..] != compressed_data[..contents.compressed_size] {
                    return Err(PreflateError::new(
                        ExitCode::RoundtripMismatch,
                        "recompressed data does not match original",
                    ));
                }
            }

            Ok(PreflateStreamChunkResult {
                corrections: cabac_encoded,
                compressed_size: contents.compressed_size,
                parameters: None,
                blocks: contents.blocks,
            })
        } else {
            let params =
                estimate_preflate_parameters(&contents, &self.parser.plain_text()).context()?;

            if params.max_chain > self.max_chain_length {
                return Err(PreflateError::new(
                    ExitCode::NoCompressionCandidates,
                    format!(
                        "max_chain {} is larger than configured max_chain_length {}",
                        params.max_chain, self.max_chain_length
                    ),
                ));
            }

            let mut input = PreflateInput::new(&self.parser.plain_text());

            let mut token_predictor = TokenPredictor::new(&params);

            predict_blocks(
                &contents.blocks,
                &mut token_predictor,
                &mut cabac_encoder,
                &mut input,
            )?;

            cabac_encoder.finish();

            let reconstruction_data = bitcode::encode(&ReconstructionData {
                parameters: params,
                corrections: cabac_encoded,
            });

            self.predictor = Some(token_predictor);

            if let Some(validator) = &mut self.validator {
                let (recompressed, _rec_blocks) = validator.recompress(
                    &mut Cursor::new(self.parser.plain_text().text()),
                    &reconstruction_data,
                )?;

                #[cfg(test)]
                for i in 0..contents.blocks.len() {
                    crate::utils::assert_block_eq(&contents.blocks[i], &_rec_blocks[i]);
                }

                // we should always succeed here in test code
                #[cfg(test)]
                crate::utils::assert_eq_array(
                    &recompressed,
                    &compressed_data[..contents.compressed_size],
                );

                if recompressed[..] != compressed_data[..contents.compressed_size] {
                    return Err(PreflateError::new(
                        ExitCode::RoundtripMismatch,
                        "recompressed data does not match original",
                    ));
                }
            }

            Ok(PreflateStreamChunkResult {
                corrections: reconstruction_data,
                compressed_size: contents.compressed_size,
                parameters: Some(params),
                blocks: contents.blocks,
            })
        }
    }
}

/// Decompresses a deflate stream and returns the plaintext and diff data that can be used to reconstruct it
/// via recreate_whole_deflate_stream
pub fn preflate_whole_deflate_stream(
    compressed_data: &[u8],
    config: &PreflateConfig,
) -> Result<(PreflateStreamChunkResult, PlainText)> {
    let mut state = PreflateStreamProcessor::new(config);
    let r = state.decompress(compressed_data)?;

    Ok((r, state.parser.detach_plain_text()))
}

/// recreates the original deflate stream, piece-by-piece
#[derive(Debug)]
pub struct RecreateStreamProcessor {
    predictor: Option<TokenPredictor>,
    writer: DeflateWriter,
    plain_text: PlainText,
}

impl RecreateStreamProcessor {
    pub fn new() -> Self {
        Self {
            predictor: None,
            writer: DeflateWriter::new(),
            plain_text: PlainText::new(),
        }
    }

    pub fn recompress(
        &mut self,
        plain_text: &mut impl BufRead,
        corrections: &[u8],
    ) -> Result<(Vec<u8>, Vec<DeflateTokenBlock>)> {
        loop {
            let buf = plain_text.fill_buf().context()?;
            let buf_len = buf.len();
            if buf_len == 0 {
                break;
            }

            self.plain_text.append(&buf);

            plain_text.consume(buf_len);
        }

        let mut input = PreflateInput::new(&self.plain_text);

        if let Some(predictor) = &mut self.predictor {
            let mut cabac_decoder =
                PredictionDecoderCabac::new(VPXBoolReader::new(Cursor::new(corrections)).unwrap());

            predictor.add_missing_previous_hash(&input);

            let blocks =
                recreate_blocks(predictor, &mut cabac_decoder, &mut self.writer, &mut input)
                    .context()?;

            self.plain_text.shrink_to_dictionary();

            self.writer.flush();

            Ok((self.writer.detach_output(), blocks))
        } else {
            let r = ReconstructionData::read(corrections)?;

            let mut predictor = TokenPredictor::new(&r.parameters);

            let mut cabac_decoder =
                PredictionDecoderCabac::new(VPXBoolReader::new(Cursor::new(r.corrections)).unwrap());

            let blocks = recreate_blocks(
                &mut predictor,
                &mut cabac_decoder,
                &mut self.writer,
                &mut input,
            )
            .context()?;

            self.predictor = Some(predictor);

            self.plain_text.shrink_to_dictionary();

            self.writer.flush();

            Ok((self.writer.detach_output(), blocks))
        }
    }
}

/// recompresses a deflate stream using the cabac_encoded data that was returned from decompress_deflate_stream
pub fn recreate_whole_deflate_stream(
    plain_text: &[u8],
    prediction_corrections: &[u8],
) -> Result<Vec<u8>> {
    let mut state = RecreateStreamProcessor::new();

    let (recompressed, _) =
        state.recompress(&mut Cursor::new(&plain_text), prediction_corrections)?;

    Ok(recompressed)
}

/// LitePack changes 1-3 (LITEPACK-CHANGES.md): [`recreate_whole_deflate_stream`] with an output
/// limit, for corrections that come from an untrusted source.
///
/// Returns the same bytes as [`recreate_whole_deflate_stream`] whenever that function's output
/// is at most `limit` bytes long and the predictor parameters are ones analysis produces.
/// Otherwise it returns an error:
/// - the predictor parameters decoded from the corrections lie outside the ranges
///   `estimate_preflate_parameters` produces (`InvalidParameterHeader`, before anything is
///   recreated);
/// - the recreation would produce more than `limit` blocks: every Deflate block this crate
///   writes takes at least 10 bits (a fixed-Huffman block with no tokens: 3 header bits and
///   the 7-bit end-of-block code; a stored block takes 35 bits or more, a dynamic one more
///   than 10), so a stream of at most `limit` bytes has at most `limit` blocks;
/// - its output exceeds `limit` bytes (`OutputLimit`). The writer's buffer is reserved once,
///   `limit + 8` bytes, and never grows: every write step checks the limit first (change 5);
///   the check after each block is kept as a backstop;
/// - a correction asks for more hops along the hash chain than the parameters' `max_chain`
///   (`InvalidPredictionData`, change 6).
pub fn recreate_whole_deflate_stream_limited(
    plain_text: &[u8],
    prediction_corrections: &[u8],
    limit: usize,
) -> Result<Vec<u8>> {
    recreate_whole_deflate_stream_limited_report(plain_text, prediction_corrections, limit).0
}

/// LitePack change 8 (LITEPACK-CHANGES.md), for tests only: `record` (a record returned by the
/// analysis) with its coded correction stream replaced by `coded`, so a test can make the
/// recreation meet a crafted correction stream under valid parameters.
#[doc(hidden)]
pub fn litepack_record_with_coded_corrections(record: &[u8], coded: Vec<u8>) -> Result<Vec<u8>> {
    let r = ReconstructionData::read(record)?;
    Ok(bitcode::encode(&ReconstructionData {
        parameters: r.parameters,
        corrections: coded,
    }))
}

/// LitePack change 5 (LITEPACK-CHANGES.md): [`recreate_whole_deflate_stream_limited`], also
/// returning the capacity of the writer's buffer when the recreation ended (for tests and
/// fuzzing: it never exceeds `limit + 8`).
#[doc(hidden)]
pub fn recreate_whole_deflate_stream_limited_report(
    plain_text: &[u8],
    prediction_corrections: &[u8],
    limit: usize,
) -> (Result<Vec<u8>>, usize) {
    let mut writer = match DeflateWriter::with_cap(limit) {
        Ok(w) => w,
        Err(e) => return (Err(e), 0),
    };
    let r = recreate_limited_into(plain_text, prediction_corrections, limit, &mut writer);
    let capacity = writer.capacity();
    (r, capacity)
}

fn recreate_limited_into(
    plain_text: &[u8],
    prediction_corrections: &[u8],
    limit: usize,
    writer: &mut DeflateWriter,
) -> Result<Vec<u8>> {
    let r = ReconstructionData::read(prediction_corrections)?;

    crate::estimator::preflate_parameter_estimator::check_decoded_parameters(&r.parameters)?;

    let mut plain = PlainText::new();
    plain.append(plain_text);
    let mut input = PreflateInput::new(&plain);

    let mut predictor = TokenPredictor::new(&r.parameters);
    predictor.limit_hops_to_max_chain();

    let reader = VPXBoolReader::new(Cursor::new(r.corrections)).map_err(|e| {
        PreflateError::new(ExitCode::InvalidPredictionData, e.to_string().as_str())
    })?;
    let mut cabac_decoder = PredictionDecoderCabac::new(reader);

    let mut blocks: usize = 0;
    loop {
        if blocks >= limit {
            return Err(PreflateError::new(
                ExitCode::OutputLimit,
                "recreation exceeds the block limit",
            ));
        }

        let block = predictor.recreate_block(&mut cabac_decoder, &mut input)?;

        writer.encode_block_capped(&block, limit)?;
        blocks += 1;

        // backstop (encode_block_capped already refuses)
        if writer.output_len() > limit {
            return Err(PreflateError::new(
                ExitCode::OutputLimit,
                "recreation exceeds the output limit",
            ));
        }

        // end of stream normally is the last block (as in recreate_blocks)
        let last = cabac_decoder
            .decode_correction_bool(CodecCorrection::EndOfChunk, input.remaining() == 0);

        if last {
            break;
        }
    }

    writer.flush();

    let output = writer.detach_output();
    if output.len() > limit {
        return Err(PreflateError::new(
            ExitCode::OutputLimit,
            "recreation exceeds the output limit",
        ));
    }
    Ok(output)
}

/// takes a deflate compressed stream, analyzes it, decoompresses it, and records
/// any differences in the encoder codec
#[cfg(test)]
fn encode_mispredictions(
    deflate: &crate::deflate::deflate_reader::DeflateContents,
    plain_text: &PlainText,
    params: &TokenPredictorParameters,
    encoder: &mut impl PredictionEncoder,
) -> Result<()> {
    let mut input = PreflateInput::new(plain_text);

    let mut token_predictor = TokenPredictor::new(&params);

    predict_blocks(&deflate.blocks, &mut token_predictor, encoder, &mut input)?;

    Ok(())
}

fn predict_blocks(
    blocks: &[DeflateTokenBlock],
    token_predictor: &mut TokenPredictor,
    encoder: &mut impl PredictionEncoder,
    input: &mut PreflateInput,
) -> Result<()> {
    for i in 0..blocks.len() {
        token_predictor.predict_block(&blocks[i], encoder, input, i == blocks.len() - 1)?;
        // end of stream normally is the last block
        encoder.encode_correction_bool(
            CodecCorrection::EndOfChunk,
            i == blocks.len() - 1,
            input.remaining() == 0,
        );
    }
    assert!(input.remaining() == 0);
    Ok(())
}

#[cfg(test)]
fn decode_mispredictions(
    params: &TokenPredictorParameters,
    input: &mut PreflateInput,
    decoder: &mut impl crate::statistical_codec::PredictionDecoder,
) -> Result<(Vec<u8>, Vec<DeflateTokenBlock>)> {
    let mut deflate_writer: DeflateWriter = DeflateWriter::new();
    let mut predictor = TokenPredictor::new(&params);

    let output_blocks = recreate_blocks(&mut predictor, decoder, &mut deflate_writer, input)?;

    deflate_writer.flush();

    Ok((deflate_writer.detach_output(), output_blocks))
}

fn recreate_blocks<D: PredictionDecoder>(
    token_predictor: &mut TokenPredictor,
    decoder: &mut D,
    deflate_writer: &mut DeflateWriter,
    input: &mut PreflateInput,
) -> Result<Vec<DeflateTokenBlock>> {
    let mut output_blocks = Vec::new();
    loop {
        let block = token_predictor.recreate_block(decoder, input)?;

        deflate_writer.encode_block(&block)?;

        output_blocks.push(block);

        // end of stream normally is the last block
        let last =
            decoder.decode_correction_bool(CodecCorrection::EndOfChunk, input.remaining() == 0);

        if last {
            break;
        }
    }
    Ok(output_blocks)
}

/// decompresses a deflate stream and returns the plaintext and cabac_encoded data that can be used to reconstruct it
/// This version used cabac's DebugWriter and DebugReader upstream; LitePack change 7 uses the VP8 coder.
#[cfg(test)]
fn decompress_deflate_stream_assert(
    compressed_data: &[u8],
    verify: bool,
) -> Result<(PreflateStreamChunkResult, PlainText)> {
    use crate::deflate::deflate_reader::parse_deflate_whole;
    // LitePack change 7: upstream's debugging coder of cabac replaced by the VP8 coder.

    use crate::preflate_error::AddContext;

    let mut cabac_encoded = Vec::new();

    let mut cabac_encoder =
        PredictionEncoderCabac::new(VPXBoolWriter::new(&mut cabac_encoded).unwrap());

    let (contents, plain_text) = parse_deflate_whole(compressed_data)?;

    let params = estimate_preflate_parameters(&contents, &plain_text).context()?;

    encode_mispredictions(&contents, &plain_text, &params, &mut cabac_encoder)?;
    assert_eq!(contents.compressed_size, compressed_data.len());
    cabac_encoder.finish();

    let reconstruction_data = bitcode::encode(&ReconstructionData {
        parameters: params,
        corrections: cabac_encoded,
    });

    if verify {
        let r = ReconstructionData::read(&reconstruction_data)?;

        let mut cabac_decoder =
            PredictionDecoderCabac::new(VPXBoolReader::new(Cursor::new(&r.corrections)).unwrap());

        let params = r.parameters;
        let mut input = PreflateInput::new(&plain_text);
        let (recompressed, _recreated_blocks) =
            decode_mispredictions(&params, &mut input, &mut cabac_decoder)?;

        if recompressed[..] != compressed_data[..] {
            return Err(PreflateError::new(
                ExitCode::RoundtripMismatch,
                "recompressed data does not match original",
            ));
        }
    }

    Ok((
        PreflateStreamChunkResult {
            corrections: reconstruction_data,
            compressed_size: contents.compressed_size,
            parameters: Some(params),
            blocks: contents.blocks,
        },
        plain_text,
    ))
}

#[test]
fn verify_roundtrip_assert() {
    crate::init_logging();

    use crate::utils::read_file;

    let v = read_file("compressed_zlib_level1.deflate");

    let (r, plain_text) = decompress_deflate_stream_assert(&v, true).unwrap();
    let recompressed = recompress_deflate_stream_assert(&plain_text, &r.corrections).unwrap();
    assert!(v == recompressed);
}

#[test]
fn verify_roundtrip_zlib() {
    crate::init_logging();

    for i in 0..9 {
        verify_file(&format!("compressed_zlib_level{}.deflate", i));
    }
}

#[test]
fn verify_roundtrip_flate2() {
    crate::init_logging();

    for i in 0..9 {
        verify_file(&format!("compressed_flate2_level{}.deflate", i));
    }
}

#[test]
fn verify_roundtrip_libdeflate() {
    crate::init_logging();

    for i in 0..9 {
        verify_file(&format!("compressed_libdeflate_level{}.deflate", i));
    }
}

#[cfg(test)]
fn verify_file(filename: &str) {
    use crate::utils::read_file;
    let v = read_file(filename);

    let (r, plain_text) = preflate_whole_deflate_stream(&v, &PreflateConfig::default()).unwrap();
    let recompressed = recreate_whole_deflate_stream(plain_text.text(), &r.corrections).unwrap();
    assert!(v == recompressed);
}

/// recompresses a deflate stream using the cabac_encoded data that was returned from decompress_deflate_stream
/// This version used cabac's DebugReader upstream; LitePack change 7 uses the VP8 coder.
#[cfg(test)]
fn recompress_deflate_stream_assert(
    plain_text: &PlainText,
    prediction_corrections: &[u8],
) -> Result<Vec<u8>> {
    // LitePack change 7: upstream's debugging coder of cabac replaced by the VP8 coder.

    let r = ReconstructionData::read(prediction_corrections)?;

    let mut cabac_decoder =
        PredictionDecoderCabac::new(VPXBoolReader::new(Cursor::new(&r.corrections)).unwrap());

    let mut input = PreflateInput::new(plain_text);
    let (recompressed, _recreated_blocks) =
        decode_mispredictions(&r.parameters, &mut input, &mut cabac_decoder)?;
    Ok(recompressed)
}

#[cfg(test)]
fn analyze_compressed_data_fast(
    compressed_data: &[u8],
    header_crc32: Option<u32>,
    uncompressed_size: &mut u64,
) {
    use crate::{
        cabac_codec::{PredictionDecoderCabac, PredictionEncoderCabac},
        deflate::deflate_reader::parse_deflate_whole,
    };
    use std::io::Cursor;

    // LitePack change 7 (LITEPACK-CHANGES.md): the VP8 coder of lpk-codecs-vp8 instead of cabac's.
use lpk_codecs_vp8::{VPXBoolReader, VPXBoolWriter};

    let mut buffer = Vec::new();

    let mut cabac_encoder = PredictionEncoderCabac::new(VPXBoolWriter::new(&mut buffer).unwrap());

    let (contents, plain_text) = parse_deflate_whole(compressed_data).unwrap();

    let params = estimate_preflate_parameters(&contents, &plain_text).unwrap();

    println!("params: {:?}", params);

    encode_mispredictions(&contents, &plain_text, &params, &mut cabac_encoder).unwrap();

    if let Some(crc) = header_crc32 {
        let result_crc = crc32fast::hash(&plain_text.text());
        assert_eq!(result_crc, crc);
    }

    assert_eq!(contents.compressed_size, compressed_data.len());

    cabac_encoder.finish();

    cabac_encoder.print();

    println!("buffer size: {}", buffer.len());

    let mut cabac_decoder =
        PredictionDecoderCabac::new(VPXBoolReader::new(Cursor::new(&buffer)).unwrap());

    let mut input = PreflateInput::new(&plain_text);

    let (recompressed, _recreated_blocks) =
        decode_mispredictions(&params, &mut input, &mut cabac_decoder).unwrap();

    assert!(recompressed[..] == compressed_data[..]);

    *uncompressed_size = plain_text.text().len() as u64;
}

#[cfg(test)]
fn analyze_compressed_data_verify(
    compressed_data: &[u8],
    header_crc32: Option<u32>,
    _deflate_info_dump_level: i32,
    uncompressed_size: &mut u64,
) {
    use crate::{
        cabac_codec::{PredictionDecoderCabac, PredictionEncoderCabac},
        deflate::{deflate_reader::parse_deflate_whole, deflate_token::DeflateTokenBlockType},
        statistical_codec::{VerifyPredictionDecoder, VerifyPredictionEncoder},
        utils::assert_eq_array,
    };
    // LitePack change 7: upstream's debugging coder of cabac replaced by the VP8 coder.
    use std::io::Cursor;

    let mut buffer = Vec::new();

    let cabac_encoder = PredictionEncoderCabac::new(VPXBoolWriter::new(&mut buffer).unwrap());
    let debug_encoder = VerifyPredictionEncoder::new();

    let mut combined_encoder = (debug_encoder, cabac_encoder);

    let (contents, plain_text) = parse_deflate_whole(compressed_data).unwrap();

    let params = estimate_preflate_parameters(&contents, &plain_text).unwrap();

    println!("params: {:?}", params);

    encode_mispredictions(&contents, &plain_text, &params, &mut combined_encoder).unwrap();

    assert_eq!(contents.compressed_size, compressed_data.len());

    combined_encoder.finish();

    combined_encoder.0.print();

    let actions = combined_encoder.0.actions();

    println!("buffer size: {}", buffer.len());

    let debug_decoder = VerifyPredictionDecoder::new(actions);
    let cabac_decoder =
        PredictionDecoderCabac::new(VPXBoolReader::new(Cursor::new(&buffer)).unwrap());

    let mut combined_decoder = (debug_decoder, cabac_decoder);
    let mut input = PreflateInput::new(&plain_text);

    let (recompressed, recreated_blocks) =
        decode_mispredictions(&params, &mut input, &mut combined_decoder).unwrap();

    assert_eq!(contents.blocks.len(), recreated_blocks.len());
    contents
        .blocks
        .iter()
        .zip(recreated_blocks)
        .enumerate()
        .for_each(|(index, (a, b))| match (&a.block_type, &b.block_type) {
            (
                DeflateTokenBlockType::Stored { uncompressed: a },
                DeflateTokenBlockType::Stored { uncompressed: c },
            ) => {
                assert_eq!(a, c, "uncompressed data differs {index}");
            }
            (
                DeflateTokenBlockType::Huffman {
                    tokens: t1,
                    huffman_type: h1,
                },
                DeflateTokenBlockType::Huffman {
                    tokens: t2,
                    huffman_type: h2,
                },
            ) => {
                assert_eq_array(t1, t2);
                assert_eq!(h1, h2, "huffman type differs {index}");
            }
            _ => panic!("block type differs {index}"),
        });

    assert_eq!(
        recompressed.len(),
        compressed_data.len(),
        "re-compressed version should be same (length)"
    );
    assert!(
        &recompressed[..] == compressed_data,
        "re-compressed version should be same (content)"
    );

    let result_crc = crc32fast::hash(&plain_text.text());

    if let Some(crc) = header_crc32 {
        assert_eq!(crc, result_crc, "crc mismatch");
    }

    *uncompressed_size = plain_text.text().len() as u64;
}

#[cfg(test)]
fn do_analyze(crc: Option<u32>, compressed_data: &[u8]) {
    let mut uncompressed_size = 0;

    analyze_compressed_data_verify(compressed_data, crc, 1, &mut uncompressed_size);
    analyze_compressed_data_fast(compressed_data, crc, &mut uncompressed_size);
}

/// verify that levels 1-6 of zlib are compressed without any correction data
///
/// Future work: figure out why level 7 and above are not perfect
#[test]
fn verify_zlib_perfect_compression() {
    crate::init_logging();

    use crate::deflate::deflate_reader::parse_deflate_whole;
    use crate::utils::read_file;

    for i in 1..6 {
        println!("iteration {}", i);
        let compressed_data: &[u8] =
            &read_file(format!("compressed_zlib_level{i}.deflate").as_str());

        let compressed_data = compressed_data;

        let (contents, plain_text) = parse_deflate_whole(compressed_data).unwrap();

        let params = estimate_preflate_parameters(&contents, &plain_text).unwrap();

        println!("params: {:?}", params);

        // this "encoder" just asserts if anything gets passed to it
        let mut verify_encoder = crate::statistical_codec::AssertDefaultOnlyEncoder {};
        encode_mispredictions(&contents, &plain_text, &params, &mut verify_encoder).unwrap();

        println!("params buffer length {}", bitcode::encode(&params).len());
    }
}

#[test]
fn verify_longmatch() {
    crate::init_logging();

    use crate::utils::read_file;
    do_analyze(
        None,
        &read_file("compressed_flate2_level1_longmatch.deflate"),
    );
}

#[test]
fn verify_zlibng() {
    crate::init_logging();

    use crate::utils::read_file;

    do_analyze(None, &read_file("compressed_zlibng_level1.deflate"));
}

#[test]
fn verify_miniz() {
    crate::init_logging();

    use crate::utils::read_file;

    do_analyze(None, &read_file("compressed_minizoxide_level1.deflate"));
}

/// this is the deflate stream extracted out of the png file (minus the idat wrapper)
#[test]
fn verify_png_deflate() {
    crate::init_logging();

    use crate::utils::read_file;
    do_analyze(None, &read_file("treegdi.extract.deflate"));
}

#[cfg(test)]
pub fn analyze_compressed_data_verify_incremental(compressed_data: &[u8], config: &PreflateConfig) {
    use crate::{deflate::deflate_reader::parse_deflate_whole, utils::assert_eq_array};

    let (original_con, _) = parse_deflate_whole(compressed_data).unwrap();

    let mut start_offset = 0;
    let mut end_offset = compressed_data.len().min(100001);

    let mut stream = PreflateStreamProcessor::new(&config);

    let mut plain_text_offset = 0;

    let mut expanded_contents = Vec::new();
    while !stream.is_done() {
        let result = stream.decompress(&compressed_data[start_offset..end_offset]);
        match result {
            Ok(r) => {
                println!(
                    "chunk cmp_start={} cmp_size={} blocks={} pt_off={}({})",
                    start_offset,
                    r.compressed_size,
                    r.blocks.len(),
                    plain_text_offset,
                    stream.plain_text().len()
                );
                start_offset += r.compressed_size;
                end_offset = (start_offset + 10001).min(compressed_data.len());

                plain_text_offset += stream.plain_text().len();
                expanded_contents.push((r.corrections, stream.plain_text().text().to_vec()));

                stream.shrink_to_dictionary();
            }
            Err(e) => {
                if e.exit_code() == ExitCode::PredictionFailure {
                    println!(
                        "Prediction failure for {:?} not great, but some corner cases where the initial estimator isn't totaly right",
                        e
                    );
                    return;
                }
                assert_eq!(
                    e.exit_code(),
                    ExitCode::ShortRead,
                    "unexpected error {:?}",
                    e
                );
                end_offset = (end_offset + 10001).min(compressed_data.len());
            }
        }
    }

    // now reconstruct the data and make sure it is identical
    let mut recompressed = Vec::new();
    let mut reconstructed_blocks = Vec::new();

    let mut reconstruct = RecreateStreamProcessor::new();
    for i in 0..expanded_contents.len() {
        let (mut r, mut b) = reconstruct
            .recompress(
                &mut Cursor::new(&expanded_contents[i].1),
                &expanded_contents[i].0,
            )
            .unwrap();

        println!(
            "reconstruct block offset={} blocks={} pt={}",
            i,
            b.len(),
            expanded_contents[i].1.len()
        );

        recompressed.append(&mut r);
        reconstructed_blocks.append(&mut b);
    }

    //assert_eq!(original_con.blocks.len(), reconstructed_blocks.len());
    for i in 0..original_con.blocks.len() {
        println!("block {}", i);
        crate::utils::assert_block_eq(&original_con.blocks[i], &reconstructed_blocks[i]);
    }

    assert_eq_array(compressed_data, &recompressed);
}

#[test]
fn verify_plain_text_limit() {
    crate::init_logging();

    analyze_compressed_data_verify_incremental(
        &crate::utils::read_file("compressed_zlib_level3.deflate"),
        &PreflateConfig {
            plain_text_limit: 1 * 1024 * 1024,
            ..Default::default()
        },
    );
}

/// test partial reading reading
#[test]
fn verify_partial_blocks() {
    crate::init_logging();

    for i in 0..=9 {
        analyze_compressed_data_verify_incremental(
            &crate::utils::read_file(&format!("compressed_zlib_level{}.deflate", i)),
            &PreflateConfig::default(),
        );
    }
}

/// Replicates exactly what `scan_deflate::find_compressable_stream` does when it
/// encounters a gzip stream in a truncated 200 KiB content buffer:
///
/// 1. strip the 10-byte gzip header (and 8-byte footer) to obtain the raw DEFLATE body
/// 2. call `decompress` with only the first ~190 KB of that body (what fits in the
///    200 KiB content window after the header)
/// 3. assert the call returns `Ok` and `is_done() == false`  ← DeflateContinue requires this
/// 4. call `decompress` again with the remainder of the body, assert `is_done() == true`
/// 5. reconstruct the original DEFLATE bytes and assert roundtrip identity
#[test]
fn verify_decompress_partial_gzip_deflate_body_roundtrip() {
    crate::init_logging();

    // sample1.bin.gz: 263 972 bytes total; gzip header = 10 bytes, footer = 8 bytes
    // → raw DEFLATE body = 263 954 bytes
    let gzip = crate::utils::read_file("sample1.bin.gz");
    assert!(gzip.len() > 18, "gzip file too short");

    // Gzip header for this file has no extra flags, so it is exactly 10 bytes.
    let deflate_start: usize = 10;
    let deflate_end: usize = gzip.len() - 8;
    let deflate_body = &gzip[deflate_start..deflate_end];

    // Mimic what find_compressable_stream does: content buffer contains the first
    // 200 000 bytes of the gzip file, and the DEFLATE body starts at offset 10,
    // so the slice passed to decompress() is bytes [10..200000] = 199 990 bytes.
    let content_window: usize = 200_000 - deflate_start; // 199 990
    assert!(
        content_window < deflate_body.len(),
        "content window must truncate the DEFLATE body (window={content_window}, body={})",
        deflate_body.len()
    );

    let mut state = PreflateStreamProcessor::new(&PreflateConfig::default());

    // ── First call: truncated slice ──────────────────────────────────────────
    let r1 = state.decompress(&deflate_body[..content_window]);
    let r1 = match r1 {
        Ok(r) => r,
        Err(e) => panic!(
            "decompress on truncated DEFLATE body ({content_window} B of {} B) returned \
             Err({e:?}); expected Ok(partial) with is_done()=false",
            deflate_body.len()
        ),
    };
    assert!(
        !state.is_done(),
        "is_done() must be false after truncated first call; compressed_size={}",
        r1.compressed_size
    );
    assert!(
        r1.compressed_size > 0,
        "at least one block must have been consumed"
    );
    assert!(
        r1.compressed_size <= content_window,
        "compressed_size ({}) must not exceed the slice length ({content_window})",
        r1.compressed_size
    );
    println!(
        "first call: compressed_size={} / {content_window}  blocks={}",
        r1.compressed_size,
        r1.blocks.len()
    );

    let first_corrections = r1.corrections.clone();
    let first_plain_text = state.plain_text().text().to_vec();
    state.shrink_to_dictionary();

    // ── Second call: remainder ────────────────────────────────────────────────
    let offset = r1.compressed_size;
    let r2 = state.decompress(&deflate_body[offset..]);
    let r2 = match r2 {
        Ok(r) => r,
        Err(e) => panic!(
            "decompress on remainder ({} B) returned Err({e:?})",
            deflate_body.len() - offset
        ),
    };
    assert!(
        state.is_done(),
        "is_done() must be true after consuming the full body"
    );
    println!(
        "second call: compressed_size={}  blocks={}",
        r2.compressed_size,
        r2.blocks.len()
    );

    let second_corrections = r2.corrections.clone();
    let second_plain_text = state.plain_text().text().to_vec();

    // ── Roundtrip: reconstruct the original bytes ─────────────────────────────
    let mut reconstruct = RecreateStreamProcessor::new();
    let (mut recompressed, _) = reconstruct
        .recompress(
            &mut std::io::Cursor::new(&first_plain_text),
            &first_corrections,
        )
        .expect("recompress chunk 1 failed");

    let (mut rest, _) = reconstruct
        .recompress(
            &mut std::io::Cursor::new(&second_plain_text),
            &second_corrections,
        )
        .expect("recompress chunk 2 failed");

    recompressed.append(&mut rest);
    crate::utils::assert_eq_array(deflate_body, &recompressed);
}

/// LitePack (LITEPACK-CHANGES.md, change 6): writes the crafted record LitePack's tests use for
/// the hop limit. Run once, by hand, from this directory:
/// `LPK_CRAFTED_OUT=<dir> cargo test --lib litepack_crafted -- --ignored`
/// It builds a periodic plain text and a fixed-Huffman stream of it, analyses it, then encodes
/// the corrections with every `DistOnlyCorrection` replaced by `max_chain + 4000` hops (a deep
/// walk of the hash chain per token for upstream), and writes
/// `hops-plain.bin` and `hops-corrections.bin`; and `junk-prefix.bin` / `junk-suffix.bin`, the
/// bytes around the CABAC data of a record with the same parameters and 4 MiB of LCG bytes as that
/// data (the memory-term test rebuilds it).
#[cfg(test)]
mod litepack_crafted {
    use super::*;
    use crate::deflate::deflate_reader::parse_deflate_whole;
    use crate::deflate::deflate_token::{
        DeflateHuffmanType, DeflateToken, DeflateTokenBlockType, DeflateTokenReference,
    };

    struct Tamper<E> {
        inner: E,
        hops: u32,
    }

    impl<E: PredictionEncoder> PredictionEncoder for Tamper<E> {
        fn encode_correction(&mut self, action: CodecCorrection, value: u32) {
            if action == CodecCorrection::DistOnlyCorrection {
                self.inner.encode_correction(action, self.hops);
            } else {
                self.inner.encode_correction(action, value);
            }
        }
        fn encode_verify_state(&mut self, message: &'static str, checksum: u64) {
            self.inner.encode_verify_state(message, checksum);
        }
        fn finish(&mut self) {
            self.inner.finish();
        }
    }

    #[test]
    #[ignore = "writes the crafted record (LPK_CRAFTED_OUT)"]
    fn write_crafted_hops_record() {
        let out_dir = std::path::PathBuf::from(std::env::var("LPK_CRAFTED_OUT").unwrap());
        let period = b"abcdefgh";
        let mut tokens: Vec<DeflateToken> =
            period.iter().map(|&c| DeflateToken::Literal(c)).collect();
        let mut plain: Vec<u8> = period.to_vec();
        for _ in 0..800 {
            tokens.push(DeflateToken::Reference(DeflateTokenReference::new(258, 8)));
            for _ in 0..258 {
                plain.push(plain[plain.len() - 8]);
            }
        }
        let block = DeflateTokenBlock {
            block_type: DeflateTokenBlockType::Huffman {
                tokens,
                huffman_type: DeflateHuffmanType::Static,
            },
            last: true,
        };
        let mut w = DeflateWriter::new();
        w.encode_block(&block).unwrap();
        w.flush();
        let deflate = w.detach_output();

        let (contents, plain_text) = parse_deflate_whole(&deflate).unwrap();
        assert_eq!(plain_text.text(), plain.as_slice());
        let params = estimate_preflate_parameters(&contents, &plain_text).unwrap();

        let mut cabac = Vec::new();
        {
            let mut enc = Tamper {
                inner: PredictionEncoderCabac::new(VPXBoolWriter::new(&mut cabac).unwrap()),
                hops: params.max_chain + 4000,
            };
            let mut input = PreflateInput::new(&plain_text);
            let mut tp = TokenPredictor::new(&params);
            predict_blocks(&contents.blocks, &mut tp, &mut enc, &mut input).unwrap();
            enc.finish();
        }
        let record = bitcode::encode(&ReconstructionData {
            parameters: params,
            corrections: cabac,
        });
        // A second record: valid parameters and JUNK_LEN pseudo-random bytes as the CABAC data
        // (x = x * 1664525 + 1013904223 from x = 1, the byte is x >> 24), written as the bytes
        // before and after them (the tests rebuild the junk).
        const JUNK_LEN: usize = 4 << 20;
        let mut x: u32 = 1;
        let data: Vec<u8> = (0..JUNK_LEN)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 24) as u8
            })
            .collect();
        let junk = bitcode::encode(&ReconstructionData {
            parameters: params,
            corrections: data.clone(),
        });
        let at = junk.windows(64).position(|w| w == &data[..64]).unwrap();
        assert_eq!(&junk[at..at + JUNK_LEN], data.as_slice());
        std::fs::write(out_dir.join("junk-prefix.bin"), &junk[..at]).unwrap();
        std::fs::write(out_dir.join("junk-suffix.bin"), &junk[at + JUNK_LEN..]).unwrap();

        std::fs::write(out_dir.join("hops-plain.bin"), &plain).unwrap();
        std::fs::write(out_dir.join("hops-corrections.bin"), &record).unwrap();
        println!(
            "deflate {} bytes, plain {} bytes, record {} bytes, max_chain {}",
            deflate.len(),
            plain.len(),
            record.len(),
            params.max_chain
        );
    }
}
