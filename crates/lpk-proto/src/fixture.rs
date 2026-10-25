//! A trivial archive format for tests only, and its reader: what the worker serves until E4-3's
//! real readers land, so spawn, protocol, limits and the host's policy pass run end to end.
//!
//! Layout (integers little-endian): [`MAGIC`] (16 bytes), version `1` (`u8`), password length
//! (`u8`) and password bytes (a test-only "encryption": the reader compares them), member count
//! (`u32`), then per member: kind (`u8`, as [`RecordKind`]), name (`u16` length + bytes), link
//! target (`u16` length + bytes), mtime (`i64` ns), size (`u64`), and for a file `size` data
//! bytes. Not a format anyone should use: it exists so that a worker has something to read.

use std::io::{Read, Seek, SeekFrom};

use crate::frame::{Record, RecordKind, NO_BLOCK};
use crate::reader::{ErrorCode, ForeignReader, ReaderError, Sink};

/// The fixture's magic.
pub const MAGIC: &[u8; 16] = b"LPK-TEST-FIXTURE";
/// The fixture's version byte.
pub const VERSION: u8 = 1;
/// The smallest member header: kind, two lengths, mtime, size.
pub const MIN_MEMBER: u64 = 1 + 2 + 2 + 8 + 8;
/// The format's name in `Opened`.
pub const FORMAT: &str = "test-fixture";

/// One member to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// What it is.
    pub kind: RecordKind,
    /// Its name.
    pub name: Vec<u8>,
    /// A link's target.
    pub target: Vec<u8>,
    /// Nanoseconds since the epoch, `i64::MIN` unknown.
    pub mtime_ns: i64,
    /// A file's bytes.
    pub data: Vec<u8>,
}

impl Member {
    /// A file.
    pub fn file(name: &str, data: &[u8]) -> Self {
        Member {
            kind: RecordKind::File,
            name: name.as_bytes().to_vec(),
            target: Vec::new(),
            mtime_ns: 1_600_000_000_000_000_000,
            data: data.to_vec(),
        }
    }
    /// A directory.
    pub fn dir(name: &str) -> Self {
        Member {
            kind: RecordKind::Directory,
            data: Vec::new(),
            ..Member::file(name, &[])
        }
    }
    /// A link of `kind` to `target`.
    pub fn link(kind: RecordKind, name: &str, target: &str) -> Self {
        Member {
            kind,
            target: target.as_bytes().to_vec(),
            ..Member::file(name, &[])
        }
    }
}

fn code(k: RecordKind) -> u8 {
    match k {
        RecordKind::File => 0,
        RecordKind::Directory => 1,
        RecordKind::Symlink => 2,
        RecordKind::HardLink => 3,
    }
}

/// The bytes of a fixture holding `members`, "encrypted" with `password` when given.
pub fn write(members: &[Member], password: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    let pw = password.unwrap_or_default();
    let pw = &pw[..pw.len().min(255)];
    out.push(pw.len() as u8);
    out.extend_from_slice(pw);
    out.extend_from_slice(&(members.len() as u32).to_le_bytes());
    for m in members {
        out.push(code(m.kind));
        let name = &m.name[..m.name.len().min(65535)];
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        let target = &m.target[..m.target.len().min(65535)];
        out.extend_from_slice(&(target.len() as u16).to_le_bytes());
        out.extend_from_slice(target);
        out.extend_from_slice(&m.mtime_ns.to_le_bytes());
        let size = if m.kind == RecordKind::File {
            m.data.len() as u64
        } else {
            0
        };
        out.extend_from_slice(&size.to_le_bytes());
        if m.kind == RecordKind::File {
            out.extend_from_slice(&m.data);
        }
    }
    out
}

/// True when `head` starts with the fixture's magic.
pub fn detect(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// The reader: the listing is read at open (every header, the data skipped by seeking), data
/// is read from the archive handle when a member is asked for.
#[derive(Debug)]
pub struct FixtureReader<R> {
    archive: R,
    records: Vec<Record>,
}

fn read_exact<R: Read>(r: &mut R, n: usize) -> Result<Vec<u8>, ReaderError> {
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)
        .map_err(|_| ReaderError::corrupt("truncated fixture"))?;
    Ok(b)
}

fn arr<R: Read, const N: usize>(r: &mut R) -> Result<[u8; N], ReaderError> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)
        .map_err(|_| ReaderError::corrupt("truncated fixture"))?;
    Ok(b)
}

impl<R: Read + Seek> FixtureReader<R> {
    /// Open a fixture, checking the password.
    pub fn open(mut archive: R, password: Option<&[u8]>) -> Result<Self, ReaderError> {
        let len = archive.seek(SeekFrom::End(0))?;
        archive.seek(SeekFrom::Start(0))?;
        let magic: [u8; 16] = arr(&mut archive)?;
        if &magic != MAGIC {
            return Err(ReaderError::corrupt("not a test fixture"));
        }
        let [version] = arr(&mut archive)?;
        if version != VERSION {
            return Err(ReaderError::new(
                ErrorCode::Unsupported,
                format!("fixture version {version}"),
            ));
        }
        let [pw_len] = arr(&mut archive)?;
        let pw = read_exact(&mut archive, usize::from(pw_len))?;
        if !pw.is_empty() {
            match password {
                None => return Err(ReaderError::new(ErrorCode::NeedPassword, "password needed")),
                Some(p) if p != pw.as_slice() => {
                    return Err(ReaderError::new(ErrorCode::WrongPassword, "wrong password"))
                }
                Some(_) => {}
            }
        }
        let count = u64::from(u32::from_le_bytes(arr(&mut archive)?));
        let mut at = archive.stream_position()?;
        if count > len.saturating_sub(at) / MIN_MEMBER {
            return Err(ReaderError::corrupt("member count exceeds the file"));
        }
        let mut records = Vec::with_capacity(count as usize);
        for i in 0..count {
            let [k] = arr(&mut archive)?;
            let kind = match k {
                0 => RecordKind::File,
                1 => RecordKind::Directory,
                2 => RecordKind::Symlink,
                3 => RecordKind::HardLink,
                _ => return Err(ReaderError::corrupt(format!("member {i}: kind {k}"))),
            };
            let n = u16::from_le_bytes(arr(&mut archive)?);
            let name = read_exact(&mut archive, usize::from(n))?;
            let n = u16::from_le_bytes(arr(&mut archive)?);
            let target = read_exact(&mut archive, usize::from(n))?;
            let mtime_ns = i64::from_le_bytes(arr(&mut archive)?);
            let size = u64::from_le_bytes(arr(&mut archive)?);
            at = archive.stream_position()?;
            if kind != RecordKind::File && size != 0 {
                return Err(ReaderError::corrupt(format!(
                    "member {i}: size on a non-file"
                )));
            }
            if size > len.saturating_sub(at) {
                return Err(ReaderError::corrupt(format!(
                    "member {i}: data past the end"
                )));
            }
            records.push(Record {
                kind,
                flags: 0,
                mtime_ns,
                size,
                packed_offset: at,
                packed_len: size,
                block: if size > 0 { i } else { NO_BLOCK },
                name,
                target,
            });
            at += size;
            archive.seek(SeekFrom::Start(at))?;
        }
        Ok(FixtureReader { archive, records })
    }
}

impl<R: Read + Seek> ForeignReader for FixtureReader<R> {
    fn format(&self) -> String {
        FORMAT.to_string()
    }

    fn entries(&mut self, from: u64, max: usize) -> Result<(Vec<Record>, bool), ReaderError> {
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        let end = from.saturating_add(max).min(self.records.len());
        let page = self.records.get(from..end).unwrap_or_default().to_vec();
        Ok((page, end < self.records.len()))
    }

    fn read_member(&mut self, index: u64, sink: &mut Sink<'_>) -> Result<bool, ReaderError> {
        let r = usize::try_from(index)
            .ok()
            .and_then(|i| self.records.get(i))
            .ok_or_else(|| ReaderError::new(ErrorCode::NoSuchMember, format!("member {index}")))?;
        let (mut left, at) = (r.size, r.packed_offset);
        self.archive.seek(SeekFrom::Start(at))?;
        let mut buf = vec![0u8; 64 << 10];
        while left > 0 {
            let n = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
            self.archive.read_exact(&mut buf[..n])?;
            sink(&buf[..n])?;
            left -= n as u64;
        }
        Ok(false)
    }
}

/// The worker's opener for fixtures.
pub fn open(
    archive: std::fs::File,
    password: Option<&[u8]>,
) -> Result<Box<dyn ForeignReader>, ReaderError> {
    Ok(Box::new(FixtureReader::open(
        std::io::BufReader::new(archive),
        password,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fixture_round_trips() {
        let members = vec![
            Member::dir("d"),
            Member::file("d/a.txt", b"hello"),
            Member::link(RecordKind::Symlink, "d/l", "a.txt"),
            Member::file("empty", b""),
        ];
        let bytes = write(&members, None);
        assert!(detect(&bytes));
        let mut r = FixtureReader::open(Cursor::new(bytes), None).unwrap();
        let (page, more) = r.entries(0, 3).unwrap();
        assert_eq!(page.len(), 3);
        assert!(more);
        assert_eq!(page[1].name, b"d/a.txt");
        assert_eq!(page[2].target, b"a.txt");
        let (page, more) = r.entries(3, 3).unwrap();
        assert_eq!(page.len(), 1);
        assert!(!more);
        let mut got = Vec::new();
        r.read_member(1, &mut |b: &[u8]| {
            got.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        assert_eq!(got, b"hello");
        assert!(r.read_member(9, &mut |_: &[u8]| Ok(())).is_err());
    }

    #[test]
    fn password_and_damage() {
        let bytes = write(&[Member::file("a", b"x")], Some(b"pw"));
        let code = |r: Result<FixtureReader<_>, ReaderError>| r.err().map(|e| e.code);
        assert_eq!(
            code(FixtureReader::open(Cursor::new(bytes.clone()), None)),
            Some(ErrorCode::NeedPassword)
        );
        assert_eq!(
            code(FixtureReader::open(Cursor::new(bytes.clone()), Some(b"no"))),
            Some(ErrorCode::WrongPassword)
        );
        assert!(FixtureReader::open(Cursor::new(bytes.clone()), Some(b"pw")).is_ok());
        for cut in 0..bytes.len() {
            let _ = FixtureReader::open(Cursor::new(bytes[..cut].to_vec()), Some(b"pw"));
        }
        let mut huge = write(&[], None);
        let n = huge.len();
        huge[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            code(FixtureReader::open(Cursor::new(huge), None)),
            Some(ErrorCode::Corrupt)
        );
    }
}
