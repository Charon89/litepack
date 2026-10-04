//! Byte-level reading: varints (section 1), the header (section 2), frames (section 3) and the
//! trailer payload (section 6).

use crate::error::{Error, Result};

/// The header length.
pub const HEADER_LEN: usize = 32;
/// The whole trailer frame length.
pub const TRAILER_FRAME_LEN: usize = 133;
/// The smallest frame: kind, flags, a one-byte length and the hash.
pub const MIN_FRAME_LEN: u64 = 37;
/// The magic bytes.
pub const MAGIC: [u8; 8] = [0x89, 0x4C, 0x50, 0x4B, 0x0D, 0x0A, 0x1A, 0x0A];

/// Header flag: the archive is encrypted.
pub const HF_ENCRYPTED: u32 = 1;
/// Header flag: the entry table is in clear.
pub const HF_LISTABLE: u32 = 2;

/// Frame flag: a reader that does not know the kind must fail.
pub const FF_MUST_UNDERSTAND: u16 = 1;
/// Frame flag: the payload is sealed.
pub const FF_SEALED: u16 = 2;

/// Frame kinds.
pub mod kind {
    /// Entry table.
    pub const ENTRY_TABLE: u16 = 1;
    /// Chunk data (one block).
    pub const CHUNK_DATA: u16 = 2;
    /// Reconstruction records.
    pub const RECORDS: u16 = 3;
    /// Recovery.
    pub const RECOVERY: u16 = 4;
    /// Index.
    pub const INDEX: u16 = 5;
    /// Trailer.
    pub const TRAILER: u16 = 6;
    /// Key slot.
    pub const KEY_SLOT: u16 = 7;
}

/// A cursor over a byte slice that reports truncation with a `what` name.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Cursor<'a> {
    /// A cursor at the start of `buf`.
    pub fn new(buf: &'a [u8], what: &'static str) -> Self {
        Self { buf, pos: 0, what }
    }

    /// Changes the name used in truncation errors.
    pub fn set_what(&mut self, what: &'static str) {
        self.what = what;
    }

    /// Bytes not yet read.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// The position.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Reads `n` bytes.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(Error::truncated(self.what));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Reads a fixed array.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.bytes(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    /// Reads a `u8`.
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    /// Reads a little-endian `u16`.
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u32`.
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u64`.
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `i64`.
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// Reads a canonical varint.
    pub fn varint(&mut self) -> Result<u64> {
        read_varint(self.buf, &mut self.pos, self.what)
    }

    /// Reads a count and checks it against the bytes left divided by `min_size`.
    pub fn count(&mut self, min_size: usize) -> Result<u64> {
        let n = self.varint()?;
        let bound = (self.remaining() / min_size.max(1)) as u64;
        if n > bound {
            return Err(Error::truncated(self.what));
        }
        Ok(n)
    }
}

/// Reads an unsigned LEB128 varint in canonical form (section 1).
pub fn read_varint(buf: &[u8], pos: &mut usize, what: &str) -> Result<u64> {
    let mut v: u64 = 0;
    for i in 0..10u32 {
        let Some(&b) = buf.get(*pos) else {
            return Err(Error::truncated(what));
        };
        *pos += 1;
        if i == 9 {
            if b != 0x01 {
                return Err(Error::new(
                    "NonCanonicalVarint",
                    format!("varint in {what} has a bad tenth byte"),
                ));
            }
            return Ok(v | (1u64 << 63));
        }
        v |= u64::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            if i > 0 && b == 0 {
                return Err(Error::new(
                    "NonCanonicalVarint",
                    format!("non-canonical varint in {what}"),
                ));
            }
            return Ok(v);
        }
    }
    Err(Error::new(
        "NonCanonicalVarint",
        format!("varint in {what} is longer than ten bytes"),
    ))
}

/// The encoded length of a varint.
pub fn varint_len(mut v: u64) -> u64 {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

/// The 32-byte header (section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The minor version.
    pub version_minor: u16,
    /// Header flags.
    pub flags: u32,
    /// The archive id.
    pub archive_id: [u8; 16],
}

impl Header {
    /// Whether the archive is encrypted.
    pub fn encrypted(&self) -> bool {
        self.flags & HF_ENCRYPTED != 0
    }

    /// Whether the entry table is in clear.
    pub fn listable(&self) -> bool {
        self.flags & HF_LISTABLE != 0
    }
}

/// Parses the header in the order of section 2: length, magic, major version, flags.
pub fn parse_header(data: &[u8]) -> Result<Header> {
    if data.len() < HEADER_LEN {
        return Err(Error::truncated("header"));
    }
    if data[..8] != MAGIC {
        return Err(Error::new("BadMagic", "not an .lpk archive (bad magic)"));
    }
    let major = u16::from_le_bytes([data[8], data[9]]);
    if major != 1 {
        return Err(Error::new(
            "UnsupportedVersion",
            format!("unsupported major version {major}"),
        ));
    }
    let version_minor = u16::from_le_bytes([data[10], data[11]]);
    let flags = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    if flags & !(HF_ENCRYPTED | HF_LISTABLE) != 0 {
        return Err(Error::new(
            "ReservedHeaderBits",
            format!("reserved header flags set: {flags:#x}"),
        ));
    }
    let mut archive_id = [0u8; 16];
    archive_id.copy_from_slice(&data[16..32]);
    Ok(Header {
        version_minor,
        flags,
        archive_id,
    })
}

/// One frame as found in the input (section 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Absolute offset of the frame.
    pub offset: usize,
    /// Frame kind.
    pub kind: u16,
    /// Frame flags.
    pub flags: u16,
    /// The payload as stored.
    pub payload: &'a [u8],
    /// Absolute offset just after the frame.
    pub end: usize,
    /// Whether the stored hash equals the payload's BLAKE3.
    pub hash_ok: bool,
}

impl Frame<'_> {
    /// The whole encoded length.
    pub fn len(&self) -> usize {
        self.end - self.offset
    }

    /// Never empty (a frame has at least 37 bytes); present for clippy.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Whether the SEALED flag is set.
    pub fn sealed(&self) -> bool {
        self.flags & FF_SEALED != 0
    }

    /// Fails with `HashMismatch` when the hash did not verify.
    pub fn check_hash(&self) -> Result<()> {
        if self.hash_ok {
            Ok(())
        } else {
            Err(Error::hash_mismatch(self.kind))
        }
    }
}

/// Parses the frame at `offset` of `data` without failing on a bad hash (the caller decides).
/// The input ends at `data.len()`; `limit` is the largest accepted `payload_len`.
pub fn parse_frame(data: &[u8], offset: usize, limit: u64) -> Result<Frame<'_>> {
    let mut pos = offset;
    if data.len() < offset || data.len() - offset < 4 {
        return Err(Error::truncated("frame header"));
    }
    let kind = u16::from_le_bytes([data[pos], data[pos + 1]]);
    let flags = u16::from_le_bytes([data[pos + 2], data[pos + 3]]);
    pos += 4;
    let payload_len = read_varint(data, &mut pos, "frame header")?;
    if kind == 0 {
        return Err(Error::new("BadFrameKind", "frame kind 0 is invalid"));
    }
    if flags & !(FF_MUST_UNDERSTAND | FF_SEALED) != 0 {
        return Err(Error::new(
            "ReservedFrameBits",
            format!("reserved frame flags {flags:#x} in frame kind {kind}"),
        ));
    }
    if payload_len > limit {
        return Err(Error::new(
            "PayloadTooLarge",
            format!("payload of {payload_len} bytes exceeds the limit {limit}"),
        ));
    }
    let left = (data.len() - pos) as u64;
    if payload_len > left {
        return Err(Error::truncated("payload"));
    }
    let plen = payload_len as usize;
    let payload = &data[pos..pos + plen];
    pos += plen;
    if data.len() - pos < 32 {
        return Err(Error::truncated("hash"));
    }
    let stored = &data[pos..pos + 32];
    pos += 32;
    let hash_ok = blake3::hash(payload).as_bytes() == stored;
    Ok(Frame {
        offset,
        kind,
        flags,
        payload,
        end: pos,
        hash_ok,
    })
}

/// The trailer payload (section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trailer {
    /// Offset of the index frame.
    pub index_offset: u64,
    /// Whole length of the index frame.
    pub index_len: u64,
    /// BLAKE3 of the index payload as stored.
    pub index_hash: [u8; 32],
    /// Generation number.
    pub generation: u64,
    /// Archive id.
    pub archive_id: [u8; 16],
    /// Offset of the previous trailer frame (0 in generation 0).
    pub previous_trailer_offset: u64,
    /// The generation's nonce salt.
    pub salt: [u8; 16],
}

/// Whether a frame has the trailer's fixed shape (kind 6, no flags, payload 96).
pub fn is_trailer_shape(f: &Frame<'_>) -> bool {
    f.kind == kind::TRAILER && f.flags == 0 && f.payload.len() == 96
}

/// Parses a 96-byte trailer payload.
pub fn parse_trailer(payload: &[u8]) -> Result<Trailer> {
    let mut c = Cursor::new(payload, "trailer");
    let t = Trailer {
        index_offset: c.u64()?,
        index_len: c.u64()?,
        index_hash: c.array()?,
        generation: c.u64()?,
        archive_id: c.array()?,
        previous_trailer_offset: c.u64()?,
        salt: c.array()?,
    };
    if c.remaining() != 0 {
        return Err(Error::trailing("trailer"));
    }
    Ok(t)
}

/// Reads the trailer frame that ends at `end` when it has the fixed shape and a valid hash.
pub fn trailer_ending_at(data: &[u8], end: usize) -> Option<(usize, Trailer)> {
    let start = end.checked_sub(TRAILER_FRAME_LEN)?;
    if start < HEADER_LEN {
        return None;
    }
    let f = parse_frame(&data[..end], start, 96).ok()?;
    if f.end != end || !is_trailer_shape(&f) || !f.hash_ok {
        return None;
    }
    parse_trailer(f.payload).ok().map(|t| (start, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints() {
        let mut p = 0;
        assert_eq!(read_varint(&[0x00], &mut p, "t"), Ok(0));
        p = 0;
        assert_eq!(read_varint(&[0xAC, 0x02], &mut p, "t"), Ok(300));
        p = 0;
        assert_eq!(
            read_varint(&[0x80, 0x00], &mut p, "t").map_err(|e| e.class),
            Err("NonCanonicalVarint")
        );
        p = 0;
        let max = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        assert_eq!(read_varint(&max, &mut p, "t"), Ok(u64::MAX));
        p = 0;
        let bad = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02];
        assert!(read_varint(&bad, &mut p, "t").is_err());
        p = 0;
        assert_eq!(
            read_varint(&[0x80], &mut p, "x").map_err(|e| e.class),
            Err("Truncated")
        );
        assert_eq!(varint_len(127), 1);
        assert_eq!(varint_len(128), 2);
        assert_eq!(varint_len(u64::MAX), 10);
    }

    #[test]
    fn header_checks() {
        assert_eq!(
            parse_header(&[0; 10]).map_err(|e| e.class),
            Err("Truncated")
        );
        assert_eq!(parse_header(&[0; 32]).map_err(|e| e.class), Err("BadMagic"));
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&MAGIC);
        h[8] = 2;
        assert_eq!(
            parse_header(&h).map_err(|e| e.class),
            Err("UnsupportedVersion")
        );
        h[8] = 1;
        h[12] = 4;
        assert!(parse_header(&h).is_err());
        h[12] = 3;
        let hd = parse_header(&h);
        assert!(hd.is_ok_and(|x| x.encrypted() && x.listable()));
    }

    #[test]
    fn frame_roundtrip() {
        let payload = b"hello";
        let mut f = vec![2, 0, 0, 0, 5];
        f.extend_from_slice(payload);
        f.extend_from_slice(blake3::hash(payload).as_bytes());
        let fr = parse_frame(&f, 0, 1 << 30);
        assert!(fr.as_ref().is_ok_and(|x| x.hash_ok && x.end == f.len()));
        let mut g = f.clone();
        g[6] ^= 1;
        assert!(parse_frame(&g, 0, 1 << 30).is_ok_and(|x| !x.hash_ok));
        assert_eq!(
            parse_frame(&f[..f.len() - 1], 0, 1 << 30).map_err(|e| e.detail),
            Err("input truncated in hash".to_string())
        );
        assert_eq!(
            parse_frame(&f, 0, 4).map_err(|e| e.class),
            Err("PayloadTooLarge")
        );
    }
}
