//! The fixed 32-byte file header.

use crate::error::{read_exact_or, FormatError};
use crate::magic::MAGIC;
use std::io::{Read, Write};

/// Format version (major, minor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatVersion {
    /// Major version; readers reject other majors.
    pub major: u16,
    /// Minor version; readers accept any.
    pub minor: u16,
}

impl FormatVersion {
    /// The version this crate writes: 1.0.
    pub const CURRENT: FormatVersion = FormatVersion { major: 1, minor: 0 };
}

/// Header flags. Bits 2..=31 are reserved and must be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeaderFlags(u32);

impl HeaderFlags {
    /// Archive content is encrypted.
    pub const ENCRYPTED: HeaderFlags = HeaderFlags(1);
    /// The archive may be listed without the key.
    pub const LISTABLE: HeaderFlags = HeaderFlags(2);
    /// No flags.
    pub const EMPTY: HeaderFlags = HeaderFlags(0);

    const KNOWN: u32 = 0b11;

    /// Raw bits.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// Validate raw bits; reserved bits give `ReservedHeaderBits`.
    pub fn from_bits(bits: u32) -> Result<Self, FormatError> {
        if bits & !Self::KNOWN != 0 {
            return Err(FormatError::ReservedHeaderBits {
                bits: bits & !Self::KNOWN,
            });
        }
        Ok(HeaderFlags(bits))
    }

    /// True when every bit of `other` is set in `self`.
    pub fn contains(self, other: HeaderFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union of two flag sets.
    pub fn union(self, other: HeaderFlags) -> HeaderFlags {
        HeaderFlags(self.0 | other.0)
    }
}

/// The file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Format version.
    pub version: FormatVersion,
    /// Header flags.
    pub flags: HeaderFlags,
    /// 16 random bytes chosen by the writer.
    pub archive_id: [u8; 16],
}

impl Header {
    /// Encoded length in bytes.
    pub const LEN: usize = 32;

    /// A header of the current version.
    pub fn new(flags: HeaderFlags, archive_id: [u8; 16]) -> Self {
        Header {
            version: FormatVersion::CURRENT,
            flags,
            archive_id,
        }
    }

    /// Write the 32 header bytes.
    pub fn write(&self, w: &mut impl Write) -> Result<(), FormatError> {
        let mut b = [0u8; Self::LEN];
        b[0..8].copy_from_slice(&MAGIC);
        b[8..10].copy_from_slice(&self.version.major.to_le_bytes());
        b[10..12].copy_from_slice(&self.version.minor.to_le_bytes());
        b[12..16].copy_from_slice(&self.flags.0.to_le_bytes());
        b[16..32].copy_from_slice(&self.archive_id);
        w.write_all(&b)?;
        Ok(())
    }

    /// Read and validate a header.
    pub fn read(r: &mut impl Read) -> Result<Self, FormatError> {
        let mut magic = [0u8; 8];
        read_exact_or(r, &mut magic, "magic")?;
        if magic != MAGIC {
            return Err(FormatError::BadMagic);
        }
        let mut rest = [0u8; Self::LEN - 8];
        read_exact_or(r, &mut rest, "header")?;
        let major = u16::from_le_bytes([rest[0], rest[1]]);
        let minor = u16::from_le_bytes([rest[2], rest[3]]);
        let flag_bits = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
        if major != FormatVersion::CURRENT.major {
            return Err(FormatError::UnsupportedMajor { found: major });
        }
        let flags = HeaderFlags::from_bits(flag_bits)?;
        let mut archive_id = [0u8; 16];
        archive_id.copy_from_slice(&rest[8..24]);
        Ok(Header {
            version: FormatVersion { major, minor },
            flags,
            archive_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(h: &Header) -> Vec<u8> {
        let mut v = Vec::new();
        h.write(&mut v).unwrap();
        v
    }

    fn sample() -> Header {
        Header::new(HeaderFlags::ENCRYPTED.union(HeaderFlags::LISTABLE), [7; 16])
    }

    #[test]
    fn round_trip_and_len() {
        let h = sample();
        let b = bytes(&h);
        assert_eq!(Header::LEN, 32);
        assert_eq!(b.len(), 32);
        assert_eq!(Header::read(&mut b.as_slice()).unwrap(), h);
    }

    #[test]
    fn bad_magic() {
        let mut b = bytes(&sample());
        b[1] = b'X';
        let e = Header::read(&mut b.as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::BadMagic));
    }

    #[test]
    fn unsupported_major() {
        let mut b = bytes(&sample());
        b[8] = 2;
        let e = Header::read(&mut b.as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::UnsupportedMajor { found: 2 }));
    }

    #[test]
    fn any_minor_accepted() {
        let mut b = bytes(&sample());
        b[10] = 9;
        assert_eq!(Header::read(&mut b.as_slice()).unwrap().version.minor, 9);
    }

    #[test]
    fn reserved_bit() {
        let mut b = bytes(&sample());
        b[12] |= 0b100;
        let e = Header::read(&mut b.as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::ReservedHeaderBits { bits: 4 }));
    }

    #[test]
    fn truncated() {
        let b = bytes(&sample());
        let e = Header::read(&mut &b[..31]).unwrap_err();
        assert!(matches!(e, FormatError::Truncated { what: "header" }));
        let e = Header::read(&mut &b[..3]).unwrap_err();
        assert!(matches!(e, FormatError::Truncated { what: "magic" }));
    }
}
