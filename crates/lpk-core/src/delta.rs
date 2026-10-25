//! The delta stage's suffix-array patch, format 1 (E2-9; design `docs/notes/e2-9-design.md`,
//! section 3). Bench-only until task C moves the writer onto it: the delta probe (`lpk-bench
//! probe delta`) prices it, and [`apply`] is the reference decoder task D's reader starts from.
//!
//! The patch, in order (varints are the format's canonical unsigned LEB128, spec section 1;
//! `L_b` is the base chunk's length):
//!
//! | Field | Size | Meaning |
//! |---|---|---|
//! | `out_len` | u64 LE | output length |
//! | `cover_count` | varint | number of covers |
//! | per cover: `gap`, `base_off`, `len` | 3 varints | new bytes before the cover; offset in the base; `len` >= 1, `base_off + len` <= `L_b` |
//! | `tail` | varint | new bytes after the last cover |
//! | `new` | sum of gaps + tail | the bytes no cover produces, in output order |
//! | `diff` | sum of lens | per covered byte: out = (base[base_off + i] + diff) mod 256, in cover order |
//!
//! Output: per cover `gap` bytes from `new`, then `len` bytes of the base plus `diff`; then
//! `tail` bytes from `new`. Base spans may overlap; output positions never do. The input length
//! is exactly the layout's.
//!
//! [`apply`] checks, in this order, and stops at the first fault (section 3, checks (3) and (4)
//! of the delta step; the caller does (1), (2), the `out_len` bound and (5)):
//! an input under 8 bytes ([`PatchFault::Short`]); `cover_count` above the input left after
//! it divided by 3 ([`PatchFault::CoverCount`]); per cover, a non-canonical varint
//! ([`PatchFault::Varint`]), `len` 0 ([`PatchFault::ZeroLen`]), a span past `L_b`
//! ([`PatchFault::Span`]); the `tail` varint; the sum of gaps, lens and tail differing from
//! `out_len` ([`PatchFault::Sum`]; the sum saturates, so an overflowing sum differs from any
//! `out_len` below `u64::MAX`, and `out_len = u64::MAX` fails the length check next); an input
//! length other than the layout's ([`PatchFault::Length`]). Input that ends inside a varint is a
//! wrong input length ([`PatchFault::Length`]).
//!
//! Decode memory is the base and the input, no table (section 3): [`parse`] reads the covers
//! once to check them and the sums and keeps none ([`Header`] is `Copy`: it cannot hold one);
//! [`apply`] re-reads them from the patch ([`Header::covers`]) as it writes the output, which is
//! allocated at `out_len` (at most the input's length) only after the header passed.
//!
//! The encoder ([`encode`], over this crate's libsais link): the suffix array of the base chunk
//! (libsais, `lpk_codecs_bwt::suffix_array`) and, in [`covers_with`], a port of the diff loop of
//! Colin Percival's bsdiff 4.3 (`bsdiff.c`, BSD-2-Clause, notice below and in `NOTICE` beside
//! this crate; `docs/LICENSING.md`): per position of the new bytes the longest exact match by
//! binary search over the suffix array; a match replaces the current alignment when it is longer
//! than what the current alignment already matches over the same span by more than 8 bytes (or
//! equals it exactly); between two accepted matches the previous cover is extended forward and
//! the next backward as long as at least half of the bytes agree, the overlap split where the
//! agreement count is best. bsdiff's own patch framing (control, diff and extra streams under
//! bzip2) is not used: the covers are laid out as above. Memory: the base's suffix array (4
//! bytes per base byte), the patch, nothing per new byte beyond the output.
//!
//! The diff loop of [`covers_with`] is ported from bsdiff 4.3, under this notice:
//!
//! ```text
//! Copyright 2003-2005 Colin Percival
//! All rights reserved
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted providing that the following conditions
//! are met:
//! 1. Redistributions of source code must retain the above copyright
//!    notice, this list of conditions and the following disclaimer.
//! 2. Redistributions in binary form must reproduce the above copyright
//!    notice, this list of conditions and the following disclaimer in the
//!    documentation and/or other materials provided with the distribution.
//!
//! THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
//! IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
//! WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
//! ARE DISCLAIMED.  IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
//! DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
//! DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS
//! OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
//! HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT,
//! STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING
//! IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
//! POSSIBILITY OF SUCH DAMAGE.
//! ```

use std::fmt;

/// The `patch_format` value of the suffix-array patch.
pub const PATCH_FORMAT_SA: u8 = 1;

/// The smallest patch: `out_len` alone.
pub const MIN_PATCH: usize = 8;

/// A fault in a patch (the `DeltaError` `patch` sub-reasons of the design, plus `length`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchFault {
    /// The input is shorter than 8 bytes.
    Short,
    /// `cover_count` above the input left divided by 3.
    CoverCount,
    /// A non-canonical varint, or one longer than ten bytes.
    Varint,
    /// A cover of length 0.
    ZeroLen,
    /// A cover's base span past the base's end.
    Span,
    /// The gaps, lens and tail do not sum to `out_len` (or the sum overflows).
    Sum,
    /// The input is not exactly as long as the layout says (or ends inside a varint).
    Length,
}

impl PatchFault {
    /// The sub-reason string.
    pub fn reason(self) -> &'static str {
        match self {
            PatchFault::Short => "length",
            PatchFault::CoverCount => "cover count",
            PatchFault::Varint => "varint",
            PatchFault::ZeroLen => "len",
            PatchFault::Span => "span",
            PatchFault::Sum => "sum",
            PatchFault::Length => "length",
        }
    }
}

impl fmt::Display for PatchFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "delta patch: {}", self.reason())
    }
}

impl std::error::Error for PatchFault {}

/// One cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cover {
    /// New bytes before the cover.
    pub gap: u64,
    /// Offset of the cover in the base.
    pub base_off: u64,
    /// Length of the cover (at least 1).
    pub len: u64,
}

/// A checked patch header. It holds no cover table (it is `Copy`): the covers are re-read from
/// the patch by [`Header::covers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The output length.
    pub out_len: u64,
    /// The number of covers.
    pub cover_count: u64,
    /// Where the first cover starts in the patch.
    pub covers_at: usize,
    /// New bytes after the last cover.
    pub tail: u64,
    /// Where the `new` bytes start in the patch.
    pub new_at: usize,
    /// Where the `diff` bytes start in the patch.
    pub diff_at: usize,
}

impl Header {
    /// The covers of `patch` (the patch this header was parsed from), in output order.
    pub fn covers<'p>(&self, patch: &'p [u8]) -> Covers<'p> {
        Covers {
            patch,
            at: self.covers_at,
            left: self.cover_count,
        }
    }
}

/// The covers of a checked patch, read one at a time ([`Header::covers`]).
#[derive(Debug, Clone)]
pub struct Covers<'p> {
    patch: &'p [u8],
    at: usize,
    left: u64,
}

impl Iterator for Covers<'_> {
    type Item = Cover;

    fn next(&mut self) -> Option<Cover> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        let gap = read_varint(self.patch, &mut self.at).ok()?;
        let base_off = read_varint(self.patch, &mut self.at).ok()?;
        let len = read_varint(self.patch, &mut self.at).ok()?;
        Some(Cover { gap, base_off, len })
    }
}

/// Read a canonical varint at `*at`: [`PatchFault::Length`] when the input ends inside it,
/// [`PatchFault::Varint`] when it is not the shortest form or continues past ten bytes.
fn read_varint(input: &[u8], at: &mut usize) -> Result<u64, PatchFault> {
    let mut v: u64 = 0;
    for i in 0..10 {
        let Some(&b) = input.get(*at) else {
            return Err(PatchFault::Length);
        };
        *at += 1;
        if i == 9 {
            // The tenth byte carries bit 63 only and ends the varint.
            if b != 0x01 {
                return Err(PatchFault::Varint);
            }
            return Ok(v | (1u64 << 63));
        }
        v |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            if i > 0 && b == 0 {
                return Err(PatchFault::Varint);
            }
            return Ok(v);
        }
    }
    Err(PatchFault::Varint)
}

/// Append `v` as a canonical varint.
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Read one cover at `*at` and check it against a base of `base_len` bytes.
fn read_cover(patch: &[u8], at: &mut usize, base_len: u64) -> Result<Cover, PatchFault> {
    let gap = read_varint(patch, at)?;
    let base_off = read_varint(patch, at)?;
    let len = read_varint(patch, at)?;
    if len == 0 {
        return Err(PatchFault::ZeroLen);
    }
    match base_off.checked_add(len) {
        Some(end) if end <= base_len => Ok(Cover { gap, base_off, len }),
        _ => Err(PatchFault::Span),
    }
}

/// Parse and check a patch's header against a base of `base_len` bytes (the checks of the
/// module documentation, in order), reading every cover once and keeping none.
pub fn parse(patch: &[u8], base_len: u64) -> Result<Header, PatchFault> {
    if patch.len() < MIN_PATCH {
        return Err(PatchFault::Short);
    }
    let mut le = [0u8; 8];
    le.copy_from_slice(&patch[..8]);
    let out_len = u64::from_le_bytes(le);
    let mut at = 8usize;
    let cover_count = read_varint(patch, &mut at)?;
    if cover_count > ((patch.len() - at) / 3) as u64 {
        return Err(PatchFault::CoverCount);
    }
    let covers_at = at;
    let (mut new_total, mut diff_total) = (0u64, 0u64);
    for _ in 0..cover_count {
        let c = read_cover(patch, &mut at, base_len)?;
        new_total = new_total.saturating_add(c.gap);
        diff_total = diff_total.saturating_add(c.len);
    }
    let tail = read_varint(patch, &mut at)?;
    new_total = new_total.saturating_add(tail);
    if new_total.saturating_add(diff_total) != out_len {
        return Err(PatchFault::Sum);
    }
    if (at as u64).checked_add(out_len) != Some(patch.len() as u64) {
        return Err(PatchFault::Length);
    }
    // The layout fits the input: both totals are at most `out_len`, which fits after `at`.
    let diff_at = at + new_total as usize;
    Ok(Header {
        out_len,
        cover_count,
        covers_at,
        tail,
        new_at: at,
        diff_at,
    })
}

/// Apply a format-1 patch to `base`: the output bytes, or the first fault. The covers are
/// read twice (checked by [`parse`], then applied); nothing but the output is allocated.
pub fn apply(base: &[u8], patch: &[u8]) -> Result<Vec<u8>, PatchFault> {
    let h = parse(patch, base.len() as u64)?;
    // `parse` proved the layout fits the input: every length below is within it.
    let mut out = Vec::with_capacity(h.out_len as usize);
    let mut new_at = h.new_at;
    let mut diff_at = h.diff_at;
    for c in h.covers(patch) {
        let gap = c.gap as usize;
        out.extend_from_slice(&patch[new_at..new_at + gap]);
        new_at += gap;
        let (off, len) = (c.base_off as usize, c.len as usize);
        let span = &base[off..off + len];
        let diff = &patch[diff_at..diff_at + len];
        out.extend(span.iter().zip(diff).map(|(b, d)| b.wrapping_add(*d)));
        diff_at += len;
    }
    out.extend_from_slice(&patch[new_at..new_at + h.tail as usize]);
    Ok(out)
}

/// Lay out a patch from its covers (each `(new position, base offset, length)`, in output
/// order, not overlapping in the output, inside both inputs, length at least 1).
pub fn layout(base: &[u8], new: &[u8], covers: &[(usize, usize, usize)]) -> Vec<u8> {
    let covered: usize = covers.iter().map(|c| c.2).sum();
    let mut head = Vec::with_capacity(16 + covers.len() * 9);
    head.extend_from_slice(&(new.len() as u64).to_le_bytes());
    put_varint(&mut head, covers.len() as u64);
    let mut fresh = Vec::with_capacity(new.len() - covered);
    let mut diff = Vec::with_capacity(covered);
    let mut prev = 0usize;
    for &(at, off, len) in covers {
        put_varint(&mut head, (at - prev) as u64);
        put_varint(&mut head, off as u64);
        put_varint(&mut head, len as u64);
        fresh.extend_from_slice(&new[prev..at]);
        diff.extend(
            new[at..at + len]
                .iter()
                .zip(&base[off..off + len])
                .map(|(n, b)| n.wrapping_sub(*b)),
        );
        prev = at + len;
    }
    put_varint(&mut head, (new.len() - prev) as u64);
    fresh.extend_from_slice(&new[prev..]);
    head.reserve(fresh.len() + diff.len());
    head.extend_from_slice(&fresh);
    head.extend_from_slice(&diff);
    head
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// The longest match of a prefix of `new` among the suffixes of `base` (`sa` its suffix array,
/// not empty): binary search, then the better of the two neighbours it ends between.
fn longest_match(sa: &[i32], base: &[u8], new: &[u8]) -> (usize, usize) {
    let (mut lo, mut hi) = (0usize, sa.len() - 1);
    while hi - lo >= 2 {
        let mid = lo + (hi - lo) / 2;
        let s = &base[sa[mid] as usize..];
        let n = s.len().min(new.len());
        if s[..n] < new[..n] {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (a, b) = (sa[lo] as usize, sa[hi] as usize);
    let (la, lb) = (
        common_prefix(&base[a..], new),
        common_prefix(&base[b..], new),
    );
    if la >= lb {
        (a, la)
    } else {
        (b, lb)
    }
}

/// The byte of `base` at `pos + offset`, when inside it.
fn at_offset(base: &[u8], pos: usize, offset: isize) -> Option<u8> {
    let o = (pos as isize).checked_add(offset)?;
    usize::try_from(o).ok().and_then(|o| base.get(o).copied())
}

/// The covers of `new` against `base` given the base's suffix array (see the module
/// documentation for the method), as `(new position, base offset, length)`.
///
/// Ported from the diff loop of bsdiff 4.3 (`bsdiff.c`, Copyright 2003-2005 Colin Percival,
/// BSD-2-Clause; the notice is in the module documentation and in this crate's `NOTICE`): the
/// scan, the forward and backward extension and the overlap split follow it; the output is the
/// covers instead of bsdiff's control triples.
pub fn covers_with(base: &[u8], sa: &[i32], new: &[u8]) -> Vec<(usize, usize, usize)> {
    let (n, m) = (new.len(), base.len());
    let mut covers = Vec::new();
    if n == 0 || m == 0 || sa.len() != m {
        return covers;
    }
    let (mut scan, mut len, mut pos) = (0usize, 0usize, 0usize);
    let (mut last_scan, mut last_pos) = (0usize, 0usize);
    let mut last_offset: isize = 0;
    while scan < n {
        let mut old_score: isize = 0;
        scan += len;
        let mut scsc = scan;
        while scan < n {
            let (p, l) = longest_match(sa, base, &new[scan..]);
            pos = p;
            len = l;
            while scsc < scan + len {
                if at_offset(base, scsc, last_offset) == Some(new[scsc]) {
                    old_score += 1;
                }
                scsc += 1;
            }
            let l = len as isize;
            if (l == old_score && len != 0) || l > old_score + 8 {
                break;
            }
            if at_offset(base, scan, last_offset) == Some(new[scan]) {
                old_score -= 1;
            }
            scan += 1;
        }
        if len as isize != old_score || scan == n {
            // Extend the previous alignment forward while at least half of the bytes agree.
            let (mut s, mut best, mut len_f) = (0isize, 0isize, 0usize);
            let mut i = 0usize;
            while last_scan + i < scan && last_pos + i < m {
                if base[last_pos + i] == new[last_scan + i] {
                    s += 1;
                }
                i += 1;
                if s * 2 - i as isize > best * 2 - len_f as isize {
                    best = s;
                    len_f = i;
                }
            }
            // Extend the new match backward likewise.
            let mut len_b = 0usize;
            if scan < n {
                let (mut s, mut best) = (0isize, 0isize);
                let mut i = 1usize;
                while scan >= last_scan + i && pos >= i {
                    if base[pos - i] == new[scan - i] {
                        s += 1;
                    }
                    if s * 2 - i as isize > best * 2 - len_b as isize {
                        best = s;
                        len_b = i;
                    }
                    i += 1;
                }
            }
            // Split an overlap where the agreement is best.
            if last_scan + len_f > scan - len_b {
                let overlap = (last_scan + len_f) - (scan - len_b);
                let (mut s, mut best, mut len_s) = (0isize, 0isize, 0usize);
                for i in 0..overlap {
                    if new[last_scan + len_f - overlap + i] == base[last_pos + len_f - overlap + i]
                    {
                        s += 1;
                    }
                    if new[scan - len_b + i] == base[pos - len_b + i] {
                        s -= 1;
                    }
                    if s > best {
                        best = s;
                        len_s = i + 1;
                    }
                }
                len_f = len_f + len_s - overlap;
                len_b -= len_s;
            }
            if len_f > 0 {
                covers.push((last_scan, last_pos, len_f));
            }
            last_scan = scan - len_b;
            last_pos = pos - len_b;
            last_offset = pos as isize - scan as isize;
        }
    }
    covers
}

/// The format-1 patch of `new` against `base`, given the base's suffix array.
pub fn encode_with(base: &[u8], sa: &[i32], new: &[u8]) -> Vec<u8> {
    layout(base, new, &covers_with(base, sa, new))
}

/// The format-1 patch of `new` against `base` (the base's suffix array built with libsais).
pub fn encode(base: &[u8], new: &[u8]) -> Result<Vec<u8>, lpk_codecs_bwt::BwtError> {
    let sa = lpk_codecs_bwt::suffix_array(base)?;
    Ok(encode_with(base, &sa, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// A version of `base` with edits: replaced runs, inserted and deleted bytes, moved blocks
    /// and single-byte changes.
    fn edit(rng: &mut Rng, base: &[u8], edits: usize) -> Vec<u8> {
        let mut v = base.to_vec();
        for _ in 0..edits {
            let at = rng.below(v.len() + 1);
            match rng.below(5) {
                0 => {
                    let n = rng.below(64).min(v.len() - at.min(v.len()));
                    let r = rng.bytes(n);
                    v[at..at + n].copy_from_slice(&r);
                }
                1 => {
                    let k = rng.below(200);
                    let r = rng.bytes(k);
                    v.splice(at..at, r);
                }
                2 => {
                    let n = rng.below(300).min(v.len() - at);
                    v.drain(at..at + n);
                }
                3 if !v.is_empty() => {
                    let i = rng.below(v.len());
                    v[i] = v[i].wrapping_add(1 + rng.below(3) as u8);
                }
                _ => {
                    let n = rng.below(500).min(v.len() - at);
                    let moved: Vec<u8> = v.drain(at..at + n).collect();
                    let to = rng.below(v.len() + 1);
                    v.splice(to..to, moved);
                }
            }
        }
        v
    }

    fn round_trip(base: &[u8], new: &[u8]) -> Vec<u8> {
        let p = encode(base, new).expect("encode");
        let h = parse(&p, base.len() as u64).expect("parse");
        assert_eq!(h.out_len, new.len() as u64);
        assert_eq!(apply(base, &p).expect("apply"), new);
        p
    }

    #[test]
    fn crafted_inputs_round_trip() {
        let text = b"the quick brown fox jumps over the lazy dog; ".repeat(40);
        let cases: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (vec![], vec![]),
            (vec![], b"no base at all".to_vec()),
            (b"a base with no output".to_vec(), vec![]),
            (vec![7], vec![7]),
            (vec![7], vec![8]),
            (vec![0; 1000], vec![0; 1000]),
            (vec![0; 1000], vec![0; 3000]),
            (vec![0; 3000], vec![0; 10]),
            (text.clone(), text.clone()),
            (text.clone(), text[100..].to_vec()),
            (text.clone(), [b"prefix".as_slice(), &text].concat()),
            ((0..=255u8).collect(), (0..=255u8).rev().collect()),
            (b"abcabcabcabc".to_vec(), b"cbacbacba".to_vec()),
        ];
        for (base, new) in &cases {
            round_trip(base, new);
        }
    }

    #[test]
    fn identical_input_is_one_cover_with_zero_diffs() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let base = rng.bytes(50_000);
        let p = round_trip(&base, &base);
        let h = parse(&p, base.len() as u64).expect("parse");
        assert_eq!(
            h.covers(&p).collect::<Vec<_>>(),
            [Cover {
                gap: 0,
                base_off: 0,
                len: 50_000
            }]
        );
        assert!(p[h.diff_at..].iter().all(|&d| d == 0));
    }

    #[test]
    fn random_inputs_round_trip_and_edits_stay_small() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for round in 0..60 {
            let n = match round % 4 {
                0 => rng.below(64),
                1 => rng.below(4096),
                _ => 1000 + rng.below(70_000),
            };
            let base = if round % 5 == 0 {
                // Low-entropy text, many repeats.
                (0..n).map(|_| b"abcd\n"[rng.below(5)]).collect()
            } else {
                rng.bytes(n)
            };
            let k = 1 + rng.below(12);
            let new = edit(&mut rng, &base, k);
            let p = round_trip(&base, &new);
            if round % 4 >= 2 && round % 5 != 0 {
                // An edited random chunk: the patch's `new` bytes are a small part of it.
                let h = parse(&p, base.len() as u64).expect("parse");
                let fresh = h.diff_at - h.new_at;
                assert!(
                    fresh * 4 < new.len(),
                    "round {round}: {fresh} of {}",
                    new.len()
                );
            }
            // Unrelated data round-trips too.
            let k = rng.below(5000);
            let other = rng.bytes(k);
            round_trip(&base, &other);
        }
    }

    fn header(out_len: u64, count: u64) -> Vec<u8> {
        let mut p = out_len.to_le_bytes().to_vec();
        put_varint(&mut p, count);
        p
    }

    #[test]
    fn every_fault_in_order() {
        let base = [1u8, 2, 3, 4];
        // Under 8 bytes.
        assert_eq!(apply(&base, &[0; 7]), Err(PatchFault::Short));
        // cover_count above the input left / 3.
        let mut p = header(0, 2);
        p.extend_from_slice(&[0; 5]);
        assert_eq!(apply(&base, &p), Err(PatchFault::CoverCount));
        // A non-canonical varint (0x80 0x00), in a cover and as cover_count.
        let mut p = header(1, 1);
        p.extend_from_slice(&[0x80, 0x00, 0, 1, 0]);
        assert_eq!(apply(&base, &p), Err(PatchFault::Varint));
        let mut p = 1u64.to_le_bytes().to_vec();
        p.extend_from_slice(&[0x81, 0x00, 0, 0, 0, 0]);
        assert_eq!(apply(&base, &p), Err(PatchFault::Varint));
        // A tenth byte other than 0x01.
        let mut p = header(0, 0);
        p.extend_from_slice(&[0xff; 9]);
        p.push(0x02);
        assert_eq!(apply(&base, &p), Err(PatchFault::Varint));
        // len 0.
        let mut p = header(0, 1);
        p.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(apply(&base, &p), Err(PatchFault::ZeroLen));
        // A span past L_b, and one whose end overflows.
        let mut p = header(2, 1);
        p.extend_from_slice(&[0, 3, 2, 0, 0, 0]);
        assert_eq!(apply(&base, &p), Err(PatchFault::Span));
        let mut p = header(1, 1);
        p.push(0);
        put_varint(&mut p, u64::MAX);
        p.extend_from_slice(&[1, 0, 0]);
        assert_eq!(apply(&base, &p), Err(PatchFault::Span));
        // The sum differs from out_len; an overflowing sum.
        let mut p = header(3, 1);
        p.extend_from_slice(&[0, 0, 2, 0, 9, 9]);
        assert_eq!(apply(&base, &p), Err(PatchFault::Sum));
        let mut p = header(0, 1);
        put_varint(&mut p, u64::MAX);
        p.extend_from_slice(&[0, 1]);
        put_varint(&mut p, 5);
        assert_eq!(apply(&base, &p), Err(PatchFault::Sum));
        // The sum saturates: with out_len = u64::MAX an overflowing sum equals it, and the
        // length check refuses the patch.
        let mut p = header(u64::MAX, 2);
        p.push(0);
        put_varint(&mut p, 0);
        put_varint(&mut p, 1);
        put_varint(&mut p, u64::MAX);
        p.extend_from_slice(&[0, 1, 0]);
        assert_eq!(apply(&base, &p), Err(PatchFault::Length));
        // A wrong input length: one byte short, one byte over, ending inside a varint.
        let good = encode(&base, &[1, 2, 9, 4, 5]).expect("encode");
        assert_eq!(apply(&base, &good).expect("apply"), [1, 2, 9, 4, 5]);
        assert_eq!(
            apply(&base, &good[..good.len() - 1]),
            Err(PatchFault::Length)
        );
        let mut over = good.clone();
        over.push(0);
        assert_eq!(apply(&base, &over), Err(PatchFault::Length));
        let mut p = header(0, 0);
        p.push(0x80);
        assert_eq!(apply(&base, &p), Err(PatchFault::Length));
        // A cover count fault comes before a varint fault later on.
        let mut p = header(0, 3);
        p.extend_from_slice(&[0x80, 0x00, 0, 0, 0, 0]);
        assert_eq!(apply(&base, &p), Err(PatchFault::CoverCount));
        assert_eq!(PatchFault::ZeroLen.reason(), "len");
    }

    #[test]
    fn many_short_covers_decode_without_a_table() {
        // The header cannot hold a table (it is Copy); 100,000 one-byte covers of 3-byte
        // varint triples decode against a one-byte base.
        fn copy<T: Copy>(_: &T) {}
        let n = 100_000u64;
        let mut p = header(n, n);
        for _ in 0..n {
            p.extend_from_slice(&[0, 0, 1]);
        }
        p.push(0);
        p.extend(std::iter::repeat_n(1u8, n as usize));
        let h = parse(&p, 1).expect("parse");
        copy(&h);
        assert_eq!(h.cover_count, n);
        assert_eq!(apply(&[41], &p).expect("apply"), vec![42u8; n as usize]);
    }

    #[test]
    fn a_hand_written_patch_decodes_as_the_text_says() {
        // base "ABCDEFGH"; output: gap "xy", cover base[2..5] + diff [0,1,0] = "CEE", gap "",
        // cover base[0..2] + 0 = "AB" (overlapping base spans are allowed), tail "z".
        let base = b"ABCDEFGH";
        let mut p = header(8, 2);
        p.extend_from_slice(&[2, 2, 3, 0, 0, 2, 1]);
        p.extend_from_slice(b"xyz");
        p.extend_from_slice(&[0, 1, 0, 0, 0]);
        assert_eq!(apply(base, &p).expect("apply"), b"xyCEEABz");
    }
}
