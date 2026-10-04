//! The store path: walk a tree and write it with the writer's default (store) graph.

use std::fs::OpenOptions;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use lpk_format::{EntryKind, Writer, WriterOptions, WriterSummary};

use crate::error::CoreError;
use crate::ingest::{file_identity, validate_input, walk, IngestOptions, Input};
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

/// Called before the trailer is written and once after it: puts the data on disk.
pub type SyncFn = Box<dyn FnMut() -> std::io::Result<()>>;

/// Walk `root` and write every entry to `out` in order, streaming file bytes.
///
/// Takes `options` by value because `WriterOptions` owns its encoder and is not `Clone`.
pub fn archive_store(
    root: &Path,
    out: impl Write,
    options: StoreOptions,
) -> Result<WriterSummary, CoreError> {
    let inputs = walk(root, &options.ingest)?;
    write_inputs(&inputs, out, options, None)
}

/// Write already-walked inputs (in entry-table order) with the store path. `sync`, when given,
/// is installed with `Writer::with_sync` so the trailer reaches the disk after the data.
pub fn write_inputs(
    inputs: &[Input],
    out: impl Write,
    options: StoreOptions,
    sync: Option<SyncFn>,
) -> Result<WriterSummary, CoreError> {
    let source = Source::new();
    let mut writer = Writer::new(BufWriter::with_capacity(OUT_BUF, out), options.writer)?;
    if let Some(sync) = sync {
        writer = writer.with_sync(sync);
    }
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
/// file is an error). The data is synced before the trailer is written. A failed write leaves
/// no partial archive behind. When `archive_path` is inside `root` the output file is left out
/// of the archive (matched by file identity) and nothing else changes.
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
    let r = (|| {
        let own_id = file_identity(&file).map_err(|e| CoreError::io(archive_path, e))?;
        let sync_file = file
            .try_clone()
            .map_err(|e| CoreError::io(archive_path, e))?;
        let mut inputs = walk(root, &options.ingest)?;
        inputs.retain(|i| i.identity != Some(own_id));
        let sync: SyncFn = Box::new(move || sync_file.sync_data());
        write_inputs(&inputs, file, options, Some(sync))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(archive_path);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::tests::{clear_readonly, file_link, make_tree, plain_input, skip};
    use lpk_format::{Archive, Resources};
    use std::io::Cursor;
    use std::path::Path;

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
        let r = write_inputs(&inputs, &mut sink, StoreOptions::default(), None);
        assert!(
            matches!(r, Err(CoreError::ChangedWhileReading { .. })),
            "{r:?}"
        );
    }

    #[test]
    fn unportable_input_is_refused_before_writing() {
        let mut i = plain_input("a\\b", EntryKind::Directory);
        let mut sink = Vec::new();
        let r = write_inputs(&[i.clone()], &mut sink, StoreOptions::default(), None);
        assert!(matches!(r, Err(CoreError::UnportableName { .. })));
        i.path = "ok".into();
        assert!(write_inputs(&[i], &mut sink, StoreOptions::default(), None).is_ok());
    }

    #[test]
    fn a_file_that_grew_is_reported_after_one_extra_byte() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), vec![7u8; 5000]).unwrap();
        let mut inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        inputs[0].len = 10; // walked when it was shorter
        let mut sink = Vec::new();
        let r = write_inputs(&inputs, &mut sink, StoreOptions::default(), None);
        assert!(matches!(r, Err(CoreError::ChangedWhileReading { .. })));
    }

    #[test]
    fn a_file_deleted_after_the_walk_is_io_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gone"), b"abc").unwrap();
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        std::fs::remove_file(dir.path().join("gone")).unwrap();
        let mut sink = Vec::new();
        match write_inputs(&inputs, &mut sink, StoreOptions::default(), None) {
            Err(CoreError::Io { path, .. }) => assert!(path.ends_with("gone")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_file_swapped_for_a_directory_or_a_link_fails_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"abc").unwrap();
        std::fs::write(dir.path().join("d"), b"abc").unwrap();
        std::fs::write(dir.path().join("l"), b"abc").unwrap();
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        std::fs::remove_file(dir.path().join("d")).unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let mut sink = Vec::new();
        let r = write_inputs(&inputs, &mut sink, StoreOptions::default(), None);
        assert!(
            matches!(r, Err(CoreError::ChangedWhileReading { .. })),
            "{r:?}"
        );
        std::fs::remove_dir(dir.path().join("d")).unwrap();
        std::fs::write(dir.path().join("d"), b"abc").unwrap();
        std::fs::remove_file(dir.path().join("l")).unwrap();
        if !file_link(&outside.path().join("secret"), &dir.path().join("l")) {
            skip("cannot create symlinks without privilege");
            return;
        }
        let inputs = walk(dir.path(), &IngestOptions::default()).unwrap();
        // The walk now sees a symlink at "l"; swap the *walked* file entry "d" instead.
        let d_input: Vec<Input> = inputs.iter().filter(|i| i.path == "d").cloned().collect();
        std::fs::remove_file(dir.path().join("d")).unwrap();
        assert!(file_link(
            &outside.path().join("secret"),
            &dir.path().join("d")
        ));
        let r = write_inputs(&d_input, &mut sink, StoreOptions::default(), None);
        assert!(
            matches!(r, Err(CoreError::ChangedWhileReading { .. })),
            "{r:?}"
        );
    }

    #[test]
    fn the_output_inside_the_root_is_not_its_own_input() {
        let dir = tempfile::tempdir().unwrap();
        make_tree(dir.path());
        let arch = dir.path().join("out.lpk");
        let s = archive_store_file(dir.path(), &arch, StoreOptions::default()).unwrap();
        assert_eq!(s.entries, 17);
        let bytes = std::fs::read(&arch).unwrap();
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        let table = a.entry_table().unwrap();
        let paths: Vec<String> = table
            .table()
            .unwrap()
            .iter()
            .map(|e| e.unwrap().path)
            .collect();
        assert_eq!(paths.len(), 17);
        assert!(!paths.iter().any(|p| p == "out.lpk"));
        a.verify().unwrap();
        clear_readonly(&dir.path().join("ro.txt"));
    }

    #[test]
    fn a_failed_store_file_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("out.lpk");
        let r = archive_store_file(
            &dir.path().join("missing-root"),
            &arch,
            StoreOptions::default(),
        );
        assert!(r.is_err());
        assert!(!arch.exists());
    }

    #[test]
    fn symlink_entries_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real"), b"x").unwrap();
        if !file_link(Path::new("real"), &dir.path().join("link")) {
            skip("cannot create symlinks without privilege");
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
