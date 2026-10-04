//! The store path: walk a tree and write it with the writer's default (store) graph.

use std::fs::OpenOptions;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use lpk_format::{EntryKind, Writer, WriterOptions, WriterSummary};

use crate::error::CoreError;
use crate::ingest::{validate_input, walk, IngestOptions, Input};
use crate::source::Source;

/// Output buffer: writes reach the file in large sequential pieces.
const OUT_BUF: usize = 4 << 20;

/// Settings of [`archive_store`].
#[derive(Debug, Default)]
pub struct StoreOptions {
    /// How the tree is walked.
    pub ingest: IngestOptions,
    /// The writer's settings (its default encoder stores).
    pub writer: WriterOptions,
}

/// Counts what passes through a reader.
struct Counting<R> {
    inner: R,
    n: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.n += n as u64;
        Ok(n)
    }
}

/// Walk `root` and write every entry to `out` in order, streaming file bytes.
///
/// Takes `options` by value because `WriterOptions` owns its encoder and is not `Clone`.
pub fn archive_store(
    root: &Path,
    out: impl Write,
    options: StoreOptions,
) -> Result<WriterSummary, CoreError> {
    let inputs = walk(root, &options.ingest)?;
    write_inputs(&inputs, out, options)
}

/// Write already-walked inputs (in entry-table order) with the store path.
pub fn write_inputs(
    inputs: &[Input],
    out: impl Write,
    options: StoreOptions,
) -> Result<WriterSummary, CoreError> {
    let source = Source::new(&options.ingest);
    let mut writer = Writer::new(BufWriter::with_capacity(OUT_BUF, out), options.writer)?;
    for input in inputs {
        validate_input(input)?;
        match input.kind {
            EntryKind::Directory => {
                writer.add_directory(&input.path, input.flags, input.mtime_ns)?;
            }
            EntryKind::Symlink => {
                let target = input.symlink_target.as_deref().unwrap_or_default();
                writer.add_symlink(&input.path, input.flags, input.mtime_ns, target)?;
            }
            EntryKind::File => {
                let mut r = Counting {
                    inner: source.open(input)?,
                    n: 0,
                };
                writer.add_file(&input.path, input.flags, input.mtime_ns, &mut r)?;
                if r.n != input.len {
                    return Err(CoreError::ChangedWhileReading {
                        path: input.source.clone(),
                    });
                }
            }
        }
    }
    Ok(writer.finish()?)
}

/// Like [`archive_store`], writing to a new file at `archive_path` (create-new: an existing
/// file is an error). A failed write leaves no partial archive behind.
pub fn archive_store_file(
    root: &Path,
    archive_path: &Path,
    options: StoreOptions,
) -> Result<WriterSummary, CoreError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(archive_path)
        .map_err(|e| CoreError::io(archive_path, e))?;
    let r = archive_store(root, file, options);
    if r.is_err() {
        let _ = std::fs::remove_file(archive_path);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::make_tree;
    use lpk_format::{Archive, Resources};
    use std::io::Cursor;

    fn store_tree() -> (tempfile::TempDir, Vec<u8>, WriterSummary) {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let mut bytes = Vec::new();
        let s = archive_store(dir.path(), &mut bytes, StoreOptions::default()).unwrap();
        (dir, bytes, s)
    }

    #[test]
    fn archive_store_round_trips_bit_exact() {
        let (dir, bytes, s) = store_tree();
        assert_eq!(s.archive_len, bytes.len() as u64);
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        let table = a.entry_table().unwrap();
        let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
        assert_eq!(entries.len() as u64, s.entries);
        assert_eq!(entries.len(), 17);
        let mut files = 0;
        for e in &entries {
            if e.kind != EntryKind::File {
                continue;
            }
            let mut out = Vec::new();
            a.extract(e, &mut out).unwrap();
            assert_eq!(
                out,
                std::fs::read(dir.path().join(&e.path)).unwrap(),
                "{}",
                e.path
            );
            files += 1;
        }
        assert_eq!(files, 12);
        a.verify().unwrap();
        // Read-only restore so the temp dir can be removed on Windows.
        let p = dir.path().join("ro.txt");
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&p, perm).unwrap();
    }

    #[test]
    fn lpk_decode_list_shows_entries_in_order() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let out_dir = tempfile::tempdir().unwrap();
        let arch = out_dir.path().join("t.lpk");
        archive_store_file(dir.path(), &arch, StoreOptions::default()).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = lpk_format::cli::run(
            ["lpk-decode".as_ref(), "list".as_ref(), arch.as_os_str()],
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        let text = String::from_utf8(out).unwrap();
        let paths: Vec<&str> = text
            .lines()
            .map(|l| l.split('\t').nth(2).unwrap())
            .collect();
        assert_eq!(paths.len(), 17);
        assert_eq!(paths[0], "a");
        assert_eq!(paths[1], "a/b");
        assert!(text.contains("File\t1048577\tmib+1.bin"), "{text}");
        let mut sorted = paths.clone();
        sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(paths, sorted);
        // create-new: a second run onto the same file is refused.
        let again = archive_store_file(dir.path(), &arch, StoreOptions::default());
        assert!(matches!(again, Err(CoreError::Io { .. })));
        let mut perm = std::fs::metadata(dir.path().join("ro.txt"))
            .unwrap()
            .permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(dir.path().join("ro.txt"), perm).unwrap();
    }

    #[test]
    fn symlinks_and_empty_trees_archive() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = Vec::new();
        let s = archive_store(dir.path(), &mut bytes, StoreOptions::default()).unwrap();
        assert_eq!(s.entries, 0);
        Archive::open(Cursor::new(bytes), &Resources::default())
            .unwrap()
            .verify()
            .unwrap();
    }

    #[test]
    fn a_file_that_changes_length_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"12345").unwrap();
        let mut inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        inputs[0].len = 9; // as if the file had been longer when walked
        let mut sink = Vec::new();
        let r = write_inputs(&inputs, &mut sink, StoreOptions::default());
        assert!(
            matches!(r, Err(CoreError::ChangedWhileReading { .. })),
            "{r:?}"
        );
    }

    #[test]
    fn unportable_input_is_refused_before_writing() {
        let mut i = Input {
            path: "a\\b".into(),
            kind: EntryKind::Directory,
            len: 0,
            mtime_ns: 0,
            flags: lpk_format::EntryFlags::EMPTY,
            source: Default::default(),
            symlink_target: None,
        };
        let mut sink = Vec::new();
        let r = write_inputs(&[i.clone()], &mut sink, StoreOptions::default());
        assert!(matches!(r, Err(CoreError::UnportableName { .. })));
        i.path = "ok".into();
        assert!(write_inputs(&[i], &mut sink, StoreOptions::default()).is_ok());
    }

    #[test]
    fn symlink_entries_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real"), b"x").unwrap();
        #[cfg(unix)]
        let ok = std::os::unix::fs::symlink("real", dir.path().join("link")).is_ok();
        #[cfg(windows)]
        let ok = std::os::windows::fs::symlink_file("real", dir.path().join("link")).is_ok();
        if !ok {
            eprintln!("skipped: cannot create symlinks without privilege");
            return;
        }
        let mut bytes = Vec::new();
        archive_store(dir.path(), &mut bytes, StoreOptions::default()).unwrap();
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        let table = a.entry_table().unwrap();
        let link = table
            .table()
            .unwrap()
            .iter()
            .map(|e| e.unwrap())
            .find(|e| e.path == "link")
            .unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
        assert_eq!(link.symlink_target.as_deref(), Some(&b"real"[..]));
    }
}
