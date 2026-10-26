//! libarchive 3.8.9's read side for LitePack's sandboxed `lpk-worker-libarchive` (E4-4; design
//! `docs/notes/e4-design.md` §3): hand-written bindings ([`ffi`], a real build only) and a small
//! safe wrapper, [`Archive`], over one archive given as one or more volume files.
//!
//! The wrapper registers only the readers and filters the caller names ([`ReadFormat`],
//! [`ReadFilter`]), never libarchive's `…_all`. It runs only inside the sandboxed worker: the
//! host never links this crate.
//!
//! When `vendor/libarchive-3.8.9/` is absent the crate is a stub (see `build.rs`): [`BUILT`] is
//! false and [`Archive::open`] fails with [`ErrorKind::NotBuilt`].
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::fs::File;

#[cfg(lpk_libarchive)]
pub mod ffi;

#[cfg(lpk_libarchive)]
mod real;
#[cfg(lpk_libarchive)]
pub use real::Archive;

#[cfg(not(lpk_libarchive))]
mod stub;
#[cfg(not(lpk_libarchive))]
pub use stub::Archive;

/// Whether libarchive is compiled into this build (false: the stub).
pub const BUILT: bool = cfg!(lpk_libarchive);

/// The vendored libarchive release.
pub const VERSION: &str = "3.8.9";

/// A libarchive format reader, registered by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFormat {
    /// RAR 2.9 to 4.x.
    Rar,
    /// RAR 5.
    Rar5,
    /// ISO 9660 with Rock Ridge and Joliet.
    Iso9660,
    /// Microsoft Cabinet.
    Cab,
    /// LHA/LZH.
    Lha,
    /// Unix `ar`.
    Ar,
    /// cpio (odc, newc, crc, binary).
    Cpio,
    /// A single stream (only behind the `compress` or `uu` filter).
    Raw,
    /// tar (only behind the `compress` or `uu` filter; plain tar is E4-3's).
    Tar,
}

/// A libarchive filter, registered by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFilter {
    /// gzip (zlib).
    Gzip,
    /// xz and lzma (liblzma).
    Xz,
    /// Zstandard (libzstd).
    Zstd,
    /// Unix `compress` (`.Z`).
    Compress,
    /// The RPM lead and header in front of a cpio payload.
    Rpm,
    /// uuencode and base64 (`begin`, `begin-base64`).
    Uu,
}

/// What a libarchive call failed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// `ARCHIVE_FATAL`: the archive object is unusable.
    Fatal,
    /// `ARCHIVE_FAILED`: this operation failed (another entry may still be read).
    Failed,
    /// `ARCHIVE_RETRY` or `ARCHIVE_WARN` where data was expected.
    Other,
    /// Reading a volume file failed.
    Io,
    /// libarchive is not compiled into this build.
    NotBuilt,
}

/// A failure of libarchive or of reading a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// What kind.
    pub kind: ErrorKind,
    /// libarchive's `archive_errno` (0 when none).
    pub errno: i32,
    /// libarchive's `archive_error_string`, or the I/O error.
    pub message: String,
}

impl Error {
    /// The stub's error.
    pub fn not_built() -> Self {
        Error {
            kind: ErrorKind::NotBuilt,
            errno: 0,
            message: format!(
                "libarchive {VERSION} is not built into this worker (vendor/libarchive-{VERSION} absent)"
            ),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// An entry's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
    /// Anything else (devices, FIFOs, sockets): the `AE_IFMT` bits.
    Other(u32),
}

/// An owned copy of what libarchive reports about an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name as UTF-8 (`None` when libarchive could not convert it).
    pub path: Option<Vec<u8>>,
    /// The type.
    pub kind: EntryKind,
    /// The size, when the header declares one.
    pub size: Option<u64>,
    /// Seconds and nanoseconds since the Unix epoch, when set.
    pub mtime: Option<(i64, i64)>,
    /// A symbolic link's target (UTF-8).
    pub symlink: Option<Vec<u8>>,
    /// A hard link's target (UTF-8).
    pub hardlink: Option<Vec<u8>>,
    /// The entry is encrypted (data or metadata).
    pub encrypted: bool,
    /// libarchive's header position in the (filtered) input.
    pub header_position: i64,
    /// A warning libarchive gave with this header (`ARCHIVE_WARN`).
    pub warning: Option<String>,
}

/// What to open: the volume files in order (one for a plain archive), the readers and filters
/// to register, and a passphrase.
#[derive(Debug)]
pub struct OpenSpec<'a> {
    /// The volumes, first to last.
    pub volumes: Vec<File>,
    /// The readers.
    pub formats: &'a [ReadFormat],
    /// The filters (`none` is always registered).
    pub filters: &'a [ReadFilter],
    /// A passphrase for libarchive's decryption (no reader compiled here decrypts; kept for
    /// completeness).
    pub passphrase: Option<&'a [u8]>,
}

#[cfg(test)]
mod tests;
