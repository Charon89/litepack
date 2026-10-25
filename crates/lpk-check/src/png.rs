//! `png-filter` (revision 1.4, section 8 "Decoding `png-filter`"): the image's geometry, its
//! filtered length F, checks (2) to (4) on the record and checks (5) to (7) at the step.
//! Written from the specification text alone.

use crate::block::too_large;
use crate::error::{Error, Result};
use crate::record::{bad_record, PngRecord};

/// The Adam7 passes in order: (x0, y0, dx, dy).
const ADAM7: [(u64, u64, u64, u64); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Samples per pixel of a colour type (0, 2, 3, 4, 6); `None` for any other.
fn samples(color_type: u8) -> Option<u64> {
    match color_type {
        0 | 3 => Some(1),
        2 => Some(3),
        4 => Some(2),
        6 => Some(4),
        _ => None,
    }
}

/// Whether the (colour type, bit depth) pair is one the list allows (PNG table 11.1).
pub fn allowed(color_type: u8, bit_depth: u8) -> bool {
    match color_type {
        0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(bit_depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(bit_depth, 8 | 16),
        _ => false,
    }
}

/// Bits per pixel b = samples × `bit_depth` (0 for a colour type outside the list).
pub fn bits_per_pixel(r: &PngRecord) -> u64 {
    samples(r.color_type).unwrap_or(0) * u64::from(r.bit_depth)
}

/// The filter unit u = max(1, ⌊b / 8⌋) bytes.
pub fn unit(r: &PngRecord) -> u64 {
    (bits_per_pixel(r) / 8).max(1)
}

/// The bytes of a scanline of `w` pixels at `b` bits per pixel: ⌈w × b / 8⌉. With `w` below
/// 2^32 and `b` at most 64 the product cannot overflow.
pub fn scanline_bytes(w: u64, b: u64) -> u64 {
    (w * b).div_ceil(8)
}

/// The image's non-empty passes in order, each (width, height) in pixels: one pass of
/// `width` × `height` when not interlaced, Adam7's seven otherwise, a pass of width 0 or height 0
/// having no scanline and being left out.
pub fn passes(r: &PngRecord) -> Vec<(u64, u64)> {
    let (w, h) = (u64::from(r.width), u64::from(r.height));
    if r.interlace == 0 {
        return if w == 0 || h == 0 {
            Vec::new()
        } else {
            vec![(w, h)]
        };
    }
    ADAM7
        .iter()
        .map(|&(x0, y0, dx, dy)| {
            let pw = if w <= x0 { 0 } else { (w - x0).div_ceil(dx) };
            let ph = if h <= y0 { 0 } else { (h - y0).div_ceil(dy) };
            (pw, ph)
        })
        .filter(|&(pw, ph)| pw > 0 && ph > 0)
        .collect()
}

/// The image's scanline count: the sum of its passes' heights.
pub fn scanline_count(r: &PngRecord) -> u64 {
    passes(r).iter().map(|&(_, ph)| ph).sum()
}

/// The filtered length F: over every scanline, 1 + its bytes, in 64-bit unsigned arithmetic;
/// `None` when it is past 2^64 − 1 (it has no value).
pub fn filtered_len(r: &PngRecord) -> Option<u64> {
    let b = bits_per_pixel(r);
    passes(r).iter().try_fold(0u64, |acc, &(pw, ph)| {
        let line = scanline_bytes(pw, b).checked_add(1)?;
        acc.checked_add(line.checked_mul(ph)?)
    })
}

/// Checks (2) to (4) on record `id` (check (1), the lookup, is the caller's): its kind (`rec` is
/// `None` when the record is of another kind), the image, then F against the step's output
/// `bound` (the block's `plain_len` when `last`). Returns the record and F.
pub fn record_checks(
    id: u64,
    rec: Option<&PngRecord>,
    bound: u64,
    last: bool,
) -> Result<(&PngRecord, u64)> {
    // (2) the kind.
    let r = rec.ok_or_else(|| bad_record(id, "kind"))?;
    // (3) the image, in the text's order.
    if r.width < 1 {
        return Err(bad_record(id, "width"));
    }
    if r.height < 1 {
        return Err(bad_record(id, "height"));
    }
    if !allowed(r.color_type, r.bit_depth) {
        return Err(bad_record(id, "bit_depth"));
    }
    if r.filters.len() as u64 != scanline_count(r) || r.filters.iter().any(|&f| f > 4) {
        return Err(bad_record(id, "filters"));
    }
    // (4) F against the step's output bound.
    let f = filtered_len(r);
    if last {
        match f {
            Some(f) if f == bound => {}
            _ => {
                return Err(Error::new(
                    "BlockLengthMismatch",
                    format!(
                        "png-filter: record {id} filtered length {} differs from plain_len {bound}",
                        f.map_or_else(|| "past 2^64 - 1".to_string(), |f| f.to_string())
                    ),
                ))
            }
        }
    }
    match f {
        Some(f) if f <= bound => Ok((r, f)),
        Some(f) => Err(too_large(false, f, bound)),
        None => Err(too_large(false, u64::MAX, bound)),
    }
}

/// The predictor of filter type `t` (1 to 4) for the byte with left `a`, above `b` and upper
/// left `c`; type 0 predicts 0.
fn predict(t: u8, a: u8, b: u8, c: u8) -> u8 {
    match t {
        1 => a,
        2 => b,
        3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
        4 => {
            let (ai, bi, ci) = (i32::from(a), i32::from(b), i32::from(c));
            let q = ai + bi - ci;
            let (pa, pb, pc) = ((q - ai).abs(), (q - bi).abs(), (q - ci).abs());
            if pa <= pb && pa <= pc {
                a
            } else if pb <= pc {
                b
            } else {
                c
            }
        }
        _ => 0,
    }
}

/// Checks (5) to (7) at the step: the input must be exactly F bytes (`scanlines`); every
/// scanline's input filter byte is undone (above 4: `scanlines`, the first such deciding) and
/// the raw scanline filtered again with the record's filter byte; the output's BLAKE3-256 must be
/// `original_hash`. `f` is the record's F (checks (2) to (4) passed). The work is done in place,
/// with two raw scanlines beside it.
pub fn decode(id: u64, r: &PngRecord, f: u64, mut data: Vec<u8>) -> Result<Vec<u8>> {
    // (5) the input length.
    if data.len() as u64 != f {
        return Err(bad_record(id, "scanlines"));
    }
    let b = bits_per_pixel(r);
    let u = usize::try_from(unit(r)).map_err(|_| bad_record(id, "scanlines"))?;
    let mut pos = 0usize;
    let mut k = 0usize;
    let mut prev: Vec<u8> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for (pw, ph) in passes(r) {
        let n = usize::try_from(scanline_bytes(pw, b)).map_err(|_| bad_record(id, "scanlines"))?;
        // The first scanline of a pass has no byte above it.
        prev.clear();
        prev.resize(n, 0);
        cur.clear();
        cur.resize(n, 0);
        for _ in 0..ph {
            // (6) the input's filter byte.
            let t_in = data[pos];
            if t_in > 4 {
                return Err(bad_record(id, "scanlines"));
            }
            let t_out = r.filters[k];
            let row = &mut data[pos + 1..pos + 1 + n];
            // Undo the input's filter, left to right, from the raw bytes already undone.
            for i in 0..n {
                let a = if i >= u { cur[i - u] } else { 0 };
                let c = if i >= u { prev[i - u] } else { 0 };
                cur[i] = row[i].wrapping_add(predict(t_in, a, prev[i], c));
            }
            // Filter the raw scanline with the record's filter byte.
            for i in 0..n {
                let a = if i >= u { cur[i - u] } else { 0 };
                let c = if i >= u { prev[i - u] } else { 0 };
                row[i] = cur[i].wrapping_sub(predict(t_out, a, prev[i], c));
            }
            data[pos] = t_out;
            std::mem::swap(&mut prev, &mut cur);
            pos += 1 + n;
            k += 1;
        }
    }
    // (7) the output's hash.
    if blake3::hash(&data).as_bytes() != &r.original_hash {
        return Err(bad_record(id, "original_hash"));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn rec(w: u32, h: u32, bd: u8, ct: u8, il: u8) -> PngRecord {
        let mut r = PngRecord {
            width: w,
            height: h,
            bit_depth: bd,
            color_type: ct,
            interlace: il,
            filters: Vec::new(),
            original_hash: [0; 32],
        };
        r.filters = vec![0; scanline_count(&r) as usize];
        r
    }

    #[test]
    fn geometry() {
        // RGB 8-bit 64 x 48: 48 lines of 1 + 192.
        let r = rec(64, 48, 8, 2, 0);
        assert_eq!((unit(&r), filtered_len(&r)), (3, Some(48 * 193)));
        // Grey 2-bit 400 x 3, Adam7: pass 3 (y0 = 4) is empty.
        let r = rec(400, 3, 2, 0, 1);
        let p = passes(&r);
        assert_eq!(
            p,
            vec![(50, 1), (50, 1), (100, 1), (200, 1), (200, 2), (400, 1)]
        );
        assert_eq!(unit(&r), 1);
        assert_eq!(filtered_len(&r), Some(14 + 14 + 26 + 51 + 2 * 51 + 101));
        // RGBA 16: u = 8; 1 x 1 Adam7 has one pass.
        let r = rec(1, 1, 16, 6, 1);
        assert_eq!(
            (unit(&r), passes(&r), filtered_len(&r)),
            (8, vec![(1, 1)], Some(9))
        );
        // A grey 1-bit line of 9 pixels has 2 bytes.
        assert_eq!(scanline_bytes(9, 1), 2);
        // The largest image does not overflow F's terms but its sum is checked.
        let r = rec(u32::MAX, 1, 16, 6, 0);
        assert_eq!(filtered_len(&r), Some(1 + u64::from(u32::MAX) * 8));
    }

    #[test]
    fn paeth_tie_order() {
        assert_eq!(predict(4, 10, 10, 10), 10);
        // pa = |b - c|, pb = |a - c|, pc = |a + b - 2c|.
        assert_eq!(predict(4, 1, 5, 5), 1); // pa 0
        assert_eq!(predict(4, 5, 1, 5), 1); // pb 0 < pa 4
        assert_eq!(predict(4, 3, 7, 10), 3); // q 0: pa 3, pb 7, pc 10
        assert_eq!(predict(4, 255, 255, 0), 255);
        assert_eq!(predict(3, 255, 255, 0), 255);
    }
}
