//! PNG row filters, filter method 0 (design `docs/notes/e2-16-design.md`, section 1): the undo
//! of the per-scanline filters of a PNG's inflated IDAT text and the re-filtering that gives the
//! text back byte for byte. Written from the PNG specification (W3C third edition, ISO/IEC
//! 15948, sections 7.2, 8.2 and 9): the geometry of a [`Layout`], Adam7's seven passes with
//! empty passes carrying no scanline and no filter byte, the five filter types with Paeth's tie
//! order, the prior row being the previous scanline of the same pass (zeros at its start),
//! sub-byte rows packed, 16-bit samples big-endian. Every length is computed with checked
//! arithmetic.
//!
//! This is the code revision 1.3's `png-filter` decoder would run, so the probe that decides
//! the gate verifies it first: `refilter(undo(text)) == text` for every eligible stream.

use std::fmt;

/// The PNG signature.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// The filter types of filter method 0.
pub const FILTER_NONE: u8 = 0;
pub const FILTER_SUB: u8 = 1;
pub const FILTER_UP: u8 = 2;
pub const FILTER_AVERAGE: u8 = 3;
pub const FILTER_PAETH: u8 = 4;

/// The names of the filter types, indexed by the filter byte.
pub const FILTER_NAMES: [&str; 5] = ["none", "sub", "up", "average", "paeth"];

/// Adam7: `(x_start, y_start, x_step, y_step)` of the seven passes, in order.
pub const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Why a PNG stream cannot be undone, or why an undo or a re-filter refused its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngError {
    /// Not the signature followed by a 13-byte IHDR chunk.
    NoHeader,
    /// A compression method other than 0.
    CompressionMethod(u8),
    /// A filter method other than 0.
    FilterMethod(u8),
    /// A bit depth and colour type the specification does not allow.
    BadPair { bit_depth: u8, color_type: u8 },
    /// A width or height of 0.
    ZeroDimension,
    /// An interlace method other than 0 or 1.
    BadInterlace(u8),
    /// A length past `u64::MAX`.
    Overflow,
    /// The input is not the length the layout requires.
    LengthMismatch { expected: u64, found: u64 },
    /// A filter byte above 4 at the given scanline.
    BadFilter { scanline: u64, filter: u8 },
    /// Not one filter per scanline.
    FilterCount { expected: u64, found: u64 },
}

impl PngError {
    /// A short name for counting by cause.
    pub fn name(&self) -> &'static str {
        match self {
            PngError::NoHeader => "no_header",
            PngError::CompressionMethod(_) => "compression_method",
            PngError::FilterMethod(_) => "filter_method",
            PngError::BadPair { .. } => "bad_pair",
            PngError::ZeroDimension => "zero_dimension",
            PngError::BadInterlace(_) => "bad_interlace",
            PngError::Overflow => "overflow",
            PngError::LengthMismatch { .. } => "length_mismatch",
            PngError::BadFilter { .. } => "bad_filter",
            PngError::FilterCount { .. } => "filter_count",
        }
    }
}

impl fmt::Display for PngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PngError::NoHeader => write!(f, "not a PNG signature followed by IHDR"),
            PngError::CompressionMethod(m) => write!(f, "compression method {m} is not 0"),
            PngError::FilterMethod(m) => write!(f, "filter method {m} is not 0"),
            PngError::BadPair {
                bit_depth,
                color_type,
            } => write!(
                f,
                "bit depth {bit_depth} is not allowed with colour type {color_type}"
            ),
            PngError::ZeroDimension => write!(f, "width and height must be at least 1"),
            PngError::BadInterlace(m) => write!(f, "interlace method {m} is not 0 or 1"),
            PngError::Overflow => write!(f, "the image's length does not fit in 64 bits"),
            PngError::LengthMismatch { expected, found } => {
                write!(f, "expected {expected} bytes, found {found}")
            }
            PngError::BadFilter { scanline, filter } => {
                write!(f, "scanline {scanline} has filter type {filter}")
            }
            PngError::FilterCount { expected, found } => {
                write!(f, "expected {expected} filter bytes, found {found}")
            }
        }
    }
}

impl std::error::Error for PngError {}

/// The IHDR fields of a PNG file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_type: u8,
    pub compression: u8,
    pub filter_method: u8,
    pub interlace: u8,
}

impl Header {
    /// The IHDR of `file`: the signature, then a chunk of length 13 named IHDR (its CRC is not
    /// checked; the walker found the IDATs by the same chunk lengths).
    pub fn parse(file: &[u8]) -> Result<Header, PngError> {
        let body = file
            .strip_prefix(&SIGNATURE)
            .and_then(|r| r.strip_prefix(&[0, 0, 0, 13]))
            .and_then(|r| r.strip_prefix(b"IHDR"))
            .and_then(|r| r.get(..13))
            .ok_or(PngError::NoHeader)?;
        let be = |i: usize| u32::from_be_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]);
        Ok(Header {
            width: be(0),
            height: be(4),
            bit_depth: body[8],
            color_type: body[9],
            compression: body[10],
            filter_method: body[11],
            interlace: body[12],
        })
    }

    /// The layout of the image, when its methods are the ones filter method 0 describes.
    pub fn layout(&self) -> Result<Layout, PngError> {
        if self.compression != 0 {
            return Err(PngError::CompressionMethod(self.compression));
        }
        if self.filter_method != 0 {
            return Err(PngError::FilterMethod(self.filter_method));
        }
        Layout::new(
            self.width,
            self.height,
            self.bit_depth,
            self.color_type,
            self.interlace,
        )
    }
}

/// One pass of the image: the whole image, or one of Adam7's seven (possibly empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// Pixels per scanline.
    pub width: u32,
    /// Scanlines; an empty pass (width or height 0) has none in the stream.
    pub height: u32,
    /// Bytes per scanline without the filter byte.
    pub row_bytes: u64,
}

impl Pass {
    /// Scanlines in the stream: `height` unless the pass is empty.
    pub fn scanlines(&self) -> u64 {
        if self.width == 0 {
            0
        } else {
            u64::from(self.height)
        }
    }
}

/// The geometry of an image: a legal (bit depth, colour type) pair, non-zero dimensions and an
/// interlace method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlace: u8,
}

/// Whether `bit_depth` is allowed with `color_type` (PNG specification, table 11.1).
pub fn pair_allowed(bit_depth: u8, color_type: u8) -> bool {
    match color_type {
        0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(bit_depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(bit_depth, 8 | 16),
        _ => false,
    }
}

/// Every legal (bit depth, colour type) pair, 15 in all.
pub const PAIRS: [(u8, u8); 15] = [
    (1, 0),
    (2, 0),
    (4, 0),
    (8, 0),
    (16, 0),
    (8, 2),
    (16, 2),
    (1, 3),
    (2, 3),
    (4, 3),
    (8, 3),
    (8, 4),
    (16, 4),
    (8, 6),
    (16, 6),
];

/// The scanlines undone: the raw rows (pass by pass, without filter bytes) and one filter byte
/// per scanline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    pub raw: Vec<u8>,
    pub filters: Vec<u8>,
}

impl Layout {
    /// A layout, checked.
    pub fn new(
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
        interlace: u8,
    ) -> Result<Layout, PngError> {
        if !pair_allowed(bit_depth, color_type) {
            return Err(PngError::BadPair {
                bit_depth,
                color_type,
            });
        }
        if width == 0 || height == 0 {
            return Err(PngError::ZeroDimension);
        }
        if interlace > 1 {
            return Err(PngError::BadInterlace(interlace));
        }
        Ok(Layout {
            width,
            height,
            bit_depth,
            color_type,
            interlace,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn bit_depth(&self) -> u8 {
        self.bit_depth
    }

    pub fn color_type(&self) -> u8 {
        self.color_type
    }

    pub fn interlace(&self) -> u8 {
        self.interlace
    }

    /// Samples per pixel: 1, 3, 1, 2, 4 for colour types 0, 2, 3, 4, 6.
    pub fn channels(&self) -> u8 {
        match self.color_type {
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 1,
        }
    }

    /// Bits per pixel: channels times bit depth.
    pub fn bits_per_pixel(&self) -> u32 {
        u32::from(self.channels()) * u32::from(self.bit_depth)
    }

    /// The filter unit (`bpp` of the specification): the bytes per complete pixel, at least 1.
    pub fn filter_unit(&self) -> usize {
        (self.bits_per_pixel() / 8).max(1) as usize
    }

    /// Bytes of a scanline of `width` pixels, without the filter byte.
    pub fn row_bytes(&self, width: u32) -> u64 {
        (u64::from(width) * u64::from(self.bits_per_pixel())).div_ceil(8)
    }

    /// The passes: one for the whole image, or Adam7's seven.
    pub fn passes(&self) -> Vec<Pass> {
        if self.interlace == 0 {
            return vec![Pass {
                width: self.width,
                height: self.height,
                row_bytes: self.row_bytes(self.width),
            }];
        }
        ADAM7
            .iter()
            .map(|&(x0, y0, dx, dy)| {
                let extent = |n: u32, start: u32, step: u32| {
                    if n > start {
                        (n - start).div_ceil(step)
                    } else {
                        0
                    }
                };
                let width = extent(self.width, x0, dx);
                let height = extent(self.height, y0, dy);
                Pass {
                    width,
                    height,
                    row_bytes: self.row_bytes(width),
                }
            })
            .collect()
    }

    /// Scanlines in the stream (filter bytes).
    pub fn scanlines(&self) -> u64 {
        self.passes().iter().map(Pass::scanlines).sum()
    }

    /// Bytes of the raw rows: the sum over the passes of scanlines times row bytes.
    pub fn raw_len(&self) -> Option<u64> {
        self.sum_rows(0)
    }

    /// Bytes of the filtered rows: the raw rows plus one filter byte per scanline.
    pub fn filtered_len(&self) -> Option<u64> {
        self.sum_rows(1)
    }

    fn sum_rows(&self, extra: u64) -> Option<u64> {
        self.passes().iter().try_fold(0u64, |acc, p| {
            let per_row = p.row_bytes.checked_add(extra)?;
            acc.checked_add(p.scanlines().checked_mul(per_row)?)
        })
    }

    /// Undo the filters of `filtered` (the inflated IDAT text): the raw rows and the filter
    /// bytes. The input must have exactly the filtered length and filter bytes at most 4.
    pub fn undo(&self, filtered: &[u8]) -> Result<Undone, PngError> {
        let expected = self.filtered_len().ok_or(PngError::Overflow)?;
        if expected != filtered.len() as u64 {
            return Err(PngError::LengthMismatch {
                expected,
                found: filtered.len() as u64,
            });
        }
        let raw_len = self.raw_len().ok_or(PngError::Overflow)?;
        let raw_len = usize::try_from(raw_len).map_err(|_| PngError::Overflow)?;
        let mut raw = vec![0u8; raw_len];
        let mut filters = Vec::with_capacity(usize::try_from(self.scanlines()).unwrap_or(0));
        let bpp = self.filter_unit();
        let (mut src, mut dst) = (0usize, 0usize);
        for pass in self.passes() {
            if pass.scanlines() == 0 {
                continue;
            }
            // The filtered length was checked, so every row of the pass fits in a usize.
            let rb = pass.row_bytes as usize;
            let zeros = vec![0u8; rb];
            for y in 0..pass.scanlines() {
                let filter = filtered[src];
                if filter > FILTER_PAETH {
                    return Err(PngError::BadFilter {
                        scanline: filters.len() as u64,
                        filter,
                    });
                }
                filters.push(filter);
                let (before, rest) = raw.split_at_mut(dst);
                let row = &mut rest[..rb];
                row.copy_from_slice(&filtered[src + 1..src + 1 + rb]);
                let prior: &[u8] = if y == 0 { &zeros } else { &before[dst - rb..] };
                unfilter_row(filter, bpp, prior, row);
                src += 1 + rb;
                dst += rb;
            }
        }
        Ok(Undone { raw, filters })
    }

    /// Re-filter `raw` (the raw rows, pass by pass) with one filter byte per scanline: the
    /// filtered rows, each scanline's filter byte then its filtered bytes.
    pub fn refilter(&self, raw: &[u8], filters: &[u8]) -> Result<Vec<u8>, PngError> {
        let expected = self.raw_len().ok_or(PngError::Overflow)?;
        if expected != raw.len() as u64 {
            return Err(PngError::LengthMismatch {
                expected,
                found: raw.len() as u64,
            });
        }
        let scanlines = self.scanlines();
        if scanlines != filters.len() as u64 {
            return Err(PngError::FilterCount {
                expected: scanlines,
                found: filters.len() as u64,
            });
        }
        if let Some((i, &f)) = filters.iter().enumerate().find(|(_, f)| **f > FILTER_PAETH) {
            return Err(PngError::BadFilter {
                scanline: i as u64,
                filter: f,
            });
        }
        let out_len = self.filtered_len().ok_or(PngError::Overflow)?;
        let out_len = usize::try_from(out_len).map_err(|_| PngError::Overflow)?;
        let mut out = vec![0u8; out_len];
        let bpp = self.filter_unit();
        let (mut src, mut dst, mut line) = (0usize, 0usize, 0usize);
        for pass in self.passes() {
            if pass.scanlines() == 0 {
                continue;
            }
            let rb = pass.row_bytes as usize;
            let zeros = vec![0u8; rb];
            for y in 0..pass.scanlines() {
                let filter = filters[line];
                let row = &raw[src..src + rb];
                let prior = if y == 0 {
                    &zeros[..]
                } else {
                    &raw[src - rb..src]
                };
                out[dst] = filter;
                filter_row(filter, bpp, prior, row, &mut out[dst + 1..dst + 1 + rb]);
                src += rb;
                dst += 1 + rb;
                line += 1;
            }
        }
        Ok(out)
    }

    /// The raw rows with every scanline given filter `filter`.
    pub fn refilter_uniform(&self, raw: &[u8], filter: u8) -> Result<Vec<u8>, PngError> {
        let n = usize::try_from(self.scanlines()).map_err(|_| PngError::Overflow)?;
        self.refilter(raw, &vec![filter; n])
    }

    /// The raw rows as one raster (`height` rows of `row_bytes(width)`, rows packed): the raw
    /// rows themselves when not interlaced, else Adam7's passes placed into the image.
    pub fn deinterlace(&self, raw: &[u8]) -> Result<Vec<u8>, PngError> {
        let expected = self.raw_len().ok_or(PngError::Overflow)?;
        if expected != raw.len() as u64 {
            return Err(PngError::LengthMismatch {
                expected,
                found: raw.len() as u64,
            });
        }
        if self.interlace == 0 {
            return Ok(raw.to_vec());
        }
        let stride = self.row_bytes(self.width);
        let len = stride
            .checked_mul(u64::from(self.height))
            .ok_or(PngError::Overflow)?;
        let len = usize::try_from(len).map_err(|_| PngError::Overflow)?;
        let stride = stride as usize;
        let mut out = vec![0u8; len];
        let bits = self.bits_per_pixel() as usize;
        let mut src = 0usize;
        for (pass, &(x0, y0, dx, dy)) in self.passes().iter().zip(ADAM7.iter()) {
            let rb = pass.row_bytes as usize;
            for y in 0..pass.scanlines() {
                let row = &raw[src..src + rb];
                let image_row = (y0 as usize + y as usize * dy as usize) * stride;
                let dst = &mut out[image_row..image_row + stride];
                for x in 0..pass.width as usize {
                    let ix = x0 as usize + x * dx as usize;
                    if bits >= 8 {
                        let n = bits / 8;
                        dst[ix * n..ix * n + n].copy_from_slice(&row[x * n..x * n + n]);
                    } else {
                        let v = get_bits(row, x, bits);
                        put_bits(dst, ix, bits, v);
                    }
                }
                src += rb;
            }
        }
        Ok(out)
    }
}

/// The pixel `x` of a packed row of `bits`-bit pixels (`bits` below 8).
fn get_bits(row: &[u8], x: usize, bits: usize) -> u8 {
    let bit = x * bits;
    let shift = 8 - bits - (bit % 8);
    (row[bit / 8] >> shift) & ((1u8 << bits) - 1)
}

/// Set the pixel `x` of a packed row (its bits must be zero beforehand).
fn put_bits(row: &mut [u8], x: usize, bits: usize, v: u8) {
    let bit = x * bits;
    let shift = 8 - bits - (bit % 8);
    row[bit / 8] |= v << shift;
}

/// The Paeth predictor (PNG specification, section 9.4): the neighbour nearest to `a + b - c`,
/// ties broken left, then up.
pub fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i16::from(a), i16::from(b), i16::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// The left neighbour of `x` in a row, zero before the first pixel.
fn left(row: &[u8], x: usize, bpp: usize) -> u8 {
    if x >= bpp {
        row[x - bpp]
    } else {
        0
    }
}

/// Undo one scanline's filter in place; `prior` is the previous raw row of the pass (zeros for
/// the first) and `row` the filtered bytes, which become the raw bytes.
fn unfilter_row(filter: u8, bpp: usize, prior: &[u8], row: &mut [u8]) {
    match filter {
        FILTER_NONE => {}
        FILTER_SUB => {
            for x in bpp..row.len() {
                row[x] = row[x].wrapping_add(row[x - bpp]);
            }
        }
        FILTER_UP => {
            for (r, p) in row.iter_mut().zip(prior) {
                *r = r.wrapping_add(*p);
            }
        }
        FILTER_AVERAGE => {
            for x in 0..row.len() {
                let a = u16::from(left(row, x, bpp));
                let b = u16::from(prior[x]);
                row[x] = row[x].wrapping_add(((a + b) / 2) as u8);
            }
        }
        _ => {
            for x in 0..row.len() {
                let a = left(row, x, bpp);
                let c = left(prior, x, bpp);
                row[x] = row[x].wrapping_add(paeth(a, prior[x], c));
            }
        }
    }
}

/// Apply one scanline's filter: `raw` the raw row, `prior` the previous raw row of the pass
/// (zeros for the first), `out` the filtered bytes.
fn filter_row(filter: u8, bpp: usize, prior: &[u8], raw: &[u8], out: &mut [u8]) {
    match filter {
        FILTER_NONE => out.copy_from_slice(raw),
        FILTER_SUB => {
            for x in 0..raw.len() {
                out[x] = raw[x].wrapping_sub(left(raw, x, bpp));
            }
        }
        FILTER_UP => {
            for x in 0..raw.len() {
                out[x] = raw[x].wrapping_sub(prior[x]);
            }
        }
        FILTER_AVERAGE => {
            for x in 0..raw.len() {
                let a = u16::from(left(raw, x, bpp));
                let b = u16::from(prior[x]);
                out[x] = raw[x].wrapping_sub(((a + b) / 2) as u8);
            }
        }
        _ => {
            for x in 0..raw.len() {
                let a = left(raw, x, bpp);
                let c = left(prior, x, bpp);
                out[x] = raw[x].wrapping_sub(paeth(a, prior[x], c));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64, deterministic.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn byte(&mut self) -> u8 {
            (self.next() >> 56) as u8
        }

        fn filter(&mut self) -> u8 {
            (self.next() % 5) as u8
        }

        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.byte()).collect()
        }
    }

    fn layout(w: u32, h: u32, depth: u8, ct: u8, interlace: u8) -> Layout {
        Layout::new(w, h, depth, ct, interlace).expect("layout")
    }

    #[test]
    fn every_pair_and_only_those_are_allowed() {
        let mut allowed = Vec::new();
        for ct in 0..=7u8 {
            for depth in [0u8, 1, 2, 3, 4, 8, 16, 32] {
                if pair_allowed(depth, ct) {
                    allowed.push((depth, ct));
                }
            }
        }
        let mut want = PAIRS.to_vec();
        want.sort_unstable();
        allowed.sort_unstable();
        assert_eq!(allowed, want);
        assert_eq!(
            Layout::new(1, 1, 16, 3, 0),
            Err(PngError::BadPair {
                bit_depth: 16,
                color_type: 3
            })
        );
        assert_eq!(Layout::new(0, 1, 8, 0, 0), Err(PngError::ZeroDimension));
        assert_eq!(Layout::new(1, 0, 8, 0, 0), Err(PngError::ZeroDimension));
        assert_eq!(Layout::new(1, 1, 8, 0, 2), Err(PngError::BadInterlace(2)));
    }

    #[test]
    fn undo_and_refilter_are_inverses_both_ways_over_every_layout() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut layouts = 0u64;
        for (depth, ct) in PAIRS {
            for interlace in 0..=1u8 {
                for w in 1..=17u32 {
                    for h in 1..=9u32 {
                        let l = layout(w, h, depth, ct, interlace);
                        layouts += 1;
                        let raw_len = l.raw_len().expect("raw") as usize;
                        let n = l.scanlines() as usize;
                        assert_eq!(
                            l.filtered_len().expect("filtered") as usize,
                            raw_len + n,
                            "{l:?}"
                        );
                        // Raw rows and filters first: refilter, then undo.
                        let raw = rng.bytes(raw_len);
                        let filters: Vec<u8> = (0..n).map(|_| rng.filter()).collect();
                        let filtered = l.refilter(&raw, &filters).expect("refilter");
                        let u = l.undo(&filtered).expect("undo");
                        assert_eq!((u.raw, u.filters), (raw, filters), "{l:?}");
                        // A filtered text first: undo, then refilter.
                        let mut text = rng.bytes(raw_len + n);
                        let mut at = 0;
                        for p in l.passes() {
                            for _ in 0..p.scanlines() {
                                text[at] = rng.filter();
                                at += 1 + p.row_bytes as usize;
                            }
                        }
                        let u = l.undo(&text).expect("undo");
                        assert_eq!(l.refilter(&u.raw, &u.filters).expect("refilter"), text);
                        // The de-interlaced raster has the image's pixel count.
                        let raster = l.deinterlace(&u.raw).expect("raster");
                        assert_eq!(raster.len() as u64, l.row_bytes(w) * u64::from(h));
                    }
                }
            }
        }
        assert_eq!(layouts, 15 * 2 * 17 * 9);
    }

    #[test]
    fn paeth_breaks_ties_left_then_up() {
        // pa == pb: left.
        assert_eq!(paeth(1, 1, 0), 1);
        // pa == pc: left.
        assert_eq!(paeth(3, 0, 1), 3);
        // pb == pc below pa: up.
        assert_eq!(paeth(4, 1, 3), 1);
        // pc alone smallest: upper left.
        assert_eq!(paeth(2, 0, 1), 1);
        assert_eq!(paeth(0, 2, 1), 1);
        // All equal: left.
        assert_eq!(paeth(7, 7, 7), 7);
        // Extremes stay in range.
        assert_eq!(paeth(255, 0, 255), 0);
        assert_eq!(paeth(0, 255, 255), 0);
    }

    #[test]
    fn a_known_grey_image_filters_as_the_specification_says() {
        let l = layout(3, 2, 8, 0, 0);
        let raw = [10, 20, 30, 5, 25, 35];
        let filtered = l
            .refilter(&raw, &[FILTER_SUB, FILTER_PAETH])
            .expect("refilter");
        assert_eq!(filtered, [1, 10, 10, 10, 4, 251, 5, 5]);
        let u = l.undo(&filtered).expect("undo");
        assert_eq!(u.raw, raw);
        assert_eq!(u.filters, [1, 4]);
    }

    #[test]
    fn average_rounds_down_and_the_first_row_averages_with_zeros() {
        let l = layout(2, 2, 8, 0, 0);
        let raw = [255, 1, 255, 3];
        let filtered = l
            .refilter(&raw, &[FILTER_AVERAGE, FILTER_AVERAGE])
            .expect("refilter");
        // Row 1: left 0, prior 0: 255 - 0; then left 255, prior 0: floor(255 / 2) = 127.
        // Row 2: left 0, prior 255: 127; then left 255, prior 1: 128.
        assert_eq!(
            filtered,
            [
                3,
                255,
                1u8.wrapping_sub(127),
                3,
                255u8.wrapping_sub(127),
                3u8.wrapping_sub(128)
            ]
        );
        assert_eq!(l.undo(&filtered).expect("undo").raw, raw);
    }

    #[test]
    fn the_filter_unit_is_the_whole_pixel_and_at_least_one_byte() {
        assert_eq!(layout(1, 1, 1, 0, 0).filter_unit(), 1);
        assert_eq!(layout(1, 1, 4, 3, 0).filter_unit(), 1);
        assert_eq!(layout(1, 1, 8, 0, 0).filter_unit(), 1);
        assert_eq!(layout(1, 1, 16, 0, 0).filter_unit(), 2);
        assert_eq!(layout(1, 1, 8, 2, 0).filter_unit(), 3);
        assert_eq!(layout(1, 1, 8, 4, 0).filter_unit(), 2);
        assert_eq!(layout(1, 1, 16, 6, 0).filter_unit(), 8);
        // Sub on 16-bit grey subtracts the sample two bytes left, not the previous byte.
        let l = layout(2, 1, 16, 0, 0);
        let raw = [0x12, 0x34, 0x56, 0x78];
        let f = l.refilter(&raw, &[FILTER_SUB]).expect("refilter");
        assert_eq!(f, [1, 0x12, 0x34, 0x56 - 0x12, 0x78 - 0x34]);
        // Sub on RGB8 uses the pixel three bytes left.
        let l = layout(2, 1, 8, 2, 0);
        let raw = [1, 2, 3, 11, 22, 33];
        let f = l.refilter(&raw, &[FILTER_SUB]).expect("refilter");
        assert_eq!(f, [1, 1, 2, 3, 10, 20, 30]);
        // Sub on a packed 2-bit row uses the previous byte (four pixels left).
        let l = layout(8, 1, 2, 3, 0);
        let raw = [0b1110_0100, 0b0001_1011];
        let f = l.refilter(&raw, &[FILTER_SUB]).expect("refilter");
        assert_eq!(f, [1, 0b1110_0100, 0b0001_1011u8.wrapping_sub(0b1110_0100)]);
    }

    #[test]
    fn sub_byte_rows_are_packed_and_the_last_byte_is_padded() {
        let l = layout(17, 3, 1, 0, 0);
        assert_eq!(l.row_bytes(17), 3);
        assert_eq!(l.filtered_len(), Some(3 * 4));
        assert_eq!(l.raw_len(), Some(9));
        let l = layout(3, 1, 4, 3, 0);
        assert_eq!(l.row_bytes(3), 2);
        let l = layout(5, 1, 2, 0, 0);
        assert_eq!(l.row_bytes(5), 2);
        let l = layout(9, 1, 1, 3, 0);
        assert_eq!(l.row_bytes(9), 2);
    }

    #[test]
    fn adam7_passes_follow_the_table_and_empty_passes_have_no_scanline() {
        // 1 x 1: only the first pass has a pixel.
        let l = layout(1, 1, 8, 0, 1);
        let p = l.passes();
        assert_eq!(p.len(), 7);
        assert_eq!((p[0].width, p[0].height), (1, 1));
        assert!(p[1..].iter().all(|p| p.scanlines() == 0));
        assert_eq!(l.scanlines(), 1);
        assert_eq!(l.filtered_len(), Some(2));
        assert_eq!(l.raw_len(), Some(1));
        // A pass with width 0 and height above 0 has no scanline and no filter byte.
        let l = layout(1, 8, 8, 0, 1);
        let p = l.passes();
        assert_eq!((p[1].width, p[1].height, p[1].scanlines()), (0, 1, 0));
        assert_eq!((p[2].width, p[2].height), (1, 1));
        assert_eq!(l.scanlines(), 8); // passes 1, 3, 5 and 7: 1 + 1 + 2 + 4
                                      // 8 x 8: every pass present.
        let l = layout(8, 8, 8, 0, 1);
        let dims: Vec<(u32, u32)> = l.passes().iter().map(|p| (p.width, p.height)).collect();
        assert_eq!(
            dims,
            [(1, 1), (1, 1), (2, 1), (2, 2), (4, 2), (4, 4), (8, 4)]
        );
        assert_eq!(l.scanlines(), 15);
        assert_eq!(l.raw_len(), Some(64));
        assert_eq!(l.filtered_len(), Some(64 + 15));
        // 5 x 1.
        let l = layout(5, 1, 8, 0, 1);
        let dims: Vec<(u32, u32)> = l.passes().iter().map(|p| (p.width, p.height)).collect();
        assert_eq!(
            dims,
            [(1, 1), (1, 1), (2, 0), (1, 1), (3, 0), (2, 1), (5, 0)]
        );
        assert_eq!(l.scanlines(), 4);
        // The undo reads one filter byte per present scanline only; pass 6's row is Paeth
        // over a zero prior, so its second pixel adds the first.
        let u = l.undo(&[0, 7, 1, 1, 2, 2, 4, 3, 3]).expect("undo");
        assert_eq!(u.filters, [0, 1, 2, 4]);
        assert_eq!(u.raw, [7, 1, 2, 3, 6]);
    }

    #[test]
    fn deinterlace_places_the_passes_into_the_image() {
        // 3 x 3 grey 8: pixel value = 10 * y + x. Pass order: p1 (0,0); p2 none; p3 none;
        // p4 (2,0); p5 (0,2),(2,2); p6 (1,0),(1,2); p7 row 1: (0,1),(1,1),(2,1).
        let l = layout(3, 3, 8, 0, 1);
        let dims: Vec<(u32, u32)> = l.passes().iter().map(|p| (p.width, p.height)).collect();
        assert_eq!(
            dims,
            [(1, 1), (0, 1), (1, 0), (1, 1), (2, 1), (1, 2), (3, 1)]
        );
        let raw = [0, 2, 20, 22, 1, 21, 10, 11, 12];
        assert_eq!(
            l.deinterlace(&raw).expect("raster"),
            [0, 1, 2, 10, 11, 12, 20, 21, 22]
        );
        // The same image at 4 bits (pixel value = x), nibbles packed high first:
        // p1 [0]; p4 [2]; p5 [0, 2]; p6 [1], [1]; p7 [0, 1, 2].
        let l = layout(3, 3, 4, 0, 1);
        let raw = [0x00, 0x20, 0x02, 0x10, 0x10, 0x01, 0x20];
        assert_eq!(
            l.deinterlace(&raw).expect("raster"),
            [0x01, 0x20, 0x01, 0x20, 0x01, 0x20]
        );
        // Not interlaced: the rows themselves.
        let l = layout(2, 2, 8, 0, 0);
        assert_eq!(l.deinterlace(&[1, 2, 3, 4]).expect("raster"), [1, 2, 3, 4]);
    }

    #[test]
    fn lengths_past_u64_are_overflow_and_the_largest_dimensions_are_checked() {
        let l = layout(u32::MAX, u32::MAX, 16, 6, 0);
        assert_eq!(l.row_bytes(u32::MAX), u64::from(u32::MAX) * 8);
        assert_eq!(l.raw_len(), None);
        assert_eq!(l.filtered_len(), None);
        assert_eq!(l.undo(&[]), Err(PngError::Overflow));
        assert_eq!(l.refilter(&[], &[]), Err(PngError::Overflow));
        assert_eq!(l.deinterlace(&[]), Err(PngError::Overflow));
        let l = layout(u32::MAX, u32::MAX, 16, 6, 1);
        assert_eq!(l.filtered_len(), None);
        // A 1-bit image of the largest dimensions: the rows fit, the image does too.
        let l = layout(u32::MAX, u32::MAX, 1, 0, 0);
        assert_eq!(l.row_bytes(u32::MAX), (u64::from(u32::MAX)).div_ceil(8));
        assert!(l.filtered_len().is_some());
        let l = layout(u32::MAX, 1, 16, 6, 1);
        assert!(l.filtered_len().is_some());
        assert_eq!(l.scanlines(), 4);
    }

    #[test]
    fn bad_inputs_are_refused_by_cause() {
        let l = layout(2, 2, 8, 0, 0);
        assert_eq!(
            l.undo(&[0, 1, 2]),
            Err(PngError::LengthMismatch {
                expected: 6,
                found: 3
            })
        );
        assert_eq!(
            l.undo(&[0, 1, 2, 5, 3, 4]),
            Err(PngError::BadFilter {
                scanline: 1,
                filter: 5
            })
        );
        assert_eq!(
            l.refilter(&[1, 2, 3], &[0, 0]),
            Err(PngError::LengthMismatch {
                expected: 4,
                found: 3
            })
        );
        assert_eq!(
            l.refilter(&[1, 2, 3, 4], &[0]),
            Err(PngError::FilterCount {
                expected: 2,
                found: 1
            })
        );
        assert_eq!(
            l.refilter(&[1, 2, 3, 4], &[0, 9]),
            Err(PngError::BadFilter {
                scanline: 1,
                filter: 9
            })
        );
        assert_eq!(
            l.deinterlace(&[1]),
            Err(PngError::LengthMismatch {
                expected: 4,
                found: 1
            })
        );
        for e in [
            PngError::NoHeader,
            PngError::CompressionMethod(1),
            PngError::FilterMethod(1),
            PngError::BadPair {
                bit_depth: 3,
                color_type: 0,
            },
            PngError::ZeroDimension,
            PngError::BadInterlace(2),
            PngError::Overflow,
            PngError::LengthMismatch {
                expected: 1,
                found: 2,
            },
            PngError::BadFilter {
                scanline: 0,
                filter: 5,
            },
            PngError::FilterCount {
                expected: 1,
                found: 2,
            },
        ] {
            assert!(!e.name().is_empty() && !e.to_string().is_empty());
        }
    }

    #[test]
    fn the_header_is_the_signature_then_ihdr() {
        let mut png = SIGNATURE.to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 1]);
        png.extend_from_slice(&[0; 4]);
        let h = Header::parse(&png).expect("header");
        assert_eq!(
            h,
            Header {
                width: 640,
                height: 480,
                bit_depth: 8,
                color_type: 6,
                compression: 0,
                filter_method: 0,
                interlace: 1,
            }
        );
        let l = h.layout().expect("layout");
        assert_eq!((l.width(), l.height(), l.bit_depth()), (640, 480, 8));
        assert_eq!((l.color_type(), l.interlace(), l.channels()), (6, 1, 4));
        assert_eq!(Header::parse(&png[..20]), Err(PngError::NoHeader));
        assert_eq!(Header::parse(b"not a png"), Err(PngError::NoHeader));
        let mut other = png.clone();
        other[12..16].copy_from_slice(b"IDAT");
        assert_eq!(Header::parse(&other), Err(PngError::NoHeader));
        let mut c = h;
        c.compression = 1;
        assert_eq!(c.layout(), Err(PngError::CompressionMethod(1)));
        let mut f = h;
        f.filter_method = 2;
        assert_eq!(f.layout(), Err(PngError::FilterMethod(2)));
        let mut z = h;
        z.width = 0;
        assert_eq!(z.layout(), Err(PngError::ZeroDimension));
    }

    #[test]
    fn filter_names_follow_the_filter_bytes() {
        assert_eq!(FILTER_NAMES[FILTER_NONE as usize], "none");
        assert_eq!(FILTER_NAMES[FILTER_PAETH as usize], "paeth");
        let l = layout(2, 3, 8, 0, 0);
        let raw = [1, 2, 3, 4, 5, 6];
        let a = l.refilter_uniform(&raw, FILTER_UP).expect("uniform");
        let b = l.refilter(&raw, &[2, 2, 2]).expect("refilter");
        assert_eq!(a, b);
    }
}
