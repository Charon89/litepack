//! Ingest: walk a directory tree into the archive's inputs, sorted as the entry table requires.
//!
//! Junctions and other reparse points that Rust reports as symlinks are recorded as `Symlink`
//! entries and not followed; with `one_file_system` (the default) a followed tree also stays on
//! the root's volume. Hidden entries: on Windows the `HIDDEN` attribute, on Unix a leading dot
//! (which also sets the `HIDDEN` flag).

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lpk_format::{validate_path, EntryFlags, EntryKind};
use walkdir::{DirEntry, WalkDir};

use crate::error::CoreError;

/// How a tree is walked and its files are opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOptions {
    /// Follow symlinks and archive what they point to (cycles are an error). Default false:
    /// a symlink is recorded as a `Symlink` entry.
    pub follow_symlinks: bool,
    /// Files of at least this many bytes are read through a memory map. Default 1 MiB.
    pub mmap_threshold: u64,
    /// Include hidden entries. Default true.
    pub include_hidden: bool,
    /// Do not cross mount points (volumes on Windows). Default true.
    pub one_file_system: bool,
}

impl Default for IngestOptions {
    fn default() -> Self {
        IngestOptions {
            follow_symlinks: false,
            mmap_threshold: 1 << 20,
            include_hidden: true,
            one_file_system: true,
        }
    }
}

/// One entry to archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    /// Archive path: components joined with `/`, relative to the walk root.
    pub path: String,
    /// File, directory or symlink.
    pub kind: EntryKind,
    /// Length in bytes (for a symlink the target's length; 0 for a directory).
    pub len: u64,
    /// Modification time, nanoseconds since the Unix epoch; `i64::MIN` when unknown.
    pub mtime_ns: i64,
    /// Attribute flags.
    pub flags: EntryFlags,
    /// Where the content is read from.
    pub source: PathBuf,
    /// The link target's bytes, for a symlink.
    pub symlink_target: Option<Vec<u8>>,
}

/// Check that an input's archive path is one the format accepts.
pub fn validate_input(input: &Input) -> Result<(), CoreError> {
    validate_path(&input.path).map_err(|reason| CoreError::UnportableName {
        path: input.path.clone(),
        reason: reason.to_string(),
    })
}

/// Every directory, file and symlink under `root` (not `root` itself), sorted by path bytes.
pub fn walk(root: &Path, options: &IngestOptions) -> Result<Vec<Input>, CoreError> {
    let md = std::fs::metadata(root).map_err(|e| CoreError::io(root, e))?;
    if !md.is_dir() {
        return Err(CoreError::NotADirectory {
            path: root.to_path_buf(),
        });
    }
    let it = WalkDir::new(root)
        .min_depth(1)
        .follow_links(options.follow_symlinks)
        .same_file_system(options.one_file_system)
        .into_iter()
        .filter_entry(|e| options.include_hidden || !is_hidden(e));
    let mut out = Vec::new();
    for entry in it {
        let entry = entry.map_err(|e| {
            let path = e
                .path()
                .map_or_else(|| root.to_path_buf(), Path::to_path_buf);
            CoreError::io(path, std::io::Error::from(e))
        })?;
        out.push(input_of(root, &entry)?);
    }
    out.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    out.dedup_by(|a, b| a.path == b.path);
    Ok(out)
}

fn input_of(root: &Path, entry: &DirEntry) -> Result<Input, CoreError> {
    let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
    let mut parts: Vec<&str> = Vec::new();
    for c in rel.components() {
        match c.as_os_str().to_str() {
            Some(s) => parts.push(s),
            None => {
                return Err(CoreError::UnportableName {
                    path: rel.to_string_lossy().into_owned(),
                    reason: "not utf-8".to_string(),
                })
            }
        }
    }
    let path = parts.join("/");
    let unportable = |reason: &str| CoreError::UnportableName {
        path: path.clone(),
        reason: reason.to_string(),
    };
    validate_path(&path).map_err(unportable)?;

    let md = entry
        .metadata()
        .map_err(|e| CoreError::io(entry.path(), std::io::Error::from(e)))?;
    let ft = entry.file_type();
    let mtime_ns = mtime_ns(&md);
    let mut flags = attribute_flags(&md, entry.file_name().to_str().unwrap_or(""));
    let source = entry.path().to_path_buf();
    if ft.is_dir() {
        Ok(Input {
            path,
            kind: EntryKind::Directory,
            len: 0,
            mtime_ns,
            flags: strip_file_only(flags),
            source,
            symlink_target: None,
        })
    } else if ft.is_symlink() {
        let target =
            std::fs::read_link(entry.path()).map_err(|e| CoreError::io(entry.path(), e))?;
        let target = path_bytes(&target);
        flags = strip_file_only(flags);
        Ok(Input {
            path,
            kind: EntryKind::Symlink,
            len: target.len() as u64,
            mtime_ns,
            flags,
            source,
            symlink_target: Some(target),
        })
    } else if ft.is_file() {
        Ok(Input {
            path,
            kind: EntryKind::File,
            len: md.len(),
            mtime_ns,
            flags,
            source,
            symlink_target: None,
        })
    } else {
        Err(unportable("special file"))
    }
}

/// Executable and read-only describe files only.
fn strip_file_only(flags: EntryFlags) -> EntryFlags {
    let mut out = EntryFlags::EMPTY;
    for f in [EntryFlags::HIDDEN, EntryFlags::SYSTEM] {
        if flags.contains(f) {
            out = out.union(f);
        }
    }
    out
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    let Ok(t) = md.modified() else {
        return i64::MIN;
    };
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    }
}

#[cfg(unix)]
fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}

#[cfg(windows)]
fn attribute_flags(md: &std::fs::Metadata, _name: &str) -> EntryFlags {
    use std::os::windows::fs::MetadataExt;
    const READONLY: u32 = 0x1;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    let a = md.file_attributes();
    let mut f = EntryFlags::EMPTY;
    if a & READONLY != 0 {
        f = f.union(EntryFlags::READ_ONLY);
    }
    if a & HIDDEN != 0 {
        f = f.union(EntryFlags::HIDDEN);
    }
    if a & SYSTEM != 0 {
        f = f.union(EntryFlags::SYSTEM);
    }
    f
}

#[cfg(unix)]
fn attribute_flags(md: &std::fs::Metadata, name: &str) -> EntryFlags {
    use std::os::unix::fs::PermissionsExt;
    let mut f = EntryFlags::EMPTY;
    if md.permissions().mode() & 0o111 != 0 {
        f = f.union(EntryFlags::EXECUTABLE);
    }
    if name.starts_with('.') {
        f = f.union(EntryFlags::HIDDEN);
    }
    f
}

#[cfg(not(any(windows, unix)))]
fn attribute_flags(_md: &std::fs::Metadata, _name: &str) -> EntryFlags {
    EntryFlags::EMPTY
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .metadata()
        .map(|md| attribute_flags(&md, entry.file_name().to_str().unwrap_or("")))
        .is_ok_and(|f| f.contains(EntryFlags::HIDDEN))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    const MIB: usize = 1 << 20;

    fn write(root: &Path, rel: &str, len: usize) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
        fs::write(p, data).unwrap();
    }

    /// The test tree; returns the sorted expected paths.
    pub(crate) fn make_tree(root: &Path) -> Vec<String> {
        write(root, "a/b/c/deep.txt", 10);
        write(root, "a/b/empty-dir-sibling.bin", 3);
        fs::create_dir_all(root.join("a/b/c/d")).unwrap();
        write(root, "empty.bin", 0);
        write(root, "one.bin", 1);
        write(root, "f4095.bin", 4095);
        write(root, "f4096.bin", 4096);
        write(root, "mib-1.bin", MIB - 1);
        write(root, "mib.bin", MIB);
        write(root, "mib+1.bin", MIB + 1);
        write(root, "unicodé-名前.txt", 5);
        write(root, "with space/name with spaces.txt", 6);
        write(root, "ro.txt", 7);
        let mut perm = fs::metadata(root.join("ro.txt")).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(root.join("ro.txt"), perm).unwrap();
        let mut v: Vec<String> = [
            "a",
            "a/b",
            "a/b/c",
            "a/b/c/d",
            "a/b/c/deep.txt",
            "a/b/empty-dir-sibling.bin",
            "empty.bin",
            "one.bin",
            "f4095.bin",
            "f4096.bin",
            "mib-1.bin",
            "mib.bin",
            "mib+1.bin",
            "unicodé-名前.txt",
            "with space",
            "with space/name with spaces.txt",
            "ro.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        v.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        v
    }

    fn find<'a>(v: &'a [Input], p: &str) -> &'a Input {
        v.iter().find(|i| i.path == p).unwrap()
    }

    #[test]
    fn walk_sorted_kinds_lengths_flags() {
        let dir = tempfile::tempdir().unwrap();
        let expect = make_tree(dir.path());
        let got = walk(dir.path(), &IngestOptions::default()).unwrap();
        let paths: Vec<&str> = got.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, expect.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(find(&got, "a").kind, EntryKind::Directory);
        assert_eq!(find(&got, "a/b/c/d").kind, EntryKind::Directory);
        assert_eq!(find(&got, "a/b/c/d").len, 0);
        for (p, len) in [
            ("empty.bin", 0u64),
            ("one.bin", 1),
            ("f4095.bin", 4095),
            ("f4096.bin", 4096),
            ("mib-1.bin", MIB as u64 - 1),
            ("mib.bin", MIB as u64),
            ("mib+1.bin", MIB as u64 + 1),
            ("unicodé-名前.txt", 5),
        ] {
            let i = find(&got, p);
            assert_eq!(i.kind, EntryKind::File, "{p}");
            assert_eq!(i.len, len, "{p}");
            assert!(i.mtime_ns > 0, "{p}");
            assert!(i.symlink_target.is_none());
        }
        let ro = find(&got, "ro.txt");
        #[cfg(windows)]
        assert!(ro.flags.contains(EntryFlags::READ_ONLY));
        #[cfg(unix)]
        assert!(!ro.flags.contains(EntryFlags::EXECUTABLE));
        assert!(!find(&got, "one.bin").flags.contains(EntryFlags::READ_ONLY));
        // Restore so the temp dir can be removed on Windows.
        let mut perm = fs::metadata(&ro.source).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&ro.source, perm).unwrap();
    }

    #[test]
    fn root_must_be_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f", 1);
        let r = walk(&dir.path().join("f"), &IngestOptions::default());
        assert!(matches!(r, Err(CoreError::NotADirectory { .. })));
        let r = walk(&dir.path().join("missing"), &IngestOptions::default());
        assert!(matches!(r, Err(CoreError::Io { .. })));
    }

    #[test]
    fn empty_root_walks_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(walk(dir.path(), &IngestOptions::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn mtime_is_nanoseconds_since_the_epoch() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f", 1);
        let got = walk(dir.path(), &IngestOptions::default()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        assert!((now - got[0].mtime_ns).abs() < 3_600_000_000_000);
    }

    #[test]
    fn unportable_name_from_a_constructed_input() {
        for (path, reason) in [
            ("a\\b", "backslash"),
            ("a\0b", "nul"),
            ("a//b", "empty component"),
            ("../x", "dot component"),
        ] {
            let i = Input {
                path: path.to_string(),
                kind: EntryKind::File,
                len: 0,
                mtime_ns: 0,
                flags: EntryFlags::EMPTY,
                source: PathBuf::new(),
                symlink_target: None,
            };
            match validate_input(&i) {
                Err(CoreError::UnportableName { path: p, reason: r }) => {
                    assert_eq!(p, path);
                    assert_eq!(r, reason);
                }
                other => panic!("{other:?}"),
            }
        }
        let long = Input {
            path: "x".repeat(70_000),
            kind: EntryKind::File,
            len: 0,
            mtime_ns: 0,
            flags: EntryFlags::EMPTY,
            source: PathBuf::new(),
            symlink_target: None,
        };
        assert!(matches!(
            validate_input(&long),
            Err(CoreError::UnportableName { reason, .. }) if reason == "too long"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_backslash_name_fails_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "bad\\name", 1);
        match walk(dir.path(), &IngestOptions::default()) {
            Err(CoreError::UnportableName { path, reason }) => {
                assert_eq!(path, "bad\\name");
                assert_eq!(reason, "backslash");
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_non_utf8_name_fails_the_walk() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let name = std::ffi::OsStr::from_bytes(b"bad\xff.txt");
        fs::write(dir.path().join(name), b"x").unwrap();
        match walk(dir.path(), &IngestOptions::default()) {
            Err(CoreError::UnportableName { reason, .. }) => assert_eq!(reason, "not utf-8"),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_hidden_and_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".hidden", 1);
        write(dir.path(), ".hdir/inner", 1);
        write(dir.path(), "run.sh", 1);
        fs::set_permissions(dir.path().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let all = walk(dir.path(), &IngestOptions::default()).unwrap();
        assert!(find(&all, ".hidden").flags.contains(EntryFlags::HIDDEN));
        assert!(find(&all, "run.sh").flags.contains(EntryFlags::EXECUTABLE));
        let opts = IngestOptions {
            include_hidden: false,
            ..IngestOptions::default()
        };
        let some = walk(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = some.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["run.sh"]);
    }

    /// Windows: the `HIDDEN` attribute cannot be set from `std`, so only the Unix test above
    /// exercises `include_hidden: false`; here the default must keep everything.
    #[cfg(windows)]
    #[test]
    fn windows_include_hidden_false_keeps_plain_files() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".dotfile-is-not-hidden-on-windows", 1);
        let opts = IngestOptions {
            include_hidden: false,
            ..IngestOptions::default()
        };
        assert_eq!(walk(dir.path(), &opts).unwrap().len(), 1);
    }

    fn make_symlink(target: &Path, link: &Path, dir: bool) -> bool {
        #[cfg(unix)]
        {
            let _ = dir;
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            if dir {
                std::os::windows::fs::symlink_dir(target, link).is_ok()
            } else {
                std::os::windows::fs::symlink_file(target, link).is_ok()
            }
        }
    }

    #[test]
    fn symlinks_are_recorded_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "real.txt", 4);
        if !make_symlink(Path::new("real.txt"), &dir.path().join("link"), false) {
            eprintln!("skipped: cannot create symlinks without privilege");
            return;
        }
        let got = walk(dir.path(), &IngestOptions::default()).unwrap();
        let l = find(&got, "link");
        assert_eq!(l.kind, EntryKind::Symlink);
        assert_eq!(l.symlink_target.as_deref(), Some(&b"real.txt"[..]));
        assert_eq!(l.len, 8);
        assert_eq!(find(&got, "real.txt").kind, EntryKind::File);
        // Followed: the link is a file of the target's length.
        let opts = IngestOptions {
            follow_symlinks: true,
            ..IngestOptions::default()
        };
        let got = walk(dir.path(), &opts).unwrap();
        let l = find(&got, "link");
        assert_eq!(l.kind, EntryKind::File);
        assert_eq!(l.len, 4);
    }

    #[test]
    fn a_cycle_through_a_followed_symlink_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "d/file", 1);
        if !make_symlink(dir.path(), &dir.path().join("d/loop"), true) {
            eprintln!("skipped: cannot create symlinks without privilege");
            return;
        }
        // Not followed: fine, the loop is just a symlink entry.
        let got = walk(dir.path(), &IngestOptions::default()).unwrap();
        assert_eq!(find(&got, "d/loop").kind, EntryKind::Symlink);
        let opts = IngestOptions {
            follow_symlinks: true,
            ..IngestOptions::default()
        };
        assert!(matches!(walk(dir.path(), &opts), Err(CoreError::Io { .. })));
    }
}
