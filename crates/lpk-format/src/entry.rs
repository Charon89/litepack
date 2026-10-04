//! The entry table: payload of a frame of kind `EntryTable` (spec section 4).

use crate::error::FormatError;
use crate::varint;
use std::cmp::Ordering;

const WHAT: &str = "entry table";
const MAX_PATH: usize = 65535;

/// Kind of an archive entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EntryKind {
    /// A regular file.
    File = 0,
    /// A directory.
    Directory = 1,
    /// A symbolic link.
    Symlink = 2,
}

impl EntryKind {
    /// All kinds in numeric order.
    pub const ALL: [EntryKind; 3] = [EntryKind::File, EntryKind::Directory, EntryKind::Symlink];

    /// The kind for a raw value, if known.
    pub fn from_u8(k: u8) -> Option<EntryKind> {
        Self::ALL.iter().copied().find(|c| *c as u8 == k)
    }

    /// Stable CamelCase name used in the spec.
    pub fn name(self) -> &'static str {
        match self {
            EntryKind::File => "File",
            EntryKind::Directory => "Directory",
            EntryKind::Symlink => "Symlink",
        }
    }
}

/// Entry flags. Bits 4..=15 are reserved and must be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntryFlags(u16);

impl EntryFlags {
    /// The file is executable.
    pub const EXECUTABLE: EntryFlags = EntryFlags(1);
    /// The entry is hidden.
    pub const HIDDEN: EntryFlags = EntryFlags(2);
    /// The entry is read-only.
    pub const READ_ONLY: EntryFlags = EntryFlags(4);
    /// The entry is a system file.
    pub const SYSTEM: EntryFlags = EntryFlags(8);
    /// No flags.
    pub const EMPTY: EntryFlags = EntryFlags(0);

    const KNOWN: u16 = 0b1111;

    /// Raw bits.
    pub fn bits(self) -> u16 {
        self.0
    }

    /// Validate raw bits; on reserved bits the error is those bits.
    pub fn from_bits(bits: u16) -> Result<Self, u16> {
        if bits & !Self::KNOWN != 0 {
            return Err(bits & !Self::KNOWN);
        }
        Ok(EntryFlags(bits))
    }

    /// True when every bit of `other` is set in `self`.
    pub fn contains(self, other: EntryFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union of two flag sets.
    pub fn union(self, other: EntryFlags) -> EntryFlags {
        EntryFlags(self.0 | other.0)
    }
}

/// One archive entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Entry kind.
    pub kind: EntryKind,
    /// Entry flags.
    pub flags: EntryFlags,
    /// Archive path: UTF-8, `/` separated (see [`validate_path`]).
    pub path: String,
    /// Nanoseconds since the Unix epoch; `i64::MIN` means unknown.
    pub mtime_ns: i64,
    /// File byte length, 0 for a directory, target length for a symlink.
    pub size: u64,
    /// Symlink target bytes; `Some` exactly for symlinks.
    pub symlink_target: Option<Vec<u8>>,
    /// Indices into the archive chunk list; empty unless a file with content.
    pub chunks: Vec<u64>,
}

/// Check an archive path; the error is the reason string.
pub fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("empty");
    }
    if path.len() > MAX_PATH {
        return Err("too long");
    }
    if path.starts_with('/') {
        return Err("leading slash");
    }
    if path.contains('\\') {
        return Err("backslash");
    }
    if path.contains('\0') {
        return Err("nul");
    }
    if path.ends_with('/') {
        return Err("trailing slash");
    }
    for c in path.split('/') {
        if c.is_empty() {
            return Err("empty component");
        }
        if c == "." || c == ".." {
            return Err("dot component");
        }
    }
    Ok(())
}

fn check_target(target: &[u8], size: u64, index: u64) -> Result<(), FormatError> {
    if target.is_empty()
        || target.len() > MAX_PATH
        || target.contains(&0)
        || target.len() as u64 != size
    {
        return Err(FormatError::InvalidPath {
            index,
            reason: "symlink target",
        });
    }
    Ok(())
}

/// Encoder of the entry table payload.
#[derive(Debug)]
pub struct EntryTableWriter;

impl EntryTableWriter {
    /// Encode `entries`, which the caller has already sorted by path bytes
    /// ascending without duplicates. Fails with the errors a reader would raise.
    pub fn encode(entries: &[Entry]) -> Result<Vec<u8>, FormatError> {
        let mut out = Vec::new();
        varint::write(&mut out, entries.len() as u64)?;
        let mut prev: Option<&[u8]> = None;
        for (i, e) in entries.iter().enumerate() {
            let index = i as u64;
            validate_path(&e.path).map_err(|reason| FormatError::InvalidPath { index, reason })?;
            if let Some(p) = prev {
                if p >= e.path.as_bytes() {
                    return Err(FormatError::UnsortedEntries { index });
                }
            }
            prev = Some(e.path.as_bytes());
            let inconsistent = FormatError::InvalidPath {
                index,
                reason: "inconsistent entry",
            };
            match e.kind {
                EntryKind::Symlink => {
                    let t = e.symlink_target.as_deref().ok_or(inconsistent)?;
                    check_target(t, e.size, index)?;
                    if !e.chunks.is_empty() {
                        return Err(FormatError::InvalidPath {
                            index,
                            reason: "inconsistent entry",
                        });
                    }
                }
                EntryKind::File | EntryKind::Directory => {
                    if e.symlink_target.is_some()
                        || (e.kind == EntryKind::Directory && !e.chunks.is_empty())
                    {
                        return Err(inconsistent);
                    }
                }
            }
            out.push(e.kind as u8);
            out.extend_from_slice(&e.flags.bits().to_le_bytes());
            varint::write(&mut out, e.path.len() as u64)?;
            out.extend_from_slice(e.path.as_bytes());
            out.extend_from_slice(&e.mtime_ns.to_le_bytes());
            varint::write(&mut out, e.size)?;
            match e.kind {
                EntryKind::Symlink => {
                    let t = e.symlink_target.as_deref().unwrap_or_default();
                    varint::write(&mut out, t.len() as u64)?;
                    out.extend_from_slice(t);
                }
                EntryKind::File => {
                    varint::write(&mut out, e.chunks.len() as u64)?;
                    for c in &e.chunks {
                        varint::write(&mut out, *c)?;
                    }
                }
                EntryKind::Directory => {}
            }
        }
        Ok(out)
    }
}

/// A parsed entry table: remembers the payload and the entry count only.
#[derive(Debug, Clone, Copy)]
pub struct EntryTable<'a> {
    payload: &'a [u8],
    body: usize,
    count: u64,
}

impl<'a> EntryTable<'a> {
    /// Read the entry count; no entry is examined until iteration.
    pub fn parse(payload: &'a [u8]) -> Result<EntryTable<'a>, FormatError> {
        let mut cur = Cursor {
            data: payload,
            pos: 0,
        };
        let count = cur.varint()?;
        Ok(EntryTable {
            payload,
            body: cur.pos,
            count,
        })
    }

    /// Number of entries declared.
    pub fn len(&self) -> u64 {
        self.count
    }

    /// True when the table declares no entries.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Stream the entries. After the last entry, leftover bytes yield
    /// `TrailingBytes`; any error ends the stream.
    pub fn iter(&self) -> EntryIter<'a> {
        EntryIter {
            cur: Cursor {
                data: self.payload,
                pos: self.body,
            },
            remaining: self.count,
            index: 0,
            prev: None,
            done: false,
        }
    }

    /// Find `path`: a linear scan that stops once the sorted order passes it.
    pub fn find(&self, path: &str) -> Result<Option<Entry>, FormatError> {
        for e in self.iter() {
            let e = e?;
            match e.path.as_bytes().cmp(path.as_bytes()) {
                Ordering::Equal => return Ok(Some(e)),
                Ordering::Greater => return Ok(None),
                Ordering::Less => {}
            }
        }
        Ok(None)
    }

    /// Walk every entry: order, paths, kinds, flags and trailing bytes.
    pub fn validate(&self) -> Result<(), FormatError> {
        for e in self.iter() {
            e?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn rest(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, n: u64) -> Result<&'a [u8], FormatError> {
        let n = usize::try_from(n)
            .ok()
            .filter(|n| *n <= self.rest())
            .ok_or(FormatError::Truncated { what: WHAT })?;
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn varint(&mut self) -> Result<u64, FormatError> {
        let mut s = &self.data[self.pos..];
        let before = s.len();
        match varint::read(&mut s) {
            Ok(v) => {
                self.pos += before - s.len();
                Ok(v)
            }
            Err(FormatError::Truncated { .. }) => Err(FormatError::Truncated { what: WHAT }),
            Err(e) => Err(e),
        }
    }
}

/// Streaming iterator over the entries of an [`EntryTable`].
#[derive(Debug)]
pub struct EntryIter<'a> {
    cur: Cursor<'a>,
    remaining: u64,
    index: u64,
    prev: Option<&'a [u8]>,
    done: bool,
}

impl<'a> EntryIter<'a> {
    fn next_entry(&mut self) -> Result<Entry, FormatError> {
        let index = self.index;
        let raw_kind = self.cur.take(1)?[0];
        let kind = EntryKind::from_u8(raw_kind).ok_or(FormatError::UnsupportedEntryKind {
            kind: raw_kind,
            index,
        })?;
        let fb = self.cur.take(2)?;
        let raw_flags = u16::from_le_bytes([fb[0], fb[1]]);
        let flags = EntryFlags::from_bits(raw_flags)
            .map_err(|bits| FormatError::ReservedEntryBits { bits, index })?;
        let path_len = self.cur.varint()?;
        if path_len == 0 {
            return Err(FormatError::InvalidPath {
                index,
                reason: "empty",
            });
        }
        if path_len > MAX_PATH as u64 {
            return Err(FormatError::InvalidPath {
                index,
                reason: "too long",
            });
        }
        let path_bytes = self.cur.take(path_len)?;
        let path = std::str::from_utf8(path_bytes).map_err(|_| FormatError::InvalidPath {
            index,
            reason: "not utf-8",
        })?;
        validate_path(path).map_err(|reason| FormatError::InvalidPath { index, reason })?;
        if let Some(p) = self.prev {
            if p >= path_bytes {
                return Err(FormatError::UnsortedEntries { index });
            }
        }
        let mt = self.cur.take(8)?;
        let mtime_ns = i64::from_le_bytes([mt[0], mt[1], mt[2], mt[3], mt[4], mt[5], mt[6], mt[7]]);
        let size = self.cur.varint()?;
        let mut symlink_target = None;
        let mut chunks = Vec::new();
        match kind {
            EntryKind::Symlink => {
                let tl = self.cur.varint()?;
                if tl != size || tl == 0 || tl > MAX_PATH as u64 {
                    return Err(FormatError::InvalidPath {
                        index,
                        reason: "symlink target",
                    });
                }
                let t = self.cur.take(tl)?;
                check_target(t, size, index)?;
                symlink_target = Some(t.to_vec());
            }
            EntryKind::File => {
                let n = self.cur.varint()?;
                // Every chunk index takes at least one byte.
                if n > self.cur.rest() as u64 {
                    return Err(FormatError::Truncated { what: WHAT });
                }
                chunks.reserve(n as usize);
                for _ in 0..n {
                    chunks.push(self.cur.varint()?);
                }
            }
            EntryKind::Directory => {}
        }
        self.prev = Some(path_bytes);
        Ok(Entry {
            kind,
            flags,
            path: path.to_owned(),
            mtime_ns,
            size,
            symlink_target,
            chunks,
        })
    }
}

impl Iterator for EntryIter<'_> {
    type Item = Result<Entry, FormatError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.remaining == 0 {
            self.done = true;
            if self.cur.rest() != 0 {
                return Some(Err(FormatError::TrailingBytes { what: WHAT }));
            }
            return None;
        }
        let r = self.next_entry();
        match &r {
            Ok(_) => {
                self.remaining -= 1;
                self.index += 1;
            }
            Err(_) => self.done = true,
        }
        Some(r)
    }
}

/// The Markdown table of entry kinds, pasted verbatim into the spec.
pub fn entry_kind_table() -> String {
    let mut s = String::from("| Value | Name |\n|---|---|\n");
    for k in EntryKind::ALL {
        s.push_str(&format!("| {} | {} |\n", k as u8, k.name()));
    }
    s
}

/// The Markdown table of entry flags, pasted verbatim into the spec.
pub fn entry_flag_table() -> String {
    let bit = |f: EntryFlags| f.bits().trailing_zeros();
    let first_reserved = 16 - EntryFlags::KNOWN.leading_zeros();
    format!(
        "| Bit | Name | Meaning |\n|---|---|---|\n\
         | {} | EXECUTABLE | the file is executable |\n\
         | {} | HIDDEN | the entry is hidden |\n\
         | {} | READ_ONLY | the entry is read-only |\n\
         | {} | SYSTEM | the entry is a system file |\n\
         | {first_reserved}-15 | reserved | must be zero; a reader rejects the entry otherwise |\n",
        bit(EntryFlags::EXECUTABLE),
        bit(EntryFlags::HIDDEN),
        bit(EntryFlags::READ_ONLY),
        bit(EntryFlags::SYSTEM),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn file(path: &str, chunks: &[u64]) -> Entry {
        Entry {
            kind: EntryKind::File,
            flags: EntryFlags::EMPTY,
            path: path.to_owned(),
            mtime_ns: 0,
            size: chunks.len() as u64 * 10,
            symlink_target: None,
            chunks: chunks.to_vec(),
        }
    }

    fn dir(path: &str) -> Entry {
        Entry {
            kind: EntryKind::Directory,
            size: 0,
            ..file(path, &[])
        }
    }

    fn link(path: &str, target: &[u8]) -> Entry {
        Entry {
            kind: EntryKind::Symlink,
            size: target.len() as u64,
            symlink_target: Some(target.to_vec()),
            ..file(path, &[])
        }
    }

    fn sorted(mut v: Vec<Entry>) -> Vec<Entry> {
        v.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        v
    }

    fn collect(payload: &[u8]) -> Result<Vec<Entry>, FormatError> {
        EntryTable::parse(payload)?.iter().collect()
    }

    #[test]
    fn round_trip_mixed() {
        let long300 = "x".repeat(300);
        let long_max = "m".repeat(65535);
        let mut all = file("all-flags", &[1]);
        all.flags = EntryFlags::EXECUTABLE
            .union(EntryFlags::HIDDEN)
            .union(EntryFlags::READ_ONLY)
            .union(EntryFlags::SYSTEM);
        let mut neg = file("neg-mtime", &[]);
        neg.mtime_ns = -1_234_567_890_123;
        let mut unk = file("unknown-mtime", &[]);
        unk.mtime_ns = i64::MIN;
        let mut cyr = file("папка/файл.txt", &[4]);
        cyr.mtime_ns = 1_700_000_000_000_000_000;
        let entries = sorted(vec![
            dir("dir"),
            file("empty", &[]),
            file("three", &[7, 8, 1_000_000]),
            link("link", b"dir/../target"),
            cyr,
            file("日本語/ファイル", &[2]),
            file("emoji-\u{1F600}", &[3]),
            file("e\u{0301}combining", &[5]),
            file(&long300, &[9]),
            file(&long_max, &[10]),
            all,
            neg,
            unk,
            file("zero-mtime", &[0]),
        ]);
        let payload = EntryTableWriter::encode(&entries).unwrap();
        let t = EntryTable::parse(&payload).unwrap();
        assert_eq!(t.len(), entries.len() as u64);
        assert!(!t.is_empty());
        t.validate().unwrap();
        assert_eq!(collect(&payload).unwrap(), entries);
        let found = t.find("three").unwrap().unwrap();
        assert_eq!(found.chunks, vec![7, 8, 1_000_000]);
        assert!(t.find("nope").unwrap().is_none());
    }

    #[test]
    fn empty_table() {
        let payload = EntryTableWriter::encode(&[]).unwrap();
        let t = EntryTable::parse(&payload).unwrap();
        assert!(t.is_empty());
        t.validate().unwrap();
        assert!(t.find("a").unwrap().is_none());
    }

    #[test]
    fn path_reasons() {
        let cases: [(&str, &str); 7] = [
            ("", "empty"),
            ("/a", "leading slash"),
            ("a\\b", "backslash"),
            ("a\0b", "nul"),
            ("a/./b", "dot component"),
            ("a/../b", "dot component"),
            ("a//b", "empty component"),
        ];
        for (p, r) in cases {
            assert_eq!(validate_path(p), Err(r), "{p:?}");
        }
        assert_eq!(validate_path("a/"), Err("trailing slash"));
        assert_eq!(validate_path(&"a".repeat(65536)), Err("too long"));
        assert_eq!(validate_path("."), Err("dot component"));
    }

    #[test]
    fn path_accepts() {
        assert_eq!(validate_path("a/b/c.txt"), Ok(()));
        assert_eq!(validate_path("a"), Ok(()));
        assert_eq!(validate_path(&"a".repeat(65535)), Ok(()));
        assert_eq!(validate_path("..a/.b/c.."), Ok(()));
    }

    #[test]
    fn reader_reports_path_reasons() {
        // Hand-built entries with a bad path.
        let build = |path: &[u8]| {
            let mut v = Vec::new();
            varint::write(&mut v, 1).unwrap();
            v.push(1);
            v.extend_from_slice(&[0, 0]);
            varint::write(&mut v, path.len() as u64).unwrap();
            v.extend_from_slice(path);
            v.extend_from_slice(&[0u8; 8]);
            varint::write(&mut v, 0).unwrap();
            v
        };
        let reason = |path: &[u8]| match collect(&build(path)).unwrap_err() {
            FormatError::InvalidPath { index: 0, reason } => reason,
            e => panic!("unexpected {e:?}"),
        };
        assert_eq!(reason(b""), "empty");
        assert_eq!(reason(&[0xFF, 0xFE]), "not utf-8");
        assert_eq!(reason(b"/a"), "leading slash");
        assert_eq!(reason(b"a\\b"), "backslash");
        assert_eq!(reason(b"a\0"), "nul");
        assert_eq!(reason(b"a/.."), "dot component");
        assert_eq!(reason(b"a//b"), "empty component");
        assert_eq!(reason(b"a/"), "trailing slash");
        assert_eq!(reason(&vec![b'a'; 65536]), "too long");
        assert!(collect(&build(b"ok")).is_ok());
    }

    #[test]
    fn unsorted_and_duplicate() {
        for entries in [
            vec![file("b", &[]), file("a", &[])],
            vec![file("a", &[]), file("a", &[])],
            vec![file("a", &[]), file("c", &[]), file("b", &[])],
        ] {
            let err = EntryTableWriter::encode(&entries).unwrap_err();
            let idx = if entries.len() == 3 { 2 } else { 1 };
            assert!(
                matches!(err, FormatError::UnsortedEntries { index } if index == idx),
                "{err:?}"
            );
        }
        // Reader: encode the sorted list, then swap by hand-built payloads.
        let good = EntryTableWriter::encode(&[file("a", &[]), file("b", &[])]).unwrap();
        let mut swapped = good.clone();
        // Entry layout for a one-byte path with no chunks: 1+2+1+1+8+1+1 = 15 bytes.
        let p0 = 1 + 4;
        let p1 = 1 + 15 + 4;
        swapped.swap(p0, p1);
        let err = collect(&swapped).unwrap_err();
        assert!(matches!(err, FormatError::UnsortedEntries { index: 1 }));
        swapped[p0] = b'b';
        swapped[p1] = b'b';
        let err = collect(&swapped).unwrap_err();
        assert!(matches!(err, FormatError::UnsortedEntries { index: 1 }));
        // Byte order is unsigned: "a" < "\u{e9}" (0xC3 ..).
        EntryTableWriter::encode(&[file("a", &[]), file("\u{e9}", &[])]).unwrap();
    }

    #[test]
    fn unsupported_kind() {
        let mut p = EntryTableWriter::encode(&[file("a", &[]), file("b", &[])]).unwrap();
        p[1 + 15] = 3;
        let err = collect(&p).unwrap_err();
        assert!(matches!(
            err,
            FormatError::UnsupportedEntryKind { kind: 3, index: 1 }
        ));
    }

    #[test]
    fn reserved_flag_bit() {
        let mut p = EntryTableWriter::encode(&[file("a", &[])]).unwrap();
        p[2] = 0x10;
        let err = collect(&p).unwrap_err();
        assert!(matches!(
            err,
            FormatError::ReservedEntryBits {
                bits: 0x10,
                index: 0
            }
        ));
        assert_eq!(EntryFlags::from_bits(0x10), Err(0x10));
        assert!(EntryFlags::from_bits(0xF).is_ok());
    }

    #[test]
    fn symlink_target_mismatch() {
        let mut p = EntryTableWriter::encode(&[link("l", b"abc")]).unwrap();
        // size varint sits just before target_len: layout 1+2+1+1+8 then size.
        let size_at = 1 + 1 + 2 + 1 + 1 + 8;
        assert_eq!(p[size_at], 3);
        p[size_at] = 2;
        let err = collect(&p).unwrap_err();
        assert!(matches!(
            err,
            FormatError::InvalidPath {
                index: 0,
                reason: "symlink target"
            }
        ));
        let mut bad = link("l", b"abc");
        bad.size = 4;
        assert!(matches!(
            EntryTableWriter::encode(&[bad]),
            Err(FormatError::InvalidPath {
                reason: "symlink target",
                ..
            })
        ));
        for t in [&b""[..], b"a\0b"] {
            assert!(EntryTableWriter::encode(&[link("l", t)]).is_err());
        }
    }

    #[test]
    fn truncation_everywhere() {
        let p =
            EntryTableWriter::encode(&[file("alpha", &[1, 2]), link("beta", b"xyz"), dir("gamma")])
                .unwrap();
        for cut in 0..p.len() {
            let r = collect(&p[..cut]);
            assert!(
                matches!(
                    r,
                    Err(FormatError::Truncated {
                        what: "entry table"
                    })
                ),
                "cut {cut}: {r:?}"
            );
        }
    }

    #[test]
    fn huge_counts_do_not_allocate() {
        // chunk_count and path_len far beyond the payload.
        let mut v = Vec::new();
        varint::write(&mut v, 1).unwrap();
        v.push(0);
        v.extend_from_slice(&[0, 0]);
        varint::write(&mut v, 1).unwrap();
        v.push(b'a');
        v.extend_from_slice(&[0u8; 8]);
        varint::write(&mut v, 0).unwrap();
        varint::write(&mut v, u64::MAX).unwrap();
        assert!(matches!(
            collect(&v),
            Err(FormatError::Truncated {
                what: "entry table"
            })
        ));
    }

    #[test]
    fn trailing_byte() {
        let mut p = EntryTableWriter::encode(&[file("a", &[1])]).unwrap();
        p.push(0);
        let t = EntryTable::parse(&p).unwrap();
        assert!(matches!(
            t.validate(),
            Err(FormatError::TrailingBytes {
                what: "entry table"
            })
        ));
        // The entries themselves are still delivered before the error.
        let items: Vec<_> = t.iter().collect();
        assert_eq!(items.len(), 2);
        assert!(items[0].is_ok());
    }

    #[test]
    fn writer_rejects_invalid_paths_and_shapes() {
        assert!(matches!(
            EntryTableWriter::encode(&[file("ok", &[]), file("bad//x", &[])]),
            Err(FormatError::InvalidPath {
                index: 1,
                reason: "empty component"
            })
        ));
        let mut d = dir("d");
        d.chunks = vec![1];
        assert!(EntryTableWriter::encode(&[d]).is_err());
        let mut f = file("f", &[]);
        f.symlink_target = Some(vec![1]);
        assert!(EntryTableWriter::encode(&[f]).is_err());
        let mut l = link("l", b"t");
        l.symlink_target = None;
        assert!(EntryTableWriter::encode(&[l]).is_err());
    }

    #[test]
    fn parse_reads_only_the_count() {
        // Count 5, then garbage: parse succeeds, iteration and validate fail.
        let payload = [5u8, 0xFF, 0xFF, 0xFF];
        let t = EntryTable::parse(&payload).unwrap();
        assert_eq!(t.len(), 5);
        assert!(matches!(
            t.validate(),
            Err(FormatError::UnsupportedEntryKind { kind: 0xFF, .. })
        ));
        assert!(t.iter().next().unwrap().is_err());
        assert!(matches!(
            EntryTable::parse(&[]),
            Err(FormatError::Truncated {
                what: "entry table"
            })
        ));
    }

    #[test]
    fn find_stops_early_and_ignores_later_garbage() {
        let mut p = EntryTableWriter::encode(&[file("a", &[]), file("c", &[])]).unwrap();
        let n = p.len();
        p[n - 1] = 0xFF; // corrupt the tail of the last entry
        let t = EntryTable::parse(&p).unwrap();
        assert!(t.find("a").unwrap().is_some());
        assert!(t.find("b").is_err());
        let ok = EntryTableWriter::encode(&[file("a", &[]), file("c", &[])]).unwrap();
        assert!(EntryTable::parse(&ok).unwrap().find("b").unwrap().is_none());
    }

    #[test]
    fn scale_one_million() {
        let n = 1_000_000usize;
        let entries: Vec<Entry> = (0..n)
            .map(|i| {
                let mut e = file(&format!("d{:04}/f{:06}.bin", i / 1000, i), &[i as u64]);
                e.size = 1;
                e
            })
            .collect();
        let payload = EntryTableWriter::encode(&entries).unwrap();
        let last_path = entries[n - 1].path.clone();
        let last = entries[n - 1].clone();
        drop(entries);
        let t = EntryTable::parse(&payload).unwrap();
        assert_eq!(t.len(), n as u64);
        let mut count = 0usize;
        for e in t.iter() {
            e.unwrap();
            count += 1;
        }
        assert_eq!(count, n);
        assert_eq!(t.find(&last_path).unwrap().unwrap(), last);
    }

    fn arb_entry(path: String) -> impl Strategy<Value = Entry> {
        (
            0u8..3,
            0u16..16,
            any::<i64>(),
            proptest::collection::vec(any::<u64>(), 0..5),
            proptest::collection::vec(1u8..=255, 1..20),
        )
            .prop_map(move |(k, f, mtime_ns, chunks, target)| {
                let flags = EntryFlags::from_bits(f).unwrap();
                match k {
                    0 => Entry {
                        kind: EntryKind::File,
                        flags,
                        path: path.clone(),
                        mtime_ns,
                        size: chunks.len() as u64,
                        symlink_target: None,
                        chunks,
                    },
                    1 => Entry {
                        kind: EntryKind::Directory,
                        flags,
                        path: path.clone(),
                        mtime_ns,
                        size: 0,
                        symlink_target: None,
                        chunks: vec![],
                    },
                    _ => Entry {
                        kind: EntryKind::Symlink,
                        flags,
                        path: path.clone(),
                        mtime_ns,
                        size: target.len() as u64,
                        symlink_target: Some(target),
                        chunks: vec![],
                    },
                }
            })
    }

    fn arb_entries() -> impl Strategy<Value = Vec<Entry>> {
        proptest::collection::btree_set("[a-c\u{e9}\u{65e5}]{1,3}(/[a-c]{1,2}){0,2}", 0..12)
            .prop_flat_map(|paths| {
                // BTreeSet<String> iterates in byte order, matching the format.
                paths.into_iter().map(arb_entry).collect::<Vec<_>>()
            })
    }

    proptest! {
        #[test]
        fn round_trip_random(entries in arb_entries()) {
            let payload = EntryTableWriter::encode(&entries).unwrap();
            prop_assert_eq!(collect(&payload).unwrap(), entries);
        }
    }
}
