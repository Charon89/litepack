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

/// The bzip2 end-of-stream magic (48 bits, bit-aligned), followed by the 32-bit stream CRC.
const BZ_EOS: u64 = 0x1772_4538_5090;
/// Bytes of look-ahead a stream-end candidate needs: the magic and CRC (10 bytes, plus one
/// for the bit offset) and the next stream's `BZh1`..`BZh9` (4 bytes).
const BZ_LOOKAHEAD: usize = 16;

/// Every bzip2 stream of a file, in order, through `bzip2-rs`'s low-level decoder (its
/// `DecoderReader` asserts on its input; this loop does not), as bzip2 and 7-Zip read
/// concatenated streams (`pbzip2` writes one stream per block).
///
/// The decoder is fed exactly one stream at a time: each stream's end is found before its
/// bytes reach the decoder (the end-of-stream magic and its CRC, bit-aligned, then byte
/// padding, then either the end of the file or the next stream's `BZh` header), so no byte
/// of the next stream sits unseen in the decoder's buffer. Anything else after a stream
/// (trailing garbage) is refused as corrupt, never dropped. Block CRCs are checked by the
/// decoder; the stream CRC is not (`bzip2-rs` 0.1.2 exposes no block CRC to combine), so a
/// `.bz2` member reports `checked = false`.
pub struct Bzip2<R> {
    dec: bzip2_rs::decoder::Decoder,
    src: R,
    /// Bytes read; `buf[pos..]` are not yet fed to the decoder (consumed bytes are dropped in
    /// bulk when the buffer is refilled, so many small streams cost no repeated moves).
    buf: Vec<u8>,
    pos: usize,
    src_eof: bool,
    /// Start offsets in `buf[pos..]` already checked for a stream end.
    scanned: usize,
    /// Where the current stream ends in `buf[pos..]`, once found.
    seg_end: Option<usize>,
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
            buf: Vec::new(),
            pos: 0,
            src_eof: false,
            // A stream end cannot start inside the 4-byte `BZh9` header.
            scanned: 4,
            seg_end: None,
            empty_writes: 0,
        }
    }

    /// The bytes not yet fed.
    fn live(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    /// Read up to 64 KiB more of the source (dropping the consumed prefix first).
    fn fill(&mut self) -> std::io::Result<()> {
        if self.src_eof {
            return Ok(());
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let old = self.buf.len();
        self.buf.resize(old + 64 * 1024, 0);
        let n = loop {
            match self.src.read(&mut self.buf[old..]) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.buf.truncate(old);
                    return Err(e);
                }
            }
        };
        self.buf.truncate(old + n);
        self.src_eof = n == 0;
        Ok(())
    }

    /// Look for the current stream's end among the unchecked offsets; stops where more
    /// look-ahead is needed. A candidate is the end-of-stream magic, its 32-bit CRC, zero
    /// padding bits to the byte boundary, then the end of the file or a `BZh1`..`BZh9` header.
    ///
    /// An end-of-stream magic followed by anything else is refused: `bzip2-rs` reads on past
    /// a stream's end into whatever follows (it would take a cut CRC and the next stream's
    /// blocks as one stream), so a magic that does not close a stream cleanly is damage. (The
    /// 48-bit magic occurs inside compressed data by chance about once per 2^48 bit positions;
    /// such an archive would be refused, never misread.)
    fn scan(&mut self) -> std::io::Result<()> {
        while self.seg_end.is_none() {
            let live = &self.buf[self.pos..];
            let i = self.scanned;
            if i >= live.len() || (!self.src_eof && live.len() < i + BZ_LOOKAHEAD) {
                return Ok(());
            }
            let mut w = 0u64;
            for k in 0..7 {
                w = (w << 8) | u64::from(live.get(i + k).copied().unwrap_or(0));
            }
            for s in 0..8 {
                if (w >> (8 - s)) & 0xFFFF_FFFF_FFFF != BZ_EOS {
                    continue;
                }
                let end_bit = i * 8 + s + 80;
                let end = end_bit.div_ceil(8);
                let pad = end * 8 - end_bit;
                let padded = live
                    .get(end - 1)
                    .is_some_and(|&last| last & ((1u8 << pad) - 1) == 0);
                let next = live.get(end..end + 4);
                let at_eof = self.src_eof && end == live.len();
                let next_stream =
                    next.is_some_and(|n| &n[..3] == b"BZh" && (b'1'..=b'9').contains(&n[3]));
                if padded && (at_eof || next_stream) {
                    self.seg_end = Some(end);
                    return Ok(());
                }
                return Err(bad(
                    "bzip2: a stream end not followed by the end of the file or another stream",
                ));
            }
            self.scanned += 1;
        }
        Ok(())
    }

    fn empty_write(&mut self) -> std::io::Result<()> {
        self.empty_writes += 1;
        if self.empty_writes > 2 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "bzip2: the stream ends early",
            ));
        }
        self.dec
            .write(&[])
            .map_err(|e| bad(format!("bzip2: {e}")))?;
        Ok(())
    }
}

impl<R: Read> Read for Bzip2<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        use bzip2_rs::decoder::{ReadState, WriteState};
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            match self.dec.read(out).map_err(|e| bad(format!("bzip2: {e}")))? {
                ReadState::Read(n) => {
                    self.empty_writes = 0;
                    return Ok(n);
                }
                ReadState::Eof => {
                    if self.seg_end != Some(0) {
                        return Err(bad("bzip2: data after the end of the stream"));
                    }
                    if self.live().is_empty() {
                        self.fill()?;
                    }
                    if self.live().is_empty() && self.src_eof {
                        return Ok(0);
                    }
                    // The next stream (its `BZh` header was seen when this one's end was).
                    self.dec = bzip2_rs::decoder::Decoder::new();
                    self.seg_end = None;
                    self.scanned = 4;
                    self.empty_writes = 0;
                }
                ReadState::NeedsWrite(space) => loop {
                    self.scan()?;
                    let len = self.live().len();
                    let limit = if self.src_eof {
                        self.seg_end.unwrap_or(len)
                    } else {
                        self.seg_end.unwrap_or(self.scanned)
                    }
                    .min(len);
                    if limit == 0 {
                        if self.seg_end == Some(0) || self.src_eof {
                            self.empty_write()?;
                            break;
                        }
                        self.fill()?;
                        continue;
                    }
                    let take = limit.min(space.max(1));
                    let chunk = &self.buf[self.pos..self.pos + take];
                    match self
                        .dec
                        .write(chunk)
                        .map_err(|e| bad(format!("bzip2: {e}")))?
                    {
                        WriteState::Written(n) => {
                            let n = n.min(take);
                            self.pos += n;
                            self.seg_end = self.seg_end.map(|e| e - n);
                            self.scanned = self.scanned.saturating_sub(n);
                            self.empty_writes = 0;
                        }
                        WriteState::NeedsRead => {}
                    }
                    break;
                },
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
            return Ok(matches!(fmt.as_str(), "gz" | "xz" | "lz"));
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

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    fn bunzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        Bzip2::new(data).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn concatenated_bzip2_streams_decode_fully() {
        let one = fixture("7zip.tar.bz2");
        let tar = fixture("7zip.tar");
        assert_eq!(bunzip(&one).unwrap(), tar);
        let mut three = one.clone();
        three.extend_from_slice(&one);
        three.extend_from_slice(&one);
        let got = bunzip(&three).unwrap();
        assert_eq!(got.len(), 3 * tar.len());
        assert!(got.chunks(tar.len()).all(|c| c == tar.as_slice()));
    }

    #[test]
    fn data_after_a_bzip2_stream_is_refused() {
        let one = fixture("7zip.tar.bz2");
        for tail in [&b"garbage!"[..], b"\0", b"BZh"] {
            let mut v = one.clone();
            v.extend_from_slice(tail);
            assert!(bunzip(&v).is_err(), "{tail:?}");
        }
        // A cut stream is refused too.
        assert!(bunzip(&one[..one.len() - 3]).is_err());
    }

    /// Small streams written by 7-Zip 26.03 (`7z a -tbzip2`), one ending at each bit offset of
    /// its last byte: (k, the stream of k copies of "litepack k ").
    const SMALL: &[(u32, &[u8])] = &[
        // 8 x "litepack 8 ", ends at bit 0 of its last byte
        (
            8,
            &[
                66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 229, 208, 7, 156, 0, 0, 32, 25, 128, 64,
                0, 0, 64, 42, 44, 68, 0, 32, 0, 49, 0, 48, 10, 168, 208, 54, 167, 69, 10, 22, 54,
                32, 131, 195, 130, 196, 31, 23, 114, 69, 56, 80, 144, 229, 208, 7, 156,
            ],
        ),
        // 2 x "litepack 2 ", ends at bit 1 of its last byte
        (
            2,
            &[
                66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 188, 73, 96, 228, 0, 0, 8, 25, 128, 64, 0,
                16, 0, 42, 44, 68, 0, 32, 0, 32, 170, 141, 3, 106, 16, 3, 14, 169, 75, 109, 17,
                231, 22, 143, 139, 185, 34, 156, 40, 72, 94, 36, 176, 114, 0,
            ],
        ),
        // 10 x "litepack 10 ", ends at bit 2 of its last byte
        (
            10,
            &[
                66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 84, 181, 65, 192, 0, 0, 45, 25, 128, 64,
                0, 96, 0, 42, 44, 68, 0, 32, 0, 32, 165, 81, 166, 134, 8, 0, 99, 228, 237, 48, 152,
                77, 38, 83, 105, 180, 253, 50, 154, 77, 167, 5, 220, 145, 78, 20, 36, 21, 45, 80,
                112, 0,
            ],
        ),
        // 16 x "litepack 16 ", ends at bit 3 of its last byte
        (
            16,
            &[
                66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 92, 107, 204, 101, 0, 0, 72, 25, 128, 64,
                0, 33, 0, 42, 44, 68, 0, 32, 0, 80, 128, 6, 2, 170, 26, 6, 60, 16, 26, 8, 9, 14, 5,
                5, 7, 195, 161, 33, 65, 98, 238, 72, 167, 10, 18, 11, 141, 121, 140, 160,
            ],
        ),
        // 12 x "litepack 12 ", ends at bit 4 of its last byte
        (
            12,
            &[
                66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 233, 77, 59, 141, 0, 0, 54, 25, 128, 64,
                0, 48, 0, 42, 44, 68, 0, 32, 0, 32, 165, 81, 160, 109, 66, 0, 24, 244, 180, 92,
                150, 139, 101, 217, 97, 97, 124, 94, 22, 203, 11, 241, 119, 36, 83, 133, 9, 14,
                148, 211, 184, 208,
            ],
        ),
        // 4 x "litepack 4 ", ends at bit 5 of its last byte
        (
            4,
            &[
                66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 80, 250, 99, 146, 0, 0, 16, 25, 128, 64,
                0, 4, 0, 42, 44, 68, 0, 32, 0, 49, 0, 48, 10, 168, 208, 54, 167, 74, 40, 179, 100,
                33, 227, 133, 144, 248, 187, 146, 41, 194, 132, 130, 135, 211, 28, 144,
            ],
        ),
        // 1 x "litepack 1 ", ends at bit 6 of its last byte
        (
            1,
            &[
                66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 192, 205, 72, 212, 0, 0, 4, 25, 128, 64,
                0, 32, 0, 42, 44, 68, 0, 32, 0, 34, 13, 3, 4, 0, 195, 128, 56, 151, 233, 76, 93,
                201, 20, 225, 66, 67, 3, 53, 35, 80,
            ],
        ),
        // 13 x "litepack 13 ", ends at bit 7 of its last byte
        (
            13,
            &[
                66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 94, 53, 129, 11, 0, 0, 58, 153, 128, 64,
                0, 40, 0, 42, 44, 68, 0, 32, 0, 49, 0, 12, 9, 170, 163, 64, 218, 158, 166, 147,
                148, 210, 109, 59, 76, 76, 79, 147, 196, 218, 98, 126, 46, 228, 138, 112, 161, 32,
                188, 107, 2, 22,
            ],
        ),
    ];

    /// An empty stream: `BZh9`, the end-of-stream magic and a zero CRC (14 bytes).
    const EMPTY: &[u8] = &[
        b'B', b'Z', b'h', b'9', 0x17, 0x72, 0x45, 0x38, 0x50, 0x90, 0, 0, 0, 0,
    ];

    fn small_content(k: u32) -> Vec<u8> {
        format!("litepack {k} ").repeat(k as usize).into_bytes()
    }

    #[test]
    fn streams_ending_at_every_bit_offset_decode_alone_and_concatenated() {
        let mut all = Vec::new();
        let mut want = Vec::new();
        for &(k, s) in SMALL {
            assert_eq!(bunzip(s).unwrap(), small_content(k), "k = {k}");
            all.extend_from_slice(s);
            want.extend(small_content(k));
        }
        assert_eq!(bunzip(&all).unwrap(), want);
        // Every pair, both orders.
        for &(a, sa) in SMALL {
            for &(b, sb) in SMALL {
                let mut v = sa.to_vec();
                v.extend_from_slice(sb);
                let mut w = small_content(a);
                w.extend(small_content(b));
                assert_eq!(bunzip(&v).unwrap(), w, "{a} then {b}");
            }
        }
    }

    #[test]
    fn empty_streams_decode_to_nothing() {
        assert_eq!(bunzip(EMPTY).unwrap(), b"");
        let (k, s) = SMALL[3];
        let mut v = EMPTY.to_vec();
        v.extend_from_slice(s);
        v.extend_from_slice(EMPTY);
        assert_eq!(bunzip(&v).unwrap(), small_content(k));
    }

    #[test]
    fn a_cut_stream_crc_is_refused() {
        for &(k, s) in SMALL {
            for cut in 1..=3 {
                let short = &s[..s.len() - cut];
                assert!(bunzip(short).is_err(), "k = {k}, cut {cut}");
                let mut v = short.to_vec();
                v.extend_from_slice(SMALL[0].1);
                assert!(bunzip(&v).is_err(), "k = {k}, cut {cut}, then a stream");
            }
        }
    }

    #[test]
    fn nonzero_padding_after_the_stream_crc_is_refused() {
        // SMALL[o] ends at bit o of its last byte, so for o in 1..8 the lowest bit is padding.
        for (o, &(k, s)) in SMALL.iter().enumerate().skip(1) {
            let mut v = s.to_vec();
            let last = v.len() - 1;
            assert_eq!(v[last] & 1, 0, "k = {k}: 7-Zip pads with zeros");
            v[last] |= 1;
            assert!(bunzip(&v).is_err(), "k = {k}, offset {o}");
        }
    }

    #[test]
    fn a_long_run_of_empty_streams_is_linear() {
        let n = 20_000;
        let v: Vec<u8> = EMPTY.repeat(n);
        let t = std::time::Instant::now();
        assert_eq!(bunzip(&v).unwrap(), b"");
        // Linear work is milliseconds even in a debug build; a quadratic one (re-scanning or
        // moving the rest of the input per stream) would take far longer.
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn single_names_drop_the_suffix() {
        assert_eq!(single_name(b"a.txt.gz"), b"a.txt");
        assert_eq!(single_name(b"x.TGZ"), b"x.tar");
        assert_eq!(single_name(b"blob"), b"blob.out");
        assert_eq!(single_name(b""), b"data");
        assert_eq!(single_name(b".gz"), b".gz.out");
    }
}
