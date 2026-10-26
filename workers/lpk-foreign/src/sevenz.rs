//! 7z through `sevenz-rust2`: at the start of the file, or after a self-extractor's stub (the
//! first start header in the first 4 MiB whose CRC holds).
//!
//! Records follow the archive's file list; a member's packed range is its block's pack
//! streams, its `block` the block's index, so the host's overlap check sees blocks, not
//! members. A block is decoded by a thread that hands each member's bytes over a bounded
//! channel; members asked for in ascending order are taken from the same decoding (a solid
//! block decodes once), and an earlier member restarts the block. Each member's CRC is checked
//! by the crate (`MemberEnd { checked }`).
//!
//! Passwords: a 7z whose header is encrypted needs it at `Open` (`NeedPassword`; a wrong one
//! is `WrongPassword` there); one whose data only is encrypted lists without it, and the host
//! asks for it before streaming an encrypted member.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::mpsc;

use lpk_proto::frame::{FLAG_ENCRYPTED, NO_BLOCK};
use lpk_proto::reader::Sink;
use lpk_proto::{ErrorCode, ForeignReader, ReaderError, Record, RecordKind};
use sevenz_rust2::{Archive, BlockDecoder, EncoderMethod, Password};

use crate::Window;

/// How far into the file a 7z start header is looked for.
const SCAN: u64 = 4 << 20;
const SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
/// The 7z signature header's length (pack positions count from its end).
const START_HEADER: u64 = 32;

fn sz_error(e: sevenz_rust2::Error, encrypted: bool) -> ReaderError {
    use sevenz_rust2::Error as E;
    match e {
        E::PasswordRequired => ReaderError::new(ErrorCode::NeedPassword, "a password is required"),
        E::MaybeBadPassword(e) => ReaderError::new(
            ErrorCode::WrongPassword,
            format!("7z: {e} (a wrong password, or damaged data)"),
        ),
        E::UnsupportedCompressionMethod(m) => ReaderError::new(
            ErrorCode::Unsupported,
            format!("7z: the coder {m} is not supported"),
        ),
        E::Unsupported(m) => ReaderError::new(ErrorCode::Unsupported, format!("7z: {m}")),
        E::ExternalUnsupported => {
            ReaderError::new(ErrorCode::Unsupported, "7z: external streams are not read")
        }
        other if encrypted => ReaderError::new(
            ErrorCode::WrongPassword,
            format!("7z: {other} (a wrong password, or damaged data)"),
        ),
        other => ReaderError::corrupt(format!("7z: {other}")),
    }
}

/// The offset of the first 7z start header (signature, version 0, CRC of the next 20 bytes)
/// in the first [`SCAN`] bytes.
fn find_start(f: &mut File) -> std::io::Result<Option<u64>> {
    let mut buf = Vec::new();
    f.seek(SeekFrom::Start(0))?;
    f.take(SCAN + START_HEADER).read_to_end(&mut buf)?;
    let last = buf.len().saturating_sub(START_HEADER as usize);
    for i in 0..=last.min(buf.len()) {
        if buf.len() < i + START_HEADER as usize {
            break;
        }
        let h = &buf[i..i + START_HEADER as usize];
        if h[..6] == SIGNATURE
            && h[6] == 0
            && u32::from_le_bytes([h[8], h[9], h[10], h[11]]) == crc32fast::hash(&h[12..32])
        {
            return Ok(Some(i as u64));
        }
    }
    Ok(None)
}

/// What a block's decoding thread sends.
enum Msg {
    Data(usize, Vec<u8>),
    End(usize),
    Failed(ReaderError),
}

/// A block being decoded: its index, the channel, the next file index it will send.
struct Running {
    block: usize,
    rx: mpsc::Receiver<Msg>,
    /// Data of a member after the one being read (taken off the channel already).
    held: Option<Msg>,
    /// The lowest file index whose data may still come (members below it are gone).
    next: usize,
}

/// A 7z archive open in the worker.
pub struct SevenZReader {
    file: File,
    base: u64,
    archive: Archive,
    password: Password,
    has_password: bool,
    records: Vec<Record>,
    running: Option<Running>,
}

impl std::fmt::Debug for SevenZReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SevenZReader")
            .field("base", &self.base)
            .field("records", &self.records.len())
            .finish_non_exhaustive()
    }
}

fn password_of(p: &[u8]) -> Password {
    match std::str::from_utf8(p) {
        Ok(s) => Password::new(s),
        Err(_) => Password::from_raw(p),
    }
}

impl SevenZReader {
    /// Read the archive's header (with the password when its header is encrypted).
    pub fn open(mut file: File, password: Option<&[u8]>) -> Result<Self, ReaderError> {
        let base = find_start(&mut file)?
            .ok_or_else(|| ReaderError::corrupt("7z: no start header in the first 4 MiB"))?;
        let mut view = Window::new(file.try_clone()?, base)?;
        let archive = match Archive::read(&mut view, &Password::empty()) {
            Ok(a) => a,
            Err(sevenz_rust2::Error::PasswordRequired) => {
                let Some(pw) = password else {
                    return Err(ReaderError::new(
                        ErrorCode::NeedPassword,
                        "the archive's header is encrypted",
                    ));
                };
                view.seek(SeekFrom::Start(0))?;
                Archive::read(&mut view, &password_of(pw)).map_err(|e| match e {
                    sevenz_rust2::Error::Io(..) | sevenz_rust2::Error::FileOpen(..) => {
                        sz_error(e, false)
                    }
                    _ => ReaderError::new(ErrorCode::WrongPassword, "wrong password"),
                })?
            }
            Err(e) => return Err(sz_error(e, false)),
        };
        let mut r = SevenZReader {
            file,
            base,
            password: password.map_or_else(Password::empty, password_of),
            has_password: password.is_some(),
            archive,
            records: Vec::new(),
            running: None,
        };
        r.records = r.build_records()?;
        Ok(r)
    }

    fn block_encrypted(&self, b: usize) -> bool {
        self.archive.blocks.get(b).is_some_and(|blk| {
            blk.coders
                .iter()
                .any(|c| c.encoder_method_id() == EncoderMethod::ID_AES256_SHA256)
        })
    }

    fn build_records(&self) -> Result<Vec<Record>, ReaderError> {
        let a = &self.archive;
        let firsts = a.stream_map.block_first_pack_stream_index();
        let offsets = a.stream_map.pack_stream_offsets();
        let sizes = a.pack_sizes();
        let mut out = Vec::with_capacity(a.files.len());
        for (i, f) in a.files.iter().enumerate() {
            if f.is_anti_item() {
                return Err(ReaderError::new(
                    ErrorCode::Unsupported,
                    "7z: anti-items (deletions of an update archive) are not read",
                ));
            }
            let mut name = f.name().replace('\\', "/");
            while name.ends_with('/') {
                name.pop();
            }
            let block = a.stream_map.file_block_index.get(i).copied().flatten();
            let (packed_offset, packed_len, block_id, encrypted) = match block {
                Some(b) if f.has_stream() && f.size() > 0 => {
                    let first = *firsts.get(b).ok_or_else(bad_map)?;
                    let next = firsts.get(b + 1).copied().unwrap_or(sizes.len());
                    let len = sizes
                        .get(first..next)
                        .ok_or_else(bad_map)?
                        .iter()
                        .try_fold(0u64, |s, &n| s.checked_add(n))
                        .ok_or_else(bad_map)?;
                    let off = offsets
                        .get(first)
                        .and_then(|&o| {
                            self.base
                                .checked_add(START_HEADER)?
                                .checked_add(a.pack_pos())?
                                .checked_add(o)
                        })
                        .ok_or_else(bad_map)?;
                    (off, len, b as u64, self.block_encrypted(b))
                }
                _ => (0, 0, NO_BLOCK, false),
            };
            let mtime_ns = if f.has_last_modified_date {
                let ticks = u64::from(f.last_modified_date());
                i64::try_from(ticks)
                    .ok()
                    .map(|t| (t - 116_444_736_000_000_000).saturating_mul(100))
                    .unwrap_or(i64::MIN)
            } else {
                i64::MIN
            };
            let dir = f.is_directory();
            out.push(Record {
                kind: if dir {
                    RecordKind::Directory
                } else {
                    RecordKind::File
                },
                flags: if encrypted { FLAG_ENCRYPTED } else { 0 },
                mtime_ns,
                size: if dir { 0 } else { f.size() },
                packed_offset,
                packed_len,
                block: block_id,
                name: name.into_bytes(),
                target: Vec::new(),
            });
        }
        Ok(out)
    }

    /// Start decoding block `b` on a thread.
    fn start(&mut self, b: usize) -> Result<Running, ReaderError> {
        let file = self.file.try_clone()?;
        let base = self.base;
        let archive = self.archive.clone();
        let password = self.password.clone();
        let encrypted = self.has_password && self.block_encrypted(b);
        let (tx, rx) = mpsc::sync_channel::<Msg>(4);
        std::thread::Builder::new()
            .name("lpk-7z-block".into())
            .spawn(move || {
                let mut src = match Window::new(file, base) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Msg::Failed(e.into()));
                        return;
                    }
                };
                let first = archive.stream_map.block_first_file_index.get(b).copied();
                let Some(first) = first else {
                    let _ = tx.send(Msg::Failed(ReaderError::corrupt("7z: no such block")));
                    return;
                };
                let mut k = 0usize;
                let dec = BlockDecoder::new(1, b, &archive, &password, &mut src);
                let r = dec.for_each_entries(&mut |_entry, reader: &mut dyn Read| {
                    let idx = first + k;
                    k += 1;
                    let mut buf = vec![0u8; crate::CHUNK];
                    loop {
                        let n = reader.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        if tx.send(Msg::Data(idx, buf[..n].to_vec())).is_err() {
                            return Ok(false);
                        }
                    }
                    Ok(tx.send(Msg::End(idx)).is_ok())
                });
                if let Err(e) = r {
                    let _ = tx.send(Msg::Failed(sz_error(e, encrypted)));
                }
            })
            .map_err(|e| ReaderError::new(ErrorCode::Io, format!("7z: a thread: {e}")))?;
        Ok(Running {
            block: b,
            rx,
            held: None,
            next: 0,
        })
    }
}

fn bad_map() -> ReaderError {
    ReaderError::corrupt("7z: a block's pack streams are out of range")
}

impl ForeignReader for SevenZReader {
    fn format(&self) -> String {
        "7z".to_string()
    }

    fn entries(&mut self, from: u64, max: usize) -> Result<(Vec<Record>, bool), ReaderError> {
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        if from > self.records.len() {
            return Err(ReaderError::new(
                ErrorCode::NoSuchMember,
                "past the listing",
            ));
        }
        let end = from.saturating_add(max).min(self.records.len());
        Ok((self.records[from..end].to_vec(), end < self.records.len()))
    }

    fn read_member(&mut self, index: u64, sink: &mut Sink<'_>) -> Result<bool, ReaderError> {
        let i = usize::try_from(index).unwrap_or(usize::MAX);
        let rec = self
            .records
            .get(i)
            .ok_or_else(|| ReaderError::new(ErrorCode::NoSuchMember, "no such member"))?;
        if rec.block == NO_BLOCK {
            return Ok(false);
        }
        let b = usize::try_from(rec.block).unwrap_or(usize::MAX);
        let has_crc = self.archive.files.get(i).is_some_and(|f| f.has_crc);
        let reuse = match &self.running {
            Some(r) if r.block == b => match &r.held {
                Some(Msg::Data(idx, _)) | Some(Msg::End(idx)) => *idx <= i,
                Some(Msg::Failed(_)) => false,
                None => r.next <= i,
            },
            _ => false,
        };
        if !reuse {
            self.running = None;
            self.running = Some(self.start(b)?);
        }
        let Some(run) = self.running.as_mut() else {
            return Err(ReaderError::corrupt("7z: no block decoder"));
        };
        loop {
            let msg = match run.held.take() {
                Some(m) => m,
                None => match run.rx.recv() {
                    Ok(m) => m,
                    Err(_) => {
                        self.running = None;
                        return Err(ReaderError::corrupt(
                            "7z: the block ended before the member",
                        ));
                    }
                },
            };
            match &msg {
                Msg::Data(idx, _) => run.next = *idx,
                Msg::End(idx) => run.next = idx + 1,
                Msg::Failed(_) => {}
            }
            match msg {
                Msg::Data(idx, d) if idx == i => {
                    if sink(&d).is_err() {
                        self.running = None;
                        return Err(lpk_proto::Stopped.into());
                    }
                }
                Msg::Data(idx, _) | Msg::End(idx) if idx < i => {}
                Msg::End(idx) if idx == i => return Ok(has_crc),
                m @ (Msg::Data(..) | Msg::End(_)) => {
                    // A later member: this one was not in the block's stream.
                    run.held = Some(m);
                    return Err(ReaderError::corrupt("7z: the member is not in its block"));
                }
                Msg::Failed(e) => {
                    self.running = None;
                    return Err(e);
                }
            }
        }
    }
}
