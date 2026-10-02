//! `probe jpeg` (PLAN P0-4): `lepton_jpeg` encode, decode and byte comparison over every JPEG of
//! the classes `photo-jpeg` and `photo-jpeg-edited`.
//!
//! Per file the probe makes its own marker scan (frame type, components, precision, size, restart
//! interval, MPF and gain-map markers, bytes after the first EOI), encodes with the library's
//! write features, decodes and compares with the input, in two timed laps: the library's default
//! threading and one processor thread. A file the library cannot handle, or whose decoded bytes
//! differ, is a failure with a cause; it counts as stored as-is (output size = input size).
//!
//! Failure cause mapping: see [`classify`] (the library's `ExitCode` and message decide; the
//! marker scan only backs up `UnsupportedJpeg`). Gain-map, multi-picture and trailing data are
//! never a cause: the library keeps the bytes after the first EOI as opaque data, so such files
//! are counted among the successes and reported separately. The code name and the library's
//! message are recorded with every failure.
//!
//! Every timed lap runs alone, on data in memory. Only this file and its tests change for the
//! probe.

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};

use anyhow::Result;
use lepton_jpeg::{
    decode_lepton, encode_lepton, EnabledFeatures, ExitCode, LeptonThreadPool, SingleThreadPool,
    DEFAULT_THREAD_POOL,
};
use serde::{Deserialize, Serialize};

use super::{mbps, md_header, md_table, pct, timed, Ctx, Envelope, Output};

pub const NAME: &str = "jpeg";

/// The classes the probe reads, in the order of the result.
pub const CLASSES: [&str; 2] = ["photo-jpeg", "photo-jpeg-edited"];

/// Bytes after the first image's EOI above which a failure is blamed on trailing data.
pub const TRAILING_LIMIT: u64 = 4 * 1024 * 1024;

/// The name of the library's feature preset the probe uses.
const PRESET: &str = "compat_lepton_vector_write";

// ---------------------------------------------------------------------------------------------
// The result

/// The probe's own part of the result file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    /// The `lepton_jpeg` feature values of the default-threading lap.
    pub features: Features,
    /// `max_processor_threads` of the one-thread lap.
    pub one_thread_processor_threads: u32,
    /// The library's thread pool of the one-thread lap (`SingleThreadPool` runs everything
    /// inline on the calling thread; the default lap uses `DEFAULT_THREAD_POOL`).
    pub one_thread_pool: String,
    /// Bytes after the first EOI above which a failure counts as trailing data.
    pub trailing_limit_bytes: u64,
    /// One entry per class present in the corpus, in the order of [`CLASSES`].
    pub classes: Vec<ClassData>,
}

/// The library's feature values (`EnabledFeatures`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Features {
    /// The library's preset these values come from.
    pub preset: String,
    pub progressive: bool,
    pub reject_dqts_with_zeros: bool,
    pub use_16bit_dc_estimate: bool,
    pub use_16bit_adv_predict: bool,
    pub accept_invalid_dht: bool,
    pub stop_reading_at_eoi: bool,
    pub max_jpeg_width: u32,
    pub max_jpeg_height: u32,
    pub max_partitions: u32,
    pub max_processor_threads: u32,
    pub max_jpeg_file_size: u32,
}

impl Features {
    /// The library's write preset as the probe records it.
    pub fn write_preset() -> Features {
        Features::of(&write_features())
    }

    fn of(f: &EnabledFeatures) -> Features {
        Features {
            preset: PRESET.to_string(),
            progressive: f.progressive,
            reject_dqts_with_zeros: f.reject_dqts_with_zeros,
            use_16bit_dc_estimate: f.use_16bit_dc_estimate,
            use_16bit_adv_predict: f.use_16bit_adv_predict,
            accept_invalid_dht: f.accept_invalid_dht,
            stop_reading_at_eoi: f.stop_reading_at_eoi,
            max_jpeg_width: f.max_jpeg_width,
            max_jpeg_height: f.max_jpeg_height,
            max_partitions: f.max_partitions,
            max_processor_threads: f.max_processor_threads,
            max_jpeg_file_size: f.max_jpeg_file_size,
        }
    }
}

fn write_features() -> EnabledFeatures {
    EnabledFeatures::compat_lepton_vector_write()
}

/// The files of one class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassData {
    pub class: String,
    /// Number of files the manifest lists for the class: every one is in `files` or
    /// `not_jpeg`, by index 0..manifest_files.
    pub manifest_files: u32,
    /// Sub-sources: the top-level folders inside the class (`group-<n>` for a private corpus;
    /// the empty label is the class folder itself).
    pub group_labels: Vec<String>,
    /// Indices of files that do not start with a JPEG SOI marker, in manifest order.
    pub not_jpeg: Vec<u32>,
    /// The JPEG files, in manifest order.
    pub files: Vec<FileRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    /// Position of the file within the class's manifest list.
    pub index: u32,
    /// Index into the class's `group_labels`.
    pub group: u32,
    /// Manifest path; absent for a private corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub bytes: u64,
    pub scan: Scan,
    /// Size of the Lepton file (default-threading lap); absent for a failed file.
    pub lepton_bytes: Option<u64>,
    /// Why the file is stored as-is; absent for a file that was recompressed and verified.
    pub failure: Option<Failure>,
    /// Encode and decode of the default-threading lap; absent for a failed file.
    pub default_threads: Option<Timing>,
    /// Encode and decode with one processor thread; absent for a failed file.
    pub one_thread: Option<Timing>,
    /// Whether the one-thread lap produced the same Lepton bytes as the default lap.
    pub one_thread_output_identical: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timing {
    pub encode_seconds: f64,
    pub decode_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub cause: Cause,
    /// The library's `ExitCode` name (`decode:` prefixed for the decode side, `panic` when the
    /// library panicked); absent for a mismatch of bytes.
    pub exit_code: Option<String>,
    /// The library's message with its context markers (everything from the first line break)
    /// removed: fixed texts and numbers, no file names. Absent for a mismatch of bytes and a
    /// panic.
    #[serde(default)]
    pub message: Option<String>,
    /// The lap in which the failure happened.
    pub lap: LapKind,
}

/// Which of the two laps of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LapKind {
    Default,
    OneThread,
}

/// Why a file is stored as-is. (A gain-map or multi-picture file never fails because of its
/// extra data: the library keeps everything after the first EOI as opaque bytes, so there is no
/// such cause; such files are counted among the successes, see the size table.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The library says progressive files are disabled (`ProgressiveUnsupported`).
    Progressive,
    /// The library rejects the progressive scan script of the file.
    ProgressiveRejected,
    FourComponents,
    Arithmetic,
    DimensionCap,
    /// The file exceeds the library's file-size cap.
    TrailingData,
    VerificationMismatch,
    Other,
}

impl Cause {
    pub const ALL: [Cause; 8] = [
        Cause::Progressive,
        Cause::ProgressiveRejected,
        Cause::FourComponents,
        Cause::Arithmetic,
        Cause::DimensionCap,
        Cause::TrailingData,
        Cause::VerificationMismatch,
        Cause::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Cause::Progressive => "progressive (disabled)",
            Cause::ProgressiveRejected => "progressive (rejected by the library)",
            Cause::FourComponents => "four components (CMYK)",
            Cause::Arithmetic => "arithmetic-coded",
            Cause::DimensionCap => "dimension cap",
            Cause::TrailingData => "file too large (size cap)",
            Cause::VerificationMismatch => "verification mismatch",
            Cause::Other => "other",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The marker scan (independent of the library)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    /// SOF0
    Baseline,
    /// SOF1
    ExtendedSequential,
    /// SOF2
    Progressive,
    /// SOF3
    Lossless,
    /// SOF9, SOF10, SOF11, SOF13 to SOF15
    Arithmetic,
    /// SOF5 to SOF7 (hierarchical, Huffman)
    Differential,
    /// SOF8 (JPEG extensions)
    Other,
}

impl FrameKind {
    pub const ALL: [FrameKind; 7] = [
        FrameKind::Baseline,
        FrameKind::ExtendedSequential,
        FrameKind::Progressive,
        FrameKind::Lossless,
        FrameKind::Arithmetic,
        FrameKind::Differential,
        FrameKind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FrameKind::Baseline => "baseline",
            FrameKind::ExtendedSequential => "extended sequential",
            FrameKind::Progressive => "progressive",
            FrameKind::Lossless => "lossless",
            FrameKind::Arithmetic => "arithmetic",
            FrameKind::Differential => "differential",
            FrameKind::Other => "other frame",
        }
    }

    fn of_marker(m: u8) -> FrameKind {
        match m {
            0xC0 => FrameKind::Baseline,
            0xC1 => FrameKind::ExtendedSequential,
            0xC2 => FrameKind::Progressive,
            0xC3 => FrameKind::Lossless,
            0xC5..=0xC7 => FrameKind::Differential,
            0xC9..=0xCB | 0xCD..=0xCF => FrameKind::Arithmetic,
            _ => FrameKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub kind: FrameKind,
    pub components: u32,
    pub precision: u32,
    pub width: u32,
    pub height: u32,
}

/// What the marker scan found in one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scan {
    /// The first frame header; absent when none was found.
    pub frame: Option<Frame>,
    /// A DRI segment with a non-zero interval.
    pub restart_interval: bool,
    /// An APP2 segment starting with `MPF\0` (multi-picture).
    pub mpf: bool,
    /// A gain-map marker: an APP1 segment mentioning `hdrgm` (Adobe/Google XMP) or `HDRGainMap`
    /// (Apple), or an APP2 segment starting with `urn:iso:std:iso:ts:21496:-1` (ISO 21496-1).
    pub gain_map_marker: bool,
    /// An APP14 segment starting with `Adobe`.
    pub adobe: bool,
    /// Start-of-scan segments before the first EOI.
    pub scans: u32,
    /// The first image's EOI was reached.
    pub eoi_found: bool,
    /// Bytes after the first image's EOI (zero without an EOI).
    pub trailing_bytes: u64,
    /// Why the scan stopped before an EOI, if it did.
    pub problem: Option<String>,
}

fn be16(b: &[u8]) -> u32 {
    u32::from(b.first().copied().unwrap_or(0)) << 8 | u32::from(b.get(1).copied().unwrap_or(0))
}

/// Scan the marker structure of a JPEG file: segment headers are walked by their length fields,
/// entropy-coded data is skipped up to the next real marker, and scanning stops at the first EOI.
/// Never panics; a damaged file ends the scan with `problem` set.
pub fn marker_scan(data: &[u8]) -> Scan {
    let mut s = Scan {
        frame: None,
        restart_interval: false,
        mpf: false,
        gain_map_marker: false,
        adobe: false,
        scans: 0,
        eoi_found: false,
        trailing_bytes: 0,
        problem: None,
    };
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        s.problem = Some("no SOI marker".to_string());
        return s;
    }
    let mut pos = 2usize;
    loop {
        if data.get(pos) != Some(&0xFF) {
            s.problem = Some(if pos >= data.len() {
                "ends before an EOI marker".to_string()
            } else {
                "a marker was expected but not found".to_string()
            });
            return s;
        }
        while data.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let Some(&m) = data.get(pos) else {
            s.problem = Some("ends inside a marker".to_string());
            return s;
        };
        pos += 1;
        match m {
            0x00 => {
                s.problem = Some("stuffed zero outside entropy-coded data".to_string());
                return s;
            }
            0xD9 => {
                s.eoi_found = true;
                s.trailing_bytes = (data.len() - pos) as u64;
                return s;
            }
            0x01 | 0xD0..=0xD8 => continue,
            _ => {}
        }
        let Some(len_bytes) = data.get(pos..pos + 2) else {
            s.problem = Some("ends inside a segment length".to_string());
            return s;
        };
        let len = be16(len_bytes) as usize;
        let Some(payload) = (len >= 2).then(|| data.get(pos + 2..pos + len)).flatten() else {
            s.problem = Some("a segment is shorter than its length field".to_string());
            return s;
        };
        match m {
            0xC0..=0xC3 | 0xC5..=0xCB | 0xCD..=0xCF => {
                if s.frame.is_none() {
                    if payload.len() < 6 {
                        s.problem = Some("a frame header is too short".to_string());
                        return s;
                    }
                    s.frame = Some(Frame {
                        kind: FrameKind::of_marker(m),
                        components: u32::from(payload[5]),
                        precision: u32::from(payload[0]),
                        height: be16(&payload[1..3]),
                        width: be16(&payload[3..5]),
                    });
                }
            }
            0xDD => s.restart_interval |= be16(payload) != 0,
            0xE1 => {
                s.gain_map_marker |= payload.windows(5).any(|w| w == b"hdrgm")
                    || payload.windows(10).any(|w| w == b"HDRGainMap")
            }
            0xE2 => {
                s.mpf |= payload.starts_with(b"MPF\0");
                s.gain_map_marker |= payload.starts_with(b"urn:iso:std:iso:ts:21496:-1");
            }
            0xEE => s.adobe |= payload.starts_with(b"Adobe"),
            _ => {}
        }
        pos += len;
        if m == 0xDA {
            s.scans += 1;
            // Entropy-coded data: skip to the next marker that is not a stuffed zero or a
            // restart marker.
            let mut p = pos;
            loop {
                let Some(i) = data[p.min(data.len())..].iter().position(|&b| b == 0xFF) else {
                    s.problem = Some("ends inside entropy-coded data".to_string());
                    return s;
                };
                let q = p + i;
                match data.get(q + 1) {
                    None => {
                        s.problem = Some("ends inside entropy-coded data".to_string());
                        return s;
                    }
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
// ---------------------------------------------------------------------------------------------
// Encode, decode, compare

/// What the one-thread lap runs on.
pub const ONE_THREAD_POOL: &str = "SingleThreadPool";

/// What went wrong in one lap.
#[derive(Debug, Clone, PartialEq)]
pub enum Fail {
    /// The library returned this code (with its cleaned message) on the encode
    /// (`decode: false`) or decode side.
    Code {
        code: ExitCode,
        message: String,
        decode: bool,
    },
    /// The library panicked.
    Panic { decode: bool },
    /// The decoded bytes differ from the input.
    Mismatch,
}

/// One verified lap.
#[derive(Debug)]
pub struct Lap {
    pub lepton: Vec<u8>,
    pub encode_seconds: f64,
    pub decode_seconds: f64,
}

/// The library's message without its context markers (everything from the first line break),
/// printable ASCII only and at most 200 characters.
pub fn clean_message(m: &str) -> String {
    m.lines()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(200)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Encode `input` on `pool`, decode the result and compare with `input`. `after_decode` may
/// change the decoded bytes before the comparison (tests use it to provoke a mismatch).
pub fn roundtrip(
    input: &[u8],
    features: &EnabledFeatures,
    pool: &dyn LeptonThreadPool,
    after_decode: &dyn Fn(&mut Vec<u8>),
) -> std::result::Result<Lap, Fail> {
    let sink = Vec::with_capacity(input.len() / 2 + 1024);
    let (enc, encode_seconds) = timed(|| {
        catch_unwind(AssertUnwindSafe(|| {
            let mut reader = Cursor::new(input);
            let mut writer = Cursor::new(sink);
            encode_lepton(&mut reader, &mut writer, features, pool).map(|_| writer.into_inner())
        }))
    });
    let lepton = match enc {
        Err(_) => return Err(Fail::Panic { decode: false }),
        Ok(Err(e)) => {
            return Err(Fail::Code {
                code: e.exit_code(),
                message: clean_message(e.message()),
                decode: false,
            })
        }
        Ok(Ok(v)) => v,
    };
    let out = Vec::with_capacity(input.len());
    let (dec, decode_seconds) = timed(|| {
        catch_unwind(AssertUnwindSafe(|| {
            let mut reader = Cursor::new(&lepton[..]);
            let mut writer = Cursor::new(out);
            decode_lepton(&mut reader, &mut writer, features, pool).map(|_| writer.into_inner())
        }))
    });
    let mut decoded = match dec {
        Err(_) => return Err(Fail::Panic { decode: true }),
        Ok(Err(e)) => {
            return Err(Fail::Code {
                code: e.exit_code(),
                message: clean_message(e.message()),
                decode: true,
            })
        }
        Ok(Ok(v)) => v,
    };
    after_decode(&mut decoded);
    if decoded != input {
        return Err(Fail::Mismatch);
    }
    Ok(Lap {
        lepton,
        encode_seconds,
        decode_seconds,
    })
}

fn code_name(code: ExitCode) -> String {
    format!("{code:?}")
}

/// The library's `ExitCode` names the probe may record (`decode:` prefixed for the decode
/// side); `panic` and `decode:panic` stand for a panic.
pub const EXIT_CODE_NAMES: [&str; 22] = [
    "AssertionFailure",
    "ShortRead",
    "Unsupported4Colors",
    "CoefficientOutOfRange",
    "StreamInconsistent",
    "ProgressiveUnsupported",
    "SamplingBeyondTwoUnsupported",
    "VersionUnsupported",
    "OsError",
    "UnsupportedJpeg",
    "UnsupportedJpegWithZeroIdct0",
    "InvalidResetCode",
    "InvalidPadding",
    "BadLeptonFile",
    "ChannelFailure",
    "IntegerCastOverflow",
    "VerificationLengthMismatch",
    "VerificationContentMismatch",
    "SyntaxError",
    "FileNotFound",
    "ExternalVerificationFailed",
    "OutOfMemory",
];

/// Whether a recorded exit-code string is a known name, a `decode:` name or a panic.
pub fn known_exit_code(s: &str) -> bool {
    let bare = s.strip_prefix("decode:").unwrap_or(s);
    bare == "panic" || EXIT_CODE_NAMES.contains(&bare)
}

/// The library's message of a progressive file whose scan script it rejects.
pub fn is_progressive_rejection(m: &str) -> bool {
    m.starts_with("progress")
        || m.contains("spectral selection")
        || m.contains("successive approximation")
}

/// The cause of a failure. The library's `ExitCode` decides: `Unsupported4Colors` is four
/// components, `ProgressiveUnsupported` is progressive (disabled), the two `Verification*` codes
/// and a difference of bytes are a verification mismatch. `UnsupportedJpeg` is the library's
/// catch-all and is explained by its message (arithmetic coding, dimensions over the caps, a file
/// over the size cap, a progressive scan script it rejects) and, failing that, by the marker scan
/// (arithmetic frame, four components, dimensions). Every other code, whatever the marker scan
/// says, is `other` with its code name: gain-map, multi-picture and trailing data never make the
/// library fail by themselves. A decode-side error is `other` (a `Verification*` code excepted)
/// with the code name prefixed `decode:`.
pub fn classify(fail: &Fail, lap: LapKind, scan: &Scan, features: &EnabledFeatures) -> Failure {
    let mk = |cause, exit_code: Option<String>, message: Option<String>| Failure {
        cause,
        exit_code,
        message,
        lap,
    };
    match fail {
        Fail::Mismatch => mk(Cause::VerificationMismatch, None, None),
        Fail::Panic { decode } => mk(
            Cause::Other,
            Some(if *decode { "decode:panic" } else { "panic" }.to_string()),
            None,
        ),
        Fail::Code {
            code,
            message,
            decode,
        } => {
            let name = code_name(*code);
            let shown = if *decode {
                format!("decode:{name}")
            } else {
                name
            };
            let verification = matches!(
                code,
                ExitCode::VerificationLengthMismatch | ExitCode::VerificationContentMismatch
            );
            let cause = if verification {
                Cause::VerificationMismatch
            } else if *decode {
                Cause::Other
            } else {
                match code {
                    ExitCode::Unsupported4Colors => Cause::FourComponents,
                    ExitCode::ProgressiveUnsupported => Cause::Progressive,
                    ExitCode::UnsupportedJpeg => unsupported_jpeg_cause(message, scan, features),
                    _ => Cause::Other,
                }
            };
            mk(cause, Some(shown), Some(message.clone()))
        }
    }
}

/// The cause of an `UnsupportedJpeg`, from the library's message first and the marker scan second.
fn unsupported_jpeg_cause(message: &str, scan: &Scan, features: &EnabledFeatures) -> Cause {
    if message.contains("arithm") {
        return Cause::Arithmetic;
    }
    if message.starts_with("image dimensions larger") {
        return Cause::DimensionCap;
    }
    if message.contains("too large to encode") {
        return Cause::TrailingData;
    }
    if is_progressive_rejection(message) {
        return Cause::ProgressiveRejected;
    }
    let frame = scan.frame.as_ref();
    if frame.is_some_and(|f| f.kind == FrameKind::Arithmetic) {
        Cause::Arithmetic
    } else if frame.is_some_and(|f| f.components == 4) {
        Cause::FourComponents
    } else if frame
        .is_some_and(|f| f.width > features.max_jpeg_width || f.height > features.max_jpeg_height)
    {
        Cause::DimensionCap
    } else {
        Cause::Other
    }
}

/// Measure one JPEG file: the marker scan, the default-threading lap (`DEFAULT_THREAD_POOL`) and
/// the one-thread lap (`SingleThreadPool`, inline).
pub fn measure(
    input: &[u8],
    features: &EnabledFeatures,
    one_thread: &EnabledFeatures,
    after_decode: &dyn Fn(&mut Vec<u8>),
) -> FileRecord {
    let scan = marker_scan(input);
    let mut rec = FileRecord {
        index: 0,
        group: 0,
        path: None,
        bytes: input.len() as u64,
        scan,
        lepton_bytes: None,
        failure: None,
        default_threads: None,
        one_thread: None,
        one_thread_output_identical: None,
    };
    let first = match roundtrip(input, features, &DEFAULT_THREAD_POOL, after_decode) {
        Ok(lap) => lap,
        Err(fail) => {
            rec.failure = Some(classify(&fail, LapKind::Default, &rec.scan, features));
            return rec;
        }
    };
    let single = SingleThreadPool::default();
    let second = match roundtrip(input, one_thread, &single, after_decode) {
        Ok(lap) => lap,
        Err(fail) => {
            rec.failure = Some(classify(&fail, LapKind::OneThread, &rec.scan, features));
            return rec;
        }
    };
    rec.lepton_bytes = Some(first.lepton.len() as u64);
    rec.one_thread_output_identical = Some(first.lepton == second.lepton);
    rec.default_threads = Some(Timing {
        encode_seconds: first.encode_seconds,
        decode_seconds: first.decode_seconds,
    });
    rec.one_thread = Some(Timing {
        encode_seconds: second.encode_seconds,
        decode_seconds: second.decode_seconds,
    });
    rec
}

/// One untimed encode and decode in each setting, so the start-up of the library's thread pool
/// and the first allocations do not land in the first file's laps.
fn warm_up(features: &EnabledFeatures, one_thread: &EnabledFeatures) {
    let (w, h) = (64usize, 64usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            rgb.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8]);
        }
    }
    if let Ok(jpeg) = crate::corpus::derive::jpegenc::encode(&rgb, w, h, 80, false) {
        let _ = roundtrip(&jpeg, features, &DEFAULT_THREAD_POOL, &|_| {});
        let _ = roundtrip(&jpeg, one_thread, &SingleThreadPool::default(), &|_| {});
    }
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let features = write_features();
    let mut one = features.clone();
    one.max_processor_threads = 1;
    warm_up(&features, &one);
    let mut classes = Vec::new();
    let mut notes = Vec::new();
    for class in CLASSES {
        let Some(files) = ctx.class_files(class) else {
            notes.push(format!(
                "the corpus has no class `{class}`: nothing was measured for it"
            ));
            continue;
        };
        let groups = ctx.groups(class, 1);
        let mut group_of: HashMap<&str, u32> = HashMap::new();
        for (gi, g) in groups.iter().enumerate() {
            for f in &g.files {
                group_of.insert(f.path.as_str(), u32::try_from(gi)?);
            }
        }
        let mut data = ClassData {
            class: class.to_string(),
            manifest_files: u32::try_from(files.len())?,
            group_labels: groups.iter().map(|g| g.label.clone()).collect(),
            not_jpeg: Vec::new(),
            files: Vec::new(),
        };
        for (i, f) in files.iter().enumerate() {
            let index = u32::try_from(i)?;
            let bytes = ctx.read_file(class, f)?;
            if bytes.get(..2) != Some(&[0xFF, 0xD8]) {
                data.not_jpeg.push(index);
                continue;
            }
            let group = group_of.get(f.path.as_str()).copied().ok_or_else(|| {
                anyhow::anyhow!(
                    "internal error: file {index} of class `{class}` belongs to no folder group"
                )
            })?;
            let mut rec = measure(&bytes, &features, &one, &|_| {});
            rec.index = index;
            rec.group = group;
            rec.path = ctx.label(f);
            data.files.push(rec);
        }
        classes.push(data);
    }
    let threads = features.max_processor_threads;
    let data = Data {
        features: Features::of(&features),
        one_thread_processor_threads: 1,
        one_thread_pool: ONE_THREAD_POOL.to_string(),
        trailing_limit_bytes: TRAILING_LIMIT,
        classes,
    };
    let mut out = Output::new(data, threads).note(
        "library threads: the default lap uses the library's default pool and lets it use up to \
         max_processor_threads processor threads (fewer when the image has fewer partitions); the \
         one-thread lap runs inline on the calling thread (SingleThreadPool, \
         max_processor_threads 1). One untimed warm-up encode and decode precede the first file",
    );
    out.notes.extend(notes);
    out.libraries
        .insert("lepton_jpeg".to_string(), lepton_jpeg::get_version_string());
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Validation rules

fn seconds_ok(s: f64) -> bool {
    s.is_finite() && s >= 0.0
}

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    let private = e.corpus.private;
    if d.features != Features::write_preset() {
        p.push(
            "/data/features: not the feature values of the library's write preset the probe uses"
                .to_string(),
        );
    }
    if d.one_thread_processor_threads != 1 {
        p.push("/data/one_thread_processor_threads: must be 1".to_string());
    }
    if d.one_thread_pool != ONE_THREAD_POOL {
        p.push(format!(
            "/data/one_thread_pool: `{}` (the probe uses {ONE_THREAD_POOL})",
            d.one_thread_pool
        ));
    }
    if d.trailing_limit_bytes != TRAILING_LIMIT {
        p.push("/data/trailing_limit_bytes: not the probe's limit".to_string());
    }
    if e.library_threads != d.features.max_processor_threads {
        p.push(format!(
            "/library_threads: {} but the features allow {} processor threads",
            e.library_threads, d.features.max_processor_threads
        ));
    }
    let mut last_class: Option<usize> = None;
    for (ci, c) in d.classes.iter().enumerate() {
        let at = format!("/data/classes/{ci}");
        match CLASSES.iter().position(|n| *n == c.class) {
            None => p.push(format!(
                "{at}/class: `{}` is not a class of this probe",
                c.class
            )),
            Some(pos) => {
                if last_class.is_some_and(|l| l >= pos) {
                    p.push(format!(
                        "{at}/class: classes must be in probe order, once each"
                    ));
                }
                last_class = Some(pos);
            }
        }
        let mut seen: Vec<u32> = c
            .files
            .iter()
            .map(|f| f.index)
            .chain(c.not_jpeg.iter().copied())
            .collect();
        seen.sort_unstable();
        let expected: Vec<u32> = (0..c.manifest_files).collect();
        if seen != expected {
            p.push(format!(
                "{at}/manifest_files: {} files are listed in the manifest but the records cover \
                 {} indices (each must appear exactly once in files or not_jpeg)",
                c.manifest_files,
                seen.len()
            ));
        }
        if c.not_jpeg.windows(2).any(|w| w[0] >= w[1]) {
            p.push(format!(
                "{at}/not_jpeg: must be in strictly increasing order"
            ));
        }
        for (i, f) in c.files.iter().enumerate() {
            check_file(&mut p, &format!("{at}/files/{i}"), c, f, private, i, d);
        }
    }
    p
}

fn check_file(
    p: &mut Vec<String>,
    at: &str,
    c: &ClassData,
    f: &FileRecord,
    private: bool,
    i: usize,
    d: &Data,
) {
    if i > 0 && c.files[i - 1].index >= f.index {
        p.push(format!(
            "{at}/index: files must be in strictly increasing manifest order"
        ));
    }
    if f.group as usize >= c.group_labels.len() {
        p.push(format!(
            "{at}/group: {} but the class has {} groups",
            f.group,
            c.group_labels.len()
        ));
    }
    if private && f.path.is_some() {
        p.push(format!("{at}/path: a private corpus carries no file names"));
    }
    if !private && f.path.is_none() {
        p.push(format!("{at}/path: missing"));
    }
    if f.scan.trailing_bytes > f.bytes {
        p.push(format!(
            "{at}/scan/trailing_bytes: more than the file's bytes"
        ));
    }
    if !f.scan.eoi_found && f.scan.trailing_bytes != 0 {
        p.push(format!(
            "{at}/scan/trailing_bytes: bytes after an EOI that was not found"
        ));
    }
    match (&f.lepton_bytes, &f.failure) {
        (Some(l), None) => {
            if *l == 0 {
                p.push(format!("{at}/lepton_bytes: zero"));
            }
            for (name, t) in [
                ("default_threads", &f.default_threads),
                ("one_thread", &f.one_thread),
            ] {
                match t {
                    None => p.push(format!(
                        "{at}/{name}: missing for a file that was recompressed"
                    )),
                    Some(t) => {
                        if !seconds_ok(t.encode_seconds) || !seconds_ok(t.decode_seconds) {
                            p.push(format!("{at}/{name}: seconds must be non-negative numbers"));
                        }
                    }
                }
            }
            if f.one_thread_output_identical.is_none() {
                p.push(format!("{at}/one_thread_output_identical: missing"));
            }
        }
        (None, Some(fail)) => {
            if f.default_threads.is_some()
                || f.one_thread.is_some()
                || f.one_thread_output_identical.is_some()
            {
                p.push(format!(
                    "{at}: timings and the thread comparison belong to recompressed files only"
                ));
            }
            check_failure(p, at, f, fail, d);
        }
        _ => p.push(format!(
            "{at}: exactly one of lepton_bytes and failure must be present"
        )),
    }
}

fn check_failure(p: &mut Vec<String>, at: &str, f: &FileRecord, fail: &Failure, d: &Data) {
    let frame = f.scan.frame.as_ref();
    let code = fail.exit_code.as_deref();
    let bare = code.map(|c| c.strip_prefix("decode:").unwrap_or(c));
    let msg = fail.message.as_deref().unwrap_or("");
    if let Some(c) = code {
        if !known_exit_code(c) {
            p.push(format!(
                "{at}/failure/exit_code: `{c}` is not a name of the library's ExitCode"
            ));
        }
    }
    if msg.contains('\n') || msg.len() > 200 || !msg.is_ascii() {
        p.push(format!(
            "{at}/failure/message: must be one printable ASCII line of at most 200 characters"
        ));
    }
    let unsupported = code == Some("UnsupportedJpeg");
    let ok = match fail.cause {
        Cause::Arithmetic => {
            unsupported
                && (msg.contains("arithm")
                    || frame.is_some_and(|x| x.kind == FrameKind::Arithmetic))
        }
        Cause::FourComponents => {
            code == Some("Unsupported4Colors")
                || (unsupported && frame.is_some_and(|x| x.components == 4))
        }
        Cause::Progressive => code == Some("ProgressiveUnsupported"),
        Cause::ProgressiveRejected => unsupported && is_progressive_rejection(msg),
        Cause::DimensionCap => {
            unsupported
                && (msg.starts_with("image dimensions larger")
                    || frame.is_some_and(|x| {
                        x.width > d.features.max_jpeg_width || x.height > d.features.max_jpeg_height
                    }))
        }
        Cause::TrailingData => unsupported && msg.contains("too large to encode"),
        Cause::VerificationMismatch => {
            bare.is_none()
                || matches!(
                    bare,
                    Some("VerificationLengthMismatch" | "VerificationContentMismatch")
                )
        }
        Cause::Other => code.is_some(),
    };
    if !ok {
        p.push(format!(
            "{at}/failure: cause `{}` is not supported by the exit code, message and marker scan",
            fail.cause.label()
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// The tables

#[derive(Default)]
struct Agg {
    files: u64,
    bytes: u64,
    ok_files: u64,
    ok_input: u64,
    ok_lepton: u64,
    fail_files: u64,
    fail_bytes: u64,
    /// Bytes after the first EOI of the recompressed files.
    ok_after_eoi: u64,
    /// Recompressed files with an MPF or gain-map marker, and their bytes.
    ok_multi_files: u64,
    ok_multi_bytes: u64,
    causes: [(u64, u64); 8],
    default_encode: f64,
    default_decode: f64,
    one_encode: f64,
    one_decode: f64,
    identical: u64,
    differ: u64,
}

impl Agg {
    fn of(files: &[&FileRecord]) -> Agg {
        let mut a = Agg::default();
        for f in files {
            a.files += 1;
            a.bytes += f.bytes;
            match (&f.lepton_bytes, &f.failure) {
                (Some(l), _) => {
                    a.ok_files += 1;
                    a.ok_input += f.bytes;
                    a.ok_lepton += l;
                    a.ok_after_eoi += f.scan.trailing_bytes;
                    if f.scan.mpf || f.scan.gain_map_marker {
                        a.ok_multi_files += 1;
                        a.ok_multi_bytes += f.bytes;
                    }
                    if let Some(t) = &f.default_threads {
                        a.default_encode += t.encode_seconds;
                        a.default_decode += t.decode_seconds;
                    }
                    if let Some(t) = &f.one_thread {
                        a.one_encode += t.encode_seconds;
                        a.one_decode += t.decode_seconds;
                    }
                    match f.one_thread_output_identical {
                        Some(true) => a.identical += 1,
                        Some(false) => a.differ += 1,
                        None => {}
                    }
                }
                (None, fail) => {
                    a.fail_files += 1;
                    a.fail_bytes += f.bytes;
                    if let Some(fail) = fail {
                        if let Some(i) = Cause::ALL.iter().position(|c| *c == fail.cause) {
                            a.causes[i].0 += 1;
                            a.causes[i].1 += f.bytes;
                        }
                    }
                }
            }
        }
        a
    }

    fn after_fallback(&self) -> u64 {
        self.ok_lepton + self.fail_bytes
    }
}

/// A label of a scope: `all`, `<class>` or `<class> / <folder>`.
fn scopes(d: &Data) -> Vec<(String, Vec<&FileRecord>)> {
    let mut out: Vec<(String, Vec<&FileRecord>)> = Vec::new();
    out.push((
        "all".to_string(),
        d.classes.iter().flat_map(|c| c.files.iter()).collect(),
    ));
    for c in &d.classes {
        out.push((c.class.clone(), c.files.iter().collect()));
        for (gi, label) in c.group_labels.iter().enumerate() {
            let files: Vec<&FileRecord> =
                c.files.iter().filter(|f| f.group as usize == gi).collect();
            if files.is_empty() {
                continue;
            }
            let shown = if label.is_empty() {
                "(class folder)"
            } else {
                label
            };
            out.push((format!("{} / {}", c.class, shown), files));
        }
    }
    out
}

fn gain(whole: u64, part: u64) -> String {
    if whole == 0 {
        "n/a".to_string()
    } else {
        format!(
            "{:.2}%",
            100.0 * (whole as f64 - part as f64) / whole as f64
        )
    }
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    let f = &d.features;
    s.push_str(&format!(
        "- features (`{}`): progressive {}, reject_dqts_with_zeros {}, use_16bit_dc_estimate {}, \
         use_16bit_adv_predict {}, accept_invalid_dht {}, stop_reading_at_eoi {}, max size {}x{}, \
         max_partitions {}, max_processor_threads {}, max_jpeg_file_size {}\n",
        f.preset,
        f.progressive,
        f.reject_dqts_with_zeros,
        f.use_16bit_dc_estimate,
        f.use_16bit_adv_predict,
        f.accept_invalid_dht,
        f.stop_reading_at_eoi,
        f.max_jpeg_width,
        f.max_jpeg_height,
        f.max_partitions,
        f.max_processor_threads,
        f.max_jpeg_file_size
    ));
    s.push_str(&format!(
        "- one-thread lap: max_processor_threads {} on the library's `{}`; the scan table counts \
         bytes after the first EOI over {} as over the limit\n",
        d.one_thread_processor_threads, d.one_thread_pool, d.trailing_limit_bytes
    ));
    for c in &d.classes {
        s.push_str(&format!(
            "- class `{}`: {} manifest files, {} not starting with a JPEG SOI marker (not measured)\n",
            c.class,
            c.manifest_files,
            c.not_jpeg.len()
        ));
    }
    s.push('\n');
    let scopes = scopes(d);
    let aggs: Vec<Agg> = scopes.iter().map(|(_, files)| Agg::of(files)).collect();

    s.push_str(
        "## Sizes (bytes)\n\nA recompressed file means: the primary image recompressed by Lepton \
         and the data after its EOI (if any) deflated by the library, verified byte for byte. \
         A failed file is stored as-is.\n\n",
    );
    let rows: Vec<Vec<String>> = scopes
        .iter()
        .zip(&aggs)
        .map(|((label, _), a)| {
            vec![
                label.clone(),
                a.files.to_string(),
                a.bytes.to_string(),
                a.ok_files.to_string(),
                a.ok_input.to_string(),
                a.ok_after_eoi.to_string(),
                format!("{} ({} bytes)", a.ok_multi_files, a.ok_multi_bytes),
                a.ok_lepton.to_string(),
                gain(a.ok_input, a.ok_lepton),
                a.fail_files.to_string(),
                a.fail_bytes.to_string(),
                a.after_fallback().to_string(),
                gain(a.bytes, a.after_fallback()),
                pct(a.fail_files, a.files),
            ]
        })
        .collect();
    s.push_str(&md_table(
        &[
            "Scope",
            "Files",
            "Input",
            "Recompressed files",
            "Their input",
            "Their bytes after EOI",
            "of them MPF / gain-map files",
            "After Lepton",
            "Gain on those",
            "Failed files",
            "Failed bytes",
            "After fallback",
            "Gain overall",
            "Failure rate",
        ],
        &rows,
    ));

    s.push_str("\n## Speed of the recompressed files, MB/s of JPEG bytes (and total seconds)\n\n");
    let rows: Vec<Vec<String>> = scopes
        .iter()
        .zip(&aggs)
        .map(|((label, _), a)| {
            let cell = |secs: f64| format!("{} ({:.3} s)", mbps(a.ok_input, secs), secs);
            vec![
                label.clone(),
                a.ok_files.to_string(),
                cell(a.default_encode),
                cell(a.default_decode),
                cell(a.one_encode),
                cell(a.one_decode),
                format!("{} / {}", a.identical, a.identical + a.differ),
            ]
        })
        .collect();
    s.push_str(&md_table(
        &[
            "Scope",
            "Files",
            "Encode, default threads",
            "Decode, default threads",
            "Encode, one thread",
            "Decode, one thread",
            "Same Lepton bytes on one thread",
        ],
        &rows,
    ));

    let mut headers = vec!["Scope"];
    headers.extend(Cause::ALL.iter().map(|c| c.label()));
    s.push_str("\n## Failures by cause, files\n\n");
    let rows: Vec<Vec<String>> = scopes
        .iter()
        .zip(&aggs)
        .map(|((label, _), a)| {
            let mut r = vec![label.clone()];
            r.extend(a.causes.iter().map(|c| c.0.to_string()));
            r
        })
        .collect();
    s.push_str(&md_table(&headers, &rows));
    s.push_str("\n## Failures by cause, input bytes\n\n");
    let rows: Vec<Vec<String>> = scopes
        .iter()
        .zip(&aggs)
        .map(|((label, _), a)| {
            let mut r = vec![label.clone()];
            r.extend(a.causes.iter().map(|c| c.1.to_string()));
            r
        })
        .collect();
    s.push_str(&md_table(&headers, &rows));

    // The library's code names behind the failures.
    let mut codes: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    for (_, files) in scopes.iter().take(1) {
        for f in files {
            if let Some(fail) = &f.failure {
                let code = fail
                    .exit_code
                    .clone()
                    .unwrap_or_else(|| "(bytes differ)".to_string());
                let lap = match fail.lap {
                    LapKind::Default => "default",
                    LapKind::OneThread => "one thread",
                };
                let message = format!("{} [{lap} lap]", fail.message.as_deref().unwrap_or(""));
                *codes
                    .entry((fail.cause.label().to_string(), code, message))
                    .or_insert(0) += 1;
            }
        }
    }
    s.push_str("\n## Failures by cause and library code, all files\n\n");
    if codes.is_empty() {
        s.push_str("No file failed.\n");
    } else {
        let rows: Vec<Vec<String>> = codes
            .into_iter()
            .map(|((cause, code, message), n)| vec![cause, code, message, n.to_string()])
            .collect();
        s.push_str(&md_table(
            &["Cause", "Library code", "Library message", "Files"],
            &rows,
        ));
    }

    s.push_str("\n## Marker scan, files\n\n");
    let mut headers: Vec<&str> = vec!["Scope"];
    headers.extend(FrameKind::ALL.iter().map(|k| k.label()));
    headers.extend([
        "no frame",
        "four components",
        "restart interval",
        "MPF",
        "gain-map marker",
        "Adobe APP14",
        "bytes after EOI",
        "over limit",
        "scan stopped early",
    ]);
    let rows: Vec<Vec<String>> = scopes
        .iter()
        .map(|(label, files)| {
            let count = |pred: &dyn Fn(&Scan) -> bool| {
                files.iter().filter(|f| pred(&f.scan)).count().to_string()
            };
            let mut r = vec![label.clone()];
            for k in FrameKind::ALL {
                r.push(count(&|sc| {
                    sc.frame.as_ref().is_some_and(|fr| fr.kind == k)
                }));
            }
            r.push(count(&|sc| sc.frame.is_none()));
            r.push(count(&|sc| {
                sc.frame.as_ref().is_some_and(|fr| fr.components == 4)
            }));
            r.push(count(&|sc| sc.restart_interval));
            r.push(count(&|sc| sc.mpf));
            r.push(count(&|sc| sc.gain_map_marker));
            r.push(count(&|sc| sc.adobe));
            r.push(count(&|sc| sc.trailing_bytes > 0));
            r.push(count(&|sc| sc.trailing_bytes > d.trailing_limit_bytes));
            r.push(count(&|sc| sc.problem.is_some()));
            r
        })
        .collect();
    s.push_str(&md_table(&headers, &rows));
    s
}

// ---------------------------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::*;
    use crate::corpus::derive::jpegenc::{encode, tests_support::picture};
    use crate::corpus::manifest::{Manifest, ManifestFile};
    use crate::probe::{execute, Config};
    use crate::run::validate::validate_dir;

    fn baseline(w: usize, h: usize) -> Vec<u8> {
        encode(&picture(w, h), w, h, 80, false).expect("encode")
    }

    fn progressive(w: usize, h: usize) -> Vec<u8> {
        encode(&picture(w, h), w, h, 80, true).expect("encode")
    }

    fn pair() -> (EnabledFeatures, EnabledFeatures) {
        let f = write_features();
        let mut one = f.clone();
        one.max_processor_threads = 1;
        (f, one)
    }

    fn offset_of(data: &[u8], marker: u8) -> usize {
        data.windows(2)
            .position(|w| w == [0xFF, marker])
            .expect("marker present")
    }

    fn nothing(_: &mut Vec<u8>) {}

    #[test]
    fn the_scan_describes_baseline_and_progressive_frames() {
        let b = marker_scan(&baseline(40, 24));
        let fr = b.frame.expect("frame");
        assert_eq!(fr.kind, FrameKind::Baseline);
        assert_eq!(
            (fr.components, fr.precision, fr.width, fr.height),
            (3, 8, 40, 24)
        );
        assert!(b.eoi_found && b.trailing_bytes == 0 && b.problem.is_none());
        assert_eq!(b.scans, 1);
        assert!(!b.restart_interval && !b.mpf && !b.gain_map_marker);
        let p = marker_scan(&progressive(40, 24));
        assert_eq!(p.frame.expect("frame").kind, FrameKind::Progressive);
        assert!(p.scans > 1 && p.eoi_found);
    }

    #[test]
    fn the_scan_finds_trailing_bytes_markers_and_damage() {
        let mut j = baseline(16, 16);
        let eoi = j.len();
        j.extend_from_slice(&[7u8; 100]);
        let s = marker_scan(&j);
        assert!(s.eoi_found && s.trailing_bytes == 100);
        assert_eq!(eoi + 100, j.len());

        // APP2 MPF, APP1 with hdrgm, APP14 Adobe and a DRI segment after SOI.
        let base = baseline(16, 16);
        let mut j = vec![0xFF, 0xD8];
        let segs: [(u8, &[u8]); 4] = [
            (0xE2, b"MPF\0xxxx"),
            (0xE1, b"http://ns.adobe.com/xap/1.0/\0 hdrgm:Version"),
            (0xEE, b"Adobe\0\0\0\0\0\0\0"),
            (0xDD, &[0, 4]),
        ];
        for (m, payload) in segs {
            j.extend_from_slice(&[0xFF, m]);
            j.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            j.extend_from_slice(payload);
        }
        j.extend_from_slice(&base[2..]);
        let s = marker_scan(&j);
        assert!(s.mpf && s.gain_map_marker && s.adobe && s.restart_interval);
        assert!(s.frame.is_some() && s.eoi_found);

        // Truncated at every length: never a panic, always an answer.
        for cut in 0..base.len() {
            let s = marker_scan(&base[..cut]);
            if cut < base.len() {
                assert!(!s.eoi_found, "cut at {cut}");
            }
        }
        assert!(marker_scan(b"hello").problem.is_some());
        assert!(marker_scan(&[0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x01])
            .problem
            .is_some());
    }

    #[test]
    fn frame_markers_map_to_kinds() {
        for (m, k) in [
            (0xC0, FrameKind::Baseline),
            (0xC1, FrameKind::ExtendedSequential),
            (0xC2, FrameKind::Progressive),
            (0xC3, FrameKind::Lossless),
            (0xC5, FrameKind::Differential),
            (0xC9, FrameKind::Arithmetic),
            (0xCA, FrameKind::Arithmetic),
            (0xCF, FrameKind::Arithmetic),
            (0xC8, FrameKind::Other),
        ] {
            assert_eq!(FrameKind::of_marker(m), k, "{m:#x}");
        }
        // DHT, JPG and DAC are not frames; JPG (SOF8) is.
        let mut j = baseline(16, 16);
        let at = offset_of(&j, 0xC0);
        j[at + 1] = 0xC9;
        assert_eq!(
            marker_scan(&j).frame.expect("frame").kind,
            FrameKind::Arithmetic
        );
    }

    #[test]
    fn a_baseline_file_is_recompressed_verified_and_timed_twice() {
        let (f, one) = pair();
        let input = baseline(64, 48);
        let r = measure(&input, &f, &one, &nothing);
        assert!(r.failure.is_none(), "{:?}", r.failure);
        assert!(r.lepton_bytes.is_some_and(|l| l > 0 && l < r.bytes));
        assert!(r.default_threads.is_some() && r.one_thread.is_some());
        assert_eq!(r.one_thread_output_identical, Some(true));
    }

    #[test]
    fn bytes_after_the_eoi_survive_the_round_trip() {
        let (f, one) = pair();
        let mut input = baseline(32, 32);
        input.extend_from_slice(&baseline(16, 16));
        let r = measure(&input, &f, &one, &nothing);
        assert!(r.failure.is_none(), "{:?}", r.failure);
        assert!(r.scan.trailing_bytes > 0);
    }

    #[test]
    fn progressive_without_the_feature_is_a_progressive_failure() {
        let mut f = write_features();
        f.progressive = false;
        let mut one = f.clone();
        one.max_processor_threads = 1;
        let r = measure(&progressive(32, 32), &f, &one, &nothing);
        let fail = r.failure.expect("fails");
        assert_eq!(fail.cause, Cause::Progressive);
        assert_eq!(fail.exit_code.as_deref(), Some("ProgressiveUnsupported"));
        assert!(r.lepton_bytes.is_none() && r.default_threads.is_none());
    }

    #[test]
    fn the_default_features_accept_progressive_files() {
        let (f, one) = pair();
        let r = measure(&progressive(32, 32), &f, &one, &nothing);
        assert!(r.failure.is_none(), "{:?}", r.failure);
    }

    #[test]
    fn dimensions_over_the_cap_are_a_dimension_cap_failure() {
        let mut f = write_features();
        f.max_jpeg_width = 8;
        f.max_jpeg_height = 8;
        let r = measure(&baseline(32, 32), &f, &f, &nothing);
        let fail = r.failure.expect("fails");
        assert_eq!(fail.cause, Cause::DimensionCap);
        assert_eq!(fail.exit_code.as_deref(), Some("UnsupportedJpeg"));
    }

    #[test]
    fn an_arithmetic_frame_is_an_arithmetic_failure() {
        let (f, one) = pair();
        let mut j = baseline(32, 32);
        let at = offset_of(&j, 0xC0);
        j[at + 1] = 0xC9;
        let r = measure(&j, &f, &one, &nothing);
        assert_eq!(r.failure.expect("fails").cause, Cause::Arithmetic);
    }

    /// Four component specifications in the frame header of a baseline file.
    fn four_components(mut j: Vec<u8>) -> Vec<u8> {
        let at = offset_of(&j, 0xC0);
        // SOF0: marker, length(2), P, Y(2), X(2), Nf, 3 x (id, hv, tq)
        assert_eq!(j[at + 9], 3);
        j[at + 9] = 4;
        let len = u16::from_be_bytes([j[at + 2], j[at + 3]]) + 3;
        j[at + 2..at + 4].copy_from_slice(&len.to_be_bytes());
        let end = at + 4 + 6 + 9;
        j.splice(end..end, [4u8, 0x11, 0]);
        j
    }

    #[test]
    fn four_components_are_a_cmyk_failure() {
        let (f, one) = pair();
        let j = four_components(baseline(32, 32));
        let s = marker_scan(&j);
        assert_eq!(s.frame.expect("frame").components, 4);
        let r = measure(&j, &f, &one, &nothing);
        let fail = r.failure.expect("fails");
        assert_eq!(fail.cause, Cause::FourComponents, "{fail:?}");
    }

    fn scan_of(frame: Option<(FrameKind, u32, u32, u32)>) -> Scan {
        Scan {
            frame: frame.map(|(kind, components, width, height)| Frame {
                kind,
                components,
                precision: 8,
                width,
                height,
            }),
            restart_interval: false,
            mpf: false,
            gain_map_marker: false,
            adobe: false,
            scans: 1,
            eoi_found: true,
            trailing_bytes: 0,
            problem: None,
        }
    }

    fn code(c: ExitCode, message: &str) -> Fail {
        Fail::Code {
            code: c,
            message: message.to_string(),
            decode: false,
        }
    }

    fn classify_default(fail: &Fail, scan: &Scan, f: &EnabledFeatures) -> Failure {
        classify(fail, LapKind::Default, scan, f)
    }

    #[test]
    fn each_branch_of_the_classification() {
        let f = write_features();
        let plain = scan_of(Some((FrameKind::Baseline, 3, 100, 100)));
        let cause = |fail: Fail, scan: &Scan| classify_default(&fail, scan, &f).cause;
        let uj = |m: &str| code(ExitCode::UnsupportedJpeg, m);
        // The exit code decides first.
        assert_eq!(
            cause(code(ExitCode::Unsupported4Colors, ""), &plain),
            Cause::FourComponents
        );
        assert_eq!(
            cause(code(ExitCode::ProgressiveUnsupported, ""), &plain),
            Cause::Progressive
        );
        for c in [
            ExitCode::VerificationContentMismatch,
            ExitCode::VerificationLengthMismatch,
        ] {
            assert_eq!(cause(code(c, ""), &plain), Cause::VerificationMismatch);
        }
        assert_eq!(cause(Fail::Mismatch, &plain), Cause::VerificationMismatch);
        assert_eq!(
            classify_default(&Fail::Mismatch, &plain, &f).exit_code,
            None
        );
        // UnsupportedJpeg: the message first.
        assert_eq!(
            cause(
                uj("sof9 marker found, image is coded arithm. sequential"),
                &plain
            ),
            Cause::Arithmetic
        );
        assert_eq!(
            cause(uj("image dimensions larger than 16386x16386"), &plain),
            Cause::DimensionCap
        );
        assert_eq!(
            cause(
                uj("file is too large to encode, increase max_jpeg_file_size"),
                &plain
            ),
            Cause::TrailingData
        );
        for m in [
            "progress can't have two DC first stages",
            "progress must start with DC stage",
            "progressive encoding range was invalid 3 to 70",
            "spectral selection parameter out of range",
            "successive approximation parameter out of range",
        ] {
            assert_eq!(cause(uj(m), &plain), Cause::ProgressiveRejected, "{m}");
        }
        // Then the scan.
        let arith = scan_of(Some((FrameKind::Arithmetic, 3, 100, 100)));
        assert_eq!(
            cause(uj("unknown marker found: FF C9"), &arith),
            Cause::Arithmetic
        );
        let cmyk = scan_of(Some((FrameKind::Baseline, 4, 100, 100)));
        assert_eq!(cause(uj("x"), &cmyk), Cause::FourComponents);
        let big = scan_of(Some((FrameKind::Baseline, 3, 20000, 100)));
        assert_eq!(cause(uj("x"), &big), Cause::DimensionCap);
        let tall = scan_of(Some((FrameKind::Baseline, 3, 100, 16387)));
        assert_eq!(cause(uj("x"), &tall), Cause::DimensionCap);
        // Not explained: lossless, differential, a progressive frame, 12-bit data.
        for kind in [
            FrameKind::Lossless,
            FrameKind::Differential,
            FrameKind::Progressive,
        ] {
            let s = scan_of(Some((kind, 3, 100, 100)));
            assert_eq!(
                cause(uj("sof3 marker found, image is coded lossless"), &s),
                Cause::Other
            );
        }
        assert_eq!(
            cause(uj("12 bit data precision is not supported"), &plain),
            Cause::Other
        );
        // Gain-map, multi-picture and trailing data explain no failure, whatever the code.
        let mut busy = plain.clone();
        busy.mpf = true;
        busy.gain_map_marker = true;
        busy.trailing_bytes = TRAILING_LIMIT + 1;
        for c in [
            ExitCode::UnsupportedJpeg,
            ExitCode::InvalidPadding,
            ExitCode::CoefficientOutOfRange,
            ExitCode::ShortRead,
            ExitCode::SamplingBeyondTwoUnsupported,
        ] {
            let o = classify_default(&code(c, "m"), &busy, &f);
            assert_eq!(o.cause, Cause::Other, "{c:?}");
            assert_eq!(o.exit_code, Some(format!("{c:?}")));
            assert_eq!(o.message.as_deref(), Some("m"));
        }
        // Only the other codes keep the code of a file with a progressive frame.
        let prog = scan_of(Some((FrameKind::Progressive, 3, 100, 100)));
        assert_eq!(
            cause(code(ExitCode::CoefficientOutOfRange, "m"), &prog),
            Cause::Other
        );
        // Panics and decode-side errors.
        let o = classify_default(&Fail::Panic { decode: false }, &plain, &f);
        assert_eq!(
            (o.cause, o.exit_code.as_deref(), o.message),
            (Cause::Other, Some("panic"), None)
        );
        let o = classify_default(&Fail::Panic { decode: true }, &plain, &f);
        assert_eq!(o.exit_code.as_deref(), Some("decode:panic"));
        let dec = |c| Fail::Code {
            code: c,
            message: "bad".to_string(),
            decode: true,
        };
        let o = classify_default(&dec(ExitCode::StreamInconsistent), &arith, &f);
        assert_eq!(
            (o.cause, o.exit_code.as_deref()),
            (Cause::Other, Some("decode:StreamInconsistent"))
        );
        let o = classify(
            &dec(ExitCode::VerificationContentMismatch),
            LapKind::OneThread,
            &arith,
            &f,
        );
        assert_eq!(o.cause, Cause::VerificationMismatch);
        assert_eq!(o.lap, LapKind::OneThread);
    }

    #[test]
    fn library_messages_lose_their_context_and_stay_plain() {
        assert_eq!(
            clean_message(
                "progress can't have two DC first stages\n at C:\\Users\\x\\a.rs:1:2\n at b"
            ),
            "progress can't have two DC first stages"
        );
        assert_eq!(clean_message(""), "");
        assert_eq!(clean_message("caf\u{e9} \u{1}x"), "caf x");
        assert_eq!(clean_message(&"a".repeat(500)).len(), 200);
    }

    #[test]
    fn exit_code_names_match_the_library() {
        for c in [
            ExitCode::AssertionFailure,
            ExitCode::ShortRead,
            ExitCode::Unsupported4Colors,
            ExitCode::CoefficientOutOfRange,
            ExitCode::StreamInconsistent,
            ExitCode::ProgressiveUnsupported,
            ExitCode::SamplingBeyondTwoUnsupported,
            ExitCode::VersionUnsupported,
            ExitCode::OsError,
            ExitCode::UnsupportedJpeg,
            ExitCode::UnsupportedJpegWithZeroIdct0,
            ExitCode::InvalidResetCode,
            ExitCode::InvalidPadding,
            ExitCode::BadLeptonFile,
            ExitCode::ChannelFailure,
            ExitCode::IntegerCastOverflow,
            ExitCode::VerificationLengthMismatch,
            ExitCode::VerificationContentMismatch,
            ExitCode::SyntaxError,
            ExitCode::FileNotFound,
            ExitCode::ExternalVerificationFailed,
            ExitCode::OutOfMemory,
        ] {
            assert!(known_exit_code(&code_name(c)), "{c:?}");
            assert!(known_exit_code(&format!("decode:{c:?}")));
        }
        assert!(known_exit_code("panic") && known_exit_code("decode:panic"));
        assert!(!known_exit_code("Bogus") && !known_exit_code("decode:"));
    }

    #[test]
    fn a_second_lap_failure_is_marked() {
        // A one-thread setting that cannot work while the default one does.
        let f = write_features();
        let mut one = f.clone();
        one.max_jpeg_width = 8;
        one.max_jpeg_height = 8;
        let r = measure(&baseline(32, 32), &f, &one, &nothing);
        let fail = r.failure.expect("fails");
        assert_eq!(fail.lap, LapKind::OneThread);
        assert_eq!(
            fail.message
                .as_deref()
                .map(|m| m.starts_with("image dimensions")),
            Some(true)
        );
        // And the default lap's failure says so.
        let mut tight = f.clone();
        tight.max_jpeg_width = 8;
        let r = measure(&baseline(32, 32), &tight, &tight, &nothing);
        assert_eq!(r.failure.expect("fails").lap, LapKind::Default);
    }

    #[test]
    fn the_scan_survives_hostile_input() {
        let ff = 0xFF;
        let cases: Vec<Vec<u8>> = vec![
            vec![ff, 0xD8, ff, 0xE0, 0, 0],
            vec![ff, 0xD8, ff, 0xE0, 0, 1],
            vec![ff, 0xD8, ff, 0xE0, 0xFF, 0xFF, 1, 2],
            vec![ff, 0xD8, ff, ff, ff, ff, ff, ff],
            vec![ff, 0xD8, ff, 0xE1],
            vec![ff, 0xD8, ff, 0xE1, 0],
            vec![ff, 0xD8, ff, 0xE1, 0, 2],
            vec![ff, 0xD8, ff, 0xDA, 0, 2, ff],
            vec![ff, 0xD8, ff, 0xDA, 0, 2, ff, 0],
            vec![ff, 0xD8, ff, 0xC0, 0, 3, 8],
            vec![ff, 0xD8, ff, 0xDD, 0, 2],
            vec![ff, 0xD8],
            vec![ff],
            vec![],
        ];
        for c in &cases {
            let s = marker_scan(c);
            assert!(!s.eoi_found || c.len() > 2, "{c:?}");
        }
        assert!(marker_scan(&cases[0]).problem.is_some());
        assert!(marker_scan(&cases[2]).problem.is_some());
        // Seeded random bytes, with and without a JPEG start.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..2000 {
            let len = (next() % 300) as usize;
            let mut v: Vec<u8> = (0..len)
                .map(|_| match next() % 4 {
                    0 => 0xFF,
                    1 => 0,
                    _ => (next() >> 8) as u8,
                })
                .collect();
            if round % 2 == 0 {
                v.splice(0..0, [0xFF, 0xD8]);
            }
            let s = marker_scan(&v);
            assert!(s.trailing_bytes <= v.len() as u64);
        }
        // Random damage to a real file.
        let base = baseline(24, 24);
        for _ in 0..300 {
            let mut v = base.clone();
            for _ in 0..3 {
                let at = (next() % v.len() as u64) as usize;
                v[at] = (next() >> 8) as u8;
            }
            let _ = marker_scan(&v);
        }
    }

    #[test]
    fn gain_map_markers_of_all_three_kinds_are_detected() {
        let base = baseline(16, 16);
        let with = |marker: u8, payload: &[u8]| {
            let mut j = vec![0xFF, 0xD8, 0xFF, marker];
            j.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            j.extend_from_slice(payload);
            j.extend_from_slice(&base[2..]);
            marker_scan(&j)
        };
        assert!(with(0xE1, b"xmp hdrgm:Version=1").gain_map_marker);
        assert!(with(0xE1, b"xmp xmlns:HDRGainMap=x").gain_map_marker);
        let iso = with(0xE2, b"urn:iso:std:iso:ts:21496:-1\0rest");
        assert!(iso.gain_map_marker && !iso.mpf);
        assert!(!with(0xE1, b"plain exif").gain_map_marker);
    }

    #[test]
    fn a_file_with_a_gain_map_marker_that_recompresses_counts_as_such_in_the_aggregate() {
        let (f, one) = pair();
        let base = baseline(32, 32);
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE2];
        let payload = b"MPF\0abcd";
        j.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        j.extend_from_slice(payload);
        j.extend_from_slice(&base[2..]);
        j.extend_from_slice(&baseline(16, 16));
        let r = measure(&j, &f, &one, &nothing);
        assert!(r.failure.is_none(), "{:?}", r.failure);
        let a = Agg::of(&[&r]);
        assert_eq!(
            (a.ok_files, a.ok_multi_files, a.ok_multi_bytes),
            (1, 1, r.bytes)
        );
        assert_eq!(a.ok_after_eoi, r.scan.trailing_bytes);
        assert!(a.ok_after_eoi > 0);
    }

    #[test]
    fn a_decoded_file_that_differs_is_a_verification_mismatch() {
        let (f, one) = pair();
        let flip = |v: &mut Vec<u8>| {
            if let Some(b) = v.get_mut(20) {
                *b ^= 1;
            }
        };
        let r = measure(&baseline(32, 32), &f, &one, &flip);
        let fail = r.failure.expect("fails");
        assert_eq!(
            (fail.cause, fail.exit_code),
            (Cause::VerificationMismatch, None)
        );
        let shorten = |v: &mut Vec<u8>| {
            v.pop();
        };
        let r = measure(&baseline(32, 32), &f, &one, &shorten);
        assert_eq!(r.failure.expect("fails").cause, Cause::VerificationMismatch);
    }

    #[test]
    fn damaged_input_is_a_failure_with_the_library_code() {
        let (f, one) = pair();
        let mut j = baseline(32, 32);
        j.truncate(j.len() / 2);
        let r = measure(&j, &f, &one, &nothing);
        let fail = r.failure.expect("fails");
        assert!(fail.exit_code.is_some(), "{fail:?}");
        let r = measure(&[0xFF, 0xD8, 1, 2, 3], &f, &one, &nothing);
        assert!(r.failure.is_some());
    }

    // ---- end to end on a tiny corpus

    fn mf(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile {
            blake3: blake3::hash(data).to_hex().to_string(),
            bytes: data.len() as u64,
            licence: "CC0-1.0".to_string(),
            path: path.to_string(),
            source: "test".to_string(),
        }
    }

    fn corpus(tmp: &Path, private: bool) -> PathBuf {
        let root = if private {
            tmp.join("data")
        } else {
            tmp.join("corpus")
        };
        let mut arith = baseline(32, 32);
        let at = offset_of(&arith, 0xC0);
        arith[at + 1] = 0xC9;
        let mut trailing = baseline(24, 24);
        trailing.extend_from_slice(&[9u8; 50]);
        let items: Vec<(&str, &str, Vec<u8>)> = vec![
            ("photo-jpeg", "camera-a/one.jpg", baseline(48, 32)),
            ("photo-jpeg", "camera-a/two.jpg", progressive(32, 32)),
            ("photo-jpeg", "camera-b/three.jpg", arith),
            ("photo-jpeg-edited", "edited.jpg", trailing),
            ("photo-jpeg-edited", "notes.txt", b"not a jpeg".to_vec()),
        ];
        let mut files = Vec::new();
        for (class, rel, data) in &items {
            let p = root.join(class).join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(&p, data).expect("write");
            files.push((class.to_string(), mf(&format!("{class}/{rel}"), data)));
        }
        let manifest =
            Manifest::with_profile_name(if private { "private" } else { "small" }, files);
        let dir = tmp.join("corpus");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("manifest.json"), manifest.render()).expect("manifest");
        if private {
            let info = serde_json::json!({"private": true, "root": root.to_string_lossy()});
            std::fs::write(dir.join("build-info.json"), info.to_string()).expect("info");
        }
        dir
    }

    fn run_probe(tmp: &Path, private: bool) -> (PathBuf, String, Envelope<Data>) {
        let cfg = Config {
            probes: vec!["jpeg".to_string()],
            corpus: corpus(tmp, private),
            into: None,
            results_root: tmp.join("results"),
            tmp_root: tmp.join("results").join("tmp"),
            threads: 2,
            allow_dirty: true,
            allow_debug_build: true,
            local_tools: PathBuf::from("no-such-local-tools.toml"),
            tool_timeout: Duration::from_secs(60),
        };
        let out = execute(&cfg).expect("execute");
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let problems = validate_dir(&out.results_dir).expect("validate").problems;
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(out.problems, 0);
        let json = std::fs::read_to_string(out.results_dir.join("probe-jpeg.json")).expect("json");
        let env: Envelope<Data> = serde_json::from_str(&json).expect("typed");
        (out.results_dir, json, env)
    }

    #[test]
    fn end_to_end_writes_json_and_table_and_the_directory_validates() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (dir, json, env) = run_probe(tmp.path(), false);
        let md = std::fs::read_to_string(dir.join("probe-jpeg.md")).expect("md");
        assert_eq!(
            md,
            crate::probe::render_file("jpeg", &json).expect("render")
        );
        assert!(env.libraries.contains_key("lepton_jpeg"));
        assert_eq!(env.data.classes.len(), 2);
        let a = &env.data.classes[0];
        assert_eq!(a.class, "photo-jpeg");
        assert_eq!(
            (a.manifest_files, a.files.len(), a.not_jpeg.len()),
            (3, 3, 0)
        );
        assert_eq!(a.group_labels, ["camera-a", "camera-b"]);
        assert!(a.files[0].failure.is_none() && a.files[1].failure.is_none());
        assert_eq!(
            a.files[2].failure.as_ref().map(|f| f.cause),
            Some(Cause::Arithmetic)
        );
        assert_eq!(a.files[2].group, 1);
        let b = &env.data.classes[1];
        assert_eq!(
            (b.manifest_files, b.files.len(), b.not_jpeg.clone()),
            (2, 1, vec![1])
        );
        assert!(b.files[0].failure.is_none() && b.files[0].scan.trailing_bytes == 50);
        assert!(md.contains("## Failures by cause, files") && md.contains("camera-b"));
        assert!(!json.contains(tmp.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn a_private_corpus_carries_no_names() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (_, json, env) = run_probe(tmp.path(), true);
        assert!(env.corpus.private);
        for word in [
            "camera",
            "one.jpg",
            "edited.jpg",
            "\"path\"",
            "\"name\"",
            "\"folder\"",
        ] {
            assert!(!json.contains(word), "{word}");
        }
        assert_eq!(env.data.classes[0].group_labels, ["group-0", "group-1"]);
    }

    #[test]
    fn broken_data_is_reported_by_the_rules() {
        let tmp = tempfile::tempdir().expect("tmp");
        let (_, _, env) = run_probe(tmp.path(), false);
        assert!(check(&env).is_empty(), "{:?}", check(&env));
        let mut bad = env.clone();
        bad.data.classes[0].files.remove(0);
        assert!(check(&bad).iter().any(|p| p.contains("manifest_files")));
        let mut bad = env.clone();
        bad.data.classes[0].files[0].failure = Some(Failure {
            cause: Cause::Arithmetic,
            exit_code: None,
            message: None,
            lap: LapKind::Default,
        });
        assert!(check(&bad).iter().any(|p| p.contains("exactly one")));
        let mut bad = env.clone();
        if let Some(f) = bad.data.classes[0].files[2].failure.as_mut() {
            f.cause = Cause::TrailingData;
        }
        assert!(check(&bad)
            .iter()
            .any(|p| p.contains("not supported by the exit code")));
        let mut bad = env.clone();
        if let Some(f) = bad.data.classes[0].files[2].failure.as_mut() {
            f.exit_code = Some("NotACode".to_string());
        }
        assert!(check(&bad).iter().any(|p| p.contains("not a name")));
        let mut bad = env.clone();
        if let Some(f) = bad.data.classes[0].files[2].failure.as_mut() {
            f.cause = Cause::VerificationMismatch;
        }
        assert!(check(&bad)
            .iter()
            .any(|p| p.contains("not supported by the exit code")));
        let mut bad = env.clone();
        if let Some(f) = bad.data.classes[0].files[2].failure.as_mut() {
            f.message = Some("two\nlines".to_string());
        }
        assert!(check(&bad).iter().any(|p| p.contains("failure/message")));
        let mut bad = env.clone();
        bad.data.one_thread_pool = "x".to_string();
        assert!(check(&bad).iter().any(|p| p.contains("one_thread_pool")));
        let mut bad = env.clone();
        bad.data.features.progressive = false;
        assert!(check(&bad).iter().any(|p| p.contains("/data/features")));
        let mut bad = env.clone();
        bad.data.classes[0].files[0].path = None;
        assert!(check(&bad).iter().any(|p| p.contains("/path: missing")));
        let mut bad = env.clone();
        bad.data.classes[0].files[0].default_threads = None;
        assert!(check(&bad).iter().any(|p| p.contains("default_threads")));
        let mut bad = env.clone();
        bad.data.classes[0].files[0].group = 9;
        assert!(check(&bad).iter().any(|p| p.contains("/group")));
        let mut bad = env;
        bad.library_threads = 1;
        assert!(check(&bad).iter().any(|p| p.contains("/library_threads")));
    }

    #[test]
    fn a_corpus_without_the_classes_notes_it() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = crate::probe::tests::tiny_class_corpus(tmp.path());
        crate::probe::tests::with_ctx(&dir, tmp.path(), |ctx| {
            let out = run(ctx).expect("run");
            assert!(out.data.classes.is_empty());
            assert_eq!(out.notes.len(), 3);
        });
    }
}
