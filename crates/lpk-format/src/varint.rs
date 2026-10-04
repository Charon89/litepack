//! Unsigned LEB128 varints for `u64`, canonical form only when reading.

use crate::error::{read_exact_or, FormatError};
use std::io::{Read, Write};

/// Encoded length of `v` in bytes (1..=10).
pub fn len(v: u64) -> usize {
    let bits = 64 - (v | 1).leading_zeros() as usize;
    bits.div_ceil(7)
}

/// Write `v` as a canonical varint.
pub fn write(w: &mut impl Write, mut v: u64) -> Result<(), FormatError> {
    let mut buf = [0u8; 10];
    let mut n = 0;
    loop {
        let low = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            buf[n] = low;
            n += 1;
            break;
        }
        buf[n] = low | 0x80;
        n += 1;
    }
    w.write_all(&buf[..n])?;
    Ok(())
}

/// Read a canonical varint.
pub fn read(r: &mut impl Read) -> Result<u64, FormatError> {
    let mut v: u64 = 0;
    for i in 0..10 {
        let mut b = [0u8; 1];
        read_exact_or(r, &mut b, "varint")?;
        let group = u64::from(b[0] & 0x7F);
        if i == 9 && group > 1 {
            return Err(FormatError::VarintTooLong);
        }
        v |= group << (7 * i);
        if b[0] & 0x80 == 0 {
            if i > 0 && group == 0 {
                return Err(FormatError::NonCanonicalVarint);
            }
            return Ok(v);
        }
    }
    Err(FormatError::VarintTooLong)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn enc(v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        write(&mut out, v).unwrap();
        out
    }

    #[test]
    fn round_trip_edges() {
        for v in [0, 1, 127, 128, 16383, 16384, u64::from(u32::MAX), u64::MAX] {
            let bytes = enc(v);
            assert_eq!(bytes.len(), len(v), "len for {v}");
            assert_eq!(read(&mut bytes.as_slice()).unwrap(), v);
        }
        assert_eq!(len(u64::MAX), 10);
    }

    #[test]
    fn non_canonical() {
        let e = read(&mut [0x80u8, 0x00].as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::NonCanonicalVarint));
    }

    #[test]
    fn too_long() {
        let bytes = [0x80u8; 11];
        let e = read(&mut bytes.as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::VarintTooLong));
    }

    #[test]
    fn tenth_byte_overflow() {
        let mut bytes = vec![0xFFu8; 9];
        bytes.push(0x02);
        let e = read(&mut bytes.as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::VarintTooLong));
    }

    #[test]
    fn truncated() {
        let e = read(&mut [0x80u8].as_slice()).unwrap_err();
        assert!(matches!(e, FormatError::Truncated { what: "varint" }));
    }

    proptest! {
        #[test]
        fn round_trip(v in any::<u64>()) {
            let bytes = enc(v);
            prop_assert_eq!(bytes.len(), len(v));
            prop_assert_eq!(read(&mut bytes.as_slice()).unwrap(), v);
        }
    }
}
