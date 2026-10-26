//! `Session::export` (`lpk export`): the ZIP and 7z sinks (E3-6) the engine's entry-ordered export
//! ([`lpk_core::export_file`]) writes into, in process (they parse nothing untrusted: the
//! entries come from the archive the reader already validated).
//!
//! **ZIP** (the `zip` crate): one entry per archive entry, in entry order, Zip64 for an entry
//! of 4 GiB or more (the sizes are known before the entry is written, so the Zip64 extra field
//! is written only where needed) and for the central directory when it needs it; names are
//! the archive's UTF-8 paths (the UTF-8 flag set); the modification time is written as the DOS
//! time (two-second resolution, 1980 to 2107, else 1980-01-01) and as the extended-timestamp
//! extra field (0x5455, to the second, when it fits a 32-bit Unix time); the host system is
//! Unix with the mode in the external attributes (directories 0o40755, files 0o644 or 0o755
//! for the executable flag, symlinks 0o120777) and the DOS read-only, hidden and system bits
//! from the entry flags; a symlink entry is a ZIP symlink (mode `S_IFLNK`, the target as its
//! stored content: what Info-ZIP and 7-Zip write); an empty file is an empty entry. Methods:
//! Deflate (the default; `flate2`'s zlib-rs backend), zstd (method 93), store; AES-256 (AE-2,
//! the `zip` crate's `aes-crypto`) with the archive's password under `--encrypt`.
//!
//! **7z** (`sevenz-rust2`): one entry per archive entry, each in its own pack stream (not
//! solid: the writer streams entry by entry; an empty file is a stream-less entry, the
//! format's `EmptyFile` property); LZMA2 (the default), Deflate or copy (zstd inside 7z is
//! the 7-Zip-ZS fork's extension, which 7-Zip 26.03 does not read: refused for 7z); the
//! modification time as an NT time; Windows attributes with the Unix mode in the high half
//! (`0x8000` set, as 7-Zip writes on Unix), the directory bit, and the read-only, hidden and
//! system bits; a symlink entry is a file whose mode says `S_IFLNK` and whose content is the
//! target; AES-256-SHA256 with the header encrypted under `--encrypt` (the password as a
//! string: a password that is not UTF-8 is refused for 7z).
//!
//! Names are written as the archive stores them (the format's reader already refuses absolute
//! paths and `..`; a colon, a device name, a trailing dot or space go through for the
//! consumer's extractor to sanitise). Each writer's own memory is outside the engine's count:
//! its directory, kept until the end (ZIP's central directory, about 200 bytes plus the name
//! per entry; 7z's entry table, about 100 plus the name), and its compressor (Deflate under
//! 1 MiB, zstd a few MiB, LZMA2 about eleven times its 16 MiB dictionary). The output is
//! opened at the first entry; a failure removes it, a panic leaves it in place.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lpk_core::{CoreError, Sink};
use lpk_format::{Entry, EntryFlags, EntryKind};
use zip::write::{ExtendedFileOptions, FileOptions};
use zip::{AesMode, CompressionMethod, DateTime, System, ZipWriter};

/// The container `lpk export` writes, by the output's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Zip,
    SevenZ,
}

impl Container {
    /// The container of an output path by its extension (`.zip`, `.7z`, case-insensitive).
    pub fn of(out: &Path) -> Option<Container> {
        let ext = out.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "zip" => Some(Container::Zip),
            "7z" => Some(Container::SevenZ),
            _ => None,
        }
    }

    /// The container's name.
    pub fn name(self) -> &'static str {
        match self {
            Container::Zip => "ZIP",
            Container::SevenZ => "7z",
        }
    }

    /// The method used when `--method` is not given.
    pub fn default_method(self) -> Method {
        match self {
            Container::Zip => Method::Deflate,
            Container::SevenZ => Method::Lzma2,
        }
    }
}

/// The compression method of the entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Deflate,
    Zstd,
    Store,
    /// 7z only.
    Lzma2,
}

impl Method {
    /// The value `--method` takes.
    pub fn name(self) -> &'static str {
        match self {
            Method::Deflate => "deflate",
            Method::Zstd => "zstd",
            Method::Store => "store",
            Method::Lzma2 => "lzma2",
        }
    }
}

/// A sink that is closed when the export is done.
pub trait Writer: Sink {
    /// Write the container's trailer and close it.
    fn finish(self: Box<Self>) -> Result<(), CoreError>;
    /// True once the output file was created (or truncated): the writer opens it at the
    /// first entry, so a refusal before that leaves whatever was at the path untouched.
    fn created(&self) -> bool;
}

/// The settings of a writer.
#[derive(Debug, Clone, Copy)]
pub struct Settings<'k> {
    pub method: Method,
    /// The password every entry is encrypted with (`--encrypt`).
    pub password: Option<&'k [u8]>,
}

/// How [`crate::Session::export`] writes.
#[derive(Debug, Clone, Copy)]
pub struct ExportOptions<'k> {
    /// ZIP or 7z.
    pub container: Container,
    /// The entries' compression method.
    pub method: Method,
    /// Encrypt every entry with this password (AES-256).
    pub password: Option<&'k [u8]>,
    /// Replace an existing output.
    pub force: bool,
    /// The thread budget.
    pub threads: Option<usize>,
    /// The memory budget.
    pub memory: Option<u64>,
}

/// What creates the output file, called at the first entry.
pub type Opener = Box<dyn FnOnce() -> std::io::Result<File>>;

/// A writer of `container` over the file `opener` creates at `path` (which the errors name).
pub fn open<'k>(
    container: Container,
    opener: Opener,
    path: &Path,
    settings: Settings<'k>,
) -> Box<dyn Writer + 'k> {
    match container {
        Container::Zip => Box::new(ZipSink::new(opener, path, settings)),
        Container::SevenZ => Box::new(SevenZSink::new(opener, path, settings)),
    }
}

/// About what the writer keeps for its directory until the end, outside the engine's
/// count: the ZIP central directory (the `zip` crate's per-entry record, about 200 bytes,
/// plus the name) or the 7z entry table (about 100 bytes plus the name per entry).
pub fn directory_estimate(container: Container, entries: u64, name_bytes: u64) -> u64 {
    let per = match container {
        Container::Zip => 200,
        Container::SevenZ => 100,
    };
    entries.saturating_mul(per).saturating_add(name_bytes)
}

/// About what the compressor of `method` holds while an entry is written (also outside
/// the engine's count).
pub fn compressor_memory(container: Container, method: Method) -> &'static str {
    match (container, method) {
        (_, Method::Store) => "none",
        (_, Method::Deflate) => "under 1 MiB",
        (Container::Zip, Method::Zstd) => "a few MiB (zstd level 3)",
        (Container::SevenZ, Method::Zstd) => "a few MiB",
        (_, Method::Lzma2) => "about 11 times the LZMA2 dictionary (16 MiB at this level)",
    }
}

fn export_error(path: &Path, entry: &str, e: impl std::fmt::Display) -> CoreError {
    CoreError::Export {
        path: path.to_path_buf(),
        entry: entry.to_string(),
        message: e.to_string(),
    }
}

/// The Unix mode of an entry: the kind in the type bits, the executable flag in the mode.
fn unix_mode(e: &Entry) -> u32 {
    match e.kind {
        EntryKind::Directory => 0o040_755,
        EntryKind::Symlink => 0o120_777,
        EntryKind::File => {
            if e.flags.contains(EntryFlags::EXECUTABLE) {
                0o100_755
            } else {
                0o100_644
            }
        }
    }
}

/// The DOS attribute bits of an entry's flags (read-only, hidden, system) and kind.
fn dos_attributes(e: &Entry) -> u32 {
    let mut a = 0;
    if e.flags.contains(EntryFlags::READ_ONLY) {
        a |= 0x01;
    }
    if e.flags.contains(EntryFlags::HIDDEN) {
        a |= 0x02;
    }
    if e.flags.contains(EntryFlags::SYSTEM) {
        a |= 0x04;
    }
    if e.kind == EntryKind::Directory {
        a |= 0x10;
    }
    a
}

/// The entry's modification time as seconds since the Unix epoch (`None` when unknown).
fn mtime_secs(e: &Entry) -> Option<i64> {
    if e.mtime_ns == i64::MIN {
        return None;
    }
    Some(e.mtime_ns.div_euclid(1_000_000_000))
}

/// The civil date and time of `secs` since the Unix epoch (proleptic Gregorian, UTC):
/// (year, month, day, hour, minute, second).
pub fn civil(secs: i64) -> (i64, u8, u8, u8, u8, u8) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m as u8,
        d as u8,
        (rem / 3600) as u8,
        ((rem % 3600) / 60) as u8,
        (rem % 60) as u8,
    )
}

/// The DOS time of `secs` (1980 to 2107, two-second resolution), else 1980-01-01.
fn dos_time(secs: Option<i64>) -> DateTime {
    let Some(secs) = secs else {
        return DateTime::default();
    };
    let (y, mo, d, h, mi, s) = civil(secs);
    u16::try_from(y)
        .ok()
        .and_then(|y| DateTime::from_date_and_time(y, mo, d, h, mi, s).ok())
        .unwrap_or_default()
}

/// The extended-timestamp extra field (0x5455) with the modification time, when it fits.
fn extended_timestamp(secs: Option<i64>) -> Option<[u8; 5]> {
    let t = u32::try_from(secs?).ok()?;
    let mut f = [1u8; 5];
    f[1..].copy_from_slice(&t.to_le_bytes());
    Some(f)
}

/// Zip64 for an entry whose stored form may reach 4 GiB: the plain size with a margin for
/// what AES (the salt, the verifier and the authentication code) or an incompressible
/// Deflate stream (five bytes per 64 KiB block) adds, so an entry just under the limit is
/// not refused by the writer once it has grown.
fn needs_zip64(size: u64) -> bool {
    size.saturating_add(size / 1000 + 64) >= u64::from(u32::MAX)
}

/// The output file: opened at the first entry.
enum Output<W> {
    Unopened(Option<Opener>),
    Open(W),
    Closed,
}

struct ZipSink<'k> {
    w: Output<ZipWriter<File>>,
    path: PathBuf,
    settings: Settings<'k>,
}

impl<'k> ZipSink<'k> {
    fn new(opener: Opener, path: &Path, settings: Settings<'k>) -> Self {
        ZipSink {
            w: Output::Unopened(Some(opener)),
            path: path.to_path_buf(),
            settings,
        }
    }

    fn writer(&mut self) -> Result<&mut ZipWriter<File>, CoreError> {
        if let Output::Unopened(opener) = &mut self.w {
            let opener = opener
                .take()
                .ok_or_else(|| export_error(&self.path, "", "the writer is closed"))?;
            let file = opener().map_err(|e| export_error(&self.path, "", e))?;
            self.w = Output::Open(ZipWriter::new(file));
        }
        match &mut self.w {
            Output::Open(w) => Ok(w),
            _ => Err(export_error(&self.path, "", "the writer is closed")),
        }
    }

    /// The options of an entry: method, time, attributes, Zip64 when the size needs it,
    /// AES when asked.
    fn options(
        &self,
        e: &Entry,
    ) -> Result<FileOptions<'k, 'static, ExtendedFileOptions>, CoreError> {
        let (method, level) = match self.settings.method {
            Method::Deflate => (CompressionMethod::Deflated, None),
            Method::Zstd => (CompressionMethod::Zstd, None),
            Method::Store => (CompressionMethod::Stored, None),
            Method::Lzma2 => {
                return Err(export_error(
                    &self.path,
                    &e.path,
                    "lzma2 is not a ZIP method",
                ))
            }
        };
        // A symlink's target is its content, stored (the crate does the same).
        let method = if e.kind == EntryKind::Symlink {
            CompressionMethod::Stored
        } else {
            method
        };
        let secs = mtime_secs(e);
        let mut o = FileOptions::<ExtendedFileOptions>::default()
            .compression_method(method)
            .compression_level(level)
            .last_modified_time(dos_time(secs))
            .system(System::Unix)
            .external_attributes((unix_mode(e) << 16) | dos_attributes(e))
            .large_file(needs_zip64(e.size));
        if let Some(f) = extended_timestamp(secs) {
            o.add_extra_field(0x5455, f, false)
                .map_err(|err| export_error(&self.path, &e.path, err))?;
        }
        if let Some(pw) = self.settings.password {
            o = o.with_aes_encryption_bytes(AesMode::Aes256, pw);
        }
        Ok(o)
    }
}

impl Sink for ZipSink<'_> {
    fn directory(&mut self, e: &Entry) -> Result<(), CoreError> {
        let o = self.options(e)?;
        let path = self.path.clone();
        self.writer()?
            .add_directory(&e.path, o)
            .map_err(|err| export_error(&path, &e.path, err))
    }

    fn symlink(&mut self, e: &Entry) -> Result<(), CoreError> {
        let o = self.options(e)?;
        let path = self.path.clone();
        let target = e.symlink_target.as_deref().unwrap_or_default();
        let target = std::str::from_utf8(target)
            .map_err(|_| export_error(&path, &e.path, "the symlink target is not UTF-8"))?;
        self.writer()?
            .add_symlink(&e.path, target, o)
            .map_err(|err| export_error(&path, &e.path, err))
    }

    fn file(&mut self, e: &Entry, content: &mut dyn Read) -> Result<(), CoreError> {
        let o = self.options(e)?;
        let path = self.path.clone();
        let w = self.writer()?;
        w.start_file(&e.path, o)
            .map_err(|err| export_error(&path, &e.path, err))?;
        std::io::copy(content, w).map_err(|err| export_error(&path, &e.path, err))?;
        Ok(())
    }
}

impl Writer for ZipSink<'_> {
    fn finish(mut self: Box<Self>) -> Result<(), CoreError> {
        // An archive without entries is still a (empty) ZIP.
        self.writer()?;
        let w = match std::mem::replace(&mut self.w, Output::Closed) {
            Output::Open(w) => w,
            _ => return Err(export_error(&self.path, "", "the writer is closed")),
        };
        let file = w.finish().map_err(|e| export_error(&self.path, "", e))?;
        file.sync_all().map_err(|e| export_error(&self.path, "", e))
    }

    fn created(&self) -> bool {
        !matches!(self.w, Output::Unopened(_))
    }
}

struct SevenZSink {
    w: Output<sevenz_rust2::ArchiveWriter<File>>,
    path: PathBuf,
    methods: Vec<sevenz_rust2::EncoderConfiguration>,
    /// A password that is not UTF-8: refused at the first entry (7z takes a string).
    bad_password: bool,
}

impl SevenZSink {
    fn new(opener: Opener, path: &Path, settings: Settings<'_>) -> Self {
        use sevenz_rust2::{
            encoder_options::AesEncoderOptions, EncoderConfiguration, EncoderMethod,
        };
        let method = match settings.method {
            Method::Deflate => EncoderMethod::DEFLATE,
            Method::Store => EncoderMethod::COPY,
            // zstd inside 7z is refused by the command line; LZMA2 stands in.
            Method::Lzma2 | Method::Zstd => EncoderMethod::LZMA2,
        };
        let mut methods = Vec::new();
        let mut bad_password = false;
        if let Some(pw) = settings.password {
            match std::str::from_utf8(pw) {
                Ok(pw) => {
                    methods.push(AesEncoderOptions::new(sevenz_rust2::Password::new(pw)).into())
                }
                Err(_) => bad_password = true,
            }
        }
        methods.push(EncoderConfiguration::new(method));
        SevenZSink {
            w: Output::Unopened(Some(opener)),
            path: path.to_path_buf(),
            methods,
            bad_password,
        }
    }

    fn writer(&mut self) -> Result<&mut sevenz_rust2::ArchiveWriter<File>, CoreError> {
        if self.bad_password {
            return Err(export_error(
                &self.path,
                "",
                "7z takes a UTF-8 password; the archive's is not",
            ));
        }
        if let Output::Unopened(opener) = &mut self.w {
            let opener = opener
                .take()
                .ok_or_else(|| export_error(&self.path, "", "the writer is closed"))?;
            let file = opener().map_err(|e| export_error(&self.path, "", e))?;
            let mut w = sevenz_rust2::ArchiveWriter::new(file)
                .map_err(|e| export_error(&self.path, "", e))?;
            w.set_content_methods(std::mem::take(&mut self.methods));
            self.w = Output::Open(w);
        }
        match &mut self.w {
            Output::Open(w) => Ok(w),
            _ => Err(export_error(&self.path, "", "the writer is closed")),
        }
    }

    fn entry(&self, e: &Entry) -> sevenz_rust2::ArchiveEntry {
        let mut a = if e.kind == EntryKind::Directory {
            sevenz_rust2::ArchiveEntry::new_directory(&e.path)
        } else {
            sevenz_rust2::ArchiveEntry::new_file(&e.path)
        };
        if let Some(t) = mtime(e.mtime_ns).and_then(|t| sevenz_rust2::NtTime::try_from(t).ok()) {
            a.last_modified_date = t;
            a.has_last_modified_date = true;
        }
        a.windows_attributes = dos_attributes(e) | 0x8000 | (unix_mode(e) << 16);
        a.has_windows_attributes = true;
        a
    }

    fn push(&mut self, e: &Entry, content: Option<&mut dyn Read>) -> Result<(), CoreError> {
        let entry = self.entry(e);
        let path = self.path.clone();
        let w = self.writer()?;
        w.push_archive_entry(entry, content)
            .map(|_| ())
            .map_err(|err| export_error(&path, &e.path, err))
    }
}

/// The entry's modification time as a system time (`None` when unknown or out of range).
fn mtime(ns: i64) -> Option<SystemTime> {
    if ns == i64::MIN {
        return None;
    }
    let d = Duration::from_nanos(ns.unsigned_abs());
    if ns >= 0 {
        UNIX_EPOCH.checked_add(d)
    } else {
        UNIX_EPOCH.checked_sub(d)
    }
}

impl Sink for SevenZSink {
    fn directory(&mut self, e: &Entry) -> Result<(), CoreError> {
        self.push(e, None)
    }

    fn symlink(&mut self, e: &Entry) -> Result<(), CoreError> {
        let target = e.symlink_target.clone().unwrap_or_default();
        if target.is_empty() {
            return self.push(e, None);
        }
        let mut c = Cursor::new(target);
        self.push(e, Some(&mut c))
    }

    fn file(&mut self, e: &Entry, content: &mut dyn Read) -> Result<(), CoreError> {
        // An empty file is a stream-less entry (the format's `EmptyFile` property): a zero
        // byte stream through the copy method is a pack stream of no bytes, which 7-Zip
        // refuses to open ("Headers Error"); the engine's reader for it yields nothing.
        if e.size == 0 {
            return self.push(e, None);
        }
        self.push(e, Some(content))
    }
}

impl Writer for SevenZSink {
    fn finish(mut self: Box<Self>) -> Result<(), CoreError> {
        self.writer()?;
        let w = match std::mem::replace(&mut self.w, Output::Closed) {
            Output::Open(w) => w,
            _ => return Err(export_error(&self.path, "", "the writer is closed")),
        };
        let file = w.finish().map_err(|e| export_error(&self.path, "", e))?;
        file.sync_all().map_err(|e| export_error(&self.path, "", e))
    }

    fn created(&self) -> bool {
        !matches!(self.w, Output::Unopened(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_instants() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(civil(1_700_000_000), (2023, 11, 14, 22, 13, 20));
        assert_eq!(civil(-1), (1969, 12, 31, 23, 59, 59));
        assert_eq!(civil(4_102_444_800), (2100, 1, 1, 0, 0, 0));
    }

    #[test]
    fn dos_time_covers_1980_to_2107_and_falls_back_outside() {
        let t = dos_time(Some(1_700_000_000));
        assert_eq!((t.year(), t.month(), t.day()), (2023, 11, 14));
        assert_eq!((t.hour(), t.minute(), t.second()), (22, 13, 20));
        assert_eq!(dos_time(Some(0)), DateTime::default());
        assert_eq!(dos_time(None), DateTime::default());
        assert_eq!(dos_time(Some(5_000_000_000)), DateTime::default());
    }

    #[test]
    fn the_extended_timestamp_fits_32_bits_or_is_left_out() {
        assert_eq!(extended_timestamp(Some(1)), Some([1, 1, 0, 0, 0]));
        assert_eq!(extended_timestamp(Some(-1)), None);
        assert_eq!(extended_timestamp(Some(1 << 33)), None);
        assert_eq!(extended_timestamp(None), None);
    }

    #[test]
    fn zip64_is_chosen_with_a_margin_under_4_gib() {
        assert!(!needs_zip64(0));
        assert!(!needs_zip64(4_000_000_000));
        assert!(needs_zip64(u64::from(u32::MAX) - 1000));
        assert!(needs_zip64(u64::from(u32::MAX)));
        assert!(needs_zip64(1 << 40));
    }

    #[test]
    fn containers_come_from_the_extension() {
        assert_eq!(Container::of(Path::new("a.zip")), Some(Container::Zip));
        assert_eq!(Container::of(Path::new("a.ZIP")), Some(Container::Zip));
        assert_eq!(Container::of(Path::new("a.7z")), Some(Container::SevenZ));
        assert_eq!(Container::of(Path::new("a.rar")), None);
        assert_eq!(Container::of(Path::new("zip")), None);
    }

    #[test]
    fn modes_and_attributes_follow_the_kind_and_the_flags() {
        let mut e = Entry {
            kind: EntryKind::File,
            flags: EntryFlags::EMPTY,
            path: "a".into(),
            mtime_ns: 0,
            size: 0,
            symlink_target: None,
            chunks: Vec::new(),
        };
        assert_eq!(unix_mode(&e), 0o100_644);
        assert_eq!(dos_attributes(&e), 0);
        e.flags = EntryFlags::EXECUTABLE
            .union(EntryFlags::READ_ONLY)
            .union(EntryFlags::HIDDEN);
        assert_eq!(unix_mode(&e), 0o100_755);
        assert_eq!(dos_attributes(&e), 0x03);
        e.kind = EntryKind::Directory;
        e.flags = EntryFlags::SYSTEM;
        assert_eq!(unix_mode(&e), 0o040_755);
        assert_eq!(dos_attributes(&e), 0x14);
        e.kind = EntryKind::Symlink;
        assert_eq!(unix_mode(&e), 0o120_777);
    }
}
