//! `jpeg-reconstruct` (revision 1.1, section 8): the stream-header check (check 5), the memory term
//! and the Lepton decoding (check 6). The assembly (check 7 and 8) needs the archive's chunks and is
//! in [`crate::archive`].

use std::io::Write;

use crate::error::Result;
use crate::record::bad_record;

/// The fixed term of an image's term (section 8, "Resources"): counted once per image in check
/// (5), which every revision 1.1 reader applies.
pub const FIXED_TERM: u64 = 67_108_864;

/// What the frame header of the JPEG gives the memory term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInfo {
    /// The SOF marker's second byte (`C2` is a progressive frame).
    pub marker: u8,
    /// Image height.
    pub height: u16,
    /// Image width.
    pub width: u16,
    /// Per component: (horizontal, vertical) sampling factors.
    pub sampling: Vec<(u8, u8)>,
}

impl FrameInfo {
    /// The coefficient term: each component's 8x8 blocks, padded to whole MCUs, times 128 bytes.
    pub fn memory_term(&self) -> u64 {
        let hmax = self
            .sampling
            .iter()
            .map(|s| u64::from(s.0))
            .max()
            .unwrap_or(1);
        let vmax = self
            .sampling
            .iter()
            .map(|s| u64::from(s.1))
            .max()
            .unwrap_or(1);
        let mcux = u64::from(self.width).div_ceil(8 * hmax.max(1));
        let mcuy = u64::from(self.height).div_ceil(8 * vmax.max(1));
        let blocks: u64 = self
            .sampling
            .iter()
            .map(|&(h, v)| mcux * u64::from(h) * mcuy * u64::from(v))
            .sum();
        blocks * 128
    }
}

fn is_sof(m: u8) -> bool {
    matches!(m, 0xC0..=0xC3 | 0xC5..=0xCB | 0xCD..=0xCF)
}

/// Finds the first frame header in JPEG marker segments that begin with SOI.
pub fn find_frame(jpeg: &[u8]) -> Option<FrameInfo> {
    if jpeg.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let mut pos = 2usize;
    loop {
        if *jpeg.get(pos)? != 0xFF {
            return None;
        }
        let m = *jpeg.get(pos + 1)?;
        match m {
            0xFF => {
                pos += 1;
                continue;
            }
            // Markers without a length field.
            0x01 | 0xD0..=0xD8 => {
                pos += 2;
                continue;
            }
            0xD9 => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([
            *jpeg.get(pos + 2)?,
            *jpeg.get(pos + 3)?,
        ]));
        if len < 2 {
            return None;
        }
        let seg = jpeg.get(pos + 4..pos + 2 + len)?;
        if is_sof(m) {
            // Offsets inside the segment after its length field: P, Y (1..3), X (3..5), Nf, then
            // 3 bytes per component, the sampling factors in the second.
            let height = u16::from_be_bytes([*seg.get(1)?, *seg.get(2)?]);
            let width = u16::from_be_bytes([*seg.get(3)?, *seg.get(4)?]);
            let nf = usize::from(*seg.get(5)?);
            let mut sampling = Vec::with_capacity(nf);
            for i in 0..nf {
                let hv = *seg.get(6 + 3 * i + 1)?;
                sampling.push((hv >> 4, hv & 0x0F));
            }
            return Some(FrameInfo {
                marker: m,
                height,
                width,
                sampling,
            });
        }
        pos += 2 + len;
        if m == 0xDA {
            // Entropy-coded data up to the next marker other than FF 00 and RST0-RST7.
            loop {
                if *jpeg.get(pos)? == 0xFF {
                    let n = *jpeg.get(pos + 1)?;
                    if n != 0x00 && !(0xD0..=0xD7).contains(&n) {
                        break;
                    }
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
        }
    }
}

/// What check (5) reads from the Lepton stream's own header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamHeader {
    /// The first frame header of the raw JPEG header.
    pub frame: FrameInfo,
    /// Inflated length of the compressed header.
    pub header_len: u64,
    /// Partitions the `HH` section declares.
    pub partitions: u64,
}

/// The decode threads `lepton_jpeg`'s preset runs at most (`max_processor_threads`).
pub const THREADS: u64 = 8;
/// Per decode thread: the library's model and row state.
pub const THREAD_TERM: u64 = 4_194_304;

/// Check 5's stream-header read: the whole compressed header is inflated (at most two primaries
/// and 64 KiB; its length counts every inflated byte, a marker cut short at the end included)
/// and walked; any length in it that the primary image does not explain, an `HH` section
/// missing, repeated, empty or with rows going down, segment sizes past their bound, an unknown
/// section, data ending inside a section, damage, or no frame header is `BadRecord` `lepton
/// stream`, before the library allocates anything. The segment sizes: without an `EEE` section
/// they sum to at most `primary_len`; with one, the library cuts the last to the JPEG length
/// less the garbage (2 bytes without `GRB`), the raw header, SOI and the other segments, which
/// must therefore fit in the JPEG length. Also `lepton stream`: the garbage longer than the
/// JPEG length; a partition with more than 7 overhang bits; an `EEE` `max_dpos` of `u32::MAX`;
/// a first restart error of 2 or more; restart counts given but fewer than the raw header's
/// `FF DA` pairs (at least one).
pub fn stream_header(id: u64, stream: &[u8], primary_len: u64) -> Result<StreamHeader> {
    let bad = || bad_record(id, "lepton stream");
    let le = |b: &[u8]| u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let hdr = stream.get(..28).ok_or_else(bad)?;
    let compressed_len = le(&hdr[24..28]);
    if le(&hdr[20..24]) > primary_len || compressed_len > primary_len {
        return Err(bad());
    }
    let z = stream
        .get(28..28 + compressed_len as usize)
        .ok_or_else(bad)?;
    let cap = primary_len.saturating_mul(2).saturating_add(65_536);
    let d = inflate_all(z, cap).ok_or_else(bad)?;
    let mut p = 0usize;
    let mut take = |n: u64| -> Option<&[u8]> {
        let n = usize::try_from(n).ok()?;
        let s = d.get(p..p.checked_add(n)?)?;
        p += n;
        Some(s)
    };
    if take(3).ok_or_else(bad)? != b"HDR" {
        return Err(bad());
    }
    let raw_len = le(take(4).ok_or_else(bad)?);
    if raw_len > primary_len {
        return Err(bad());
    }
    let mut jpeg = vec![0xFF, 0xD8];
    jpeg.extend_from_slice(take(raw_len).ok_or_else(bad)?);
    let (mut restarts, mut errors) = (0u64, 0u64);
    let mut segments: Option<Vec<u64>> = None;
    let (mut garbage, mut early_eof) = (2u64, false);
    let mut rst_err0: Option<u8> = None;
    // The library ends the header at the first section marker it cannot read whole.
    while let Some(m) = take(3) {
        let m = [m[0], m[1], m[2]];
        match &m {
            b"P0D" => {
                take(1).ok_or_else(bad)?;
            }
            b"CRS" => {
                let n = le(take(4).ok_or_else(bad)?);
                restarts += n;
                if restarts * 4 > primary_len {
                    return Err(bad());
                }
                take(n * 4).ok_or_else(bad)?;
            }
            [b'H', b'H', n] => {
                if segments.is_some() || *n == 0 {
                    return Err(bad());
                }
                let (h, _) = take(16 * u64::from(*n)).ok_or_else(bad)?.as_chunks::<16>();
                let rows: Vec<u16> = h.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                // Rows going down, or overhang bits past a byte's seven.
                if rows.windows(2).any(|w| w[1] < w[0]) || h.iter().any(|c| c[7] > 7) {
                    return Err(bad());
                }
                segments = Some(h.iter().map(|c| le(&c[2..6])).collect());
            }
            b"FRS" | b"GRB" => {
                let n = le(take(4).ok_or_else(bad)?);
                if &m == b"FRS" {
                    errors += n;
                }
                if n > primary_len || errors > primary_len {
                    return Err(bad());
                }
                if &m == b"GRB" {
                    garbage = n;
                }
                let body = take(n).ok_or_else(bad)?;
                if &m == b"FRS" && rst_err0.is_none() {
                    rst_err0 = body.first().copied();
                }
            }
            b"EEE" => {
                let e = take(28).ok_or_else(bad)?;
                // Four `max_dpos` values after three other u32s; the library adds one to each.
                if (12..28)
                    .step_by(4)
                    .any(|i| le(&e[i..i + 4]) == u64::from(u32::MAX))
                {
                    return Err(bad());
                }
                early_eof = true;
            }
            _ => return Err(bad()),
        }
    }
    let segments = segments.ok_or_else(bad)?;
    let all: u64 = segments.iter().sum();
    let last = segments.last().copied().unwrap_or(0);
    let jpeg_len = le(&hdr[20..24]);
    let fits = if early_eof {
        all - last + garbage + raw_len + 2 <= jpeg_len
    } else {
        all <= primary_len
    };
    if !fits {
        return Err(bad());
    }
    // The library takes the garbage (2 bytes without `GRB`) off the JPEG length, adds the first
    // restart error to an 8-bit marker count, and looks the restart counts up by scan.
    if garbage > jpeg_len || rst_err0.is_some_and(|e| e >= 2) {
        return Err(bad());
    }
    let sos = jpeg
        .windows(2)
        .filter(|w| w[0] == 0xFF && w[1] == 0xDA)
        .count() as u64;
    if restarts != 0 && restarts < sos.max(1) {
        return Err(bad());
    }
    Ok(StreamHeader {
        frame: find_frame(&jpeg).ok_or_else(bad)?,
        header_len: d.len() as u64,
        partitions: segments.len() as u64,
    })
}

impl StreamHeader {
    /// The image's term (section 8, "Resources"): the larger of the coefficient term plus the
    /// fixed term, and the bound on the library's allocations: the coefficients (twice for a
    /// progressive frame), a progressive scan's output (1536 bytes per block: 512 of content, a
    /// buffer grown by doubling and its old copy while it moves), per thread (at most
    /// [`THREADS`], at most the partitions) [`THREAD_TERM`], 128 KiB and 512 bytes per block of an
    /// MCU row; four times the inflated header; `primary_len`; the stream's length; 1 MiB.
    pub fn term(&self, primary_len: u64, stream_len: u64) -> u64 {
        let f = &self.frame;
        let hmax = f
            .sampling
            .iter()
            .map(|s| u64::from(s.0))
            .max()
            .unwrap_or(1)
            .max(1);
        let vmax = f
            .sampling
            .iter()
            .map(|s| u64::from(s.1))
            .max()
            .unwrap_or(1)
            .max(1);
        let mcux = u64::from(f.width).div_ceil(8 * hmax);
        let mcuy = u64::from(f.height).div_ceil(8 * vmax);
        let row: u64 = f
            .sampling
            .iter()
            .map(|&(h, v)| mcux * u64::from(h) * u64::from(v))
            .sum();
        let blocks = row * mcuy;
        let per_block = if f.marker == 0xC2 { 256 + 1536 } else { 128 };
        let library = blocks * per_block
            + self.partitions.min(THREADS) * (THREAD_TERM + 131_072 + 512 * row)
            + 4 * self.header_len
            + primary_len
            + stream_len
            + 1_048_576;
        (f.memory_term() + FIXED_TERM).max(library)
    }
}

/// Inflates all of the zlib data (RFC 1950) with `flate2`; `None` on damage or past `cap`.
fn inflate_all(z: &[u8], cap: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(z)
        .take(cap.saturating_add(1))
        .read_to_end(&mut out)
        .ok()?;
    (out.len() as u64 <= cap).then_some(out)
}

struct Bounded {
    buf: Vec<u8>,
    limit: usize,
    overflow: bool,
}

impl Write for Bounded {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() + b.len() > self.limit {
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

/// Check 6: decodes the Lepton stream with `lepton_jpeg` into at most `primary_len` bytes and
/// requires exactly `primary_len`.
pub fn decode(id: u64, stream: &[u8], primary_len: u64, frame: &FrameInfo) -> Result<Vec<u8>> {
    let limit = usize::try_from(primary_len).map_err(|_| bad_record(id, "primary_len"))?;
    let mut w = Bounded {
        buf: Vec::new(),
        limit,
        overflow: false,
    };
    // The preset with the size limits of section 8: the primary's length and the frame's size.
    let mut features = lepton_jpeg::EnabledFeatures::compat_lepton_vector_write();
    features.max_jpeg_file_size = u32::try_from(primary_len).unwrap_or(u32::MAX);
    features.max_jpeg_width = u32::from(frame.width);
    features.max_jpeg_height = u32::from(frame.height);
    let pool = lepton_jpeg::SingleThreadPool {};
    let r = lepton_jpeg::catch_unwind_result(|| {
        let mut rd = std::io::Cursor::new(stream);
        lepton_jpeg::decode_lepton(&mut rd, &mut w, &features, &pool)
    });
    if w.overflow || r.is_err() {
        return Err(bad_record(id, "lepton stream"));
    }
    if w.buf.len() != limit {
        return Err(bad_record(id, "primary_len"));
    }
    Ok(w.buf)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_found_after_segments_and_scan() {
        // APP0 (len 4), SOS (len 3) with entropy data containing FF 00 and RST0, then SOF2.
        let j = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 1, 2, 0xFF, 0xDA, 0x00, 0x03, 0, 0x12, 0xFF, 0x00,
            0x34, 0xFF, 0xD0, 0xFF, 0xC2, 0x00, 0x0E, 8, 0x00, 0x11, 0x00, 0x21, 2, 1, 0x22, 0, 2,
            0x11, 1,
        ];
        let f = find_frame(&j).unwrap_or_else(|| panic!("frame"));
        assert_eq!(
            f,
            FrameInfo {
                marker: 0xC2,
                height: 17,
                width: 33,
                sampling: vec![(2, 2), (1, 1)]
            }
        );
        // MCU 16x16: 3 x 2 MCUs; component 0: 6 x 4 = 24 blocks, component 1: 6 blocks.
        assert_eq!(f.memory_term(), 30 * 128);
        assert!(find_frame(&j[..20]).is_none());
        assert!(find_frame(&[0xFF, 0xD8, 0xFF, 0xD9]).is_none());
    }

    #[test]
    fn unrecognised_layouts_are_lepton_stream() {
        let r = |s: &[u8]| stream_header(3, s, 100).map_err(|e| e.detail);
        let want = Err("bad record 3: lepton stream".to_string());
        assert_eq!(r(&[0; 27]), want);
        let mut s = vec![0u8; 28];
        s[24] = 5;
        assert_eq!(r(&s), want);
        s.extend_from_slice(&[0x78, 0x01, 0x01, 0x00]);
        s.push(0);
        assert_eq!(r(&s), want);
    }

    #[test]
    fn memory_comparison() {
        // Check 5's comparison: the image's term against the archive's `decode_memory` (never
        // above the reader's memory) minus `max_block_plain`.
        let check_memory = |term, decode_memory, memory, max_block_plain| {
            crate::deflate::Memory {
                decode_memory,
                memory,
                max_block_plain,
            }
            .check(term)
        };
        assert!(check_memory(100, 1000, 2000, 900).is_ok());
        let e = check_memory(101, 1000, 2000, 900).unwrap_err();
        assert_eq!(e.class, "Refused");
        assert!(e.detail.contains("decode_memory"));
        assert!(check_memory(100, 1000, 950, 900).is_err());
    }

    #[test]
    fn garbage_is_refused_by_the_library() {
        assert_eq!(
            decode(
                0,
                &[0xCF, 0x84, 1, 2, 3],
                10,
                &find_frame(&[0xFF, 0xD8, 0xFF, 0xC0, 0, 8, 8, 0, 8, 0, 8, 0]).unwrap()
            )
            .unwrap_err()
            .detail,
            "bad record 0: lepton stream"
        );
    }
}
