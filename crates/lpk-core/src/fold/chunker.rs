//! Content-defined chunking for the writer: FastCDC over the `fastcdc` crate.

use fastcdc::v2020::{
    FastCDC, AVERAGE_MAX, AVERAGE_MIN, MAXIMUM_MAX, MAXIMUM_MIN, MINIMUM_MAX, MINIMUM_MIN,
};
use lpk_format::Chunker;

use crate::error::CoreError;

/// Smallest chunk of the default cut (bytes).
pub const MIN_CHUNK: usize = 4 * 1024;
/// Average chunk of the default cut (bytes).
pub const AVG_CHUNK: usize = 64 * 1024;
/// Largest chunk of the default cut (bytes); the writer's `chunk_size`.
pub const MAX_CHUNK: usize = 512 * 1024;

/// FastCDC (the crate's 2020 variant, normalization level 1) as a streaming [`Chunker`].
///
/// A cut depends only on the bytes from the start of its chunk up to the maximum chunk size
/// away, so the chunker cuts a chunk as soon as that many bytes are known and holds back less
/// than the maximum; at `eof` everything is cut. The result equals the crate's iterator over
/// the whole file, however the bytes are split into calls. Memory: the held-back bytes (under
/// the maximum chunk size) are copied; the bytes of a call are cut in place and only their
/// uncut tail (under the maximum) is copied.
#[derive(Debug, Clone)]
pub struct FastCdcChunker {
    min: usize,
    avg: usize,
    max: usize,
    held: Vec<u8>,
}

impl FastCdcChunker {
    /// A chunker with the given sizes in bytes. The crate's limits apply (minimum 64 to 1 MiB,
    /// average 256 B to 4 MiB, maximum 1 KiB to 16 MiB, all even) with `min <= avg <= max`.
    pub fn new(min: usize, avg: usize, max: usize) -> Result<Self, CoreError> {
        let bad = |what: &str| Err(CoreError::InvalidOption(format!("chunker: {what}")));
        if !(MINIMUM_MIN..=MINIMUM_MAX).contains(&min) {
            return bad("minimum size out of range");
        }
        if !(AVERAGE_MIN..=AVERAGE_MAX).contains(&avg) {
            return bad("average size out of range");
        }
        if !(MAXIMUM_MIN..=MAXIMUM_MAX).contains(&max) {
            return bad("maximum size out of range");
        }
        if !min.is_multiple_of(2) || !avg.is_multiple_of(2) || !max.is_multiple_of(2) {
            return bad("sizes must be even");
        }
        if !(min <= avg && avg <= max) {
            return bad("sizes must satisfy minimum <= average <= maximum");
        }
        Ok(FastCdcChunker {
            min,
            avg,
            max,
            held: Vec::new(),
        })
    }

    /// The default cut: minimum 4 KiB, average 64 KiB, maximum 512 KiB.
    pub fn standard() -> Self {
        FastCdcChunker {
            min: MIN_CHUNK,
            avg: AVG_CHUNK,
            max: MAX_CHUNK,
            held: Vec::new(),
        }
    }

    /// The largest chunk this chunker makes (the writer's `chunk_size`).
    pub fn max(&self) -> usize {
        self.max
    }
}

impl Chunker for FastCdcChunker {
    fn feed(&mut self, bytes: &[u8], eof: bool) -> Vec<usize> {
        let mut cuts = Vec::new();
        // Stream position where `held` starts.
        let mut base = 0usize;
        let mut off = 0usize;
        // First the held-back tail: top it up to the maximum with the new bytes and cut.
        while !self.held.is_empty() {
            let take = self
                .max
                .saturating_sub(self.held.len())
                .min(bytes.len() - off);
            self.held.extend_from_slice(&bytes[off..off + take]);
            off += take;
            if self.held.len() < self.max && !(eof && off == bytes.len()) {
                return cuts;
            }
            let (_, end) =
                FastCDC::new(&self.held, self.min, self.avg, self.max).cut(0, self.held.len());
            cuts.push(base + end);
            base += end;
            self.held.drain(..end);
        }
        // Then the rest of the call, in place.
        let rest = &bytes[off..];
        let cdc = FastCDC::new(rest, self.min, self.avg, self.max);
        let mut start = 0usize;
        loop {
            let remaining = rest.len() - start;
            if remaining == 0 || (!eof && remaining < self.max) {
                break;
            }
            let (_, end) = cdc.cut(start, remaining);
            cuts.push(base + end);
            start = end;
        }
        if !eof {
            self.held.extend_from_slice(&rest[start..]);
        }
        cuts
    }

    fn reset(&mut self) {
        self.held.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut v = Vec::with_capacity(len);
        while v.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.truncate(len);
        v
    }

    /// The crate's iterator over the whole buffer: the cut positions.
    fn reference(data: &[u8]) -> Vec<usize> {
        FastCDC::new(data, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK)
            .map(|c| c.offset + c.length)
            .collect()
    }

    /// Stream `data` through `feed` in pieces of the given sizes (cycled); absolute positions.
    fn streamed(data: &[u8], sizes: &[usize]) -> Vec<usize> {
        let mut c = FastCdcChunker::standard();
        let mut out = Vec::new();
        let (mut at, mut base, mut i) = (0usize, 0usize, 0usize);
        while at < data.len() {
            let n = sizes[i % sizes.len()].min(data.len() - at);
            i += 1;
            let cuts = c.feed(&data[at..at + n], false);
            at += n;
            for cut in cuts {
                out.push(base + cut);
            }
            if let Some(&last) = out.last() {
                base = last;
            }
            assert!(c.held.len() <= MAX_CHUNK, "held back more than the maximum");
        }
        for cut in c.feed(&[], true) {
            out.push(base + cut);
        }
        out
    }

    #[test]
    fn streaming_in_odd_pieces_equals_one_call_and_the_crates_iterator() {
        let data = noise(7, 3_000_000);
        let want = reference(&data);
        assert!(want.len() > 10);
        let mut whole = FastCdcChunker::standard();
        assert_eq!(whole.feed(&data, true), want);
        for sizes in [
            vec![1usize],
            vec![4097],
            vec![65_537, 3, 999_983],
            vec![524_288],
            vec![1_000_000],
        ] {
            if sizes == [1] {
                // Byte by byte on a prefix only (quadratic otherwise).
                let part = &data[..40_000];
                assert_eq!(streamed(part, &sizes), reference(part));
                continue;
            }
            assert_eq!(streamed(&data, &sizes), want, "pieces {sizes:?}");
        }
    }

    #[test]
    fn chunk_sizes_respect_the_limits() {
        let data = noise(9, 3_000_000);
        let cuts = reference(&data);
        let mut c = FastCdcChunker::standard();
        let got = c.feed(&data, true);
        assert_eq!(got, cuts);
        let mut prev = 0;
        for (i, &cut) in got.iter().enumerate() {
            assert!(cut - prev <= MAX_CHUNK);
            if i + 1 < got.len() {
                assert!(cut - prev >= MIN_CHUNK);
            }
            prev = cut;
        }
        assert_eq!(prev, data.len());
        // All-zero data runs into the maximum.
        let zeros = vec![0u8; 3 * MAX_CHUNK + 10];
        let got = FastCdcChunker::standard().feed(&zeros, true);
        assert_eq!(got, reference(&zeros));
        assert!(got.windows(2).all(|w| w[1] - w[0] <= MAX_CHUNK));
    }

    #[test]
    fn short_and_empty_files() {
        let mut c = FastCdcChunker::standard();
        assert!(c.feed(&[], true).is_empty());
        let small = noise(1, MIN_CHUNK - 1);
        assert_eq!(c.feed(&small, true), vec![MIN_CHUNK - 1]);
        // Fed in two calls: held back, then cut at eof.
        assert!(c.feed(&small[..100], false).is_empty());
        assert_eq!(c.feed(&small[100..], true), vec![MIN_CHUNK - 1]);
        // A file of exactly the minimum, and one just above it.
        assert_eq!(c.feed(&noise(2, MIN_CHUNK), true), vec![MIN_CHUNK]);
        let above = noise(3, MIN_CHUNK + 1);
        assert_eq!(c.feed(&above, true), reference(&above));
    }

    #[test]
    fn reset_forgets_the_previous_file() {
        let mut c = FastCdcChunker::standard();
        assert!(c.feed(&noise(5, 1000), false).is_empty());
        c.reset();
        let data = noise(6, 200_000);
        assert_eq!(c.feed(&data, true), reference(&data));
    }

    #[test]
    fn the_sizes_are_validated() {
        assert!(FastCdcChunker::new(4096, 65_536, 524_288).is_ok());
        for (a, b, c) in [
            (10, 65_536, 524_288),
            (4096, 100, 524_288),
            (4096, 65_536, 100),
            (4096, 65_536, 20_000_000),
            (4097, 65_536, 524_288),
            (8192, 4096, 524_288),
            (4096, 65_536, 32_768),
        ] {
            assert!(
                matches!(
                    FastCdcChunker::new(a, b, c),
                    Err(CoreError::InvalidOption(_))
                ),
                "{a} {b} {c}"
            );
        }
    }
}
