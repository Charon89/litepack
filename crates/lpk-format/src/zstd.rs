//! The `zstd` primitive (ID 1) decoded in pure Rust with `ruzstd` (spec
//! section 8). Window and prior rules are enforced here, before any input
//! is decoded.

use crate::decode::PrimitiveDecoder;
use crate::envelope::Resources;
use crate::error::FormatError;
use crate::priors::{prior_id, NoPriors, PriorStore};
use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, Dictionary, FrameDecoder};
use std::sync::Arc;

const ID: u16 = 1;
/// Decoding goes on in steps of about this many output bytes.
const STEP: usize = 64 * 1024;

fn zerr(e: impl std::fmt::Display) -> FormatError {
    FormatError::ZstdError {
        reason: e.to_string(),
    }
}

/// Decodes a zstd frame, or a sequence of them, into at most `expected_len`
/// bytes, with the priors of its store.
#[derive(Clone)]
pub struct ZstdDecoder {
    priors: Arc<dyn PriorStore>,
}

impl std::fmt::Debug for ZstdDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ZstdDecoder")
    }
}

impl Default for ZstdDecoder {
    fn default() -> Self {
        ZstdDecoder::new(Arc::new(NoPriors))
    }
}

impl ZstdDecoder {
    /// A decoder that looks priors up in `priors`.
    pub fn new(priors: Arc<dyn PriorStore>) -> Self {
        ZstdDecoder { priors }
    }

    /// The dictionary a step names, checked against its ID, or `None`.
    fn dictionary(&self, id: &[u8; 32]) -> Result<Option<Dictionary>, FormatError> {
        if id.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        let bytes = self
            .priors
            .get(id)
            .filter(|b| prior_id(b) == *id)
            .ok_or(FormatError::MissingPrior { id: *id })?;
        Dictionary::decode_dict(bytes).map(Some).map_err(zerr)
    }
}

/// Map a frame decoder error: a window above the declared one is a parameter
/// error, everything else is the decoder's text.
fn map_frame_error(e: FrameDecoderError) -> FormatError {
    match e {
        FrameDecoderError::WindowSizeTooBig { .. } => FormatError::BadParams {
            id: ID,
            reason: "frame window exceeds declared",
        },
        other => zerr(other),
    }
}

/// Move what the decoder can hand out into `out`, refusing to pass `max`.
fn drain(dec: &mut FrameDecoder, out: &mut Vec<u8>, max: u64) -> Result<(), FormatError> {
    while dec.can_collect() > 0 {
        let len = (out.len() as u64).saturating_add(dec.can_collect() as u64);
        if len > max {
            return Err(FormatError::PayloadTooLarge { len, max });
        }
        match dec.collect() {
            Some(chunk) if !chunk.is_empty() => out.extend_from_slice(&chunk),
            _ => break,
        }
    }
    Ok(())
}

impl PrimitiveDecoder for ZstdDecoder {
    fn decode(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        let bad = |reason| FormatError::BadParams { id: ID, reason };
        let (&window_log, rest) = params.split_first().ok_or_else(|| bad("length"))?;
        let dict: [u8; 32] = rest.try_into().map_err(|_| bad("length"))?;
        if !(10..=31).contains(&window_log) {
            return Err(bad("window_log"));
        }
        let declared = 1u64 << window_log;
        if declared > limits.max_window {
            return Err(FormatError::WindowTooLarge {
                needed: declared,
                allowed: limits.max_window,
            });
        }
        let dictionary = self.dictionary(&dict)?;
        let dict_key = dictionary.as_ref().map(|d| d.id);

        let mut dec = FrameDecoder::new();
        dec.set_max_window_size(declared);
        if let Some(d) = dictionary {
            dec.add_dict(d).map_err(map_frame_error)?;
        }
        let mut src = input;
        let mut out: Vec<u8> = Vec::new();
        while !src.is_empty() {
            match dec.init(&mut src) {
                Ok(()) => {}
                Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                    length,
                    ..
                })) => {
                    src = src
                        .get(length as usize..)
                        .ok_or_else(|| zerr("skippable frame is cut short"))?;
                    continue;
                }
                Err(e) => return Err(map_frame_error(e)),
            }
            if let Some(id) = dict_key {
                dec.force_dict(id).map_err(map_frame_error)?;
            }
            loop {
                dec.decode_blocks(&mut src, BlockDecodingStrategy::UptoBytes(STEP))
                    .map_err(map_frame_error)?;
                drain(&mut dec, &mut out, expected_len)?;
                if dec.is_finished() {
                    break;
                }
            }
            if let (Some(stored), Some(computed)) =
                (dec.get_checksum_from_data(), dec.get_calculated_checksum())
            {
                if stored != computed {
                    return Err(zerr("content checksum mismatch"));
                }
            }
        }
        Ok(out)
    }
}
