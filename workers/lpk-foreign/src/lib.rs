//! LitePack's pure-Rust foreign-format readers (E4-3; design `docs/notes/e4-design.md` §3),
//! each behind `lpk-proto`'s [`ForeignReader`], linked by the sandboxed worker only (the host
//! calls no foreign parser: `cargo tree -e normal -i lpk-foreign` lists `lpk-worker` alone).
//!
//! * [`zip`](mod@zip): ZIP through the `zip` crate (Deflate over zlib-rs, Deflate64, LZMA, XZ,
//!   PPMd, the legacy methods, ZipCrypto and AES); bzip2 and zstd members read raw and decoded
//!   by `bzip2-rs` and `ruzstd`. Backslashes in names are separators.
//! * [`sevenz`]: 7z through `sevenz-rust2` (AES-256, PPMd, Deflate, Brotli, LZ4, LZMA/LZMA2
//!   and the filters), at the start of the file or after a self-extractor's stub; a solid block
//!   is decoded once for the members asked of it, in ascending order.
//! * [`stream`]: tar, and the single compressed streams (gzip, bzip2, xz, zstd, lzip, `.lzma`),
//!   each either a compressed tar or one compressed file — the worker decides by looking at the
//!   decompressed bytes.
//!
//! The worker selects the reader by the format the host detected (`Open { format }`); a format
//! it does not know is [`ErrorCode::Unsupported`]. No C codec is in this crate's tree (the
//! workers' `deny.toml` bans every C codec crate; the release check searches the worker's map).
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::io::{Read, Seek, SeekFrom};

use lpk_proto::reader::Sink;
use lpk_proto::{ErrorCode, ForeignReader, OpenArgs, ReaderError};

pub mod sevenz;
pub mod stream;
pub mod zip;

/// The size of the pieces handed to the sink.
pub const CHUNK: usize = 256 * 1024;

/// The reader for `args.format` over `archive` (the worker's [`lpk_proto::reader::Opener`]).
pub fn open(
    archive: std::fs::File,
    args: &OpenArgs<'_>,
) -> Result<Box<dyn ForeignReader>, ReaderError> {
    match args.format {
        "zip" => Ok(Box::new(zip::ZipReader::open(archive, args.password)?)),
        "7z" => Ok(Box::new(sevenz::SevenZReader::open(
            archive,
            args.password,
        )?)),
        "tar" | "gz" | "bz2" | "xz" | "zst" | "lz" | "lzma" => Ok(Box::new(
            stream::StreamReader::open(archive, args.format, args.name)?,
        )),
        lpk_proto::fixture::FORMAT => lpk_proto::fixture::open(archive, args),
        other => Err(ReaderError::new(
            ErrorCode::Unsupported,
            format!("no reader for the format {other:?} in this worker"),
        )),
    }
}

/// Copy everything `r` yields to `sink` in [`CHUNK`]s; the bytes copied. A read error is
/// returned as is (the caller names it).
pub(crate) fn copy_to_sink(
    r: &mut dyn Read,
    sink: &mut Sink<'_>,
    limit: u64,
) -> Result<u64, CopyError> {
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let want = usize::try_from((limit - total).min(CHUNK as u64)).unwrap_or(CHUNK);
        if want == 0 {
            return Ok(total);
        }
        let n = match r.read(&mut buf[..want]) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(CopyError::Read(e, total)),
        };
        sink(&buf[..n]).map_err(|_| CopyError::Stopped)?;
        total += n as u64;
    }
}

/// Why [`copy_to_sink`] stopped early.
#[derive(Debug)]
pub(crate) enum CopyError {
    /// The source failed (damaged data, a checksum, a wrong password) after this many bytes
    /// were handed to the sink.
    Read(std::io::Error, u64),
    /// The sink refused more.
    Stopped,
}

impl CopyError {
    /// As a reader error. A read failure of an encrypted member before its first byte is
    /// `WrongPassword` (a wrong key fails the first decoded bytes); after output began it is
    /// `Corrupt`, worded as possibly either (an authentication code or CRC checked at the end
    /// cannot tell a wrong key from damage). Unencrypted: `Corrupt`.
    pub(crate) fn into_reader(self, what: &str, encrypted: bool) -> ReaderError {
        match self {
            CopyError::Stopped => ReaderError::from(lpk_proto::Stopped),
            CopyError::Read(e, 0) if encrypted => ReaderError::new(
                ErrorCode::WrongPassword,
                format!("{what}: {e} (a wrong password, or damaged data)"),
            ),
            CopyError::Read(e, _) if encrypted => {
                ReaderError::corrupt(format!("{what}: {e} (damaged data, or a wrong password)"))
            }
            CopyError::Read(e, _) => ReaderError::corrupt(format!("{what}: {e}")),
        }
    }
}

/// Bytes of `r` read and dropped, at most `n`; the count dropped.
pub(crate) fn skip(r: &mut dyn Read, n: u64) -> std::io::Result<u64> {
    std::io::copy(&mut r.take(n), &mut std::io::sink())
}

/// Nanoseconds since the Unix epoch of a civil date and time (UTC; proleptic Gregorian).
pub(crate) fn civil_ns(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    // Howard Hinnant's days_from_civil.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(mo) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s);
    secs.saturating_mul(1_000_000_000)
}

/// A window `[base, end)` of a seekable reader, seen as a whole file (a 7z after an SFX stub).
#[derive(Debug)]
pub(crate) struct Window<R> {
    inner: R,
    base: u64,
    end: u64,
    pos: u64,
}

impl<R: Read + Seek> Window<R> {
    pub(crate) fn new(mut inner: R, base: u64) -> std::io::Result<Self> {
        let end = inner.seek(SeekFrom::End(0))?;
        inner.seek(SeekFrom::Start(base))?;
        Ok(Window {
            inner,
            base,
            end,
            pos: base,
        })
    }
}

impl<R: Read + Seek> Read for Window<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.end.saturating_sub(self.pos);
        let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
        let n = self.inner.read(&mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for Window<R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let len = self.end - self.base;
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => len.checked_add_signed(d),
            SeekFrom::Current(d) => (self.pos - self.base).checked_add_signed(d),
        }
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before 0"))?;
        self.pos = self.inner.seek(SeekFrom::Start(self.base + target))?;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_convert() {
        assert_eq!(civil_ns(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(civil_ns(2000, 3, 1, 0, 0, 1), 951_868_801_000_000_000);
        assert_eq!(civil_ns(1980, 1, 1, 0, 0, 0), 315_532_800_000_000_000);
    }

    #[test]
    fn a_window_reads_and_seeks_inside_its_bounds() {
        let mut w = Window::new(std::io::Cursor::new(b"0123456789".to_vec()), 4).unwrap();
        let mut s = String::new();
        w.read_to_string(&mut s).unwrap();
        assert_eq!(s, "456789");
        assert_eq!(w.seek(SeekFrom::End(-2)).unwrap(), 4);
        let mut b = [0u8; 2];
        w.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"89");
        assert!(w.seek(SeekFrom::Current(-100)).is_err());
    }

    #[test]
    fn an_unknown_format_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x");
        std::fs::write(&p, b"x").unwrap();
        let args = OpenArgs {
            format: "arj",
            name: b"",
            password: None,
        };
        let e = open(std::fs::File::open(&p).unwrap(), &args).err().unwrap();
        assert_eq!(e.code, ErrorCode::Unsupported);
    }
}
