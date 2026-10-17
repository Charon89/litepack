//! Analysing the streams the walker found, moved unchanged from Phase 0's `probe deflate`.
//!
//! Per stream: analysis with `PreflateStreamProcessor` and its `is_done` (the library's own
//! verification is off; the stream is verified here), then `recreate_whole_deflate_stream` from
//! the plain data and the corrections, compared byte for byte with the original stream. The
//! library returns `Ok` with a shortened stream when the input is truncated or the plain-text
//! limit is hit after a first block, so the stream must be complete (`is_done`) and its plain
//! size must equal the known one. A stream the library rejects is failed under the name of its
//! `ExitCode`; one that is incomplete is failed as `ShortRead` (`InvalidDeflate` when the data is
//! corrupt), or skipped when it is a complete stream whose plain size is over the limit;
//! `PlainTextLimit` and a declared or measured plain size over the limit are skipped; a
//! recreation that returns other bytes (or a plain size other than the known one) is failed as
//! `RoundtripMismatch`; a panic in the library is [`PANIC`].

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use preflate_rs::{
    recreate_whole_deflate_stream, ExitCode, PlainText, PreflateConfig, PreflateStreamProcessor,
    TokenPredictorParameters,
};

use super::walk::{find_streams, inflate_extent, trim_spans, Found, Inflated};

/// Plain text the library may hold for one stream.
pub const PLAIN_TEXT_LIMIT: u64 = 64 * 1024 * 1024;
/// Plain text kept in memory for one file; later streams of the file are skipped for size.
pub const FILE_PLAIN_BUDGET: u64 = 512 * 1024 * 1024;
/// The library's match-chain bound used for analysis (the library's default).
pub const MAX_CHAIN_LENGTH: u32 = 4096;

/// The name recorded for a panic inside the library.
pub const PANIC: &str = "Panic";

/// Every name a failed stream can carry: the library's `ExitCode` names and [`PANIC`].
pub const FAILURE_NAMES: [&str; 29] = [
    "ReadDeflate",
    "InvalidPredictionData",
    "AnalyzeFailed",
    "RecompressFailed",
    "RoundtripMismatch",
    "ReadBlock",
    "PredictBlock",
    "PredictTree",
    "RecreateBlock",
    "RecreateTree",
    "EncodeBlock",
    "InvalidCompressedWrapper",
    "ZstdError",
    "InvalidParameterHeader",
    "ShortRead",
    "OsError",
    "GeneralFailure",
    "InvalidIDat",
    "MatchNotFound",
    "InvalidDeflate",
    "NoCompressionCandidates",
    "InvalidParameter",
    "AssertionFailure",
    "NonZeroPadding",
    "PredictionFailure",
    "PlainTextLimit",
    "WebPDecodeError",
    "OutOfMemory",
    PANIC,
];

/// The name of a library exit code (its `Debug` text).
pub fn code_name(c: ExitCode) -> String {
    format!("{c:?}")
}

/// The analysis configuration for a plain-text limit: [`MAX_CHAIN_LENGTH`], the limit, and the
/// library's own verification off (each stream is verified by recreation instead).
pub fn preflate_config(plain_text_limit: u64) -> PreflateConfig {
    PreflateConfig {
        max_chain_length: MAX_CHAIN_LENGTH,
        plain_text_limit: usize::try_from(plain_text_limit).unwrap_or(usize::MAX),
        verify_compression: false,
    }
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed().as_secs_f64())
}

/// The outcome of [`analyse_stream`].
#[derive(Debug)]
pub enum StreamOutcome {
    /// Reconstructed: recreation from `plain` and `corrections` gave the stream's first
    /// `deflate_len` bytes back exactly.
    Done {
        /// The inflated text.
        plain: PlainText,
        /// The library's correction record.
        corrections: Vec<u8>,
        /// Bytes of the stream the library consumed (the rest of the candidate is not part of it).
        deflate_len: usize,
        /// The library's estimate of the encoder's parameters.
        parameters: Option<TokenPredictorParameters>,
        /// Seconds spent in analysis.
        analyse_seconds: f64,
        /// Seconds spent in recreation.
        recreate_seconds: f64,
    },
    /// Not analysed or not analysable for size.
    Skipped {
        /// The candidate's length.
        deflate_len: usize,
        /// Seconds spent.
        seconds: f64,
    },
    /// Failed, with the library's error name, `RoundtripMismatch` or [`PANIC`].
    Failed {
        /// One of [`FAILURE_NAMES`].
        cause: String,
        /// The candidate's length.
        deflate_len: usize,
        /// Seconds spent.
        seconds: f64,
    },
}

/// Analyse one raw Deflate stream and verify it by recreation (see the module text).
pub fn analyse_stream(
    deflate: &[u8],
    plain_known: Option<u64>,
    cfg: &PreflateConfig,
) -> StreamOutcome {
    let failed = |cause: &str, seconds: f64| StreamOutcome::Failed {
        cause: cause.to_string(),
        deflate_len: deflate.len(),
        seconds,
    };
    let skipped = |seconds: f64| StreamOutcome::Skipped {
        deflate_len: deflate.len(),
        seconds,
    };
    // The library returns Ok with a shortened stream when the input ends early or the plain-text
    // limit is hit after a first block: `is_done` tells a whole stream from a part.
    let (r, analyse_seconds) = timed(|| {
        catch_unwind(AssertUnwindSafe(|| {
            let mut sp = PreflateStreamProcessor::new(cfg);
            let chunk = sp.decompress(deflate)?;
            let done = sp.is_done();
            Ok::<_, preflate_rs::PreflateError>((chunk, done, sp.detach_plain_text()))
        }))
    });
    let (chunk, done, plain) = match r {
        Err(_) => return failed(PANIC, analyse_seconds),
        Ok(Err(e)) if e.exit_code() == ExitCode::PlainTextLimit => return skipped(analyse_seconds),
        Ok(Err(e)) => return failed(&code_name(e.exit_code()), analyse_seconds),
        Ok(Ok(v)) => v,
    };
    if !done {
        // Over the limit when the stream is complete and its plain size exceeds the limit.
        let mut buf = vec![0u8; 64 * 1024];
        return match inflate_extent(deflate, false, &mut buf) {
            Inflated::Clean { out, .. } if out > cfg.plain_text_limit as u64 => {
                skipped(analyse_seconds)
            }
            // Corrupt data is InvalidDeflate; only input that ran out is ShortRead.
            Inflated::Bad => failed(&code_name(ExitCode::InvalidDeflate), analyse_seconds),
            _ => failed(&code_name(ExitCode::ShortRead), analyse_seconds),
        };
    }
    let deflate_len = chunk.compressed_size;
    let original = deflate.get(..deflate_len);
    let (back, recreate_seconds) = timed(|| {
        catch_unwind(AssertUnwindSafe(|| {
            recreate_whole_deflate_stream(plain.text(), &chunk.corrections)
        }))
    });
    let seconds = analyse_seconds + recreate_seconds;
    if plain_known.is_some_and(|k| k != plain.text().len() as u64) {
        return failed(&code_name(ExitCode::RoundtripMismatch), seconds);
    }
    match (back, original) {
        (Err(_), _) => failed(PANIC, seconds),
        (Ok(Err(e)), _) => failed(&code_name(e.exit_code()), seconds),
        (Ok(Ok(b)), Some(o)) if b == o => StreamOutcome::Done {
            plain,
            corrections: chunk.corrections,
            deflate_len,
            parameters: chunk.parameters,
            analyse_seconds,
            recreate_seconds,
        },
        (Ok(Ok(_)), _) => failed(&code_name(ExitCode::RoundtripMismatch), seconds),
    }
}

/// Size limits of [`examine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Plain text the library may hold for one stream.
    pub plain_text_limit: u64,
    /// Plain text of reconstructed streams one file may produce before later streams are skipped.
    pub file_plain_budget: u64,
}

impl Limits {
    /// [`PLAIN_TEXT_LIMIT`] and [`FILE_PLAIN_BUDGET`].
    pub fn standard() -> Self {
        Limits {
            plain_text_limit: PLAIN_TEXT_LIMIT,
            file_plain_budget: FILE_PLAIN_BUDGET,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::standard()
    }
}

/// Counts of one file's streams. Every candidate is exactly one of reconstructed, skipped or
/// failed: `found == reconstructed + skipped + sum(failed)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counts {
    /// Candidates analysed (after the overlap refusals).
    pub found: u64,
    /// Candidates reconstructed and verified.
    pub reconstructed: u64,
    /// Candidates skipped for size.
    pub skipped: u64,
    /// Failed candidates by name (one of [`FAILURE_NAMES`]).
    pub failed: BTreeMap<String, u64>,
    /// ZIP entries refused before analysis (encrypted, outside the file, unreadable) and
    /// overlapping candidates.
    pub structural_refusals: u64,
    /// Zlib-scan hits that did not inflate cleanly to the minimum size.
    pub scan_false_hits: u64,
    /// The walker panicked (the file is then kind `other` with no streams).
    pub walker_panics: u64,
    /// Bytes of every candidate (the consumed length for reconstructed ones).
    pub found_deflate_bytes: u64,
    /// Bytes of the reconstructed streams.
    pub reconstructed_deflate_bytes: u64,
    /// Bytes of the skipped candidates.
    pub skipped_deflate_bytes: u64,
    /// Bytes of the failed candidates.
    pub failed_deflate_bytes: u64,
    /// Plain bytes of the reconstructed streams.
    pub reconstructed_plain_bytes: u64,
    /// Correction bytes of the reconstructed streams.
    pub correction_bytes: u64,
    /// Analysis seconds of the reconstructed streams.
    pub analyse_seconds: f64,
    /// Recreation seconds of the reconstructed streams.
    pub recreate_seconds: f64,
    /// Seconds spent on the streams that did not reconstruct.
    pub unreconstructed_seconds: f64,
}

/// One reconstructed stream, handed to the caller of [`examine`] in file order.
#[derive(Debug)]
pub struct Reconstructed {
    /// The stream's bytes in the file: the candidate's spans trimmed to the `deflate_len` bytes
    /// the library consumed. Sorted, disjoint, not empty unless the stream is.
    pub spans: Vec<(usize, usize)>,
    /// The inflated text.
    pub plain: PlainText,
    /// The library's correction record.
    pub corrections: Vec<u8>,
    /// The stream's length (the sum of the span lengths).
    pub deflate_len: usize,
    /// The library's estimate of the encoder's parameters.
    pub parameters: Option<TokenPredictorParameters>,
    /// Seconds spent in analysis.
    pub analyse_seconds: f64,
    /// Seconds spent in recreation.
    pub recreate_seconds: f64,
}

/// What [`examine`] reports for one file.
#[derive(Debug, Clone, PartialEq)]
pub struct Examined {
    /// One of [`KINDS`](super::KINDS).
    pub kind: &'static str,
    /// The stream counts.
    pub counts: Counts,
}

fn contiguous<'a>(bytes: &'a [u8], spans: &[(usize, usize)]) -> Cow<'a, [u8]> {
    match spans {
        [(a, b)] => Cow::Borrowed(&bytes[*a..*b]),
        _ => Cow::Owned(
            spans
                .iter()
                .flat_map(|(a, b)| bytes[*a..*b].iter().copied())
                .collect(),
        ),
    }
}

/// Find and analyse every Deflate stream of one file (`ext`, lower case without the dot, only
/// labels ZIP kinds). `on_done` is called once per reconstructed stream, in file order; a
/// stream's plain text is dropped when `on_done` returns unless the caller keeps it. A panic in
/// the walker is caught and counted; panics in the library are caught per stream.
pub fn examine(
    bytes: &[u8],
    ext: &str,
    limits: &Limits,
    mut on_done: impl FnMut(Reconstructed),
) -> Examined {
    let (found, panicked) = match catch_unwind(AssertUnwindSafe(|| find_streams(bytes, ext))) {
        Ok(f) => (f, false),
        Err(_) => (
            Found {
                kind: "other",
                candidates: Vec::new(),
                rejected: 0,
                false_hits: 0,
            },
            true,
        ),
    };
    let cfg = preflate_config(limits.plain_text_limit);
    let mut st = Counts {
        structural_refusals: found.rejected,
        scan_false_hits: found.false_hits,
        walker_panics: u64::from(panicked),
        ..Counts::default()
    };
    let mut plain_held = 0u64;
    for c in &found.candidates {
        st.found += 1;
        let data = contiguous(bytes, &c.spans);
        let too_big = c.plain_known.is_some_and(|p| p > limits.plain_text_limit)
            || plain_held >= limits.file_plain_budget;
        let outcome = if too_big {
            StreamOutcome::Skipped {
                deflate_len: data.len(),
                seconds: 0.0,
            }
        } else {
            analyse_stream(&data, c.plain_known, &cfg)
        };
        match outcome {
            StreamOutcome::Done {
                plain,
                corrections,
                deflate_len,
                parameters,
                analyse_seconds,
                recreate_seconds,
            } => {
                let (corr, defl, plain_len) = (
                    corrections.len() as u64,
                    deflate_len as u64,
                    plain.text().len() as u64,
                );
                st.reconstructed += 1;
                st.found_deflate_bytes += defl;
                st.reconstructed_deflate_bytes += defl;
                st.reconstructed_plain_bytes += plain_len;
                st.correction_bytes += corr;
                st.analyse_seconds += analyse_seconds;
                st.recreate_seconds += recreate_seconds;
                plain_held += plain_len;
                on_done(Reconstructed {
                    spans: trim_spans(&c.spans, 0, deflate_len),
                    plain,
                    corrections,
                    deflate_len,
                    parameters,
                    analyse_seconds,
                    recreate_seconds,
                });
            }
            StreamOutcome::Skipped {
                deflate_len,
                seconds,
            } => {
                st.skipped += 1;
                st.skipped_deflate_bytes += deflate_len as u64;
                st.found_deflate_bytes += deflate_len as u64;
                st.unreconstructed_seconds += seconds;
            }
            StreamOutcome::Failed {
                cause,
                deflate_len,
                seconds,
            } => {
                *st.failed.entry(cause).or_default() += 1;
                st.failed_deflate_bytes += deflate_len as u64;
                st.found_deflate_bytes += deflate_len as u64;
                st.unreconstructed_seconds += seconds;
            }
        }
    }
    Examined {
        kind: found.kind,
        counts: st,
    }
}
