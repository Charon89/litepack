//! Source: open an input's bytes through a plain file, never following a link.

use std::fs::{File, OpenOptions};
use std::io::Read;

use lpk_format::EntryKind;

use crate::error::CoreError;
use crate::ingest::{file_identity, Input};

/// Opens the content of file inputs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Source;

impl Source {
    /// A source.
    pub fn new() -> Self {
        Source
    }

    /// The bytes of a file input, at most `len + 1` of them (one past the walked length, so a
    /// file that grew shows as a length mismatch without reading it all).
    ///
    /// Unix opens with `O_NOFOLLOW | O_NONBLOCK`. Windows opens normally (the reparse-point
    /// flag would bypass the cloud-files, deduplication and WOF filters and read a stub instead
    /// of the content); there the protection against a swapped link is the identity check, with
    /// the residual that a file reached through a swapped junction is opened (read access only,
    /// no side effect) before the check rejects it. The opened object must be a regular file
    /// and the object the walk saw; otherwise [`CoreError::ChangedWhileReading`].
    pub fn open(&self, input: &Input) -> Result<Box<dyn Read + '_>, CoreError> {
        if input.kind != EntryKind::File {
            return Err(CoreError::io(
                &input.source,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
            ));
        }
        let changed = || CoreError::ChangedWhileReading {
            path: input.source.clone(),
        };
        let file = match open_no_follow(&input.source) {
            Ok(f) => f,
            Err(e) => {
                // A path that is now a link, a directory or another kind of object is a change,
                // not an I/O fault.
                return Err(match std::fs::symlink_metadata(&input.source) {
                    Ok(m) if !m.is_file() => changed(),
                    _ => CoreError::io(&input.source, e),
                });
            }
        };
        let md = file
            .metadata()
            .map_err(|e| CoreError::io(&input.source, e))?;
        if !md.is_file() {
            return Err(changed());
        }
        if let Some(id) = input.identity {
            let now = file_identity(&file).map_err(|e| CoreError::io(&input.source, e))?;
            if now != id {
                return Err(changed());
            }
        }
        Ok(Box::new(file.take(input.len.saturating_add(1))))
    }
}

#[cfg(unix)]
fn open_no_follow(path: &std::path::Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &std::path::Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::{clear_readonly, file_link, make_tree, skip};
    use crate::ingest::{walk, IngestOptions};

    #[test]
    fn open_yields_the_files_bytes_on_both_sides_of_1_mib() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        let mut checked = 0;
        for i in inputs.iter().filter(|i| i.kind == EntryKind::File) {
            let want = std::fs::read(&i.source).unwrap();
            let mut got = Vec::new();
            Source::new()
                .open(i)
                .unwrap()
                .read_to_end(&mut got)
                .unwrap();
            assert_eq!(got, want, "{}", i.path);
            checked += 1;
        }
        assert!(checked >= 12);
        clear_readonly(&dir.path().join("ro.txt"));
    }

    #[test]
    fn reads_stop_one_byte_past_the_walked_length() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), vec![1u8; 100]).unwrap();
        let mut inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        inputs[0].len = 10;
        let mut got = Vec::new();
        Source::new()
            .open(&inputs[0])
            .unwrap()
            .read_to_end(&mut got)
            .unwrap();
        assert_eq!(got.len(), 11);
    }

    #[test]
    fn opening_a_directory_input_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        assert!(Source::new().open(&inputs[0]).is_err());
    }

    #[test]
    fn a_file_swapped_for_a_directory_is_changed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"abc").unwrap();
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        std::fs::remove_file(dir.path().join("f")).unwrap();
        std::fs::create_dir(dir.path().join("f")).unwrap();
        assert!(matches!(
            Source::new().open(&inputs[0]),
            Err(CoreError::ChangedWhileReading { .. })
        ));
    }

    #[test]
    fn a_file_swapped_for_a_symlink_is_changed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"abc").unwrap();
        std::fs::write(dir.path().join("f"), b"abc").unwrap();
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        std::fs::remove_file(dir.path().join("f")).unwrap();
        if !file_link(&outside.path().join("secret"), &dir.path().join("f")) {
            skip("cannot create symlinks without privilege");
            return;
        }
        assert!(matches!(
            Source::new().open(&inputs[0]),
            Err(CoreError::ChangedWhileReading { .. })
        ));
    }

    #[test]
    fn a_file_replaced_by_another_file_is_changed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"abc").unwrap();
        std::fs::write(dir.path().join("g"), b"abc").unwrap();
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        std::fs::remove_file(dir.path().join("f")).unwrap();
        std::fs::rename(dir.path().join("g"), dir.path().join("f")).unwrap();
        assert!(matches!(
            Source::new().open(&inputs[0]),
            Err(CoreError::ChangedWhileReading { .. })
        ));
    }
}
