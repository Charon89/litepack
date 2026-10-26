//! Frames: `u32` LE payload length, `u16` LE kind, payload.
//!
//! The length is checked against the kind's cap ([`MAX_CONTROL`] or [`MAX_BULK`]) before the
//! payload is read, an unknown kind is refused before its payload is read, and every count inside
//! a payload is checked against the bytes left before a vector is reserved. Decoding never
//! panics and never allocates more than one frame's payload plus what that payload describes.

use std::io::{self, Read, Write};

use zeroize::Zeroizing;

/// Bytes of the frame header: length (`u32`) and kind (`u16`).
pub const HEADER_LEN: usize = 6;
/// The largest payload of a control frame (everything but `Entries` and `Data`).
pub const MAX_CONTROL: usize = 64 << 10;
/// The largest payload of a data frame or a list page.
pub const MAX_BULK: usize = 1 << 20;
/// The payload size the worker cuts member data into.
pub const DATA_CHUNK: usize = 256 << 10;
/// The longest name or link target a record may carry (the `.lpk` path limit).
pub const MAX_NAME: usize = 65535;
/// The longest password `Open` may carry.
pub const MAX_PASSWORD: usize = 4096;
/// The longest format name `Open` may carry.
pub const MAX_FORMAT: usize = 64;
/// The most member indices one `ReadMember` may carry.
pub const MAX_INDICES: usize = (MAX_CONTROL - 4) / 8;
/// The smallest encoded record: kind, flags, mtime, size, packed offset, packed length, block,
/// and the two length fields.
pub const RECORD_MIN: usize = 1 + 2 + 8 * 5 + 2 + 2;

/// Frame kinds. Responses (worker to host) are `0x00xx`, requests `0x01xx`.
pub mod kind {
    /// `Hello { proto, build }`, the worker's first frame.
    pub const HELLO: u16 = 0x0001;
    /// `Opened { format }`.
    pub const OPENED: u16 = 0x0002;
    /// `NeedPassword`.
    pub const NEED_PASSWORD: u16 = 0x0003;
    /// `Entries { records, more }` (a list page; bulk).
    pub const ENTRIES: u16 = 0x0004;
    /// `MemberStart { index }`.
    pub const MEMBER_START: u16 = 0x0005;
    /// `Data` (bulk).
    pub const DATA: u16 = 0x0006;
    /// `MemberEnd { index, checked }`.
    pub const MEMBER_END: u16 = 0x0007;
    /// `Cancelled`.
    pub const CANCELLED: u16 = 0x0008;
    /// `Error { code, message }`.
    pub const ERROR: u16 = 0x0009;
    /// `Fatal { message }` (a panic in the worker).
    pub const FATAL: u16 = 0x000A;
    /// `Open { password }`.
    pub const OPEN: u16 = 0x0101;
    /// `List { from, max }`.
    pub const LIST: u16 = 0x0102;
    /// `ReadMember { indices }`.
    pub const READ_MEMBER: u16 = 0x0103;
    /// `Cancel`.
    pub const CANCEL: u16 = 0x0104;
    /// `Close`.
    pub const CLOSE: u16 = 0x0105;
}

/// The cap of a kind's payload, or `None` for a kind that does not exist.
pub fn max_payload(k: u16) -> Option<usize> {
    match k {
        kind::ENTRIES | kind::DATA => Some(MAX_BULK),
        kind::HELLO
        | kind::OPENED
        | kind::NEED_PASSWORD
        | kind::MEMBER_START
        | kind::MEMBER_END
        | kind::CANCELLED
        | kind::ERROR
        | kind::FATAL
        | kind::OPEN
        | kind::LIST
        | kind::READ_MEMBER
        | kind::CANCEL
        | kind::CLOSE => Some(MAX_CONTROL),
        _ => None,
    }
}

/// Why a frame could not be read or decoded.
#[derive(Debug)]
pub enum ProtoError {
    /// The stream failed.
    Io(io::Error),
    /// The stream ended inside a frame.
    Truncated,
    /// A kind that does not exist (or is not allowed in this direction).
    UnknownKind(u16),
    /// A payload longer than its kind's cap.
    TooLong {
        /// The frame kind.
        kind: u16,
        /// The length the header gave.
        len: u32,
        /// The kind's cap.
        max: usize,
    },
    /// A payload that does not decode as its kind.
    Malformed(&'static str),
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoError::Io(e) => write!(f, "pipe error: {e}"),
            ProtoError::Truncated => write!(f, "stream ended inside a frame"),
            ProtoError::UnknownKind(k) => write!(f, "unknown frame kind {k:#06x}"),
            ProtoError::TooLong { kind, len, max } => {
                write!(f, "frame {kind:#06x} of {len} bytes exceeds {max}")
            }
            ProtoError::Malformed(m) => write!(f, "malformed frame: {m}"),
        }
    }
}

impl std::error::Error for ProtoError {}

/// Write one frame.
pub fn write_frame(w: &mut dyn Write, k: u16, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too long"))?;
    let mut head = [0u8; HEADER_LEN];
    head[..4].copy_from_slice(&len.to_le_bytes());
    head[4..].copy_from_slice(&k.to_le_bytes());
    w.write_all(&head)?;
    w.write_all(payload)?;
    w.flush()
}

/// Read one frame's kind and payload into `buf`. `Ok(None)` when the stream ended cleanly
/// before a header. The length is checked against the kind's cap before the payload is read.
pub fn read_frame(r: &mut dyn Read, buf: &mut Vec<u8>) -> Result<Option<u16>, ProtoError> {
    let mut head = [0u8; HEADER_LEN];
    let mut got = 0;
    while got < HEADER_LEN {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(ProtoError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe && got == 0 => return Ok(None),
            Err(e) => return Err(ProtoError::Io(e)),
        }
    }
    let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let k = u16::from_le_bytes([head[4], head[5]]);
    let max = max_payload(k).ok_or(ProtoError::UnknownKind(k))?;
    let n = usize::try_from(len).unwrap_or(usize::MAX);
    if n > max {
        return Err(ProtoError::TooLong { kind: k, len, max });
    }
    buf.clear();
    buf.resize(n, 0);
    let mut at = 0;
    while at < n {
        match r.read(&mut buf[at..]) {
            Ok(0) => return Err(ProtoError::Truncated),
            Ok(m) => at += m,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ProtoError::Io(e)),
        }
    }
    Ok(Some(k))
}

/// A bounds-checked reader over one payload.
struct Cur<'a> {
    b: &'a [u8],
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtoError> {
        if n > self.b.len() {
            return Err(ProtoError::Malformed("field past the end of the frame"));
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], ProtoError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.arr::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtoError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Result<u32, ProtoError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> Result<u64, ProtoError> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    fn i64(&mut self) -> Result<i64, ProtoError> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    fn bool(&mut self) -> Result<bool, ProtoError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtoError::Malformed("boolean not 0 or 1")),
        }
    }
    /// A `u16`-prefixed byte string of at most `max` bytes.
    fn bytes16(&mut self, max: usize) -> Result<&'a [u8], ProtoError> {
        let n = usize::from(self.u16()?);
        if n > max {
            return Err(ProtoError::Malformed("string too long"));
        }
        self.take(n)
    }
    fn text16(&mut self) -> Result<String, ProtoError> {
        let b = self.bytes16(MAX_CONTROL)?;
        Ok(String::from_utf8_lossy(b).into_owned())
    }
    /// A count of items of at least `min` bytes each, checked against the bytes left.
    fn count(&mut self, min: usize) -> Result<usize, ProtoError> {
        let n = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if n > self.b.len() / min.max(1) {
            return Err(ProtoError::Malformed("count exceeds the frame"));
        }
        Ok(n)
    }
    fn end(&self) -> Result<(), ProtoError> {
        if self.b.is_empty() {
            Ok(())
        } else {
            Err(ProtoError::Malformed("trailing bytes"))
        }
    }
}

fn put16(out: &mut Vec<u8>, b: &[u8]) {
    let n = b.len().min(usize::from(u16::MAX));
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&b[..n]);
}

/// What a record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link (`target` is its target).
    Symlink,
    /// A hard link to an earlier member (`target` is that member's name).
    HardLink,
}

impl RecordKind {
    fn code(self) -> u8 {
        match self {
            RecordKind::File => 0,
            RecordKind::Directory => 1,
            RecordKind::Symlink => 2,
            RecordKind::HardLink => 3,
        }
    }
    fn from_code(c: u8) -> Result<Self, ProtoError> {
        Ok(match c {
            0 => RecordKind::File,
            1 => RecordKind::Directory,
            2 => RecordKind::Symlink,
            3 => RecordKind::HardLink,
            _ => return Err(ProtoError::Malformed("unknown record kind")),
        })
    }
}

/// Record flag: the member is encrypted.
pub const FLAG_ENCRYPTED: u16 = 1;
/// No solid block (`Record::block`).
pub const NO_BLOCK: u64 = u64::MAX;

/// One listing record, as the worker reports it. Nothing in it is trusted by the host: names
/// pass the extraction policy, sizes are checked against the bytes that arrive, packed ranges
/// against the archive's length and each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// What the member is.
    pub kind: RecordKind,
    /// [`FLAG_ENCRYPTED`]; other bits are reserved.
    pub flags: u16,
    /// Nanoseconds since the Unix epoch; `i64::MIN` when unknown.
    pub mtime_ns: i64,
    /// The declared unpacked size (0 for directories and links).
    pub size: u64,
    /// Where the member's packed bytes start in the archive.
    pub packed_offset: u64,
    /// How many packed bytes (0 when the reader does not know).
    pub packed_len: u64,
    /// The solid block the member is in ([`NO_BLOCK`] when none).
    pub block: u64,
    /// The member's name as the archive stores it (the reader converts separators to `/`).
    pub name: Vec<u8>,
    /// A link's target; empty otherwise.
    pub target: Vec<u8>,
}

impl Record {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.kind.code());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.mtime_ns.to_le_bytes());
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(&self.packed_offset.to_le_bytes());
        out.extend_from_slice(&self.packed_len.to_le_bytes());
        out.extend_from_slice(&self.block.to_le_bytes());
        put16(out, &self.name);
        put16(out, &self.target);
    }

    /// The bytes [`Record::encode`] writes.
    pub fn encoded_len(&self) -> usize {
        RECORD_MIN + self.name.len().min(MAX_NAME) + self.target.len().min(MAX_NAME)
    }

    fn decode(c: &mut Cur<'_>) -> Result<Self, ProtoError> {
        Ok(Record {
            kind: RecordKind::from_code(c.u8()?)?,
            flags: c.u16()?,
            mtime_ns: c.i64()?,
            size: c.u64()?,
            packed_offset: c.u64()?,
            packed_len: c.u64()?,
            block: c.u64()?,
            name: c.bytes16(MAX_NAME)?.to_vec(),
            target: c.bytes16(MAX_NAME)?.to_vec(),
        })
    }
}

/// Host to worker.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// Open the archive as `format` (the host's detection, by magic) with a password when one
    /// was given. Answered `Opened`, `NeedPassword` or `Error`.
    Open {
        /// The format the host detected (its wire name, e.g. `zip`, `7z`, `tar`, `gz`); the
        /// worker selects its reader by it and refuses a name it does not know.
        format: String,
        /// The archive's file name without its directory (single-stream formats such as `.gz`
        /// name their one member from it when the stream stores no name); may be empty.
        name: Vec<u8>,
        /// The password, wiped on drop.
        password: Option<Zeroizing<Vec<u8>>>,
    },
    /// A page of the listing: at most `max` records from position `from`.
    List {
        /// The first record's position.
        from: u64,
        /// The most records.
        max: u32,
    },
    /// Stream the members at `indices` (strictly ascending): per member `MemberStart`,
    /// `Data`..., `MemberEnd`.
    ReadMember {
        /// Listing positions, strictly ascending.
        indices: Vec<u64>,
    },
    /// Stop the current `ReadMember`; answered `Cancelled`.
    Cancel,
    /// End the session; the worker exits.
    Close,
}

impl Request {
    /// The kind and payload; an `Open` whose format name is longer than [`MAX_FORMAT`] is
    /// refused (never truncated, which could split a UTF-8 character).
    pub fn encode(&self) -> Result<(u16, Vec<u8>), ProtoError> {
        let mut p = Vec::new();
        let k = match self {
            Request::Open {
                format,
                name,
                password,
            } => {
                if format.len() > MAX_FORMAT {
                    return Err(ProtoError::Malformed("format name longer than 64 bytes"));
                }
                match password {
                    Some(pw) => {
                        p.push(1);
                        put16(&mut p, &pw[..pw.len().min(MAX_PASSWORD)]);
                    }
                    None => p.push(0),
                }
                put16(&mut p, format.as_bytes());
                put16(&mut p, &name[..name.len().min(MAX_NAME)]);
                kind::OPEN
            }
            Request::List { from, max } => {
                p.extend_from_slice(&from.to_le_bytes());
                p.extend_from_slice(&max.to_le_bytes());
                kind::LIST
            }
            Request::ReadMember { indices } => {
                let n = indices.len().min(MAX_INDICES);
                p.extend_from_slice(&(n as u32).to_le_bytes());
                for i in &indices[..n] {
                    p.extend_from_slice(&i.to_le_bytes());
                }
                kind::READ_MEMBER
            }
            Request::Cancel => kind::CANCEL,
            Request::Close => kind::CLOSE,
        };
        Ok((k, p))
    }

    /// Decode a request frame.
    pub fn decode(k: u16, payload: &[u8]) -> Result<Self, ProtoError> {
        let mut c = Cur { b: payload };
        let r = match k {
            kind::OPEN => {
                let password = if c.bool()? {
                    Some(Zeroizing::new(c.bytes16(MAX_PASSWORD)?.to_vec()))
                } else {
                    None
                };
                let format = std::str::from_utf8(c.bytes16(MAX_FORMAT)?)
                    .map_err(|_| ProtoError::Malformed("format name is not UTF-8"))?
                    .to_string();
                let name = c.bytes16(MAX_NAME)?.to_vec();
                Request::Open {
                    format,
                    name,
                    password,
                }
            }
            kind::LIST => Request::List {
                from: c.u64()?,
                max: c.u32()?,
            },
            kind::READ_MEMBER => {
                let n = c.count(8)?;
                let mut indices = Vec::with_capacity(n);
                for _ in 0..n {
                    indices.push(c.u64()?);
                }
                Request::ReadMember { indices }
            }
            kind::CANCEL => Request::Cancel,
            kind::CLOSE => Request::Close,
            other => return Err(ProtoError::UnknownKind(other)),
        };
        c.end()?;
        Ok(r)
    }

    /// Write as one frame.
    pub fn write_to(&self, w: &mut dyn Write) -> io::Result<()> {
        let (k, p) = self
            .encode()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let p = Zeroizing::new(p);
        write_frame(w, k, &p)
    }

    /// Read one request; `Ok(None)` at a clean end of stream.
    pub fn read_from(r: &mut dyn Read, buf: &mut Vec<u8>) -> Result<Option<Self>, ProtoError> {
        match read_frame(r, buf)? {
            None => Ok(None),
            Some(k) => {
                let req = Request::decode(k, buf);
                // The payload may hold a password.
                zeroize::Zeroize::zeroize(buf);
                req.map(Some)
            }
        }
    }
}

/// Worker to host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// The worker's first frame.
    Hello {
        /// [`crate::PROTO`] of the worker.
        proto: u16,
        /// [`crate::BUILD`] of the worker.
        build: String,
    },
    /// The archive opened.
    Opened {
        /// The format's name, for messages.
        format: String,
    },
    /// The archive needs a password that was not given.
    NeedPassword,
    /// A page of the listing.
    Entries {
        /// The records, in listing order.
        records: Vec<Record>,
        /// More records follow.
        more: bool,
    },
    /// A member's data starts.
    MemberStart {
        /// Its listing position.
        index: u64,
    },
    /// Member bytes.
    Data(Vec<u8>),
    /// A member's data ended.
    MemberEnd {
        /// Its listing position.
        index: u64,
        /// The reader verified a checksum the archive stores.
        checked: bool,
    },
    /// The `ReadMember` was stopped by `Cancel`.
    Cancelled,
    /// A request failed.
    Error {
        /// [`crate::ErrorCode`] as a number (an unknown number is kept).
        code: u16,
        /// The reader's message (shown escaped, never parsed).
        message: String,
    },
    /// The worker panicked.
    Fatal {
        /// The panic message.
        message: String,
    },
}

impl Response {
    /// The kind and payload.
    pub fn encode(&self) -> (u16, Vec<u8>) {
        let mut p = Vec::new();
        let k = match self {
            Response::Hello { proto, build } => {
                p.extend_from_slice(&proto.to_le_bytes());
                put16(&mut p, build.as_bytes());
                kind::HELLO
            }
            Response::Opened { format } => {
                put16(&mut p, format.as_bytes());
                kind::OPENED
            }
            Response::NeedPassword => kind::NEED_PASSWORD,
            Response::Entries { records, more } => {
                p.extend_from_slice(&(records.len() as u32).to_le_bytes());
                p.push(u8::from(*more));
                for r in records {
                    r.encode(&mut p);
                }
                kind::ENTRIES
            }
            Response::MemberStart { index } => {
                p.extend_from_slice(&index.to_le_bytes());
                kind::MEMBER_START
            }
            Response::Data(d) => {
                p.extend_from_slice(d);
                kind::DATA
            }
            Response::MemberEnd { index, checked } => {
                p.extend_from_slice(&index.to_le_bytes());
                p.push(u8::from(*checked));
                kind::MEMBER_END
            }
            Response::Cancelled => kind::CANCELLED,
            Response::Error { code, message } => {
                p.extend_from_slice(&code.to_le_bytes());
                put16(&mut p, truncate(message, MAX_CONTROL - 4).as_bytes());
                kind::ERROR
            }
            Response::Fatal { message } => {
                put16(&mut p, truncate(message, MAX_CONTROL - 4).as_bytes());
                kind::FATAL
            }
        };
        (k, p)
    }

    /// Decode a response frame (whose length [`read_frame`] already checked).
    pub fn decode(k: u16, payload: &[u8]) -> Result<Self, ProtoError> {
        let mut c = Cur { b: payload };
        let r = match k {
            kind::HELLO => Response::Hello {
                proto: c.u16()?,
                build: c.text16()?,
            },
            kind::OPENED => Response::Opened {
                format: c.text16()?,
            },
            kind::NEED_PASSWORD => Response::NeedPassword,
            kind::ENTRIES => {
                let n = c.count(1)?;
                let more = c.bool()?;
                // Checked again past the `more` byte, against the smallest record.
                if n > c.b.len() / RECORD_MIN {
                    return Err(ProtoError::Malformed("record count exceeds the frame"));
                }
                let mut records = Vec::with_capacity(n);
                for _ in 0..n {
                    records.push(Record::decode(&mut c)?);
                }
                Response::Entries { records, more }
            }
            kind::MEMBER_START => Response::MemberStart { index: c.u64()? },
            kind::DATA => {
                let d = c.take(c.b.len())?.to_vec();
                Response::Data(d)
            }
            kind::MEMBER_END => Response::MemberEnd {
                index: c.u64()?,
                checked: c.bool()?,
            },
            kind::CANCELLED => Response::Cancelled,
            kind::ERROR => Response::Error {
                code: c.u16()?,
                message: c.text16()?,
            },
            kind::FATAL => Response::Fatal {
                message: c.text16()?,
            },
            other => return Err(ProtoError::UnknownKind(other)),
        };
        c.end()?;
        Ok(r)
    }

    /// Write as one frame.
    pub fn write_to(&self, w: &mut dyn Write) -> io::Result<()> {
        let (k, p) = self.encode();
        write_frame(w, k, &p)
    }

    /// Read one response; `Ok(None)` at a clean end of stream.
    pub fn read_from(r: &mut dyn Read, buf: &mut Vec<u8>) -> Result<Option<Self>, ProtoError> {
        match read_frame(r, buf)? {
            None => Ok(None),
            Some(k) => Response::decode(k, buf).map(Some),
        }
    }
}

/// At most `max` bytes of `s`, cut at a character boundary.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str) -> Record {
        Record {
            kind: RecordKind::File,
            flags: 0,
            mtime_ns: 1_700_000_000_000_000_000,
            size: 5,
            packed_offset: 10,
            packed_len: 5,
            block: 0,
            name: name.as_bytes().to_vec(),
            target: Vec::new(),
        }
    }

    fn round_trip_response(r: Response) {
        let mut buf = Vec::new();
        r.write_to(&mut buf).unwrap();
        let mut scratch = Vec::new();
        let back = Response::read_from(&mut buf.as_slice(), &mut scratch)
            .unwrap()
            .unwrap();
        assert_eq!(back, r);
    }

    fn round_trip_request(r: Request) {
        let mut buf = Vec::new();
        r.write_to(&mut buf).unwrap();
        let mut scratch = Vec::new();
        let back = Request::read_from(&mut buf.as_slice(), &mut scratch)
            .unwrap()
            .unwrap();
        assert_eq!(back, r);
        assert!(scratch.iter().all(|&b| b == 0), "payload not wiped");
    }

    #[test]
    fn an_over_long_format_name_is_refused_not_truncated() {
        let r = Request::Open {
            format: "\u{e9}".repeat(40), // 80 bytes of two-byte characters
            name: Vec::new(),
            password: None,
        };
        assert!(r.encode().is_err());
        assert!(r.write_to(&mut Vec::new()).is_err());
        let ok = Request::Open {
            format: "x".repeat(MAX_FORMAT),
            name: Vec::new(),
            password: None,
        };
        assert!(ok.encode().is_ok());
    }

    #[test]
    fn every_frame_round_trips() {
        round_trip_request(Request::Open {
            format: "zip".into(),
            name: b"a.zip".to_vec(),
            password: None,
        });
        round_trip_request(Request::Open {
            format: "7z".into(),
            name: Vec::new(),
            password: Some(Zeroizing::new(b"secret".to_vec())),
        });
        round_trip_request(Request::List { from: 7, max: 100 });
        round_trip_request(Request::ReadMember {
            indices: vec![1, 2, 9],
        });
        round_trip_request(Request::Cancel);
        round_trip_request(Request::Close);
        round_trip_response(Response::Hello {
            proto: 1,
            build: crate::BUILD.into(),
        });
        round_trip_response(Response::Opened {
            format: "fixture".into(),
        });
        round_trip_response(Response::NeedPassword);
        let mut link = rec("l");
        link.kind = RecordKind::Symlink;
        link.target = b"a/b".to_vec();
        round_trip_response(Response::Entries {
            records: vec![rec("a"), link],
            more: true,
        });
        round_trip_response(Response::MemberStart { index: 3 });
        round_trip_response(Response::Data(vec![1, 2, 3]));
        round_trip_response(Response::MemberEnd {
            index: 3,
            checked: true,
        });
        round_trip_response(Response::Cancelled);
        round_trip_response(Response::Error {
            code: 4,
            message: "bad".into(),
        });
        round_trip_response(Response::Fatal {
            message: "boom".into(),
        });
    }

    #[test]
    fn length_and_kind_are_checked_before_the_payload() {
        // A data frame claiming 4 GiB: refused from the header alone.
        let mut f = Vec::new();
        f.extend_from_slice(&u32::MAX.to_le_bytes());
        f.extend_from_slice(&kind::DATA.to_le_bytes());
        let mut buf = Vec::new();
        let e = read_frame(&mut f.as_slice(), &mut buf).unwrap_err();
        assert!(matches!(e, ProtoError::TooLong { .. }), "{e}");
        assert_eq!(buf.capacity(), 0);
        // A control frame past 64 KiB.
        let mut f = Vec::new();
        f.extend_from_slice(&((MAX_CONTROL + 1) as u32).to_le_bytes());
        f.extend_from_slice(&kind::HELLO.to_le_bytes());
        assert!(matches!(
            read_frame(&mut f.as_slice(), &mut buf),
            Err(ProtoError::TooLong { .. })
        ));
        // An unknown kind.
        let mut f = Vec::new();
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&0xBEEFu16.to_le_bytes());
        assert!(matches!(
            read_frame(&mut f.as_slice(), &mut buf),
            Err(ProtoError::UnknownKind(0xBEEF))
        ));
        // Truncated header and payload; a clean end.
        assert!(matches!(
            read_frame(&mut [1u8, 0].as_slice(), &mut buf),
            Err(ProtoError::Truncated)
        ));
        let mut f = Vec::new();
        write_frame(&mut f, kind::DATA, &[1, 2, 3]).unwrap();
        f.pop();
        assert!(matches!(
            read_frame(&mut f.as_slice(), &mut buf),
            Err(ProtoError::Truncated)
        ));
        assert!(read_frame(&mut [0u8; 0].as_slice(), &mut buf)
            .unwrap()
            .is_none());
    }

    #[test]
    fn huge_counts_are_refused_before_allocation() {
        // Entries claiming 4 billion records in a 10-byte payload.
        let mut p = Vec::new();
        p.extend_from_slice(&u32::MAX.to_le_bytes());
        p.push(0);
        p.extend_from_slice(&[0u8; 5]);
        let e = Response::decode(kind::ENTRIES, &p).unwrap_err();
        assert!(matches!(e, ProtoError::Malformed(_)), "{e}");
        // ReadMember claiming more indices than the frame holds.
        let mut p = Vec::new();
        p.extend_from_slice(&1000u32.to_le_bytes());
        p.extend_from_slice(&[0u8; 16]);
        assert!(Request::decode(kind::READ_MEMBER, &p).is_err());
        // A record whose name length runs past the frame.
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_le_bytes());
        p.push(0);
        let mut r = Vec::new();
        rec("abc").encode(&mut r);
        let at = RECORD_MIN - 4;
        r[at..at + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        p.extend_from_slice(&r);
        assert!(Response::decode(kind::ENTRIES, &p).is_err());
    }

    #[test]
    fn malformed_payloads_are_errors_not_panics() {
        for k in [
            kind::HELLO,
            kind::OPENED,
            kind::ENTRIES,
            kind::MEMBER_START,
            kind::MEMBER_END,
            kind::ERROR,
            kind::FATAL,
        ] {
            for len in 0..80 {
                let p: Vec<u8> = (0..len).map(|i| (i * 37 + k as usize) as u8).collect();
                let _ = Response::decode(k, &p);
            }
        }
        // Trailing bytes and a bad boolean.
        assert!(Response::decode(kind::CANCELLED, &[0]).is_err());
        assert!(Response::decode(kind::MEMBER_END, &[0, 0, 0, 0, 0, 0, 0, 0, 2]).is_err());
        // A request kind is not a response.
        assert!(Response::decode(kind::OPEN, &[0]).is_err());
        assert!(Request::decode(kind::HELLO, &[]).is_err());
    }
}
