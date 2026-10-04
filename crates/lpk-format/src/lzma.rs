//! The `lzma` primitive (ID 2) decoded in pure Rust with `lzma-rs` (spec
//! section 8). The encoded bytes are a raw LZMA1 stream: no header, and the
//! decoder stops at exactly `expected_len` output bytes. The dictionary rule
//! is enforced here, before any input is read.

use crate::decode::PrimitiveDecoder;
use crate::envelope::Resources;
use crate::error::FormatError;
use lzma_rs::decompress::raw::{LzmaDecoder as Inner, LzmaParams, LzmaProperties};
use std::io::{self, Cursor, Write};

const ID: u16 = 2;
/// The smallest dictionary buffer asked of the decoder.
const MIN_DICT: u64 = 4096;

fn lerr(reason: impl Into<String>) -> FormatError {
    FormatError::LzmaError {
        reason: reason.into(),
    }
}

/// An output sink that refuses to pass `max` bytes.
struct Bounded {
    out: Vec<u8>,
    max: u64,
    over: Option<u64>,
}

impl Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = (self.out.len() as u64).saturating_add(buf.len() as u64);
        if len > self.max {
            self.over = Some(len);
            return Err(io::Error::other("output exceeds expected length"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Decodes a raw LZMA1 stream into exactly `expected_len` bytes.
#[derive(Debug, Clone, Copy, Default)]
pub struct LzmaDecoder;

impl PrimitiveDecoder for LzmaDecoder {
    fn decode(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        let bad = |reason| FormatError::BadParams { id: ID, reason };
        let params: [u8; 7] = params.try_into().map_err(|_| bad("length"))?;
        let dict_size = u32::from_le_bytes([params[0], params[1], params[2], params[3]]);
        let (lc, lp, pb) = (params[4], params[5], params[6]);
        if lc > 8 {
            return Err(bad("lc"));
        }
        if lp > 4 {
            return Err(bad("lp"));
        }
        if pb > 4 {
            return Err(bad("pb"));
        }
        if lc + lp > 4 {
            return Err(bad("lc + lp"));
        }
        if u64::from(dict_size) > limits.max_window {
            return Err(FormatError::WindowTooLarge {
                needed: u64::from(dict_size),
                allowed: limits.max_window,
            });
        }
        // No distance can reach back past the output, so the buffer need not
        // be larger than it; never beyond the declared dictionary either.
        let buffer = expected_len
            .min(u64::from(dict_size))
            .max(MIN_DICT)
            .min(limits.max_window.max(1));
        let buffer = u32::try_from(buffer).map_err(|_| lerr("dictionary"))?;
        let props = LzmaProperties {
            lc: u32::from(lc),
            lp: u32::from(lp),
            pb: u32::from(pb),
        };
        // First as a stream that ends with the end-of-payload marker (what
        // liblzma writes); a stream without one fails that pass and is
        // decoded again by its known size.
        if let Ok(out) = run(props, buffer, None, input, expected_len) {
            if out.len() as u64 == expected_len {
                return Ok(out);
            }
        }
        let (out, used) = run_sized(props, buffer, input, expected_len)?;
        if (out.len() as u64) < expected_len {
            return Err(lerr("truncated"));
        }
        if used < input.len() as u64 {
            return Err(lerr("trailing input"));
        }
        Ok(out)
    }
}

/// One decoding pass, ignoring how far the input was used.
fn run(
    props: LzmaProperties,
    buffer: u32,
    size: Option<u64>,
    input: &[u8],
    expected_len: u64,
) -> Result<Vec<u8>, FormatError> {
    pass(props, buffer, size, input, expected_len).map(|(o, _)| o)
}

fn run_sized(
    props: LzmaProperties,
    buffer: u32,
    input: &[u8],
    expected_len: u64,
) -> Result<(Vec<u8>, u64), FormatError> {
    pass(props, buffer, Some(expected_len), input, expected_len)
}

fn pass(
    props: LzmaProperties,
    buffer: u32,
    size: Option<u64>,
    input: &[u8],
    expected_len: u64,
) -> Result<(Vec<u8>, u64), FormatError> {
    let mem = usize::try_from(buffer).map_err(|_| lerr("dictionary"))?;
    let mut inner = Inner::new(LzmaParams::new(props, buffer, size), Some(mem))
        .map_err(|e| lerr(e.to_string()))?;
    let mut src = Cursor::new(input);
    let mut sink = Bounded {
        out: Vec::new(),
        max: expected_len,
        over: None,
    };
    let res = inner.decompress(&mut src, &mut sink);
    if let Some(len) = sink.over {
        return Err(FormatError::PayloadTooLarge {
            len,
            max: expected_len,
        });
    }
    if let Err(e) = res {
        // `lzma-rs` itself reports a stream that ends off its size.
        let text = e.to_string();
        if let Some(got) = text
            .split("decompressed to ")
            .nth(1)
            .and_then(|t| t.trim().parse::<u64>().ok())
        {
            return Err(if got > expected_len {
                FormatError::PayloadTooLarge {
                    len: got,
                    max: expected_len,
                }
            } else {
                lerr("truncated")
            });
        }
        return Err(lerr(map_reason(&e)));
    }
    let used = src.position();
    if size.is_none() && used < input.len() as u64 {
        return Err(lerr("trailing input"));
    }
    Ok((sink.out, used))
}

/// A short reason for a `lzma-rs` error: running out of input is "truncated".
fn map_reason(e: &lzma_rs::error::Error) -> String {
    use lzma_rs::error::Error;
    match e {
        Error::IoError(io) if io.kind() == io::ErrorKind::UnexpectedEof => "truncated".into(),
        Error::HeaderTooShort(_) => "truncated".into(),
        other => {
            let t = other.to_string();
            if t.contains("too short") || t.contains("UnexpectedEof") {
                "truncated".into()
            } else {
                t
            }
        }
    }
}
