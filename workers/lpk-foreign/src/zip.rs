//! ZIP (and JAR, EPUB, Office files, self-extracting ZIPs) through the `zip` crate.
//!
//! Records come from the central directory in its order; each one's packed range runs from
//! its local header to the end of its data (the local header is read at listing time, so a
//! record without a valid local header is refused before anything is streamed). Names are
//! decoded as the crate does (UTF-8 when flagged, else CP437) with backslashes read as
//! separators. bzip2 (method 12) and zstd (method 93) members are read raw and decoded by
//! `bzip2-rs` and `ruzstd`, their CRC-32 checked here; encrypted ones of those methods are a
//! named gap. A password, when given, is checked at `Open` against the first encrypted member
//! (ZipCrypto's check byte or AES's verifier), so a wrong one is `WrongPassword` before any
//! write.

use std::fs::File;
use std::io::{BufReader, Read};

use ::zip::result::ZipError;
use ::zip::ZipArchive;
use lpk_proto::frame::{FLAG_ENCRYPTED, NO_BLOCK};
use lpk_proto::reader::Sink;
use lpk_proto::{ErrorCode, ForeignReader, ReaderError, Record, RecordKind, Zeroizing};

use crate::{civil_ns, copy_to_sink, CopyError};

/// The longest symlink target read from a member.
const MAX_TARGET: u64 = 4096;
/// ZIP's bzip2 and zstd method numbers.
const METHOD_BZIP2: u16 = 12;
const METHOD_ZSTD: u16 = 93;

fn zip_error(e: ZipError) -> ReaderError {
    match e {
        ZipError::Io(e) => ReaderError::new(ErrorCode::Io, e.to_string()),
        ZipError::InvalidArchive(m) => ReaderError::corrupt(format!("ZIP: {m}")),
        ZipError::UnsupportedArchive(m) if m == ZipError::PASSWORD_REQUIRED => {
            ReaderError::new(ErrorCode::NeedPassword, "a password is required")
        }
        ZipError::UnsupportedArchive(m) => {
            ReaderError::new(ErrorCode::Unsupported, format!("ZIP: {m}"))
        }
        ZipError::InvalidPassword => ReaderError::new(ErrorCode::WrongPassword, "wrong password"),
        ZipError::CompressionMethodNotSupported(m) => ReaderError::new(
            ErrorCode::Unsupported,
            format!("ZIP compression method {m} is not supported"),
        ),
        other => ReaderError::corrupt(format!("ZIP: {other}")),
    }
}

/// One member as listed, with what streaming it needs.
#[derive(Debug)]
struct Member {
    method: u16,
    encrypted: bool,
    crc: u32,
    size: u64,
}

/// A ZIP archive open in the worker.
#[derive(Debug)]
pub struct ZipReader {
    zip: ZipArchive<BufReader<File>>,
    records: Vec<Record>,
    members: Vec<Member>,
    password: Option<Zeroizing<Vec<u8>>>,
}

/// A name as the archive stores it, separators made `/`, a directory's trailing `/` dropped.
fn clean_name(name: &str) -> Vec<u8> {
    let mut n = name.replace('\\', "/");
    while n.ends_with('/') {
        n.pop();
    }
    n.into_bytes()
}

impl ZipReader {
    /// Read the central directory and every local header; check the password if one is given.
    pub fn open(archive: File, password: Option<&[u8]>) -> Result<Self, ReaderError> {
        let mut zip = ZipArchive::new(BufReader::new(archive)).map_err(zip_error)?;
        let mut records = Vec::with_capacity(zip.len());
        let mut members = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i).map_err(zip_error)?;
            let name = clean_name(&f.name().map_err(zip_error)?);
            let start = f.header_start();
            let data = f
                .data_start()
                .ok_or_else(|| ReaderError::corrupt("ZIP: a member's data start is unknown"))?;
            let end = data
                .checked_add(f.compressed_size())
                .ok_or_else(|| ReaderError::corrupt("ZIP: a member's size overflows"))?;
            let mut mtime_ns = f
                .last_modified()
                .filter(|d| d.is_valid())
                .map(|d| {
                    civil_ns(
                        i64::from(d.year()),
                        u32::from(d.month()),
                        u32::from(d.day()),
                        u32::from(d.hour()),
                        u32::from(d.minute()),
                        u32::from(d.second()),
                    )
                })
                .unwrap_or(i64::MIN);
            for x in f.extra_data_fields() {
                if let ::zip::extra_fields::ExtraField::ExtendedTimestamp(t) = x {
                    if let Some(m) = t.mod_time() {
                        mtime_ns = i64::from(m).saturating_mul(1_000_000_000);
                    }
                }
            }
            let encrypted = f.encrypted();
            let kind = if f.is_dir() {
                RecordKind::Directory
            } else if f.is_symlink() && !encrypted && f.size() <= MAX_TARGET {
                RecordKind::Symlink
            } else {
                RecordKind::File
            };
            // The method's number whatever features unify into `zip` (a fuzz build may turn
            // its `zstd` feature on and change the variant 93 maps to).
            #[allow(deprecated)]
            let method = f.compression().to_u16();
            let member = Member {
                method,
                encrypted,
                crc: f.crc32(),
                size: f.size(),
            };
            drop(f);
            let mut target = Vec::new();
            if kind == RecordKind::Symlink {
                let mut m = zip.by_index(i).map_err(zip_error)?;
                (&mut m)
                    .take(MAX_TARGET)
                    .read_to_end(&mut target)
                    .map_err(|e| ReaderError::corrupt(format!("ZIP: a link's target: {e}")))?;
            }
            let kind = if kind == RecordKind::Symlink && target.is_empty() {
                RecordKind::File
            } else {
                kind
            };
            records.push(Record {
                kind,
                flags: if encrypted { FLAG_ENCRYPTED } else { 0 },
                mtime_ns,
                size: if kind == RecordKind::File {
                    member.size
                } else {
                    0
                },
                packed_offset: start,
                packed_len: end.saturating_sub(start),
                block: NO_BLOCK,
                name,
                target,
            });
            members.push(member);
        }
        let mut r = ZipReader {
            zip,
            records,
            members,
            password: password.map(|p| Zeroizing::new(p.to_vec())),
        };
        r.check_password()?;
        Ok(r)
    }

    /// With a password: open the first encrypted member the crate decrypts (its check byte or
    /// AES verifier fails a wrong password).
    fn check_password(&mut self) -> Result<(), ReaderError> {
        let Some(pw) = self.password.clone() else {
            return Ok(());
        };
        // The smallest encrypted member the crate decrypts: its check byte or verifier first,
        // then its data decoded (whole when at most 1 MiB, so its CRC-32 or authentication
        // code is checked too; else its first 64 KiB), since ZipCrypto's check byte lets one
        // wrong password in 256 through.
        let smallest = self
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.encrypted && m.method != METHOD_BZIP2 && m.method != METHOD_ZSTD)
            // An empty member proves nothing under ZipCrypto (no data, CRC 0): last choice.
            .min_by_key(|(_, m)| (m.size == 0, m.size))
            .map(|(i, m)| (i, m.size));
        if let Some((i, size)) = smallest {
            let mut f = self.zip.by_index_decrypt(i, &pw).map_err(zip_error)?;
            let limit = if size <= 1 << 20 { u64::MAX } else { 64 << 10 };
            let mut probe = (&mut f).take(limit);
            // An error before the first decoded byte is a wrong password. One after output
            // began does not fail Open: the check byte passed and data decoded, so the password
            // is plausible, and that member alone fails when extracted (Corrupt, worded as
            // damaged data or a wrong password).
            let mut buf = vec![0u8; 64 * 1024];
            let mut out = 0u64;
            loop {
                match probe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => out += n as u64,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if out == 0 => {
                        return Err(ReaderError::new(
                            ErrorCode::WrongPassword,
                            format!("wrong password (ZIP: {e})"),
                        ))
                    }
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }
}

impl ForeignReader for ZipReader {
    fn format(&self) -> String {
        "zip".to_string()
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
        let m = self
            .members
            .get(i)
            .ok_or_else(|| ReaderError::new(ErrorCode::NoSuchMember, "no such member"))?;
        let (method, encrypted, crc, size) = (m.method, m.encrypted, m.crc, m.size);
        let what = String::from_utf8_lossy(&self.records[i].name).into_owned();
        if method == METHOD_BZIP2 || method == METHOD_ZSTD {
            if encrypted {
                return Err(ReaderError::new(
                    ErrorCode::Unsupported,
                    format!("{what}: an encrypted bzip2 or zstd ZIP member (a named gap)"),
                ));
            }
            let raw = self.zip.by_index_raw(i).map_err(zip_error)?;
            let mut dec: Box<dyn Read + '_> = if method == METHOD_BZIP2 {
                Box::new(crate::stream::Bzip2::new(raw))
            } else {
                Box::new(crate::stream::Zstd::new(BufReader::new(raw)))
            };
            let mut hasher = crc32fast::Hasher::new();
            let mut tee = Tee {
                inner: &mut dec,
                hasher: &mut hasher,
            };
            let n = copy_to_sink(&mut tee, sink, size.saturating_add(1))
                .map_err(|e| e.into_reader(&what, false))?;
            if n != size {
                return Err(ReaderError::corrupt(format!(
                    "{what}: {n} bytes decoded where {size} were declared"
                )));
            }
            if hasher.finalize() != crc {
                return Err(ReaderError::corrupt(format!("{what}: CRC-32 mismatch")));
            }
            return Ok(true);
        }
        let pw = self.password.clone();
        let mut f = match (&pw, encrypted) {
            (Some(p), true) => self.zip.by_index_decrypt(i, p),
            _ => self.zip.by_index(i),
        }
        .map_err(zip_error)?;
        // The crate checks the CRC-32 (or AES's authentication code) at the end of the data.
        copy_to_sink(&mut f, sink, u64::MAX)
            .map_err(|e: CopyError| e.into_reader(&what, encrypted))?;
        Ok(true)
    }
}

/// A reader that hashes what passes through it.
struct Tee<'a, 'b> {
    inner: &'a mut Box<dyn Read + 'b>,
    hasher: &'a mut crc32fast::Hasher,
}

impl Read for Tee<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}
