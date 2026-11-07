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
//! one peeled primary image. A primary whose decoder memory allowance plus the block size exceeds
//! the reader's default memory resource falls back as `decoder memory allowance exceeded`, so a
//! default reader never refuses an archive the peel wrote. An input longer than
//! [`JpegPeel::max_file`] is not read whole: it falls back as `size cap` before any byte is read.
//!
//! Decoder memory (the envelope's `decode_memory`): an allowance, not a bound. Its coefficient
//! term is computed from the image's frame header over the library's data layout (every
//! component's 8x8 blocks padded to whole MCUs, 64 coefficients of 2 bytes each); its fixed term,
//! [`MODEL_ALLOWANCE`], stands for the library's probability models and thread buffers, which
//! revision 1.1 does not bound (E2-5b measures them). Before the library decodes a stream,
//! [`read_stream_header`] checks every length in the stream's header against the primary image
//! (the library allocates each at once). The writer declares [`image_term`], the larger of
//! revision 1.1's term and a stated bound on the library's allocations; the reader checks
//! revision 1.1's term against the archive's `decode_memory` (so an archive an earlier writer
//! declared honestly is never refused for it) and [`image_term`] against its own memory
//! resource, the extraction's budget.

use std::io::{Cursor, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};

use lepton_jpeg::{
    decode_lepton, encode_lepton, EnabledFeatures, ExitCode, LeptonThreadPool, SingleThreadPool,
    DEFAULT_THREAD_POOL,
};
use lpk_format::envelope::{Refusal, DEFAULT_MEMORY};
use lpk_format::{
    DecodeContext, FormatError, JpegRecord, PrimitiveDecoder, PrimitiveId, Record, RecordBody,
    Resources, SecondaryImage,
};

use super::{Fallback, NestedPart, PeelPlan, PeelStage};
use crate::classify::Class;

/// The `lepton_version` this peel writes into its records: the format `lepton_jpeg` 0.5 writes.
pub const LEPTON_VERSION: u8 = 0;

/// Fixed part of the decoder memory allowance: the library's models and per-thread buffers. An
/// allowance, not a measured figure (E2-5b measures it).
pub const MODEL_ALLOWANCE: u64 = 64 << 20;

/// The default of [`JpegPeel::max_file`]: `lepton_jpeg`'s own file-size cap, the
/// `max_jpeg_file_size` of its `EnabledFeatures` presets (`128 * 1024 * 1024` in
/// `src/enabled_features.rs` of lepton_jpeg 0.5.8). A library constant, not a measurement.
pub const LEPTON_MAX_FILE: u64 = 128 * 1024 * 1024;

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
    /// The library refuses the image's dimensions (`image dimensions larger than ...`). With
    /// the caps this peel sets (65535, the largest a frame header states) the library does not
    /// raise it; the label stays the probe's.
    DimensionCap,
    /// The file exceeds a file-size cap: the primary image is longer than one block, or the
    /// whole input is longer than [`JpegPeel::max_file`] (refused before it is read).
    SizeCap,
    /// The decoded bytes differ from the input, or the library reports a verification error.
    VerificationMismatch,
    /// Any other library error, a panic, or a decode-side error.
    Other,
    /// No EOI marker: the file is truncated or damaged and is not handed to the library.
    NoEoi,
    /// The Lepton stream is not smaller than the primary image (the net-gain gate, D-09).
    NoGain,
    /// The image's decoder memory allowance plus the block exceeds [`JpegPeel::memory_limit`].
    MemoryCap,
}

impl Cause {
    /// Every cause, in report order.
    pub const ALL: [Cause; 11] = [
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
        Cause::MemoryCap,
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
            Cause::MemoryCap => "decoder memory allowance exceeded",
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
    /// Decoder memory a peeled image may need, with the block, before it falls back as
    /// [`Cause::MemoryCap`]: by default the reader's default memory resource.
    pub memory_limit: u64,
    /// Longest input the peel reads whole; a longer one is stored through the streaming path
    /// and counted as [`Cause::SizeCap`] before any byte is read. Default [`LEPTON_MAX_FILE`].
    pub max_file: u64,
    /// Lepton's `max_processor_threads` (the preset's 8). The stream does not depend on it:
    /// lepton_jpeg 0.5.8 splits an image by `max_partitions` and writes the partitions in a
    /// fixed order. The peel pool's workers use 1 (design
    /// `docs/notes/e2-10-peel-pool-design.md`, section 4).
    pub processor_threads: u32,
}

impl Default for JpegPeel {
    fn default() -> Self {
        JpegPeel {
            progressive: true,
            memory_limit: DEFAULT_MEMORY,
            max_file: LEPTON_MAX_FILE,
            processor_threads: 8,
        }
    }
}

/// The decode threads of an extraction (E2-25, D-54's thread budget): `total` tokens, the
/// `--threads N` of the extraction. A thread that decodes holds one:
/// a decode worker for each block it decodes ([`DecodeThreads::hold`]), except while a JPEG
/// stream of its block decodes, when it waits for the stream's partitions and lends its token;
/// each of the stream's partition threads (on the library's pool, up to the preset's eight per
/// stream) takes one before it runs and gives it back when the stream's work queue is empty.
/// So the threads decoding at any moment never exceed `total`. With `total` 1 (`--threads 1`)
/// a stream decodes on the calling thread alone.
#[derive(Debug)]
pub struct DecodeThreads {
    free: std::sync::Mutex<usize>,
    cv: std::sync::Condvar,
    total: usize,
}

impl DecodeThreads {
    /// A budget of `total` decode threads (at least one).
    pub fn new(total: usize) -> std::sync::Arc<Self> {
        let total = total.max(1);
        std::sync::Arc::new(DecodeThreads {
            free: std::sync::Mutex::new(total),
            cv: std::sync::Condvar::new(),
            total,
        })
    }

    /// The tokens in all.
    pub fn total(&self) -> usize {
        self.total
    }

    /// The tokens not held at the moment.
    pub fn free(&self) -> usize {
        *self.free.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn acquire(&self) {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        while *free == 0 {
            free = self.cv.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        *free -= 1;
    }

    fn release(&self) {
        *self.free.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.cv.notify_one();
    }

    /// Wait until every token is free again, at most `timeout`; whether they all came back.
    /// A partition task gives its token back when its closure ends, which can be just after
    /// the decoder received the task's result, so a check right after a decode may see a token
    /// still out for a moment.
    #[cfg(test)]
    fn wait_all_free(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        while *free < self.total {
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let step = (deadline - now).min(std::time::Duration::from_millis(10));
            free = self
                .cv
                .wait_timeout(free, step)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// Take a token for this thread until the guard drops (blocks while none is free).
    pub fn hold(self: &std::sync::Arc<Self>) -> DecodeToken {
        self.acquire();
        DecodeToken(std::sync::Arc::clone(self))
    }
}

/// A token of [`DecodeThreads`], given back on drop.
#[derive(Debug)]
pub struct DecodeToken(std::sync::Arc<DecodeThreads>);

impl Drop for DecodeToken {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The library's pool, each task holding a token of the budget while it runs.
struct BudgetPool(std::sync::Arc<DecodeThreads>);

impl LeptonThreadPool for BudgetPool {
    fn max_parallelism(&self) -> usize {
        self.0.total
    }

    fn run(&self, f: Box<dyn FnOnce() + Send + 'static>) {
        let budget = std::sync::Arc::clone(&self.0);
        DEFAULT_THREAD_POOL.run(Box::new(move || {
            let _token = budget.hold();
            f();
        }));
    }
}

/// Takes the lent token back on drop (also when the decode unwinds).
struct Lend<'a>(&'a DecodeThreads);

impl Drop for Lend<'_> {
    fn drop(&mut self) {
        self.0.acquire();
    }
}

thread_local! {
    static DECODE_THREADS: std::cell::RefCell<Option<std::sync::Arc<DecodeThreads>>> =
        const { std::cell::RefCell::new(None) };
    /// The streams this thread decoded on itself and those it handed to a pool.
    static DECODE_PATHS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// Test hook: the JPEG streams the current thread has decoded on itself alone and those whose
/// partitions it handed to a pool (the library's default pool or the budget's), so far.
#[doc(hidden)]
pub fn decode_paths() -> (u64, u64) {
    DECODE_PATHS.with(std::cell::Cell::get)
}

fn count_path(own: bool) {
    DECODE_PATHS.with(|c| {
        let (a, b) = c.get();
        c.set(if own { (a + 1, b) } else { (a, b + 1) });
    });
}

/// Puts the thread's previous budget back on drop.
struct Restore(Option<std::sync::Arc<DecodeThreads>>);

impl Drop for Restore {
    fn drop(&mut self) {
        let old = self.0.take();
        DECODE_THREADS.with(|b| *b.borrow_mut() = old);
    }
}

/// Run `f` with the `jpeg-reconstruct` decodes on this thread under `budget` (restored after
/// `f`, also on a panic). The thread must hold one of the budget's tokens whenever it decodes
/// a block ([`DecodeThreads::hold`]); a JPEG stream lends it while its partitions decode.
/// Without a budget, the default, a stream decodes on up to eight of the library's pool
/// threads.
pub fn with_decode_threads<T>(
    budget: Option<std::sync::Arc<DecodeThreads>>,
    f: impl FnOnce() -> T,
) -> T {
    let old = DECODE_THREADS.with(|b| std::mem::replace(&mut *b.borrow_mut(), budget));
    let _restore = Restore(old);
    f()
}

/// Decode `stream` under this thread's budget ([`with_decode_threads`]).
fn budgeted_decode(stream: &[u8], max: usize, f: &mut EnabledFeatures) -> Result<Vec<u8>, Fail> {
    let Some(budget) = DECODE_THREADS.with(|b| b.borrow().clone()) else {
        count_path(false);
        return lepton_decode(stream, max, f, &DEFAULT_THREAD_POOL);
    };
    count_path(budget.total == 1);
    if budget.total == 1 {
        f.max_processor_threads = 1;
        return lepton_decode(stream, max, f, &SingleThreadPool::default());
    }
    // Lend this thread's token while the partitions decode; take it back after, so the
    // caller's guard gives back exactly one.
    budget.release();
    let _lend = Lend(&budget);
    lepton_decode(stream, max, f, &BudgetPool(std::sync::Arc::clone(&budget)))
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
            return Err(Cause::MemoryCap);
        }
        let mut f = features(u32::try_from(max_part).unwrap_or(u32::MAX));
        f.progressive = self.progressive;
        f.max_processor_threads = self.processor_threads.max(1);
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
        // The net-gain gate first: a stream that gains nothing is not worth the verifying decode.
        if stream.len() >= primary_len {
            return Err(Cause::NoGain);
        }
        // The term the record declares is the one its reader computes from the stream itself.
        let header = read_stream_header(&stream, primary_len as u64).ok_or(Cause::Other)?;
        let memory = image_term(&header, primary_len as u64, stream.len() as u64);
        if memory.saturating_add(max_part) > self.memory_limit {
            return Err(Cause::MemoryCap);
        }
        // Verify by re-encode: the stream must give back the primary bytes exactly.
        let mut decoded = lepton_decode(&stream, primary_len, &f, pool).map_err(fail)?;
        after_decode(&mut decoded);
        if decoded != primary {
            return Err(Cause::VerificationMismatch);
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

    fn refuse_unread(&self, len: u64) -> Option<Fallback> {
        (len > self.max_file).then_some(Fallback::Jpeg(Cause::SizeCap))
    }

    fn worker_copy(&self) -> Option<Box<dyn PeelStage>> {
        Some(Box::new(JpegPeel {
            processor_threads: 1,
            ..self.clone()
        }))
    }

    fn peel(&self, data: &[u8], max_part: u64) -> Result<PeelPlan, Fallback> {
        self.peel_with(data, max_part, &|_| {}, &|_| {})
            .map_err(Fallback::Jpeg)
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
/// secondary images, read through the decode context, the trailing chunks hashed as they stream)
/// and checks `original_hash` and `original_len`. Order of checks (spec section 8): record
/// lookup; `kind`; `lepton_version`; `primary_len` against the step's bound; the image's memory
/// term against `limits.memory - limits.max_block_plain` (`Refused`, field `decode_memory`);
/// decode; the primary's length; assembly; `original_len` and `original_hash`. Errors besides
/// `Refused` are `BadRecord` (reasons `kind`, `lepton_version`, `lepton stream`, `primary_len`,
/// `gainmaps`, `trailing`, `original_hash`) or the chunk errors of the decode context.
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
        limits: &Resources,
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
        // Resources. The header is checked first (no length in it past what the primary
        // explains). Then revision 1.1's rule: the image's coefficient term plus the fixed
        // allowance must fit in the decode memory left after the block buffer (the archive's
        // `decode_memory`, never above the reader's resource). Then the library's own need
        // ([`image_term`], which may exceed what an archive written under revision 1.1
        // declares) must fit in the reader's own memory resource, the extraction's budget, left
        // after the block buffer. The library's caps are then the checked dimensions and the
        // primary's length.
        let header =
            read_stream_header(input, j.primary_len).ok_or_else(|| bad(id, "lepton stream"))?;
        let refuse = |need: u64, allowed: u64| {
            if need > allowed.saturating_sub(limits.max_block_plain) {
                return Err(FormatError::Refused(Refusal {
                    field: "decode_memory",
                    needed: limits.max_block_plain.saturating_add(need),
                    allowed,
                }));
            }
            Ok(())
        };
        refuse(memory_bound(&header.frame), limits.memory)?;
        let need = image_term(&header, j.primary_len, input.len() as u64);
        // The need is reserved from the extraction's gate (R115): it waits for room beside the
        // other decodes, waits for the pool to empty and runs alone, or is refused with the
        // `--memory` that would let it run. Without a gate it must fit the reader's own
        // resource.
        let reserved = match lpk_format::reserve(&*ctx, need)? {
            Some(r) => Some(r),
            None => {
                refuse(need, ctx.memory_budget().unwrap_or(limits.memory))?;
                None
            }
        };
        let frame = header.frame;
        let mut f = features(u32::try_from(j.primary_len).unwrap_or(u32::MAX));
        f.max_jpeg_width = frame.width;
        f.max_jpeg_height = frame.height;
        let primary = budgeted_decode(input, max, &mut f).map_err(|_| bad(id, "lepton stream"))?;
        // The library's working memory is freed: the reservation goes back before the trailing
        // chunks are read (which may decode a lower block).
        drop(reserved);
        if primary.len() as u64 != j.primary_len {
            return Err(bad(id, "primary_len"));
        }
        let mut h = blake3::Hasher::new();
        h.update(&primary);
        let mut total = j.primary_len;
        let overflow = || bad(id, "original_hash");
        if !j.trailing.is_empty() {
            h.update(&j.trailing);
            total = total
                .checked_add(j.trailing.len() as u64)
                .ok_or_else(overflow)?;
        } else {
            // The nested trailing data fills the ranges the secondary images leave, in order;
            // it is hashed as it streams, one chunk held at a time.
            let mut rest = Trailing {
                ids: j.nested_trailing_chunks.iter(),
                buf: Vec::new(),
                pos: 0,
                record: id,
            };
            for g in &j.gainmaps {
                let gap = g
                    .offset
                    .checked_sub(total)
                    .ok_or_else(|| bad(id, "gainmaps"))?;
                if rest.feed(ctx, &mut h, gap)? != gap {
                    return Err(bad(id, "trailing"));
                }
                let mut glen = 0u64;
                for &c in &g.chunks {
                    let b = ctx.chunk(id, c)?;
                    glen = glen.saturating_add(b.len() as u64);
                    h.update(&b);
                }
                if glen != g.len {
                    return Err(bad(id, "gainmaps"));
                }
                total = g
                    .offset
                    .checked_add(g.len)
                    .ok_or_else(|| bad(id, "gainmaps"))?;
            }
            let tail = rest.feed(ctx, &mut h, u64::MAX)?;
            total = total.checked_add(tail).ok_or_else(overflow)?;
        }
        if total != j.original_len || h.finalize().as_bytes() != &j.original_hash {
            return Err(bad(id, "original_hash"));
        }
        Ok(primary)
    }
}

/// The nested trailing chunks of a record, read one at a time through the decode context.
struct Trailing<'a> {
    ids: std::slice::Iter<'a, u64>,
    buf: Vec<u8>,
    pos: usize,
    record: u64,
}

impl Trailing<'_> {
    /// Hash up to `n` of the next bytes; returns how many there were.
    fn feed(
        &mut self,
        ctx: &mut dyn DecodeContext,
        h: &mut blake3::Hasher,
        mut n: u64,
    ) -> Result<u64, FormatError> {
        let mut done = 0u64;
        while n > 0 {
            if self.pos == self.buf.len() {
                let Some(&c) = self.ids.next() else {
                    break;
                };
                self.buf = ctx.chunk(self.record, c)?;
                self.pos = 0;
                continue;
            }
            let left = self.buf.len() - self.pos;
            let k = usize::try_from(n).map_or(left, |n| n.min(left));
            h.update(&self.buf[self.pos..self.pos + k]);
            self.pos += k;
            n -= k as u64;
            done += k as u64;
        }
        Ok(done)
    }
}

/// What [`read_stream_header`] reads from a Lepton stream's header before the library sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamHeader {
    /// The first frame header of the raw JPEG header.
    pub frame: Frame,
    /// The inflated length of the zlib-compressed header (every byte the library reads from it).
    pub header_len: u64,
    /// The number of partitions (thread handoffs) the header declares.
    pub partitions: u32,
}

/// The largest inflated header [`read_stream_header`] accepts for a primary of `primary_len`
/// bytes: two primaries and 64 KiB. Everything the header holds describes bytes of the primary
/// image (its raw header, its trailing garbage, its restart counts and errors) or is a few bytes
/// per partition, so a header lepton_jpeg writes stays far below this.
pub fn header_cap(primary_len: u64) -> u64 {
    primary_len.saturating_mul(2).saturating_add(64 << 10)
}

/// Reads exactly `n` bytes of `z` into `out` (or skips them when `out` is `None`), counting them
/// in `total` against `cap`. `None` when the data ends first, is damaged, or passes `cap`.
fn take_exact(
    z: &mut impl std::io::Read,
    n: u64,
    total: &mut u64,
    cap: u64,
    mut out: Option<&mut Vec<u8>>,
) -> Option<()> {
    *total = total.checked_add(n).filter(|&t| t <= cap)?;
    let mut left = n;
    let mut buf = [0u8; 4096];
    while left > 0 {
        let want = usize::try_from(left).map_or(buf.len(), |l| l.min(buf.len()));
        z.read_exact(&mut buf[..want]).ok()?;
        if let Some(o) = out.as_deref_mut() {
            o.extend_from_slice(&buf[..want]);
        }
        left -= want as u64;
    }
    Some(())
}

fn take_u32(z: &mut impl std::io::Read, total: &mut u64, cap: u64) -> Option<u32> {
    let mut v = Vec::with_capacity(4);
    take_exact(z, 4, total, cap, Some(&mut v))?;
    Some(u32::from_le_bytes(v.try_into().ok()?))
}

/// The header of a Lepton stream, read and checked before the library decodes it (the layout
/// lepton_jpeg 0.5.8 writes and reads, `src/structs/lepton_header.rs`): a 28-byte fixed header
/// whose bytes 20..24 are the JPEG's length and 24..28 the length of the zlib-compressed header
/// that follows. Inflated, that header is `HDR`, a u32 LE length and the raw JPEG header (which
/// follows SOI), then recovery sections until its end, each a 3-byte marker: `P0D` and one byte;
/// `CRS`, a u32 count and that many u32s; `HH` with a count byte and 16 bytes per partition;
/// `FRS` or `GRB`, a u32 length and that many bytes; `EEE` and 28 bytes.
///
/// `None` (the step's `lepton stream`) when the stream does not have that shape or a length in
/// it could make the library allocate past what the primary image explains: the JPEG length,
/// the compressed header, the raw header, the garbage, the restart errors (summed) or four bytes
/// per restart count (summed) above `primary_len`; the inflated header above
/// [`header_cap`] (a marker cut short at its end counts its bytes); not exactly one `HH`
/// section, or one with no partition or partitions whose first rows go down; segment sizes
/// past what the library bounds them by (below); an unknown section; data ending inside a
/// section; or no frame header in the raw header. Segment sizes, as the library treats them
/// (`lepton_header.rs`, the early-EOF adjustment): without an `EEE` section all of them sum to
/// at most `primary_len`; with one, the library cuts the last segment to the JPEG length less
/// the garbage (2 bytes, the EOI it assumes, when there is no `GRB`), the raw header, SOI and
/// the other segments, so those must fit in the JPEG length (a raw header longer than the
/// library's parse of it only makes this stricter). Also refused, for the library's unchecked
/// arithmetic and indexing on them: the garbage (2 bytes without `GRB`) above the JPEG length
/// (`lepton_file_reader.rs:299` subtracts it, whatever the encoder version); a partition whose
/// overhang bits exceed 7 (`bit_writer.rs:143`); an `EEE` section with a `max_dpos` of
/// `u32::MAX` (`truncate_components.rs:69` adds one); a first restart error above 1
/// (`lepton_file_reader.rs:393` adds it to an 8-bit count); restart counts, when any are given,
/// fewer than the `FF DA` pairs of the raw header, and fewer than one when the raw header has
/// no `FF DA` (`jpeg_write.rs:323` indexes them by scan). Each of these is a stream the
/// library refuses too, or one it does not write: the library allocates each length it reads
/// at once (a `GRB` length of 4 GB was a fuzzing finding), and partitions whose rows overlap
/// each hold a coefficient buffer for their rows.
pub fn read_stream_header(stream: &[u8], primary_len: u64) -> Option<StreamHeader> {
    let fixed = stream.get(..28)?;
    let jpeg_len = u64::from(u32::from_le_bytes(fixed[20..24].try_into().ok()?));
    let compressed_len = u32::from_le_bytes(fixed[24..28].try_into().ok()?);
    if jpeg_len > primary_len || u64::from(compressed_len) > primary_len {
        return None;
    }
    let compressed = stream.get(28..28usize.checked_add(usize::try_from(compressed_len).ok()?)?)?;
    let mut z = flate2::read::ZlibDecoder::new(compressed);
    let cap = header_cap(primary_len);
    let mut total = 0u64;
    let mut marker = Vec::with_capacity(3);
    take_exact(&mut z, 3, &mut total, cap, Some(&mut marker))?;
    if marker != b"HDR" {
        return None;
    }
    let n = u64::from(take_u32(&mut z, &mut total, cap)?);
    if n > primary_len {
        return None;
    }
    let mut raw = vec![0xFF, 0xD8];
    take_exact(&mut z, n, &mut total, cap, Some(&mut raw))?;
    let (mut restarts, mut errors) = (0u64, 0u64);
    let mut segments: Option<Vec<u64>> = None;
    let (mut garbage, mut early_eof) = (2u64, false);
    let mut first_error: Option<u8> = None;
    loop {
        // The library ends the header at the first marker it cannot read whole.
        let mut m = [0u8; 3];
        let mut got = 0;
        while got < 3 {
            match std::io::Read::read(&mut z, &mut m[got..]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
        if got < 3 {
            total = total.checked_add(got as u64).filter(|&t| t <= cap)?;
            break;
        }
        total = total.checked_add(3).filter(|&t| t <= cap)?;
        match &m {
            b"P0D" => take_exact(&mut z, 1, &mut total, cap, None)?,
            b"CRS" => {
                let count = u64::from(take_u32(&mut z, &mut total, cap)?);
                restarts = restarts.checked_add(count)?;
                if restarts.saturating_mul(4) > primary_len {
                    return None;
                }
                take_exact(&mut z, count * 4, &mut total, cap, None)?;
            }
            [b'H', b'H', count] => {
                if segments.is_some() || *count == 0 {
                    return None;
                }
                let mut h = Vec::with_capacity(16 * usize::from(*count));
                take_exact(
                    &mut z,
                    16 * u64::from(*count),
                    &mut total,
                    cap,
                    Some(&mut h),
                )?;
                let mut last_row = 0u16;
                let mut sizes = Vec::with_capacity(usize::from(*count));
                for p in h.as_chunks::<16>().0 {
                    let row = u16::from_le_bytes([p[0], p[1]]);
                    if row < last_row {
                        return None;
                    }
                    last_row = row;
                    // The bits of its first byte the partition's writer starts from (0..=7).
                    if p[7] > 7 {
                        return None;
                    }
                    sizes.push(u64::from(u32::from_le_bytes(p[2..6].try_into().ok()?)));
                }
                segments = Some(sizes);
            }
            b"FRS" | b"GRB" => {
                let len = u64::from(take_u32(&mut z, &mut total, cap)?);
                if &m == b"FRS" {
                    errors = errors.checked_add(len)?;
                }
                if len > primary_len || errors > primary_len {
                    return None;
                }
                if &m == b"GRB" {
                    garbage = len;
                }
                if &m == b"FRS" && first_error.is_none() && len > 0 {
                    let mut b = Vec::with_capacity(1);
                    take_exact(&mut z, 1, &mut total, cap, Some(&mut b))?;
                    first_error = b.first().copied();
                    take_exact(&mut z, len - 1, &mut total, cap, None)?;
                } else {
                    take_exact(&mut z, len, &mut total, cap, None)?;
                }
            }
            b"EEE" => {
                let mut e = Vec::with_capacity(28);
                take_exact(&mut z, 28, &mut total, cap, Some(&mut e))?;
                // `max_dpos[0..4]` (bytes 12..28), each of which the library adds one to.
                let (dpos, _) = e.get(12..)?.as_chunks::<4>();
                if dpos.iter().any(|d| u32::from_le_bytes(*d) == u32::MAX) {
                    return None;
                }
                early_eof = true;
            }
            _ => return None,
        }
    }
    let segments = segments?;
    let (_last, others) = segments.split_last()?;
    let others: u64 = others.iter().sum();
    let fits = if early_eof {
        others + garbage + n + 2 <= jpeg_len
    } else {
        segments.iter().sum::<u64>() <= primary_len
    };
    if !fits {
        return None;
    }
    // The garbage (the EOI the library assumes without `GRB`) within the JPEG length, which the
    // library lowers by it; the first restart error at most 1, which the library adds to an 8-bit
    // count; restart counts, when given, one for every scan the raw header can hold (each
    // `FF DA` in it), which the library indexes by scan.
    if garbage > jpeg_len || first_error.is_some_and(|e| e > 1) {
        return None;
    }
    if restarts > 0 {
        let scans = raw.windows(2).filter(|w| *w == [0xFF, 0xDA]).count() as u64;
        if restarts < scans.max(1) {
            return None;
        }
    }
    Some(StreamHeader {
        frame: marker_scan(&raw).frame?,
        header_len: total,
        partitions: u32::try_from(segments.len()).ok()?,
    })
}

/// The decode threads lepton_jpeg runs at most: the `max_processor_threads` of the preset, which
/// no reader of this crate raises.
pub const LEPTON_THREADS: u64 = 8;

/// Per decode thread, lepton_jpeg's probability model and per-row state, an over-approximation
/// of `Box<Model>` (`src/structs/model.rs`) and the neighbour caches.
pub const THREAD_ALLOWANCE: u64 = 4 << 20;

/// An image's decode memory, the term its record declares and its reader checks: the larger of
/// [`memory_bound`] (revision 1.1's term: coefficients plus [`MODEL_ALLOWANCE`]) and this bound
/// on what lepton_jpeg 0.5.8 allocates for a stream whose header passed
/// [`read_stream_header`], with `B` the frame's blocks (each component's 8x8 blocks padded to
/// whole MCUs), `R` the blocks of one MCU row, `P` = min(partitions, [`LEPTON_THREADS`]) and
/// `H` the inflated header length:
///
/// - `128·B` for the coefficients (64 of 2 bytes per block), twice for a progressive frame
///   (SOF2), whose partitions' buffers stay alive while the library merges them into one;
/// - for a progressive frame, `1536·B` for one scan's output, which the library builds whole:
///   the content is at most 512 bytes per block (64 coefficients of a 16-bit Huffman code and
///   16 bits each, doubled for the zero byte stuffed after each `FF`), and the buffer grows from
///   128 KiB by `reserve` (doubling), so its capacity is under twice the content and, while it
///   moves, the old buffer is live too: three times the content;
/// - per decode thread, `P · (THREAD_ALLOWANCE + 128 KiB + 512·R)`: its model, its output
///   buffer, which the library hands on once it holds 128 KiB, and one MCU row's output;
/// - `4·H` for the inflated header, which the library holds, grows by doubling and copies for
///   its threads;
/// - `primary_len` for the output a baseline decode queues (each partition's output is cut at
///   its segment size, whose sum [`read_stream_header`] bounds by `primary_len`, or by the JPEG
///   length after the library's early-EOF cut);
/// - the stream's length, for the copy of the partitions' bytes the library queues per thread;
/// - 1 MiB for the rest (Huffman and quantisation tables, the inflater's window, channels).
pub fn image_term(h: &StreamHeader, primary_len: u64, stream_len: u64) -> u64 {
    let f = &h.frame;
    let hmax = f.sampling.iter().map(|s| s.0).max().unwrap_or(1).max(1);
    let vmax = f.sampling.iter().map(|s| s.1).max().unwrap_or(1).max(1);
    let mcus_x = u64::from(f.width).div_ceil(u64::from(8 * hmax));
    let mcus_y = u64::from(f.height).div_ceil(u64::from(8 * vmax));
    let row: u64 = f
        .sampling
        .iter()
        .map(|&(sh, sv)| mcus_x * u64::from(sh) * u64::from(sv))
        .sum();
    let blocks = row.saturating_mul(mcus_y);
    let progressive = f.marker == 0xC2;
    let coefficients = blocks.saturating_mul(if progressive { 256 } else { 128 });
    let scan = if progressive {
        blocks.saturating_mul(1536)
    } else {
        0
    };
    let threads = u64::from(h.partitions).min(LEPTON_THREADS);
    let per_thread = THREAD_ALLOWANCE
        .saturating_add(128 << 10)
        .saturating_add(row.saturating_mul(512));
    let honest = coefficients
        .saturating_add(scan)
        .saturating_add(threads.saturating_mul(per_thread))
        .saturating_add(h.header_len.saturating_mul(4))
        .saturating_add(primary_len)
        .saturating_add(stream_len)
        .saturating_add(1 << 20);
    memory_bound(f).max(honest)
}

/// Register the full reader's decoders (revision 1.1's `jpeg-reconstruct`, revision 1.2's
/// `deflate-reconstruct`, revision 1.3's `bwt`, revision 1.4's `png-filter`) on `archive`, so its
/// peeled files extract and verify. Call it after `set_priors` (which keeps it anyway).
pub fn register_full_reader<R: std::io::Read + std::io::Seek>(
    archive: &mut lpk_format::Archive<R>,
) {
    register_full_decoders(archive.registry_mut());
}

/// The decoders of [`register_full_reader`] on a bare registry (a decode without an archive
/// behind it, as the fuzz targets run).
pub fn register_full_decoders(registry: &mut lpk_format::Registry) {
    registry.register(PrimitiveId::JpegReconstruct, Box::new(JpegDecoder));
    registry.register(
        PrimitiveId::DeflateReconstruct,
        Box::new(super::deflate::DeflateDecoder),
    );
    registry.register(PrimitiveId::Bwt, Box::new(crate::bwt::BwtDecoder));
    registry.register(
        PrimitiveId::PngFilter,
        Box::new(super::png_filter::PngFilterDecoder),
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod decode_threads_tests {
    use super::*;
    use std::sync::Arc;

    fn stream() -> (Vec<u8>, Vec<u8>) {
        let jpeg = include_bytes!("../../tests/fixtures/baseline.jpg").to_vec();
        let mut f = features(u32::try_from(jpeg.len()).unwrap());
        f.max_processor_threads = 1;
        let mut w = Cursor::new(Vec::new());
        encode_lepton(
            &mut Cursor::new(&jpeg[..]),
            &mut w,
            &f,
            &DEFAULT_THREAD_POOL,
        )
        .unwrap();
        (jpeg, w.into_inner())
    }

    fn decode(s: &[u8], len: usize) -> Vec<u8> {
        let mut f = features(u32::try_from(len).unwrap());
        budgeted_decode(s, len, &mut f).ok().unwrap()
    }

    /// One token: the stream decodes on the calling thread (the library's single-thread pool
    /// panics if asked to run anything elsewhere), and the token is the caller's again after.
    #[test]
    fn one_thread_decodes_on_the_calling_thread() {
        let (jpeg, s) = stream();
        let budget = DecodeThreads::new(1);
        let out = with_decode_threads(Some(Arc::clone(&budget)), || {
            let _t = budget.hold();
            let out = decode(&s, jpeg.len());
            assert_eq!(
                budget.free(),
                0,
                "the caller's token is held through the decode"
            );
            out
        });
        assert_eq!(out, jpeg);
        assert_eq!(budget.free(), 1);
    }

    /// Several workers on a budget smaller than their streams' threads: every decode is right
    /// and every token comes back; outside a budget the default pool is used.
    #[test]
    fn workers_share_the_budget_and_give_every_token_back() {
        let (jpeg, s) = stream();
        let budget = DecodeThreads::new(3);
        std::thread::scope(|sc| {
            for _ in 0..3 {
                sc.spawn(|| {
                    with_decode_threads(Some(Arc::clone(&budget)), || {
                        for _ in 0..4 {
                            let _t = budget.hold();
                            assert_eq!(decode(&s, jpeg.len()), jpeg);
                        }
                    });
                });
            }
        });
        assert!(
            budget.wait_all_free(std::time::Duration::from_secs(10)),
            "tokens still out: {} of {} free",
            budget.free(),
            budget.total()
        );
        assert_eq!(decode(&s, jpeg.len()), jpeg);
    }
}
