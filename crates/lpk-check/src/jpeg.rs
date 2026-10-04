//! `jpeg-reconstruct` (revision 1.1, section 8): the stream-header check (check 5), the memory term
//! and the Lepton decoding (check 6). The assembly (check 7 and 8) needs the archive's chunks and is
//! in [`crate::archive`].

use std::io::Write;

use crate::error::{Error, Result};
use crate::record::bad_record;

/// The fixed term of the writer's allowance (section 8, "Resources"); informative for a reader.
pub const FIXED_TERM: u64 = 67_108_864;

/// What the frame header of the JPEG gives the memory term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInfo {
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

/// Check 5's stream-header read: the frame header from the Lepton stream's own header, or
/// `BadRecord` `lepton stream` when the layout is not recognised.
pub fn stream_frame(id: u64, stream: &[u8], primary_len: u64) -> Result<FrameInfo> {
    let bad = || bad_record(id, "lepton stream");
    let hdr = stream.get(..28).ok_or_else(bad)?;
    let compressed_len = u32::from_le_bytes([hdr[24], hdr[25], hdr[26], hdr[27]]) as usize;
    let z = stream.get(28..28 + compressed_len).ok_or_else(bad)?;
    // Inflate no further than the 7 header bytes and `primary_len` bytes of raw header.
    let limit = usize::try_from(primary_len.saturating_add(7)).unwrap_or(usize::MAX);
    let d = zlib_prefix(z, limit);
    if d.len() < 7 || &d[..3] != b"HDR" {
        return Err(bad());
    }
    let raw_len = u32::from_le_bytes([d[3], d[4], d[5], d[6]]) as u64;
    if raw_len > primary_len {
        return Err(bad());
    }
    let raw = d.get(7..7 + raw_len as usize).ok_or_else(bad)?;
    let mut jpeg = Vec::with_capacity(raw.len() + 2);
    jpeg.extend_from_slice(&[0xFF, 0xD8]);
    jpeg.extend_from_slice(raw);
    find_frame(&jpeg).ok_or_else(bad)
}

/// Inflates zlib data (RFC 1950) into at most `limit` bytes, with `flate2`. Only as far as the
/// header needs: reaching `limit` stops it, the Adler-32 is not required, and the bytes produced
/// before any damage are kept (too few of them is the caller's `lepton stream` refusal).
fn zlib_prefix(z: &[u8], limit: usize) -> Vec<u8> {
    use std::io::Read;
    let mut dec = flate2::read::ZlibDecoder::new(z);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while out.len() < limit {
        let want = buf.len().min(limit - out.len());
        match dec.read(&mut buf[..want]) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
    out
}

/// Check 5's comparison: the image's term against the archive's `decode_memory` (never above the
/// reader's memory) minus `max_block_plain`.
pub fn check_memory(
    term: u64,
    decode_memory: u64,
    memory: u64,
    max_block_plain: u64,
) -> Result<()> {
    let left = decode_memory.min(memory).saturating_sub(max_block_plain);
    if term > left {
        return Err(Error::new(
            "Refused",
            format!("the archive needs decode_memory of {term} bytes; this reader allows {left}"),
        ));
    }
    Ok(())
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
pub fn decode(id: u64, stream: &[u8], primary_len: u64) -> Result<Vec<u8>> {
    let limit = usize::try_from(primary_len).map_err(|_| bad_record(id, "primary_len"))?;
    let mut w = Bounded {
        buf: Vec::new(),
        limit,
        overflow: false,
    };
    let features = lepton_jpeg::EnabledFeatures::compat_lepton_vector_write();
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
        let r = |s: &[u8]| stream_frame(3, s, 100).map_err(|e| e.detail);
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
        assert!(check_memory(100, 1000, 2000, 900).is_ok());
        let e = check_memory(101, 1000, 2000, 900).unwrap_err();
        assert_eq!(e.class, "Refused");
        assert!(e.detail.contains("decode_memory"));
        assert!(check_memory(100, 1000, 950, 900).is_err());
    }

    #[test]
    fn garbage_is_refused_by_the_library() {
        assert_eq!(
            decode(0, &[0xCF, 0x84, 1, 2, 3], 10).unwrap_err().detail,
            "bad record 0: lepton stream"
        );
    }
}
