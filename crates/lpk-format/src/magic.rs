//! The file magic.

/// `\x89LPK\r\n\x1a\n`: non-ASCII lead byte, name, CR LF, SUB, LF (the PNG pattern).
pub const MAGIC: [u8; 8] = [0x89, 0x4C, 0x50, 0x4B, 0x0D, 0x0A, 0x1A, 0x0A];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_bytes() {
        assert_eq!(&MAGIC[1..4], b"LPK");
        assert_eq!(MAGIC.len(), 8);
    }
}
