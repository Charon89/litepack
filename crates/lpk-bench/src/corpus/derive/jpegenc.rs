//! A small JPEG encoder for corpus derivations: baseline or progressive, 4:2:0, JFIF.
//!
//! Why in-tree: the pure-Rust encoders available either lack progressive mode (`image`) or carry
//! the IJG licence (`jpeg-encoder`), which `deny.toml` does not allow. The encoder is a pure
//! function of its input: colour conversion, downsampling, the DCT and quantisation use integer
//! arithmetic only (the DCT matrix is a table of constants), so the bytes are the same on every
//! platform and CPU.
//!
//! Pipeline: RGB to YCbCr (fixed point), 2x2 box downsampling of the chroma planes, an 8x8 DCT
//! as two fixed-point matrix products, quantisation with the Annex K tables scaled by the usual
//! quality formula, and Huffman coding (the Annex K.3 standard tables for baseline, as libjpeg writes by default; tables optimised per scan, two passes, for progressive). The
//! progressive script uses spectral selection only (DC; Y 1-5; Cb 1-63; Cr 1-63; Y 6-63), no
//! successive approximation.

use anyhow::{bail, Result};

/// Natural index of the coefficient at zigzag position `k`.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const LUMA_Q: [u32; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

const CHROMA_Q: [u32; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// `round(2^14 * c(u) / 2 * cos((2n+1) u pi / 16))` with `c(0) = 1/sqrt(2)`, else 1: row `u`,
/// column `n`. Generated once with f64 and pasted so that no run depends on a platform's libm.
const DCT: [[i32; 8]; 8] = [
    [5793, 5793, 5793, 5793, 5793, 5793, 5793, 5793],
    [8035, 6811, 4551, 1598, -1598, -4551, -6811, -8035],
    [7568, 3135, -3135, -7568, -7568, -3135, 3135, 7568],
    [6811, -1598, -8035, -4551, 4551, 8035, 1598, -6811],
    [5793, -5793, -5793, 5793, 5793, -5793, -5793, 5793],
    [4551, -8035, 1598, 6811, -6811, -1598, 8035, -4551],
    [3135, -7568, 7568, -3135, -3135, 7568, -7568, 3135],
    [1598, -4551, 6811, -8035, 8035, -6811, 4551, -1598],
];

fn scaled_table(base: &[u32; 64], quality: u8) -> [u32; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let mut t = [0u32; 64];
    for (o, b) in t.iter_mut().zip(base) {
        *o = ((b * scale + 50) / 100).clamp(1, 255);
    }
    t
}

/// Forward DCT of one level-shifted block, then quantisation (round half away from zero).
/// Output in zigzag order.
fn dct_quant(block: &[i32; 64], q: &[u32; 64]) -> [i16; 64] {
    let mut tmp = [0i32; 64];
    for r in 0..8 {
        for u in 0..8 {
            let mut s = 0i32;
            for n in 0..8 {
                s += DCT[u][n] * block[r * 8 + n];
            }
            tmp[r * 8 + u] = (s + 128) >> 8;
        }
    }
    let mut out = [0i16; 64];
    for u in 0..8 {
        for (v, row) in DCT.iter().enumerate() {
            let mut s = 0i64;
            for n in 0..8 {
                s += i64::from(row[n]) * i64::from(tmp[n * 8 + u]);
            }
            let natural = v * 8 + u;
            let d = i64::from(q[natural]) << 20;
            let c = if s >= 0 {
                (s + d / 2) / d
            } else {
                -((-s + d / 2) / d)
            };
            if let Some(k) = ZIGZAG.iter().position(|z| *z == natural) {
                out[k] = c as i16;
            }
        }
    }
    out
}

struct Component {
    id: u8,
    /// Horizontal and vertical sampling factors.
    hv: u8,
    /// Quantisation table selector.
    tq: u8,
    /// Blocks per row / column in the MCU-padded grid.
    bw: usize,
    bh: usize,
    /// Blocks per row / column that cover the real samples (non-interleaved scans).
    real_bw: usize,
    real_bh: usize,
    blocks: Vec<[i16; 64]>,
}

#[allow(clippy::too_many_arguments)]
fn component(
    plane: &[u8],
    pw: usize,
    ph: usize,
    real_w: usize,
    real_h: usize,
    id: u8,
    hv: u8,
    tq: u8,
    qt: &[u32; 64],
) -> Component {
    let (bw, bh) = (pw / 8, ph / 8);
    let mut blocks = Vec::with_capacity(bw * bh);
    for by in 0..bh {
        for bx in 0..bw {
            let mut b = [0i32; 64];
            for y in 0..8 {
                for x in 0..8 {
                    b[y * 8 + x] = i32::from(plane[(by * 8 + y) * pw + bx * 8 + x]) - 128;
                }
            }
            blocks.push(dct_quant(&b, qt));
        }
    }
    Component {
        id,
        hv,
        tq,
        bw,
        bh,
        real_bw: real_w.div_ceil(8),
        real_bh: real_h.div_ceil(8),
        blocks,
    }
}

fn components(rgb: &[u8], w: usize, h: usize, quality: u8) -> [Component; 3] {
    let (mw, mh) = (w.div_ceil(16) * 16, h.div_ceil(16) * 16);
    // YCbCr of the real image.
    let mut ycc = vec![[0u8; 3]; w * h];
    for (px, o) in rgb.as_chunks::<3>().0.iter().zip(ycc.iter_mut()) {
        let (r, g, b) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
        let y = (19595 * r + 38470 * g + 7471 * b + 32768) >> 16;
        let cb = ((-11059 * r - 21709 * g + 32768 * b + (128 << 16) + 32767) >> 16).clamp(0, 255);
        let cr = ((32768 * r - 27439 * g - 5329 * b + (128 << 16) + 32767) >> 16).clamp(0, 255);
        *o = [y.clamp(0, 255) as u8, cb as u8, cr as u8];
    }
    let at = |x: usize, y: usize, c: usize| ycc[y.min(h - 1) * w + x.min(w - 1)][c];
    let mut yp = vec![0u8; mw * mh];
    for y in 0..mh {
        for x in 0..mw {
            yp[y * mw + x] = at(x, y, 0);
        }
    }
    let (cw, ch) = (mw / 2, mh / 2);
    let mut cbp = vec![0u8; cw * ch];
    let mut crp = vec![0u8; cw * ch];
    for y in 0..ch {
        for x in 0..cw {
            let mut sb = 2u32;
            let mut sr = 2u32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                sb += u32::from(at(2 * x + dx, 2 * y + dy, 1));
                sr += u32::from(at(2 * x + dx, 2 * y + dy, 2));
            }
            cbp[y * cw + x] = (sb >> 2) as u8;
            crp[y * cw + x] = (sr >> 2) as u8;
        }
    }
    let lq = scaled_table(&LUMA_Q, quality);
    let cq = scaled_table(&CHROMA_Q, quality);
    let (rcw, rch) = (w.div_ceil(2), h.div_ceil(2));
    [
        component(&yp, mw, mh, w, h, 1, 0x22, 0, &lq),
        component(&cbp, cw, ch, rcw, rch, 2, 0x11, 1, &cq),
        component(&crp, cw, ch, rcw, rch, 3, 0x11, 1, &cq),
    ]
}

/// Bit length of `v` (0 for 0).
fn bit_len(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// The `size` low bits that follow a Huffman symbol for the signed value `v`.
fn value_bits(v: i32, size: u32) -> u32 {
    let x = if v < 0 { v - 1 } else { v };
    (x as u32) & ((1u32 << size) - 1)
}

#[derive(Clone)]
struct HuffTable {
    bits: [u8; 17],
    vals: Vec<u8>,
    code: [u16; 256],
    len: [u8; 256],
}

/// Optimal length-limited (16) Huffman table for the symbol counts (JPEG Annex K.2).
fn build_table(counts: &[u64; 257]) -> Result<HuffTable> {
    let mut freq = *counts;
    freq[256] = 1;
    if freq[..256].iter().all(|f| *f == 0) {
        freq[0] = 1;
    }
    let mut codesize = [0usize; 257];
    let mut others = [-1i32; 257];
    loop {
        let (mut c1, mut v) = (-1i32, u64::MAX);
        for (i, f) in freq.iter().enumerate() {
            if *f != 0 && *f <= v {
                v = *f;
                c1 = i as i32;
            }
        }
        let (mut c2, mut v) = (-1i32, u64::MAX);
        for (i, f) in freq.iter().enumerate() {
            if *f != 0 && *f <= v && i as i32 != c1 {
                v = *f;
                c2 = i as i32;
            }
        }
        if c2 < 0 {
            break;
        }
        freq[c1 as usize] += freq[c2 as usize];
        freq[c2 as usize] = 0;
        codesize[c1 as usize] += 1;
        while others[c1 as usize] >= 0 {
            c1 = others[c1 as usize];
            codesize[c1 as usize] += 1;
        }
        others[c1 as usize] = c2;
        codesize[c2 as usize] += 1;
        while others[c2 as usize] >= 0 {
            c2 = others[c2 as usize];
            codesize[c2 as usize] += 1;
        }
    }
    let mut bits = [0u32; 64];
    for cs in codesize {
        if cs > 32 {
            bail!("Huffman code longer than 32 bits");
        }
        if cs > 0 {
            bits[cs] += 1;
        }
    }
    for i in (17..=32).rev() {
        while bits[i] > 0 {
            let mut j = i - 2;
            while bits[j] == 0 {
                j -= 1;
            }
            bits[i] -= 2;
            bits[i - 1] += 1;
            bits[j + 1] += 2;
            bits[j] -= 1;
        }
    }
    let mut i = 16;
    while bits[i] == 0 {
        i -= 1;
    }
    bits[i] -= 1; // the reserved all-ones code (symbol 256)
    let mut vals = Vec::new();
    for len in 1..=32 {
        for (sym, cs) in codesize.iter().enumerate().take(256) {
            if *cs == len {
                vals.push(sym as u8);
            }
        }
    }
    let mut counts16 = [0u8; 16];
    for (l, c) in counts16.iter_mut().enumerate() {
        *c = bits[l + 1] as u8;
    }
    Ok(table_from_spec(&counts16, vals))
}

/// A table from the DHT form: code counts for lengths 1..=16 and the symbols in code order.
fn table_from_spec(counts: &[u8; 16], vals: Vec<u8>) -> HuffTable {
    let mut t = HuffTable {
        bits: [0; 17],
        vals,
        code: [0; 256],
        len: [0; 256],
    };
    t.bits[1..].copy_from_slice(counts);
    let (mut code, mut k) = (0u32, 0usize);
    for l in 1..=16usize {
        for _ in 0..t.bits[l] {
            let s = t.vals[k] as usize;
            t.code[s] = code as u16;
            t.len[s] = l as u8;
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    t
}

/// Annex K.3 standard tables (what libjpeg writes by default and most cameras use), in table
/// slots 0..4: DC luminance, DC chrominance, AC luminance, AC chrominance.
pub const STD_DC_LUMA: ([u8; 16], [u8; 12]) = (
    [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
);
pub const STD_DC_CHROMA: ([u8; 16], [u8; 12]) = (
    [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
);
pub const STD_AC_LUMA: ([u8; 16], [u8; 162]) = (
    [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);
pub const STD_AC_CHROMA: ([u8; 16], [u8; 162]) = (
    [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
        0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
        0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
        0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);

fn standard_tables() -> Vec<Option<HuffTable>> {
    vec![
        Some(table_from_spec(&STD_DC_LUMA.0, STD_DC_LUMA.1.to_vec())),
        Some(table_from_spec(&STD_DC_CHROMA.0, STD_DC_CHROMA.1.to_vec())),
        Some(table_from_spec(&STD_AC_LUMA.0, STD_AC_LUMA.1.to_vec())),
        Some(table_from_spec(&STD_AC_CHROMA.0, STD_AC_CHROMA.1.to_vec())),
    ]
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | u64::from(value & ((1u32 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            let b = ((self.acc >> (self.n - 8)) & 0xFF) as u8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
            self.n -= 8;
        }
    }

    fn finish(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1u32 << pad) - 1, pad);
        }
        self.acc = 0;
    }
}

/// Table slots: 0 and 1 are the DC tables (luma, chroma), 2 and 3 the AC tables.
enum Sink {
    Count(Box<[[u64; 257]; 4]>),
    Write {
        tables: Vec<Option<HuffTable>>,
        bw: BitWriter,
        bad: bool,
    },
}

impl Sink {
    fn sym(&mut self, table: usize, symbol: u8) {
        match self {
            Sink::Count(c) => c[table][symbol as usize] += 1,
            Sink::Write { tables, bw, bad } => match tables[table].as_ref() {
                Some(t) if t.len[symbol as usize] > 0 => {
                    bw.put(
                        u32::from(t.code[symbol as usize]),
                        u32::from(t.len[symbol as usize]),
                    );
                }
                _ => *bad = true,
            },
        }
    }

    fn bits(&mut self, value: u32, n: u32) {
        if let Sink::Write { bw, .. } = self {
            bw.put(value, n);
        }
    }
}

fn emit_dc(s: &mut Sink, table: usize, diff: i32) {
    let size = bit_len(diff.unsigned_abs());
    s.sym(table, size as u8);
    if size > 0 {
        s.bits(value_bits(diff, size), size);
    }
}

/// Baseline AC coding of one block (zigzag order).
fn emit_ac_baseline(s: &mut Sink, table: usize, blk: &[i16; 64]) {
    let mut run = 0u32;
    for &v in &blk[1..] {
        if v == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            s.sym(table, 0xF0);
            run -= 16;
        }
        let size = bit_len(u32::from(v.unsigned_abs()));
        s.sym(table, ((run << 4) | size) as u8);
        s.bits(value_bits(i32::from(v), size), size);
        run = 0;
    }
    if run > 0 {
        s.sym(table, 0x00);
    }
}

fn flush_eobrun(s: &mut Sink, table: usize, eobrun: &mut u32) {
    if *eobrun > 0 {
        let nbits = bit_len(*eobrun) - 1;
        s.sym(table, (nbits << 4) as u8);
        if nbits > 0 {
            s.bits(*eobrun & ((1 << nbits) - 1), nbits);
        }
        *eobrun = 0;
    }
}

#[derive(Clone, Copy)]
enum Scan {
    Baseline,
    DcFirst,
    /// Component index, spectral start and end (inclusive).
    AcFirst(usize, usize, usize),
}

/// Run `scan` over the blocks, feeding symbols and bits to `s`.
fn run_scan(s: &mut Sink, comps: &[Component; 3], scan: Scan) {
    match scan {
        Scan::Baseline | Scan::DcFirst => {
            let mut pred = [0i32; 3];
            let (ybw, cbw) = (comps[0].bw, comps[1].bw);
            for my in 0..comps[1].bh {
                for mx in 0..cbw {
                    let mut order = [(0usize, 0usize); 6];
                    for (n, (dy, dx)) in [(0, 0), (0, 1), (1, 0), (1, 1)].into_iter().enumerate() {
                        order[n] = (0, (2 * my + dy) * ybw + 2 * mx + dx);
                    }
                    order[4] = (1, my * cbw + mx);
                    order[5] = (2, my * cbw + mx);
                    for (c, idx) in order {
                        let blk = &comps[c].blocks[idx];
                        let tbl = usize::from(c > 0);
                        let dc = i32::from(blk[0]);
                        emit_dc(s, tbl, dc - pred[c]);
                        pred[c] = dc;
                        if matches!(scan, Scan::Baseline) {
                            emit_ac_baseline(s, 2 + tbl, blk);
                        }
                    }
                }
            }
        }
        Scan::AcFirst(c, ss, se) => {
            let comp = &comps[c];
            let mut eobrun = 0u32;
            for by in 0..comp.real_bh {
                for bx in 0..comp.real_bw {
                    let blk = &comp.blocks[by * comp.bw + bx];
                    let mut run = 0u32;
                    for &v in &blk[ss..=se] {
                        if v == 0 {
                            run += 1;
                            continue;
                        }
                        flush_eobrun(s, 2, &mut eobrun);
                        while run > 15 {
                            s.sym(2, 0xF0);
                            run -= 16;
                        }
                        let size = bit_len(u32::from(v.unsigned_abs()));
                        s.sym(2, ((run << 4) | size) as u8);
                        s.bits(value_bits(i32::from(v), size), size);
                        run = 0;
                    }
                    if run > 0 {
                        eobrun += 1;
                        if eobrun == 0x7FFF {
                            flush_eobrun(s, 2, &mut eobrun);
                        }
                    }
                }
            }
            flush_eobrun(s, 2, &mut eobrun);
        }
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

/// Encode one scan: optimised tables (DHT), SOS and the entropy-coded data.
fn write_scan(out: &mut Vec<u8>, comps: &[Component; 3], scan: Scan) -> Result<()> {
    let mut sink = Sink::Count(Box::new([[0u64; 257]; 4]));
    run_scan(&mut sink, comps, scan);
    let Sink::Count(counts) = sink else {
        bail!("internal error: counting sink changed");
    };
    // Baseline uses the standard tables (as libjpeg does by default); progressive scans need
    // tables that cover the EOBn symbols, so they get tables optimised for the scan.
    let mut tables: Vec<Option<HuffTable>> = Vec::new();
    if matches!(scan, Scan::Baseline) {
        tables = standard_tables();
    } else {
        for c in counts.iter() {
            tables.push(if c.iter().any(|f| *f != 0) {
                Some(build_table(c)?)
            } else {
                None
            });
        }
    }
    for (slot, t) in tables.iter().enumerate() {
        if let Some(t) = t {
            let mut p = vec![((slot / 2) as u8) << 4 | (slot % 2) as u8];
            p.extend_from_slice(&t.bits[1..]);
            p.extend_from_slice(&t.vals);
            segment(out, 0xC4, &p);
        }
    }
    // The AC tables of progressive scans live in slot 2 and are declared as AC table 0 above.
    let (comp_sel, ss, se): (Vec<(u8, u8)>, u8, u8) = match scan {
        Scan::Baseline => (vec![(1, 0x00), (2, 0x11), (3, 0x11)], 0, 63),
        Scan::DcFirst => (vec![(1, 0x00), (2, 0x10), (3, 0x10)], 0, 0),
        Scan::AcFirst(c, ss, se) => (vec![(comps[c].id, 0x00)], ss as u8, se as u8),
    };
    let mut p = vec![comp_sel.len() as u8];
    for (id, sel) in comp_sel {
        p.push(id);
        p.push(sel);
    }
    p.extend_from_slice(&[ss, se, 0x00]);
    segment(out, 0xDA, &p);
    let mut sink = Sink::Write {
        tables,
        bw: BitWriter {
            out: Vec::new(),
            acc: 0,
            n: 0,
        },
        bad: false,
    };
    run_scan(&mut sink, comps, scan);
    let Sink::Write { mut bw, bad, .. } = sink else {
        bail!("internal error: writing sink changed");
    };
    if bad {
        bail!("internal error: symbol without a Huffman code");
    }
    bw.finish();
    out.extend_from_slice(&bw.out);
    Ok(())
}

/// Encode interleaved 8-bit RGB (`width * height * 3` bytes) as a 4:2:0 JFIF JPEG.
pub fn encode(
    rgb: &[u8],
    width: usize,
    height: usize,
    quality: u8,
    progressive: bool,
) -> Result<Vec<u8>> {
    encode_with(rgb, width, height, quality, progressive, &[])
}

/// The APP1..APP15 segments (EXIF, XMP, ICC and the like) of a JPEG, verbatim, each from its
/// `FF En` marker to the end of its payload, concatenated in file order.
pub fn app_segments(jpeg: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF {
        let m = jpeg[i + 1];
        if m == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if m == 0xDA || m == 0xD9 {
            break;
        }
        let len = (usize::from(jpeg[i + 2]) << 8) | usize::from(jpeg[i + 3]);
        let end = i + 2 + len;
        if len < 2 || end > jpeg.len() {
            break;
        }
        if (0xE1..=0xEF).contains(&m) {
            out.extend_from_slice(&jpeg[i..end]);
        }
        i = end;
    }
    out
}

/// [`encode`] with `extra` (complete marker segments, e.g. from [`app_segments`]) written
/// right after the JFIF APP0 segment.
pub fn encode_with(
    rgb: &[u8],
    width: usize,
    height: usize,
    quality: u8,
    progressive: bool,
    extra: &[u8],
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > 65535 || height > 65535 {
        bail!("unsupported image size {width}x{height}");
    }
    if rgb.len() != width * height * 3 {
        bail!("pixel buffer does not match {width}x{height} RGB");
    }
    let comps = components(rgb, width, height, quality);
    let mut out = vec![0xFF, 0xD8];
    segment(
        &mut out,
        0xE0,
        &[b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0],
    );
    out.extend_from_slice(extra);
    for (id, table) in [(0u8, &LUMA_Q), (1u8, &CHROMA_Q)] {
        let t = scaled_table(table, quality);
        let mut p = vec![id];
        p.extend(ZIGZAG.iter().map(|z| t[*z] as u8));
        segment(&mut out, 0xDB, &p);
    }
    let mut sof = vec![8u8];
    sof.extend_from_slice(&(height as u16).to_be_bytes());
    sof.extend_from_slice(&(width as u16).to_be_bytes());
    sof.push(3);
    for c in &comps {
        sof.extend_from_slice(&[c.id, c.hv, c.tq]);
    }
    segment(&mut out, if progressive { 0xC2 } else { 0xC0 }, &sof);
    if progressive {
        for scan in [
            Scan::DcFirst,
            Scan::AcFirst(0, 1, 5),
            Scan::AcFirst(1, 1, 63),
            Scan::AcFirst(2, 1, 63),
            Scan::AcFirst(0, 6, 63),
        ] {
            write_scan(&mut out, &comps, scan)?;
        }
    } else {
        write_scan(&mut out, &comps, Scan::Baseline)?;
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

/// Decode a JPEG to 8-bit RGB with the pure-Rust `jpeg-decoder` (platform-independent IDCT).
/// Returns `(rgb, width, height)`; greyscale is expanded, CMYK is rejected.
pub fn decode_rgb(data: &[u8]) -> Result<(Vec<u8>, usize, usize)> {
    let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    let pixels = d
        .decode()
        .map_err(|e| anyhow::anyhow!("JPEG decode: {e}"))?;
    let Some(info) = d.info() else {
        bail!("JPEG without frame info");
    };
    let (w, h) = (usize::from(info.width), usize::from(info.height));
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|g| [*g, *g, *g]).collect(),
        jpeg_decoder::PixelFormat::L16 => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|g| [g[0], g[0], g[0]])
            .collect(),
        jpeg_decoder::PixelFormat::CMYK32 => bail!("CMYK JPEG is not supported"),
    };
    if rgb.len() != w * h * 3 {
        bail!("decoded JPEG has an unexpected size");
    }
    Ok((rgb, w, h))
}

/// Dimensions of a JPEG from its headers only.
pub fn jpeg_dims(data: &[u8]) -> Result<(usize, usize)> {
    let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    d.read_info()
        .map_err(|e| anyhow::anyhow!("JPEG header: {e}"))?;
    let Some(info) = d.info() else {
        bail!("JPEG without frame info");
    };
    Ok((usize::from(info.width), usize::from(info.height)))
}

#[cfg(test)]
pub mod tests_support {
    /// A smooth test picture with a little structure.
    pub fn picture(w: usize, h: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w.max(1)) as u8;
                let g = (y * 255 / h.max(1)) as u8;
                let b = (((x / 8 + y / 8) % 2) * 80 + 60) as u8;
                v.extend_from_slice(&[r, g, b]);
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::picture;
    use super::*;

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse = a
            .iter()
            .zip(b)
            .map(|(x, y)| {
                let d = f64::from(*x) - f64::from(*y);
                d * d
            })
            .sum::<f64>()
            / a.len() as f64;
        10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10()
    }

    /// `(Tc/Th byte, counts, symbols)` of every DHT segment before the first scan.
    fn dht_tables(jpeg: &[u8]) -> Vec<(u8, Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        let mut i = 2;
        while i + 4 <= jpeg.len() && jpeg[i + 1] != 0xDA {
            let len = (usize::from(jpeg[i + 2]) << 8) | usize::from(jpeg[i + 3]);
            if jpeg[i + 1] == 0xC4 {
                let mut j = i + 4;
                while j < i + 2 + len {
                    let counts = jpeg[j + 1..j + 17].to_vec();
                    let n: usize = counts.iter().map(|c| usize::from(*c)).sum();
                    out.push((jpeg[j], counts, jpeg[j + 17..j + 17 + n].to_vec()));
                    j += 17 + n;
                }
            }
            i += 2 + len;
        }
        out
    }

    #[test]
    fn baseline_uses_the_standard_tables_and_progressive_optimised_ones() {
        let rgb = picture(64, 48);
        let base = encode(&rgb, 64, 48, 90, false).expect("baseline");
        let tables = dht_tables(&base);
        let expect = [
            (0x00, STD_DC_LUMA.0.to_vec(), STD_DC_LUMA.1.to_vec()),
            (0x10, STD_AC_LUMA.0.to_vec(), STD_AC_LUMA.1.to_vec()),
            (0x01, STD_DC_CHROMA.0.to_vec(), STD_DC_CHROMA.1.to_vec()),
            (0x11, STD_AC_CHROMA.0.to_vec(), STD_AC_CHROMA.1.to_vec()),
        ];
        let mut got = tables.clone();
        got.sort();
        let mut want = expect.to_vec();
        want.sort();
        assert_eq!(got, want);
        // The standard AC tables hold every (run, size) pair with size 1..=10, EOB and ZRL.
        for t in [&STD_AC_LUMA.1, &STD_AC_CHROMA.1] {
            let mut v = t.to_vec();
            v.sort_unstable();
            let mut all: Vec<u8> = (0..16u8)
                .flat_map(|r| (1..=10u8).map(move |s| (r << 4) | s))
                .collect();
            all.extend([0x00, 0xF0]);
            all.sort_unstable();
            assert_eq!(v, all);
        }
        let prog = encode(&rgb, 64, 48, 90, true).expect("progressive");
        assert!(dht_tables(&prog)
            .iter()
            .all(|t| !expect.iter().any(|e| e.1 == t.1 && e.2 == t.2)));
    }

    #[test]
    fn zigzag_is_a_permutation() {
        let mut z = ZIGZAG.to_vec();
        z.sort_unstable();
        assert_eq!(z, (0..64).collect::<Vec<_>>());
    }

    #[test]
    fn baseline_and_progressive_decode_to_the_same_good_picture() {
        for (w, h) in [(70usize, 45usize), (16, 16), (1, 1), (33, 7), (200, 130)] {
            let rgb = picture(w, h);
            let base = encode(&rgb, w, h, 90, false).expect("baseline");
            let prog = encode(&rgb, w, h, 90, true).expect("progressive");
            assert!(base.windows(2).any(|p| p == [0xFF, 0xC0]));
            assert!(prog.windows(2).any(|p| p == [0xFF, 0xC2]));
            assert_eq!(jpeg_dims(&base).expect("dims"), (w, h));
            let (a, aw, ah) = decode_rgb(&base).expect("decode baseline");
            let (b, bw, bh) = decode_rgb(&prog).expect("decode progressive");
            assert_eq!((aw, ah, bw, bh), (w, h, w, h));
            assert_eq!(a, b, "{w}x{h}: same coefficients must decode identically");
            if w * h > 300 {
                let p = psnr(&rgb, &a);
                assert!(p > 28.0, "{w}x{h}: psnr {p}");
            }
        }
    }

    #[test]
    fn quality_changes_size_and_output_repeats() {
        let rgb = picture(120, 90);
        let hi = encode(&rgb, 120, 90, 95, false).expect("hi");
        let lo = encode(&rgb, 120, 90, 30, false).expect("lo");
        assert!(lo.len() < hi.len());
        assert_eq!(hi, encode(&rgb, 120, 90, 95, false).expect("again"));
        let p1 = encode(&rgb, 120, 90, 85, true).expect("p1");
        assert_eq!(p1, encode(&rgb, 120, 90, 85, true).expect("p2"));
    }

    #[test]
    fn noisy_images_with_long_zero_runs_and_big_eob_runs_round_trip() {
        // Flat areas (long EOB runs) next to noise (all symbols, long runs).
        let (w, h) = (256usize, 256usize);
        let mut state = 12345u32;
        let mut rgb = vec![128u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                if x > 128 && y < 128 {
                    state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                    let n = (state >> 24) as u8;
                    rgb[(y * w + x) * 3..][..3].copy_from_slice(&[n, n ^ 0x55, 255 - n]);
                }
            }
        }
        let a = encode(&rgb, w, h, 75, false).expect("base");
        let b = encode(&rgb, w, h, 75, true).expect("prog");
        assert_eq!(decode_rgb(&a).expect("da").0, decode_rgb(&b).expect("db").0);
    }

    #[test]
    fn rejects_bad_buffers() {
        assert!(encode(&[0; 5], 1, 1, 80, false).is_err());
        assert!(encode(&[], 0, 1, 80, false).is_err());
    }
}
