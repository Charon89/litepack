//! Revision 1.3's `bwt` step (spec section 8, "Decoding `bwt`"): the layout of the step's input,
//! the inverse transform in `u32`, the input bound I(O, B) and the memory term.
//!
//! The step's input is `out_len` (u64 LE), then k = ⌈out_len / B⌉ BWT blocks (B = the step's
//! `block_size`), all of B bytes except the last; block j holds c(n_j) indices (u32 LE) and then
//! its n_j transformed bytes. The indices are derived from the text, not chosen: index[i] is the
//! row of the rotation that starts at text position i · r(n), with r(n) the smallest power of two
//! at least max(2, ⌈n / 32⌉) and c(n) = ⌈n / r(n)⌉ ≤ 32; index[0] is the primary index.
//!
//! The inverse here walks forward from every index at once ([`CHAINS`] interleaved walks) with
//! the successor table LF⁻¹ in `u32` (n + 1 entries), reading each byte from the transformed
//! bytes themselves, so a block costs 4 bytes per byte beyond the step's input and the output
//! (the probe's inverse in `lib.rs` packs the byte into a `u64` link, about ten bytes per byte).
//! Every validity condition of the text is checked: a chain ends on the next index, the last
//! chain ends on row 0, and no earlier step lands on row 0; a block whose auxiliary indices are
//! wrong is refused even when the walk from the primary index alone would give a text.

use crate::CHAINS;

/// The fixed part of the memory term: the counts, the first-column lookup and the chain cursors.
pub const MEMORY_ALLOWANCE: u64 = 1 << 20;

/// The length of the `out_len` field that starts the step's input.
pub const OUT_LEN_FIELD: u64 = 8;

/// Why one BWT block, or the step's input, is refused: the normative `reason` strings of
/// `BwtError` (spec section 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The input is shorter than 8 bytes, or its length is not 8 + out_len + 4·Σ c(n_j).
    Length,
    /// An index is outside 1..=n.
    Index,
    /// A validity condition of the inverse fails.
    Inverse,
}

impl Reason {
    /// The `reason` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Length => "length",
            Reason::Index => "index",
            Reason::Inverse => "inverse",
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// log2 of r(n): the smallest power of two at least max(2, ⌈n / 32⌉).
pub fn rate_log2(n: u64) -> u32 {
    let per = n.div_ceil(CHAINS as u64).max(2);
    per.next_power_of_two().trailing_zeros()
}

/// c(n) = ⌈n / r(n)⌉, at most [`CHAINS`]; 0 for n = 0 (no block has 0 bytes).
pub fn index_count(n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        n.div_ceil(1u64 << rate_log2(n))
    }
}

/// The lengths of the k = ⌈out_len / B⌉ BWT blocks of a step: `(full, last)` with `full` blocks
/// of `block_size` bytes followed by one of `last` bytes (`last` is 0 only when `out_len` is 0,
/// which has no block at all).
pub fn blocks(out_len: u64, block_size: u64) -> (u64, u64) {
    let b = block_size.max(1);
    let k = out_len.div_ceil(b);
    let full = k.saturating_sub(1);
    (full, out_len - full * b)
}

/// I(O, B): the exact input length of a `bwt` step whose output is `out_len` bytes with BWT
/// blocks of `block_size`: 8 + out_len + 4·Σ c(n_j), in 64-bit arithmetic saturating at
/// 2^64 - 1. The output bound of the step before `bwt`.
pub fn input_len(out_len: u64, block_size: u64) -> u64 {
    let (full, last) = blocks(out_len, block_size);
    let indices = full
        .saturating_mul(index_count(block_size.max(1)))
        .saturating_add(index_count(last));
    OUT_LEN_FIELD
        .saturating_add(out_len)
        .saturating_add(indices.saturating_mul(4))
}

/// term(O, B) = I(O, B) + 4·min(O, B) + 1048576 (saturating): the step's input, a `u32`
/// successor table for the largest block, and the fixed allowance. The output is the block's own
/// buffer, counted in `max_block_plain`.
pub fn memory_term(out_len: u64, block_size: u64) -> u64 {
    input_len(out_len, block_size)
        .saturating_add(out_len.min(block_size).saturating_mul(4))
        .saturating_add(MEMORY_ALLOWANCE)
}

/// Invert one BWT block: `bwt` is its n ≥ 1 transformed bytes, `indices` its c(n) indices
/// (`index[0]` the primary), `out` the n bytes of text it produces. `next` is the successor
/// table, resized here to n + 1 entries (a caller keeps one across blocks so it is allocated once
/// for the largest block). Errors: a wrong count of indices or an `out` of another length is
/// `Length` (a caller that parsed the layout never meets it), an index outside 1..=n is `Index`,
/// a failed validity condition is `Inverse`; nothing panics on any input.
pub fn inverse_block(
    bwt: &[u8],
    indices: &[u32],
    next: &mut Vec<u32>,
    out: &mut [u8],
) -> Result<(), Reason> {
    let n = bwt.len();
    // Rows 0..=n must fit a `u32`; `index_count` is at most CHAINS.
    if n == 0 || out.len() != n || u32::try_from(n).is_err() {
        return Err(Reason::Length);
    }
    let c = usize::try_from(index_count(n as u64)).map_err(|_| Reason::Length)?;
    if indices.len() != c {
        return Err(Reason::Length);
    }
    if indices.iter().any(|&i| i == 0 || i as usize > n) {
        return Err(Reason::Index);
    }
    let i0 = indices[0] as usize;

    // C[c] = 1 + the number of bytes below c: row 0 is the sentinel's, then every byte's rows in
    // order. `start[c]` walks from C[c] to C[c + 1] as the bytes are placed; the final value of
    // the last used byte may be n + 1, which overflows a `u32` only when n = 2^32 - 1, and is
    // never read, so the additions wrap.
    let mut count = [0u32; 256];
    for &b in bwt {
        count[usize::from(b)] = count[usize::from(b)].wrapping_add(1);
    }
    let mut start = [0u32; 256];
    let mut acc = 1u32;
    for (s, &k) in start.iter_mut().zip(count.iter()) {
        *s = acc;
        acc = acc.wrapping_add(k);
    }

    // The successor table: next[LF(i)] = i for every row i. LF(i0) = 0 (the sentinel row's
    // rotation starts at position 0, whose predecessor is row 0's rotation), so next[0] = i0;
    // for the other rows LF(i) = C[L[i]] + the occurrences of L[i] before i, where L is the
    // transformed bytes with the sentinel inserted at row i0 (so byte f of `bwt` is at row f
    // below i0 and at row f + 1 above it). Every slot 1..=n is written exactly once: `next` is
    // a permutation of 0..=n whatever the input.
    next.clear();
    next.resize(n + 1, 0);
    next[0] = indices[0];
    for (f, &b) in bwt.iter().enumerate() {
        let row = if f < i0 { f } else { f + 1 };
        let slot = &mut start[usize::from(b)];
        next[*slot as usize] = row as u32;
        *slot = slot.wrapping_add(1);
    }

    // The chains: chain j starts at index[j] and runs r steps (the last one n - (c - 1)·r), all
    // interleaved so a core keeps several memory reads in flight. Row p is the rotation starting
    // at some position k; next[p] = q is the rotation starting at k + 1, whose last character,
    // L[q], is T[k]. The step lands on row 0 (the rotation starting at position n, the
    // sentinel's) only as the last step of the last chain.
    let rate = 1usize << rate_log2(n as u64);
    let last_len = n - (c - 1) * rate;
    let mut rows = [0u32; CHAINS];
    rows[..c].copy_from_slice(indices);
    for step in 0..rate {
        let live = if step < last_len { c } else { c - 1 };
        for (j, p) in rows.iter_mut().enumerate().take(live) {
            let q = next[*p as usize];
            let qi = q as usize;
            if qi == 0 && !(j == c - 1 && step == last_len - 1) {
                return Err(Reason::Inverse);
            }
            // `next` is a permutation, so row i0 (next[0]) is reached only from row 0, which no
            // chain continues from; the check keeps the read below in range on any input.
            if qi == i0 {
                return Err(Reason::Inverse);
            }
            out[j * rate + step] = if qi < i0 { bwt[qi] } else { bwt[qi - 1] };
            *p = q;
        }
    }
    // Chain j ends on index[j + 1]; the last chain ends on row 0.
    if rows[..c - 1] != indices[1..] || rows[c - 1] != 0 {
        return Err(Reason::Inverse);
    }
    Ok(())
}

/// One parsed BWT block of a step's input: its indices and transformed bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block<'a> {
    /// The c(n) indices, little-endian `u32`s as stored (not yet range-checked).
    pub indices: &'a [u8],
    /// The n transformed bytes.
    pub bwt: &'a [u8],
}

/// The blocks of a step's input, walked lazily: nothing is allocated per block (a step of B = 1
/// has one block per output byte, far more than its memory term could hold as a list).
/// Made by [`blocks_of`], which checks the total length first (check (4) of the text), so every
/// block the iterator yields is complete.
#[derive(Debug, Clone)]
pub struct Blocks<'a> {
    rest: &'a [u8],
    full: u64,
    last: u64,
    block_size: u64,
}

impl<'a> Iterator for Blocks<'a> {
    type Item = Block<'a>;

    fn next(&mut self) -> Option<Block<'a>> {
        let n = if self.full > 0 {
            self.full -= 1;
            self.block_size
        } else if self.last > 0 {
            std::mem::take(&mut self.last)
        } else {
            return None;
        };
        // `blocks_of` checked the exact total length, so the splits cannot fail.
        let n = usize::try_from(n).ok()?;
        let c = usize::try_from(index_count(n as u64)).ok()?;
        let (indices, after) = self.rest.split_at_checked(4 * c)?;
        let (bwt, after) = after.split_at_checked(n)?;
        self.rest = after;
        Some(Block { indices, bwt })
    }
}

/// The step's blocks, after `out_len` was read and checked by the caller: `None` when `input`
/// (the bytes after the `out_len` field) is not exactly I(out_len, B) - 8 = Σ (4·c(n_j) + n_j)
/// bytes (`BwtError`, `length`), computed from the lengths alone before any block is looked at.
pub fn blocks_of(input: &[u8], out_len: u64, block_size: u64) -> Option<Blocks<'_>> {
    if (input.len() as u64).saturating_add(OUT_LEN_FIELD) != input_len(out_len, block_size) {
        return None;
    }
    let (full, last) = blocks(out_len, block_size);
    Some(Blocks {
        rest: input,
        full,
        last,
        block_size: block_size.max(1),
    })
}

/// The indices of a block as `u32`s on the stack: the first `c` entries of the array, `c` =
/// `indices.len() / 4` (at most [`CHAINS`]; more are ignored).
pub fn parse_indices(indices: &[u8]) -> ([u32; CHAINS], usize) {
    let mut out = [0u32; CHAINS];
    let mut c = 0;
    for (slot, bytes) in out.iter_mut().zip(indices.as_chunks::<4>().0) {
        *slot = u32::from_le_bytes(*bytes);
        c += 1;
    }
    (out, c)
}

/// The whole step: `input` is the step's input after the `out_len` field (the caller read and
/// checked `out_len` against its bound), `out` has `out_len` bytes. Checks (4) and (5) of the
/// text: the length (arithmetically, before any block is read), then per block in order the
/// index range and the inverse. Beyond `out` and the caller's input it allocates the successor
/// table once, for the largest block: the term's 4·min(O, B).
pub fn decode_blocks(
    input: &[u8],
    out_len: u64,
    block_size: u64,
    out: &mut [u8],
) -> Result<(), Reason> {
    if out.len() as u64 != out_len {
        return Err(Reason::Length);
    }
    let blocks = blocks_of(input, out_len, block_size).ok_or(Reason::Length)?;
    let mut next = Vec::new();
    let mut at = 0usize;
    for b in blocks {
        let (indices, c) = parse_indices(b.indices);
        let n = b.bwt.len();
        let dst = out.get_mut(at..at + n).ok_or(Reason::Length)?;
        inverse_block(b.bwt, &indices[..c], &mut next, dst)?;
        at += n;
    }
    if at != out.len() {
        return Err(Reason::Length);
    }
    Ok(())
}

/// The step's input for a text, from the transformed blocks (feature `encode`): `out_len`, then
/// per BWT block of `block_size` bytes its indices and transformed bytes.
#[cfg(feature = "encode")]
pub fn encode(text: &[u8], block_size: u32) -> Result<Vec<u8>, crate::BwtError> {
    let b = usize::try_from(block_size.max(1)).map_err(|_| crate::BwtError::TooLarge(0))?;
    let mut out = Vec::with_capacity(
        usize::try_from(input_len(text.len() as u64, u64::from(block_size))).unwrap_or(text.len()),
    );
    out.extend_from_slice(&(text.len() as u64).to_le_bytes());
    for block in text.chunks(b) {
        let t = crate::forward(block)?;
        debug_assert_eq!(u32::from(t.rate_log2), rate_log2(block.len() as u64));
        for a in &t.aux {
            out.extend_from_slice(&a.to_le_bytes());
        }
        out.extend_from_slice(&t.bwt);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_and_count_follow_the_text() {
        // r(n) is the smallest power of two at least max(2, ceil(n / 32)); c(n) = ceil(n / r).
        for (n, r, c) in [
            (1u64, 1u32, 1u64),
            (2, 1, 1),
            (6, 1, 3),
            (63, 1, 32),
            (64, 1, 32),
            (65, 2, 17),
            (128, 2, 32),
            (129, 3, 17),
            (4096, 7, 32),
            (4097, 8, 17),
            (16384, 9, 32),
            (1 << 26, 21, 32),
        ] {
            assert_eq!(rate_log2(n), r, "r({n})");
            assert_eq!(index_count(n), c, "c({n})");
            assert!(index_count(n) <= CHAINS as u64);
        }
        assert_eq!(index_count(0), 0);
    }

    #[test]
    fn input_len_and_term_are_the_formulas() {
        // 16384 bytes in 4096-byte blocks: 4 blocks of 32 indices each.
        assert_eq!(input_len(16384, 4096), 8 + 16384 + 4 * 128);
        // 16384 in one block: 32 indices.
        assert_eq!(input_len(16384, 16384), 8 + 16384 + 4 * 32);
        // 1 byte, block 1: one index.
        assert_eq!(input_len(1, 1), 8 + 1 + 4);
        // 7232 in 4096-byte blocks: 4096 (32 indices) and 3136 (r = 128, c = 25).
        assert_eq!(index_count(3136), 25);
        assert_eq!(input_len(7232, 4096), 8 + 7232 + 4 * (32 + 25));
        assert_eq!(input_len(0, 4096), 8);
        assert_eq!(blocks(0, 4096), (0, 0));
        assert_eq!(blocks(4096, 4096), (0, 4096));
        assert_eq!(blocks(4097, 4096), (1, 1));
        // The term at the defaults (design section 2): 537,920,008 bytes.
        assert_eq!(memory_term(256 << 20, 64 << 20), 537_920_008);
        assert_eq!(
            memory_term(100, 4096),
            input_len(100, 4096) + 400 + MEMORY_ALLOWANCE
        );
        // Saturation.
        assert_eq!(input_len(u64::MAX, 1), u64::MAX);
        assert_eq!(memory_term(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn banana_inverts_from_the_texts_example() {
        // "banana" gives "annbaa", r = 2, indices 4, 6, 5.
        let mut next = Vec::new();
        let mut out = [0u8; 6];
        inverse_block(b"annbaa", &[4, 6, 5], &mut next, &mut out).unwrap();
        assert_eq!(&out, b"banana");
        assert_eq!(next.len(), 7);
        // A wrong auxiliary index with a valid primary walk is `inverse`.
        assert_eq!(
            inverse_block(b"annbaa", &[4, 5, 6], &mut next, &mut out),
            Err(Reason::Inverse)
        );
        assert_eq!(
            inverse_block(b"annbaa", &[4, 6, 6], &mut next, &mut out),
            Err(Reason::Inverse)
        );
        // Out of range: `index`; wrong count or length: `length`.
        assert_eq!(
            inverse_block(b"annbaa", &[0, 6, 5], &mut next, &mut out),
            Err(Reason::Index)
        );
        assert_eq!(
            inverse_block(b"annbaa", &[4, 7, 5], &mut next, &mut out),
            Err(Reason::Index)
        );
        assert_eq!(
            inverse_block(b"annbaa", &[4, 6], &mut next, &mut out),
            Err(Reason::Length)
        );
        assert_eq!(
            inverse_block(b"", &[], &mut next, &mut []),
            Err(Reason::Length)
        );
        let mut short = [0u8; 5];
        assert_eq!(
            inverse_block(b"annbaa", &[4, 6, 5], &mut next, &mut short),
            Err(Reason::Length)
        );
    }

    #[test]
    fn one_byte_block() {
        let mut next = Vec::new();
        let mut out = [0u8; 1];
        inverse_block(b"x", &[1], &mut next, &mut out).unwrap();
        assert_eq!(&out, b"x");
        let mut layout = 1u64.to_le_bytes().to_vec();
        layout.extend_from_slice(&1u32.to_le_bytes());
        layout.push(b'x');
        let mut text = [0u8; 1];
        decode_blocks(&layout[8..], 1, 1, &mut text).unwrap();
        assert_eq!(&text, b"x");
    }

    #[test]
    fn every_in_range_index_set_gives_bytes_or_an_error() {
        // Nothing panics; a wrong primary index gives other bytes or `inverse`.
        let mut next = Vec::new();
        for p in 1..=6u32 {
            for q in 1..=6u32 {
                for s in 1..=6u32 {
                    let mut out = [0u8; 6];
                    let _ = inverse_block(b"annbaa", &[p, q, s], &mut next, &mut out);
                }
            }
        }
        // A run of one byte: every primary index but the right one fails or differs.
        for p in 1..=5u32 {
            let mut out = [0u8; 5];
            let _ = inverse_block(b"aaaaa", &[p, 1, 1], &mut next, &mut out);
        }
    }

    #[test]
    fn blocks_of_checks_the_exact_length_first() {
        // Two blocks of 4 (c = 2 each: r = 2) in a 4-byte block size, out_len 8.
        let mut v = Vec::new();
        for _ in 0..2 {
            v.extend_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0]);
            v.extend_from_slice(b"abcd");
        }
        let b: Vec<Block<'_>> = blocks_of(&v, 8, 4).unwrap().collect();
        assert_eq!(b.len(), 2);
        assert_eq!(b[1].bwt, b"abcd");
        let (idx, c) = parse_indices(b[1].indices);
        assert_eq!(&idx[..c], &[1, 2]);
        assert!(blocks_of(&v[..v.len() - 1], 8, 4).is_none());
        let mut longer = v.clone();
        longer.push(0);
        assert!(blocks_of(&longer, 8, 4).is_none());
        // out_len 0 has no block: only an empty rest passes.
        assert_eq!(blocks_of(&[], 0, 4).unwrap().count(), 0);
        assert!(blocks_of(&[0], 0, 4).is_none());
        // Blocks of 1 byte: one block per output byte, c = 1 each.
        let mut v = Vec::new();
        for b in b"xyz" {
            v.extend_from_slice(&[1, 0, 0, 0, *b]);
        }
        let b: Vec<Block<'_>> = blocks_of(&v, 3, 1).unwrap().collect();
        assert_eq!(b.len(), 3);
        assert_eq!(b[2].bwt, b"z");
        let mut out = [0u8; 3];
        decode_blocks(&v, 3, 1, &mut out).unwrap();
        assert_eq!(&out, b"xyz");
    }

    /// A naive BWT by sorting the rotations of `text + $`: the transformed bytes and, for every
    /// text position, its row.
    fn naive(text: &[u8]) -> (Vec<u8>, Vec<u32>) {
        let n = text.len();
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

    /// The indices of the text, by the definition: the row of every position i · r(n).
    fn indices_of(row_of: &[u32], n: usize) -> Vec<u32> {
        let rate = 1usize << rate_log2(n as u64);
        (0..n).step_by(rate).map(|i| row_of[i]).collect()
    }

    #[test]
    fn inverse_undoes_a_naive_rotation_sort_for_lengths_1_to_200() {
        let mut r = xorshift(5);
        let mut next = Vec::new();
        for len in 1..=200usize {
            for alphabet in [1u64, 2, 3, 26, 256] {
                let text: Vec<u8> = (0..len).map(|_| (r() % alphabet) as u8).collect();
                let (bwt, row_of) = naive(&text);
                let idx = indices_of(&row_of, len);
                assert_eq!(idx.len(), index_count(len as u64) as usize);
                let mut out = vec![0u8; len];
                inverse_block(&bwt, &idx, &mut next, &mut out).unwrap();
                assert_eq!(out, text, "len {len} alphabet {alphabet}");
                // Any single auxiliary index moved to another row is refused, never silently
                // wrong (the walk from the primary index alone would still give the text).
                for j in 1..idx.len() {
                    let mut bad = idx.clone();
                    bad[j] = if bad[j] == 1 { 2 } else { bad[j] - 1 };
                    assert_eq!(
                        inverse_block(&bwt, &bad, &mut next, &mut out),
                        Err(Reason::Inverse),
                        "len {len} index {j}"
                    );
                }
            }
        }
    }

    #[test]
    fn decode_blocks_over_several_blocks_matches_the_naive_transform() {
        let mut r = xorshift(9);
        let text: Vec<u8> = (0..1000).map(|_| b"ab \n"[(r() % 4) as usize]).collect();
        for block_size in [1u64, 2, 7, 64, 100, 999, 1000, 4096] {
            let mut v = Vec::new();
            for block in text.chunks(block_size as usize) {
                let (bwt, row_of) = naive(block);
                for i in indices_of(&row_of, block.len()) {
                    v.extend_from_slice(&i.to_le_bytes());
                }
                v.extend_from_slice(&bwt);
            }
            assert_eq!(v.len() as u64 + OUT_LEN_FIELD, input_len(1000, block_size));
            let mut out = vec![0u8; 1000];
            decode_blocks(&v, 1000, block_size, &mut out).unwrap();
            assert_eq!(out, text, "block size {block_size}");
            // One byte more or less is `length`.
            assert_eq!(
                decode_blocks(&v[..v.len() - 1], 1000, block_size, &mut out),
                Err(Reason::Length)
            );
            let mut longer = v.clone();
            longer.push(7);
            assert_eq!(
                decode_blocks(&longer, 1000, block_size, &mut out),
                Err(Reason::Length)
            );
            // The first fault in stream order decides: an index of 0 in the first block is
            // `index` even when a later block is damaged.
            if block_size < 1000 {
                let mut bad = v.clone();
                bad[0..4].copy_from_slice(&0u32.to_le_bytes());
                let last = bad.len() - 1;
                bad[last] ^= 0x55;
                assert_eq!(
                    decode_blocks(&bad, 1000, block_size, &mut out),
                    Err(Reason::Index)
                );
            }
        }
    }

    #[cfg(feature = "encode")]
    #[test]
    fn encode_then_decode_through_libsais() {
        let mut r = xorshift(21);
        let mut text = Vec::new();
        while text.len() < 300_000 {
            let w = ["the ", "quick ", "brown ", "fox\n", "jumps ", "INFO "];
            text.extend_from_slice(w[(r() % 6) as usize].as_bytes());
        }
        for block_size in [1u32, 100, 4096, 65_536, 1 << 20] {
            let enc = encode(&text, block_size).unwrap();
            assert_eq!(
                enc.len() as u64,
                input_len(text.len() as u64, u64::from(block_size))
            );
            assert_eq!(
                u64::from_le_bytes(enc[..8].try_into().unwrap()),
                text.len() as u64
            );
            let mut out = vec![0u8; text.len()];
            decode_blocks(
                &enc[8..],
                text.len() as u64,
                u64::from(block_size),
                &mut out,
            )
            .unwrap();
            assert_eq!(out, text, "block size {block_size}");
        }
        // The probe's inverse and this one agree on libsais's output.
        let t = crate::forward(&text).unwrap();
        let mut out = vec![0u8; text.len()];
        let mut next = Vec::new();
        inverse_block(&t.bwt, &t.aux, &mut next, &mut out).unwrap();
        assert_eq!(out, crate::inverse(&t).unwrap());
    }

    /// The timing clause of revision 1.3's task C: this inverse against the probe's on a text
    /// the caller names. `LPK_BWT_TEXT` is a file (or a directory, read recursively in path
    /// order), `LPK_BWT_BLOCK` the BWT block size (default 64 MiB); both inverses run on every
    /// block and the summed times are printed. Run in release:
    /// `LPK_BWT_TEXT=<path> cargo test -p lpk-codecs-bwt --release --features encode -- --ignored
    /// --nocapture inverse_timing_against_the_probe`. No figure is written anywhere.
    #[cfg(feature = "encode")]
    #[test]
    #[ignore = "times both inverses on the text LPK_BWT_TEXT names; run in release with --nocapture"]
    fn inverse_timing_against_the_probe() {
        use std::time::{Duration, Instant};
        let path = std::path::PathBuf::from(
            std::env::var_os("LPK_BWT_TEXT").expect("LPK_BWT_TEXT: a file or a directory"),
        );
        let block_size: usize = std::env::var("LPK_BWT_BLOCK")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(64 << 20);
        fn collect(p: &std::path::Path, into: &mut Vec<u8>) {
            if p.is_dir() {
                let mut entries: Vec<_> = std::fs::read_dir(p)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .collect();
                entries.sort();
                for e in entries {
                    collect(&e, into);
                }
            } else {
                into.extend_from_slice(&std::fs::read(p).unwrap());
            }
        }
        let mut text = Vec::new();
        collect(&path, &mut text);
        let (mut ours, mut probe, mut forward) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        let mut next = Vec::new();
        let mut blocks = 0;
        for block in text.chunks(block_size) {
            let t0 = Instant::now();
            let t = crate::forward(block).unwrap();
            forward += t0.elapsed();
            let mut out = vec![0u8; block.len()];
            let t1 = Instant::now();
            inverse_block(&t.bwt, &t.aux, &mut next, &mut out).unwrap();
            ours += t1.elapsed();
            assert_eq!(out, block);
            let t2 = Instant::now();
            let p = crate::inverse(&t).unwrap();
            probe += t2.elapsed();
            assert_eq!(p, block);
            blocks += 1;
        }
        println!(
            "bwt inverse timing: {} bytes in {blocks} blocks of at most {block_size}: \
             forward (libsais) {:.3} s, u32 inverse (revision 1.3) {:.3} s, probe inverse (u64 links) {:.3} s",
            text.len(),
            forward.as_secs_f64(),
            ours.as_secs_f64(),
            probe.as_secs_f64()
        );
    }
}
