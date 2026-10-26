//! tar, and the single compressed streams: gzip (`flate2` over zlib-rs, every member of a
//! multi-member file), bzip2 (`bzip2-rs`), xz, lzip and `.lzma` (`lzma-rust2`), zstd (`ruzstd`,
//! every frame, skippable frames skipped, content checksums verified).
//!
//! A compressed stream is a compressed tar when its first decompressed 512 bytes are a tar
//! header (`ustar`, or a v7 header whose checksum holds); then its members are listed by one
//! pass over the decompressed stream and streamed by another, kept open across `ReadMember`s
//! (ascending indices never restart it). Otherwise it is one file, named from the archive's
//! name without its compression suffix, whose size is learnt by decompressing it at `Open`.
//!
//! Packed ranges: a plain tar's members run from their header to the end of their data; a
//! compressed stream is one block covering the whole file (its offsets are in the
//! decompressed stream, not in the archive).

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use lpk_proto::frame::{Record, RecordKind, NO_BLOCK};
use lpk_proto::reader::Sink;
use lpk_proto::{ErrorCode, ForeignReader, ReaderError};

use crate::{copy_to_sink, skip};

fn bad(m: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, m.into())
}

// ---------------------------------------------------------------------------------------------
// bzip2

/// A bzip2 stream through `bzip2-rs`'s low-level decoder (its `DecoderReader` asserts on its
/// input; this loop does not). Block CRCs are checked by the decoder; the stream CRC is not
/// (`bzip2-rs` 0.1.2). Data after the end of the stream (concatenated streams, as `pbzip2`
/// writes) is refused as a named gap rather than dropped.
pub struct Bzip2<R> {
    dec: bzip2_rs::decoder::Decoder,
    src: R,
    pending: Vec<u8>,
    at: usize,
    src_eof: bool,
    empty_writes: u32,
}

impl<R> std::fmt::Debug for Bzip2<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bzip2").finish_non_exhaustive()
    }
}

impl<R: Read> Bzip2<R> {
    /// A decoder over `src`.
    pub fn new(src: R) -> Self {
        Bzip2 {
            dec: bzip2_rs::decoder::Decoder::new(),
            src,
            pending: Vec::new(),
            at: 0,
            src_eof: false,
            empty_writes: 0,
        }
    }
}

impl<R: Read> Read for Bzip2<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use bzip2_rs::decoder::{ReadState, WriteState};
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.dec.read(buf).map_err(|e| bad(format!("bzip2: {e}")))? {
                ReadState::Read(n) => {
                    self.empty_writes = 0;
                    return Ok(n);
                }
                ReadState::Eof => {
                    let mut probe = [0u8; 1];
                    let more = self.at < self.pending.len()
                        || (!self.src_eof && self.src.read(&mut probe)? > 0);
                    if more {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Unsupported,
                            "data after the bzip2 stream (concatenated bzip2 streams are not read)",
                        ));
                    }
                    return Ok(0);
                }
                ReadState::NeedsWrite(space) => {
                    if self.at == self.pending.len() && !self.src_eof {
                        self.pending.resize(64 * 1024, 0);
                        let n = self.src.read(&mut self.pending)?;
                        self.pending.truncate(n);
                        self.at = 0;
                        self.src_eof = n == 0;
                    }
                    let end = self.pending.len().min(self.at.saturating_add(space.max(1)));
                    let chunk = &self.pending[self.at..end];
                    if chunk.is_empty() {
                        self.empty_writes += 1;
                        if self.empty_writes > 2 {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "bzip2: the stream ends early",
                            ));
                        }
                    } else {
                        self.empty_writes = 0;
                    }
                    match self
                        .dec
                        .write(chunk)
                        .map_err(|e| bad(format!("bzip2: {e}")))?
                    {
                        WriteState::Written(n) => self.at += n.min(chunk.len()),
                        WriteState::NeedsRead => {}
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// zstd

/// Every zstd frame of a stream through `ruzstd`, skippable frames skipped, each frame's
/// content checksum (when it has one) compared with the decoded bytes'.
pub struct Zstd<R> {
    src: R,
    dec: ruzstd::decoding::FrameDecoder,
    active: bool,
}

impl<R> std::fmt::Debug for Zstd<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Zstd").finish_non_exhaustive()
    }
}

impl<R: BufRead> Zstd<R> {
    /// A decoder over `src`.
    pub fn new(src: R) -> Self {
        Zstd {
            src,
            dec: ruzstd::decoding::FrameDecoder::new(),
            active: false,
        }
    }
}

impl<R: BufRead> Read for Zstd<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.active {
                if self.src.fill_buf()?.is_empty() {
                    return Ok(0);
                }
                match self.dec.init(&mut self.src) {
                    Ok(()) => self.active = true,
                    Err(FrameDecoderError::ReadFrameHeaderError(
                        ReadFrameHeaderError::SkipFrame { length, .. },
                    )) => {
                        let n = skip(&mut self.src, u64::from(length))?;
                        if n != u64::from(length) {
                            return Err(bad("zstd: a skippable frame ends early"));
                        }
                    }
                    Err(e) => return Err(bad(format!("zstd: {e}"))),
                }
                continue;
            }
            if self.dec.can_collect() > 0 {
                return self.dec.read(buf);
            }
            if self.dec.is_finished() {
                if let (Some(stored), Some(computed)) = (
                    self.dec.get_checksum_from_data(),
                    self.dec.get_calculated_checksum(),
                ) {
                    if stored != computed {
                        return Err(bad("zstd: content checksum mismatch"));
                    }
                }
                self.active = false;
                continue;
            }
            self.dec
                .decode_blocks(
                    &mut self.src,
                    ruzstd::decoding::BlockDecodingStrategy::UptoBlocks(1),
                )
                .map_err(|e| bad(format!("zstd: {e}")))?;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// tar and single streams

/// A tar header in `h` (at least 512 bytes): `ustar` at 257, or a v7 header whose checksum holds.
pub fn tar_header(h: &[u8]) -> bool {
    let Some(h) = h.get(..512) else {
        return false;
    };
    if &h[257..262] == b"ustar" {
        return true;
    }
    if h[0] == 0 {
        return false;
    }
    let digits: Vec<u8> = h[148..156]
        .iter()
        .copied()
        .skip_while(|&b| b == b' ')
        .take_while(|b| (b'0'..=b'7').contains(b))
        .collect();
    if digits.is_empty() || digits.len() > 7 {
        return false;
    }
    let stored = digits
        .iter()
        .fold(0u32, |a, &d| a * 8 + u32::from(d - b'0'));
    let sum: u32 = h
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u32::from(b)
            }
        })
        .sum();
    stored == sum
}

/// A tar member's data in the (decompressed) stream.
#[derive(Debug, Clone, Copy)]
struct Span {
    at: u64,
    size: u64,
}

/// tar or a single compressed stream, open in the worker.
pub struct StreamReader {
    file: File,
    format: String,
    tar: bool,
    records: Vec<Record>,
    spans: Vec<Span>,
    /// The decompressed stream being read, and its position.
    cur: Option<(Box<dyn Read>, u64)>,
}

impl std::fmt::Debug for StreamReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamReader")
            .field("format", &self.format)
            .field("tar", &self.tar)
            .finish_non_exhaustive()
    }
}

/// The single member's name: the archive's name without its compression suffix (`.tgz` and
/// the like become `.tar`); `data` when that leaves nothing.
fn single_name(archive: &[u8]) -> Vec<u8> {
    let name = String::from_utf8_lossy(archive).into_owned();
    let lower = name.to_ascii_lowercase();
    for (suffix, to) in [
        (".tgz", ".tar"),
        (".tbz2", ".tar"),
        (".tbz", ".tar"),
        (".txz", ".tar"),
        (".tzst", ".tar"),
        (".tlz", ".tar"),
        (".gz", ""),
        (".bz2", ""),
        (".xz", ""),
        (".zst", ""),
        (".lz", ""),
        (".lzma", ""),
    ] {
        if lower.ends_with(suffix) && name.len() > suffix.len() {
            return format!("{}{to}", &name[..name.len() - suffix.len()]).into_bytes();
        }
    }
    if name.is_empty() {
        b"data".to_vec()
    } else {
        format!("{name}.out").into_bytes()
    }
}

/// Read until `buf` is full or the stream ends; the count read.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(m) => n += m,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn corrupt(format: &str, e: std::io::Error) -> ReaderError {
    if e.kind() == std::io::ErrorKind::Unsupported {
        ReaderError::new(ErrorCode::Unsupported, format!("{format}: {e}"))
    } else {
        ReaderError::corrupt(format!("{format}: {e}"))
    }
}

impl StreamReader {
    /// Open `archive` as `format` (`tar`, `gz`, `bz2`, `xz`, `zst`, `lz`, `lzma`); `name` is the
    /// archive's file name.
    pub fn open(archive: File, format: &str, name: &[u8]) -> Result<Self, ReaderError> {
        let archive_len = archive.metadata()?.len();
        let mut r = StreamReader {
            file: archive,
            format: format.to_string(),
            tar: format == "tar",
            records: Vec::new(),
            spans: Vec::new(),
            cur: None,
        };
        if !r.tar {
            let mut d = r.decoder()?;
            let mut head = [0u8; 512];
            let n = read_full(&mut d, &mut head).map_err(|e| corrupt(format, e))?;
            r.tar = n == 512 && tar_header(&head);
        }
        if r.tar {
            r.list_tar(archive_len)?;
        } else {
            let mut d = r.decoder()?;
            let size = skip(&mut d, u64::MAX).map_err(|e| corrupt(format, e))?;
            r.records.push(Record {
                kind: RecordKind::File,
                flags: 0,
                mtime_ns: i64::MIN,
                size,
                packed_offset: 0,
                packed_len: archive_len,
                block: 0,
                name: single_name(name),
                target: Vec::new(),
            });
            r.spans.push(Span { at: 0, size });
        }
        Ok(r)
    }

    /// A fresh decompressed stream from the start of the archive.
    fn decoder(&self) -> Result<Box<dyn Read>, ReaderError> {
        let mut f = self.file.try_clone()?;
        f.seek(SeekFrom::Start(0))?;
        let b = BufReader::with_capacity(64 * 1024, f);
        Ok(match self.format.as_str() {
            "tar" => Box::new(b),
            "gz" => Box::new(flate2::bufread::MultiGzDecoder::new(b)),
            "bz2" => Box::new(Bzip2::new(b)),
            "xz" => Box::new(lzma_rust2::XzReader::new(b, true)),
            "zst" => Box::new(Zstd::new(b)),
            "lz" => Box::new(lzma_rust2::LzipReader::new(b)),
            "lzma" => Box::new(
                lzma_rust2::LzmaReader::new_mem_limit(b, u32::MAX, None)
                    .map_err(|e| corrupt("lzma", e))?,
            ),
            other => {
                return Err(ReaderError::new(
                    ErrorCode::Unsupported,
                    format!("no stream reader for {other:?}"),
                ))
            }
        })
    }

    fn list_tar(&mut self, archive_len: u64) -> Result<(), ReaderError> {
        let plain = self.format == "tar";
        let fmt = self.format.clone();
        let mut ar = tar::Archive::new(self.decoder()?);
        let entries = ar.entries().map_err(|e| corrupt(&fmt, e))?;
        for e in entries {
            let e = e.map_err(|e| corrupt(&fmt, e))?;
            let h = e.header();
            let kind = match h.entry_type() {
                tar::EntryType::Regular | tar::EntryType::Continuous => RecordKind::File,
                tar::EntryType::Directory => RecordKind::Directory,
                tar::EntryType::Symlink => RecordKind::Symlink,
                tar::EntryType::Link => RecordKind::HardLink,
                tar::EntryType::GNUSparse => {
                    return Err(ReaderError::new(
                        ErrorCode::Unsupported,
                        "tar: GNU sparse members are not read",
                    ))
                }
                // Devices, FIFOs and unknown kinds are not extracted.
                _ => continue,
            };
            let mtime_ns = h
                .mtime()
                .ok()
                .and_then(|s| i64::try_from(s).ok())
                .map_or(i64::MIN, |s| s.saturating_mul(1_000_000_000));
            let mut name = e.path_bytes().into_owned();
            while name.last() == Some(&b'/') {
                name.pop();
            }
            let target = match kind {
                RecordKind::Symlink | RecordKind::HardLink => e
                    .link_name_bytes()
                    .map(|c| c.into_owned())
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            let size = if kind == RecordKind::File {
                e.size()
            } else {
                0
            };
            let (at, header) = (e.raw_file_position(), e.raw_header_position());
            let (packed_offset, packed_len, block) = if plain {
                let end = at
                    .checked_add(e.size())
                    .ok_or_else(|| ReaderError::corrupt("tar: a member's size overflows"))?;
                (header, end.saturating_sub(header), NO_BLOCK)
            } else {
                (0, archive_len, 0)
            };
            self.records.push(Record {
                kind,
                flags: 0,
                mtime_ns,
                size,
                packed_offset,
                packed_len,
                block,
                name,
                target,
            });
            self.spans.push(Span { at, size });
        }
        Ok(())
    }
}

impl ForeignReader for StreamReader {
    fn format(&self) -> String {
        if self.tar && self.format != "tar" {
            format!("tar.{}", self.format)
        } else {
            self.format.clone()
        }
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
        let span = *self
            .spans
            .get(i)
            .ok_or_else(|| ReaderError::new(ErrorCode::NoSuchMember, "no such member"))?;
        let what = String::from_utf8_lossy(&self.records[i].name).into_owned();
        let fmt = self.format.clone();
        if !self.tar {
            // One file: decoded whole, so the stream's own checks run to its end.
            let mut d = self.decoder()?;
            let n = copy_to_sink(&mut d, sink, span.size.saturating_add(1))
                .map_err(|e| e.into_reader(&what, false))?;
            if n != span.size {
                return Err(ReaderError::corrupt(format!(
                    "{what}: {n} bytes decoded where {} were listed",
                    span.size
                )));
            }
            return Ok(matches!(fmt.as_str(), "gz" | "bz2" | "xz" | "lz"));
        }
        let restart = match &self.cur {
            Some((_, pos)) => *pos > span.at,
            None => true,
        };
        if restart {
            self.cur = Some((self.decoder()?, 0));
        }
        let Some((d, pos)) = self.cur.as_mut() else {
            return Err(ReaderError::corrupt("tar: no stream"));
        };
        let gap = span.at - *pos;
        let skipped = skip(d, gap).map_err(|e| corrupt(&fmt, e))?;
        *pos += skipped;
        if skipped != gap {
            self.cur = None;
            return Err(ReaderError::corrupt(format!(
                "{what}: the stream ends early"
            )));
        }
        let r = copy_to_sink(d, sink, span.size);
        match r {
            Ok(n) => {
                *pos += n;
                if n != span.size {
                    self.cur = None;
                    return Err(ReaderError::corrupt(format!(
                        "{what}: the stream ends inside the member"
                    )));
                }
                Ok(false)
            }
            Err(e) => {
                self.cur = None;
                Err(e.into_reader(&what, false))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_names_drop_the_suffix() {
        assert_eq!(single_name(b"a.txt.gz"), b"a.txt");
        assert_eq!(single_name(b"x.TGZ"), b"x.tar");
        assert_eq!(single_name(b"blob"), b"blob.out");
        assert_eq!(single_name(b""), b"data");
        assert_eq!(single_name(b".gz"), b".gz.out");
    }
}
