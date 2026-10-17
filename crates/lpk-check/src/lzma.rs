//! An `lzma` step (section 8, "Normative decoders" and "What the reference decoder enforces for
//! `lzma`"): a raw LZMA1 decoder written from the LZMA SDK's `lzma-specification.txt` (`LzmaSpec`),
//! with the stream-end rules of section 8 instead of `LzmaSpec`'s own (rules 3 to 6).
//!
//! The decoder checks the step's output bound before every symbol, so a non-final step whose
//! output crosses its bound is `PayloadTooLarge` at the crossing, before any later error of its
//! stream (rules 3 and 6; CHANGES revision 1.2 item 7).

use crate::block::too_large;
use crate::error::{Error, Result};

fn lzerr(reason: &str) -> Error {
    Error::new("LzmaError", format!("lzma: {reason}"))
}

/// Why the range decoder could not go on.
enum Stop {
    /// The input ended where the decoder needed a byte.
    Eof,
}

struct RangeDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

const PROB_INIT: u16 = 1 << 10;

impl RangeDecoder<'_> {
    fn byte(&mut self) -> std::result::Result<u8, Stop> {
        let b = *self.data.get(self.pos).ok_or(Stop::Eof)?;
        self.pos += 1;
        Ok(b)
    }

    fn normalize(&mut self) -> std::result::Result<(), Stop> {
        if self.range < (1 << 24) {
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(self.byte()?);
        }
        Ok(())
    }

    fn bit(&mut self, p: &mut u16) -> std::result::Result<u32, Stop> {
        let bound = (self.range >> 11) * u32::from(*p);
        let bit = if self.code < bound {
            *p += ((1 << 11) - *p) >> 5;
            self.range = bound;
            0
        } else {
            *p -= *p >> 5;
            self.code -= bound;
            self.range -= bound;
            1
        };
        self.normalize()?;
        Ok(bit)
    }

    fn direct(&mut self, n: u32) -> std::result::Result<u32, Stop> {
        let mut res = 0u32;
        for _ in 0..n {
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);
            let t = 0u32.wrapping_sub(self.code >> 31);
            self.code = self.code.wrapping_add(self.range & t);
            self.normalize()?;
            res = (res << 1).wrapping_add(t.wrapping_add(1));
        }
        Ok(res)
    }

    fn tree(&mut self, probs: &mut [u16], n: u32) -> std::result::Result<u32, Stop> {
        let mut m = 1usize;
        for _ in 0..n {
            m = (m << 1) + self.bit(&mut probs[m])? as usize;
        }
        Ok(m as u32 - (1 << n))
    }

    fn reverse(&mut self, probs: &mut [u16], n: u32) -> std::result::Result<u32, Stop> {
        let mut m = 1usize;
        let mut sym = 0u32;
        for i in 0..n {
            let b = self.bit(&mut probs[m])?;
            m = (m << 1) + b as usize;
            sym |= b << i;
        }
        Ok(sym)
    }
}

struct LenDecoder {
    choice: u16,
    choice2: u16,
    low: Vec<[u16; 8]>,
    mid: Vec<[u16; 8]>,
    high: [u16; 256],
}

impl LenDecoder {
    fn new() -> Self {
        Self {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: vec![[PROB_INIT; 8]; 16],
            mid: vec![[PROB_INIT; 8]; 16],
            high: [PROB_INIT; 256],
        }
    }

    fn decode(
        &mut self,
        rc: &mut RangeDecoder<'_>,
        pos_state: usize,
    ) -> std::result::Result<u32, Stop> {
        if rc.bit(&mut self.choice)? == 0 {
            return rc.tree(&mut self.low[pos_state], 3);
        }
        if rc.bit(&mut self.choice2)? == 0 {
            return Ok(8 + rc.tree(&mut self.mid[pos_state], 3)?);
        }
        Ok(16 + rc.tree(&mut self.high, 8)?)
    }
}

/// What the next symbol is, after its bits are decoded.
enum Symbol {
    Literal(u8),
    /// A match or a rep match: length and distance (1-based).
    Match(usize, usize),
    /// The end-of-payload marker.
    Marker,
}

struct Decoder {
    lc: u32,
    lp: u32,
    pb: u32,
    dict_size: usize,
    literal: Vec<u16>,
    is_match: [u16; 192],
    is_rep: [u16; 12],
    is_rep_g0: [u16; 12],
    is_rep_g1: [u16; 12],
    is_rep_g2: [u16; 12],
    is_rep0_long: [u16; 192],
    pos_slot: [[u16; 64]; 4],
    pos_special: [u16; 115],
    align: [u16; 16],
    len: LenDecoder,
    rep_len: LenDecoder,
    state: usize,
    rep: [u32; 4],
}

impl Decoder {
    /// The next symbol, given the output so far (rule 3's distance checks included).
    fn symbol(
        &mut self,
        rc: &mut RangeDecoder<'_>,
        out: &[u8],
    ) -> std::result::Result<std::result::Result<Symbol, Error>, Stop> {
        let total = out.len();
        let pos_state = total & ((1 << self.pb) - 1);
        let st = self.state;
        if rc.bit(&mut self.is_match[(st << 4) + pos_state])? == 0 {
            // Literal.
            let prev = out.last().copied().map_or(0u32, u32::from);
            let lit_state =
                ((total & ((1 << self.lp) - 1)) << self.lc) + (prev >> (8 - self.lc)) as usize;
            let probs = &mut self.literal[0x300 * lit_state..0x300 * (lit_state + 1)];
            let mut symbol = 1usize;
            if st >= 7 {
                let d = self.rep[0] as usize + 1;
                let mut match_byte = match total.checked_sub(d) {
                    Some(i) => u32::from(out[i]),
                    None => return Ok(Err(lzerr("match byte beyond output"))),
                };
                while symbol < 0x100 {
                    let match_bit = ((match_byte >> 7) & 1) as usize;
                    match_byte <<= 1;
                    let b = rc.bit(&mut probs[((1 + match_bit) << 8) + symbol])? as usize;
                    symbol = (symbol << 1) | b;
                    if match_bit != b {
                        break;
                    }
                }
            }
            while symbol < 0x100 {
                symbol = (symbol << 1) | rc.bit(&mut probs[symbol])? as usize;
            }
            self.state = if st < 4 {
                0
            } else if st < 10 {
                st - 3
            } else {
                st - 6
            };
            return Ok(Ok(Symbol::Literal((symbol - 0x100) as u8)));
        }
        let len;
        if rc.bit(&mut self.is_rep[st])? != 0 {
            if total == 0 {
                return Ok(Err(lzerr("rep match at the start of the stream")));
            }
            if rc.bit(&mut self.is_rep_g0[st])? == 0 {
                if rc.bit(&mut self.is_rep0_long[(st << 4) + pos_state])? == 0 {
                    // Short rep: one byte at rep0.
                    self.state = if st < 7 { 9 } else { 11 };
                    return Ok(self.checked(1, self.rep[0]));
                }
            } else {
                let dist;
                if rc.bit(&mut self.is_rep_g1[st])? == 0 {
                    dist = self.rep[1];
                } else {
                    if rc.bit(&mut self.is_rep_g2[st])? == 0 {
                        dist = self.rep[2];
                    } else {
                        dist = self.rep[3];
                        self.rep[3] = self.rep[2];
                    }
                    self.rep[2] = self.rep[1];
                }
                self.rep[1] = self.rep[0];
                self.rep[0] = dist;
            }
            len = self.rep_len.decode(rc, pos_state)?;
            self.state = if st < 7 { 8 } else { 11 };
        } else {
            self.rep[3] = self.rep[2];
            self.rep[2] = self.rep[1];
            self.rep[1] = self.rep[0];
            len = self.len.decode(rc, pos_state)?;
            self.state = if st < 7 { 7 } else { 10 };
            let d = self.distance(rc, len)?;
            self.rep[0] = d;
            if d == 0xFFFF_FFFF {
                return Ok(Ok(Symbol::Marker));
            }
        }
        Ok(self.checked(len as usize + 2, self.rep[0]))
    }

    /// Rule 3: a distance beyond `dict_size` or beyond the bytes produced so far is `LzmaError`.
    fn checked(&self, len: usize, rep0: u32) -> std::result::Result<Symbol, Error> {
        if rep0 as usize >= self.dict_size {
            return Err(lzerr("match distance beyond dict_size"));
        }
        Ok(Symbol::Match(len, rep0 as usize + 1))
    }

    fn distance(&mut self, rc: &mut RangeDecoder<'_>, len: u32) -> std::result::Result<u32, Stop> {
        let len_state = len.min(3) as usize;
        let slot = rc.tree(&mut self.pos_slot[len_state], 6)?;
        if slot < 4 {
            return Ok(slot);
        }
        let direct = (slot >> 1) - 1;
        let mut dist = (2 | (slot & 1)) << direct;
        if slot < 14 {
            let base = (dist - slot) as usize;
            dist += rc.reverse(&mut self.pos_special[base..], direct)?;
        } else {
            dist = dist.wrapping_add(rc.direct(direct - 4)? << 4);
            dist = dist.wrapping_add(rc.reverse(&mut self.align, 4)?);
        }
        Ok(dist)
    }
}

/// Decodes a raw LZMA1 stream with the properties `[lc, lp, pb]` into at most `bound` bytes. In
/// the last step the output must be exactly `bound` bytes; a step that is not the last has only
/// the bound and its stream must end with the end-of-payload marker. Rules 1 and 2 are checked by
/// the caller.
pub fn decode(
    input: &[u8],
    dict_size: u32,
    props: [u8; 3],
    bound: u64,
    last: bool,
) -> Result<Vec<u8>> {
    let [lc, lp, pb] = props.map(u32::from);
    // Rule 4: the first byte of the range coder stream is 0.
    match input.first() {
        None => return Err(lzerr("truncated")),
        Some(0) => {}
        Some(_) => return Err(lzerr("range coder")),
    }
    let mut rc = RangeDecoder {
        data: input,
        pos: 1,
        range: 0xFFFF_FFFF,
        code: 0,
    };
    for _ in 0..4 {
        let b = rc.byte().map_err(|_| lzerr("truncated"))?;
        rc.code = (rc.code << 8) | u32::from(b);
    }
    // LzmaSpec raises a dictionary below 4 KiB to 4 KiB.
    let dict_size = (dict_size as usize).max(1 << 12);
    let mut d = Decoder {
        lc,
        lp,
        pb,
        dict_size,
        literal: vec![PROB_INIT; 0x300 << (lc + lp)],
        is_match: [PROB_INIT; 192],
        is_rep: [PROB_INIT; 12],
        is_rep_g0: [PROB_INIT; 12],
        is_rep_g1: [PROB_INIT; 12],
        is_rep_g2: [PROB_INIT; 12],
        is_rep0_long: [PROB_INIT; 192],
        pos_slot: [[PROB_INIT; 64]; 4],
        pos_special: [PROB_INIT; 115],
        align: [PROB_INIT; 16],
        len: LenDecoder::new(),
        rep_len: LenDecoder::new(),
        state: 0,
        rep: [0; 4],
    };
    let mut out: Vec<u8> = Vec::with_capacity(usize::try_from(bound.min(1 << 20)).unwrap_or(0));
    loop {
        let at_bound = out.len() as u64 == bound;
        if at_bound && rc.pos == input.len() {
            // The input ends at the bound without a marker: the last step accepts it (rule 4: the
            // range coder's end condition is not checked); any other step needs the marker.
            return if last {
                Ok(out)
            } else {
                Err(lzerr("marker required"))
            };
        }
        let sym = match d.symbol(&mut rc, &out) {
            Err(Stop::Eof) => {
                return Err(lzerr(match (at_bound, last) {
                    (true, true) => "trailing input",
                    (true, false) => "marker required",
                    (false, _) => "truncated",
                }))
            }
            Ok(Err(e)) => {
                // In the last step at the bound, whatever follows is not the marker.
                return Err(if at_bound && last {
                    lzerr("trailing input")
                } else {
                    e
                });
            }
            Ok(Ok(s)) => s,
        };
        match sym {
            Symbol::Marker => {
                // Rule 5: in the last step a marker before the bound is `truncated`, whatever
                // follows it (input after it, or an end condition that fails).
                if last && !at_bound {
                    return Err(lzerr("truncated"));
                }
                // Rule 4: after the marker the range coder's code is 0. In the last step with the
                // marker at the bound a failing end condition is `trailing input` (rule 5); in a
                // step that is not the last the reason is not normative.
                if rc.code != 0 {
                    return Err(lzerr(if last {
                        "trailing input"
                    } else {
                        "range coder end condition after the marker"
                    }));
                }
                // Input after the marker, wherever it comes, is `trailing input`.
                if rc.pos != input.len() {
                    return Err(lzerr("trailing input"));
                }
                return Ok(out);
            }
            _ if at_bound && last => return Err(lzerr("trailing input")),
            Symbol::Literal(b) => {
                if at_bound {
                    return Err(too_large(last, bound + 1, bound));
                }
                out.push(b);
            }
            Symbol::Match(len, dist) => {
                // Rule 3 (beyond the bytes produced so far) before rule 6 (crossing the bound).
                if dist > out.len() {
                    return Err(lzerr("match distance beyond the output"));
                }
                let end = out.len() as u64 + len as u64;
                if end > bound {
                    return Err(too_large(last, end, bound));
                }
                let start = out.len() - dist;
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// A raw LZMA1 stream (lc 3, lp 0, pb 2) of `plain` that ends with the end-of-payload marker.
    fn marked_stream(plain: &[u8]) -> (Vec<u8>, u32) {
        let mut out = Vec::new();
        lzma_rs::lzma_compress(&mut std::io::Cursor::new(plain), &mut out).unwrap();
        // `.lzma` header: properties, dictionary size, then an unknown size (all ones), so the
        // stream carries the marker.
        assert_eq!(out[0], (2 * 5) * 9 + 3);
        assert_eq!(out[5..13], [0xFF; 8]);
        let dict = u32::from_le_bytes(out[1..5].try_into().unwrap());
        (out[13..].to_vec(), dict)
    }

    fn plain() -> Vec<u8> {
        let mut v = Vec::new();
        let mut x = 0x1234_5678u32;
        for i in 0..20_000u32 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            // Repetitive enough for matches, varied enough for literals.
            v.push(if i % 7 < 4 {
                b"abcdefgh"[(i % 8) as usize]
            } else {
                (x % 61) as u8
            });
        }
        v
    }

    const P: [u8; 3] = [3, 0, 2];

    #[test]
    fn marker_before_the_declared_length_is_truncated() {
        let plain = b"an lzma stream with its end marker, then a block that claims one byte more";
        let (enc, dict) = marked_stream(plain);
        let n = plain.len() as u64;
        assert_eq!(decode(&enc, dict, P, n, true).unwrap(), plain);
        let err = decode(&enc, dict, P, n + 1, true).unwrap_err();
        assert_eq!(err.detail, "lzma: truncated");
        // A step that is not the last is bounded only, and the marker ends it there.
        assert_eq!(decode(&enc, dict, P, n + 1, false).unwrap(), plain);
        assert_eq!(decode(&enc, dict, P, n, false).unwrap(), plain);
    }

    #[test]
    fn round_trip_and_damage_classes() {
        let plain = plain();
        let (enc, dict) = marked_stream(&plain);
        let n = plain.len() as u64;
        assert_eq!(decode(&enc, dict, P, n, true).unwrap(), plain);
        // Rule 5: input after the marker.
        let mut extra = enc.clone();
        extra.push(0);
        assert_eq!(
            decode(&extra, dict, P, n, true).unwrap_err().detail,
            "lzma: trailing input"
        );
        assert_eq!(
            decode(&extra, dict, P, n + 5, false).unwrap_err().detail,
            "lzma: trailing input"
        );
        // Last step, bound below the stream: what follows the bound is not the marker.
        assert_eq!(
            decode(&enc, dict, P, n - 100, true).unwrap_err().detail,
            "lzma: trailing input"
        );
        // Rule 4: the first byte.
        let mut first = enc.clone();
        first[0] = 1;
        assert_eq!(
            decode(&first, dict, P, n, true).unwrap_err().detail,
            "lzma: range coder"
        );
    }

    /// Rules 3 and 6 (CHANGES 1.2 item 7): a non-final step whose output crosses its bound is
    /// `PayloadTooLarge` at the crossing, before any later fault of its stream: here the stream is
    /// cut short far after the bound, and the dictionary is larger than the bound.
    #[test]
    fn non_final_crossing_wins_over_a_later_fault() {
        let plain = plain();
        let (enc, dict) = marked_stream(&plain);
        assert!(u64::from(dict) > 10_000);
        let cut = &enc[..enc.len() - 200];
        let e = decode(cut, dict, P, 10_000, false).unwrap_err();
        assert_eq!(e.class, "PayloadTooLarge", "{}", e.detail);
        // The same cut without a bound to cross: truncated.
        let e = decode(cut, dict, P, plain.len() as u64 + 10, false).unwrap_err();
        assert_eq!(e.detail, "lzma: truncated");
        // On the last step the crossing is `BlockLengthMismatch` or the bytes after the bound.
        let e = decode(&enc, dict, P, 10_000, true).unwrap_err();
        assert!(
            e.class == "BlockLengthMismatch" || e.detail == "lzma: trailing input",
            "{e:?}"
        );
    }

    /// Rules 4 and 5 (round 2 text): the end condition failing after the marker, and input after
    /// a marker, in each position.
    #[test]
    fn marker_end_condition_and_input_after_it() {
        let plain = plain();
        let (enc, dict) = marked_stream(&plain);
        let n = plain.len() as u64;
        // The last byte changed: the marker still decodes, with a code value that is not 0.
        let mut bad = enc.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(
            decode(&bad, dict, P, n + 1, false).unwrap_err().detail,
            "lzma: range coder end condition after the marker",
            "not the last step: a reason that is not normative"
        );
        // Last step, marker at the bound: `trailing input`.
        assert_eq!(
            decode(&bad, dict, P, n, true).unwrap_err().detail,
            "lzma: trailing input"
        );
        // Last step, marker before the bound: `truncated`, whatever follows it.
        assert_eq!(
            decode(&bad, dict, P, n + 1, true).unwrap_err().detail,
            "lzma: truncated"
        );
        let mut extra = enc.clone();
        extra.push(0);
        assert_eq!(
            decode(&extra, dict, P, n + 1, true).unwrap_err().detail,
            "lzma: truncated"
        );
        // Not the last step: input after a marker before the bound is `trailing input`.
        assert_eq!(
            decode(&extra, dict, P, n + 1, false).unwrap_err().detail,
            "lzma: trailing input"
        );
    }

    /// Rule 3: the dictionary size is at least 4096, so a distance up to 4096 is never refused for
    /// the dictionary; a larger one beyond a small `dict_size` is.
    #[test]
    fn dictionary_floor_of_4096() {
        // A literal block of 3000 bytes, then a repeat of it: distances up to 3000.
        let mut plain: Vec<u8> = (0..3000u32).map(|i| (i * 31 % 253) as u8).collect();
        plain.extend_from_within(..);
        let (enc, _) = marked_stream(&plain);
        let n = plain.len() as u64;
        assert_eq!(decode(&enc, 1, P, n, true).unwrap(), plain);
        assert_eq!(decode(&enc, 4096, P, n, true).unwrap(), plain);
    }

    #[test]
    fn empty_input_is_truncated() {
        assert_eq!(
            decode(&[], 1 << 16, P, 1, true).unwrap_err().detail,
            "lzma: truncated"
        );
        assert_eq!(
            decode(&[0, 0], 1 << 16, P, 1, false).unwrap_err().detail,
            "lzma: truncated"
        );
    }
}
