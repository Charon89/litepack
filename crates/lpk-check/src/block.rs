//! The decode graph, the `ChunkData` block header and the decoders (section 8).

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::record::DeflateRecord;
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
    /// 0x0003 `bwt` (revision 1.3).
    Bwt {
        /// The length of its BWT blocks (not 0).
        block_size: u32,
    },
    /// 0x0007 `jpeg-reconstruct` (revision 1.1).
    Jpeg {
        /// The record in the `Records` frame.
        record_id: u64,
    },
    /// 0x0008 `deflate-reconstruct` (revision 1.2).
    Deflate {
        /// The record in the `Records` frame.
        record_id: u64,
    },
    /// A known primitive this decoder does not run (bwt, bcj, delta, the other reconstructions).
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
    /// Whether the graph names a reconstruction primitive (7 to 12).
    pub reconstruction: bool,
    /// The record ids the graph names.
    pub record_ids: Vec<u64>,
}

fn bad_params(id: u16, reason: &str) -> Error {
    Error::new(
        "BadParams",
        format!("bad parameters for primitive {id:#06x}: {reason}"),
    )
}

/// Parses the block header of a `ChunkData` payload; `record_count` bounds the record ids.
pub fn parse_block<'a>(
    payload: &'a [u8],
    record_count: &mut dyn FnMut() -> Result<u64>,
) -> Result<BlockHeader<'a>> {
    let mut c = Cursor::new(payload, "graph");
    let n = c.varint()?;
    if n == 0 || n > 16 {
        return Err(Error::new("BadGraph", "bad graph: step count"));
    }
    let mut steps = Vec::with_capacity(n as usize);
    let mut bwt_block = 0u64;
    let mut record_ids = Vec::new();
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
                Err(bad_params(id, "length"))
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
                Prim::Bwt { block_size: bs }
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
                record_ids.push(rid);
                match id {
                    0x0007 => Prim::Jpeg { record_id: rid },
                    0x0008 => Prim::Deflate { record_id: rid },
                    _ => Prim::Unimplemented(id),
                }
            }
        };
        steps.push(prim);
    }
    // Section 8 order step 3: record ids after the whole graph, before the block header fields.
    if !record_ids.is_empty() {
        let count = record_count()?;
        if let Some(rid) = record_ids.iter().find(|r| **r >= count) {
            return Err(Error::new(
                "RecordOutOfRange",
                format!("record {rid} out of range (count {count})"),
            ));
        }
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
        reconstruction: !record_ids.is_empty(),
        record_ids,
    })
}

/// What decoding a block needs besides its bytes.
#[derive(Debug)]
pub struct DecodeCtx<'a> {
    /// The caller's prior store, by BLAKE3 id.
    pub priors: &'a HashMap<[u8; 32], Vec<u8>>,
    /// The reader's `max_window`.
    pub max_window: u64,
    /// The envelope's `max_block_plain` (bounds every intermediate output, section 8).
    pub max_block_plain: u64,
    /// The revision this reader emulates (its minor): 0 runs `store`, `zstd` and `lzma`; 1 adds
    /// `jpeg-reconstruct`; 2 adds `deflate-reconstruct` and the bound of the step before it;
    /// 3 adds `bwt`, the bound I(O, B) of the step before it and the look-ahead.
    pub revision: u8,
    /// What the memory checks compare a step's term against (`deflate-reconstruct` check (5),
    /// `bwt` check (1)).
    pub memory: crate::deflate::Memory,
}

/// The reconstruction steps, which need the archive (its records and chunks).
pub trait Recon {
    /// A `jpeg-reconstruct` step: record id, the step's input, its output bound, whether it is
    /// the last step; returns the step's output (the primary image).
    fn jpeg(&mut self, id: u64, input: &[u8], bound: u64, last: bool) -> Result<Vec<u8>>;
    /// Checks (1) to (5) of `deflate-reconstruct` on record `id`, with a step's output bound and
    /// whether that step is the last; returns the record.
    fn deflate_record(&mut self, id: u64, bound: u64, last: bool) -> Result<DeflateRecord>;
}

/// The `UnimplementedPrimitive` error.
pub fn unimplemented(id: u16) -> Error {
    Error::new(
        "UnimplementedPrimitive",
        format!("primitive {id:#06x} is not implemented by this reader"),
    )
}

/// An output of `len` bytes past a step's bound `max`: `BlockLengthMismatch` on the last step,
/// `PayloadTooLarge` otherwise.
pub fn too_large(last: bool, len: u64, max: u64) -> Error {
    if last {
        Error::new(
            "BlockLengthMismatch",
            format!("block output of {len} bytes or more, plain_len {max}"),
        )
    } else {
        Error::new(
            "PayloadTooLarge",
            format!("payload length {len} exceeds limit {max}"),
        )
    }
}

/// Decodes a block whose header has been checked; returns exactly `plain_len` bytes.
pub fn decode(h: &BlockHeader<'_>, ctx: &DecodeCtx<'_>, recon: &mut dyn Recon) -> Result<Vec<u8>> {
    if h.plain_len > ctx.max_block_plain {
        return Err(too_large(false, h.plain_len, ctx.max_block_plain));
    }
    // Check the whole graph before any step runs.
    for s in &h.steps {
        match s {
            Prim::Unimplemented(id) => return Err(unimplemented(*id)),
            Prim::Jpeg { .. } if ctx.revision < 1 => return Err(unimplemented(0x0007)),
            Prim::Deflate { .. } if ctx.revision < 2 => return Err(unimplemented(0x0008)),
            Prim::Bwt { .. } if ctx.revision < 3 => return Err(unimplemented(0x0003)),
            _ => {}
        }
    }
    let mut data: Vec<u8> = h.encoded.to_vec();
    let last_i = h.steps.len() - 1;
    for (i, s) in h.steps.iter().enumerate() {
        let last = i == last_i;
        let bound = step_bound(h, ctx, i, recon)?;
        data = match s {
            Prim::Store => {
                if data.len() as u64 > bound {
                    return Err(too_large(last, data.len() as u64, bound));
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
            } => {
                // Rule 2, before the input is read.
                if u64::from(*dict_size) > ctx.max_window {
                    return Err(Error::new(
                        "WindowTooLarge",
                        format!("dictionary {dict_size} needed, {} allowed", ctx.max_window),
                    ));
                }
                crate::lzma::decode(&data, *dict_size, [*lc, *lp, *pb], bound, last)?
            }
            Prim::Jpeg { record_id } => recon.jpeg(*record_id, &data, bound, last)?,
            Prim::Deflate { record_id } => {
                // Checks (1) to (5) again at the step, with its actual bound; then (6) to (8).
                let r = recon.deflate_record(*record_id, bound, last)?;
                crate::deflate::recreate(*record_id, &r, &data)?
            }
            // Checks (1) to (5) at the step, with its actual bound O.
            Prim::Bwt { block_size } => {
                crate::bwt::decode(&data, *block_size, bound, last, &ctx.memory)?
            }
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

/// The output bound of step `i` (section 8, "Bounds"): the block's `plain_len` on the last step;
/// `max_block_plain` on any other, except a step whose next step S bounds its input:
/// `deflate-reconstruct` (revision 1.2: checks (1) to (5) run on its record first, with S's own
/// output bound, and the bound is the record's `plain_len`) or `bwt` (revision 1.3: check (1), the
/// term, runs first with S's own output bound O, and the bound is I(O, B)). S's own output bound
/// comes from one level of look-ahead ([`own_bound`]), asked before S's pre-checks. Runs before
/// any of step `i`'s own rules.
fn step_bound(
    h: &BlockHeader<'_>,
    ctx: &DecodeCtx<'_>,
    i: usize,
    recon: &mut dyn Recon,
) -> Result<u64> {
    let last_i = h.steps.len() - 1;
    if i == last_i {
        return Ok(h.plain_len);
    }
    let s = i + 1;
    match h.steps.get(s) {
        Some(Prim::Deflate { record_id }) => {
            let o = own_bound(h, ctx, s, recon)?;
            Ok(recon.deflate_record(*record_id, o, s == last_i)?.plain_len)
        }
        Some(Prim::Bwt { block_size }) => {
            let o = own_bound(h, ctx, s, recon)?;
            crate::bwt::check_memory(o, *block_size, &ctx.memory)?;
            Ok(crate::bwt::input_len(o, *block_size))
        }
        _ => Ok(ctx.max_block_plain),
    }
}

/// The look-ahead (section 8, "Bounds", revision 1.3): the own output bound of a bounding step
/// `s`: the block's `plain_len` when it is the last step; otherwise the input bound the step
/// after it gives, when that step gives one (its pre-checks run here), asked with the block's
/// `plain_len` when that step is the last and `max_block_plain` otherwise; otherwise
/// `max_block_plain`. The look-ahead goes no further. It is the rule of a reader of revision
/// 1.3; a reader of revision 1.2 or below keeps that revision's rule (the bound of the
/// pre-checks never derived from a later step: `max_block_plain` when `s` is not the last).
fn own_bound(
    h: &BlockHeader<'_>,
    ctx: &DecodeCtx<'_>,
    s: usize,
    recon: &mut dyn Recon,
) -> Result<u64> {
    let last_i = h.steps.len() - 1;
    if s == last_i {
        return Ok(h.plain_len);
    }
    if ctx.revision < 3 {
        return Ok(ctx.max_block_plain);
    }
    let after = s + 1;
    let after_bound = if after == last_i {
        h.plain_len
    } else {
        ctx.max_block_plain
    };
    match h.steps.get(after) {
        Some(Prim::Deflate { record_id }) => Ok(recon
            .deflate_record(*record_id, after_bound, after == last_i)?
            .plain_len),
        Some(Prim::Bwt { block_size }) => {
            crate::bwt::check_memory(after_bound, *block_size, &ctx.memory)?;
            Ok(crate::bwt::input_len(after_bound, *block_size))
        }
        _ => Ok(ctx.max_block_plain),
    }
}

/// A `zstd` step (section 8, "What the reference decoder enforces for `zstd`", rules 2 to 7).
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
    // Rule 2.
    let window = 1u64 << window_log;
    if window > ctx.max_window {
        return Err(Error::new(
            "WindowTooLarge",
            format!("window {window} needed, {} allowed", ctx.max_window),
        ));
    }
    // Rule 3.
    let prior = if *dictionary == [0u8; 32] {
        None
    } else {
        let p = ctx
            .priors
            .get(dictionary)
            .filter(|p| blake3::hash(p).as_bytes() == dictionary)
            .ok_or_else(|| Error::new("MissingPrior", "a prior the block names is missing"))?;
        Some(p)
    };
    // A decoder with the declared window as its limit (rule 5) and the prior, if any.
    let fresh = || -> Result<(FrameDecoder, Option<u32>)> {
        let mut dec = FrameDecoder::new();
        dec.set_max_window_size(window);
        let mut id = None;
        if let Some(p) = prior {
            let d = Dictionary::decode_dict(p).map_err(|e| zerr(&e))?;
            id = Some(d.id);
            dec.add_dict(d).map_err(|e| zerr(&e))?;
        }
        Ok((dec, id))
    };
    fresh()?;
    // Rule 4.
    if input.is_empty() && bound > 0 {
        return Err(Error::new("ZstdError", "zstd: truncated"));
    }
    let mut out: Vec<u8> = Vec::new();
    let mut src = input;
    while !src.is_empty() {
        let frame = src;
        let (mut dec, dict_id) = fresh()?;
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
        // Rule 6: a frame without a Dictionary_ID is decoded with the step's prior.
        if let Some(id) = dict_id {
            dec.force_dict(id).map_err(|e| zerr(&e))?;
        }
        let frame_base = out.len() as u64;
        // Rule 7, one block at a time: the output so far is checked against the bound after
        // every complete block; a damaged block's partial output is never counted.
        let mut last_block_start = None;
        loop {
            let start = dec.bytes_read_from_source();
            if let Err(e) = dec.decode_blocks(&mut src, BlockDecodingStrategy::UptoBlocks(1)) {
                // `Content_Checksum` belongs to no block: a frame cut short or damaged inside it
                // has all its blocks complete, the one just decoded included.
                let in_checksum = matches!(e, FrameDecoderError::FailedToReadChecksum(_));
                let complete = if in_checksum {
                    Some((start, dec.bytes_read_from_source()))
                } else {
                    last_block_start.map(|s| (s, start))
                };
                let done = match complete {
                    None => 0,
                    Some((s, end)) => complete_blocks_len(frame, s, end, &fresh).unwrap_or(0),
                };
                if frame_base + done > bound {
                    return Err(too_large(last, frame_base + done, bound));
                }
                return Err(zerr(&e));
            }
            last_block_start = Some(start);
            if let Some(v) = dec.collect() {
                out.extend_from_slice(&v);
            }
            if dec.is_finished() {
                if let Some(v) = dec.collect() {
                    out.extend_from_slice(&v);
                }
            }
            if out.len() as u64 > bound {
                return Err(too_large(last, out.len() as u64, bound));
            }
            if dec.is_finished() {
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

/// The output of a frame's complete blocks, the last of which starts at `last_start` and ends at
/// `end` (offsets from the frame's first byte): the frame is cut at `end`, that block marked as the
/// last one and the checksum flag cleared, then decoded to its end. The decoder keeps a window of
/// output it has not released, so this is how the count of rule 7 is taken after damage.
fn complete_blocks_len(
    frame: &[u8],
    last_start: u64,
    end: u64,
    fresh: &dyn Fn() -> Result<(ruzstd::decoding::FrameDecoder, Option<u32>)>,
) -> Option<u64> {
    use ruzstd::decoding::BlockDecodingStrategy;
    let mut cut = frame.get(..usize::try_from(end).ok()?)?.to_vec();
    // Frame_Header_Descriptor after the 4-byte magic: bit 2 is Content_Checksum_flag.
    *cut.get_mut(4)? &= !0x04;
    // Block_Header bit 0 is Last_Block.
    *cut.get_mut(usize::try_from(last_start).ok()?)? |= 1;
    let (mut dec, dict_id) = fresh().ok()?;
    let mut src = &cut[..];
    dec.reset(&mut src).ok()?;
    if let Some(id) = dict_id {
        dec.force_dict(id).ok()?;
    }
    let mut n = 0u64;
    loop {
        dec.decode_blocks(&mut src, BlockDecodingStrategy::UptoBlocks(1))
            .ok()?;
        if dec.is_finished() {
            n += dec.collect().map_or(0, |v| v.len() as u64);
            return Some(n);
        }
        n += dec.collect().map_or(0, |v| v.len() as u64);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn ctx(priors: &HashMap<[u8; 32], Vec<u8>>) -> DecodeCtx<'_> {
        DecodeCtx {
            priors,
            max_window: 1 << 30,
            max_block_plain: 1 << 20,
            revision: 3,
            memory: crate::deflate::Memory {
                decode_memory: u64::MAX,
                memory: u64::MAX,
                max_block_plain: 1 << 20,
            },
        }
    }

    /// A zstd frame (window 1 KiB, no checksum, no content size) of raw blocks, each `n` bytes of
    /// `b`, then optionally a block of the reserved type 3 (damage).
    fn raw_frame(blocks: &[(usize, u8)], damaged_tail: bool) -> Vec<u8> {
        let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x00];
        for (i, &(n, b)) in blocks.iter().enumerate() {
            let last = !damaged_tail && i == blocks.len() - 1;
            let h = (n as u32) << 3 | u32::from(last);
            f.extend_from_slice(&h.to_le_bytes()[..3]);
            f.extend(std::iter::repeat_n(b, n));
        }
        if damaged_tail {
            let h = 10u32 << 3 | 3 << 1 | 1;
            f.extend_from_slice(&h.to_le_bytes()[..3]);
            f.extend_from_slice(&[0; 10]);
        }
        f
    }

    /// Rule 7 (round 2 text): a frame cut short inside its `Content_Checksum` has all its blocks
    /// complete, and their output counts.
    #[test]
    fn zstd_rule_7_checksum_cut_short() {
        let p = HashMap::new();
        let mut f = raw_frame(&[(600, 1), (300, 2)], false);
        f[4] |= 0x04; // Content_Checksum_flag
        f.extend_from_slice(&[0, 0]); // two of the four checksum bytes
        let class = |bound: u64, last: bool| {
            zstd(&f, 10, &[0; 32], bound, last, &ctx(&p))
                .unwrap_err()
                .class
        };
        assert_eq!(class(899, false), "PayloadTooLarge");
        assert_eq!(class(899, true), "BlockLengthMismatch");
        assert_eq!(class(900, false), "ZstdError");
    }

    #[test]
    fn zstd_raw_blocks_decode() {
        let p = HashMap::new();
        let f = raw_frame(&[(600, 1), (600, 2)], false);
        let out = zstd(&f, 10, &[0; 32], 1200, true, &ctx(&p)).unwrap();
        assert_eq!(out.len(), 1200);
        assert!(out[..600].iter().all(|&b| b == 1) && out[600..].iter().all(|&b| b == 2));
        assert_eq!(
            zstd(&f, 10, &[0; 32], 1199, false, &ctx(&p))
                .unwrap_err()
                .class,
            "PayloadTooLarge"
        );
    }

    /// Rule 7: damage after complete blocks whose output passes the bound is `PayloadTooLarge`
    /// (`BlockLengthMismatch` on the last step), also while the decoder still holds that output in
    /// its window; otherwise `ZstdError`. Earlier frames count.
    #[test]
    fn zstd_rule_7_by_block() {
        let p = HashMap::new();
        let f = raw_frame(&[(600, 1), (300, 2)], true);
        let class = |input: &[u8], bound: u64, last: bool| {
            zstd(input, 10, &[0; 32], bound, last, &ctx(&p))
                .unwrap_err()
                .class
        };
        assert_eq!(class(&f, 899, false), "PayloadTooLarge");
        assert_eq!(class(&f, 899, true), "BlockLengthMismatch");
        assert_eq!(class(&f, 900, false), "ZstdError");
        assert_eq!(class(&f, 5000, false), "ZstdError");
        // The first block damaged: nothing of the frame counts.
        let first = raw_frame(&[], true);
        assert_eq!(class(&first, 0, false), "ZstdError");
        // An earlier whole frame counts.
        let mut two = raw_frame(&[(500, 3)], false);
        two.extend_from_slice(&f);
        assert_eq!(class(&two, 1399, false), "PayloadTooLarge");
        assert_eq!(class(&two, 1400, false), "ZstdError");
    }

    /// A `Recon` that records the (record id, bound, last) every record check is asked with and
    /// answers with a record of `plain_len` 100 + id.
    struct Asked(Vec<(u64, u64, bool)>);

    impl Recon for Asked {
        fn jpeg(&mut self, _: u64, _: &[u8], _: u64, _: bool) -> Result<Vec<u8>> {
            Err(Error::new("Internal", "not asked"))
        }
        fn deflate_record(&mut self, id: u64, bound: u64, last: bool) -> Result<DeflateRecord> {
            self.0.push((id, bound, last));
            Ok(DeflateRecord {
                original_len: 0,
                plain_len: 100 + id,
                corrections: Vec::new(),
                original_hash: [0; 32],
            })
        }
    }

    /// The look-ahead (section 8, "Bounds") is the rule of a revision 1.3 reader: on
    /// `[store, deflate-reconstruct 0, deflate-reconstruct 1]` the bound of step 0 is record 0's
    /// `plain_len` either way, but record 0's own bound is record 1's `plain_len` (record 1
    /// asked first, with the block's `plain_len`) at revision 3 and `max_block_plain` below it,
    /// where record 1 is not asked before step 0; a `bwt` after `deflate-reconstruct` the same.
    #[test]
    fn look_ahead_is_gated_on_revision_3() {
        let p = HashMap::new();
        let h = BlockHeader {
            steps: vec![
                Prim::Store,
                Prim::Deflate { record_id: 0 },
                Prim::Deflate { record_id: 1 },
            ],
            bwt_block: 0,
            plain_len: 7,
            encoded: &[],
            reconstruction: true,
            record_ids: vec![0, 1],
        };
        let mut c3 = ctx(&p);
        let mut asked = Asked(Vec::new());
        assert_eq!(step_bound(&h, &c3, 0, &mut asked).unwrap(), 100);
        assert_eq!(asked.0, vec![(1, 7, true), (0, 101, false)]);
        c3.revision = 2;
        let mut asked = Asked(Vec::new());
        assert_eq!(step_bound(&h, &c3, 0, &mut asked).unwrap(), 100);
        assert_eq!(asked.0, vec![(0, 1 << 20, false)]);
        // `[store, deflate-reconstruct 0, bwt]`: step 0 is bounded by record 0's plain_len, and
        // record 0 by I(plain_len, B) at revision 3, max_block_plain at revision 2.
        let h = BlockHeader {
            steps: vec![
                Prim::Store,
                Prim::Deflate { record_id: 0 },
                Prim::Bwt { block_size: 16 },
            ],
            ..h
        };
        c3.revision = 3;
        let mut asked = Asked(Vec::new());
        assert_eq!(step_bound(&h, &c3, 0, &mut asked).unwrap(), 100);
        assert_eq!(asked.0, vec![(0, crate::bwt::input_len(7, 16), false)]);
        c3.revision = 2;
        let mut asked = Asked(Vec::new());
        assert_eq!(step_bound(&h, &c3, 0, &mut asked).unwrap(), 100);
        assert_eq!(asked.0, vec![(0, 1 << 20, false)]);
    }
}
