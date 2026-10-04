//! The incompressibility gate: a cheap test that says whether a block is worth handing to a
//! compressor.
//!
//! The sample (the first 4 KiB of every 64 KiB window) rejects early: a sampled entropy below
//! `sample_reject` means the block is compressible and no second pass is made. Otherwise the
//! whole block's entropy decides against `full_threshold`.
//!
//! Why the defaults: the Phase 0 report `bench/reports/phase0-2026-10-03.md`, section "Probe
//! `entropy-gate`". On the video and encrypted-random classes the entropy gate at the full
//! threshold reaches the top precision of the grid with near-complete recall; on every other
//! class that threshold has the best precision of the entropy thresholds (precision is the size
//! kept, recall the time saved); a sample below the reject value is almost never an
//! incompressible block, so the sample can reject without a second pass. The thresholds are
//! parameters, not constants, so that a tier can tune them.
//!
//! Accepted failure mode of any sampling gate: a block whose sampled heads are low-entropy but
//! whose remainder is random is judged compressible.

/// Block size the pipeline gates at; the functions here accept any slice.
pub const GATE_BLOCK: usize = 1 << 20;
/// Bytes sampled at the start of each window.
pub const SAMPLE_HEAD: usize = 4 << 10;
/// Window length of the sample.
pub const SAMPLE_WINDOW: usize = 64 << 10;

/// Gate thresholds in bits per byte.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gate {
    /// A sampled entropy below this means compressible.
    pub sample_reject: f64,
    /// Entropy of the whole block at or above this means incompressible.
    pub full_threshold: f64,
}

impl Gate {
    /// The thresholds the Phase 0 probe supports (see the module documentation).
    pub const DEFAULT: Gate = Gate {
        sample_reject: 7.5,
        full_threshold: 7.95,
    };

    /// True when `block` is judged incompressible. Blocks shorter than the sample head skip the
    /// sample; an empty block is compressible.
    pub fn is_incompressible(&self, block: &[u8]) -> bool {
        if block.is_empty() {
            return false;
        }
        if block.len() >= SAMPLE_HEAD && sampled_entropy(block) < self.sample_reject {
            return false;
        }
        entropy(block) >= self.full_threshold
    }
}

impl Default for Gate {
    fn default() -> Self {
        Gate::DEFAULT
    }
}

/// [`Gate::is_incompressible`] with [`Gate::DEFAULT`].
pub fn is_incompressible(block: &[u8]) -> bool {
    Gate::DEFAULT.is_incompressible(block)
}

/// Byte histogram counted with four interleaved tables merged at the end.
pub(crate) fn histogram(data: &[u8]) -> [u64; 256] {
    let mut h = [[0u64; 256]; 4];
    add_counts(&mut h, data);
    merge(&h)
}

fn add_counts(h: &mut [[u64; 256]; 4], data: &[u8]) {
    let (chunks, rest) = data.as_chunks::<4>();
    for c in chunks {
        h[0][c[0] as usize] += 1;
        h[1][c[1] as usize] += 1;
        h[2][c[2] as usize] += 1;
        h[3][c[3] as usize] += 1;
    }
    for &b in rest {
        h[0][b as usize] += 1;
    }
}

fn merge(h: &[[u64; 256]; 4]) -> [u64; 256] {
    let mut out = [0u64; 256];
    for (i, o) in out.iter_mut().enumerate() {
        *o = h[0][i] + h[1][i] + h[2][i] + h[3][i];
    }
    out
}

pub(crate) fn entropy_of(counts: &[u64; 256]) -> f64 {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let n = total as f64;
    let mut e = 0.0;
    for &c in counts.iter().filter(|&&c| c > 0) {
        let p = c as f64 / n;
        e -= p * p.log2();
    }
    // Rounding can leave a hair below zero for a single-valued input.
    e.max(0.0)
}

/// Order-0 Shannon entropy of `data` in bits per byte (0 for an empty slice).
pub fn entropy(data: &[u8]) -> f64 {
    entropy_of(&histogram(data))
}

/// Order-0 entropy of the sample of `data`: the first 4 KiB of every 64 KiB window (a shorter
/// last window contributes all of its bytes up to 4 KiB). Data shorter than 4 KiB is used whole.
pub fn sampled_entropy(data: &[u8]) -> f64 {
    let mut h = [[0u64; 256]; 4];
    for window in data.chunks(SAMPLE_WINDOW) {
        add_counts(&mut h, &window[..window.len().min(SAMPLE_HEAD)]);
    }
    entropy_of(&merge(&h))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn xorshift(seed: u64, len: usize) -> Vec<u8> {
        let mut s = seed;
        let mut v = Vec::with_capacity(len + 8);
        while v.len() < len {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            v.extend_from_slice(&s.to_le_bytes());
        }
        v.truncate(len);
        v
    }

    #[test]
    fn zeros_are_compressible_random_is_not() {
        assert!(!is_incompressible(&vec![0u8; GATE_BLOCK]));
        assert!(is_incompressible(&xorshift(1, GATE_BLOCK)));
        assert!(!is_incompressible(&[]));
    }

    #[test]
    fn sample_fast_path_rejects_a_block_with_zeroed_heads() {
        // The accepted failure mode of a sampling gate: the heads are zero, the rest is random.
        let mut b = xorshift(2, GATE_BLOCK);
        for w in b.chunks_mut(SAMPLE_WINDOW) {
            w[..SAMPLE_HEAD].fill(0);
        }
        assert!(sampled_entropy(&b) < 1.0);
        assert!(entropy(&b) > 7.0);
        assert!(!is_incompressible(&b));
        // Without the sample the same block is incompressible by the full entropy alone.
        let full_only = Gate {
            sample_reject: 0.0,
            full_threshold: 7.0,
        };
        assert!(full_only.is_incompressible(&b));
    }

    #[test]
    fn blocks_around_the_sample_head_size() {
        for n in [SAMPLE_HEAD - 1, SAMPLE_HEAD] {
            // Skips the sample below 4 KiB; at 4 KiB the sample is the block itself.
            assert!(!is_incompressible(&vec![7u8; n]), "{n}");
            let r = xorshift(3, n);
            assert_eq!(
                is_incompressible(&r),
                entropy(&r) >= Gate::DEFAULT.full_threshold,
                "{n}"
            );
        }
        let r = xorshift(3, SAMPLE_HEAD);
        assert_eq!(sampled_entropy(&r), entropy(&r));
    }

    #[test]
    fn sampled_entropy_is_the_entropy_of_the_concatenated_heads() {
        let b = xorshift(4, GATE_BLOCK);
        let heads: Vec<u8> = b
            .chunks(SAMPLE_WINDOW)
            .flat_map(|w| w[..w.len().min(SAMPLE_HEAD)].iter().copied())
            .collect();
        assert_eq!(heads.len(), 16 * SAMPLE_HEAD);
        assert_eq!(sampled_entropy(&b), entropy(&heads));
        // A partial last window: shorter than a head, and longer than one.
        for extra in [100usize, 10_000] {
            let b = xorshift(5, 2 * SAMPLE_WINDOW + extra);
            let mut heads = Vec::new();
            for w in b.chunks(SAMPLE_WINDOW) {
                heads.extend_from_slice(&w[..w.len().min(SAMPLE_HEAD)]);
            }
            assert_eq!(sampled_entropy(&b), entropy(&heads));
        }
    }

    #[test]
    fn thresholds_are_monotone() {
        let r = xorshift(6, 100_000);
        let never = Gate {
            sample_reject: 0.0,
            full_threshold: 8.0001,
        };
        let always = Gate {
            sample_reject: 0.0,
            full_threshold: 0.0,
        };
        assert!(!never.is_incompressible(&r));
        assert!(always.is_incompressible(&[0u8; 10]));
        assert!(always.is_incompressible(&r));
        assert!(!always.is_incompressible(&[]));
    }

    #[test]
    fn entropy_of_known_distributions() {
        assert_eq!(entropy(&[]), 0.0);
        assert_eq!(entropy(&[9u8; 1000]), 0.0);
        let two: Vec<u8> = (0..1000).map(|i| (i % 2) as u8).collect();
        assert!((entropy(&two) - 1.0).abs() < 1e-12);
        let all: Vec<u8> = (0..=255u8).cycle().take(256 * 7 + 3).collect();
        assert!((entropy(&all) - 8.0).abs() < 0.001);
        // The remainder of the four-way split is counted.
        assert_eq!(histogram(&[1, 2, 3, 4, 5, 5]).iter().sum::<u64>(), 6);
    }
}
