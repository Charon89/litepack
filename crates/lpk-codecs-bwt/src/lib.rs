//! The BWT codec of LitePack's measured gate E2-15 (D-05: BWT only on text-classified streams).
//!
//! * `forward` (feature `encode`): the Burrows-Wheeler transform of a block through libsais
//!   (the safe `libsais` wrapper, single-threaded, no OpenMP), giving the transformed bytes and
//!   the primary index. Only the `encode` feature links the C library: a reader build
//!   (the default features) has no C dependency.
//! * [`inverse`]: the inverse transform, written here in safe Rust (the wrapper's own inverse
//!   needs an `unsafe` constructor for a BWT that did not come from its forward call).
//! * [`entropy`]: a dedicated entropy stage for BWT output: move-to-front, zero-run coding
//!   (bzip2's RUNA/RUNB) and an adaptive binary arithmetic coder with order-1 contexts on the
//!   ranks (bzip2's structure with arithmetic coding instead of Huffman tables).
//! * `encode_block` (feature `encode`) / [`decode_block`]: candidate (b) of the gate, a whole
//!   block: the primary index (4 bytes, little-endian) followed by the entropy stage's output.
//!
//! Nothing here is part of the `.lpk` format yet: shipping it is a format revision (D-49), which
//! follows only if the gate opens.
//!
//! libsais convention (`libsais` crate documentation): the text is treated as if a unique
//! smallest sentinel followed it, and the sentinel's entry is left out of the output, so the BWT
//! has the block's length. The primary index `i0` is the row of the sorted rotations of
//! `text + $` whose last character is the sentinel; it is in `1..=n` for a block of `n >= 1`
//! bytes and 0 for the empty block.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod entropy;
mod rc;

#[cfg(feature = "encode")]
use libsais::bwt::AuxIndicesSamplingRate;
#[cfg(feature = "encode")]
use libsais::BwtConstruction;

/// The largest block `forward` accepts: libsais's `i32` temporary array limits the text to
/// `i32::MAX` bytes; the format's default cap (`max_bwt_block`) is far below.
pub const MAX_BLOCK: usize = i32::MAX as usize;

/// The version of the libsais C library linked (feature `encode`).
#[cfg(feature = "encode")]
pub const LIBSAIS_VERSION: &str = libsais::LIBSAIS_VERSION_STRING;

/// What can go wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BwtError {
    /// The block is longer than [`MAX_BLOCK`].
    TooLarge(usize),
    /// libsais refused or failed (its error, as text).
    Libsais(String),
    /// The primary index is not valid for the block's length.
    BadPrimary { primary: u32, len: usize },
    /// The encoded data is damaged or does not belong to this length.
    Corrupt(&'static str),
}

impl std::fmt::Display for BwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BwtError::TooLarge(n) => write!(f, "block of {n} bytes exceeds {MAX_BLOCK}"),
            BwtError::Libsais(e) => write!(f, "libsais: {e}"),
            BwtError::BadPrimary { primary, len } => {
                write!(f, "primary index {primary} is invalid for {len} bytes")
            }
            BwtError::Corrupt(why) => write!(f, "corrupt BWT stream: {why}"),
        }
    }
}

impl std::error::Error for BwtError {}

/// Chains the inverse walks side by side (see [`inverse`]); the forward transform samples one
/// auxiliary index per chain.
pub const CHAINS: usize = 32;

/// A transformed block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transformed {
    /// The BWT, as long as the block.
    pub bwt: Vec<u8>,
    /// log2 of the sampling rate `r` of the auxiliary indices (libsais: a power of two, at least
    /// 2).
    pub rate_log2: u8,
    /// The auxiliary indices: `aux[i]` is the row whose rotation starts at text position
    /// `i * r` (libsais: `AUX[i] == k => SUF[k - 1] == i * r`); `aux[0]` is the primary index.
    /// `(n - 1) / r + 1` of them for `n >= 1` bytes, none for the empty block.
    pub aux: Vec<u32>,
}

impl Transformed {
    /// The primary index (0 for the empty block).
    pub fn primary(&self) -> u32 {
        self.aux.first().copied().unwrap_or(0)
    }

    /// The header that carries the indices: `rate_log2` (1 byte), then each auxiliary index as
    /// 4 bytes LE. Its length follows from the block length and `rate_log2`.
    pub fn header(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 4 * self.aux.len());
        out.push(self.rate_log2);
        for a in &self.aux {
            out.extend_from_slice(&a.to_le_bytes());
        }
        out
    }

    /// Split `data` into the indices of a block of `len` bytes and the rest; `bwt` is left
    /// empty.
    pub fn parse_header(data: &[u8], len: usize) -> Result<(Transformed, &[u8]), BwtError> {
        let (&rate_log2, rest) = data.split_first().ok_or(BwtError::Corrupt("no header"))?;
        if !(1..=31).contains(&rate_log2) {
            return Err(BwtError::Corrupt("sampling rate out of range"));
        }
        let count = aux_count(len, rate_log2);
        let bytes = rest
            .get(..4 * count)
            .ok_or(BwtError::Corrupt("header shorter than its indices"))?;
        let aux = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        Ok((
            Transformed {
                bwt: Vec::new(),
                rate_log2,
                aux,
            },
            &rest[4 * count..],
        ))
    }
}

fn aux_count(len: usize, rate_log2: u8) -> usize {
    if len == 0 {
        0
    } else {
        ((len - 1) >> rate_log2) + 1
    }
}

/// The sampling rate's log2 for `len` bytes: the smallest power of two (at least 2) that
/// splits the block into at most [`CHAINS`] pieces.
#[cfg(feature = "encode")]
fn rate_log2_for(len: usize) -> u8 {
    let per = len.div_ceil(CHAINS).max(2);
    per.next_power_of_two().trailing_zeros() as u8
}

/// The BWT of `block` (libsais, one thread), with one auxiliary index per [`CHAINS`]th of it.
/// Feature `encode`.
#[cfg(feature = "encode")]
pub fn forward(block: &[u8]) -> Result<Transformed, BwtError> {
    if block.len() > MAX_BLOCK {
        return Err(BwtError::TooLarge(block.len()));
    }
    let rate_log2 = rate_log2_for(block.len());
    if block.is_empty() {
        return Ok(Transformed {
            bwt: Vec::new(),
            rate_log2,
            aux: Vec::new(),
        });
    }
    let rate = AuxIndicesSamplingRate::from(1i32 << rate_log2);
    let res = BwtConstruction::for_text(block)
        .with_owned_temporary_array_buffer32()
        .single_threaded()
        .with_aux_indices(rate)
        .run()
        .map_err(|e| BwtError::Libsais(e.to_string()))?;
    let (bwt, aux, _) = res.into_parts();
    let aux = aux
        .iter()
        .map(|&a| u32::try_from(a).map_err(|_| BwtError::Corrupt("negative index from libsais")))
        .collect::<Result<Vec<u32>, BwtError>>()?;
    Ok(Transformed {
        bwt,
        rate_log2,
        aux,
    })
}

/// The block of `t` (its BWT and auxiliary indices).
///
/// Rows are those of the sorted rotations of `text + $` (`n + 1` rows); row 0 is `$text`, and
/// row `aux[0]` (the primary index) is the one whose last character is the sentinel.
/// `next[r]` is the row of the rotation that starts one position later than row `r`'s, and
/// `text[k]` is the last character of the row starting at `k + 1`. Walking `next` from
/// `aux[i]` gives the text from position `i * r`; the walks of all indices run interleaved, so
/// a core keeps several memory reads in flight instead of one. Every index is checked, so
/// damaged input gives wrong bytes or an error, never a panic.
pub fn inverse(t: &Transformed) -> Result<Vec<u8>, BwtError> {
    let bwt = &t.bwt;
    let n = bwt.len();
    let bad = || BwtError::BadPrimary {
        primary: t.primary(),
        len: n,
    };
    if !(1..=31).contains(&t.rate_log2) || t.aux.len() != aux_count(n, t.rate_log2) {
        return Err(bad());
    }
    if n == 0 {
        return Ok(Vec::new());
    }
    let i0 = t.primary() as usize;
    if n >= u32::MAX as usize || t.aux.iter().any(|&a| a == 0 || a as usize > n) {
        return Err(bad());
    }
    // The full last column L (n + 1 entries): L[f] = bwt[f] for f < i0, the sentinel at i0,
    // bwt[f - 1] above.
    let mut count = [0u32; 256];
    for &b in bwt {
        count[usize::from(b)] += 1;
    }
    // First row of each character in the sorted first column: the sentinel's row 0 comes first.
    let mut start = [0u32; 256];
    let mut acc = 1u32;
    for c in 0..256 {
        start[c] = acc;
        acc += count[c];
    }
    // `link[r]` packs `next[r] << 8 | L[next[r]]`, so each output byte costs one random read.
    // The sentinel's rotation (row i0) has nothing before it in the first column: its LF image
    // is row 0, so next[0] = i0 (whose L, the sentinel, is never read: marked 0).
    let mut link = vec![0u64; n + 1];
    link[0] = (i0 as u64) << 8;
    for (f, &c) in bwt.iter().enumerate() {
        let full = if f < i0 { f } else { f + 1 };
        let slot = &mut start[usize::from(c)];
        // `slot` stays below n + 1: each character's rows are exactly `count[c]` entries.
        link[*slot as usize] = (full as u64) << 8;
        *slot += 1;
    }
    for v in link.iter_mut() {
        let q = (*v >> 8) as usize;
        let b = match q.cmp(&i0) {
            std::cmp::Ordering::Less => bwt[q],
            std::cmp::Ordering::Greater => bwt[q - 1],
            std::cmp::Ordering::Equal => 0,
        };
        *v |= u64::from(b);
    }
    let rate = 1usize << t.rate_log2;
    let mut out = vec![0u8; n];
    let mut rows: Vec<usize> = t.aux.iter().map(|&a| a as usize).collect();
    // All chains but the last are `rate` long; the last ends at n.
    let last_len = n - (rows.len() - 1) * rate;
    let full_chains = if last_len == rate {
        rows.len()
    } else {
        rows.len() - 1
    };
    for step in 0..rate {
        let live = if step < last_len {
            rows.len()
        } else {
            full_chains
        };
        for (j, p) in rows.iter_mut().enumerate().take(live) {
            let v = link[*p];
            let q = (v >> 8) as usize;
            if q == i0 {
                return Err(BwtError::Corrupt("cycle reached the start"));
            }
            out[j * rate + step] = v as u8;
            *p = q;
        }
    }
    Ok(out)
}

/// Candidate (b): `block` transformed and entropy-coded, after the index header
/// ([`Transformed::header`]). Feature `encode`.
#[cfg(feature = "encode")]
pub fn encode_block(block: &[u8]) -> Result<Vec<u8>, BwtError> {
    let t = forward(block)?;
    Ok(encode_transformed(&t))
}

/// The bytes of candidate (b) for an already transformed block (so a caller can time the
/// transform and the entropy stage apart).
pub fn encode_transformed(t: &Transformed) -> Vec<u8> {
    let mut out = t.header();
    out.extend_from_slice(&entropy::encode(&t.bwt));
    out
}

/// The block of `len` bytes that [`encode_block`] encoded into `data`.
pub fn decode_block(data: &[u8], len: usize) -> Result<Vec<u8>, BwtError> {
    inverse(&decode_to_transformed(data, len)?)
}

/// The transformed block inside `data` (candidate (b) without the inverse transform).
pub fn decode_to_transformed(data: &[u8], len: usize) -> Result<Transformed, BwtError> {
    let (mut t, rest) = Transformed::parse_header(data, len)?;
    t.bwt = entropy::decode(rest, len)?;
    Ok(t)
}

// The tests need the forward transform: the crate's own dev-dependency on itself turns the
// `encode` feature on for them.
#[cfg(all(test, feature = "encode"))]
mod tests {
    use super::*;

    /// The BWT by sorting the rotations of `text + $` (sentinel smallest), libsais convention:
    /// the BWT without the sentinel and the row of every text position.
    fn naive(text: &[u8]) -> (Vec<u8>, Vec<u32>) {
        let n = text.len();
        // Symbols: 0 is the sentinel, byte b is b + 1.
        let s: Vec<u16> = text
            .iter()
            .map(|&b| u16::from(b) + 1)
            .chain(std::iter::once(0))
            .collect();
        let mut rows: Vec<usize> = (0..=n).collect();
        rows.sort_by(|&a, &b| {
            (0..=n)
                .map(|k| s[(a + k) % (n + 1)])
                .cmp((0..=n).map(|k| s[(b + k) % (n + 1)]))
        });
        let mut bwt = Vec::new();
        let mut row_of = vec![0u32; n + 1];
        for (r, &start) in rows.iter().enumerate() {
            row_of[start] = r as u32;
            let last = s[(start + n) % (n + 1)];
            if last != 0 {
                bwt.push((last - 1) as u8);
            }
        }
        (bwt, row_of)
    }

    fn xorshift(seed: u64) -> impl FnMut() -> u64 {
        let mut s = seed | 1;
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        }
    }

    #[test]
    fn known_answer_banana() {
        // Rotations of "banana$": $banana, a$banan, ana$ban, anana$b, banana$, na$bana, nana$ba.
        // Last column: a n n b $ a a; without the sentinel "annbaa", sentinel at row 4.
        let t = forward(b"banana").unwrap();
        assert_eq!(t.bwt, b"annbaa");
        assert_eq!(t.primary(), 4);
        // Rate 2 (the smallest): rows of positions 0, 2, 4 ("banana$", "nana$ba", "na$bana").
        assert_eq!(t.rate_log2, 1);
        assert_eq!(t.aux, vec![4, 6, 5]);
        assert_eq!(inverse(&t).unwrap(), b"banana");
    }

    #[test]
    fn forward_matches_the_naive_transform_and_inverse_undoes_it() {
        let mut r = xorshift(7);
        for len in 1..200usize {
            for alphabet in [1u64, 2, 3, 26, 256] {
                let text: Vec<u8> = (0..len).map(|_| (r() % alphabet) as u8).collect();
                let t = forward(&text).unwrap();
                let (bwt, row_of) = naive(&text);
                assert_eq!(t.bwt, bwt, "len {len} alphabet {alphabet}");
                let rate = 1usize << t.rate_log2;
                let want: Vec<u32> = (0..len).step_by(rate).map(|i| row_of[i]).collect();
                assert_eq!(t.aux, want, "len {len} alphabet {alphabet}");
                assert_eq!(inverse(&t).unwrap(), text);
                // The same BWT inverts from the primary index alone (one chain).
                let one = Transformed {
                    bwt: t.bwt.clone(),
                    rate_log2: (len.next_power_of_two().max(2)).trailing_zeros() as u8,
                    aux: vec![t.primary()],
                };
                assert_eq!(inverse(&one).unwrap(), text, "len {len}");
            }
        }
    }

    #[test]
    fn chains_cover_lengths_around_powers_of_two() {
        let mut r = xorshift(11);
        for len in [63usize, 64, 65, 127, 128, 129, 1000, 4096, 4097, 100_003] {
            let text: Vec<u8> = (0..len).map(|_| b"ab \n"[(r() % 4) as usize]).collect();
            let t = forward(&text).unwrap();
            assert!(t.aux.len() <= CHAINS, "len {len}");
            assert_eq!(inverse(&t).unwrap(), text, "len {len}");
            let header = t.header();
            let (h, rest) = Transformed::parse_header(&header, len).unwrap();
            assert!(rest.is_empty());
            assert_eq!((h.rate_log2, h.aux), (t.rate_log2, t.aux.clone()));
        }
    }

    #[test]
    fn inverse_agrees_with_the_wrappers_inverse() {
        let mut r = xorshift(99);
        let text: Vec<u8> = (0..100_000)
            .map(|_| b"abcde \n"[(r() % 7) as usize])
            .collect();
        let res = BwtConstruction::for_text(&text)
            .with_owned_temporary_array_buffer32()
            .single_threaded()
            .run()
            .unwrap();
        let one = Transformed {
            bwt: res.bwt().to_vec(),
            rate_log2: 17,
            aux: vec![res.primary_index() as u32],
        };
        let ours = inverse(&one).unwrap();
        let theirs = res
            .unbwt()
            .with_owned_temporary_array_buffer32()
            .single_threaded()
            .run()
            .unwrap();
        assert_eq!(ours, theirs.as_slice());
        assert_eq!(ours, text);
        assert_eq!(forward(&text).unwrap().bwt, one.bwt);
    }

    #[test]
    fn empty_and_bad_indices() {
        let t = forward(b"").unwrap();
        assert_eq!(t.primary(), 0);
        assert!(t.aux.is_empty());
        assert_eq!(inverse(&t).unwrap(), Vec::<u8>::new());
        let mk = |bwt: &[u8], rate_log2: u8, aux: Vec<u32>| Transformed {
            bwt: bwt.to_vec(),
            rate_log2,
            aux,
        };
        assert!(inverse(&mk(b"", 1, vec![1])).is_err());
        assert!(inverse(&mk(b"abc", 2, vec![0])).is_err());
        assert!(inverse(&mk(b"abc", 2, vec![4])).is_err());
        assert!(inverse(&mk(b"abc", 1, vec![1])).is_err(), "wrong count");
        assert!(inverse(&mk(b"abc", 0, vec![1, 1, 1])).is_err(), "rate 1");
        assert!(Transformed::parse_header(&[], 5).is_err());
        assert!(Transformed::parse_header(&[40], 5).is_err());
        assert!(Transformed::parse_header(&[1, 0, 0], 5).is_err());
        // Any in-range indices on arbitrary bytes give n bytes or an error, never a panic.
        for p in 1..=3 {
            for q in 1..=3 {
                let _ = inverse(&mk(b"zzy", 1, vec![p, q]));
            }
        }
    }

    #[test]
    fn block_round_trip_and_damage() {
        let mut r = xorshift(3);
        let mut text = Vec::new();
        while text.len() < 300_000 {
            let w = [
                "the ",
                "quick ",
                "brown ",
                "fox\n",
                "jumps ",
                "2026-10-17 INFO ",
            ];
            text.extend_from_slice(w[(r() % 6) as usize].as_bytes());
        }
        let enc = encode_block(&text).unwrap();
        assert!(
            enc.len() < text.len() / 4,
            "{} of {}",
            enc.len(),
            text.len()
        );
        assert_eq!(decode_block(&enc, text.len()).unwrap(), text);
        // Truncated or damaged: an error or other bytes, never a panic.
        assert!(decode_block(&enc[..3], text.len()).is_err());
        let mut bad = enc.clone();
        for i in (200..bad.len()).step_by(997) {
            bad[i] ^= 0x5A;
        }
        if let Ok(v) = decode_block(&bad, text.len()) {
            assert_ne!(v, text);
        }
        let _ = decode_block(&enc[..enc.len() / 2], text.len());
        let mut bad_header = enc.clone();
        bad_header[3] ^= 0x40;
        let _ = decode_block(&bad_header, text.len());
    }
}
