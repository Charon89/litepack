//! The hashed frame grammar: `kind | flags | payload_len | payload | blake3(payload)`.

use crate::error::{read_exact_or, FormatError};
use crate::varint;
use std::io::{Read, Write};

/// Kind of a frame. Raw values `7..=0x7FFF` are reserved, `0x8000..` experimental.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum FrameKind {
    /// Entry table.
    EntryTable = 1,
    /// Chunk data.
    ChunkData = 2,
    /// Records.
    Records = 3,
    /// Recovery data.
    Recovery = 4,
    /// Index.
    Index = 5,
    /// Trailer.
    Trailer = 6,
}

impl FrameKind {
    /// All known kinds in numeric order.
    pub const ALL: [FrameKind; 6] = [
        FrameKind::EntryTable,
        FrameKind::ChunkData,
        FrameKind::Records,
        FrameKind::Recovery,
        FrameKind::Index,
        FrameKind::Trailer,
    ];

    /// The kind for a raw value, if this reader knows it.
    pub fn from_u16(k: u16) -> Option<FrameKind> {
        Self::ALL.iter().copied().find(|c| *c as u16 == k)
    }

    /// Stable lower-case name used in the spec.
    pub fn name(self) -> &'static str {
        match self {
            FrameKind::EntryTable => "EntryTable",
            FrameKind::ChunkData => "ChunkData",
            FrameKind::Records => "Records",
            FrameKind::Recovery => "Recovery",
            FrameKind::Index => "Index",
            FrameKind::Trailer => "Trailer",
        }
    }
}

/// Frame flags. Bit 0 is `MUST_UNDERSTAND`; bits 1..=15 are reserved and must be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameFlags(u16);

impl FrameFlags {
    /// A reader that does not know the kind must fail instead of skipping.
    pub const MUST_UNDERSTAND: FrameFlags = FrameFlags(1);
    /// No flags.
    pub const EMPTY: FrameFlags = FrameFlags(0);

    /// Raw bits.
    pub fn bits(self) -> u16 {
        self.0
    }

    /// Validate raw bits; reserved bits give `ReservedFrameBits`.
    pub fn from_bits(bits: u16) -> Result<Self, FormatError> {
        if bits & !1 != 0 {
            return Err(FormatError::ReservedFrameBits { bits: bits & !1 });
        }
        Ok(FrameFlags(bits))
    }

    /// True when every bit of `other` is set in `self`.
    pub fn contains(self, other: FrameFlags) -> bool {
        self.0 & other.0 == other.0
    }
}

/// A frame of a known kind with its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Frame kind.
    pub kind: FrameKind,
    /// Frame flags.
    pub flags: FrameFlags,
    /// Payload bytes (hashed on write, verified on read).
    pub payload: Vec<u8>,
}

/// Result of reading one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadFrame {
    /// A frame of known kind.
    Known(Frame),
    /// A skipped frame of unknown kind; its hash was verified.
    Unknown {
        /// Raw kind.
        kind: u16,
        /// Flags.
        flags: FrameFlags,
        /// Payload length that was skipped.
        payload_len: u64,
    },
}

/// Limits applied by the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    /// Largest accepted `payload_len`.
    pub max_payload: u64,
}

impl Default for ReadLimits {
    fn default() -> Self {
        ReadLimits {
            max_payload: 1 << 30,
        }
    }
}

const HASH_LEN: usize = 32;

impl Frame {
    /// Write the frame, hashing the payload.
    pub fn write(&self, w: &mut impl Write) -> Result<(), FormatError> {
        w.write_all(&(self.kind as u16).to_le_bytes())?;
        w.write_all(&self.flags.0.to_le_bytes())?;
        varint::write(w, self.payload.len() as u64)?;
        w.write_all(&self.payload)?;
        w.write_all(blake3::hash(&self.payload).as_bytes())?;
        Ok(())
    }

    /// Number of bytes `write` produces.
    pub fn encoded_len(&self) -> u64 {
        let n = self.payload.len();
        (4 + varint::len(n as u64) + n + HASH_LEN) as u64
    }

    /// Read one frame. `Ok(None)` is a clean end of input at a frame boundary.
    pub fn read(r: &mut impl Read, limits: &ReadLimits) -> Result<Option<ReadFrame>, FormatError> {
        let mut head = [0u8; 4];
        loop {
            match r.read(&mut head[..1]) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        read_exact_or(r, &mut head[1..], "frame header")?;
        let kind = u16::from_le_bytes([head[0], head[1]]);
        let flags = FrameFlags::from_bits(u16::from_le_bytes([head[2], head[3]]))?;
        if kind == 0 {
            return Err(FormatError::InvalidKind);
        }
        let len = varint::read(r).map_err(|e| match e {
            FormatError::Truncated { .. } => FormatError::Truncated {
                what: "frame header",
            },
            other => other,
        })?;
        if len > limits.max_payload {
            return Err(FormatError::PayloadTooLarge {
                len,
                max: limits.max_payload,
            });
        }
        let known = FrameKind::from_u16(kind);
        if known.is_none() && flags.contains(FrameFlags::MUST_UNDERSTAND) {
            return Err(FormatError::UnknownMustUnderstand { kind });
        }

        let mut hasher = blake3::Hasher::new();
        let mut payload = Vec::new();
        let mut remaining = len;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            read_exact_or(r, &mut buf[..want], "payload")?;
            hasher.update(&buf[..want]);
            if known.is_some() {
                payload.extend_from_slice(&buf[..want]);
            }
            remaining -= want as u64;
        }
        let mut hash = [0u8; HASH_LEN];
        read_exact_or(r, &mut hash, "hash")?;
        if hasher.finalize().as_bytes() != &hash {
            return Err(FormatError::HashMismatch { kind });
        }
        Ok(Some(match known {
            Some(kind) => ReadFrame::Known(Frame {
                kind,
                flags,
                payload,
            }),
            None => ReadFrame::Unknown {
                kind,
                flags,
                payload_len: len,
            },
        }))
    }
}

/// The Markdown table of frame kinds, pasted verbatim into the spec.
pub fn frame_kind_table() -> String {
    let mut s = String::from("| Kind | Name |\n|---|---|\n");
    for k in FrameKind::ALL {
        s.push_str(&format!("| {} | {} |\n", k as u16, k.name()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    fn raw(kind: u16, flags: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(&flags.to_le_bytes());
        varint::write(&mut v, payload.len() as u64).unwrap();
        v.extend_from_slice(payload);
        v.extend_from_slice(blake3::hash(payload).as_bytes());
        v
    }

    fn read_one(b: &[u8]) -> Result<Option<ReadFrame>, FormatError> {
        Frame::read(&mut &b[..], &ReadLimits::default())
    }

    fn encode(f: &Frame) -> Vec<u8> {
        let mut v = Vec::new();
        f.write(&mut v).unwrap();
        v
    }

    proptest! {
        #[test]
        fn round_trip(
            ki in 0usize..6,
            must in any::<bool>(),
            payload in proptest::collection::vec(any::<u8>(), 0..=65536),
        ) {
            let f = Frame {
                kind: FrameKind::ALL[ki],
                flags: if must { FrameFlags::MUST_UNDERSTAND } else { FrameFlags::EMPTY },
                payload,
            };
            let b = encode(&f);
            prop_assert_eq!(b.len() as u64, f.encoded_len());
            let mut cur = &b[..];
            let got = Frame::read(&mut cur, &ReadLimits::default()).unwrap();
            prop_assert_eq!(got, Some(ReadFrame::Known(f)));
            prop_assert!(Frame::read(&mut cur, &ReadLimits::default()).unwrap().is_none());
        }
    }

    fn sample() -> Frame {
        Frame {
            kind: FrameKind::ChunkData,
            flags: FrameFlags::EMPTY,
            payload: (0..200u16).map(|i| i as u8).collect(),
        }
    }

    #[test]
    fn flipped_payload_byte() {
        let mut b = encode(&sample());
        b[10] ^= 1;
        assert!(matches!(
            read_one(&b),
            Err(FormatError::HashMismatch { kind: 2 })
        ));
    }

    #[test]
    fn flipped_hash_byte() {
        let mut b = encode(&sample());
        let n = b.len();
        b[n - 1] ^= 1;
        assert!(matches!(
            read_one(&b),
            Err(FormatError::HashMismatch { kind: 2 })
        ));
    }

    #[test]
    fn kind_zero_invalid() {
        let b = raw(0, 0, b"x");
        assert!(matches!(read_one(&b), Err(FormatError::InvalidKind)));
    }

    #[test]
    fn unknown_kind_skipped_and_next_reads() {
        let mut b = raw(7, 0, &[9u8; 300]);
        b.extend(encode(&sample()));
        let mut cur = &b[..];
        let l = ReadLimits::default();
        match Frame::read(&mut cur, &l).unwrap() {
            Some(ReadFrame::Unknown {
                kind: 7,
                payload_len: 300,
                flags,
            }) => assert_eq!(flags, FrameFlags::EMPTY),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            Frame::read(&mut cur, &l).unwrap(),
            Some(ReadFrame::Known(sample()))
        );
    }

    #[test]
    fn unknown_kind_hash_still_checked() {
        let mut b = raw(0x9000, 0, b"abc");
        b[5] ^= 1;
        assert!(matches!(
            read_one(&b),
            Err(FormatError::HashMismatch { kind: 0x9000 })
        ));
    }

    #[test]
    fn unknown_must_understand() {
        let b = raw(7, 1, b"abc");
        assert!(matches!(
            read_one(&b),
            Err(FormatError::UnknownMustUnderstand { kind: 7 })
        ));
    }

    #[test]
    fn reserved_flag_bit() {
        let b = raw(1, 2, b"abc");
        assert!(matches!(
            read_one(&b),
            Err(FormatError::ReservedFrameBits { bits: 2 })
        ));
    }

    #[test]
    fn payload_too_large_not_read() {
        let b = raw(2, 0, &[1u8; 100]);
        let header_len = 4 + varint::len(100) as u64;
        let mut cur = Cursor::new(&b[..]);
        let limits = ReadLimits { max_payload: 99 };
        let e = Frame::read(&mut cur, &limits).unwrap_err();
        assert!(matches!(
            e,
            FormatError::PayloadTooLarge { len: 100, max: 99 }
        ));
        assert_eq!(cur.position(), header_len);
    }

    #[test]
    fn huge_declared_len_does_not_allocate() {
        let mut b = Vec::new();
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        varint::write(&mut b, u64::MAX).unwrap();
        assert!(matches!(
            read_one(&b),
            Err(FormatError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn truncations() {
        let b = encode(&sample());
        let n = b.len();
        assert!(matches!(
            read_one(&b[..3]),
            Err(FormatError::Truncated {
                what: "frame header"
            })
        ));
        assert!(matches!(
            read_one(&b[..4]),
            Err(FormatError::Truncated {
                what: "frame header"
            })
        ));
        assert!(matches!(
            read_one(&b[..50]),
            Err(FormatError::Truncated { what: "payload" })
        ));
        assert!(matches!(
            read_one(&b[..n - 1]),
            Err(FormatError::Truncated { what: "hash" })
        ));
        assert!(matches!(
            read_one(&b[..n - 32]),
            Err(FormatError::Truncated { what: "hash" })
        ));
    }

    #[test]
    fn clean_end() {
        assert!(read_one(&[]).unwrap().is_none());
    }

    #[test]
    fn kind_table_and_names() {
        for k in FrameKind::ALL {
            assert_eq!(FrameKind::from_u16(k as u16), Some(k));
        }
        assert_eq!(FrameKind::from_u16(0), None);
        assert_eq!(FrameKind::from_u16(7), None);
        assert!(frame_kind_table().contains("| 6 | Trailer |"));
    }
}
