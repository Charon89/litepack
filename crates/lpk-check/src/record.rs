//! The `Records` frame (section 12): the walk, every `body_hash`, and the field rules of every body.

use crate::error::{Error, Result};
use crate::wire::Cursor;

/// A secondary image of a JPEG record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gainmap {
    /// Position inside the original file.
    pub offset: u64,
    /// Length.
    pub len: u64,
    /// The chunks holding it.
    pub chunks: Vec<u64>,
}

/// A `jpeg` record body (kind 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegRecord {
    /// Length of the JPEG file.
    pub original_len: u64,
    /// Length of the primary image.
    pub primary_len: u64,
    /// Raw trailing bytes (empty when peeled).
    pub trailing: Vec<u8>,
    /// Chunks of the peeled trailing data.
    pub nested_trailing_chunks: Vec<u64>,
    /// Secondary images.
    pub gainmaps: Vec<Gainmap>,
    /// Lepton format revision.
    pub lepton_version: u8,
    /// BLAKE3 of the whole file.
    pub original_hash: [u8; 32],
}

/// A parsed record body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Kind 7.
    Jpeg(JpegRecord),
    /// Kind 12: per member, its length and chunks (the rest is not needed by this decoder).
    Container(Vec<(u64, Vec<u64>)>),
    /// Kinds 8 to 11: no chunk lists.
    Other,
}

/// One record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The record kind (7 to 12).
    pub kind: u16,
    /// The parsed body.
    pub body: Body,
}

impl Record {
    /// Every chunk list the record names, each with the length its chunks must add up to.
    pub fn chunk_lists(&self) -> Vec<(&[u64], u64)> {
        match &self.body {
            Body::Jpeg(j) => {
                let mut v: Vec<(&[u64], u64)> =
                    j.gainmaps.iter().map(|g| (&g.chunks[..], g.len)).collect();
                if j.trailing.is_empty() {
                    let sec: u64 = j.gainmaps.iter().map(|g| g.len).sum();
                    let rest = j.original_len - j.primary_len - sec;
                    v.push((&j.nested_trailing_chunks[..], rest));
                }
                v
            }
            Body::Container(m) => m.iter().map(|(len, c)| (&c[..], *len)).collect(),
            Body::Other => Vec::new(),
        }
    }
}

/// `BadRecord` with the record id and a reason.
pub fn bad_record(id: u64, reason: &str) -> Error {
    Error::new("BadRecord", format!("bad record {id}: {reason}"))
}

fn chunk_list(c: &mut Cursor<'_>) -> Result<Vec<u64>> {
    let n = c.count(1)?;
    (0..n).map(|_| c.varint()).collect()
}

fn byte_string<'a>(c: &mut Cursor<'a>) -> Result<&'a [u8]> {
    let n = c.varint()?;
    if n > c.remaining() as u64 {
        return Err(Error::truncated("records"));
    }
    c.bytes(n as usize)
}

/// What a walk returns: the record count and the records walked with their bodies parsed.
#[derive(Debug, Clone)]
pub struct Walk {
    /// The `record_count`.
    pub count: u64,
    /// The records walked (all of them, or up to the one asked for).
    pub records: Vec<Record>,
}

/// Walks a `Records` payload. `bodies` = parse every body under its field rules; `upto` = stop
/// after that record (asking for record `n` walks from the start, section 12). Without `bodies`
/// the whole frame's structure and every `body_hash` are checked (this decoder's reading of
/// "the archive's record count is read", section 8 step 3).
pub fn walk(payload: &[u8], bodies: bool, upto: Option<u64>) -> Result<Walk> {
    let mut c = Cursor::new(payload, "records");
    let n = c.count(5)?;
    let mut records = Vec::new();
    for id in 0..n {
        let k = c.u16()?;
        if !(7..=12).contains(&k) {
            return Err(Error::new(
                "UnknownRecordKind",
                format!("record {id} has unknown kind {k}"),
            ));
        }
        if c.u16()? != 0 {
            return Err(Error::new(
                "ReservedRecordBits",
                format!("record {id} flags"),
            ));
        }
        let len = c.varint()?;
        if len > c.remaining() as u64 {
            return Err(Error::truncated("records"));
        }
        let body = c.bytes(len as usize)?;
        let h: [u8; 32] = c.array()?;
        if blake3::hash(body).as_bytes() != &h {
            return Err(Error::new(
                "RecordHashMismatch",
                format!("record {id} body hash mismatch"),
            ));
        }
        if bodies {
            records.push(Record {
                kind: k,
                body: parse_body(id, k, body)?,
            });
            if upto == Some(id) {
                return Ok(Walk { count: n, records });
            }
        }
    }
    if c.remaining() != 0 {
        return Err(Error::trailing("records"));
    }
    Ok(Walk { count: n, records })
}

fn u8_in(c: &mut Cursor<'_>, id: u64, name: &str, max: u8) -> Result<u8> {
    let v = c.u8()?;
    if v > max {
        return Err(bad_record(id, name));
    }
    Ok(v)
}

fn parse_body(id: u64, kind: u16, body: &[u8]) -> Result<Body> {
    let mut c = Cursor::new(body, "records");
    let b = match kind {
        7 => Body::Jpeg(jpeg_body(&mut c, id)?),
        8 => {
            c.u64()?;
            c.u64()?;
            byte_string(&mut c)?;
            u8_in(&mut c, id, "library", 0)?;
            c.array::<32>()?;
            Body::Other
        }
        9 => {
            c.u32()?;
            let height = c.u32()?;
            let bd = c.u8()?;
            if ![1, 2, 4, 8, 16].contains(&bd) {
                return Err(bad_record(id, "bit_depth"));
            }
            let ct = c.u8()?;
            if ![0, 2, 3, 4, 6].contains(&ct) {
                return Err(bad_record(id, "color_type"));
            }
            let interlace = u8_in(&mut c, id, "interlace", 1)?;
            let filters = byte_string(&mut c)?;
            c.array::<32>()?;
            if interlace == 0 && filters.len() as u64 != u64::from(height) {
                return Err(bad_record(id, "filters"));
            }
            Body::Other
        }
        10 => {
            u8_in(&mut c, id, "variant", 1)?;
            let line_len = c.u16()?;
            let ending = u8_in(&mut c, id, "line_ending", 2)?;
            u8_in(&mut c, id, "padding", 1)?;
            c.u64()?;
            c.array::<32>()?;
            if (line_len == 0) != (ending == 2) {
                return Err(bad_record(id, "line_ending"));
            }
            Body::Other
        }
        11 => {
            u8_in(&mut c, id, "endian", 1)?;
            u8_in(&mut c, id, "bom", 1)?;
            c.u64()?;
            c.array::<32>()?;
            Body::Other
        }
        _ => {
            u8_in(&mut c, id, "format", 3)?;
            let original_len = c.u64()?;
            let framing = byte_string(&mut c)?.len() as u64;
            let n = c.count(17)?;
            let mut members = Vec::new();
            let mut end = 0u64;
            let mut sum = 0u64;
            for _ in 0..n {
                let off = c.u64()?;
                let len = c.u64()?;
                let chunks = chunk_list(&mut c)?;
                match off.checked_add(len) {
                    Some(e) if e <= original_len && off >= end => end = e,
                    _ => return Err(bad_record(id, "members")),
                }
                sum += len;
                members.push((len, chunks));
            }
            c.array::<32>()?;
            if framing.checked_add(sum) != Some(original_len) {
                return Err(bad_record(id, "original_len"));
            }
            Body::Container(members)
        }
    };
    if c.remaining() != 0 {
        return Err(Error::trailing("record body"));
    }
    Ok(b)
}

fn jpeg_body(c: &mut Cursor<'_>, id: u64) -> Result<JpegRecord> {
    let original_len = c.u64()?;
    let primary_len = c.u64()?;
    let trailing = byte_string(c)?.to_vec();
    let nested_trailing_chunks = chunk_list(c)?;
    let n = c.count(17)?;
    let mut gainmaps = Vec::new();
    for _ in 0..n {
        let offset = c.u64()?;
        let len = c.u64()?;
        let chunks = chunk_list(c)?;
        gainmaps.push(Gainmap {
            offset,
            len,
            chunks,
        });
    }
    let lepton_version = c.u8()?;
    let original_hash = c.array::<32>()?;
    // Field rules, in the order section 12 lists them.
    if primary_len > original_len {
        return Err(bad_record(id, "primary_len"));
    }
    if !trailing.is_empty() && !nested_trailing_chunks.is_empty() {
        return Err(bad_record(id, "nested_trailing_chunks"));
    }
    let mut end = primary_len;
    let mut sec = 0u64;
    for g in &gainmaps {
        match g.offset.checked_add(g.len) {
            Some(e) if e <= original_len && g.offset >= end => end = e,
            _ => return Err(bad_record(id, "gainmaps")),
        }
        sec += g.len;
    }
    let after = original_len - primary_len;
    if !trailing.is_empty() {
        // The raw bytes are the whole tail; no secondary image is peeled separately.
        if !gainmaps.is_empty() {
            return Err(bad_record(id, "gainmaps"));
        }
        if trailing.len() as u64 != after {
            return Err(bad_record(id, "trailing"));
        }
    } else if nested_trailing_chunks.is_empty() && sec != after {
        // With no nested chunks the secondary images must cover every byte after the primary.
        return Err(bad_record(id, "trailing"));
    }
    Ok(JpegRecord {
        original_len,
        primary_len,
        trailing,
        nested_trailing_chunks,
        gainmaps,
        lepton_version,
        original_hash,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn jpeg(
        original: u64,
        primary: u64,
        trailing: &[u8],
        nested: &[u64],
        g: &[(u64, u64)],
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&original.to_le_bytes());
        b.extend_from_slice(&primary.to_le_bytes());
        b.push(trailing.len() as u8);
        b.extend_from_slice(trailing);
        b.push(nested.len() as u8);
        b.extend(nested.iter().map(|&x| x as u8));
        b.push(g.len() as u8);
        for &(o, l) in g {
            b.extend_from_slice(&o.to_le_bytes());
            b.extend_from_slice(&l.to_le_bytes());
            b.push(1);
            b.push(0);
        }
        b.push(0);
        b.extend_from_slice(&[0; 32]);
        b
    }

    fn reason(body: &[u8]) -> String {
        match parse_body(0, 7, body) {
            Ok(_) => "ok".into(),
            Err(e) => e.detail,
        }
    }

    #[test]
    fn jpeg_field_rules() {
        assert_eq!(reason(&jpeg(10, 10, &[], &[], &[])), "ok");
        assert_eq!(
            reason(&jpeg(10, 11, &[], &[], &[])),
            "bad record 0: primary_len"
        );
        assert_eq!(
            reason(&jpeg(12, 10, b"ab", &[1], &[])),
            "bad record 0: nested_trailing_chunks"
        );
        assert_eq!(reason(&jpeg(12, 10, b"ab", &[], &[])), "ok");
        assert_eq!(
            reason(&jpeg(12, 10, b"a", &[], &[])),
            "bad record 0: trailing"
        );
        assert_eq!(reason(&jpeg(20, 10, &[], &[3], &[(12, 4)])), "ok");
        assert_eq!(
            reason(&jpeg(20, 10, &[], &[3], &[(8, 4)])),
            "bad record 0: gainmaps"
        );
        assert_eq!(
            reason(&jpeg(20, 10, &[], &[3], &[(18, 4)])),
            "bad record 0: gainmaps"
        );
        assert_eq!(
            reason(&jpeg(20, 10, &[], &[3], &[(12, 4), (14, 2)])),
            "bad record 0: gainmaps"
        );
        assert_eq!(reason(&jpeg(20, 10, &[], &[], &[(10, 10)])), "ok");
        assert_eq!(
            reason(&jpeg(20, 10, &[], &[], &[(10, 9)])),
            "bad record 0: trailing"
        );
        let mut b = jpeg(10, 10, &[], &[], &[]);
        b.push(0);
        assert_eq!(reason(&b), "trailing bytes after record body");
        b.truncate(20);
        assert_eq!(reason(&b), "input truncated in records");
    }

    #[test]
    fn walk_stops_at_the_record_asked_for() {
        let body = jpeg(10, 10, &[], &[], &[]);
        let mut p = vec![2u8];
        for b in [&body[..], &[1u8][..]] {
            p.extend_from_slice(&7u16.to_le_bytes());
            p.extend_from_slice(&0u16.to_le_bytes());
            p.push(b.len() as u8);
            p.extend_from_slice(b);
            p.extend_from_slice(blake3::hash(b).as_bytes());
        }
        // Record 1's body is broken: asking for record 0 does not see it; the full walk does.
        assert_eq!(walk(&p, true, Some(0)).unwrap().records.len(), 1);
        assert_eq!(walk(&p, true, None).unwrap_err().class, "Truncated");
        assert_eq!(walk(&p, false, None).unwrap().count, 2);
    }
}
