//! The JPEG peel (PLAN E2-5) and revision 1.1's `jpeg-reconstruct` decoder.
//!
//! Peel: the primary image (from SOI to its first EOI, found by a marker scan) is recompressed
//! with `lepton_jpeg`, the stream is decoded again and compared with the primary bytes (verify by
//! re-encode, D-09), and the bytes after the EOI become nested parts: secondary images (MPF
//! pictures and gain maps, found as complete JPEGs inside the trailing data of a file whose
//! primary carries an MPF or gain-map marker) and the trailing data between and after them. Any
//! failure stores the file as it is and counts the cause with `probe jpeg`'s taxonomy, plus two
//! causes of the peel's own: no EOI marker (a truncated or damaged file, never handed to the
//! library) and no gain (a Lepton stream not smaller than the primary bytes).
//!
//! Library settings: the preset `probe jpeg` used (`compat_lepton_vector_write`) with two caps
//! changed. `max_jpeg_width` and `max_jpeg_height` are raised to 65535, the largest dimension a
//! JPEG frame header can state, so no file falls back for its dimensions alone (PLAN E2-5:
//! "dimension limit raised"); the decoder memory bound below takes over that role.
//! `max_jpeg_file_size` is the writer's block size (capped at `u32::MAX`), because one block holds
//! one peeled primary image. A primary whose decoder memory bound plus the block size exceeds the
//! reader's default memory resource falls back as `dimension cap`, so a default reader never
//! refuses an archive the peel wrote.
//!
//! Decoder memory (the envelope's `decode_memory`): a bound computed from the image's frame
//! header over the library's data layout, not a measurement: every component's 8x8 blocks padded
//! to whole MCUs, 64 coefficients of 2 bytes each, plus [`MODEL_ALLOWANCE`] for the library's
//! probability models and thread buffers.

use std::io::{Cursor, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};

use lepton_jpeg::{
    decode_lepton, encode_lepton, EnabledFeatures, ExitCode, LeptonThreadPool, DEFAULT_THREAD_POOL,
};
use lpk_format::envelope::DEFAULT_MEMORY;
use lpk_format::{
    DecodeContext, FormatError, JpegRecord, PrimitiveDecoder, PrimitiveId, Record, RecordBody,
    Resources, SecondaryImage,
};

use super::{NestedPart, PeelPlan, PeelStage};
use crate::classify::Class;

/// The `lepton_version` this peel writes into its records: the format `lepton_jpeg` 0.5 writes.
pub const LEPTON_VERSION: u8 = 0;

/// Fixed part of the decoder memory bound: the library's models and per-thread buffers. An
/// allowance, not a measured figure (E2-5b measures it).
pub const MODEL_ALLOWANCE: u64 = 64 << 20;

/// The largest dimension a JPEG frame header can state.
const MAX_DIMENSION: u32 = 65535;

/// Why a file is stored as-is. The first eight are `probe jpeg`'s causes (copied from
/// `crates/lpk-bench/src/probe/jpeg.rs`, same names and meanings); the last two are the peel's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cause {
    /// The library says progressive files are disabled (`ProgressiveUnsupported`).
    Progressive,
    /// The library rejects the progressive scan script of the file.
    ProgressiveRejected,
    /// Four components (CMYK).
    FourComponents,
    /// Arithmetic coding.
    Arithmetic,
    /// Over a dimension cap (here: the decoder memory bound).
    DimensionCap,
    /// The file exceeds the library's file-size cap (here: the block size).
    SizeCap,
    /// The decoded bytes differ from the input, or the library reports a verification error.
    VerificationMismatch,
    /// Any other library error, a panic, or a decode-side error.
    Other,
    /// No EOI marker: the file is truncated or damaged and is not handed to the library.
    NoEoi,
    /// The Lepton stream is not smaller than the primary image (the net-gain gate, D-09).
    NoGain,
}

impl Cause {
    /// Every cause, in report order.
    pub const ALL: [Cause; 10] = [
        Cause::Progressive,
        Cause::ProgressiveRejected,
        Cause::FourComponents,
        Cause::Arithmetic,
        Cause::DimensionCap,
        Cause::SizeCap,
        Cause::VerificationMismatch,
        Cause::Other,
        Cause::NoEoi,
        Cause::NoGain,
    ];

    /// Position in [`Cause::ALL`].
    pub fn index(self) -> usize {
        Cause::ALL.iter().position(|&c| c == self).unwrap_or(0)
    }

    /// The label (the probe's labels for its causes).
    pub fn label(self) -> &'static str {
        match self {
            Cause::Progressive => "progressive (disabled)",
            Cause::ProgressiveRejected => "progressive (rejected by the library)",
            Cause::FourComponents => "four components (CMYK)",
            Cause::Arithmetic => "arithmetic-coded",
            Cause::DimensionCap => "dimension cap",
            Cause::SizeCap => "file too large (size cap)",
            Cause::VerificationMismatch => "verification mismatch",
            Cause::Other => "other",
            Cause::NoEoi => "no EOI marker (truncated or damaged)",
            Cause::NoGain => "no gain",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The marker scan: copied from `probe jpeg` (crates/lpk-bench/src/probe/jpeg.rs, `marker_scan`,
// same repository and licence), trimmed to what the peel needs and extended with the primary's
// length and the components' sampling factors.

/// The first frame header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The SOF marker's second byte.
    pub marker: u8,
    /// Sample precision.
    pub precision: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Per component: horizontal and vertical sampling factors.
    pub sampling: Vec<(u32, u32)>,
}

impl Frame {
    fn progressive(&self) -> bool {
        self.marker == 0xC2
    }

    fn arithmetic(&self) -> bool {
        matches!(self.marker, 0xC9..=0xCB | 0xCD..=0xCF)
    }
}

/// What the marker scan found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    /// The first frame header, if found.
    pub frame: Option<Frame>,
    /// An APP2 segment starting with `MPF\0`.
    pub mpf: bool,
    /// A gain-map marker (APP1 `hdrgm` or `HDRGainMap`, APP2 ISO 21496-1).
    pub gain_map_marker: bool,
    /// Bytes from SOI through the first EOI; `None` when the scan stopped before an EOI.
    pub primary_len: Option<usize>,
}

fn be16(b: &[u8]) -> u32 {
    u32::from(b.first().copied().unwrap_or(0)) << 8 | u32::from(b.get(1).copied().unwrap_or(0))
}

/// Scan the marker structure: segments are walked by their length fields, entropy-coded data is
/// skipped up to the next real marker, and scanning stops at the first EOI. Never panics.
pub fn marker_scan(data: &[u8]) -> Scan {
    let mut s = Scan::default();
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return s;
    }
    let mut pos = 2usize;
    loop {
        if data.get(pos) != Some(&0xFF) {
            return s;
        }
        while data.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let Some(&m) = data.get(pos) else {
            return s;
        };
        pos += 1;
        match m {
            0x00 => return s,
            0xD9 => {
                s.primary_len = Some(pos);
                return s;
            }
            0x01 | 0xD0..=0xD8 => continue,
            _ => {}
        }
        let Some(len_bytes) = data.get(pos..pos + 2) else {
            return s;
        };
        let len = be16(len_bytes) as usize;
        let Some(payload) = (len >= 2).then(|| data.get(pos + 2..pos + len)).flatten() else {
            return s;
        };
        match m {
            0xC0..=0xC3 | 0xC5..=0xCB | 0xCD..=0xCF => {
                if s.frame.is_none() {
                    if payload.len() < 6 {
                        return s;
                    }
                    let n = usize::from(payload[5]);
                    let sampling = (0..n)
                        .filter_map(|i| payload.get(6 + 3 * i + 1))
                        .map(|&hv| (u32::from(hv >> 4), u32::from(hv & 15)))
                        .collect();
                    s.frame = Some(Frame {
                        marker: m,
                        precision: u32::from(payload[0]),
                        height: be16(&payload[1..3]),
                        width: be16(&payload[3..5]),
                        sampling,
                    });
                }
            }
            0xE1 => {
                s.gain_map_marker |= payload.windows(5).any(|w| w == b"hdrgm")
                    || payload.windows(10).any(|w| w == b"HDRGainMap");
            }
            0xE2 => {
                s.mpf |= payload.starts_with(b"MPF\0");
                s.gain_map_marker |= payload.starts_with(b"urn:iso:std:iso:ts:21496:-1");
            }
            _ => {}
        }
        pos += len;
        if m == 0xDA {
            let mut p = pos;
            loop {
                let Some(i) = data[p.min(data.len())..].iter().position(|&b| b == 0xFF) else {
                    return s;
                };
                let q = p + i;
                match data.get(q + 1) {
                    None => return s,
                    Some(0x00) | Some(0xD0..=0xD7) => p = q + 2,
                    Some(0xFF) => p = q + 1,
                    Some(_) => {
                        pos = q;
                        break;
                    }
                }
            }
        }
    }
}

/// The secondary images inside `data[from..]`: complete JPEGs (SOI through EOI by the marker
/// scan), in order, not overlapping. Returns `(offset, len)` pairs.
pub fn find_secondaries(data: &[u8], from: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut p = from;
    while let Some(i) = data
        .get(p..)
        .and_then(|d| d.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]))
    {
        let at = p + i;
        match marker_scan(&data[at..]).primary_len {
            Some(len) if marker_scan(&data[at..]).frame.is_some() => {
                out.push((at, len));
                p = at + len;
            }
            _ => p = at + 1,
        }
    }
    out
}

/// The decoder memory bound of an image (see the module documentation).
pub fn memory_bound(frame: &Frame) -> u64 {
    let hmax = frame.sampling.iter().map(|s| s.0).max().unwrap_or(1).max(1);
    let vmax = frame.sampling.iter().map(|s| s.1).max().unwrap_or(1).max(1);
    let mcus_x = u64::from(frame.width).div_ceil(u64::from(8 * hmax));
    let mcus_y = u64::from(frame.height).div_ceil(u64::from(8 * vmax));
    let blocks: u64 = frame
        .sampling
        .iter()
        .map(|&(h, v)| mcus_x * u64::from(h) * mcus_y * u64::from(v))
        .sum();
    blocks.saturating_mul(128).saturating_add(MODEL_ALLOWANCE)
}

// ---------------------------------------------------------------------------------------------
// Failure causes: the rules of `probe jpeg`'s `classify` and `unsupported_jpeg_cause` (copied
// from crates/lpk-bench/src/probe/jpeg.rs; they follow lepton_jpeg 0.5.8's wording).

fn is_progressive_rejection(m: &str) -> bool {
    m.starts_with("progress")
        || m.contains("spectral selection")
        || m.contains("successive approximation")
}

fn clean_message(m: &str) -> String {
    m.lines().next().unwrap_or("").trim().to_string()
}

fn cause_of(code: ExitCode, message: &str, decode: bool, scan: &Scan) -> Cause {
    if matches!(
        code,
        ExitCode::VerificationLengthMismatch | ExitCode::VerificationContentMismatch
    ) {
        return Cause::VerificationMismatch;
    }
    if decode {
        return Cause::Other;
    }
    match code {
        ExitCode::Unsupported4Colors => Cause::FourComponents,
        ExitCode::ProgressiveUnsupported => Cause::Progressive,
        ExitCode::UnsupportedJpeg => {
            let message = clean_message(message);
            let frame = scan.frame.as_ref();
            if message.contains("arithm") {
                Cause::Arithmetic
            } else if message.starts_with("image dimensions larger") {
                Cause::DimensionCap
            } else if message.contains("too large to encode") {
                Cause::SizeCap
            } else if is_progressive_rejection(&message) && frame.is_some_and(Frame::progressive) {
                Cause::ProgressiveRejected
            } else if frame.is_some_and(Frame::arithmetic) {
                Cause::Arithmetic
            } else if frame.is_some_and(|f| f.sampling.len() == 4) {
                Cause::FourComponents
            } else {
                Cause::Other
            }
        }
        _ => Cause::Other,
    }
}

/// The library settings the peel writes with and the decoder reads with.
pub fn features(max_file_size: u32) -> EnabledFeatures {
    let mut f = EnabledFeatures::compat_lepton_vector_write();
    f.max_jpeg_width = MAX_DIMENSION;
    f.max_jpeg_height = MAX_DIMENSION;
    f.max_jpeg_file_size = max_file_size;
    f
}

/// The JPEG peel stage.
#[derive(Debug, Clone)]
pub struct JpegPeel {
    /// Accept progressive files (the preset's value, true). Tests turn it off.
    pub progressive: bool,
    /// Decoder memory a peeled image may need, beyond the block, before it falls back as
    /// `dimension cap`: by default the reader's default memory resource.
    pub memory_limit: u64,
}

impl Default for JpegPeel {
    fn default() -> Self {
        JpegPeel {
            progressive: true,
            memory_limit: DEFAULT_MEMORY,
        }
    }
}

/// A writer that refuses to grow past `max` bytes.
struct Bounded {
    buf: Vec<u8>,
    max: usize,
}

impl Write for Bounded {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() + b.len() > self.max {
            return Err(std::io::Error::other("output past its bound"));
        }
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

enum Fail {
    Code(ExitCode, String, bool),
    Panic,
}

fn lepton_decode(
    stream: &[u8],
    max: usize,
    f: &EnabledFeatures,
    pool: &dyn LeptonThreadPool,
) -> Result<Vec<u8>, Fail> {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut reader = Cursor::new(stream);
        let mut out = Bounded {
            buf: Vec::with_capacity(max),
            max,
        };
        decode_lepton(&mut reader, &mut out, f, pool).map(|_| out.buf)
    }));
    match r {
        Err(_) => Err(Fail::Panic),
        Ok(Err(e)) => Err(Fail::Code(e.exit_code(), e.message().to_string(), true)),
        Ok(Ok(v)) => Ok(v),
    }
}

impl JpegPeel {
    /// [`PeelStage::peel`] with hooks for tests: `after_encode` may change the Lepton stream,
    /// `after_decode` the decoded bytes, before the comparison.
    pub fn peel_with(
        &self,
        data: &[u8],
        max_part: u64,
        after_encode: &dyn Fn(&mut Vec<u8>),
        after_decode: &dyn Fn(&mut Vec<u8>),
    ) -> Result<PeelPlan, Cause> {
        let scan = marker_scan(data);
        let primary_len = scan.primary_len.ok_or(Cause::NoEoi)?;
        let primary = &data[..primary_len];
        if primary_len as u64 > max_part {
            return Err(Cause::SizeCap);
        }
        let memory = match &scan.frame {
            Some(f) => memory_bound(f),
            None => MODEL_ALLOWANCE,
        };
        if memory.saturating_add(max_part) > self.memory_limit {
            return Err(Cause::DimensionCap);
        }
        let mut f = features(u32::try_from(max_part).unwrap_or(u32::MAX));
        f.progressive = self.progressive;
        let pool: &dyn LeptonThreadPool = &DEFAULT_THREAD_POOL;
        let enc = catch_unwind(AssertUnwindSafe(|| {
            let mut reader = Cursor::new(primary);
            let mut writer = Cursor::new(Vec::with_capacity(primary_len / 2 + 1024));
            encode_lepton(&mut reader, &mut writer, &f, pool).map(|_| writer.into_inner())
        }));
        let fail = |e: Fail| match e {
            Fail::Panic => Cause::Other,
            Fail::Code(code, m, decode) => cause_of(code, &m, decode, &scan),
        };
        let mut stream = match enc {
            Err(_) => return Err(Cause::Other),
            Ok(Err(e)) => {
                return Err(fail(Fail::Code(
                    e.exit_code(),
                    e.message().to_string(),
                    false,
                )))
            }
            Ok(Ok(v)) => v,
        };
        after_encode(&mut stream);
        // Verify by re-encode: the stream must give back the primary bytes exactly.
        let mut decoded = lepton_decode(&stream, primary_len, &f, pool).map_err(fail)?;
        after_decode(&mut decoded);
        if decoded != primary {
            return Err(Cause::VerificationMismatch);
        }
        if stream.len() >= primary_len {
            return Err(Cause::NoGain);
        }
        // The bytes after the primary image: secondary images and the trailing data around them.
        let mut nested = Vec::new();
        let secondaries = if scan.mpf || scan.gain_map_marker {
            find_secondaries(data, primary_len)
        } else {
            Vec::new()
        };
        let mut at = primary_len;
        for (off, len) in secondaries {
            if off > at {
                nested.push(NestedPart {
                    offset: at as u64,
                    len: (off - at) as u64,
                    secondary: false,
                });
            }
            nested.push(NestedPart {
                offset: off as u64,
                len: len as u64,
                secondary: true,
            });
            at = off + len;
        }
        if at < data.len() {
            nested.push(NestedPart {
                offset: at as u64,
                len: (data.len() - at) as u64,
                secondary: false,
            });
        }
        Ok(PeelPlan {
            primitive: PrimitiveId::JpegReconstruct,
            primary_len: primary_len as u64,
            stream,
            nested,
            memory,
            original_hash: *blake3::hash(data).as_bytes(),
            original_len: data.len() as u64,
        })
    }
}

impl PeelStage for JpegPeel {
    fn name(&self) -> &'static str {
        "jpeg"
    }

    fn applies_to(&self, class: Class) -> bool {
        class == Class::Jpeg
    }

    fn peel(&self, data: &[u8], max_part: u64) -> Result<PeelPlan, Cause> {
        self.peel_with(data, max_part, &|_| {}, &|_| {})
    }

    fn record(&self, plan: &PeelPlan, nested_chunks: &[Vec<u64>]) -> Record {
        let mut trailing = Vec::new();
        let mut gainmaps = Vec::new();
        for (p, chunks) in plan.nested.iter().zip(nested_chunks) {
            if p.secondary {
                gainmaps.push(SecondaryImage {
                    offset: p.offset,
                    len: p.len,
                    chunks: chunks.clone(),
                });
            } else {
                trailing.extend_from_slice(chunks);
            }
        }
        Record::new(RecordBody::Jpeg(JpegRecord {
            original_len: plan.original_len,
            primary_len: plan.primary_len,
            trailing: Vec::new(),
            nested_trailing_chunks: trailing,
            gainmaps,
            lepton_version: LEPTON_VERSION,
            original_hash: plan.original_hash,
        }))
    }
}

// ---------------------------------------------------------------------------------------------
// The full reader's decoder.

/// Revision 1.1's `jpeg-reconstruct` decoder: decodes the block's Lepton stream into the primary
/// image, then assembles the original file from the record (the nested trailing chunks and the
/// secondary images, read through the decode context) and checks `original_hash` and
/// `original_len`. Errors: a record of another kind or an unknown `lepton_version`, a stream the
/// library cannot decode, output longer than `primary_len`, a primary of another length, or an
/// original that does not match are `BadRecord` (reasons `kind`, `lepton_version`,
/// `lepton stream`, `primary_len`, `original_hash`).
#[derive(Debug, Clone, Copy, Default)]
pub struct JpegDecoder;

fn bad(record: u64, reason: &'static str) -> FormatError {
    FormatError::BadRecord { record, reason }
}

impl PrimitiveDecoder for JpegDecoder {
    fn decode(
        &self,
        _params: &[u8],
        _input: &[u8],
        _expected_len: u64,
        _limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        // Without the archive behind it the record cannot be read.
        Err(FormatError::UnimplementedPrimitive {
            id: PrimitiveId::JpegReconstruct as u16,
        })
    }

    fn decode_in(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        _last: bool,
        _limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        let id = PrimitiveId::JpegReconstruct
            .record_id(params)
            .ok_or(FormatError::BadParams {
                id: PrimitiveId::JpegReconstruct as u16,
                reason: "record_id",
            })?;
        let RecordBody::Jpeg(j) = ctx.record(id)?.body else {
            return Err(bad(id, "kind"));
        };
        if j.lepton_version != LEPTON_VERSION {
            return Err(bad(id, "lepton_version"));
        }
        if j.primary_len > expected_len {
            return Err(FormatError::PayloadTooLarge {
                len: j.primary_len,
                max: expected_len,
            });
        }
        let max = usize::try_from(j.primary_len).map_err(|_| bad(id, "primary_len"))?;
        let f = features(u32::MAX);
        let primary = lepton_decode(input, max, &f, &DEFAULT_THREAD_POOL)
            .map_err(|_| bad(id, "lepton stream"))?;
        if primary.len() as u64 != j.primary_len {
            return Err(bad(id, "primary_len"));
        }
        let mut h = blake3::Hasher::new();
        h.update(&primary);
        let mut total = j.primary_len;
        if !j.trailing.is_empty() {
            h.update(&j.trailing);
            total += j.trailing.len() as u64;
        } else {
            // The nested trailing data fills the ranges the secondary images leave, in order.
            let mut rest = Vec::new();
            for &c in &j.nested_trailing_chunks {
                rest.extend_from_slice(&ctx.chunk(id, c)?);
            }
            let mut used = 0usize;
            for g in &j.gainmaps {
                let gap = g
                    .offset
                    .checked_sub(total)
                    .and_then(|d| usize::try_from(d).ok())
                    .ok_or_else(|| bad(id, "gainmaps"))?;
                let piece = rest
                    .get(used..used + gap)
                    .ok_or_else(|| bad(id, "trailing"))?;
                h.update(piece);
                used += gap;
                let mut glen = 0u64;
                for &c in &g.chunks {
                    let b = ctx.chunk(id, c)?;
                    glen += b.len() as u64;
                    h.update(&b);
                }
                if glen != g.len {
                    return Err(bad(id, "gainmaps"));
                }
                total = g.offset + g.len;
            }
            h.update(&rest[used.min(rest.len())..]);
            total += (rest.len() - used.min(rest.len())) as u64;
        }
        if total != j.original_len || h.finalize().as_bytes() != &j.original_hash {
            return Err(bad(id, "original_hash"));
        }
        Ok(primary)
    }
}

/// Register the full reader's decoders (revision 1.1's `jpeg-reconstruct`) on `archive`, so its
/// peeled files extract and verify. Call it after `set_priors` (which keeps it anyway).
pub fn register_full_reader<R: std::io::Read + std::io::Seek>(
    archive: &mut lpk_format::Archive<R>,
) {
    archive
        .registry_mut()
        .register(PrimitiveId::JpegReconstruct, Box::new(JpegDecoder));
}
