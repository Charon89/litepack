//! Revision 1.4's `png-filter` decoder (spec section 8, "Decoding `png-filter`"; design
//! `docs/notes/e2-16-design.md`, section 2).
//!
//! The step's input is a filtered text of one PNG image under filter method 0: per scanline a
//! filter byte (at most 4) and the scanline's filtered bytes, in PNG's order (Adam7's passes in
//! order, empty passes without scanlines), so exactly the image's filtered length. A writer
//! gives every scanline the filter Up (the layout the E2-16 probe measured best). The output is
//! the same image filtered with the record's filter bytes: the inflated IDAT text of the PNG
//! file, the next step's (`deflate-reconstruct`'s) input.
//!
//! Order of checks, exactly as performed:
//! 1. the record by id (the errors of the `Records` frame);
//! 2. its kind is `png-filter`, otherwise `BadRecord`, `kind`;
//! 3. the image ([`lpk_format::PngFilterRecord::check_image`]): `width`, `height`,
//!    `bit_depth`, `filters`;
//! 4. the filtered length against the step's output bound: on the last step a length other
//!    than the block's `plain_len` is `BlockLengthMismatch`; otherwise a length above the bound,
//!    or past 2^64 − 1, is `PayloadTooLarge`;
//! 5. the input exactly the filtered length, otherwise `BadRecord`, `scanlines`;
//! 6. the re-filtering: an input scanline whose filter byte is above 4 is `BadRecord`,
//!    `scanlines`;
//! 7. a BLAKE3 of the output other than `original_hash` is `BadRecord`, `original_hash`.
//!
//! Checks 1 to 4 need only the record: [`PrimitiveDecoder::input_bound`] runs them before the
//! previous step and returns the filtered length as that step's output bound; `decode_in` runs
//! them again and then 5 to 7. Resources: none — the decoder holds its input, its output (both
//! the filtered length, at most the step's bound) and two rows.

use lpk_format::{
    DecodeContext, FormatError, PngFilterRecord, PrimitiveDecoder, PrimitiveId, RecordBody,
    Resources,
};

use super::png::{Layout, PngError};

/// Revision 1.4's `png-filter` decoder; see the module text for its checks.
#[derive(Debug, Clone, Copy, Default)]
pub struct PngFilterDecoder;

fn bad(record: u64, reason: &'static str) -> FormatError {
    FormatError::BadRecord { record, reason }
}

impl PngFilterDecoder {
    /// Checks 1 to 4: the record, its image and its filtered length against the bound.
    fn checked_record(
        params: &[u8],
        expected_len: u64,
        last: bool,
        ctx: &mut dyn DecodeContext,
    ) -> Result<(u64, PngFilterRecord, u64), FormatError> {
        let id = PrimitiveId::PngFilter
            .record_id(params)
            .ok_or(FormatError::BadParams {
                id: PrimitiveId::PngFilter as u16,
                reason: "record_id",
            })?;
        // 1, 2
        let RecordBody::PngFilter(p) = ctx.record(id)?.body else {
            return Err(bad(id, "kind"));
        };
        // 3
        p.check_image(id)?;
        // 4
        let len = p.filtered_len();
        if last {
            if len != Some(expected_len) {
                return Err(FormatError::BlockLengthMismatch { block: ctx.block() });
            }
        } else if len.is_none_or(|l| l > expected_len) {
            return Err(FormatError::PayloadTooLarge {
                len: len.unwrap_or(u64::MAX),
                max: expected_len,
            });
        }
        Ok((id, p, len.unwrap_or(u64::MAX)))
    }
}

impl PrimitiveDecoder for PngFilterDecoder {
    fn decode(
        &self,
        _params: &[u8],
        _input: &[u8],
        _expected_len: u64,
        _limits: &Resources,
    ) -> Result<Vec<u8>, FormatError> {
        // Without the archive behind it the record cannot be read.
        Err(FormatError::UnimplementedPrimitive {
            id: PrimitiveId::PngFilter as u16,
        })
    }

    fn decode_in(
        &self,
        params: &[u8],
        input: &[u8],
        expected_len: u64,
        last: bool,
        _limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Vec<u8>, FormatError> {
        let (id, p, len) = Self::checked_record(params, expected_len, last, ctx)?;
        // 5
        if input.len() as u64 != len {
            return Err(bad(id, "scanlines"));
        }
        // 6 (check 3 made the layout valid)
        let layout = Layout::new(p.width, p.height, p.bit_depth, p.color_type, p.interlace)
            .map_err(|_| bad(id, "bit_depth"))?;
        let out = match layout.transcode(input, &p.filters) {
            Ok(out) => out,
            Err(PngError::BadFilter { .. } | PngError::LengthMismatch { .. }) => {
                return Err(bad(id, "scanlines"))
            }
            Err(_) => return Err(bad(id, "filters")),
        };
        // 7
        if !p.verify_hash(&out) {
            return Err(bad(id, "original_hash"));
        }
        Ok(out)
    }

    fn bounds_input(&self) -> bool {
        true
    }

    fn input_bound(
        &self,
        params: &[u8],
        expected_len: u64,
        last: bool,
        _limits: &Resources,
        ctx: &mut dyn DecodeContext,
    ) -> Result<Option<u64>, FormatError> {
        Self::checked_record(params, expected_len, last, ctx).map(|(_, _, len)| Some(len))
    }
}
