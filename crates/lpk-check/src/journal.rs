//! Generations (section 15): the trailer chain and rollback.

use crate::error::{Error, Result};
use crate::wire::{
    is_trailer_shape, parse_frame, parse_header, parse_trailer, trailer_ending_at, Trailer,
    HEADER_LEN, TRAILER_FRAME_LEN,
};

/// One generation of the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenInfo {
    /// Generation number.
    pub generation: u64,
    /// Offset of its trailer frame.
    pub trailer_offset: usize,
    /// Its trailer.
    pub trailer: Trailer,
}

/// The end of the last complete trailer: the end of the file when it ends with one, otherwise
/// found by walking the frames with a length limit of the file's length (section 15 "Rollback").
pub fn last_trailer_end(data: &[u8]) -> Option<usize> {
    if trailer_ending_at(data, data.len()).is_some() {
        return Some(data.len());
    }
    let mut pos = HEADER_LEN;
    let mut last = None;
    while pos < data.len() {
        let Ok(f) = parse_frame(data, pos, data.len() as u64) else {
            break;
        };
        if !f.hash_ok {
            break;
        }
        if is_trailer_shape(&f) {
            last = Some(f.end);
        }
        pos = f.end;
    }
    last
}

/// The chain from the last complete trailer back to generation 0, newest first.
pub fn history(data: &[u8]) -> Result<Vec<GenInfo>> {
    let header = parse_header(data)?;
    let end = last_trailer_end(data).ok_or_else(|| Error::new("NoTrailer", "no trailer found"))?;
    let (mut off, mut t) =
        trailer_ending_at(data, end).ok_or_else(|| Error::new("NoTrailer", "no trailer found"))?;
    if t.archive_id != header.archive_id {
        return Err(Error::new(
            "ArchiveIdMismatch",
            "trailer archive id differs",
        ));
    }
    let mut out = Vec::new();
    loop {
        // Section 15 "The trailer chain", in its order.
        if t.index_offset.checked_add(t.index_len) != Some(off as u64) {
            return Err(Error::bad_location("index"));
        }
        out.push(GenInfo {
            generation: t.generation,
            trailer_offset: off,
            trailer: t,
        });
        if t.generation == 0 {
            if t.previous_trailer_offset != 0 {
                return Err(Error::new(
                    "BadTrailer",
                    "bad trailer: previous_trailer_offset",
                ));
            }
            return Ok(out);
        }
        let prev = t.previous_trailer_offset;
        let prev_end_ok = prev >= HEADER_LEN as u64
            && prev
                .checked_add(TRAILER_FRAME_LEN as u64)
                .is_some_and(|e| e <= t.index_offset);
        if !prev_end_ok {
            return Err(Error::bad_location("trailer"));
        }
        let prev = prev as usize;
        let f = parse_frame(&data[..t.index_offset as usize], prev, 96).map_err(|e| {
            if e.class == "Truncated" {
                Error::truncated("trailer")
            } else {
                Error::new("NoTrailer", format!("previous trailer: {e}"))
            }
        })?;
        if !is_trailer_shape(&f) {
            return Err(Error::new(
                "NoTrailer",
                "previous trailer has the wrong shape",
            ));
        }
        f.check_hash()?;
        let (poff, pt) = (prev, parse_trailer(f.payload)?);
        if pt.archive_id != header.archive_id {
            return Err(Error::new(
                "ArchiveIdMismatch",
                "trailer archive id differs",
            ));
        }
        if pt.generation.checked_add(1) != Some(t.generation) {
            return Err(Error::new(
                "GenerationMismatch",
                format!(
                    "trailer generation {} is not {} - 1",
                    pt.generation, t.generation
                ),
            ));
        }
        off = poff;
        t = pt;
    }
}

/// The length the file has after a rollback to `generation` (section 15 "Rollback").
pub fn rollback_len(data: &[u8], generation: u64) -> Result<usize> {
    let chain = history(data)?;
    let latest = chain.first().map(|g| g.generation).unwrap_or(0);
    chain
        .iter()
        .find(|g| g.generation == generation)
        .map(|g| g.trailer_offset + TRAILER_FRAME_LEN)
        .ok_or_else(|| {
            Error::new(
                "NoSuchGeneration",
                format!("no generation {generation} (latest is {latest})"),
            )
        })
}
