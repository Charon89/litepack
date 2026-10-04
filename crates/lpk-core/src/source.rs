//! Source: open an input's bytes, through a memory map for large files.

use std::fs::File;
use std::io::{BufReader, Cursor, Read};

use lpk_format::EntryKind;
use lpk_mmap_sys::Mapped;

use crate::error::CoreError;
use crate::ingest::{IngestOptions, Input};

/// Buffer of the plain file reader.
const BUF: usize = 256 << 10;

/// Opens the content of file inputs.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    mmap_threshold: u64,
}

impl Source {
    /// A source using the options' `mmap_threshold`.
    pub fn new(options: &IngestOptions) -> Self {
        Source {
            mmap_threshold: options.mmap_threshold,
        }
    }

    /// The bytes of a file input: a read-only memory map when `input.len` is at least the
    /// threshold, a buffered file otherwise. Both yield the same bytes.
    pub fn open(&self, input: &Input) -> Result<Box<dyn Read + '_>, CoreError> {
        if input.kind != EntryKind::File {
            return Err(CoreError::io(
                &input.source,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
            ));
        }
        let file = File::open(&input.source).map_err(|e| CoreError::io(&input.source, e))?;
        if input.len > 0 && input.len >= self.mmap_threshold {
            let map = Mapped::map(&file).map_err(|e| CoreError::io(&input.source, e))?;
            Ok(Box::new(Cursor::new(map)))
        } else {
            Ok(Box::new(BufReader::with_capacity(BUF, file)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{walk, IngestOptions};

    #[test]
    fn mmap_and_buffered_yield_identical_bytes() {
        let dir = tempfile::tempdir().unwrap();
        crate::ingest::tests::make_tree(dir.path());
        let opts = IngestOptions::default();
        let inputs = walk(dir.path(), &opts).unwrap();
        let mapped = Source::new(&opts);
        let buffered = Source::new(&IngestOptions {
            mmap_threshold: u64::MAX,
            ..opts
        });
        let mut checked = 0;
        for i in inputs.iter().filter(|i| i.kind == EntryKind::File) {
            let want = std::fs::read(&i.source).unwrap();
            for s in [&mapped, &buffered] {
                let mut got = Vec::new();
                s.open(i).unwrap().read_to_end(&mut got).unwrap();
                assert_eq!(got, want, "{}", i.path);
            }
            checked += 1;
        }
        assert!(checked >= 12);
    }

    #[test]
    fn opening_a_directory_input_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let opts = IngestOptions::default();
        let inputs = walk(dir.path(), &opts).unwrap();
        assert!(Source::new(&opts).open(&inputs[0]).is_err());
    }
}
