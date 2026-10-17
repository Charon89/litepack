//! Spec section 8, lzma rules 3 and 6 (task C fix round, R13): a non-final `lzma` step whose
//! output would cross its bound is `PayloadTooLarge` whenever the crossing comes before any other
//! error in the stream, whatever lzma-rs's buffer flush timing; an error at an earlier position
//! keeps its class.
#![allow(clippy::unwrap_used)]

mod common;

use common::{lzma_options, lzma_params, lzma_raw};
use lpk_format::{FormatError, LzmaDecoder, PrimitiveDecoder, Resources};

/// Pseudo-random bytes (xorshift64): incompressible, so a repeat is one long match.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 32) as u8
        })
        .collect()
}

fn reason(r: Result<Vec<u8>, FormatError>) -> String {
    match r.unwrap_err() {
        FormatError::LzmaError { reason } => reason,
        e => panic!("unexpected {e:?}"),
    }
}

fn step(p: &[u8], raw: &[u8], bound: u64) -> Result<Vec<u8>, FormatError> {
    LzmaDecoder.decode_step(p, raw, bound, false, &Resources::default())
}

fn assert_over(r: Result<Vec<u8>, FormatError>, bound: u64) {
    match r {
        Err(FormatError::PayloadTooLarge { len, max }) => {
            assert_eq!(max, bound);
            assert!(len > bound, "{len}");
        }
        other => panic!("bound {bound}: {other:?}"),
    }
}

/// (a) The reviewer's probe: a 64 KiB dictionary, 30000 random bytes written twice (one match at
/// distance 30000). Below the match's distance the first pass meets the match before the sink
/// sees the crossing; the class is still `PayloadTooLarge`.
#[test]
fn crossing_the_bound_before_a_far_match_is_payload_too_large() {
    let mut data = noise(7, 30_000);
    data.extend_from_slice(&data.clone());
    let p = lzma_params(1 << 16, 3, 0, 2);
    let raw = lzma_raw(&lzma_options(6, 1 << 16, 3, 0, 2), &data);
    assert_over(step(&p, &raw, 20_000), 20_000);
    assert_over(step(&p, &raw, 35_000), 35_000);
    assert!(step(&p, &raw, 60_000).unwrap() == data);
}

/// (b) `dict_size` below the bound (the first pass's buffer is `dict_size`); after the crossing,
/// within one buffer, a match whose distance exceeds `dict_size`: the crossing comes first.
#[test]
fn a_distance_above_dict_size_after_the_crossing_is_payload_too_large() {
    let a = noise(11, 3000);
    let mut data = a.clone();
    data.extend_from_slice(&noise(12, 3000));
    data.extend_from_slice(&a);
    // Encoded with a large dictionary (the match at 6000 reaches back 6000), declared 4096.
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    let p = lzma_params(4096, 3, 0, 2);
    assert_over(step(&p, &raw, 5000), 5000);
}

/// (c) A distance above `dict_size` at a position before the crossing keeps its class,
/// `LzmaError` (lzma-rs refuses a distance beyond its buffer, which is the smaller of the
/// dictionary size, at least 4096, and the bound, so a distance above the bytes produced is the
/// same check).
#[test]
fn a_distance_error_before_the_crossing_stays_lzma_error() {
    let a = noise(21, 3000);
    let mut data = a.clone();
    data.extend_from_slice(&noise(22, 2000));
    data.extend_from_slice(&a);
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    // The match at 5000 reaches back 5000 > 4096 (the stored 2048 raised to LzmaSpec's floor);
    // the bound (7000) would be crossed later.
    let r = reason(step(&lzma_params(2048, 3, 0, 2), &raw, 7000));
    assert!(r.contains("beyond"), "{r}");
}

/// (d) Damage before the crossing (the input ends early) stays `LzmaError`.
#[test]
fn damage_before_the_crossing_stays_lzma_error() {
    let mut data = noise(31, 30_000);
    data.extend_from_slice(&data.clone());
    let p = lzma_params(1 << 16, 3, 0, 2);
    let raw = lzma_raw(&lzma_options(6, 1 << 16, 3, 0, 2), &data);
    assert_eq!(reason(step(&p, &raw[..raw.len() / 4], 50_000)), "truncated");
}

/// (d) Real byte damage: one byte of the reviewer's probe stream changed at a quarter of its
/// length (about output position 15000, the data being incompressible). A bound before the
/// damage is crossed first (`PayloadTooLarge`); bounds after it meet the damage first
/// (`LzmaError`).
#[test]
fn byte_damage_before_the_crossing_is_lzma_error_and_after_it_payload_too_large() {
    let mut data = noise(7, 30_000);
    data.extend_from_slice(&data.clone());
    let p = lzma_params(1 << 16, 3, 0, 2);
    let mut raw = lzma_raw(&lzma_options(6, 1 << 16, 3, 0, 2), &data);
    let at = raw.len() / 4;
    raw[at] ^= 0xFF;
    assert_over(step(&p, &raw, 5000), 5000);
    for bound in [20_000u64, 59_000] {
        assert!(
            matches!(step(&p, &raw, bound), Err(FormatError::LzmaError { .. })),
            "bound {bound}: {:?}",
            step(&p, &raw, bound)
        );
    }
}

/// The LZMA range encoder (LZMA SDK `RangeEnc`), enough to write a stream by hand.
struct RangeEnc {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl RangeEnc {
    fn new() -> Self {
        RangeEnc {
            low: 0,
            range: 0xFFFF_FFFF,
            cache: 0,
            cache_size: 1,
            out: Vec::new(),
        }
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    /// One bit with a fresh probability (1024 of 2048): every context below is used once.
    fn bit(&mut self, bit: bool) {
        let bound = (self.range >> 11) * 1024;
        if bit {
            self.low += u64::from(bound);
            self.range -= bound;
        } else {
            self.range = bound;
        }
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

/// (c) A distance above the bytes produced, before the crossing: a stream whose first symbol is
/// a short rep (is_match 1, is_rep 1, is_rep_g0 0, is_rep0_long 0), which copies one byte from
/// distance rep0 + 1 = 1 with no byte produced yet. `LzmaError`, whatever the bound.
#[test]
fn a_distance_above_the_bytes_produced_before_the_crossing_is_lzma_error() {
    let mut e = RangeEnc::new();
    for b in [true, true, false, false] {
        e.bit(b);
    }
    // Further symbols the decoder never reaches.
    for i in 0..64 {
        e.bit(i % 3 == 0);
    }
    let raw = e.finish();
    assert_eq!(raw[0], 0);
    let p = lzma_params(1 << 16, 3, 0, 2);
    for bound in [1u64, 1000] {
        let r = reason(step(&p, &raw, bound));
        assert!(r.contains("beyond"), "bound {bound}: {r}");
    }
}

fn last(p: &[u8], raw: &[u8], len: u64) -> Result<Vec<u8>, FormatError> {
    LzmaDecoder.decode_step(p, raw, len, true, &Resources::default())
}

/// Text with repeats, encoded with the end-of-payload marker (the raw encoder always writes it).
fn marked(len: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let data: Vec<u8> = (0..len)
        .map(|i| b"lzma text gap "[i % 14] ^ (i / 700) as u8)
        .collect();
    let p = lzma_params(1 << 16, 3, 0, 2);
    let raw = lzma_raw(&lzma_options(6, 1 << 16, 3, 0, 2), &data);
    (data, p, raw)
}

/// Ruling 1 (gap 1): in a step that is not the last, once the output has reached the bound, input
/// that ends inside the next symbol (here the marker, cut short) is `marker required`.
#[test]
fn input_ending_inside_the_symbol_after_the_bound_is_marker_required() {
    let (data, p, raw) = marked(5000);
    let bound = data.len() as u64;
    assert!(step(&p, &raw, bound).unwrap() == data);
    // Cut inside the marker: the output still reaches the bound (in the last step the cut
    // marker's bytes are input after the position at the bound).
    for cut in 1..=3 {
        let short = &raw[..raw.len() - cut];
        assert_eq!(
            reason(last(&p, short, bound)),
            "trailing input",
            "cut {cut}"
        );
        assert_eq!(
            reason(step(&p, short, bound)),
            "marker required",
            "cut {cut}"
        );
    }
}

/// Ruling 2 (gap 2): LzmaSpec raises a `dict_size` below 4096 to 4096, so a distance in
/// (`dict_size`, 4096] is accepted; a distance above 4096 is still refused.
#[test]
fn a_dict_size_below_4096_is_raised_to_4096() {
    let a = noise(41, 2000);
    let mut data = a.clone();
    data.extend_from_slice(&noise(42, 1000));
    data.extend_from_slice(&a);
    // The repeat is one match at distance 3000, above the stored 1024 and within 4096.
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    let p = lzma_params(1024, 3, 0, 2);
    assert!(last(&p, &raw, data.len() as u64).unwrap() == data);
    assert!(step(&p, &raw, 1 << 20).unwrap() == data);
    // A distance of 5000, above 4096, stays `LzmaError`.
    let mut far = a.clone();
    far.extend_from_slice(&noise(43, 3000));
    far.extend_from_slice(&a);
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &far);
    let r = reason(last(&p, &raw, far.len() as u64));
    assert!(r.contains("beyond"), "{r}");
}

/// Ruling 10 (S4): in a step that is not the last, input after the marker is `trailing input`,
/// wherever the marker comes; in the last step a marker before the bound is `truncated` whatever
/// follows it, and input after a marker at the bound is `trailing input`.
#[test]
fn input_after_the_marker() {
    let (data, p, mut raw) = marked(5000);
    let n = data.len() as u64;
    raw.push(0);
    // Not the last step: marker at the bound, and before it.
    assert_eq!(reason(step(&p, &raw, n)), "trailing input");
    assert_eq!(reason(step(&p, &raw, n + 100)), "trailing input");
    // The last step: marker at the bound, and before it.
    assert_eq!(reason(last(&p, &raw, n)), "trailing input");
    assert_eq!(reason(last(&p, &raw, n + 100)), "truncated");
}
