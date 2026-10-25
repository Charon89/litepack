//! Unit tests of the coder's edge cases; the oracle replay is `tests/vp8_cross.rs`.
#![allow(clippy::unwrap_used)]

use crate::{Branch, VPXBoolReader, VPXBoolWriter, Vp8Decode, Vp8Encode};

fn ctx(n: usize) -> Vec<Branch> {
    vec![Branch::new(); n]
}

fn encode(f: impl FnOnce(&mut VPXBoolWriter<&mut Vec<u8>>)) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = VPXBoolWriter::new(&mut out).unwrap();
        f(&mut w);
        Vp8Encode::finish(&mut w).unwrap();
    }
    out
}

/// A tiny deterministic generator for the round trips (xorshift64*).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[test]
fn the_empty_stream_is_empty() {
    assert!(encode(|_| {}).is_empty());
    // and an empty stream reads as zeros without failing
    let mut r = VPXBoolReader::new(&[][..]).unwrap();
    let mut c = ctx(4);
    assert!(!r.get(&mut c[0]).unwrap());
    assert_eq!(r.get_n_bits(4, &mut c).unwrap(), 0);
}

#[test]
fn a_long_run_of_probable_symbols_emits_zero_bytes_behind_the_window() {
    // a run of zeros moves the coding position but leaves the code value zero: the tail writes
    // the bytes the reference coder has emitted (16 bits behind the position), all zero
    let mut c = Branch::new();
    let out = encode(|w| {
        for _ in 0..100_000 {
            w.put(false, &mut c).unwrap();
        }
    });
    assert!(!out.is_empty());
    assert!(out.iter().all(|&b| b == 0));
    let mut r = VPXBoolReader::new(&out[..]).unwrap();
    let mut c = Branch::new();
    for _ in 0..100_000 {
        assert!(!r.get(&mut c).unwrap());
    }
}

#[test]
fn the_tail_ends_on_a_nonzero_byte_or_the_emitted_length() {
    let mut rng = Rng(7);
    for n in 0..400 {
        let mut c = ctx(2);
        let syms: Vec<bool> = (0..n).map(|_| rng.next().is_multiple_of(5)).collect();
        let out = encode(|w| {
            for (k, &s) in syms.iter().enumerate() {
                w.put(s, &mut c[k & 1]).unwrap();
            }
        });
        let mut r = VPXBoolReader::new(&out[..]).unwrap();
        let mut c = ctx(2);
        for (k, &s) in syms.iter().enumerate() {
            assert_eq!(r.get(&mut c[k & 1]).unwrap(), s, "length {n} symbol {k}");
        }
    }
}

#[test]
fn n_bits_ignores_bits_at_or_above_the_width() {
    for w in 1..=63usize {
        let v = 0x5555_5555_5555_5555u64 & ((1u64 << w) - 1);
        let a = encode(|wr| wr.put_n_bits(v, w, &mut ctx(4)).unwrap());
        let b = encode(|wr| wr.put_n_bits(v | (1u64 << w), w, &mut ctx(4)).unwrap());
        assert_eq!(a, b, "width {w}");
    }
}

#[test]
fn n_bits_wider_than_the_contexts_uses_the_last_context() {
    let v = 0xDEAD_BEEF_0123_4567u64;
    for (w, a) in [(64usize, 4usize), (40, 4), (33, 32), (5, 1), (9, 8)] {
        let composite = encode(|wr| wr.put_n_bits(v, w, &mut ctx(a)).unwrap());
        let single = encode(|wr| {
            let mut c = ctx(a);
            for i in (0..w).rev() {
                wr.put((v >> i) & 1 != 0, &mut c[i.min(a - 1)]).unwrap();
            }
        });
        assert_eq!(composite, single, "width {w} contexts {a}");
        let mut r = VPXBoolReader::new(&composite[..]).unwrap();
        let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
        assert_eq!(r.get_n_bits(w, &mut ctx(a)).unwrap(), v & mask);
    }
}

#[test]
fn unary_is_ones_then_a_zero_with_the_last_context_reused() {
    for a in [1usize, 2, 3, 32, 33] {
        for v in 0..=a {
            let composite = encode(|wr| wr.put_unary_encoded(v, &mut ctx(a)).unwrap());
            let single = encode(|wr| {
                let mut c = ctx(a);
                for i in 0..v {
                    wr.put(true, &mut c[i.min(a - 1)]).unwrap();
                }
                wr.put(false, &mut c[v.min(a - 1)]).unwrap();
            });
            assert_eq!(composite, single, "value {v} contexts {a}");
            let mut r = VPXBoolReader::new(&composite[..]).unwrap();
            assert_eq!(r.get_unary_encoded(&mut ctx(a)).unwrap(), v);
        }
    }
}

#[test]
fn invalid_arguments_are_refused() {
    let mut out = Vec::new();
    let mut w = VPXBoolWriter::new(&mut out).unwrap();
    assert!(w.put_unary_encoded(0, &mut ctx(0)).is_err());
    assert!(w.put_n_bits(1, 65, &mut ctx(4)).is_err());
    assert!(w.put_n_bits(1, 1, &mut ctx(0)).is_err());
    // a width of 0 codes nothing
    w.put_n_bits(1, 0, &mut ctx(4)).unwrap();

    let mut r = VPXBoolReader::new(&[][..]).unwrap();
    assert!(r.get_n_bits(65, &mut ctx(4)).is_err());
    assert!(r.get_n_bits(1, &mut ctx(0)).is_err());
    assert!(r.get_unary_encoded(&mut ctx(0)).is_err());
    assert_eq!(r.get_n_bits(0, &mut ctx(0)).unwrap(), 0);
}

#[test]
fn unary_values_above_the_contexts_reuse_the_last_context() {
    for (a, v) in [(4usize, 7usize), (1, 40), (3, 100)] {
        let composite = encode(|wr| wr.put_unary_encoded(v, &mut ctx(a)).unwrap());
        let single = encode(|wr| {
            let mut c = ctx(a);
            for i in 0..v {
                wr.put(true, &mut c[i.min(a - 1)]).unwrap();
            }
            wr.put(false, &mut c[a - 1]).unwrap();
        });
        assert_eq!(composite, single, "value {v} contexts {a}");
        let mut r = VPXBoolReader::new(&composite[..]).unwrap();
        assert_eq!(r.get_unary_encoded(&mut ctx(a)).unwrap(), v);
    }
}

#[test]
fn unary_on_an_exhausted_stream_ends() {
    let mut r = VPXBoolReader::new(&[0x7Fu8, 0xFF, 0xFF][..]).unwrap();
    let mut c = ctx(2);
    for _ in 0..10 {
        r.get_unary_encoded(&mut c).unwrap();
    }
}

#[test]
fn a_stream_whose_marker_symbol_is_one_is_read_on() {
    // byte 0 at 0x80 or more decodes the marker as one; the reader discards it and goes on
    // decoding from the state that symbol left
    for first in [0x80u8, 0xC3, 0xFF] {
        let bytes = [first, 0x5A, 0x00, 0xE7, 0x11];
        let mut r = VPXBoolReader::new(&bytes[..]).unwrap();
        let mut c = ctx(8);
        let got: Vec<u64> = (0..4).map(|_| r.get_n_bits(8, &mut c).unwrap()).collect();
        let mut r2 = VPXBoolReader::raw(&bytes[..]);
        assert!(
            r2.get_bit(&mut Branch::new()).unwrap(),
            "marker of {first:#x} is one"
        );
        let mut c2 = ctx(8);
        let want: Vec<u64> = (0..4).map(|_| r2.get_n_bits(8, &mut c2).unwrap()).collect();
        assert_eq!(got, want);
    }
    let mut r = VPXBoolReader::raw(&[0x7Fu8][..]);
    assert!(!r.get_bit(&mut Branch::new()).unwrap());
}

#[test]
fn a_bounded_unary_refuses_a_run_past_its_maximum() {
    let out = encode(|wr| {
        let mut c = ctx(32);
        wr.put_unary_encoded(70, &mut c).unwrap();
        wr.put_unary_encoded(65, &mut c).unwrap();
    });
    let mut r = VPXBoolReader::new(&out[..]).unwrap();
    assert!(r.get_unary_encoded_max(&mut ctx(32), 65).is_err());
    let mut r = VPXBoolReader::new(&out[..]).unwrap();
    let mut c = ctx(32);
    assert_eq!(r.get_unary_encoded_max(&mut c, 70).unwrap(), 70);
    assert_eq!(r.get_unary_encoded_max(&mut c, 65).unwrap(), 65);
    // a long run of ones: the bound stops it after max + 1 symbols
    let out = encode(|wr| wr.put_unary_encoded(100_000, &mut ctx(1)).unwrap());
    let mut r = VPXBoolReader::new(&out[..]).unwrap();
    assert!(r.get_unary_encoded_max(&mut ctx(1), 65).is_err());
}

#[test]
fn a_field_wider_than_64_bits_is_refused() {
    let mut r = VPXBoolReader::new(&[0x12u8, 0x34][..]).unwrap();
    assert!(r.get_n_bits(65, &mut ctx(32)).is_err());
    assert!(r.get_n_bits(64, &mut ctx(32)).is_ok());
}

#[test]
fn random_mixed_streams_round_trip() {
    let mut rng = Rng(0x4C50_4B56_5038);
    for _ in 0..200 {
        let n = (rng.next() % 3000) as usize;
        let mut ops = Vec::new();
        for _ in 0..n {
            let kind = rng.next() % 3;
            let v = rng.next();
            ops.push((kind, v, (rng.next() % 64) as usize + 1));
        }
        let skew = rng.next() % 4;
        let out = encode(|wr| {
            let mut one = Branch::new();
            let mut arr = ctx(32);
            for &(kind, v, w) in &ops {
                match kind {
                    0 => wr.put(v % (skew + 2) == 0, &mut one).unwrap(),
                    1 => wr.put_n_bits(v, w, &mut arr).unwrap(),
                    _ => wr.put_unary_encoded((v % 33) as usize, &mut arr).unwrap(),
                }
            }
        });
        let mut r = VPXBoolReader::new(&out[..]).unwrap();
        let mut one = Branch::new();
        let mut arr = ctx(32);
        for &(kind, v, w) in &ops {
            match kind {
                0 => assert_eq!(r.get(&mut one).unwrap(), v % (skew + 2) == 0),
                1 => {
                    let mask = if w == 64 { u64::MAX } else { (1u64 << w) - 1 };
                    assert_eq!(r.get_n_bits(w, &mut arr).unwrap(), v & mask);
                }
                _ => assert_eq!(r.get_unary_encoded(&mut arr).unwrap(), (v % 33) as usize),
            }
        }
    }
}

#[test]
fn carries_propagate_through_ff_bytes() {
    // highly probable ones followed by improbable symbols make long 0xFF runs and carries
    let mut rng = Rng(99);
    for _ in 0..50 {
        let syms: Vec<bool> = (0..20_000)
            .map(|_| !rng.next().is_multiple_of(1000))
            .collect();
        let out = encode(|wr| {
            let mut c = Branch::new();
            for &s in &syms {
                wr.put(s, &mut c).unwrap();
            }
        });
        let mut r = VPXBoolReader::new(&out[..]).unwrap();
        let mut c = Branch::new();
        for &s in &syms {
            assert_eq!(r.get(&mut c).unwrap(), s);
        }
    }
}
