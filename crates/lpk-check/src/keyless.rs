//! Reading an encrypted archive without credentials (section 14 "Order of checks and outcomes",
//! step 2): the trailer is checked, then the frame envelopes are walked.

use crate::archive::{open_head, walk_sealing_rules, Options};
use crate::entries::{parse_entry_table, Entry};
use crate::error::{Error, Result};
use crate::index::{GenEntry, Loc};
use crate::recovery;
use crate::wire::{kind, parse_frame, read_varint, FF_SEALED, HEADER_LEN};

/// What the envelope walk found.
#[derive(Debug, Clone, Default)]
pub struct Walk {
    /// Frames walked after the key slot.
    pub frames: u64,
    /// Recovery frame locations, in order.
    pub recovery: Vec<Loc>,
    /// Generation starts: the end of the header, then the end of every trailer but the last.
    pub generations: Vec<GenEntry>,
    /// The last entry table frame: flags and payload range.
    pub entry_table: Option<(u16, usize, usize)>,
    /// Offset of the last trailer frame.
    pub trailer_offset: usize,
    /// Whether the header says LISTABLE.
    pub listable: bool,
}

/// The keyless walk: from the end of the key slot (sequence 1), each frame's envelope is read and
/// its payload skipped by its declared length, without being read or hashed.
pub fn walk(data: &[u8], opts: &Options) -> Result<Walk> {
    let mut o = opts.clone();
    o.password = None;
    let head = open_head(data, &o)?;
    let mut w = Walk {
        generations: vec![GenEntry {
            generation: 0,
            start_offset: HEADER_LEN as u64,
            first_sequence: 0,
            salt: [0; 16],
        }],
        trailer_offset: head.trailer_offset,
        listable: head.header.listable(),
        ..Walk::default()
    };
    let mut pos = head.body_start;
    let mut seq = if head.header.encrypted() { 1u64 } else { 0 };
    let end_of_file = data.len();
    while pos < end_of_file {
        let mut p = pos + 4;
        if end_of_file < p {
            return Err(Error::truncated("frames"));
        }
        let kind_v = u16::from_le_bytes([data[pos], data[pos + 1]]);
        let flags = u16::from_le_bytes([data[pos + 2], data[pos + 3]]);
        let plen = read_varint(data, &mut p, "frames")?;
        let end = (p as u64)
            .checked_add(plen)
            .and_then(|e| e.checked_add(32))
            .filter(|e| *e <= end_of_file as u64)
            .ok_or_else(|| Error::truncated("frames"))? as usize;
        let loc = Loc {
            offset: pos as u64,
            len: (end - pos) as u64,
            sequence: seq,
        };
        match kind_v {
            kind::KEY_SLOT => {
                return Err(Error::new("UnexpectedKeySlot", "a second key slot"));
            }
            kind::RECOVERY => w.recovery.push(loc),
            kind::ENTRY_TABLE => w.entry_table = Some((flags, p, p + plen as usize)),
            kind::TRAILER if end < end_of_file => w.generations.push(GenEntry {
                generation: w.generations.len() as u64,
                start_offset: end as u64,
                first_sequence: seq + 1,
                salt: [0; 16],
            }),
            _ => {}
        }
        w.frames += 1;
        seq += 1;
        pos = end;
    }
    Ok(w)
}

/// Lists a listable archive's entry table without the key (not authenticated by the index).
pub fn list(data: &[u8], opts: &Options) -> Result<Vec<Entry>> {
    let w = walk(data, opts)?;
    if !w.listable {
        return Err(Error::new("PasswordRequired", "the entry table is sealed"));
    }
    let (flags, a, b) = w
        .entry_table
        .ok_or_else(|| Error::bad_location("entry table"))?;
    if flags & FF_SEALED != 0 {
        return Err(Error::new(
            "UnexpectedSealedFrame",
            "frame kind 1 is sealed but must not be",
        ));
    }
    parse_entry_table(&data[a..b])
}

/// Keyless `verify`: every frame as the diagnosis walk reads it (frame hashes and the sealing
/// rules), except that a recovery frame whose hash fails is passed over; no recovery payload is
/// interpreted. Returns the frames verified and, for a listable archive, the entry count.
pub fn verify(data: &[u8], opts: &Options) -> Result<(u64, Option<usize>)> {
    let mut o = opts.clone();
    o.password = None;
    let head = open_head(data, &o)?;
    let limit = opts.resources.max_frame_payload;
    let mut pos = HEADER_LEN;
    let mut seq = 0u64;
    while pos < data.len() {
        let f = parse_frame(data, pos, limit)?;
        if !f.hash_ok && f.kind != kind::RECOVERY {
            return Err(Error::hash_mismatch(f.kind));
        }
        walk_sealing_rules(&f, seq, &head.header)?;
        pos = f.end;
        seq += 1;
    }
    let entries = if head.header.listable() {
        Some(list(data, opts)?.len())
    } else {
        None
    };
    Ok((seq, entries))
}

/// The recovery scan without the key; `repair` as in [`recovery::scan`].
pub fn recovery_scan(
    data: &[u8],
    opts: &Options,
    repair: Option<&mut Vec<u8>>,
) -> Result<recovery::Report> {
    let w = walk(data, opts)?;
    recovery::scan(
        data,
        &w.recovery,
        &w.generations,
        w.trailer_offset as u64,
        repair,
        opts.resources.memory,
    )
}
