//! The Reed-Solomon code of `Recovery` frames re-derived from the text of spec
//! section 13 ("The code") alone (task E1-15).
//!
//! Everything under "The description" below is written from the specification's
//! words, in plain Rust, and calls nothing of `reed-solomon-simd`. The tests then
//! hold it against the committed vector (part a), against the library on random
//! data (part b), against the normative decoding rule (part c) and against the
//! check values the text gives (part d). Each function names the paragraph of
//! section 13 it implements.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::needless_range_loop)]

use lpk_format::{Archive, FrameKind, RecoveryFrame, Resources};
use std::io::Cursor;

// ======================================================================
// The description
// ======================================================================

/// 13, "The field": the defining polynomial x^16 + x^5 + x^3 + x^2 + 1.
const POLY: u32 = 0x1_002D;

/// 13, "The field": the basis b_0 .. b_15, each written in the polynomial
/// representation (bit n is the coefficient of x^n).
const BASIS: [u16; 16] = [
    0x0001, 0xACCA, 0x3C0E, 0x163E, 0xC582, 0xED2E, 0x914C, 0x4012, 0x6C98, 0x10D8, 0x6A72, 0xB900,
    0xFDB8, 0xFB34, 0xFF38, 0x991E,
];

/// 13, "The field": the product of two field elements in the polynomial
/// representation, the carry-less product reduced modulo `POLY`. Slow; used
/// to build and to check the tables.
fn clmul(a: u16, b: u16) -> u16 {
    let mut acc: u32 = 0;
    for n in 0..16 {
        if b >> n & 1 == 1 {
            acc ^= u32::from(a) << n;
        }
    }
    for n in (16..32).rev() {
        if acc >> n & 1 == 1 {
            acc ^= POLY << (n - 16);
        }
    }
    acc as u16
}

/// Field arithmetic in the polynomial representation, with the conversion
/// between a symbol (a 16-bit value read from a shard) and its element.
struct Field {
    /// exp[n] = x^n for n in 0..2*65535.
    exp: Vec<u16>,
    /// log[e] for e != 0.
    log: Vec<u32>,
    /// 13, "The field": symbol s is the element sum of b_n over the set bits n of s.
    sym_to_el: Vec<u16>,
    el_to_sym: Vec<u16>,
}

impl Field {
    fn new() -> Field {
        let mut exp = vec![0u16; 2 * 65535];
        let mut log = vec![0u32; 65536];
        let mut e: u16 = 1;
        for n in 0..65535u32 {
            exp[n as usize] = e;
            exp[n as usize + 65535] = e;
            log[e as usize] = n;
            e = clmul(e, 2);
        }
        assert_eq!(e, 1, "x generates the multiplicative group");
        let mut sym_to_el = vec![0u16; 65536];
        let mut el_to_sym = vec![0u16; 65536];
        for s in 1..65536usize {
            let low = s.trailing_zeros() as usize;
            sym_to_el[s] = sym_to_el[s & (s - 1)] ^ BASIS[low];
        }
        for s in 0..65536usize {
            el_to_sym[sym_to_el[s] as usize] = s as u16;
        }
        // The basis is a basis: the map is one to one.
        assert!(el_to_sym[1..].iter().all(|&s| s != 0));
        Field {
            exp,
            log,
            sym_to_el,
            el_to_sym,
        }
    }

    fn mul(&self, a: u16, b: u16) -> u16 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[(self.log[a as usize] + self.log[b as usize]) as usize]
        }
    }

    fn inv(&self, a: u16) -> u16 {
        assert_ne!(a, 0);
        self.exp[((65535 - self.log[a as usize]) % 65535) as usize]
    }

    /// 13, "The evaluation points": w_j, the element of symbol j.
    fn w(&self, j: usize) -> u16 {
        self.sym_to_el[j]
    }

    /// 13, "The evaluation points": W_m(x), the product of (x + w_s) over s < m.
    fn subspace_poly(&self, m: usize, x: u16) -> u16 {
        (0..m).fold(1, |acc, s| self.mul(acc, x ^ self.w(s)))
    }

    /// 13, "The evaluation points": k_m, the product of w_s over 1 <= s < m.
    fn kappa(&self, m: usize) -> u16 {
        (1..m).fold(1, |acc, s| self.mul(acc, self.w(s)))
    }
}

/// 13, "Shards and symbols": shard bytes to its `shard_len / 2` symbols; in
/// each 64-byte unit, byte t is the low byte and byte 32 + t the high byte of
/// the unit's symbol t.
fn shard_symbols(shard: &[u8]) -> Vec<u16> {
    assert_eq!(shard.len() % 64, 0);
    let mut out = Vec::with_capacity(shard.len() / 2);
    for unit in shard.as_chunks::<64>().0 {
        for t in 0..32 {
            out.push(u16::from(unit[t]) | u16::from(unit[32 + t]) << 8);
        }
    }
    out
}

/// The inverse of [`shard_symbols`].
fn symbols_shard(symbols: &[u16]) -> Vec<u8> {
    assert_eq!(symbols.len() % 32, 0);
    let mut out = vec![0u8; symbols.len() * 2];
    for (u, unit) in symbols.as_chunks::<32>().0.iter().enumerate() {
        for t in 0..32 {
            out[64 * u + t] = unit[t] as u8;
            out[64 * u + 32 + t] = (unit[t] >> 8) as u8;
        }
    }
    out
}

/// 13, "Supported counts": the predicate of supported pairs.
fn supports(k: usize, r: usize) -> bool {
    k >= 1
        && r >= 1
        && k <= 65536
        && r <= 65536
        && k.next_power_of_two().min(r.next_power_of_two()) + k.max(r) <= 65536
}

/// 13, "Which form a pair uses".
fn uses_high_form(k: usize, r: usize) -> bool {
    let (pk, pr) = (k.next_power_of_two(), r.next_power_of_two());
    if pk != pr {
        pk > pr
    } else {
        k <= r
    }
}

/// 13, "The encoding map": the r x k matrix, in field elements, with
/// recovery symbol t = sum over i of C[t][i] * data symbol i (data symbols as
/// elements, the implicit zero shards included as zeros).
fn coefficients(f: &Field, k: usize, r: usize) -> Vec<Vec<u16>> {
    assert!(supports(k, r));
    let mut c = vec![vec![0u16; k]; r];
    if uses_high_form(k, r) {
        // High form: m = next_pow2(r); data shard i at w_{m+i}, recovery shard
        // t at w_t; C[t][i] = W_m(w_{m+i}) / (k_m * (w_t + w_{m+i})).
        let m = r.next_power_of_two();
        let inv_kappa = f.inv(f.kappa(m));
        let mut scale = Vec::new();
        for i in 0..k {
            let x = f.w(m + i);
            if i % m == 0 {
                scale.push(f.mul(f.subspace_poly(m, x), inv_kappa));
            }
            for t in 0..r {
                c[t][i] = f.mul(scale[i / m], f.inv(f.w(t) ^ x));
            }
        }
    } else {
        // Low form: m = next_pow2(k); data shard i at w_i, recovery shard j at
        // w_{m+j}; C[j][i] = W_m(w_{m+j}) / (k_m * (w_{m+j} + w_i)).
        let m = k.next_power_of_two();
        let inv_kappa = f.inv(f.kappa(m));
        for j in 0..r {
            let y = f.w(m + j);
            let scale = f.mul(f.subspace_poly(m, y), inv_kappa);
            for i in 0..k {
                c[j][i] = f.mul(scale, f.inv(y ^ f.w(i)));
            }
        }
    }
    c
}

/// Shards as element vectors (symbol by symbol through the basis).
fn to_elements(f: &Field, shard: &[u8]) -> Vec<u16> {
    shard_symbols(shard)
        .into_iter()
        .map(|s| f.sym_to_el[s as usize])
        .collect()
}

fn to_shard(f: &Field, elements: &[u16]) -> Vec<u8> {
    let symbols: Vec<u16> = elements.iter().map(|&e| f.el_to_sym[e as usize]).collect();
    symbols_shard(&symbols)
}

/// 13, "The encoding map", by the matrix: the recovery shards of a group of
/// `k` data shards of which `data` are the real ones (the rest implicit zeros).
fn encode_by_matrix(f: &Field, k: usize, r: usize, data: &[Vec<u8>]) -> Vec<Vec<u8>> {
    assert!(data.len() <= k);
    let len = data[0].len();
    let c = coefficients(f, k, r);
    let d: Vec<Vec<u16>> = data.iter().map(|s| to_elements(f, s)).collect();
    (0..r)
        .map(|t| {
            let mut acc = vec![0u16; len / 2];
            for (i, di) in d.iter().enumerate() {
                let ci = c[t][i];
                for (a, &x) in acc.iter_mut().zip(di) {
                    *a ^= f.mul(ci, x);
                }
            }
            to_shard(f, &acc)
        })
        .collect()
}

/// 13, "The fast transform": the twist factor of a butterfly block,
/// W_{2^b}(w_base) / W_{2^b}(w_{2^b}), by the definition.
fn twist(f: &Field, b: usize, base: usize) -> u16 {
    let n = 1usize << b;
    f.mul(
        f.subspace_poly(n, f.w(base)),
        f.inv(f.subspace_poly(n, f.w(n))),
    )
}

/// 13, "The fast transform": FFT at offset `delta` (a multiple of x.len()).
/// From coefficients in the subspace-polynomial basis to the values at
/// w_{delta + i}.
fn fft(f: &Field, x: &mut [Vec<u16>], delta: usize) {
    let m = x.len();
    let mut dist = m / 2;
    while dist > 0 {
        let b = dist.trailing_zeros() as usize;
        for r in (0..m).step_by(2 * dist) {
            let lambda = twist(f, b, delta + r);
            for i in r..r + dist {
                let (lo, hi) = x.split_at_mut(i + dist);
                let (a, c) = (&mut lo[i], &mut hi[0]);
                for (av, &cv) in a.iter_mut().zip(c.iter()) {
                    *av ^= f.mul(lambda, cv);
                }
                for (cv, &av) in c.iter_mut().zip(a.iter()) {
                    *cv ^= av;
                }
            }
        }
        dist /= 2;
    }
}

/// 13, "The fast transform": the inverse of [`fft`] at the same offset.
fn ifft(f: &Field, x: &mut [Vec<u16>], delta: usize) {
    let m = x.len();
    let mut dist = 1;
    while dist < m {
        let b = dist.trailing_zeros() as usize;
        for r in (0..m).step_by(2 * dist) {
            let lambda = twist(f, b, delta + r);
            for i in r..r + dist {
                let (lo, hi) = x.split_at_mut(i + dist);
                let (a, c) = (&mut lo[i], &mut hi[0]);
                for (cv, &av) in c.iter_mut().zip(a.iter()) {
                    *cv ^= av;
                }
                for (av, &cv) in a.iter_mut().zip(c.iter()) {
                    *av ^= f.mul(lambda, cv);
                }
            }
        }
        dist *= 2;
    }
}

/// 13, "The fast transform": the recovery shards computed with the transforms
/// (the high form by chunks of the data, the low form by chunks of the
/// recovery shards).
fn encode_by_transform(f: &Field, k: usize, r: usize, data: &[Vec<u8>]) -> Vec<Vec<u8>> {
    assert!(supports(k, r) && data.len() <= k);
    let syms = data[0].len() / 2;
    let el = |i: usize| -> Vec<u16> {
        if i < data.len() {
            to_elements(f, &data[i])
        } else {
            vec![0u16; syms]
        }
    };
    let out: Vec<Vec<u16>> = if uses_high_form(k, r) {
        let m = r.next_power_of_two();
        let mut sum = vec![vec![0u16; syms]; m];
        for c in 0..k.div_ceil(m) {
            let mut chunk: Vec<Vec<u16>> = (c * m..(c + 1) * m).map(el).collect();
            ifft(f, &mut chunk, (c + 1) * m);
            for (s, y) in sum.iter_mut().zip(&chunk) {
                for (a, &b) in s.iter_mut().zip(y) {
                    *a ^= b;
                }
            }
        }
        fft(f, &mut sum, 0);
        sum.truncate(r);
        sum
    } else {
        let m = k.next_power_of_two();
        let mut coef: Vec<Vec<u16>> = (0..m).map(el).collect();
        ifft(f, &mut coef, 0);
        let mut out = Vec::new();
        for c in 0..r.div_ceil(m) {
            let mut chunk = coef.clone();
            fft(f, &mut chunk, (c + 1) * m);
            out.extend(chunk);
        }
        out.truncate(r);
        out
    };
    out.iter().map(|e| to_shard(f, e)).collect()
}

/// 13, "Decoding" (normative rule): rebuild the erased shards of a group by
/// solving the encoding map's linear equations (Gaussian elimination over the
/// field). `shards` holds the k data shards then the r recovery shards; `None`
/// marks an erasure. At most r may be erased.
fn decode_by_elimination(
    f: &Field,
    k: usize,
    r: usize,
    shards: &[Option<Vec<u8>>],
) -> Vec<Vec<u8>> {
    assert_eq!(shards.len(), k + r);
    let len = shards.iter().flatten().next().unwrap().len();
    let syms = len / 2;
    let c = coefficients(f, k, r);
    let lost: Vec<usize> = (0..k).filter(|&i| shards[i].is_none()).collect();
    let rows: Vec<usize> = (0..r)
        .filter(|&t| shards[k + t].is_some())
        .take(lost.len())
        .collect();
    assert_eq!(rows.len(), lost.len(), "more erasures than recovery shards");
    let mut d: Vec<Vec<u16>> = (0..k)
        .map(|i| match &shards[i] {
            Some(s) => to_elements(f, s),
            None => vec![0u16; syms],
        })
        .collect();
    let n = lost.len();
    if n > 0 {
        // A[row][col] = C[rows[row]][lost[col]]; b[row] = rec - known part.
        let mut a: Vec<Vec<u16>> = rows
            .iter()
            .map(|&t| lost.iter().map(|&i| c[t][i]).collect())
            .collect();
        let mut b: Vec<Vec<u16>> = rows
            .iter()
            .map(|&t| {
                let mut v = to_elements(f, shards[k + t].as_ref().unwrap());
                for i in 0..k {
                    if shards[i].is_some() {
                        for (x, &y) in v.iter_mut().zip(&d[i]) {
                            *x ^= f.mul(c[t][i], y);
                        }
                    }
                }
                v
            })
            .collect();
        for col in 0..n {
            let p = (col..n).find(|&q| a[q][col] != 0).expect("singular");
            a.swap(col, p);
            b.swap(col, p);
            let iv = f.inv(a[col][col]);
            for x in a[col].iter_mut() {
                *x = f.mul(*x, iv);
            }
            for x in b[col].iter_mut() {
                *x = f.mul(*x, iv);
            }
            for q in 0..n {
                if q != col && a[q][col] != 0 {
                    let fac = a[q][col];
                    let (prow, brow) = (a[col].clone(), b[col].clone());
                    for (x, &y) in a[q].iter_mut().zip(&prow) {
                        *x ^= f.mul(fac, y);
                    }
                    for (x, &y) in b[q].iter_mut().zip(&brow) {
                        *x ^= f.mul(fac, y);
                    }
                }
            }
        }
        for (col, &i) in lost.iter().enumerate() {
            d[i] = b[col].clone();
        }
    }
    let data: Vec<Vec<u8>> = d.iter().map(|e| to_shard(f, e)).collect();
    let rec = encode_by_matrix(f, k, r, &data);
    data.into_iter().chain(rec).collect()
}

// ======================================================================
// Tests
// ======================================================================

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 24) as u8).collect()
    }
}

/// The library's recovery shards for the same group (the implicit zero shards
/// given explicitly).
fn library(k: usize, r: usize, data: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let len = data[0].len();
    let mut all = data.to_vec();
    all.resize(k, vec![0u8; len]);
    reed_solomon_simd::encode(k, r, &all).unwrap()
}

/// Part (a): every recovery shard of the committed `recovery-groups.lpk`,
/// re-derived from its covered bytes by the description (13, "Shards and
/// symbols", "The encoding map", "The fast transform").
#[test]
fn a_recovery_vector_rederived_from_the_text() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join("recovery-groups.lpk");
    let bytes = std::fs::read(path).unwrap();
    let mut a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
    let index_at = a.trailer().index_offset;
    let locs = a.recovery_frames().to_vec();
    assert!(locs.len() >= 2);
    let f = Field::new();
    let mut checked = 0;
    for at in locs {
        let raw = a.read_frame_at(at, FrameKind::Recovery).unwrap();
        let fr = RecoveryFrame::parse(&raw.payload, index_at).unwrap();
        let (k, r, len) = (
            fr.group_shards as usize,
            fr.recovery_shards as usize,
            fr.shard_len as usize,
        );
        let data: Vec<Vec<u8>> = (0..fr.data_shards)
            .map(|i| {
                let (off, n) = fr.shard_range(i);
                let mut s = bytes[off as usize..off as usize + n].to_vec();
                s.resize(len, 0);
                s
            })
            .collect();
        let stored: Vec<Vec<u8>> = fr.recovery.chunks(len).map(<[u8]>::to_vec).collect();
        assert_eq!(stored.len(), r);
        assert_eq!(
            encode_by_matrix(&f, k, r, &data),
            stored,
            "frame at {}",
            at.offset
        );
        assert_eq!(
            encode_by_transform(&f, k, r, &data),
            stored,
            "frame at {}",
            at.offset
        );
        checked += r;
    }
    assert!(checked >= 2);
}

/// Part (b): the description against the library on random data over a spread
/// of supported pairs: both forms (13, "Which form a pair uses"), one and many
/// chunks, implicit zero shards, shard lengths of one and more 64-byte units.
#[test]
fn b_description_matches_the_library() {
    let f = Field::new();
    let mut rng = Rng(0x00E1_0015_5EED);
    // (group_shards, recovery_shards, high form expected)
    let pairs: &[(usize, usize, bool)] = &[
        (1, 1, true),
        (2, 1, true),
        (12, 3, true),
        (5, 7, true),
        (64, 64, true),
        (100, 64, true),
        (300, 70, true),
        (1000, 3, true),
        (2048, 410, true),
        (32768, 2, true),
        (1, 2, false),
        (2, 3, false),
        (3, 5, false),
        (7, 5, false),
        (10, 40, false),
        (33, 1000, false),
        (100, 300, false),
        (4, 4000, false),
    ];
    // 13, "Supported counts": the predicate agrees with the library's at the edges.
    let edges = [0usize, 1, 2, 3, 4, 5, 4095, 4096, 4097, 32767, 32768, 32769];
    for &k in edges
        .iter()
        .chain(&[61439, 61440, 61441, 65535, 65536, 65537])
    {
        for &r in edges
            .iter()
            .chain(&[61439, 61440, 61441, 65535, 65536, 65537])
        {
            assert_eq!(
                supports(k, r),
                reed_solomon_simd::ReedSolomonDecoder::supports(k, r),
                "{k} {r}"
            );
        }
    }
    for (n, &(k, r, high)) in pairs.iter().enumerate() {
        assert!(supports(k, r), "{k} {r}");
        assert_eq!(uses_high_form(k, r), high, "{k} {r}");
        let len = [64, 128, 1024][n % 3];
        let len = if k * r > 100_000 { 64 } else { len };
        let real = if n % 2 == 0 { k } else { 1 + rng.below(k) };
        let data: Vec<Vec<u8>> = (0..real).map(|_| rng.bytes(len)).collect();
        let want = library(k, r, &data);
        assert_eq!(encode_by_matrix(&f, k, r, &data), want, "matrix {k} {r}");
        if k * r <= 1_000_000 {
            assert_eq!(encode_by_transform(&f, k, r, &data), want, "fft {k} {r}");
        }
    }
}

/// Part (c): erase up to `recovery_shards` shards of random groups and rebuild
/// them by the normative rule of 13, "Decoding" (any inverse of the encoding
/// map; here Gaussian elimination).
#[test]
fn c_erasures_rebuilt_by_elimination() {
    let f = Field::new();
    let mut rng = Rng(0xDEC0_DE00_0015);
    for &(k, r) in &[(12, 3), (5, 7), (7, 5), (40, 9), (3, 5), (16, 16), (1, 1)] {
        for round in 0..4 {
            let len = 64 * (1 + round % 2);
            let real = 1 + rng.below(k);
            let mut data: Vec<Vec<u8>> = (0..real).map(|_| rng.bytes(len)).collect();
            data.resize(k, vec![0u8; len]);
            let rec = library(k, r, &data[..real]);
            let full: Vec<Vec<u8>> = data.iter().cloned().chain(rec).collect();
            // Erase `e` shards; the implicit zero shards are never erased.
            let e = if round == 0 { r } else { 1 + rng.below(r) };
            let mut candidates: Vec<usize> = (0..real).chain(k..k + r).collect();
            let mut erased = Vec::new();
            while erased.len() < e.min(candidates.len()) {
                erased.push(candidates.swap_remove(rng.below(candidates.len())));
            }
            let damaged: Vec<Option<Vec<u8>>> = full
                .iter()
                .enumerate()
                .map(|(i, s)| (!erased.contains(&i)).then(|| s.clone()))
                .collect();
            assert_eq!(decode_by_elimination(&f, k, r, &damaged), full, "{k} {r}");
        }
    }
}

/// 13, "The fast transform": what the transform computes. FFT at offset delta
/// maps coefficients c_n in the basis X_n(X) = product over the set bits b of
/// n of W_{2^b}(X) / W_{2^b}(w_{2^b}) to the values at w_{delta + i}, and IFFT
/// at the same offset undoes it.
#[test]
fn e_transform_evaluates_in_the_subspace_polynomial_basis() {
    let f = Field::new();
    let mut rng = Rng(0x0FF7);
    let basis_poly = |n: usize, x: u16| -> u16 {
        (0..16)
            .filter(|b| n >> b & 1 == 1)
            .fold(1, |acc, b| f.mul(acc, twist_at(&f, b, x)))
    };
    for &(m, delta) in &[
        (1usize, 0usize),
        (2, 6),
        (8, 0),
        (8, 40),
        (16, 4096),
        (64, 128),
    ] {
        let coef: Vec<Vec<u16>> = (0..m).map(|_| vec![rng.next() as u16]).collect();
        let mut x = coef.clone();
        fft(&f, &mut x, delta);
        for i in 0..m {
            let at = f.w(delta + i);
            let want = (0..m).fold(0, |acc, n| acc ^ f.mul(coef[n][0], basis_poly(n, at)));
            assert_eq!(x[i][0], want, "m {m} delta {delta} i {i}");
        }
        ifft(&f, &mut x, delta);
        assert_eq!(x, coef);
    }
}

/// 13, "The encoding map": the matrix is the interpolation the text defines
/// the two forms by (high form: the sum over chunks c of the polynomial of
/// degree < m through the chunk's points w_{(c+1)m+t}, evaluated at w_j; low
/// form: the polynomial of degree < m through w_i, evaluated at w_{m+j}),
/// computed here by plain Lagrange products.
#[test]
fn f_matrix_is_the_interpolation_of_the_definition() {
    let f = Field::new();
    // The coefficient of the value at point `a` (one of `points`) in the
    // interpolating polynomial evaluated at `x`.
    let lagrange = |points: &[u16], a: u16, x: u16| -> u16 {
        points
            .iter()
            .filter(|&&b| b != a)
            .fold(1, |acc, &b| f.mul(acc, f.mul(x ^ b, f.inv(a ^ b))))
    };
    for &(k, r) in &[
        (12usize, 3usize),
        (5, 7),
        (7, 5),
        (3, 5),
        (40, 9),
        (2, 9),
        (1, 1),
    ] {
        let c = coefficients(&f, k, r);
        for j in 0..r {
            for i in 0..k {
                let want = if uses_high_form(k, r) {
                    let m = r.next_power_of_two();
                    let chunk = i / m;
                    let points: Vec<u16> = (0..m).map(|t| f.w((chunk + 1) * m + t)).collect();
                    lagrange(&points, f.w(m + i), f.w(j))
                } else {
                    let m = k.next_power_of_two();
                    let points: Vec<u16> = (0..m).map(|t| f.w(t)).collect();
                    lagrange(&points, f.w(i), f.w(m + j))
                };
                assert_eq!(c[j][i], want, "{k} {r} C[{j}][{i}]");
            }
        }
    }
}

/// W_{2^b}(x) / W_{2^b}(w_{2^b}) at any element x.
fn twist_at(f: &Field, b: usize, x: u16) -> u16 {
    let n = 1usize << b;
    f.mul(f.subspace_poly(n, x), f.inv(f.subspace_poly(n, f.w(n))))
}

/// Part (d): the check values printed in section 13.
#[test]
fn d_check_values_of_the_text() {
    let f = Field::new();
    // "The field": the basis is a Cantor basis, b_0 = 1 and b_n^2 + b_n = b_{n-1}.
    assert_eq!(BASIS[0], 1);
    for n in 1..16 {
        assert_eq!(clmul(BASIS[n], BASIS[n]) ^ BASIS[n], BASIS[n - 1], "b_{n}");
    }
    // Table and slow product agree on a sample.
    let mut rng = Rng(7);
    for _ in 0..1000 {
        let (a, b) = (rng.next() as u16, rng.next() as u16);
        assert_eq!(f.mul(a, b), clmul(a, b));
    }
    // "The evaluation points": W_m(w_j) = w_{j / m} and k_m = 1 for every m a
    // power of two (checked for m <= 256 and j over a sample).
    for lg in 0..=8 {
        let m = 1usize << lg;
        assert_eq!(f.kappa(m), 1, "k_{m}");
        for j in (0..65536).step_by(997).chain([m, 2 * m, 65535]) {
            assert_eq!(f.subspace_poly(m, f.w(j)), f.w(j >> lg), "W_{m}(w_{j})");
        }
    }
    // "The fast transform": the twist factor of block base at layer b is w_{base >> b}.
    for b in 0..6 {
        for base in (0..4096).step_by(2 << b) {
            assert_eq!(twist(&f, b, base), f.w(base >> b), "b {b} base {base}");
        }
    }

    // "The field": the check values of the symbol arithmetic.
    let sym = |e: u16| f.el_to_sym[e as usize];
    let el = |s: u16| f.sym_to_el[s as usize];
    let prod = |a: u16, b: u16| sym(f.mul(el(a), el(b)));
    assert_eq!(el(0x0002), 0xACCA);
    assert_eq!(el(0x8000), 0x991E);
    assert_eq!(sym(0x0002), 0x4690, "the element x as a symbol");
    assert_eq!(prod(0x0002, 0x0002), 0x0003);
    assert_eq!(prod(0x0002, 0x0003), 0x0001);
    assert_eq!(prod(0x1234, 0xABCD), 0x43C1);
    assert_eq!(sym(f.inv(el(0x0002))), 0x0003);

    // "The encoding map": the matrices (as symbols) and one coded symbol position
    // of two small groups, one of each form.
    let as_syms = |c: Vec<Vec<u16>>| -> Vec<Vec<u16>> {
        c.into_iter()
            .map(|row| row.into_iter().map(sym).collect())
            .collect()
    };
    assert!(uses_high_form(3, 2) && !uses_high_form(2, 3));
    assert_eq!(
        as_syms(coefficients(&f, 3, 2)),
        vec![vec![0x0003, 0x0002, 0x0005], vec![0x0002, 0x0003, 0x0004]]
    );
    assert_eq!(
        as_syms(coefficients(&f, 2, 3)),
        vec![
            vec![0x0003, 0x0002],
            vec![0x0002, 0x0003],
            vec![0x0005, 0x0004]
        ]
    );
    // Data shards of 64 bytes whose symbol 0 is 0x1234, 0xABCD, 0xFFFF (the rest 0).
    let data: Vec<Vec<u8>> = [0x1234u16, 0xABCD, 0xFFFF]
        .iter()
        .map(|&v| {
            let mut s = vec![0u8; 64];
            s[0] = v as u8;
            s[32] = (v >> 8) as u8;
            s
        })
        .collect();
    let symbol0 = |shards: Vec<Vec<u8>>| -> Vec<u16> {
        shards
            .iter()
            .map(|s| u16::from(s[0]) | u16::from(s[32]) << 8)
            .collect()
    };
    let high = encode_by_matrix(&f, 3, 2, &data);
    assert_eq!(high, library(3, 2, &data));
    assert_eq!(symbol0(high), vec![0xFFC3, 0xB9C5]);
    let low = encode_by_matrix(&f, 2, 3, &data[..2]);
    assert_eq!(low, library(2, 3, &data[..2]));
    assert_eq!(symbol0(low), vec![0xEC26, 0x55DF, 0x9998]);
    // With one recovery shard the code is the XOR of the data shards.
    let parity = encode_by_matrix(&f, 3, 1, &data);
    let xor: Vec<u8> = (0..64)
        .map(|n| data[0][n] ^ data[1][n] ^ data[2][n])
        .collect();
    assert_eq!(parity, vec![xor]);
}
