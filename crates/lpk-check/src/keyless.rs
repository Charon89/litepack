//! Reading an encrypted archive without credentials (sections 14 and 15 "Without the key"):
//! walk the frame envelopes, verify every frame hash, find the recovery frames and the clear
//! entry table of a listable archive.

use crate::entries::{parse_entry_table, Entry};
use crate::error::{Error, Result};
use crate::index::{GenEntry, Loc};
use crate::recovery;
use crate::wire::{kind, parse_frame, parse_header, read_varint, HEADER_LEN};

/// What the walk found.
#[derive(Debug, Clone, Default)]
pub struct Walk {
    /// Frames read.
    pub frames: u64,
    /// Frames whose hash failed.
    pub bad_hashes: u64,
    /// Recovery frame locations, in order.
    pub recovery: Vec<Loc>,
    /// Generation starts (offset after the header, then after every trailer but the last).
    pub generations: Vec<GenEntry>,
    /// The last entry table frame: offset, flags, payload range.
    pub entry_table: Option<(usize, u16, usize, usize)>,
    /// Offset of the last trailer frame.
    pub last_trailer: Option<usize>,
}

/// Walks the frames by their envelopes; a damaged payload is skipped by its declared length.
pub fn walk(data: &[u8]) -> Result<Walk> {
    parse_header(data)?;
    let mut w = Walk {
        generations: vec![GenEntry {
            generation: 0,
            start_offset: HEADER_LEN as u64,
            first_sequence: 0,
            salt: [0; 16],
        }],
        ..Walk::default()
    };
    let mut pos = HEADER_LEN;
    let mut seq = 0u64;
    while pos < data.len() {
        // Read the envelope even when the payload is damaged.
        let mut p = pos + 4;
        if data.len() < p {
            return Err(Error::truncated("frame header"));
        }
        let kind_v = u16::from_le_bytes([data[pos], data[pos + 1]]);
        let flags = u16::from_le_bytes([data[pos + 2], data[pos + 3]]);
        let plen = read_varint(data, &mut p, "frame header")?;
        let end = (p as u64)
            .checked_add(plen)
            .and_then(|e| e.checked_add(32))
            .filter(|e| *e <= data.len() as u64)
            .ok_or_else(|| Error::truncated("payload"))? as usize;
        let ok = parse_frame(&data[..end], pos, u64::MAX).is_ok_and(|f| f.hash_ok);
        if !ok {
            w.bad_hashes += 1;
        }
        let loc = Loc {
            offset: pos as u64,
            len: (end - pos) as u64,
            sequence: seq,
        };
        match kind_v {
            kind::RECOVERY => w.recovery.push(loc),
            kind::ENTRY_TABLE => w.entry_table = Some((pos, flags, p, p + plen as usize)),
            kind::TRAILER => {
                w.last_trailer = Some(pos);
                if end < data.len() {
                    w.generations.push(GenEntry {
                        generation: w.generations.len() as u64,
                        start_offset: end as u64,
                        first_sequence: seq + 1,
                        salt: [0; 16],
                    });
                }
            }
            _ => {}
        }
        w.frames += 1;
        seq += 1;
        pos = end;
    }
    Ok(w)
}

/// Lists a listable archive's entry table without the key (not authenticated by the index).
pub fn list(data: &[u8]) -> Result<Vec<Entry>> {
    let header = parse_header(data)?;
    let w = walk(data)?;
    let (_, flags, a, b) = w
        .entry_table
        .ok_or_else(|| Error::new("PasswordRequired", "no entry table found"))?;
    if flags & crate::wire::FF_SEALED != 0 || !header.listable() {
        return Err(Error::new("PasswordRequired", "the entry table is sealed"));
    }
    parse_entry_table(&data[a..b])
}

/// The recovery scan without the key; `repair` as in [`recovery::scan`].
pub fn recovery_scan(
    data: &[u8],
    repair: Option<&mut Vec<u8>>,
    memory: u64,
) -> Result<recovery::Report> {
    let w = walk(data)?;
    let bound = w.last_trailer.unwrap_or(data.len()) as u64;
    recovery::scan(data, &w.recovery, &w.generations, bound, repair, memory)
}
