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
    /// The version this crate writes by default: 1.0.
    pub const CURRENT: FormatVersion = FormatVersion { major: 1, minor: 0 };
    /// The latest revision this crate knows: 1.1 (`jpeg-reconstruct` decoding).
    pub const LATEST: FormatVersion = FormatVersion { major: 1, minor: 1 };
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

    /// Validate raw bits; reserved bits give `ReservedHeaderBits`, then
    /// `LISTABLE` without `ENCRYPTED` gives `BadHeaderFlags`.
    pub fn from_bits(bits: u32) -> Result<Self, FormatError> {
        if bits & !Self::KNOWN != 0 {
            return Err(FormatError::ReservedHeaderBits {
                bits: bits & !Self::KNOWN,
            });
        }
        if bits & Self::LISTABLE.0 != 0 && bits & Self::ENCRYPTED.0 == 0 {
            return Err(FormatError::BadHeaderFlags { bits });
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

/// The Markdown table of header flags, pasted verbatim into the spec.
pub fn header_flag_table() -> String {
    let enc = HeaderFlags::ENCRYPTED.bits().trailing_zeros();
    let list = HeaderFlags::LISTABLE.bits().trailing_zeros();
    let first_reserved = 32 - HeaderFlags::KNOWN.leading_zeros();
    format!(
        "| Bit | Name | Meaning |\n|---|---|---|\n\
         | {enc} | ENCRYPTED | the archive content is encrypted |\n\
         | {list} | LISTABLE | the archive can be listed without the key |\n\
         | {first_reserved}-31 | reserved | must be zero; a reader rejects the header otherwise |\n"
    )
}

/// The Markdown byte table of the header, pasted verbatim into the spec.
pub fn header_byte_table() -> String {
    let magic = MAGIC
        .iter()
        .map(|b| format!("0x{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let v = FormatVersion::CURRENT;
    format!(
        "| Offset | Size | Field | Value / meaning |\n|---|---|---|---|\n\
         | 0 | 8 | magic | `{magic}` (`\\x89LPK\\r\\n\\x1a\\n`) |\n\
         | 8 | 2 | version_major | {} |\n\
         | 10 | 2 | version_minor | the revision the writer wrote under: {}, or {} for revision 1.1 (see \"Revisions\") |\n\
         | 12 | 4 | flags | see below |\n\
         | 16 | 16 | archive_id | 16 random bytes chosen by the writer |\n",
        v.major,
        v.minor,
        FormatVersion::LATEST.minor
    )
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

    /// A header of version 1.`minor`.
    pub fn with_minor(flags: HeaderFlags, archive_id: [u8; 16], minor: u16) -> Self {
        Header {
            version: FormatVersion {
                major: FormatVersion::CURRENT.major,
                minor,
            },
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
        let mut all = [0u8; Self::LEN];
        read_exact_or(r, &mut all, "header")?;
        if all[0..8] != MAGIC {
            return Err(FormatError::BadMagic);
        }
        let rest = &all[8..];
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
    fn listable_without_encrypted_is_bad_header_flags() {
        let mut b = bytes(&sample());
        b[12] = 0b10;
        let e = Header::read(&mut b.as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::BadHeaderFlags { bits: 2 }));
        // Reserved bits are checked first.
        b[12] = 0b110;
        let e = Header::read(&mut b.as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::ReservedHeaderBits { bits: 4 }));
        // ENCRYPTED alone and no flags are fine.
        for f in [0u8, 1, 3] {
            b[12] = f;
            assert!(Header::read(&mut b.as_slice()).is_ok());
        }
    }

    #[test]
    fn major_checked_before_flags() {
        let mut b = bytes(&sample());
        b[8] = 2;
        b[12] |= 0b100;
        let e = Header::read(&mut b.as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::UnsupportedMajor { found: 2 }));
    }

    #[test]
    fn truncated() {
        let b = bytes(&sample());
        let e = Header::read(&mut &b[..31]).unwrap_err();
        assert!(matches!(e, FormatError::Truncated { what: "header" }));
        let e = Header::read(&mut &b[..3]).unwrap_err();
        assert!(matches!(e, FormatError::Truncated { what: "header" }));
    }
}
