//! Revision 1.3's `bwt` decoder (spec section 8, "Decoding `bwt`"; design
//! `docs/notes/e2-15-revision-1.3-design.md`, section 2).
//!
//! The step's input is `out_len` (u64 LE) followed by ⌈out_len / B⌉ BWT blocks, B the step's
//! `block_size`; its output is the `out_len` bytes of text. The layout, the bound I(O, B), the
//! memory term and the inverse are `lpk_codecs_bwt::layout`; this type is the format's view of
//! them: the order of checks and the error classes.
//!
//! Checks, in order, exactly as performed (O is the step's output bound: the block's `plain_len`
//! on the last step, the next step's input bound or `max_block_plain` otherwise):
//! 1. term(O, B) within the decode memory left after `max_block_plain`, otherwise `Refused`,
//!    field `decode_memory`;
//! 2. input shorter than 8 bytes: `BwtError`, `length`;
//! 3. `out_len` against O: on the last step a different value is `BlockLengthMismatch`, on
//!    another step a larger one is `PayloadTooLarge` (nothing allocated before this);
//! 4. the input length is not 8 + out_len + 4·Σ c(n_j): `BwtError`, `length`;
//! 5. per block, in order: an index outside 1..=n_j is `BwtError`, `index`; a failed validity
//!    condition of the inverse is `BwtError`, `inverse`.
//!
//! Check 1 needs only the parameters and the bound: [`PrimitiveDecoder::input_bound`] runs it
//! before the previous step and returns I(O, B) as that step's output bound, so an input longer
//! than the layout allows is `PayloadTooLarge` before this step runs, and a shorter one fails
//! check 4 here.

use lpk_codecs_bwt::layout::{decode_blocks, input_len, memory_term, Reason, OUT_LEN_FIELD};
use lpk_format::envelope::Refusal;
use lpk_format::{DecodeContext, FormatError, NoContext, PrimitiveDecoder, Resources};

/// Revision 1.3's `bwt` decoder; see the module text for its checks.
#[derive(Debug, Clone, Copy, Default)]
pub struct BwtDecoder;

fn bwt_error(reason: Reason) -> FormatError {
    FormatError::BwtError {
        reason: reason.as_str(),
    }
}

/// The step's `block_size` (the parameters are validated: 4 bytes, not 0).
fn block_size(params: &[u8]) -> u64 {
    params
        .get(..4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(1, |b| u64::from(u32::from_le_bytes(b)).max(1))
}

impl BwtDecoder {
    /// Check 1: the term against the decode memory left after `max_block_plain`
    /// (`limits.memory` is the archive's `decode_memory`, never above the reader's).
    fn check_term(out_bound: u64, block_size: u64, limits: &Resources) -> Result<(), FormatError> {
        let need = memory_term(out_bound, block_size);
        let allowance = limits.memory.saturating_sub(limits.max_block_plain);
        if need > allowance {
            return Err(FormatError::Refused(Refusal {
                field: "decode_memory",
                needed: limits.max_block_plain.saturating_add(need),
                allowed: limits.memory,
            }));
        }
        Ok(())
    }

    fn run(
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
        block: usize,
    ) -> Result<Vec<u8>, FormatError> {
        let b = block_size(params);
        // 1
        Self::check_term(expected_len, b, limits)?;
        // 2
        let (field, rest) = input
            .split_at_checked(OUT_LEN_FIELD as usize)
            .ok_or(bwt_error(Reason::Length))?;
        let out_len = u64::from_le_bytes(field.try_into().map_err(|_| bwt_error(Reason::Length))?);
        // 3
        if last {
            if out_len != expected_len {
                return Err(FormatError::BlockLengthMismatch { block });
            }
        } else if out_len > expected_len {
            return Err(FormatError::PayloadTooLarge {
                len: out_len,
                max: expected_len,
            });
        }
        // 4 (the length is compared before anything is allocated for the output; a length the
        // address space cannot hold is reported as `length` too, which a 64-bit reader never
        // meets: `out_len` is within the memory check 1 passed)
        if input.len() as u64 != input_len(out_len, b) {
            return Err(bwt_error(Reason::Length));
        }
        let len = usize::try_from(out_len).map_err(|_| bwt_error(Reason::Length))?;
        let mut out = vec![0u8; len];
        // 5
        decode_blocks(rest, out_len, b, &mut out).map_err(bwt_error)?;
        Ok(out)
    }
}

impl PrimitiveDecoder for BwtDecoder {
    fn decode(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        // Without `last` the step is taken as the last one: `expected_len` is exact.
        Self::run(params, input, expected_len, true, limits, NoContext.block())
    }

    fn decode_step(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        Self::run(params, input, expected_len, last, limits, NoContext.block())
    }

    fn decode_in(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        last: bool,
        limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        // Check 1, then the term is reserved from the extraction's gate (R115): it waits for
        // room, runs alone or is refused there; returned on every path out.
        let b = block_size(params);
        Self::check_term(expected_len, b, limits)?;
        let _reserved = lpk_format::reserve(&*ctx, memory_term(expected_len, b))?;
        Self::run(params, input, expected_len, last, limits, ctx.block())
    }

    fn bounds_input(&self) -> bool {
        true
    }

    fn input_bound(
        &self,
        params: &[u8],
        expected_len: u64,
        _last: bool,
        limits: &Resources,
        _ctx: &mut dyn DecodeContext,
    ) -> Result<Option<u64>, FormatError> {
        // Check 1, then I(O, B): the bound of the step before this one. Check 1 keeps the bound
        // within the archive's decode memory, as the hook's contract requires.
        let b = block_size(params);
        Self::check_term(expected_len, b, limits)?;
        Ok(Some(input_len(expected_len, b)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lpk_codecs_bwt::layout::{index_count, MEMORY_ALLOWANCE};

    fn params(block_size: u32) -> Vec<u8> {
        block_size.to_le_bytes().to_vec()
    }

    /// The step's input for `text` from a naive rotation sort (no libsais in the unit tests).
    fn layout(text: &[u8], block_size: usize) -> Vec<u8> {
        let mut out = (text.len() as u64).to_le_bytes().to_vec();
        for block in text.chunks(block_size) {
            let n = block.len();
            let s: Vec<u16> = block
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
            let mut row_of = vec![0u32; n + 1];
            let mut bwt = Vec::new();
            for (r, &start) in rows.iter().enumerate() {
                row_of[start] = r as u32;
                let last = s[(start + n) % (n + 1)];
                if last != 0 {
                    bwt.push((last - 1) as u8);
                }
            }
            let rate = 1usize << lpk_codecs_bwt::layout::rate_log2(n as u64);
            for i in (0..n).step_by(rate) {
                out.extend_from_slice(&row_of[i].to_le_bytes());
            }
            out.extend_from_slice(&bwt);
        }
        out
    }

    fn text() -> Vec<u8> {
        let w = ["the ", "quick ", "brown ", "fox\n", "jumps ", "INFO "];
        let mut x = 7u32;
        let mut t = Vec::new();
        while t.len() < 3000 {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            t.extend_from_slice(w[(x >> 24) as usize % 6].as_bytes());
        }
        t
    }

    fn lim() -> Resources {
        Resources::default()
    }

    #[test]
    fn a_text_decodes_last_and_not_last() {
        let t = text();
        let input = layout(&t, 1000);
        let n = t.len() as u64;
        let d = BwtDecoder;
        assert_eq!(d.decode(&params(1000), &input, n, &lim()).unwrap(), t);
        assert_eq!(
            d.decode_step(&params(1000), &input, n, true, &lim())
                .unwrap(),
            t
        );
        // Not the last step: `out_len` within the bound decodes.
        assert_eq!(
            d.decode_step(&params(1000), &input, n + 500, false, &lim())
                .unwrap(),
            t
        );
        let mut ctx = NoContext;
        assert_eq!(
            d.decode_in(&params(1000), &input, n, true, &lim(), &mut ctx)
                .unwrap(),
            t
        );
    }

    #[test]
    fn check_3_block_length_and_payload_bound() {
        let t = text();
        let input = layout(&t, 4096);
        let n = t.len() as u64;
        let d = BwtDecoder;
        assert!(matches!(
            d.decode_step(&params(4096), &input, n + 1, true, &lim()),
            Err(FormatError::BlockLengthMismatch { block: 0 })
        ));
        assert!(matches!(
            d.decode_step(&params(4096), &input, n - 1, true, &lim()),
            Err(FormatError::BlockLengthMismatch { block: 0 })
        ));
        let r = d.decode_step(&params(4096), &input, n - 1, false, &lim());
        assert!(
            matches!(r, Err(FormatError::PayloadTooLarge { len, max }) if len == n && max == n - 1)
        );
    }

    #[test]
    fn check_2_and_4_are_length() {
        let t = text();
        let input = layout(&t, 4096);
        let n = t.len() as u64;
        let d = BwtDecoder;
        let reason = |r: Result<Vec<u8>, FormatError>| match r {
            Err(FormatError::BwtError { reason }) => reason,
            other => panic!("{other:?}"),
        };
        for short in [0usize, 1, 7] {
            assert_eq!(
                reason(d.decode_step(&params(4096), &input[..short], n, true, &lim())),
                "length",
                "{short} bytes"
            );
        }
        // One byte more or less than 8 + out_len + 4·Σ c(n_j).
        assert_eq!(
            reason(d.decode_step(&params(4096), &input[..input.len() - 1], n, true, &lim())),
            "length"
        );
        let mut longer = input.clone();
        longer.push(0);
        assert_eq!(
            reason(d.decode_step(&params(4096), &longer, n, true, &lim())),
            "length"
        );
        // A block size other than the one the layout was made for changes Σ c(n_j).
        assert_eq!(
            reason(d.decode_step(&params(1000), &input, n, true, &lim())),
            "length"
        );
        assert_eq!(index_count(3000) * 4 + 8 + 3000, input_len(3000, 4096));
    }

    #[test]
    fn check_5_index_and_inverse() {
        let t = text();
        let input = layout(&t, 4096);
        let n = t.len() as u64;
        let d = BwtDecoder;
        let reason = |r: Result<Vec<u8>, FormatError>| match r {
            Err(FormatError::BwtError { reason }) => reason,
            other => panic!("{other:?}"),
        };
        // index[0] = 0, then index[3] = n + 1.
        let mut zero = input.clone();
        zero[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            reason(d.decode_step(&params(4096), &zero, n, true, &lim())),
            "index"
        );
        let mut over = input.clone();
        over[8 + 12..8 + 16].copy_from_slice(&(n as u32 + 1).to_le_bytes());
        assert_eq!(
            reason(d.decode_step(&params(4096), &over, n, true, &lim())),
            "index"
        );
        // An auxiliary index moved by one (in range): the walk from the primary index alone would
        // still give the text; the step reports `inverse`.
        let mut aux = input.clone();
        let v = u32::from_le_bytes(aux[12..16].try_into().unwrap());
        aux[12..16].copy_from_slice(&(if v == 1 { 2 } else { v - 1 }).to_le_bytes());
        assert_eq!(
            reason(d.decode_step(&params(4096), &aux, n, true, &lim())),
            "inverse"
        );
        // Damaged transformed bytes: other bytes or `inverse`, never a panic.
        let mut bytes = input.clone();
        for i in (200..bytes.len()).step_by(97) {
            bytes[i] ^= 0x5A;
        }
        if let Ok(v) = d.decode_step(&params(4096), &bytes, n, true, &lim()) {
            assert_ne!(v, t);
        }
        // The first fault in stream order decides: the first block's bad index before the
        // second block's bad inverse.
        let input = layout(&t, 1000);
        let mut both = input.clone();
        let v = u32::from_le_bytes(both[12..16].try_into().unwrap());
        both[12..16].copy_from_slice(&(if v == 1 { 2 } else { v - 1 }).to_le_bytes());
        let second = 8 + 4 * index_count(1000) as usize + 1000;
        both[second..second + 4].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            reason(d.decode_step(&params(1000), &both, n, true, &lim())),
            "inverse"
        );
        let mut both = input.clone();
        both[8..12].copy_from_slice(&0u32.to_le_bytes());
        let v = u32::from_le_bytes(both[second + 4..second + 8].try_into().unwrap());
        both[second + 4..second + 8]
            .copy_from_slice(&(if v == 1 { 2 } else { v - 1 }).to_le_bytes());
        assert_eq!(
            reason(d.decode_step(&params(1000), &both, n, true, &lim())),
            "index"
        );
    }

    #[test]
    fn check_1_refuses_by_the_term_and_the_hook_gives_the_bound() {
        let t = text();
        let input = layout(&t, 4096);
        let n = t.len() as u64;
        let d = BwtDecoder;
        let need = memory_term(n, 4096);
        assert_eq!(need, input_len(n, 4096) + 4 * n + MEMORY_ALLOWANCE);
        let exact = Resources {
            max_block_plain: 5000,
            memory: 5000 + need,
            ..Resources::default()
        };
        assert_eq!(
            d.decode_step(&params(4096), &input, n, true, &exact)
                .unwrap(),
            t
        );
        let less = Resources {
            memory: 4999 + need,
            ..exact
        };
        let r = d.decode_step(&params(4096), &input, n, true, &less);
        assert!(
            matches!(r, Err(FormatError::Refused(Refusal { field: "decode_memory", needed, allowed }))
            if needed == 5000 + need && allowed == 4999 + need),
            "{r:?}"
        );
        // Check 1 comes before every other: a too-short input under too little memory is
        // `Refused`.
        assert!(matches!(
            d.decode_step(&params(4096), &input[..3], n, true, &less),
            Err(FormatError::Refused(_))
        ));
        // The hook: the same refusal, else I(O, B).
        let mut ctx = NoContext;
        assert!(matches!(
            d.input_bound(&params(4096), n, true, &less, &mut ctx),
            Err(FormatError::Refused(_))
        ));
        assert_eq!(
            d.input_bound(&params(4096), n, true, &exact, &mut ctx)
                .unwrap(),
            Some(input_len(n, 4096))
        );
        assert_eq!(
            d.input_bound(&params(4096), n, false, &exact, &mut ctx)
                .unwrap(),
            Some(input_len(n, 4096))
        );
        // The term grows with the bound, not with the input: a bound of 2^63 is refused.
        assert!(matches!(
            d.input_bound(
                &params(4096),
                1 << 63,
                false,
                &Resources::default(),
                &mut ctx
            ),
            Err(FormatError::Refused(_))
        ));
    }

    #[test]
    fn a_one_byte_text_in_a_one_byte_block() {
        let input = layout(b"x", 1);
        assert_eq!(input.len(), 13);
        assert_eq!(
            BwtDecoder.decode(&params(1), &input, 1, &lim()).unwrap(),
            b"x"
        );
    }
}
