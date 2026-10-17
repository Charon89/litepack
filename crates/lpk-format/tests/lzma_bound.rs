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
/// `LzmaError` (lzma-rs refuses a distance beyond its buffer, which is the smaller of `dict_size`
/// and the bound, so a distance above the bytes produced is the same check).
#[test]
fn a_distance_error_before_the_crossing_stays_lzma_error() {
    let a = noise(21, 3000);
    let mut data = a.clone();
    data.extend_from_slice(&noise(22, 1000));
    data.extend_from_slice(&a);
    let raw = lzma_raw(&lzma_options(6, 1 << 20, 3, 0, 2), &data);
    // The match at 4000 reaches back 4000 > 2048; the bound (6000) would be crossed later.
    let r = reason(step(&lzma_params(2048, 3, 0, 2), &raw, 6000));
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
