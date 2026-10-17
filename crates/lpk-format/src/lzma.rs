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
    /// A step that is not the last of its graph has only a bound, not a
    /// length, so its stream must end with the end-of-payload marker: a
    /// marker-less stream that reaches the bound is `LzmaError` "marker
    /// required" (spec section 8).
    fn decode_step(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        if last {
            return self.decode(params, input, expected_len, limits);
        }
        let (props, buffer) = setup(params, input, expected_len, limits)?;
        match pass(props, buffer, None, input, expected_len, limits) {
            Ok((out, _)) => {
                // lzma-rs also ends a marker-less stream whose input runs out
                // cleanly. Decoded again by the length it gave, a stream with
                // the marker leaves the marker's bytes unread; one without it
                // consumes everything.
                let n = out.len() as u64;
                match pass(props, buffer, Some(n), input, expected_len, limits) {
                    Ok((again, used)) if again.len() as u64 == n && used == input.len() as u64 => {
                        Err(lerr(if n == expected_len {
                            "marker required"
                        } else {
                            "truncated"
                        }))
                    }
                    _ => Ok(out),
                }
            }
            Err(e @ FormatError::PayloadTooLarge { .. }) => Err(e),
            Err(e) => match pass(
                props,
                buffer,
                Some(expected_len),
                input,
                expected_len,
                limits,
            ) {
                Ok((out, used))
                    if out.len() as u64 == expected_len && used == input.len() as u64 =>
                {
                    Err(lerr("marker required"))
                }
                // lzma-rs hands its buffer to the sink only when the buffer is full or at
                // the end, so the first pass may meet an error past the bound before it sees
                // the crossing. Decoded again to stop at exactly one byte past the bound, a
                // stream that gets there crossed the bound before any later error, which is
                // `PayloadTooLarge` (spec section 8, lzma rules 3 and 6); an error at an
                // earlier position keeps its class. The byte past the bound cannot fail on
                // its distance (it is at most the bytes produced, which fit the buffer).
                _ => match pass(
                    props,
                    buffer,
                    Some(expected_len.saturating_add(1)),
                    input,
                    expected_len,
                    limits,
                ) {
                    // (A marker before the bound reads as a short unpacked size: not a crossing.)
                    Err(p @ FormatError::PayloadTooLarge { len, .. }) if len > expected_len => {
                        Err(p)
                    }
                    _ => Err(e),
                },
            },
        }
    }

    fn decode(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        let (props, buffer) = setup(params, input, expected_len, limits)?;
        // First as a stream that ends with the end-of-payload marker (what
        // liblzma writes); a stream without one fails that pass and is
        // decoded again by its known size.
        let first = pass(props, buffer, None, input, expected_len, limits);
        let short = first.is_ok();
        if let Ok((out, _)) = first {
            if out.len() as u64 == expected_len {
                return Ok(out);
            }
        }
        match pass(
            props,
            buffer,
            Some(expected_len),
            input,
            expected_len,
            limits,
        ) {
            // The marker came before the bound: the first pass saw it too.
            Err(FormatError::PayloadTooLarge { .. }) if short => Err(lerr("truncated")),
            Err(e) => Err(e),
            Ok((out, _)) if (out.len() as u64) < expected_len => Err(lerr("truncated")),
            Ok((_, used)) if used < input.len() as u64 => Err(lerr("trailing input")),
            Ok((out, _)) => Ok(out),
        }
    }
}

/// Validate the parameters (in the spec's order) and the first byte, and
/// size the dictionary buffer.
fn setup(
    params: &[u8],
    input: &[u8],
    expected_len: u64,
    limits: &Resources,
) -> Result<(LzmaProperties, u32), FormatError> {
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
    // The buffer is the dictionary, capped by the output: lzma-rs refuses
    // a distance beyond the buffer, so exactly the distances beyond
    // `dict_size` or beyond the bytes produced are refused, whatever the
    // reader's `max_window` is (`dict_size` <= `max_window` was checked).
    let buffer = expected_len.min(u64::from(dict_size)).max(1);
    let buffer = u32::try_from(buffer).map_err(|_| lerr("dictionary"))?;
    // The first byte of a range coder stream is always 0.
    if input.first().is_some_and(|&b| b != 0) {
        return Err(lerr("range coder"));
    }
    let props = LzmaProperties {
        lc: u32::from(lc),
        lp: u32::from(lp),
        pb: u32::from(pb),
    };
    Ok((props, buffer))
}

/// One decoding pass: with `size` the stream ends at that many bytes, without
/// it at the end-of-payload marker (which must then be the last input).
fn pass(
    props: LzmaProperties,
    buffer: u32,
    size: Option<u64>,
    input: &[u8],
    expected_len: u64,
    limits: &Resources,
) -> Result<(Vec<u8>, u64), FormatError> {
    let mem = usize::try_from(buffer).map_err(|_| lerr("dictionary"))?;
    let mut inner = Inner::new(LzmaParams::new(props, buffer, size), Some(mem))
        .map_err(|e| lerr(e.to_string()))?;
    let mut src = Cursor::new(input);
    // Sized once to the block (bounded by the reader's block limit), so the output never
    // doubles past it: the decoder holds the block plus its dictionary buffer (E2-20).
    let reserve = usize::try_from(expected_len.min(limits.max_block_plain)).unwrap_or(0);
    let mut sink = Bounded {
        out: Vec::with_capacity(reserve),
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
        // Only the over-length case is read from the text: lzma-rs checks its
        // size after a match crossed the bound and says so in words. A marker
        // before the bound gives the same words; the caller tells the two
        // apart by the first pass.
        let text = e.to_string();
        if text.starts_with("lzma error: Expected unpacked size") {
            let len = text
                .rsplit("decompressed to ")
                .next()
                .and_then(|t| t.trim().parse::<u64>().ok())
                .unwrap_or(expected_len.saturating_add(1));
            return Err(FormatError::PayloadTooLarge {
                len,
                max: expected_len,
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
