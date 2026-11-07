//! Listing batches, v1 (`docs/notes/e5-design.md` section 3): what `rows` returns to the page
//! as one `ArrayBuffer`. Little-endian:
//!
//! - a 28-byte header: magic `LPKB`, version `u16` (1), reserved `u16` (0), the view's
//!   generation `u32`, the view's total rows `u32`, the first row `u32`, the row count `u32`,
//!   the names' byte length `u32`;
//! - [`ROW_LEN`] (32) bytes per row: id `u32`, kind `u8` (0 file, 1 folder, 2 link), marks `u8`
//!   (bit 0: selected, [`MARK_PICKED`]), flags `u16`, size `u64`,
//!   modification time `i64` (nanoseconds since 1970), name offset `u32` and length `u32`
//!   into the names;
//! - the names, UTF-8.
//!
//! A batch holds at most [`MAX_ROWS`] rows and stops before its names pass [`MAX_NAMES`].

use lpk_api::RowRef;

/// The header's length.
pub const HEADER_LEN: usize = 28;
/// A row's length.
pub const ROW_LEN: usize = 32;
/// The most rows in one batch.
pub const MAX_ROWS: usize = 4096;
/// The most name bytes in one batch (1 MiB).
pub const MAX_NAMES: usize = 1 << 20;
/// The magic.
pub const MAGIC: &[u8; 4] = b"LPKB";
/// The version.
pub const VERSION: u16 = 1;

/// The marks bit of a selected row.
pub const MARK_PICKED: u8 = 1;

/// The most bytes one batch takes: the header, [`MAX_ROWS`] rows and [`MAX_NAMES`] of names
/// (the "one batch" of the backend's bound, `docs/notes/e5-design.md` section 3).
pub const MAX_BATCH_BYTES: usize = HEADER_LEN + MAX_ROWS * ROW_LEN + MAX_NAMES;

/// Encode `rows` (starting at view row `first` of `total`, of view `generation`); `flat`
/// names each row by its path, otherwise by its name; `picked` sets [`MARK_PICKED`]. Rows
/// past the limits are left out (the header's count says how many are in). The output is
/// allocated once, at its exact size; `rows` is walked twice and nothing else is kept.
pub fn encode<'m, I>(
    generation: u64,
    total: usize,
    first: usize,
    rows: I,
    flat: bool,
    picked: impl Fn(u32) -> bool,
) -> Vec<u8>
where
    I: Iterator<Item = RowRef<'m>> + Clone,
{
    let name_of = |r: &RowRef<'m>| -> &'m str {
        if flat {
            r.path
        } else {
            r.name
        }
    };
    // First pass: how many rows fit, and their names' length.
    let mut count = 0usize;
    let mut names_len = 0usize;
    for r in rows.clone().take(MAX_ROWS) {
        let len = name_of(&r).len();
        if names_len + len > MAX_NAMES && count > 0 {
            break;
        }
        names_len += len;
        count += 1;
    }
    let mut out = Vec::with_capacity(HEADER_LEN + count * ROW_LEN + names_len);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(generation as u32).to_le_bytes());
    out.extend_from_slice(&(total.min(u32::MAX as usize) as u32).to_le_bytes());
    out.extend_from_slice(&(first as u32).to_le_bytes());
    out.extend_from_slice(&(count as u32).to_le_bytes());
    out.extend_from_slice(&(names_len as u32).to_le_bytes());
    let mut off = 0u32;
    for r in rows.clone().take(count) {
        let len = name_of(&r).len() as u32;
        out.extend_from_slice(&r.id.to_le_bytes());
        out.push(r.row.kind);
        out.push(if picked(r.id) { MARK_PICKED } else { 0 });
        out.extend_from_slice(&u16::from(r.row.flags).to_le_bytes());
        out.extend_from_slice(&r.row.size.to_le_bytes());
        out.extend_from_slice(&r.row.mtime_ns.to_le_bytes());
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        off += len;
    }
    for r in rows.take(count) {
        out.extend_from_slice(name_of(&r).as_bytes());
    }
    out
}

/// The header's fields: (generation, total, first, count); `None` when it is not a v1 batch.
pub fn header(b: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let u32_at = |o: usize| {
        b.get(o..o + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    if b.get(..4)? != MAGIC || b.get(4..6)? != VERSION.to_le_bytes() {
        return None;
    }
    Some((u32_at(8)?, u32_at(12)?, u32_at(16)?, u32_at(20)?))
}

/// One decoded row (tests and tools; the page decodes in `apps/gui/batch.js`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The entry id.
    pub id: u32,
    /// The kind byte.
    pub kind: u8,
    /// The marks byte.
    pub marks: u8,
    /// The size.
    pub size: u64,
    /// The modification time.
    pub mtime_ns: i64,
    /// The name.
    pub name: String,
}

/// Decode a batch: (total, first, rows); `None` when it is not a v1 batch.
pub fn decode(b: &[u8]) -> Option<(u32, u32, Vec<Decoded>)> {
    let u32_at = |o: usize| {
        b.get(o..o + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let u64_at = |o: usize| {
        b.get(o..o + 8).map(|s| {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            u64::from_le_bytes(a)
        })
    };
    if b.get(..4)? != MAGIC || b.get(4..6)? != VERSION.to_le_bytes() {
        return None;
    }
    let total = u32_at(12)?;
    let first = u32_at(16)?;
    let count = u32_at(20)? as usize;
    let names_at = HEADER_LEN + count * ROW_LEN;
    let mut rows = Vec::with_capacity(count);
    for i in 0..count {
        let o = HEADER_LEN + i * ROW_LEN;
        let off = u32_at(o + 24)? as usize;
        let len = u32_at(o + 28)? as usize;
        let name = b.get(names_at + off..names_at + off + len)?;
        rows.push(Decoded {
            id: u32_at(o)?,
            kind: *b.get(o + 4)?,
            marks: *b.get(o + 5)?,
            size: u64_at(o + 8)?,
            mtime_ns: u64_at(o + 16)? as i64,
            name: String::from_utf8(name.to_vec()).ok()?,
        });
    }
    Some((total, first, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lpk_api::Row;

    fn row(id: u32, path: &'static str, size: u64) -> RowRef<'static> {
        RowRef {
            id,
            row: Row {
                size,
                mtime_ns: -5,
                parent: lpk_api::ROOT,
                first_child: 0,
                child_count: 0,
                name_off: 0,
                name_len: 0,
                kind: 1,
                flags: 0,
            },
            path,
            name: path.rsplit('/').next().unwrap_or(path),
        }
    }

    #[test]
    fn a_batch_round_trips_with_the_documented_layout() {
        let rows = [row(3, "a/b.txt", 10), row(9, "a/ü.bin", u64::MAX)];
        let b = encode(7, 100, 40, rows.iter().copied(), true, |id| id == 9);
        assert_eq!(b.len(), HEADER_LEN + 2 * ROW_LEN + "a/b.txta/ü.bin".len());
        let (total, first, got) = decode(&b).unwrap();
        assert_eq!((total, first), (100, 40));
        assert_eq!(got[1].name, "a/ü.bin");
        assert_eq!(got[1].size, u64::MAX);
        assert_eq!(got[0].mtime_ns, -5);
        assert_eq!(header(&b), Some((7, 100, 40, 2)));
        assert_eq!((got[0].marks, got[1].marks), (0, MARK_PICKED));
        let names = encode(7, 100, 40, rows.iter().copied(), false, |_| false);
        assert_eq!(decode(&names).unwrap().2[0].name, "b.txt");
    }

    #[test]
    fn a_batch_is_capped_in_rows() {
        let rows: Vec<_> = (0..MAX_ROWS as u32 + 10).map(|i| row(i, "x", 1)).collect();
        let (_, _, got) = decode(&encode(
            0,
            rows.len(),
            0,
            rows.iter().copied(),
            true,
            |_| false,
        ))
        .unwrap();
        assert_eq!(got.len(), MAX_ROWS);
    }

    #[test]
    fn a_batch_stops_before_its_names_pass_one_mib() {
        let long: &'static str = Box::leak("n".repeat(300_000).into_boxed_str());
        let rows: Vec<_> = (0..10).map(|i| row(i, long, 1)).collect();
        let b = encode(0, 10, 0, rows.iter().copied(), true, |_| false);
        let (_, _, got) = decode(&b).unwrap();
        assert_eq!(got.len(), 3);
        assert!(b.len() <= MAX_BATCH_BYTES);
    }
}
