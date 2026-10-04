//! A read-only memory map of a file, behind a safe constructor.
//!
//! `memmap2::Mmap::map` is `unsafe` because another process may truncate or rewrite the file
//! while it is mapped: reading past a truncation faults (SIGBUS on Unix, an access violation on
//! Windows) and a concurrent write changes bytes under the reader. LitePack archives files that
//! are normally idle; a file changing under a live read is outside what this crate can detect.
//! `lpk-core` only maps files of at least `IngestOptions::mmap_threshold` bytes (default 1 MiB).

use std::fs::File;
use std::io;

/// A file mapped read-only into memory; `AsRef<[u8]>` gives its bytes.
#[derive(Debug)]
pub struct Mapped(memmap2::Mmap);

impl Mapped {
    /// Map `file` read-only. An empty file cannot be mapped on every platform, so callers read
    /// empty files directly.
    ///
    /// The file must not be truncated by another process while the map lives (see the crate
    /// documentation).
    pub fn map(file: &File) -> io::Result<Mapped> {
        // SAFETY: the map is read-only and private to this process. The remaining hazard is an
        // external truncation or write during the map's life, which is the documented contract
        // of this function and cannot cause memory unsafety beyond a process fault.
        unsafe { memmap2::Mmap::map(file) }.map(Mapped)
    }
}

impl AsRef<[u8]> for Mapped {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn maps_the_bytes_of_a_file() {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(b"hello mapped world").unwrap();
        let m = Mapped::map(&f).unwrap();
        assert_eq!(m.as_ref(), b"hello mapped world");
    }
}
