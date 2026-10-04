//! The fixed-size trailer frame at the very end of an archive (spec section 6).

use crate::error::{read_exact_or, FormatError};
use crate::frame::{Frame, FrameFlags, FrameKind};
use std::io::{Read, Seek, SeekFrom, Write};

/// Length of the trailer payload in bytes.
pub const TRAILER_PAYLOAD_LEN: usize = 72;
/// Length of the whole trailer frame: kind (2), flags (2), payload length
/// varint (1), payload (72) and hash (32).
pub const TRAILER_FRAME_LEN: u64 = 2 + 2 + 1 + TRAILER_PAYLOAD_LEN as u64 + 32;

const FRAME_LEN: usize = TRAILER_FRAME_LEN as usize;
const PAYLOAD_AT: usize = 5;
const HASH_AT: usize = PAYLOAD_AT + TRAILER_PAYLOAD_LEN;

/// The trailer: finds and authenticates the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trailer {
    /// Absolute offset of the index frame.
    pub index_offset: u64,
    /// Whole encoded length of the index frame.
    pub index_len: u64,
    /// BLAKE3-256 of the index frame's payload.
    pub index_hash: [u8; 32],
    /// 0 for an archive written in one go; later appends increment it.
    pub generation: u64,
    /// Must equal the header's `archive_id`.
    pub archive_id: [u8; 16],
}

impl Trailer {
    fn payload(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(TRAILER_PAYLOAD_LEN);
        p.extend_from_slice(&self.index_offset.to_le_bytes());
        p.extend_from_slice(&self.index_len.to_le_bytes());
        p.extend_from_slice(&self.index_hash);
        p.extend_from_slice(&self.generation.to_le_bytes());
        p.extend_from_slice(&self.archive_id);
        p
    }

    /// Write the whole trailer frame (`TRAILER_FRAME_LEN` bytes).
    pub fn write(&self, w: &mut impl Write) -> Result<(), FormatError> {
        Frame {
            kind: FrameKind::Trailer,
            flags: FrameFlags::EMPTY,
            payload: self.payload(),
        }
        .write(w)
    }

    /// Decode a whole trailer frame. Bytes that are not a trailer frame of the
    /// fixed shape give `NoTrailer`; a wrong hash gives `HashMismatch`.
    fn parse_frame(b: &[u8; FRAME_LEN]) -> Result<Trailer, FormatError> {
        let kind = u16::from_le_bytes([b[0], b[1]]);
        let flags = u16::from_le_bytes([b[2], b[3]]);
        if kind != FrameKind::Trailer as u16
            || flags != FrameFlags::EMPTY.bits()
            || b[4] as usize != TRAILER_PAYLOAD_LEN
        {
            return Err(FormatError::NoTrailer);
        }
        let payload = &b[PAYLOAD_AT..HASH_AT];
        if blake3::hash(payload).as_bytes() != &b[HASH_AT..] {
            return Err(FormatError::HashMismatch { kind });
        }
        let u = |at: usize| {
            let mut x = [0u8; 8];
            x.copy_from_slice(&payload[at..at + 8]);
            u64::from_le_bytes(x)
        };
        let mut index_hash = [0u8; 32];
        index_hash.copy_from_slice(&payload[16..48]);
        let mut archive_id = [0u8; 16];
        archive_id.copy_from_slice(&payload[56..72]);
        Ok(Trailer {
            index_offset: u(0),
            index_len: u(8),
            index_hash,
            generation: u(48),
            archive_id,
        })
    }

    /// Read the trailer from the last `TRAILER_FRAME_LEN` bytes of an archive
    /// of `archive_len` bytes. An archive shorter than that, or a tail that is
    /// not a trailer frame, is `NoTrailer`; a tail of the right shape with a
    /// wrong hash is `HashMismatch`.
    pub fn read_tail(r: &mut (impl Read + Seek), archive_len: u64) -> Result<Trailer, FormatError> {
        if archive_len < TRAILER_FRAME_LEN {
            return Err(FormatError::NoTrailer);
        }
        r.seek(SeekFrom::Start(archive_len - TRAILER_FRAME_LEN))?;
        let mut b = [0u8; FRAME_LEN];
        read_exact_or(r, &mut b, "trailer")?;
        Self::parse_frame(&b)
    }
}

/// The Markdown byte table of the trailer payload, pasted verbatim into the spec.
pub fn trailer_layout_table() -> String {
    String::from(
        "| Offset | Size | Field | Meaning |\n|---|---|---|---|\n\
         | 0 | 8 | index_offset | absolute offset of the index frame (u64, little-endian) |\n\
         | 8 | 8 | index_len | whole encoded length of the index frame (u64) |\n\
         | 16 | 32 | index_hash | BLAKE3-256 of the index frame's payload |\n\
         | 48 | 8 | generation | 0 for an archive written in one go (u64) |\n\
         | 56 | 16 | archive_id | must equal the header's archive_id |\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample() -> Trailer {
        Trailer {
            index_offset: 12345,
            index_len: 678,
            index_hash: [7; 32],
            generation: 3,
            archive_id: [9; 16],
        }
    }

    fn bytes(t: &Trailer) -> Vec<u8> {
        let mut v = Vec::new();
        t.write(&mut v).unwrap();
        v
    }

    #[test]
    fn constants_and_round_trip() {
        assert_eq!(TRAILER_FRAME_LEN, 109);
        assert_eq!(TRAILER_PAYLOAD_LEN, 72);
        let t = sample();
        let b = bytes(&t);
        assert_eq!(b.len() as u64, TRAILER_FRAME_LEN);
        let mut c = Cursor::new(b.clone());
        assert_eq!(Trailer::read_tail(&mut c, b.len() as u64).unwrap(), t);
    }

    #[test]
    fn read_tail_after_a_body() {
        let t = sample();
        let mut v = vec![0xAAu8; 1000];
        t.write(&mut v).unwrap();
        let n = v.len() as u64;
        assert_eq!(Trailer::read_tail(&mut Cursor::new(v), n).unwrap(), t);
    }

    #[test]
    fn is_an_ordinary_frame() {
        let b = bytes(&sample());
        let f = Frame::read(&mut &b[..], &crate::frame::ReadLimits::default())
            .unwrap()
            .unwrap();
        assert!(matches!(
            f,
            crate::frame::ReadFrame::Known(Frame {
                kind: FrameKind::Trailer,
                ..
            })
        ));
    }

    #[test]
    fn flipped_hash_and_payload_bytes() {
        let b = bytes(&sample());
        for at in [HASH_AT, HASH_AT + 31, PAYLOAD_AT, PAYLOAD_AT + 40] {
            let mut x = b.clone();
            x[at] ^= 1;
            let n = x.len() as u64;
            assert!(
                matches!(
                    Trailer::read_tail(&mut Cursor::new(x), n),
                    Err(FormatError::HashMismatch { kind: 6 })
                ),
                "byte {at}"
            );
        }
    }

    #[test]
    fn wrong_shape_is_no_trailer() {
        let b = bytes(&sample());
        for at in [0usize, 2, 4] {
            let mut x = b.clone();
            x[at] ^= 1;
            let n = x.len() as u64;
            assert!(matches!(
                Trailer::read_tail(&mut Cursor::new(x), n),
                Err(FormatError::NoTrailer)
            ));
        }
    }

    #[test]
    fn short_input() {
        let b = bytes(&sample());
        let short = &b[1..];
        assert_eq!(short.len(), 108);
        assert!(matches!(
            Trailer::read_tail(&mut Cursor::new(short.to_vec()), 108),
            Err(FormatError::NoTrailer)
        ));
        assert!(matches!(
            Trailer::read_tail(&mut Cursor::new(Vec::new()), 0),
            Err(FormatError::NoTrailer)
        ));
    }

    #[test]
    fn layout_table_offsets_add_up() {
        let t = trailer_layout_table();
        assert!(t.contains("| 56 | 16 | archive_id |"));
        assert_eq!(8 + 8 + 32 + 8 + 16, TRAILER_PAYLOAD_LEN);
    }
}
