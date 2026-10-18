//! `bwt` (revision 1.3, section 8 "Decoding `bwt`"): the step's input layout, its bound I(O, B),
//! its memory term, checks (1) to (5) in the text's order and the inverse transform, written from
//! the text's definition (the sorted rotations of T·$, the LF mapping, the backward walk).

use crate::block::too_large;
use crate::deflate::Memory;
use crate::error::{Error, Result};

/// The fixed allowance of a step's term (section 8, "Resources"): 1 MiB.
pub const FIXED_TERM: u64 = 1_048_576;

/// `BwtError { reason }`.
pub fn bwt_error(reason: &str) -> Error {
    Error::new("BwtError", format!("bwt: {reason}"))
}

/// r(n): the smallest power of two at least max(2, ceil(n / 32)).
pub fn r_of(n: u64) -> u64 {
    n.div_ceil(32).max(2).next_power_of_two()
}

/// c(n): ceil(n / r(n)), the number of indices of a block of n bytes (at most 32; 0 for n = 0).
pub fn c_of(n: u64) -> u64 {
    n.div_ceil(r_of(n))
}

/// I(out_len, B) = 8 + out_len + 4·Σ c(n_j) over the k = ceil(out_len / B) blocks (n_j = B except
/// the last, out_len − (k − 1)·B), in 64-bit unsigned arithmetic that saturates.
pub fn input_len(out_len: u64, block_size: u32) -> u64 {
    let b = u64::from(block_size);
    if out_len == 0 {
        return 8;
    }
    let k = out_len.div_ceil(b);
    let last = out_len - (k - 1) * b;
    let indices = (k - 1).saturating_mul(c_of(b)).saturating_add(c_of(last));
    8u64.saturating_add(out_len)
        .saturating_add(indices.saturating_mul(4))
}

/// term(O, B) = I(O, B) + 4·min(O, B) + 1048576, saturating at 2^64 − 1.
pub fn term(o: u64, block_size: u32) -> u64 {
    input_len(o, block_size)
        .saturating_add(o.min(u64::from(block_size)).saturating_mul(4))
        .saturating_add(FIXED_TERM)
}

/// Check (1): the step's term for output bound `o` against the decode memory left after
/// `max_block_plain` (`Refused`, field `decode_memory`). Runs before the step preceding `bwt`
/// too, with the same O (section 8, "Bounds").
pub fn check_memory(o: u64, block_size: u32, mem: &Memory) -> Result<()> {
    mem.check(term(o, block_size))
}

/// A `bwt` step: `input` its bytes, `block_size` its parameter B, `bound` its output bound O (the
/// block's `plain_len` on the last step, else the bound "Bounds" gives it), `last` whether it is
/// the last step. Checks (1) to (5) in the text's order; returns the `out_len` bytes of text.
pub fn decode(
    input: &[u8],
    block_size: u32,
    bound: u64,
    last: bool,
    mem: &Memory,
) -> Result<Vec<u8>> {
    // (1) the term, before anything is read.
    check_memory(bound, block_size, mem)?;
    // (2) the fixed part.
    if input.len() < 8 {
        return Err(bwt_error("length"));
    }
    let out_len = u64::from_le_bytes(input[..8].try_into().unwrap_or([0; 8]));
    // (3) `out_len` against O; nothing is allocated before this.
    if last {
        if out_len != bound {
            return Err(Error::new(
                "BlockLengthMismatch",
                format!("bwt: out_len {out_len} differs from plain_len {bound}"),
            ));
        }
    } else if out_len > bound {
        return Err(too_large(false, out_len, bound));
    }
    // (4) the whole length against I(out_len, B).
    if input.len() as u64 != input_len(out_len, block_size) {
        return Err(bwt_error("length"));
    }
    let Ok(total) = usize::try_from(out_len) else {
        return Err(bwt_error("length"));
    };
    // (5) per block, in input order: the index range, then the inverse.
    let b = u64::from(block_size);
    let mut out = Vec::with_capacity(total);
    let mut pos = 8usize;
    let mut left = out_len;
    while left > 0 {
        let n = left.min(b);
        let c = c_of(n);
        // Within I(out_len, B) by check (4): these slices exist.
        let (ix_len, n_us) = (c as usize * 4, n as usize);
        let indices: Vec<u32> = input[pos..pos + ix_len]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|q| u32::from_le_bytes(*q))
            .collect();
        pos += ix_len;
        let bytes = &input[pos..pos + n_us];
        pos += n_us;
        if indices.iter().any(|&i| i == 0 || u64::from(i) > n) {
            return Err(bwt_error("index"));
        }
        inverse(bytes, &indices, &mut out)?;
        left -= n;
    }
    Ok(out)
}

/// The inverse of one block (section 8, "Inverse"): `l` the n transformed bytes, `indices` its
/// c(n) indices (index[0] the primary), every index already in 1..=n. Appends the n bytes of
/// text to `out`, or `BwtError` `inverse` when a validity condition fails.
fn inverse(l: &[u8], indices: &[u32], out: &mut Vec<u8>) -> Result<()> {
    let n = l.len();
    let primary = indices[0] as usize;
    // n + 1 rows: n is at most B (a u32), so every row fits a u32 (the LF table below); only an
    // address space that cannot count the rows refuses, with `length` as check (4) says.
    let rows = n.checked_add(1).ok_or_else(|| bwt_error("length"))?;
    // L with the sentinel at row `primary`: row i < primary reads l[i], row i > primary l[i - 1].
    let at = |i: usize| -> Option<u8> {
        match i.cmp(&primary) {
            std::cmp::Ordering::Less => Some(l[i]),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(l[i - 1]),
        }
    };
    // C[c] = 1 + the number of transformed bytes below c.
    let mut counts = [0usize; 256];
    for &b in l {
        counts[usize::from(b)] += 1;
    }
    let mut first = [0usize; 256];
    let mut acc = 1usize;
    for (c, f) in first.iter_mut().enumerate() {
        *f = acc;
        acc = acc.saturating_add(counts[c]);
    }
    // LF(i) = C[L[i]] + occurrences of L[i] in L[0..i); LF(primary) = 0. Every value is a row
    // (at most n), so the table holds u32 entries: 4 bytes per byte of the block, as the term says.
    let mut lf = vec![0u32; rows];
    let mut occ = [0usize; 256];
    for (i, slot) in lf.iter_mut().enumerate() {
        if let Some(b) = at(i) {
            let bi = usize::from(b);
            *slot = u32::try_from(first[bi] + occ[bi]).map_err(|_| bwt_error("length"))?;
            occ[bi] += 1;
        }
    }
    // p_n = 0, p_k = LF(p_{k+1}); T[k] = L[p_{k+1}]. The walk checks p_k against index[0] for
    // 0 < k < n, p_{j·r} against index[j], and p_0 against index[0].
    let r = r_of(n as u64) as usize;
    let start = out.len();
    out.resize(start + n, 0);
    let mut p = 0usize;
    let mut k = n;
    while k > 0 {
        let row = lf[p] as usize;
        // T[k-1] = L[p_k], where p_k is the row before the step.
        out[start + k - 1] = at(p).ok_or_else(|| bwt_error("inverse"))?;
        p = row;
        k -= 1;
        if k > 0 && p == primary {
            return Err(bwt_error("inverse"));
        }
        if k.is_multiple_of(r) {
            let j = k / r;
            if j < indices.len() && indices[j] as usize != p {
                return Err(bwt_error("inverse"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn mem() -> Memory {
        Memory {
            decode_memory: u64::MAX,
            memory: u64::MAX,
            max_block_plain: 1 << 20,
        }
    }

    /// The text's values: r and c, I(64, B) = 200, I(65, B) = 141, an out_len of 0.
    #[test]
    fn lengths_of_the_text() {
        assert_eq!((r_of(6), c_of(6)), (2, 3));
        assert_eq!((r_of(64), c_of(64)), (2, 32));
        assert_eq!((r_of(65), c_of(65)), (4, 17));
        assert_eq!(r_of(4096), 128);
        assert_eq!(c_of(4096), 32);
        assert_eq!(input_len(64, 1 << 20), 200);
        assert_eq!(input_len(65, 1 << 20), 141);
        assert_eq!(input_len(0, 7), 8);
        // The bound vector: O = 16384 in blocks of 4096 (4 blocks of 32 indices).
        assert_eq!(input_len(16384, 4096), 16904);
        assert_eq!(term(16384, 4096), 16904 + 16384 + FIXED_TERM);
        // A last block of 1 byte: c(1) = 1.
        assert_eq!(input_len(4097, 4096), 8 + 4097 + 4 * 33);
        assert_eq!(term(u64::MAX, 1), u64::MAX);
    }

    /// The text's example: "banana" is "annbaa" with indices 4, 6, 5.
    fn banana() -> Vec<u8> {
        let mut v = 6u64.to_le_bytes().to_vec();
        for i in [4u32, 6, 5] {
            v.extend_from_slice(&i.to_le_bytes());
        }
        v.extend_from_slice(b"annbaa");
        v
    }

    #[test]
    fn banana_inverts() {
        let out = decode(&banana(), 16, 6, true, &mem()).unwrap();
        assert_eq!(out, b"banana");
        // Not the last step, out_len below the bound: decodes.
        assert_eq!(
            decode(&banana(), 16, 100, false, &mem()).unwrap(),
            b"banana"
        );
        // Two blocks of 3: "ban" and "ana".
        let mut v = 6u64.to_le_bytes().to_vec();
        for (ix, t) in [([2u32, 3], b"nba"), ([2u32, 1], b"ana")] {
            for i in ix {
                v.extend_from_slice(&i.to_le_bytes());
            }
            v.extend_from_slice(t);
        }
        assert_eq!(input_len(6, 3), v.len() as u64);
        assert_eq!(decode(&v, 3, 6, true, &mem()).unwrap(), b"banana");
    }

    /// Checks (2), (3) and (4): the length classes and the bound.
    #[test]
    fn length_checks() {
        let class =
            |v: &[u8], bound: u64, last: bool| decode(v, 16, bound, last, &mem()).unwrap_err();
        let e = class(&[0; 7], 6, true);
        assert_eq!((e.class, e.detail.as_str()), ("BwtError", "bwt: length"));
        assert_eq!(class(&banana(), 7, true).class, "BlockLengthMismatch");
        let e = class(&banana(), 5, false);
        assert_eq!(e.class, "PayloadTooLarge");
        assert_eq!(e.detail, "payload length 6 exceeds limit 5");
        let mut short = banana();
        short.pop();
        assert_eq!(class(&short, 6, true).detail, "bwt: length");
        let mut long = banana();
        long.push(0);
        assert_eq!(class(&long, 6, true).detail, "bwt: length");
        // out_len 0: 8 bytes, nothing out.
        assert_eq!(
            decode(&0u64.to_le_bytes(), 16, 0, true, &mem()).unwrap(),
            b""
        );
    }

    /// Check (5): an index of 0 or above n is `index` (every index, before the inverse); a wrong
    /// auxiliary index with a valid primary walk is `inverse`; a wrong primary is `inverse`.
    #[test]
    fn index_and_inverse_checks() {
        let mut v = banana();
        v[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            decode(&v, 16, 6, true, &mem()).unwrap_err().detail,
            "bwt: index"
        );
        let mut v = banana();
        v[16..20].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(
            decode(&v, 16, 6, true, &mem()).unwrap_err().detail,
            "bwt: index"
        );
        // The primary valid, index[1] replaced by index[2]: the chain does not end where stated.
        let mut v = banana();
        v[12..16].copy_from_slice(&5u32.to_le_bytes());
        assert_eq!(
            decode(&v, 16, 6, true, &mem()).unwrap_err().detail,
            "bwt: inverse"
        );
        // A wrong primary.
        let mut v = banana();
        v[8..12].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(
            decode(&v, 16, 6, true, &mem()).unwrap_err().detail,
            "bwt: inverse"
        );
        // Transformed bytes that are no rotation's column: "aaaaab" with primary 1 cycles early.
        let mut v = 6u64.to_le_bytes().to_vec();
        for i in [1u32, 2, 3] {
            v.extend_from_slice(&i.to_le_bytes());
        }
        v.extend_from_slice(b"aaaaab");
        assert_eq!(
            decode(&v, 16, 6, true, &mem()).unwrap_err().detail,
            "bwt: inverse"
        );
    }

    /// Check (1): the term against the decode memory left after `max_block_plain`, before the
    /// input is looked at.
    #[test]
    fn memory_check() {
        let m = Memory {
            decode_memory: 1000 + term(6, 16),
            memory: u64::MAX,
            max_block_plain: 1000,
        };
        assert!(decode(&banana(), 16, 6, true, &m).is_ok());
        let m = Memory {
            decode_memory: 999 + term(6, 16),
            ..m
        };
        let e = decode(&[], 16, 6, true, &m).unwrap_err();
        assert_eq!(e.class, "Refused");
        assert!(e.detail.contains("decode_memory"));
    }

    /// A single-byte text and a text of every byte value, with the transform computed here by the
    /// definition (sorted rotations of T·$).
    #[test]
    fn transform_by_definition_round_trips() {
        for t in [
            b"x".to_vec(),
            (0..=255u8).collect::<Vec<u8>>(),
            vec![7u8; 300],
            b"mississippi".repeat(13),
        ] {
            let n = t.len();
            // Rotations of T·$ with $ = -1, sorted.
            let s: Vec<i16> = t.iter().map(|&b| i16::from(b)).chain([-1]).collect();
            let mut rows: Vec<usize> = (0..=n).collect();
            rows.sort_by(|&a, &b| {
                (0..=n)
                    .map(|i| s[(a + i) % (n + 1)])
                    .cmp((0..=n).map(|i| s[(b + i) % (n + 1)]))
            });
            let row_of = |p: usize| rows.iter().position(|&x| x == p).unwrap() as u32;
            let lastcol: Vec<u8> = rows
                .iter()
                .filter_map(|&st| {
                    let c = s[(st + n) % (n + 1)];
                    (c >= 0).then_some(c as u8)
                })
                .collect();
            let r = r_of(n as u64) as usize;
            let mut v = (n as u64).to_le_bytes().to_vec();
            for j in 0..c_of(n as u64) as usize {
                v.extend_from_slice(&row_of(j * r).to_le_bytes());
            }
            v.extend_from_slice(&lastcol);
            assert_eq!(decode(&v, 1 << 20, n as u64, true, &mem()).unwrap(), t);
        }
    }
}
