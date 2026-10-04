//! The decode graph, the `ChunkData` block header and the decoders (section 8).

use std::collections::HashMap;
use std::io::Write;

use crate::error::{Error, Result};
use crate::wire::{read_varint, Cursor};

/// A primitive of the registry with its parsed parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prim {
    /// 0x0000.
    Store,
    /// 0x0001.
    Zstd {
        /// log2 of the window.
        window_log: u8,
        /// Prior id, all zeros = none.
        dictionary: [u8; 32],
    },
    /// 0x0002.
    Lzma {
        /// Dictionary size.
        dict_size: u32,
        /// Literal context bits.
        lc: u8,
        /// Literal position bits.
        lp: u8,
        /// Position bits.
        pb: u8,
    },
    /// A known primitive this decoder does not run (bwt, bcj, delta, reconstruction).
    Unimplemented(u16),
}

impl Prim {
    /// The window this step needs.
    pub fn window(&self) -> u64 {
        match self {
            Self::Zstd { window_log, .. } => 1u64 << window_log,
            Self::Lzma { dict_size, .. } => u64::from(*dict_size),
            _ => 0,
        }
    }
}

/// The parsed block header.
#[derive(Debug, Clone)]
pub struct BlockHeader<'a> {
    /// The steps in order of application.
    pub steps: Vec<Prim>,
    /// BWT block sizes the graph names (for the envelope check).
    pub bwt_block: u64,
    /// The block's plain length.
    pub plain_len: u64,
    /// The encoded bytes.
    pub encoded: &'a [u8],
}

fn bad_params(id: u16, reason: &str) -> Error {
    Error::new(
        "BadParams",
        format!("bad parameters for primitive {id:#06x}: {reason}"),
    )
}

/// Parses the block header of a `ChunkData` payload; `record_count` bounds the record ids.
pub fn parse_block(payload: &[u8], record_count: u64) -> Result<BlockHeader<'_>> {
    let mut c = Cursor::new(payload, "graph");
    let n = c.varint()?;
    if n == 0 || n > 16 {
        return Err(Error::new("BadGraph", "bad graph: step count"));
    }
    let mut steps = Vec::with_capacity(n as usize);
    let mut bwt_block = 0u64;
    for _ in 0..n {
        let id = c.u16()?;
        if id > 0x000C {
            return Err(Error::new(
                "UnknownPrimitive",
                format!("unknown primitive {id:#06x}"),
            ));
        }
        let flags = c.u8()?;
        if flags != 0 {
            return Err(Error::new("BadGraph", "bad graph: step flags"));
        }
        let plen = c.varint()?;
        if plen > 256 {
            return Err(Error::new("BadGraph", "bad graph: params length"));
        }
        let p = c.bytes(plen as usize)?;
        let need = |len: usize| -> Result<()> {
            if p.len() == len {
                Ok(())
            } else {
                Err(bad_params(id, "params length"))
            }
        };
        let prim = match id {
            0x0000 => {
                need(0)?;
                Prim::Store
            }
            0x0001 => {
                need(33)?;
                let window_log = p[0];
                if !(10..=31).contains(&window_log) {
                    return Err(bad_params(id, "window_log"));
                }
                let mut dictionary = [0u8; 32];
                dictionary.copy_from_slice(&p[1..33]);
                Prim::Zstd {
                    window_log,
                    dictionary,
                }
            }
            0x0002 => {
                need(7)?;
                let dict_size = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                let (lc, lp, pb) = (p[4], p[5], p[6]);
                if lc > 8 {
                    return Err(bad_params(id, "lc"));
                }
                if lp > 4 {
                    return Err(bad_params(id, "lp"));
                }
                if pb > 4 {
                    return Err(bad_params(id, "pb"));
                }
                if lc + lp > 4 {
                    return Err(bad_params(id, "lc + lp"));
                }
                Prim::Lzma {
                    dict_size,
                    lc,
                    lp,
                    pb,
                }
            }
            0x0003 => {
                need(4)?;
                let bs = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                if bs == 0 {
                    return Err(bad_params(id, "block_size"));
                }
                bwt_block = bwt_block.max(u64::from(bs));
                Prim::Unimplemented(id)
            }
            0x0004 | 0x0005 => {
                need(0)?;
                Prim::Unimplemented(id)
            }
            0x0006 => {
                need(9)?;
                if p[8] > 1 {
                    return Err(bad_params(id, "patch_format"));
                }
                Prim::Unimplemented(id)
            }
            _ => {
                // 7..=12: one canonical varint, the record id.
                let mut pos = 0;
                let rid =
                    read_varint(p, &mut pos, "params").map_err(|_| bad_params(id, "record_id"))?;
                if pos != p.len() {
                    return Err(bad_params(id, "record_id"));
                }
                if rid >= record_count {
                    return Err(Error::new(
                        "RecordOutOfRange",
                        format!("record {rid} out of range (count {record_count})"),
                    ));
                }
                Prim::Unimplemented(id)
            }
        };
        steps.push(prim);
    }
    c.set_what("block header");
    let plain_len = c.varint()?;
    let encoded_len = c.varint()?;
    if encoded_len != c.remaining() as u64 {
        return Err(Error::new(
            "BlockLengthMismatch",
            "encoded_len differs from the bytes present",
        ));
    }
    let encoded = c.bytes(c.remaining())?;
    Ok(BlockHeader {
        steps,
        bwt_block,
        plain_len,
        encoded,
    })
}

/// What decoding a block needs besides its bytes.
#[derive(Debug)]
pub struct DecodeCtx<'a> {
    /// The caller's prior store, by BLAKE3 id.
    pub priors: &'a HashMap<[u8; 32], Vec<u8>>,
    /// The reader's `max_window`.
    pub max_window: u64,
    /// The reader's `max_block_plain`.
    pub max_block_plain: u64,
}

fn too_large(last: bool, what: &str) -> Error {
    if last {
        Error::new(
            "BlockLengthMismatch",
            format!("{what}: output longer than plain_len"),
        )
    } else {
        Error::new(
            "PayloadTooLarge",
            format!("{what}: output above max_block_plain"),
        )
    }
}

/// Decodes a block whose header has been checked; returns exactly `plain_len` bytes.
pub fn decode(h: &BlockHeader<'_>, ctx: &DecodeCtx<'_>) -> Result<Vec<u8>> {
    if h.plain_len > ctx.max_block_plain {
        return Err(Error::new(
            "PayloadTooLarge",
            "block plain_len above max_block_plain",
        ));
    }
    // Check the whole graph before any step runs.
    for s in &h.steps {
        if let Prim::Unimplemented(id) = s {
            return Err(Error::new(
                "UnimplementedPrimitive",
                format!("primitive {id:#06x} has no decoder here"),
            ));
        }
    }
    let mut data: Vec<u8> = h.encoded.to_vec();
    let last_i = h.steps.len() - 1;
    for (i, s) in h.steps.iter().enumerate() {
        let last = i == last_i;
        let bound = if last {
            h.plain_len
        } else {
            ctx.max_block_plain
        };
        data = match s {
            Prim::Store => {
                if data.len() as u64 > bound {
                    return Err(too_large(last, "store"));
                }
                data
            }
            Prim::Zstd {
                window_log,
                dictionary,
            } => zstd(&data, *window_log, dictionary, bound, last, ctx)?,
            Prim::Lzma {
                dict_size,
                lc,
                lp,
                pb,
            } => lzma(&data, *dict_size, *lc, *lp, *pb, bound, last, ctx)?,
            Prim::Unimplemented(_) => return Err(Error::new("Internal", "unreachable")),
        };
    }
    if data.len() as u64 != h.plain_len {
        return Err(Error::new(
            "BlockLengthMismatch",
            format!(
                "block decoded to {} bytes, plain_len {}",
                data.len(),
                h.plain_len
            ),
        ));
    }
    Ok(data)
}

fn zstd(
    input: &[u8],
    window_log: u8,
    dictionary: &[u8; 32],
    bound: u64,
    last: bool,
    ctx: &DecodeCtx<'_>,
) -> Result<Vec<u8>> {
    use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
    use ruzstd::decoding::{BlockDecodingStrategy, Dictionary, FrameDecoder};
    let zerr = |e: &dyn std::fmt::Display| Error::new("ZstdError", format!("zstd: {e}"));
    let window = 1u64 << window_log;
    if window > ctx.max_window {
        return Err(Error::new(
            "WindowTooLarge",
            format!("window {window} needed, {} allowed", ctx.max_window),
        ));
    }
    let mut dec = FrameDecoder::new();
    dec.set_max_window_size(window);
    let mut dict_id = None;
    if *dictionary != [0u8; 32] {
        let prior = ctx
            .priors
            .get(dictionary)
            .filter(|p| blake3::hash(p).as_bytes() == dictionary)
            .ok_or_else(|| Error::new("MissingPrior", "a prior the block names is missing"))?;
        let d = Dictionary::decode_dict(prior).map_err(|e| zerr(&e))?;
        dict_id = Some(d.id);
        dec.add_dict(d).map_err(|e| zerr(&e))?;
    }
    let mut out: Vec<u8> = Vec::new();
    let mut src = input;
    while !src.is_empty() {
        match dec.reset(&mut src) {
            Ok(()) => {}
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                length,
                ..
            })) => {
                src = src
                    .get(length as usize..)
                    .ok_or_else(|| Error::new("ZstdError", "zstd: skippable frame cut short"))?;
                continue;
            }
            Err(FrameDecoderError::WindowSizeTooBig { .. }) => {
                return Err(bad_params(0x0001, "frame window exceeds declared"))
            }
            Err(e) => return Err(zerr(&e)),
        }
        if let Some(id) = dict_id {
            dec.force_dict(id).map_err(|e| zerr(&e))?;
        }
        loop {
            dec.decode_blocks(&mut src, BlockDecodingStrategy::UptoBytes(64 * 1024))
                .map_err(|e| zerr(&e))?;
            if let Some(v) = dec.collect() {
                out.extend_from_slice(&v);
            }
            if out.len() as u64 > bound {
                return Err(too_large(last, "zstd"));
            }
            if dec.is_finished() {
                if let Some(v) = dec.collect() {
                    out.extend_from_slice(&v);
                }
                if out.len() as u64 > bound {
                    return Err(too_large(last, "zstd"));
                }
                break;
            }
        }
        if let (Some(a), Some(b)) = (dec.get_checksum_from_data(), dec.get_calculated_checksum()) {
            if a != b {
                return Err(Error::new("ZstdError", "zstd: content checksum mismatch"));
            }
        }
    }
    Ok(out)
}

/// A writer that refuses to grow past a bound.
struct Bounded {
    buf: Vec<u8>,
    limit: u64,
    overflow: bool,
}

impl Write for Bounded {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() as u64 + b.len() as u64 > self.limit {
            self.overflow = true;
            return Err(std::io::Error::other("output bound"));
        }
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn lzma(
    input: &[u8],
    dict_size: u32,
    lc: u8,
    lp: u8,
    pb: u8,
    bound: u64,
    last: bool,
    ctx: &DecodeCtx<'_>,
) -> Result<Vec<u8>> {
    use lzma_rs::decompress::{Options, UnpackedSize};
    if u64::from(dict_size) > ctx.max_window {
        return Err(Error::new(
            "WindowTooLarge",
            format!("dictionary {dict_size} needed, {} allowed", ctx.max_window),
        ));
    }
    match input.first() {
        None => return Err(Error::new("LzmaError", "lzma: truncated")),
        Some(0) => {}
        Some(_) => return Err(Error::new("LzmaError", "lzma: range coder")),
    }
    // lzma-rs reads a `.lzma` header; build one with the step's properties and no size field.
    let props = (pb * 5 + lp) * 9 + lc;
    let mut stream = Vec::with_capacity(input.len() + 5);
    stream.push(props);
    stream.extend_from_slice(&dict_size.to_le_bytes());
    stream.extend_from_slice(input);
    let run = |size: Option<u64>| -> (std::result::Result<(), String>, Bounded, u64) {
        let mut w = Bounded {
            buf: Vec::new(),
            limit: bound,
            overflow: false,
        };
        let opts = Options {
            unpacked_size: UnpackedSize::UseProvided(size),
            memlimit: None,
            allow_incomplete: false,
        };
        let mut rd = std::io::Cursor::new(&stream[..]);
        let r = lzma_rs::lzma_decompress_with_options(&mut rd, &mut w, &opts)
            .map_err(|e| e.to_string());
        let consumed = rd.position();
        (r, w, consumed)
    };
    // With the end-of-payload marker (what liblzma writes).
    let (r, w, _) = run(None);
    if r.is_ok() && !w.overflow {
        return Ok(w.buf);
    }
    // Without the marker: stop at the output bound; all input must be used.
    let (r2, w2, consumed) = run(Some(bound));
    if w2.overflow {
        return Err(too_large(last, "lzma"));
    }
    match r2 {
        Ok(()) if consumed == stream.len() as u64 => Ok(w2.buf),
        Ok(()) => Err(Error::new("LzmaError", "lzma: trailing input")),
        Err(e) => Err(Error::new("LzmaError", format!("lzma: {e}"))),
    }
}
