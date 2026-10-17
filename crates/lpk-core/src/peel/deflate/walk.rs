//! Finding the Deflate streams of a file, by content and one level deep (nothing inside inflated
//! data is scanned). Moved unchanged from Phase 0's `probe deflate`, which validated it:
//!
//! * ZIP family: every entry stored with method 8; the raw compressed bytes. Labelled `apk`,
//!   `office`, `jar` or `zip` by file extension first, then by entry names (`AndroidManifest.xml`,
//!   an ODF `mimetype` or `word/`, `xl/`, `ppt/` directories, `META-INF/MANIFEST.MF`);
//! * PNG: the concatenated `IDAT` payload, a zlib stream (one candidate over several spans);
//! * gzip: every member (RFC 1952 header, raw deflate, 8-byte trailer);
//! * PDF: after each `stream` keyword and its end-of-line, any valid zlib header (method 8,
//!   window field at most 7, header divisible by 31, no preset dictionary). A stream that
//!   inflates cleanly is cut exactly; one that does not is a candidate with the bytes up to the
//!   next `endstream` (or the end of the file), so the analysis names what is wrong with it;
//! * any other file, and a ZIP that cannot be parsed: a scan for the four common zlib headers
//!   (`78 01`, `78 5E`, `78 9C`, `78 DA`) at any offset, accepted only when the stream inflates
//!   cleanly (checksum included) to at least [`MIN_SCANNED_PLAIN`] bytes (everything else counts
//!   as a false hit). The scan resumes after an accepted stream.
//!
//! A candidate's spans are the raw Deflate bytes only (no zlib header or checksum, no gzip
//! header or trailer); [`find_streams`] returns candidates sorted and pairwise disjoint.

use std::io::Cursor;

use flate2::{Decompress, FlushDecompress, Status};

/// Container kinds, in table order.
pub const KINDS: [&str; 8] = ["office", "jar", "apk", "zip", "png", "gzip", "pdf", "other"];

/// Smallest plain size of a stream found by the zlib scan.
pub const MIN_SCANNED_PLAIN: u64 = 1024;
/// Pre-inflation stops at this many output bytes (it discards the output; this only bounds time).
pub const INFLATE_CAP: u64 = 1 << 32;

/// A stream found in a file: where its Deflate bytes are (several spans for PNG).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Half-open byte ranges of the file, in file order; their concatenation is the stream.
    pub spans: Vec<(usize, usize)>,
    /// The plain size, when known before analysis (measured by inflation, or declared by a ZIP
    /// entry).
    pub plain_known: Option<u64>,
    /// Plain bytes the walker inflated while finding it (0 when it was not inflated, as for a
    /// ZIP entry); for a stream of unknown size, what its analysis is charged
    /// ([`examine`](super::examine)).
    pub inflated: u64,
}

/// What the walker found in one file.
#[derive(Debug)]
pub struct Found {
    /// One of [`KINDS`].
    pub kind: &'static str,
    /// Sorted by first offset, pairwise disjoint.
    pub candidates: Vec<Candidate>,
    /// Structural refusals.
    pub rejected: u64,
    /// Zlib-scan false hits.
    pub false_hits: u64,
    /// ZIP entries stored with Deflate64 (method 9): not candidates, they stay framing.
    pub zip_deflate64: u64,
    /// ZIP entries stored with a method other than stored, Deflate and Deflate64.
    pub zip_other_method: u64,
}

/// The outcome of [`inflate_extent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inflated {
    Clean {
        consumed: usize,
        out: u64,
    },
    /// The data is not a valid deflate stream.
    Bad,
    /// Valid so far, but the input ran out (or the output cap was reached) before the end.
    Unfinished,
}

thread_local! {
    /// Inflated bytes this thread may still produce in [`inflate_extent`] (the per-file work
    /// budget of [`examine`](super::examine); unlimited outside it).
    static WORK_LEFT: std::cell::Cell<u64> = const { std::cell::Cell::new(u64::MAX) };
}

/// Set this thread's work left (see [`WORK_LEFT`]).
pub(crate) fn set_work_left(n: u64) {
    WORK_LEFT.with(|w| w.set(n));
}

/// Sets this thread's work left for its lifetime and restores it to unlimited when dropped,
/// on the panic path too.
pub(crate) struct WorkGuard;

impl WorkGuard {
    /// Set the work left to `n` until the guard is dropped.
    pub(crate) fn new(n: u64) -> Self {
        set_work_left(n);
        WorkGuard
    }
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        set_work_left(u64::MAX);
    }
}

/// This thread's work left.
pub(crate) fn work_left() -> u64 {
    WORK_LEFT.with(|w| w.get())
}

/// Inflate `data` (zlib or raw deflate), discard the output, and say how many input bytes the
/// stream took and how many bytes it produced. Inside [`examine`](super::examine) the output is
/// charged to the file's work budget; a stream that would pass it is `Unfinished`.
pub fn inflate_extent(data: &[u8], zlib: bool, buf: &mut [u8]) -> Inflated {
    let mut d = Decompress::new(zlib);
    loop {
        if work_left() == 0 {
            return Inflated::Unfinished;
        }
        let in_before = d.total_in();
        let out_before = d.total_out();
        let Some(rest) = data.get(in_before as usize..) else {
            return Inflated::Unfinished;
        };
        let r = d.decompress(rest, buf, FlushDecompress::None);
        set_work_left(work_left().saturating_sub(d.total_out() - out_before));
        match r {
            Ok(Status::StreamEnd) => {
                return Inflated::Clean {
                    consumed: d.total_in() as usize,
                    out: d.total_out(),
                }
            }
            Ok(_) => {
                if d.total_in() == in_before && d.total_out() == out_before {
                    // No progress and no error: the input ended inside the stream.
                    return Inflated::Unfinished;
                }
                if d.total_out() > INFLATE_CAP {
                    return Inflated::Unfinished;
                }
            }
            Err(_) => return Inflated::Bad,
        }
    }
}

fn zlib_header(b: &[u8]) -> bool {
    matches!(b, [0x78, 0x01 | 0x5e | 0x9c | 0xda, ..])
}

/// Any valid zlib header: method 8, window field at most 7, check divisible by 31, no preset
/// dictionary.
pub fn valid_zlib_header(b: &[u8]) -> bool {
    match b {
        [c, f, ..] => {
            c & 0x0f == 8
                && c >> 4 <= 7
                && (u32::from(*c) * 256 + u32::from(*f)) % 31 == 0
                && f & 0x20 == 0
        }
        _ => false,
    }
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The zlib stream at `at` (header, deflate, 4-byte checksum) as the span of its deflate bytes.
fn zlib_candidate(bytes: &[u8], at: usize, buf: &mut [u8]) -> Option<(Candidate, usize)> {
    let rest = bytes.get(at..)?;
    match inflate_extent(rest, true, buf) {
        Inflated::Clean { consumed, out } if consumed >= 6 => Some((
            Candidate {
                spans: vec![(at + 2, at + consumed - 4)],
                plain_known: Some(out),
                inflated: out,
            },
            consumed,
        )),
        _ => None,
    }
}

fn scan_zlib(bytes: &[u8], buf: &mut [u8]) -> (Vec<Candidate>, u64) {
    let mut out = Vec::new();
    let mut rejected = 0;
    let mut i = 0;
    while i + 2 <= bytes.len() {
        if bytes[i] == 0x78 && zlib_header(&bytes[i..]) {
            match zlib_candidate(bytes, i, buf) {
                Some((c, consumed)) if c.plain_known.unwrap_or(0) >= MIN_SCANNED_PLAIN => {
                    out.push(c);
                    i += consumed;
                    continue;
                }
                _ => rejected += 1,
            }
        }
        i += 1;
    }
    (out, rejected)
}

fn find_zip(bytes: &[u8], ext: &str) -> Option<Found> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let mut candidates = Vec::new();
    let mut rejected = 0;
    let (mut apk, mut office, mut jar) = (false, false, false);
    let (mut zip_deflate64, mut zip_other_method) = (0u64, 0u64);
    let mut office_dirs = false;
    for i in 0..zip.len() {
        let Ok(f) = zip.by_index_raw(i) else {
            rejected += 1;
            continue;
        };
        match f.name().as_deref() {
            Ok("AndroidManifest.xml") => apk = true,
            Ok("mimetype") => office = true,
            Ok("META-INF/MANIFEST.MF") => jar = true,
            Ok(n) if n.starts_with("word/") || n.starts_with("xl/") || n.starts_with("ppt/") => {
                office_dirs = true
            }
            Ok(_) => {}
            Err(_) => {
                rejected += 1;
                continue;
            }
        }
        if f.is_dir() {
            continue;
        }
        match f.compression() {
            zip::CompressionMethod::Deflated => {}
            zip::CompressionMethod::Stored => continue,
            // Method 9 is `Deflate64` or `Unsupported(9)`, depending on the crate's features.
            m if matches!(format!("{m:?}").as_str(), "Deflate64" | "Unsupported(9)") => {
                zip_deflate64 += 1;
                continue;
            }
            _ => {
                zip_other_method += 1;
                continue;
            }
        }
        let (Some(start), size) = (f.data_start(), f.compressed_size()) else {
            rejected += 1;
            continue;
        };
        let (start, end) = (
            start as usize,
            (start as usize).saturating_add(size as usize),
        );
        if f.encrypted() || end > bytes.len() {
            rejected += 1;
            continue;
        }
        candidates.push(Candidate {
            spans: vec![(start, end)],
            plain_known: Some(f.size()),
            inflated: 0,
        });
    }
    // Packages that share `[Content_Types].xml` with Office are told apart by extension first.
    let kind = match ext {
        "apk" => "apk",
        "docx" | "docm" | "dotx" | "xlsx" | "xlsm" | "pptx" | "ppsx" | "potx" | "odt" | "ods"
        | "odp" | "odg" | "epub" | "vsdx" => "office",
        "jar" | "war" | "ear" => "jar",
        "msix" | "appx" | "nupkg" | "vsix" | "xps" => "zip",
        _ if apk => "apk",
        _ if office || office_dirs => "office",
        _ if jar => "jar",
        _ => "zip",
    };
    Some(Found {
        kind,
        candidates,
        rejected,
        false_hits: 0,
        zip_deflate64,
        zip_other_method,
    })
}

fn find_png(bytes: &[u8], buf: &mut [u8]) -> Found {
    let mut spans = Vec::new();
    let mut pos = 8;
    while let Some(head) = bytes.get(pos..pos + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let start = pos + 8;
        let Some(end) = start.checked_add(len).filter(|e| *e <= bytes.len()) else {
            break;
        };
        match &head[4..8] {
            b"IDAT" => spans.push((start, end)),
            b"IEND" => break,
            _ => {}
        }
        pos = end + 4;
    }
    let mut found = Found {
        kind: "png",
        candidates: Vec::new(),
        rejected: 0,
        false_hits: 0,
        zip_deflate64: 0,
        zip_other_method: 0,
    };
    let total: usize = spans.iter().map(|(a, b)| b - a).sum();
    if total < 2 {
        return found;
    }
    let mut concat = Vec::with_capacity(total);
    for (a, b) in &spans {
        concat.extend_from_slice(&bytes[*a..*b]);
    }
    let before = work_left();
    let inflated_png = inflate_extent(&concat, true, buf);
    let inflated = before.saturating_sub(work_left());
    let (take, plain_known) = match inflated_png {
        Inflated::Clean { consumed, out } if consumed >= 6 => (consumed - 6, Some(out)),
        _ => (total - 2, None),
    };
    found.candidates.push(Candidate {
        spans: trim_spans(&spans, 2, take),
        plain_known,
        inflated,
    });
    found
}

/// The parts of `spans` (taken as one concatenated byte string) from `skip` on, `take` bytes.
pub fn trim_spans(spans: &[(usize, usize)], skip: usize, take: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut skip, mut take) = (skip, take);
    for &(a, b) in spans {
        let len = b - a;
        if skip >= len {
            skip -= len;
            continue;
        }
        let from = a + skip;
        skip = 0;
        let n = (b - from).min(take);
        if n > 0 {
            out.push((from, from + n));
        }
        take -= n;
        if take == 0 {
            break;
        }
    }
    out
}

pub fn gzip_header_len(b: &[u8]) -> Option<usize> {
    if b.len() < 10 || b[0] != 0x1f || b[1] != 0x8b || b[2] != 8 {
        return None;
    }
    let flags = b[3];
    let mut pos = 10;
    if flags & 4 != 0 {
        let xlen = u16::from_le_bytes([*b.get(pos)?, *b.get(pos + 1)?]) as usize;
        pos += 2 + xlen;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            pos += b.get(pos..)?.iter().position(|c| *c == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        pos += 2;
    }
    (pos <= b.len()).then_some(pos)
}

fn find_gzip(bytes: &[u8], buf: &mut [u8]) -> Found {
    let mut found = Found {
        kind: "gzip",
        candidates: Vec::new(),
        rejected: 0,
        false_hits: 0,
        zip_deflate64: 0,
        zip_other_method: 0,
    };
    let mut pos = 0;
    while let Some(h) = gzip_header_len(&bytes[pos..]) {
        let start = pos + h;
        let before = work_left();
        let r = inflate_extent(&bytes[start..], false, buf);
        let inflated = before.saturating_sub(work_left());
        match r {
            Inflated::Clean { consumed, out } => {
                found.candidates.push(Candidate {
                    spans: vec![(start, start + consumed)],
                    plain_known: Some(out),
                    inflated,
                });
                pos = start + consumed + 8;
                if pos >= bytes.len() {
                    break;
                }
            }
            Inflated::Bad | Inflated::Unfinished => {
                // Let the library name what is wrong with the rest of the file.
                found.candidates.push(Candidate {
                    spans: vec![(start, bytes.len())],
                    plain_known: None,
                    inflated,
                });
                break;
            }
        }
    }
    found
}

/// Does the dictionary of the object whose `stream` keyword is at `kw` (the text after the last
/// `obj` before it, at most 8 KiB back) name `/FlateDecode`?
fn dictionary_has_flate(bytes: &[u8], kw: usize) -> bool {
    let window = &bytes[kw.saturating_sub(8192)..kw];
    let start = window
        .windows(3)
        .rposition(|w| w == b"obj")
        .map_or(0, |p| p + 3);
    window[start..]
        .windows(b"/FlateDecode".len())
        .any(|w| w == b"/FlateDecode")
}

fn find_pdf(bytes: &[u8], buf: &mut [u8]) -> Found {
    let mut found = Found {
        kind: "pdf",
        candidates: Vec::new(),
        rejected: 0,
        false_hits: 0,
        zip_deflate64: 0,
        zip_other_method: 0,
    };
    let mut from = 0;
    while let Some(k) = find_from(bytes, b"stream", from) {
        from = k + 6;
        if k >= 3 && &bytes[k - 3..k] == b"end" {
            continue;
        }
        let data = match bytes.get(from..from + 2) {
            Some(b"\r\n") => from + 2,
            Some([b'\n', _]) => from + 1,
            _ => continue,
        };
        let Some(head) = bytes.get(data..).filter(|h| valid_zlib_header(h)) else {
            continue;
        };
        // A header other than the common `78 xx` needs the object's dictionary to say Flate.
        if !zlib_header(head) && !dictionary_has_flate(bytes, k) {
            continue;
        }
        let before = work_left();
        let r = zlib_candidate(bytes, data, buf);
        let inflated = before.saturating_sub(work_left());
        match r {
            Some((c, consumed)) => {
                found.candidates.push(c);
                from = data + consumed;
            }
            None => {
                // Not a clean stream: the library names what is wrong with it.
                let end = find_from(bytes, b"endstream", data).unwrap_or(bytes.len());
                found.candidates.push(Candidate {
                    spans: vec![(data + 2, end.max(data + 2))],
                    plain_known: None,
                    inflated,
                });
                from = end.max(from);
            }
        }
    }
    found
}

/// Find the streams of a file by its content (`ext` only labels ZIP kinds).
pub fn find_streams(bytes: &[u8], ext: &str) -> Found {
    let mut buf = vec![0u8; 64 * 1024];
    let mut found = if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        find_zip(bytes, ext)
    } else {
        None
    }
    .unwrap_or_else(|| {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            find_png(bytes, &mut buf)
        } else if bytes.starts_with(b"\x1f\x8b\x08") && gzip_header_len(bytes).is_some() {
            find_gzip(bytes, &mut buf)
        } else if bytes.starts_with(b"%PDF-") {
            find_pdf(bytes, &mut buf)
        } else {
            let (candidates, false_hits) = scan_zlib(bytes, &mut buf);
            Found {
                kind: "other",
                candidates,
                rejected: 0,
                false_hits,
                zip_deflate64: 0,
                zip_other_method: 0,
            }
        }
    });
    // No two candidates may overlap: the replacement must be well defined.
    found
        .candidates
        .sort_by_key(|c| c.spans.first().map_or(0, |s| s.0));
    let mut end = 0;
    let mut kept = Vec::new();
    for c in std::mem::take(&mut found.candidates) {
        let first = c.spans.first().map_or(0, |s| s.0);
        let last = c.spans.last().map_or(0, |s| s.1);
        if first < end {
            found.rejected += 1;
        } else {
            end = last;
            kept.push(c);
        }
    }
    found.candidates = kept;
    found
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{every_kind, multi_idat_png, noise, text, zlib};
    use super::*;

    /// Spans of every candidate, flattened, are sorted, disjoint and inside the file. That is
    /// the whole partition property: spans plus the gaps between them then cover the file
    /// exactly once, so a byte-for-byte rebuild would add nothing.
    fn assert_partition(bytes: &[u8], found: &Found, what: &str) {
        let spans: Vec<(usize, usize)> = found
            .candidates
            .iter()
            .flat_map(|c| c.spans.iter().copied())
            .collect();
        let mut pos = 0;
        for &(a, b) in &spans {
            assert!(pos <= a && a <= b && b <= bytes.len(), "{what}: {spans:?}");
            pos = b;
        }
        assert!(KINDS.contains(&found.kind), "{what}");
    }

    #[test]
    fn spans_are_trimmed_across_chunks() {
        let spans = [(10, 20), (30, 40), (50, 60)];
        assert_eq!(trim_spans(&spans, 2, 6), [(12, 18)]);
        assert_eq!(trim_spans(&spans, 8, 6), [(18, 20), (30, 34)]);
        assert_eq!(trim_spans(&spans, 2, 100), [(12, 20), (30, 40), (50, 60)]);
        assert_eq!(trim_spans(&spans, 0, 0), []);
    }

    #[test]
    fn inflate_extent_tells_corrupt_data_from_input_that_ran_out() {
        let plain = text(50_000, 9);
        let z = zlib(&plain);
        let mut buf = vec![0u8; 4096];
        assert!(matches!(
            inflate_extent(&z, true, &mut buf),
            Inflated::Clean { .. }
        ));
        assert!(matches!(
            inflate_extent(&z[..z.len() / 2], true, &mut buf),
            Inflated::Unfinished
        ));
        // A reserved block type (11) is corrupt, not short.
        assert!(matches!(
            inflate_extent(&[0x07, 0, 0, 0, 0], false, &mut buf),
            Inflated::Bad
        ));
    }

    #[test]
    fn zlib_headers_are_checked_like_the_probe() {
        // The 0x58 0x85 header is valid (method 8, window 5, divisible by 31).
        assert!(valid_zlib_header(&[0x58, 0x85]));
        assert!(!valid_zlib_header(&[0x78, 0x9d]));
        assert!(!valid_zlib_header(&[0x78, 0xbb]), "preset dictionary");
        assert!(!valid_zlib_header(&[0x78]));
        assert!(zlib_header(&[0x78, 0xda]) && !zlib_header(&[0x78, 0x85]));
        assert_eq!(MIN_SCANNED_PLAIN, 1024);
        assert_eq!(INFLATE_CAP, 1 << 32);
    }

    #[test]
    fn every_kind_is_found_as_the_probe_found_it() {
        let kinds: Vec<&str> = every_kind()
            .iter()
            .map(|(ext, b)| find_streams(b, ext).kind)
            .collect();
        assert_eq!(kinds, ["zip", "office", "png", "gzip", "pdf", "other"]);
        let png = find_streams(&multi_idat_png(), "png");
        assert_eq!(png.candidates.len(), 1);
        assert_eq!(
            png.candidates[0].spans.len(),
            3,
            "one stream over three IDATs"
        );
        let gz = &every_kind()[3].1;
        assert_eq!(find_streams(gz, "gz").candidates.len(), 2);
    }

    #[test]
    fn spans_partition_every_synthetic_and_random_input() {
        for (ext, bytes) in every_kind() {
            assert_partition(&bytes, &find_streams(&bytes, ext), ext);
        }
        let magics: [&[u8]; 6] = [
            b"PK\x03\x04",
            b"PK\x05\x06",
            b"\x89PNG\r\n\x1a\n",
            b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03",
            b"%PDF-1.7\nstream\n\x78\x9c",
            b"",
        ];
        for seed in 0..40u32 {
            for m in magics {
                let mut b = m.to_vec();
                b.extend_from_slice(&noise(1 + (seed as usize * 97) % 3000, seed));
                if seed % 3 == 0 {
                    // Random data with a real stream inside.
                    b.extend_from_slice(&zlib(&text(2000 + seed as usize, seed)));
                    b.extend_from_slice(&noise(50, seed + 1));
                }
                assert_partition(&b, &find_streams(&b, "bin"), "random");
            }
        }
    }

    #[test]
    fn truncated_and_damaged_containers_do_not_panic() {
        for (ext, bytes) in every_kind() {
            // Every truncation point.
            let n = bytes.len();
            for cut in 0..=n {
                let t = &bytes[..cut];
                assert_partition(t, &find_streams(t, ext), ext);
            }
            // Flipped bytes along the file.
            for at in (0..n).step_by(29) {
                let mut d = bytes.clone();
                d[at] ^= 0xa5;
                assert_partition(&d, &find_streams(&d, ext), ext);
            }
        }
    }

    #[test]
    fn a_png_with_a_huge_declared_chunk_length_is_not_followed() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&u32::MAX.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&[1, 2, 3]);
        let f = find_streams(&png, "png");
        assert_eq!((f.kind, f.candidates.len()), ("png", 0));
    }
}
