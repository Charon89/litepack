//! The entry table (section 4).

use crate::error::{Error, Result};
use crate::wire::Cursor;

/// The kind of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
}

impl EntryKind {
    /// The name the reference listing uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Directory => "Directory",
            Self::Symlink => "Symlink",
        }
    }
}

/// One entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The kind.
    pub kind: EntryKind,
    /// Entry flags.
    pub flags: u16,
    /// The path (validated UTF-8).
    pub path: String,
    /// Modification time in ns since the epoch; `i64::MIN` = unknown.
    pub mtime_ns: i64,
    /// File length, 0 for a directory, target length for a symlink.
    pub size: u64,
    /// Symlink target.
    pub target: Vec<u8>,
    /// Chunk indices of a file.
    pub chunks: Vec<u64>,
}

const MIN_ENTRY: usize = 14;
const WHAT: &str = "entry table";

fn invalid(i: u64, reason: &str) -> Error {
    Error::new(
        "InvalidPath",
        format!("invalid path in entry {i}: {reason}"),
    )
}

/// Checks a path against the rules of section 4; returns the reason of the first violation.
pub fn path_problem(p: &[u8]) -> Option<&'static str> {
    if p.is_empty() {
        return Some("empty");
    }
    if p.len() > 65535 {
        return Some("too long");
    }
    let Ok(s) = std::str::from_utf8(p) else {
        return Some("not utf-8");
    };
    if s.starts_with('/') {
        return Some("leading slash");
    }
    if s.contains('\\') {
        return Some("backslash");
    }
    if s.contains('\0') {
        return Some("nul");
    }
    let comps: Vec<&str> = s.split('/').collect();
    let last = comps.len() - 1;
    for (i, c) in comps.iter().enumerate() {
        if c.is_empty() {
            if i == last {
                return Some("trailing slash");
            }
            return Some("empty component");
        }
        if *c == "." || *c == ".." {
            return Some("dot component");
        }
    }
    None
}

/// Parses the entry table payload as a stream (section 4).
pub fn parse_entry_table(payload: &[u8]) -> Result<Vec<Entry>> {
    let mut c = Cursor::new(payload, WHAT);
    let count = c.count(MIN_ENTRY)?;
    let mut out: Vec<Entry> = Vec::with_capacity(count as usize);
    for i in 0..count {
        let kind = match c.u8()? {
            0 => EntryKind::File,
            1 => EntryKind::Directory,
            2 => EntryKind::Symlink,
            k => {
                return Err(Error::new(
                    "UnsupportedEntryKind",
                    format!("entry {i} has unknown kind {k}"),
                ))
            }
        };
        let flags = c.u16()?;
        if flags & !0x000F != 0 {
            return Err(Error::new(
                "ReservedEntryBits",
                format!("entry {i} has reserved flags {flags:#x}"),
            ));
        }
        let path_len = c.varint()?;
        if path_len == 0 {
            return Err(invalid(i, "empty"));
        }
        if path_len > 65535 {
            return Err(invalid(i, "too long"));
        }
        let path = c.bytes(path_len as usize)?;
        if let Some(r) = path_problem(path) {
            return Err(invalid(i, r));
        }
        if let Some(prev) = out.last() {
            if path <= prev.path.as_bytes() {
                return Err(Error::new(
                    "UnsortedEntries",
                    format!("entry {i} is not sorted after the previous path"),
                ));
            }
        }
        let path = String::from_utf8_lossy(path).into_owned();
        let mtime_ns = c.i64()?;
        let size = c.varint()?;
        let mut target = Vec::new();
        let mut chunks = Vec::new();
        match kind {
            EntryKind::Directory => {
                if size != 0 {
                    return Err(Error::new(
                        "InconsistentEntry",
                        format!("entry {i}: directory size"),
                    ));
                }
            }
            EntryKind::Symlink => {
                let tl = c.varint()?;
                if tl != size || tl == 0 || tl > 65535 {
                    return Err(invalid(i, "symlink target"));
                }
                let t = c.bytes(tl as usize)?;
                if t.contains(&0) {
                    return Err(invalid(i, "symlink target"));
                }
                target = t.to_vec();
            }
            EntryKind::File => {
                // Each index takes at least one byte, so a count above the bytes left cannot be true.
                let n = c.count(1)?;
                chunks.reserve(n as usize);
                for _ in 0..n {
                    chunks.push(c.varint()?);
                }
            }
        }
        out.push(Entry {
            kind,
            flags,
            path,
            mtime_ns,
            size,
            target,
            chunks,
        });
    }
    if c.remaining() != 0 {
        return Err(Error::trailing(WHAT));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_rules() {
        assert_eq!(path_problem(b""), Some("empty"));
        assert_eq!(path_problem(b"/a"), Some("leading slash"));
        assert_eq!(path_problem(b"a/"), Some("trailing slash"));
        assert_eq!(path_problem(b"a//b"), Some("empty component"));
        assert_eq!(path_problem(b"a/../b"), Some("dot component"));
        assert_eq!(path_problem(b"a\\b"), Some("backslash"));
        assert_eq!(path_problem(b"a\0b"), Some("nul"));
        assert_eq!(path_problem(&[0xFF]), Some("not utf-8"));
        assert_eq!(path_problem(b"a/b.txt"), None);
    }

    fn dir(path: &[u8]) -> Vec<u8> {
        let mut e = vec![1, 0, 0, path.len() as u8];
        e.extend_from_slice(path);
        e.extend_from_slice(&0i64.to_le_bytes());
        e.push(0);
        e
    }

    #[test]
    fn table_order_and_bounds() {
        let mut t = vec![2];
        t.extend(dir(b"a"));
        t.extend(dir(b"b"));
        assert!(parse_entry_table(&t).is_ok_and(|v| v.len() == 2));
        let mut u = vec![2];
        u.extend(dir(b"b"));
        u.extend(dir(b"a"));
        assert_eq!(
            parse_entry_table(&u).map_err(|e| e.class),
            Err("UnsortedEntries")
        );
        assert_eq!(
            parse_entry_table(&[5, 1, 0]).map_err(|e| e.class),
            Err("Truncated")
        );
        let mut tr = t.clone();
        tr.push(0);
        assert_eq!(
            parse_entry_table(&tr).map_err(|e| e.class),
            Err("TrailingBytes")
        );
    }
}
