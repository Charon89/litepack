//! The Reed-Solomon code of recovery frames, written from section 13 ("The code") alone.
//!
//! - The field GF(2^16) = GF(2)[x] / (x^16 + x^5 + x^3 + x^2 + 1), with log/exp tables on the
//!   generator x, and the Cantor basis b_0 .. b_15 that maps a 16-bit symbol `s` to the element
//!   e(s) (XOR of the b_n over the set bits of `s`). Arithmetic is done on elements in the
//!   polynomial representation; symbols are converted at the shard boundary.
//! - The symbol layout of a shard: 64-byte units, the first 32 bytes the low bytes of 32
//!   symbols and the last 32 their high bytes.
//! - The encoding map in its matrix form, y_j = sum over i < k of C[j][i] * d_i, with
//!   C[j][i] = w_(1 + floor(i/m)) / (w_j + w_(m+i)) in the high form (m = P(r)) and
//!   C[j][i] = w_(1 + floor(j/m)) / (w_(m+j) + w_i) in the low form (m = P(k)), where w_n = e(n).
//!   Since e is linear, w_a + w_b = w_(a XOR b).
//! - Decoding by the normative rule ("any method that inverts the encoding"): the e unknown data
//!   shards solve the e-by-e system that e known recovery shards give. The submatrix is a Cauchy
//!   matrix scaled by rows and columns (section 13, "Decoding"), so its inverse has a closed form
//!   ([`cauchy_inverse`]): `O(e)` memory and `O(e^2)` scalar work, no e-by-e matrix.
//!
//! Cost (no fast transform): encoding is `r * k * S` field multiplications, where `S` is
//! `shard_len / 2` symbols. Rebuilding `e` shards of a group with `d` real data shards is
//! `e * d * S` field multiply-adds ([`repair_work`]); a reader refuses a rebuild above
//! [`REPAIR_WORK_LIMIT`]. Memory besides the caller's shards: `e` residuals and `e` rebuilt
//! shards, plus one scratch shard (`2 * e + 1` shards, each `shard_len` bytes), within section
//! 13's threshold `(P(r) + k).next_pow2() * shard_len` up to that scratch shard since `e <= r`
//! and `e <= d <= k`. That is quadratic in the group where the reference's transform is
//! quasi-linear.

use std::sync::OnceLock;

use crate::error::{Error, Result};

/// p(x) = x^16 + x^5 + x^3 + x^2 + 1.
const POLY: u32 = 0x1002D;
/// Order of the multiplicative group.
const ORDER: usize = 65535;

/// The Cantor basis b_0 .. b_15 in the polynomial representation (section 13 table).
pub const BASIS: [u16; 16] = [
    0x0001, 0xACCA, 0x3C0E, 0x163E, 0xC582, 0xED2E, 0x914C, 0x4012, 0x6C98, 0x10D8, 0x6A72, 0xB900,
    0xFDB8, 0xFB34, 0xFF38, 0x991E,
];

struct Tables {
    /// exp[n] = x^n, for n in 0 .. 2 * ORDER (doubled so sums of two logs need no reduction).
    exp: Vec<u16>,
    /// log[a] for a != 0.
    log: Vec<u16>,
    /// Symbol to element.
    elem: Vec<u16>,
    /// Element to symbol.
    sym: Vec<u16>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut exp = vec![0u16; 2 * ORDER];
        let mut log = vec![0u16; 65536];
        let mut a: u32 = 1;
        for (n, slot) in exp.iter_mut().enumerate().take(ORDER) {
            *slot = a as u16;
            log[a as usize] = n as u16;
            a <<= 1;
            if a & 0x10000 != 0 {
                a ^= POLY;
            }
        }
        for n in ORDER..2 * ORDER {
            exp[n] = exp[n - ORDER];
        }
        let mut elem = vec![0u16; 65536];
        let mut sym = vec![0u16; 65536];
        for s in 1..65536usize {
            // e(s) = e(s without its lowest set bit) XOR b_(lowest set bit).
            let low = s.trailing_zeros() as usize;
            elem[s] = elem[s & (s - 1)] ^ BASIS[low];
        }
        for s in 0..65536usize {
            sym[elem[s] as usize] = s as u16;
        }
        Tables {
            exp,
            log,
            elem,
            sym,
        }
    })
}

/// e(s): the element (polynomial representation) a symbol denotes.
pub fn elem(s: u16) -> u16 {
    tables().elem[s as usize]
}

/// e^-1(a): the symbol of an element.
pub fn sym(a: u16) -> u16 {
    tables().sym[a as usize]
}

/// w_n = e(n), the evaluation point with symbol n.
fn w(n: u32) -> u16 {
    elem(n as u16)
}

/// Product of two elements (polynomial representation).
pub fn mul(a: u16, b: u16) -> u16 {
    if a == 0 || b == 0 {
        return 0;
    }
    let t = tables();
    t.exp[t.log[a as usize] as usize + t.log[b as usize] as usize]
}

/// Inverse of a non-zero element.
pub fn inv(a: u16) -> Option<u16> {
    if a == 0 {
        return None;
    }
    let t = tables();
    Some(t.exp[(ORDER - t.log[a as usize] as usize) % ORDER])
}

/// Product of two symbols: e^-1(e(s) * e(t)).
pub fn mul_sym(s: u16, t: u16) -> u16 {
    sym(mul(elem(s), elem(t)))
}

/// Carry-less product reduced modulo p(x), without tables (for checking the tables).
pub fn mul_slow(a: u16, b: u16) -> u16 {
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

/// P(n): the smallest power of two at least `n` (P(1) = 1, P(0) = 1).
pub fn p2(n: u32) -> u32 {
    n.max(1).next_power_of_two()
}

/// "Supported counts": k, r in 1 ..= 65536 and min(P(k), P(r)) + max(k, r) <= 65536.
pub fn supported(k: u32, r: u32) -> bool {
    if k == 0 || r == 0 || k > 65536 || r > 65536 {
        return false;
    }
    u64::from(p2(k).min(p2(r))) + u64::from(k.max(r)) <= 65536
}

/// The form of the code a pair uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// m = P(r); data at w_(m+i), recovery at w_j.
    High,
    /// m = P(k); data at w_i, recovery at w_(m+j).
    Low,
}

/// "Which form a pair uses": high when P(k) > P(r), or P(k) = P(r) and k <= r.
pub fn form(k: u32, r: u32) -> Form {
    let (pk, pr) = (p2(k), p2(r));
    if pk > pr || (pk == pr && k <= r) {
        Form::High
    } else {
        Form::Low
    }
}

/// C[j][i] as an element (polynomial representation), for a supported pair, j < r, i < k.
pub fn coeff(k: u32, r: u32, j: u32, i: u32) -> u16 {
    let (num, den) = match form(k, r) {
        Form::High => {
            let m = p2(r);
            (w(1 + i / m), w(j ^ (m + i)))
        }
        Form::Low => {
            let m = p2(k);
            (w(1 + j / m), w((m + j) ^ i))
        }
    };
    // The points are distinct, so den != 0 for a supported pair.
    inv(den).map_or(0, |d| mul(num, d))
}

/// Reads a shard's symbols as elements (the unit layout of "Shards and symbols").
pub fn shard_to_elems(shard: &[u8]) -> Vec<u16> {
    let mut out = vec![0u16; shard.len() / 2];
    shard_to_elems_into(shard, &mut out);
    out
}

/// [`shard_to_elems`] into a buffer of `shard.len() / 2` elements.
fn shard_to_elems_into(shard: &[u8], out: &mut [u16]) {
    for (unit, o) in shard
        .as_chunks::<64>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<32>().0)
    {
        for t in 0..32 {
            o[t] = elem(u16::from(unit[t]) | u16::from(unit[32 + t]) << 8);
        }
    }
}

/// Writes elements back as a shard's bytes.
pub fn elems_to_shard(v: &[u16]) -> Vec<u8> {
    let mut out = vec![0u8; v.len() * 2];
    for (unit, syms) in out
        .as_chunks_mut::<64>()
        .0
        .iter_mut()
        .zip(v.as_chunks::<32>().0)
    {
        for t in 0..32 {
            let s = sym(syms[t]);
            unit[t] = s as u8;
            unit[32 + t] = (s >> 8) as u8;
        }
    }
    out
}

/// acc += c * x, element by element.
fn mul_add(acc: &mut [u16], c: u16, x: &[u16]) {
    if c == 0 {
        return;
    }
    let t = tables();
    let lc = t.log[c as usize] as usize;
    for (a, &v) in acc.iter_mut().zip(x) {
        if v != 0 {
            *a ^= t.exp[lc + t.log[v as usize] as usize];
        }
    }
}

fn check_geometry(k: u32, r: u32, shard_len: usize) -> Result<()> {
    if !supported(k, r) {
        return Err(Error::new("RecoveryError", "unsupported shard counts"));
    }
    if shard_len == 0 || !shard_len.is_multiple_of(64) {
        return Err(Error::new(
            "RecoveryError",
            "shard length not a multiple of 64",
        ));
    }
    Ok(())
}

/// Encodes: the `r` recovery shards of a group coded with `k` data shards, of which `data` are
/// the first ones (the rest implicit zero shards). Every shard has the same length.
pub fn encode(k: u32, r: u32, data: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
    let shard_len = data.first().map_or(64, |d| d.len());
    check_geometry(k, r, shard_len)?;
    if data.len() > k as usize || data.iter().any(|d| d.len() != shard_len) {
        return Err(Error::new("RecoveryError", "bad data shards"));
    }
    let d: Vec<Vec<u16>> = data.iter().map(|s| shard_to_elems(s)).collect();
    let mut out = Vec::with_capacity(r as usize);
    for j in 0..r {
        let mut acc = vec![0u16; shard_len / 2];
        for (i, di) in d.iter().enumerate() {
            mul_add(&mut acc, coeff(k, r, j, i as u32), di);
        }
        out.push(elems_to_shard(&acc));
    }
    Ok(out)
}

/// The most field multiply-adds this reader spends on one group's rebuild (section 13, "Repair":
/// a reader may refuse, with `Refused` field `recovery group`, a group whose rebuild exceeds a
/// work bound it documents). See [`repair_work`] for the count. 2^32 admits, for example, 64
/// damaged shards in a default group (2048 shards of 65536 bytes), which takes seconds.
pub const REPAIR_WORK_LIMIT: u64 = 1 << 32;

/// The work of rebuilding `e` unknown shards of a group with `data_shards` real data shards of
/// `shard_len` bytes: `e * data_shards * (shard_len / 2)` field multiply-adds. That counts the
/// residuals (each of the `data_shards - e` known shards into each of the `e` residuals) and the
/// combination (each of the `e` residuals into each of the `e` rebuilt shards), one per symbol;
/// the `O(e^2)` scalar work of the closed-form inverse is smaller than one symbol pass (`shard_len
/// / 2` is at least 32) and is not counted.
pub fn repair_work(e: u64, data_shards: u64, shard_len: u64) -> u64 {
    e.saturating_mul(data_shards).saturating_mul(shard_len / 2)
}

/// (alpha_i, u_i): the point of data shard i and its factor in "Decoding",
/// C[j][i] = u_i * v_j / (alpha_i + beta_j); u_i is W_m(w_(m+i)) / K_m = w_(1 + floor(i/m)).
fn data_point(k: u32, r: u32, i: u32) -> (u16, u16) {
    match form(k, r) {
        Form::High => {
            let m = p2(r);
            (w(m + i), w(1 + i / m))
        }
        Form::Low => (w(i), 1),
    }
}

/// (beta_j, v_j): the point of recovery shard j and its factor; v_j is w_(1 + floor(j/m)).
fn rec_point(k: u32, r: u32, j: u32) -> (u16, u16) {
    match form(k, r) {
        Form::High => (w(j), 1),
        Form::Low => {
            let m = p2(k);
            (w(m + j), w(1 + j / m))
        }
    }
}

/// The closed-form inverse of a scaled Cauchy submatrix (see [`cauchy_inverse`]).
#[derive(Debug)]
struct CauchyInverse {
    x: Vec<u16>,
    y: Vec<u16>,
    g: Vec<u16>,
    h: Vec<u16>,
}

impl CauchyInverse {
    /// Entry [b][a] of the inverse: G_a * H_b / (x_a + y_b).
    fn at(&self, b: usize, a: usize) -> u16 {
        inv(self.x[a] ^ self.y[b]).map_or(0, |d| mul(mul(self.g[a], self.h[b]), d))
    }
}

/// The inverse of the e-by-e submatrix of C on recovery rows `rows` and data columns `cols`, in
/// closed form, as a function of (column b, row a): entry [b][a] of the inverse.
///
/// The submatrix is M[a][b] = v_a * u_b / (x_a + y_b), with x_a = beta of row a and y_b = alpha
/// of column b. Solving sum over b of M[a][b] * d_b = r_a: f(z) = sum over b of (u_b d_b) / (z +
/// y_b) is N(z) / D(z) with D(z) = prod over b of (z + y_b) and deg N < e; N(x_a) = D(x_a) r_a / v_a
/// fixes N by interpolation on the x_a, and u_b d_b is the residue of f at y_b, N(y_b) / D'(y_b).
/// That gives inverse[b][a] = G_a * H_b / (x_a + y_b), with
/// G_a = D(x_a) / (v_a * prod over c != a of (x_a + x_c)) and
/// H_b = E(y_b) / (u_b * prod over c != b of (y_b + y_c)), E(z) = prod over c of (z + x_c).
/// Memory `O(e)`, time `O(e^2)`. `None` when two points coincide (a repeated row or column).
fn cauchy_inverse(k: u32, r: u32, rows: &[u32], cols: &[u32]) -> Option<CauchyInverse> {
    let (x, v): (Vec<u16>, Vec<u16>) = rows.iter().map(|&j| rec_point(k, r, j)).unzip();
    let (y, u): (Vec<u16>, Vec<u16>) = cols.iter().map(|&i| data_point(k, r, i)).unzip();
    let mut g = Vec::with_capacity(x.len());
    for (a, &xa) in x.iter().enumerate() {
        let mut num = 1u16;
        for &yb in &y {
            num = mul(num, xa ^ yb);
        }
        let mut den = v[a];
        for (c, &xc) in x.iter().enumerate() {
            if c != a {
                den = mul(den, xa ^ xc);
            }
        }
        g.push(mul(num, inv(den)?));
    }
    let mut h = Vec::with_capacity(y.len());
    for (b, &yb) in y.iter().enumerate() {
        let mut num = 1u16;
        for &xc in &x {
            num = mul(num, yb ^ xc);
        }
        let mut den = u[b];
        for (c, &yc) in y.iter().enumerate() {
            if c != b {
                den = mul(den, yb ^ yc);
            }
        }
        h.push(mul(num, inv(den)?));
    }
    Some(CauchyInverse { x, y, g, h })
}

/// Decodes: rebuilds the unknown data shards of a group coded with `k` data shards and `r`
/// recovery shards.
///
/// `data[i]` is data shard i when known and `None` when unknown; shards at `data.len()` and
/// above (up to `k`) are implicit zero shards, always known. `recovery` holds known recovery
/// shards as `(j, bytes)` with distinct `j` (a repeated `j` is a `RecoveryError`); the first `e`
/// of them are used, where `e` is the number of unknown data shards. Returns the rebuilt shards as
/// `(i, bytes)`, in ascending `i`. The caller bounds the work with [`repair_work`].
pub fn decode(
    k: u32,
    r: u32,
    data: &[Option<&[u8]>],
    recovery: &[(u32, &[u8])],
) -> Result<Vec<(u32, Vec<u8>)>> {
    let shard_len = data
        .iter()
        .flatten()
        .map(|d| d.len())
        .chain(recovery.iter().map(|(_, d)| d.len()))
        .next()
        .unwrap_or(64);
    check_geometry(k, r, shard_len)?;
    let lens_ok = data.iter().flatten().all(|d| d.len() == shard_len)
        && recovery.iter().all(|(j, d)| d.len() == shard_len && *j < r);
    if data.len() > k as usize || !lens_ok {
        return Err(Error::new(
            "RecoveryError",
            "bad shards given to the decoder",
        ));
    }
    let unknown: Vec<u32> = (0..data.len() as u32)
        .filter(|&i| data[i as usize].is_none())
        .collect();
    let e = unknown.len();
    if e == 0 {
        return Ok(Vec::new());
    }
    if e > r as usize || recovery.len() < e {
        return Err(Error::new(
            "RecoveryError",
            format!(
                "{e} unknown shards, {} recovery shards given",
                recovery.len()
            ),
        ));
    }
    let rows = &recovery[..e];
    let row_idx: Vec<u32> = rows.iter().map(|&(j, _)| j).collect();
    let ci = cauchy_inverse(k, r, &row_idx, &unknown)
        .ok_or_else(|| Error::new("RecoveryError", "repeated recovery shard"))?;
    // Residuals: y_j + sum over known i of C[j][i] * d_i = sum over unknown u of C[j][u] * d_u.
    // One known shard at a time is converted, then added into every residual.
    let mut res: Vec<Vec<u16>> = rows.iter().map(|&(_, b)| shard_to_elems(b)).collect();
    let mut scratch = vec![0u16; shard_len / 2];
    for (i, d) in data.iter().enumerate() {
        let Some(d) = d else { continue };
        shard_to_elems_into(d, &mut scratch);
        for (acc, &j) in res.iter_mut().zip(&row_idx) {
            mul_add(acc, coeff(k, r, j, i as u32), &scratch);
        }
    }
    let mut out = Vec::with_capacity(e);
    for (b, &u) in unknown.iter().enumerate() {
        scratch.fill(0);
        for (a, ra) in res.iter().enumerate() {
            let c = ci.at(b, a);
            mul_add(&mut scratch, c, ra);
        }
        out.push((u, elems_to_shard(&scratch)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inverts a square matrix of elements by Gauss-Jordan elimination.
    fn invert(mut a: Vec<Vec<u16>>) -> Option<Vec<Vec<u16>>> {
        let n = a.len();
        let mut b: Vec<Vec<u16>> = (0..n)
            .map(|i| (0..n).map(|j| u16::from(i == j)).collect())
            .collect();
        for col in 0..n {
            let piv = (col..n).find(|&row| a[row][col] != 0)?;
            a.swap(col, piv);
            b.swap(col, piv);
            let pinv = inv(a[col][col])?;
            for x in 0..n {
                a[col][x] = mul(a[col][x], pinv);
                b[col][x] = mul(b[col][x], pinv);
            }
            for row in 0..n {
                let f = a[row][col];
                if row == col || f == 0 {
                    continue;
                }
                for x in 0..n {
                    a[row][x] ^= mul(f, a[col][x]);
                    b[row][x] ^= mul(f, b[col][x]);
                }
            }
        }
        Some(b)
    }

    fn s(v: u16) -> u16 {
        sym(v)
    }

    #[test]
    fn tables_agree_with_the_carry_less_product() {
        let mut x: u32 = 12345;
        for _ in 0..20000 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let a = (x >> 8) as u16;
            let b = (x >> 15) as u16;
            assert_eq!(mul(a, b), mul_slow(a, b));
        }
        // x generates the multiplicative group: its powers are all distinct and non-zero.
        let t = tables();
        let mut seen = vec![false; 65536];
        for n in 0..ORDER {
            let v = t.exp[n] as usize;
            assert!(v != 0 && !seen[v]);
            seen[v] = true;
        }
    }

    #[test]
    fn check_values_of_the_field() {
        assert_eq!(elem(0x0002), 0xACCA);
        assert_eq!(elem(0x8000), 0x991E);
        assert_eq!(sym(0x0002), 0x4690);
        assert_eq!(mul_sym(0x0002, 0x0002), 0x0003);
        assert_eq!(mul_sym(0x0002, 0x0003), 0x0001);
        assert_eq!(mul_sym(0x1234, 0xABCD), 0x43C1);
        assert_eq!(inv(elem(0x0002)).map(sym), Some(0x0003));
        // Cantor basis: b_0 = 1 and b_n^2 + b_n = b_(n-1).
        assert_eq!(BASIS[0], 1);
        for n in 1..16 {
            assert_eq!(mul_slow(BASIS[n], BASIS[n]) ^ BASIS[n], BASIS[n - 1]);
        }
        // e is one to one.
        let mut seen = vec![false; 65536];
        for v in 0..=u16::MAX {
            assert!(!seen[elem(v) as usize]);
            seen[elem(v) as usize] = true;
        }
    }

    #[test]
    fn subspace_identities_hold() {
        // W_m(w_j) = w_(floor(j/m)) and K_m = 1, by the products, for small m.
        for a in 0..5 {
            let m = 1u32 << a;
            for j in [0u32, 1, 5, 17, 100, 1000, 65535] {
                let mut prod = 1u16;
                for n in 0..m {
                    prod = mul(prod, w(j) ^ w(n));
                }
                assert_eq!(prod, w(j / m), "m {m} j {j}");
            }
            let mut km = 1u16;
            for n in 1..m {
                km = mul(km, w(n));
            }
            assert_eq!(km, 1);
        }
    }

    #[test]
    fn supported_and_form() {
        assert!(supported(1, 1));
        assert!(supported(12, 3));
        assert!(!supported(20000, 40000));
        assert!(!supported(0, 1));
        assert!(!supported(1, 0));
        assert!(supported(32768, 32768));
        assert!(!supported(32769, 32768));
        assert_eq!(form(3, 2), Form::High);
        assert_eq!(form(2, 3), Form::Low);
        assert_eq!(form(1, 1), Form::High);
        assert_eq!(form(3, 4), Form::High);
        assert_eq!(form(4, 3), Form::Low);
    }

    #[test]
    fn check_values_of_the_matrices() {
        let c32: Vec<Vec<u16>> = (0..2)
            .map(|j| (0..3).map(|i| s(coeff(3, 2, j, i))).collect())
            .collect();
        assert_eq!(c32, vec![vec![0x3, 0x2, 0x5], vec![0x2, 0x3, 0x4]]);
        let c23: Vec<Vec<u16>> = (0..3)
            .map(|j| (0..2).map(|i| s(coeff(2, 3, j, i))).collect())
            .collect();
        assert_eq!(c23, vec![vec![0x3, 0x2], vec![0x2, 0x3], vec![0x5, 0x4]]);
    }

    fn shard_with_symbol0(v: u16) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[0] = v as u8;
        b[32] = (v >> 8) as u8;
        b
    }

    fn symbol0(b: &[u8]) -> u16 {
        u16::from(b[0]) | u16::from(b[32]) << 8
    }

    #[test]
    fn check_values_of_the_encoding() {
        let d: Vec<Vec<u8>> = [0x1234, 0xABCD, 0xFFFF]
            .iter()
            .map(|&v| shard_with_symbol0(v))
            .collect();
        let refs: Vec<&[u8]> = d.iter().map(|v| v.as_slice()).collect();
        let y = encode(3, 2, &refs).expect("encode");
        assert_eq!(
            y.iter().map(|b| symbol0(b)).collect::<Vec<_>>(),
            [0xFFC3, 0xB9C5]
        );
        assert!(y
            .iter()
            .all(|b| b[1..32].iter().chain(&b[33..]).all(|&x| x == 0)));
        let y = encode(2, 3, &refs[..2]).expect("encode");
        assert_eq!(
            y.iter().map(|b| symbol0(b)).collect::<Vec<_>>(),
            [0xEC26, 0x55DF, 0x9998]
        );
        // r = 1 is the XOR of the data shards.
        let y = encode(3, 1, &refs).expect("encode");
        assert_eq!(symbol0(&y[0]), 0x1234 ^ 0xABCD ^ 0xFFFF);
    }

    #[test]
    fn symbol_layout_round_trips() {
        let b: Vec<u8> = (0..256u32).map(|n| (n * 7 + 3) as u8).collect();
        let e = shard_to_elems(&b);
        assert_eq!(e.len(), 128);
        // Symbol 32u + t is byte[64u + t] + 256 * byte[64u + 32 + t].
        assert_eq!(
            sym(e[32 + 5]),
            u16::from(b[64 + 5]) | u16::from(b[96 + 5]) << 8
        );
        assert_eq!(elems_to_shard(&e), b);
    }

    fn pseudo(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    fn round_trip(k: u32, r: u32, real: u32, erase: &[u32], rows: &[u32]) {
        let sl = 128;
        let d: Vec<Vec<u8>> = (0..real).map(|i| pseudo(sl, i + 7 * k + r)).collect();
        let refs: Vec<&[u8]> = d.iter().map(|v| v.as_slice()).collect();
        let y = encode(k, r, &refs).expect("encode");
        let given: Vec<Option<&[u8]>> = (0..real)
            .map(|i| (!erase.contains(&i)).then(|| d[i as usize].as_slice()))
            .collect();
        let rec: Vec<(u32, &[u8])> = rows
            .iter()
            .map(|&j| (j, y[j as usize].as_slice()))
            .collect();
        let out = decode(k, r, &given, &rec).expect("decode");
        assert_eq!(out.len(), erase.len());
        for (i, b) in out {
            assert_eq!(b, d[i as usize], "k {k} r {r} shard {i}");
        }
    }

    #[test]
    fn erasures_up_to_r_rebuild_in_both_forms() {
        // High form, implicit zero shards (12 coded, 10 real).
        round_trip(12, 3, 10, &[0, 4, 9], &[0, 1, 2]);
        round_trip(12, 3, 10, &[2, 3], &[2, 0]);
        // Low form.
        round_trip(2, 3, 2, &[0, 1], &[2, 1]);
        round_trip(5, 9, 5, &[0, 1, 2, 3, 4], &[8, 0, 3, 5, 6]);
        // P(k) > P(r): high form with several chunks of m (m = 16, k = 40).
        round_trip(40, 9, 40, &[1, 17, 33, 39], &[0, 4, 7, 8]);
        round_trip(1, 1, 1, &[0], &[0]);
        // The ties of "Which form a pair uses": P(k) = P(r) and k <= r is high, k > r is low.
        assert_eq!((form(3, 4), form(4, 3)), (Form::High, Form::Low));
        round_trip(3, 4, 3, &[0, 1, 2], &[3, 1, 0]);
        round_trip(4, 3, 4, &[0, 2, 3], &[2, 0, 1]);
        round_trip(4, 4, 4, &[0, 1, 2, 3], &[0, 1, 2, 3]);
    }

    #[test]
    fn closed_form_inverse_matches_elimination() {
        for (k, r, rows, cols) in [
            (12u32, 3u32, vec![0u32, 2, 1], vec![0u32, 5, 11]),
            (2, 3, vec![2, 0], vec![1, 0]),
            (40, 9, vec![8, 3, 0, 5], vec![39, 1, 16, 17]),
            (4, 3, vec![1, 2, 0], vec![3, 0, 2]),
        ] {
            let m: Vec<Vec<u16>> = rows
                .iter()
                .map(|&j| cols.iter().map(|&i| coeff(k, r, j, i)).collect())
                .collect();
            let want = invert(m).expect("invertible");
            let ci = cauchy_inverse(k, r, &rows, &cols).expect("closed form");
            for (b, row) in want.iter().enumerate() {
                for (a, &entry) in row.iter().enumerate() {
                    assert_eq!(ci.at(b, a), entry, "k {k} r {r} [{b}][{a}]");
                }
            }
        }
    }

    #[test]
    fn repeated_recovery_index_is_an_error() {
        let d = [pseudo(64, 1), pseudo(64, 2), pseudo(64, 3)];
        let refs: Vec<&[u8]> = d.iter().map(|v| v.as_slice()).collect();
        let y = encode(3, 2, &refs).expect("encode");
        let given = [None, None, Some(d[2].as_slice())];
        let rec = [(0, y[0].as_slice()), (0, y[0].as_slice())];
        assert_eq!(
            decode(3, 2, &given, &rec).expect_err("repeat").class,
            "RecoveryError"
        );
    }

    #[test]
    fn repair_work_expression() {
        assert_eq!(repair_work(3, 10, 1024), 3 * 10 * 512);
        assert_eq!(repair_work(u64::MAX, 2, 64), u64::MAX);
        // The hostile group of the review: 32767 unknowns of 32768 shards of 64 bytes.
        assert!(repair_work(32767, 32768, 64) > REPAIR_WORK_LIMIT);
    }

    #[test]
    fn more_than_r_unknown_is_refused() {
        let d = [pseudo(64, 1), pseudo(64, 2), pseudo(64, 3)];
        let refs: Vec<&[u8]> = d.iter().map(|v| v.as_slice()).collect();
        let y = encode(3, 1, &refs).expect("encode");
        let given = [None, None, Some(d[2].as_slice())];
        let err = decode(3, 1, &given, &[(0, y[0].as_slice())]).expect_err("too many");
        assert_eq!(err.class, "RecoveryError");
    }
}
