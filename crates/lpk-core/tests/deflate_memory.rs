//! The `deflate-reconstruct` memory term (`lpk_core::peel::deflate::memory_term`) against the
//! peak heap use of the patched recreation, measured by a counting global allocator (review B1
//! of E2-10 C1). Its own test binary, so no other test allocates meanwhile.
//!
//! The worst case of 0.7.6's layout is one block holding a token per plain byte: a fixed-Huffman
//! block of nothing but literals (a valid stream; a crafted `TokenCount` cannot reserve more,
//! the reservation is bounded by the input). An ordinary stream (zlib level 6 text) is measured
//! too. What the term must cover: the step's input (the previous step's output, `plain_len`
//! bytes, held by the reader) plus everything the recreation allocates beyond its output, whose
//! buffer (`original_len + 8` bytes, at most the block buffer) is the block's own output.
#![allow(clippy::unwrap_used)]

use std::io::Write;

use lpk_core::peel::deflate::{analyse_stream, memory_term, preflate_config, StreamOutcome};
use lpk_procstat_sys::alloc::CountingAlloc;
use preflate_rs::recreate_whole_deflate_stream_limited;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

fn text(n: usize) -> Vec<u8> {
    let words = [
        "alpha ", "bravo ", "charlie ", "delta ", "echo ", "foxtrot ",
    ];
    let mut x = 7u32;
    let mut out = Vec::with_capacity(n + 16);
    while out.len() < n {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(words[(x >> 24) as usize % words.len()].as_bytes());
    }
    out.truncate(n);
    out
}

/// LSB-first bit packer for a hand-made Deflate stream.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, v: u32, len: u32) {
        self.acc |= u64::from(v) << self.n;
        self.n += len;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// A Huffman code, most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        let rev = code.reverse_bits() >> (32 - len);
        self.put(rev, len);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// One final fixed-Huffman block of literals only (RFC 1951, 3.2.6).
fn literals_only(plain: &[u8]) -> Vec<u8> {
    let mut b = Bits::default();
    b.put(1, 1); // BFINAL
    b.put(1, 2); // BTYPE = fixed Huffman
    for &c in plain {
        if c < 144 {
            b.code(0x30 + u32::from(c), 8);
        } else {
            b.code(0x190 + u32::from(c) - 144, 9);
        }
    }
    b.code(0, 7); // end of block
    b.finish()
}

fn zlib_level6(plain: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(plain).unwrap();
    e.finish().unwrap()
}

/// Peak bytes allocated by the limited recreation beyond what was live before it.
fn measure(name: &str, plain: &[u8], stream: &[u8]) -> (u64, u64) {
    let corr = match analyse_stream(stream, None, &preflate_config(64 * 1024 * 1024)) {
        StreamOutcome::Done {
            corrections,
            deflate_len,
            ..
        } => {
            assert_eq!(deflate_len, stream.len(), "{name}");
            corrections
        }
        other => panic!("{name}: {other:?}"),
    };
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let out = recreate_whole_deflate_stream_limited(plain, &corr, stream.len()).unwrap();
    let peak = (ALLOC.peak() - before) as u64;
    assert_eq!(out, stream, "{name}");
    let plain_len = plain.len() as u64;
    // The step's input is live during the whole recreation (held by the reader); the output
    // buffer is the block's own (`original_len + 8` bytes at most).
    let needed = plain_len + peak - (stream.len() as u64 + 8).min(peak);
    let term = memory_term(plain_len);
    println!(
        "{name}: plain {plain_len}, peak {peak} ({:.2} x plain), needed {needed} ({:.2} x plain), term {term}",
        peak as f64 / plain_len as f64,
        needed as f64 / plain_len as f64
    );
    assert!(needed <= term, "{name}: needed {needed} > term {term}");
    (needed, term)
}

#[test]
fn recreation_peak_is_within_the_memory_term() {
    let mib = 1024 * 1024;
    // The worst layout: one block, a literal token per plain byte.
    let plain = noise(8 * mib, 3);
    let stream = literals_only(&plain);
    let (needed, term) = measure("literals-only block", &plain, &stream);
    // The term leaves at least the fixed allowance's margin on the worst case's per-byte part.
    assert!(term - needed >= 8 * mib as u64, "margin {}", term - needed);
    // An ordinary stream.
    let plain = text(8 * mib);
    let stream = zlib_level6(&plain);
    measure("zlib level 6 text", &plain, &stream);
}
