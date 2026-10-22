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
    /// Threads a call's cut fans out to at most (1: this thread only).
    threads: usize,
}

/// Bytes of a call each cutting thread takes (at least the maximum chunk, so
/// a chunk that crosses a piece's end ends in the next piece).
const CUT_PIECE: usize = 2 << 20;

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
            threads: 1,
        })
    }

    /// The default cut: minimum 4 KiB, average 64 KiB, maximum 512 KiB.
    pub fn standard() -> Self {
        FastCdcChunker {
            min: MIN_CHUNK,
            avg: AVG_CHUNK,
            max: MAX_CHUNK,
            held: Vec::new(),
            threads: 1,
        }
    }

    /// Cut each call on up to `threads` scoped threads (this one included;
    /// at most 1 is sequential; never more than the writer's hash threads,
    /// `MAX_HASH_THREADS`, one per [`CUT_PIECE`]). The cuts are the same: each thread cuts its
    /// piece of the call from the piece's start, and the pieces are joined in
    /// order by the rule that a cut sequence depends only on its start — the
    /// true sequence enters a piece at a known chunk start, and from the first
    /// of its cuts the piece's thread also produced, the thread's cuts are the
    /// true ones; until then the chunks are cut here, one at a time (on data
    /// where no cut ever coincides, such as a run of one byte value, the whole
    /// call is cut here, as with one thread). A thread that cannot be created
    /// leaves its piece to this thread.
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// The largest chunk this chunker makes (the writer's `chunk_size`).
    pub fn max(&self) -> usize {
        self.max
    }

    /// The cuts of `bytes` from `at` on, in place: every chunk start with at
    /// least the maximum ahead of it (or any, at `eof`) is cut, as the crate's
    /// iterator cuts it; the positions are `bytes`'s.
    fn cut_in_place(&self, bytes: &[u8], at: usize, eof: bool) -> Vec<usize> {
        let len = bytes.len();
        let can_cut = |p: usize| p < len && (eof || len - p >= self.max);
        let cdc = FastCDC::new(bytes, self.min, self.avg, self.max);
        let pieces = (len - at)
            .div_ceil(CUT_PIECE)
            .min(self.threads)
            .min(lpk_format::writer::MAX_HASH_THREADS);
        if pieces <= 1 {
            let mut cuts = Vec::new();
            let mut p = at;
            while can_cut(p) {
                p = cdc.cut(p, len - p).1;
                cuts.push(p);
            }
            return cuts;
        }
        // Piece k is `[start(k), start(k + 1))`; its thread cuts from
        // `start(k)` up to and including the first cut at or past its end.
        let piece = (len - at).div_ceil(pieces);
        let start = |k: usize| (at + k * piece).min(len);
        let cut_piece = |k: usize| -> Vec<usize> {
            let end = start(k + 1);
            let mut cuts = Vec::new();
            let mut p = start(k);
            while can_cut(p) {
                p = cdc.cut(p, len - p).1;
                cuts.push(p);
                if p >= end {
                    break;
                }
            }
            cuts
        };
        let lists: Vec<Vec<usize>> = std::thread::scope(|s| {
            let handles: Vec<_> = (1..pieces)
                .map(|k| {
                    std::thread::Builder::new()
                        .name("lpk-cut".to_string())
                        .spawn_scoped(s, move || cut_piece(k))
                        .ok()
                })
                .collect();
            let mut lists = vec![cut_piece(0)];
            for (k, h) in (1..pieces).zip(handles) {
                lists.push(match h {
                    // A cutting thread's panic is a bug; re-raised as a panic here.
                    Some(h) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
                    None => cut_piece(k),
                });
            }
            lists
        });
        // Join the pieces: `p` is always a true chunk start.
        let mut cuts = Vec::with_capacity(lists.iter().map(Vec::len).sum());
        let mut p = at;
        while can_cut(p) {
            let k = ((p - at) / piece).min(pieces - 1);
            let list = &lists[k];
            let from = if p == start(k) {
                Some(0)
            } else {
                list.binary_search(&p).ok().map(|i| i + 1)
            };
            match from {
                Some(i) if i < list.len() => {
                    cuts.extend_from_slice(&list[i..]);
                    p = list[list.len() - 1];
                }
                _ => {
                    p = cdc.cut(p, len - p).1;
                    cuts.push(p);
                }
            }
        }
        cuts
    }
}

impl Chunker for FastCdcChunker {
    fn feed(&mut self, bytes: &[u8], eof: bool) -> Vec<usize> {
        let mut cuts = Vec::new();
        // Positions are relative to the start of the held-back tail.
        let held_len = self.held.len();
        let total = held_len + bytes.len();
        // The next chunk's start.
        let mut start = 0usize;
        // The chunks that start inside the held-back tail are cut from the tail
        // topped up with what the cut may look at (at most the maximum); the
        // copy is bounded by the maximum per call, whatever the call's size.
        while start < held_len {
            let avail = total - start;
            if !eof && avail < self.max {
                break;
            }
            let need = avail.min(self.max);
            let want = start + need;
            if self.held.len() < want {
                let from = self.held.len() - held_len;
                self.held.extend_from_slice(&bytes[from..want - held_len]);
            }
            let (_, end) =
                FastCDC::new(&self.held[start..want], self.min, self.avg, self.max).cut(0, need);
            cuts.push(start + end);
            start += end;
        }
        if start < held_len {
            // Held back: the uncut tail, then the whole call.
            self.held.truncate(held_len);
            self.held.drain(..start);
            self.held.extend_from_slice(bytes);
            return cuts;
        }
        // The rest starts inside the call: cut in place, no copy.
        let mut at = start - held_len;
        for end in self.cut_in_place(bytes, at, eof) {
            cuts.push(held_len + end);
            at = end;
        }
        self.held.clear();
        self.held.extend_from_slice(&bytes[at..]);
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

    /// Data whose cut sequences from different starts never meet (runs of one
    /// byte value), do meet (noise), and both in turn.
    fn mixed(seed: u64, len: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(len);
        let mut i = 0u64;
        while v.len() < len {
            let n = (300_000 + (i * 7919) % 900_000) as usize;
            if i % 3 == 1 {
                v.extend(std::iter::repeat_n((i % 251) as u8, n));
            } else {
                v.extend_from_slice(&noise(seed + i, n));
            }
            i += 1;
        }
        v.truncate(len);
        v
    }

    /// The parallel cut (threads 2, 3, 4, 24) is the sequential cut, whole and
    /// streamed in pieces, on noise, on one byte value (no sequence ever meets
    /// another's: every chunk cut on the calling thread), and on a mix; a call
    /// shorter than a piece and the end of a file at any position.
    #[test]
    fn the_parallel_cut_equals_the_sequential_cut() {
        let len = 11_000_000 + 12_345;
        for (name, data) in [
            ("noise", noise(11, len)),
            ("zeros", vec![0u8; len]),
            ("mixed", mixed(12, len)),
        ] {
            let want = reference(&data);
            for threads in [2usize, 3, 4, 24] {
                let mut c = FastCdcChunker::standard().with_threads(threads);
                assert_eq!(c.feed(&data, true), want, "{name} whole t{threads}");
                // Streamed: every call ends inside a piece, the last at eof.
                for sizes in [
                    vec![8 << 20],
                    vec![(3 << 20) + 7],
                    vec![CUT_PIECE + 1, 100_003],
                ] {
                    let mut c = FastCdcChunker::standard().with_threads(threads);
                    let mut out = Vec::new();
                    let (mut at, mut base, mut i) = (0usize, 0usize, 0usize);
                    while at < data.len() {
                        let n = sizes[i % sizes.len()].min(data.len() - at);
                        i += 1;
                        let eof = at + n == data.len();
                        for cut in c.feed(&data[at..at + n], eof) {
                            out.push(base + cut);
                        }
                        if let Some(&last) = out.last() {
                            base = last;
                        }
                        at += n;
                    }
                    assert_eq!(out, want, "{name} streamed {sizes:?} t{threads}");
                }
            }
        }
        // Shorter than a piece: one thread's worth, cut here.
        let short = noise(13, CUT_PIECE - 1);
        let mut c = FastCdcChunker::standard().with_threads(4);
        assert_eq!(c.feed(&short, true), reference(&short));
        assert!(FastCdcChunker::standard().with_threads(0).threads == 1);
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
