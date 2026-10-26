//! The [`ForeignReader`] over libarchive 3.8.9 (E4-4; design `docs/notes/e4-design.md` §3),
//! run only inside the sandboxed `lpk-worker-libarchive`.
//!
//! The host detects the format by magic and sends its wire name in `Open`; [`open`] registers
//! only that format's libarchive readers and filters (never `…_all`):
//!
//! | wire | readers | filters | checksums verified |
//! |---|---|---|---|
//! | `rar` | `rar`, `rar5` | — | yes (CRC-32, BLAKE2sp) |
//! | `iso` | `iso9660` | — | no |
//! | `cab` | `cab` | — | yes (CFDATA checksum) |
//! | `lzh` | `lha` | — | yes (CRC-16) |
//! | `ar` | `ar` | — | no |
//! | `cpio` | `cpio` | — | no |
//! | `rpm` | `cpio` | `rpm`, `gzip`, `xz`, `zstd` | gzip's CRC only |
//! | `z` | `tar`, `raw` | `compress` | no |
//! | `uue` | `tar`, `raw` | `uu`, `compress`, `gzip`, `xz`, `zstd` | no |
//!
//! RAR's named gaps are found before libarchive sees the archive ([`rar::named_gap`]):
//! encrypted RAR (either version, headers or data), solid RAR4, RAR 1.5/2.x compression and RAR5
//! dictionaries above 64 MiB; filters inside PPMd are reported from libarchive's error. Each is
//! an `Unsupported` error naming the gap, never a crash.
//!
//! libarchive is a streaming reader: the listing is one pass at `Open`; members are streamed in
//! order from a second pass, which restarts from the first volume when a member before the
//! current position is asked for.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::fs::File;
use std::io::{Seek, SeekFrom};

use lpk_libarchive_sys::{Archive, EntryKind, ErrorKind as LaErrorKind, OpenSpec};
use lpk_libarchive_sys::{ReadFilter as Fi, ReadFormat as Fo};
use lpk_proto::frame::{Record, RecordKind, FLAG_ENCRYPTED};
use lpk_proto::reader::Sink;
use lpk_proto::worker::roles;
use lpk_proto::{ErrorCode, ForeignReader, OpenArgs, ReaderError};

pub mod rar;

/// The wire names this worker reads (the host's `Format::name` of each libarchive format).
pub const FORMATS: &[&str] = &["rar", "iso", "cab", "lzh", "ar", "cpio", "rpm", "z", "uue"];

/// The most volumes one worker takes (the bootstrap page holds 16 handles, three of them pipes).
pub const MAX_VOLUMES: usize = 13;

/// The most entries a listing holds (beyond the host's own cap; a bound on the worker's memory).
const MAX_RECORDS: usize = 1 << 24;

/// Bytes read from libarchive per call while streaming.
const CHUNK: usize = 64 * 1024;

/// How a wire format is opened.
#[derive(Debug, Clone, Copy)]
struct Plan {
    formats: &'static [Fo],
    filters: &'static [Fi],
    /// libarchive verifies a checksum the format stores (a mismatch is an error).
    checked: bool,
    /// Backslashes in names are separators (DOS formats).
    dos_names: bool,
}

fn plan(wire: &str) -> Option<Plan> {
    let p = |formats, filters, checked, dos_names| Plan {
        formats,
        filters,
        checked,
        dos_names,
    };
    Some(match wire {
        "rar" => p(&[Fo::Rar, Fo::Rar5][..], &[][..], true, true),
        "iso" => p(&[Fo::Iso9660], &[], false, false),
        "cab" => p(&[Fo::Cab], &[], true, true),
        "lzh" => p(&[Fo::Lha], &[], true, true),
        "ar" => p(&[Fo::Ar], &[], false, false),
        "cpio" => p(&[Fo::Cpio], &[], false, false),
        "rpm" => p(
            &[Fo::Cpio],
            &[Fi::Rpm, Fi::Gzip, Fi::Xz, Fi::Zstd],
            false,
            false,
        ),
        "z" => p(&[Fo::Tar, Fo::Raw], &[Fi::Compress], false, false),
        "uue" => p(
            &[Fo::Tar, Fo::Raw],
            &[Fi::Uu, Fi::Compress, Fi::Gzip, Fi::Xz, Fi::Zstd],
            false,
            false,
        ),
        _ => return None,
    })
}

/// Split the worker's handles into the pipes plus the first volume (for `lpk_proto`'s loop) and
/// the further volumes (roles `ARCHIVE + 1` …, in role order).
pub fn split_volumes(handles: Vec<(u32, File)>) -> (Vec<(u32, File)>, Vec<File>) {
    let mut base = Vec::new();
    let mut more: Vec<(u32, File)> = Vec::new();
    for (role, f) in handles {
        if role > roles::ARCHIVE {
            more.push((role, f));
        } else {
            base.push((role, f));
        }
    }
    more.sort_by_key(|(r, _)| *r);
    (base, more.into_iter().map(|(_, f)| f).collect())
}

/// Map a libarchive error to the protocol's codes, naming the gap where it is one.
fn map_err(wire: &str, e: lpk_libarchive_sys::Error) -> ReaderError {
    let lower = e.message.to_ascii_lowercase();
    match e.kind {
        LaErrorKind::NotBuilt => ReaderError::new(ErrorCode::Unsupported, e.message),
        LaErrorKind::Io => ReaderError::new(ErrorCode::Io, e.message),
        _ if lower.contains("encrypt") || lower.contains("passphrase") => ReaderError::new(
            ErrorCode::Unsupported,
            format!(
                "{wire}: encrypted archive (named gap: libarchive 3.8.9 does not decrypt it): {}",
                e.message
            ),
        ),
        _ if lower.contains("ppmd") && lower.contains("filter") => ReaderError::new(
            ErrorCode::Unsupported,
            format!(
                "{wire}: a filter inside PPMd data (named gap): {}",
                e.message
            ),
        ),
        _ if lower.contains("not supported")
            || lower.contains("unsupported")
            || lower.contains("not implemented") =>
        {
            ReaderError::new(ErrorCode::Unsupported, format!("{wire}: {}", e.message))
        }
        _ => ReaderError::corrupt(format!("{wire}: {}", e.message)),
    }
}

/// The member name of a single stream (libarchive's `raw` calls it `data`): the archive's file
/// name without its suffix.
fn stream_name(archive_name: &[u8], wire: &str) -> Vec<u8> {
    let name = String::from_utf8_lossy(archive_name).into_owned();
    let lower = name.to_ascii_lowercase();
    let suffixes: &[(&str, &str)] = match wire {
        "z" => &[(".taz", ".tar"), (".tz", ".tar"), (".z", "")],
        "uue" => &[(".uue", ""), (".uu", "")],
        _ => &[],
    };
    for (s, r) in suffixes {
        if lower.len() > s.len() && lower.ends_with(s) {
            let stem = &name[..name.len() - s.len()];
            return format!("{stem}{r}").into_bytes();
        }
    }
    if name.is_empty() {
        b"data".to_vec()
    } else {
        format!("{name}.out").into_bytes()
    }
}

/// Open the archive whose first volume is `first` (further volumes in `more`) as `args.format`.
pub fn open(
    first: File,
    more: &[File],
    args: &OpenArgs<'_>,
) -> Result<Box<dyn ForeignReader>, ReaderError> {
    let wire = FORMATS
        .iter()
        .copied()
        .find(|w| *w == args.format)
        .ok_or_else(|| {
            ReaderError::new(
                ErrorCode::Unsupported,
                format!("the libarchive worker has no reader for {:?}", args.format),
            )
        })?;
    let plan = plan(wire).ok_or_else(|| ReaderError::new(ErrorCode::Unsupported, wire))?;
    if 1 + more.len() > MAX_VOLUMES {
        return Err(ReaderError::new(
            ErrorCode::Unsupported,
            format!("more than {MAX_VOLUMES} volumes"),
        ));
    }
    let mut volumes = vec![first];
    for f in more {
        volumes.push(f.try_clone()?);
    }
    let mut total_len = 0u64;
    for v in &volumes {
        total_len = total_len.saturating_add(v.metadata()?.len());
    }
    if wire == "rar" {
        let mut f = volumes[0].try_clone()?;
        if let Some(gap) = rar::named_gap(&mut f)? {
            return Err(ReaderError::new(ErrorCode::Unsupported, gap));
        }
    }
    let mut r = LaReader {
        wire,
        plan,
        volumes,
        total_len,
        records: Vec::new(),
        ordinals: Vec::new(),
        cur: None,
        label: String::new(),
        stream_name: stream_name(args.name, wire),
    };
    r.list()?;
    Ok(Box::new(r))
}

/// The reader.
struct LaReader {
    wire: &'static str,
    plan: Plan,
    volumes: Vec<File>,
    total_len: u64,
    records: Vec<Record>,
    /// libarchive's entry ordinal of each record (entries of other types are left out).
    ordinals: Vec<u64>,
    /// The streaming pass and the ordinal of the next header it would read.
    cur: Option<(Archive, u64)>,
    label: String,
    stream_name: Vec<u8>,
}

impl std::fmt::Debug for LaReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaReader")
            .field("wire", &self.wire)
            .field("records", &self.records.len())
            .finish()
    }
}

impl LaReader {
    /// A fresh pass from the first volume's start.
    fn pass(&mut self) -> Result<Archive, ReaderError> {
        self.cur = None;
        let mut vols = Vec::with_capacity(self.volumes.len());
        for v in &self.volumes {
            let mut c = v.try_clone()?;
            c.seek(SeekFrom::Start(0))?;
            vols.push(c);
        }
        Archive::open(OpenSpec {
            volumes: vols,
            formats: self.plan.formats,
            filters: self.plan.filters,
            passphrase: None,
        })
        .map_err(|e| map_err(self.wire, e))
    }

    fn list(&mut self) -> Result<(), ReaderError> {
        let mut a = self.pass()?;
        let mut ordinal = 0u64;
        let mut buf = vec![0u8; CHUNK];
        while let Some(e) = a.next_entry().map_err(|e| map_err(self.wire, e))? {
            let this = ordinal;
            ordinal += 1;
            if e.encrypted {
                return Err(ReaderError::new(
                    ErrorCode::Unsupported,
                    format!(
                        "{}: encrypted members (named gap: libarchive 3.8.9 does not decrypt them)",
                        self.wire
                    ),
                ));
            }
            let raw = a.format_name().eq_ignore_ascii_case("raw");
            let mut name = if raw {
                self.stream_name.clone()
            } else {
                e.path.clone().ok_or_else(|| {
                    ReaderError::corrupt(format!(
                        "{}: a member name libarchive cannot convert to UTF-8",
                        self.wire
                    ))
                })?
            };
            if self.plan.dos_names {
                for b in &mut name {
                    if *b == b'\\' {
                        *b = b'/';
                    }
                }
            }
            let (kind, target) = if let Some(h) = e.hardlink.clone().filter(|h| !h.is_empty()) {
                (RecordKind::HardLink, h)
            } else {
                match e.kind {
                    EntryKind::File => (RecordKind::File, Vec::new()),
                    EntryKind::Directory => (RecordKind::Directory, Vec::new()),
                    EntryKind::Symlink => {
                        (RecordKind::Symlink, e.symlink.clone().unwrap_or_default())
                    }
                    // Devices, FIFOs and sockets are left out of the listing.
                    EntryKind::Other(_) => continue,
                }
            };
            let size = match (kind, e.size) {
                (RecordKind::File, Some(s)) => s,
                // A single stream declares no size: count it.
                (RecordKind::File, None) => {
                    let mut n = 0u64;
                    loop {
                        let k = a.read_data(&mut buf).map_err(|e| map_err(self.wire, e))?;
                        if k == 0 {
                            break;
                        }
                        n += k as u64;
                    }
                    n
                }
                _ => 0,
            };
            let mtime_ns = e
                .mtime
                .and_then(|(s, ns)| s.checked_mul(1_000_000_000)?.checked_add(ns))
                .unwrap_or(i64::MIN);
            if self.records.len() == MAX_RECORDS {
                return Err(ReaderError::new(
                    ErrorCode::Unsupported,
                    format!("more than {MAX_RECORDS} members"),
                ));
            }
            // Packed ranges: libarchive reports no packed extent, so every member with data is
            // in one block spanning the whole input (the host's overlap check is then moot for
            // these formats; its expansion cap still applies).
            let (packed_offset, packed_len, block) = if kind == RecordKind::File && size > 0 {
                (0, self.total_len, 0)
            } else {
                (0, 0, lpk_proto::frame::NO_BLOCK)
            };
            self.records.push(Record {
                kind,
                flags: if e.encrypted { FLAG_ENCRYPTED } else { 0 },
                mtime_ns,
                size,
                packed_offset,
                packed_len,
                block,
                name,
                target,
            });
            self.ordinals.push(this);
        }
        let filters = a.filter_names();
        self.label = if filters.is_empty() {
            format!(
                "{} (libarchive {})",
                a.format_name(),
                lpk_libarchive_sys::VERSION
            )
        } else {
            format!(
                "{} in {} (libarchive {})",
                a.format_name(),
                filters.join("+"),
                lpk_libarchive_sys::VERSION
            )
        };
        Ok(())
    }
}

impl ForeignReader for LaReader {
    fn format(&self) -> String {
        self.label.clone()
    }

    fn entries(&mut self, from: u64, max: usize) -> Result<(Vec<Record>, bool), ReaderError> {
        let start = usize::try_from(from)
            .ok()
            .filter(|&s| s <= self.records.len())
            .ok_or_else(|| ReaderError::new(ErrorCode::NoSuchMember, format!("no entry {from}")))?;
        let end = start.saturating_add(max).min(self.records.len());
        Ok((self.records[start..end].to_vec(), end < self.records.len()))
    }

    fn read_member(&mut self, index: u64, sink: &mut Sink<'_>) -> Result<bool, ReaderError> {
        let i = usize::try_from(index)
            .ok()
            .filter(|&i| i < self.records.len())
            .ok_or_else(|| {
                ReaderError::new(ErrorCode::NoSuchMember, format!("no entry {index}"))
            })?;
        let want = self.ordinals[i];
        let restart = match &self.cur {
            Some((_, next)) => *next > want,
            None => true,
        };
        if restart {
            let a = self.pass()?;
            self.cur = Some((a, 0));
        }
        let wire = self.wire;
        let Some((a, next)) = self.cur.as_mut() else {
            return Err(ReaderError::corrupt("no pass"));
        };
        loop {
            let e = a.next_entry().map_err(|e| map_err(wire, e))?;
            if e.is_none() {
                return Err(ReaderError::corrupt(format!(
                    "{wire}: member {index} not found on the second pass"
                )));
            }
            let this = *next;
            *next += 1;
            if this == want {
                break;
            }
        }
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match a.read_data(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    self.cur = None;
                    return Err(map_err(wire, e));
                }
            };
            if n == 0 {
                break;
            }
            sink(&buf[..n])?;
        }
        Ok(self.plan.checked)
    }
}

#[cfg(test)]
mod tests;
