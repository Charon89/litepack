//! The entropy stage for BWT output (candidate (b) of E2-15): bzip2's structure with an
//! arithmetic coder.
//!
//! 1. Move-to-front over the byte values (the list starts as 0, 1, ..., 255).
//! 2. Zero-run coding: a run of `L` rank-0 values becomes the bijective base-2 digits of `L`,
//!    least significant first, as `RUNA` (digit 1) and `RUNB` (digit 2); a rank `r >= 1` becomes
//!    the symbol `r + 1`. Alphabet: 257 symbols.
//! 3. Each symbol is binarised and coded with adaptive bit models, the context being the
//!    previous symbol (order 1, bucketed):
//!    * "is it a run digit?"; if so, "is it `RUNB`?";
//!    * otherwise `v = r - 1` in `0..=254`: the bit length group `g = floor(log2(v + 1))` in
//!      unary (`g` ones, then a zero unless `g == 7`), then the `g` low bits of `v + 1` through a
//!      binary tree of models per group (also in the previous symbol's context bucket).
//!
//! The stream carries no length and no end symbol: the decoder is told the block length and
//! stops after exactly that many bytes (a run whose digits would pass it is an error).

use crate::rc::{Bit, Decoder, Encoder};
use crate::BwtError;

const RUNA: u16 = 0;
const RUNB: u16 = 1;
/// Context buckets of the previous symbol.
const CTX: usize = 8;
/// Bit length groups of `v + 1` (1 to 255).
const GROUPS: usize = 8;
/// Context buckets for the mantissa trees (coarser than [`CTX`]).
const MCTX: usize = 3;

fn bucket(prev: u16) -> usize {
    match prev {
        0..=3 => usize::from(prev),
        4..=5 => 4,
        6..=9 => 5,
        10..=17 => 6,
        _ => 7,
    }
}

fn mbucket(prev: u16) -> usize {
    match prev {
        0..=2 => 0,
        3..=9 => 1,
        _ => 2,
    }
}

/// All the adaptive models of one stream.
struct Models {
    is_run: [Bit; CTX],
    is_runb: [[Bit; CTX]; 2],
    group: [[Bit; GROUPS]; CTX],
    mantissa: Vec<Bit>,
}

impl Models {
    fn new() -> Self {
        Models {
            is_run: [Bit::default(); CTX],
            is_runb: [[Bit::default(); CTX]; 2],
            group: [[Bit::default(); GROUPS]; CTX],
            // Per mantissa context and group, a tree of up to 128 nodes.
            mantissa: vec![Bit::default(); MCTX * GROUPS * 128],
        }
    }
}

/// Where the RUNB model is chosen: whether the previous symbol was a run digit too (a later
/// digit of the same run).
fn runb_slot(prev: u16) -> usize {
    usize::from(prev <= RUNB)
}

fn put_symbol(e: &mut Encoder, m: &mut Models, prev: u16, sym: u16) {
    let c = bucket(prev);
    if sym <= RUNB {
        e.encode(&mut m.is_run[c], true);
        e.encode(&mut m.is_runb[runb_slot(prev)][c], sym == RUNB);
        return;
    }
    e.encode(&mut m.is_run[c], false);
    let v1 = u32::from(sym - 1); // v + 1, in 1..=255
    let g = (31 - v1.leading_zeros()) as usize; // 0..=7
    for i in 0..g {
        e.encode(&mut m.group[c][i], true);
    }
    if g < GROUPS - 1 {
        e.encode(&mut m.group[c][g], false);
    }
    let base = (mbucket(prev) * GROUPS + g) * 128;
    let mut node = 1usize;
    for i in (0..g).rev() {
        let bit = (v1 >> i) & 1 == 1;
        e.encode(&mut m.mantissa[base + node], bit);
        node = (node << 1) | usize::from(bit);
    }
}

fn get_symbol(d: &mut Decoder<'_>, m: &mut Models, prev: u16) -> u16 {
    let c = bucket(prev);
    if d.decode(&mut m.is_run[c]) {
        return if d.decode(&mut m.is_runb[runb_slot(prev)][c]) {
            RUNB
        } else {
            RUNA
        };
    }
    let mut g = 0usize;
    while g < GROUPS - 1 && d.decode(&mut m.group[c][g]) {
        g += 1;
    }
    let base = (mbucket(prev) * GROUPS + g) * 128;
    let mut node = 1usize;
    for _ in 0..g {
        let bit = d.decode(&mut m.mantissa[base + node]);
        node = (node << 1) | usize::from(bit);
    }
    // node is v + 1 (the leading one is the group's implicit bit), in 1..=255.
    node as u16 + 1
}

/// The move-to-front ranks of `data`.
pub fn mtf(data: &[u8]) -> Vec<u8> {
    let mut list: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        let mut r = 0usize;
        while list[r] != b {
            r += 1;
        }
        list.copy_within(0..r, 1);
        list[0] = b;
        out.push(r as u8);
    }
    out
}

/// The bytes whose move-to-front ranks are `ranks`.
pub fn unmtf(ranks: &[u8]) -> Vec<u8> {
    let mut list: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut out = Vec::with_capacity(ranks.len());
    for &r in ranks {
        let r = usize::from(r);
        let b = list[r];
        list.copy_within(0..r, 1);
        list[0] = b;
        out.push(b);
    }
    out
}

/// Zero-run coding of MTF ranks into the 257-symbol alphabet.
pub fn zero_runs(ranks: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(ranks.len() / 2);
    let mut run = 0u64;
    let flush = |run: &mut u64, out: &mut Vec<u16>| {
        while *run > 0 {
            if *run & 1 == 1 {
                out.push(RUNA);
                *run = (*run - 1) / 2;
            } else {
                out.push(RUNB);
                *run = (*run - 2) / 2;
            }
        }
    };
    for &r in ranks {
        if r == 0 {
            run += 1;
        } else {
            flush(&mut run, &mut out);
            out.push(u16::from(r) + 1);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Code a BWT (or any bytes) with the stage.
pub fn encode(bwt: &[u8]) -> Vec<u8> {
    let symbols = zero_runs(&mtf(bwt));
    let mut e = Encoder::new(bwt.len() / 3 + 16);
    let mut m = Models::new();
    let mut prev = RUNA;
    for &s in &symbols {
        put_symbol(&mut e, &mut m, prev, s);
        prev = s;
    }
    e.finish()
}

/// Decode exactly `len` bytes from `data`.
pub fn decode(data: &[u8], len: usize) -> Result<Vec<u8>, BwtError> {
    let mut d = Decoder::new(data);
    let mut m = Models::new();
    let mut list: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut out = Vec::with_capacity(len);
    let mut prev = RUNA;
    // The pending run: its value so far and the weight of the next digit.
    let (mut run, mut weight) = (0usize, 1usize);
    while out.len() + run < len {
        let s = get_symbol(&mut d, &mut m, prev);
        if d.overrun() {
            return Err(BwtError::Corrupt("stream ends early"));
        }
        prev = s;
        if s <= RUNB {
            let digit = usize::from(s) + 1;
            run = weight
                .checked_mul(digit)
                .and_then(|x| x.checked_add(run))
                .filter(|&r| out.len() + r <= len)
                .ok_or(BwtError::Corrupt("run passes the block end"))?;
            weight = weight.saturating_mul(2);
            continue;
        }
        if run > 0 {
            out.resize(out.len() + run, list[0]);
            run = 0;
            weight = 1;
            if out.len() >= len {
                return Err(BwtError::Corrupt("symbol after the block end"));
            }
        }
        let r = usize::from(s - 1);
        let b = list[r];
        list.copy_within(0..r, 1);
        list[0] = b;
        out.push(b);
    }
    out.resize(out.len() + run, list[0]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed | 1;
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        }
    }

    #[test]
    fn known_answers_mtf_and_runs() {
        assert_eq!(mtf(b"bananaaa"), vec![98, 98, 110, 1, 1, 1, 0, 0]);
        assert_eq!(unmtf(&[98, 98, 110, 1, 1, 1, 0, 0]), b"bananaaa");
        // Runs 1..=6: A, B, AA, BA, AB, BB (bzip2's bijective base 2, least significant first).
        let runs = |n: usize| zero_runs(&vec![0u8; n]);
        assert_eq!(runs(1), vec![RUNA]);
        assert_eq!(runs(2), vec![RUNB]);
        assert_eq!(runs(3), vec![RUNA, RUNA]);
        assert_eq!(runs(4), vec![RUNB, RUNA]);
        assert_eq!(runs(5), vec![RUNA, RUNB]);
        assert_eq!(runs(6), vec![RUNB, RUNB]);
        assert_eq!(zero_runs(&[0, 0, 3, 0, 255]), vec![RUNB, 4, RUNA, 256]);
    }

    #[test]
    fn known_answer_stream_is_stable() {
        // The coded form of a fixed input: a change of the coder shows up here.
        let enc = encode(b"annbaa");
        assert_eq!(decode(&enc, 6).unwrap(), b"annbaa");
        assert_eq!(enc, vec![0, 126, 133, 122, 234, 64, 47, 73, 48, 132, 0]);
        assert_eq!(encode(b""), vec![0, 0, 0, 0, 0]);
        assert_eq!(decode(&encode(b""), 0).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn property_round_trip_on_many_shapes() {
        let mut r = rng(42);
        for case in 0..400 {
            let len = (r() % 5000) as usize;
            let data: Vec<u8> = match case % 5 {
                0 => (0..len).map(|_| r() as u8).collect(),
                1 => (0..len).map(|_| (r() % 3) as u8).collect(),
                2 => vec![(r() % 256) as u8; len],
                3 => {
                    // Long runs of a few values (BWT-like).
                    let mut v = Vec::new();
                    while v.len() < len {
                        let n = (r() % 300) as usize + 1;
                        v.extend(std::iter::repeat_n((r() % 5) as u8 + 97, n));
                    }
                    v.truncate(len);
                    v
                }
                _ => (0..len)
                    .map(|i| {
                        if r().is_multiple_of(7) {
                            r() as u8
                        } else {
                            (i % 251) as u8
                        }
                    })
                    .collect(),
            };
            let enc = encode(&data);
            assert_eq!(decode(&enc, data.len()).unwrap(), data, "case {case}");
            assert_eq!(unmtf(&mtf(&data)), data);
        }
    }

    #[test]
    fn a_run_longer_than_any_digit_count_round_trips() {
        let mut data = vec![7u8; 3_000_001];
        data.push(9);
        data.extend(vec![9u8; 1 << 20]);
        let enc = encode(&data);
        assert!(enc.len() < 200, "{}", enc.len());
        assert_eq!(decode(&enc, data.len()).unwrap(), data);
    }

    #[test]
    fn wrong_length_and_damage_are_errors_or_other_bytes() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 17) as u8 * 3).collect();
        let enc = encode(&data);
        // Asking for more than was coded runs out of stream.
        assert!(decode(&enc, data.len() * 4).is_err());
        assert!(decode(&[], 10).is_err());
        let mut r = rng(5);
        for _ in 0..50 {
            let mut bad = enc.clone();
            let i = (r() as usize) % bad.len();
            bad[i] ^= 1 << (r() % 8);
            if let Ok(v) = decode(&bad, data.len()) {
                assert_eq!(v.len(), data.len());
            }
        }
    }
}
