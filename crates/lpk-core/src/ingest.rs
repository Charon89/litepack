//! Ingest: walk a directory tree into the archive's inputs, sorted as the entry table requires.
//!
//! Hidden entries: on Windows the `HIDDEN` attribute, on Unix a leading dot (which also sets the
//! `HIDDEN` flag). Windows junctions and other reparse points that Rust reports as symlinks are
//! recorded as `Symlink` entries unless `follow_symlinks` is on.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata};
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use lpk_format::{validate_path, EntryFlags, EntryKind};
use same_file::Handle;

use crate::error::CoreError;
use crate::select::Selection;

/// How a tree is walked.
///
/// With `follow_symlinks` on: (1) a directory reached a second time through a link (a cycle or
/// a DAG of links) is not descended again, the link is recorded as a `Symlink` entry with its
/// target bytes instead (so a directory first reached through a link and later by its real
/// path is listed under both archive paths, bounded by the visited set, as `tar -h` does); (2) a dangling link is recorded as a `Symlink` entry; (3) a junction
/// or directory symlink to the same volume is followed like a directory, one to another volume
/// is recorded as a `Symlink` entry when `one_file_system` is on (the volume is the device id
/// on Unix and the drive prefix of the canonical path on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestOptions {
    /// Follow symlinks and archive what they point to. Default false: a symlink is recorded as
    /// a `Symlink` entry.
    pub follow_symlinks: bool,
    /// Include hidden entries. Default true.
    pub include_hidden: bool,
    /// Do not cross mount points (volumes on Windows). Default true.
    pub one_file_system: bool,
}

impl Default for IngestOptions {
    fn default() -> Self {
        IngestOptions {
            follow_symlinks: false,
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
    /// The link target's bytes, for a symlink. Unix targets are the raw bytes. Windows targets
    /// are stored raw as UTF-8 of the link's target string: backslash separators and
    /// drive-absolute or volume-GUID forms are possible, and refusing them on extraction is the
    /// format crate's job.
    pub symlink_target: Option<Vec<u8>>,
    /// Identity of the file object the walk saw (see [`file_identity`]); `Source::open` refuses
    /// a different object. `None` skips the check. A process-local value, not stable across
    /// runs; file identities on ReFS and on network file systems may not be unique or stable
    /// (the caveat of the `same-file` crate).
    pub identity: Option<u64>,
}

/// Check that an input's archive path is one the format accepts.
pub fn validate_input(input: &Input) -> Result<(), CoreError> {
    validate_path(&input.path).map_err(|reason| CoreError::UnportableName {
        path: input.path.clone(),
        reason: reason.to_string(),
    })
}

fn handle_id(h: &Handle) -> u64 {
    let mut s = DefaultHasher::new();
    h.hash(&mut s);
    s.finish()
}

/// A 64-bit digest of an open file's identity (volume and file index on Windows, device and
/// inode on Unix).
pub fn file_identity(file: &File) -> std::io::Result<u64> {
    #[cfg(unix)]
    {
        Ok(dev_ino_id(&file.metadata()?))
    }
    #[cfg(not(unix))]
    {
        Handle::from_file(file.try_clone()?).map(|h| handle_id(&h))
    }
}

#[cfg(unix)]
fn dev_ino_id(md: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut s = DefaultHasher::new();
    (md.dev(), md.ino()).hash(&mut s);
    s.finish()
}

/// Identity of a file the walk saw: Unix takes (dev, ino) from the metadata already in hand,
/// Windows opens a `same_file::Handle` briefly (no stable std alternative).
fn walked_identity(path: &Path, md: &Metadata) -> Result<u64, CoreError> {
    #[cfg(unix)]
    {
        let _ = path;
        Ok(dev_ino_id(md))
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        path_identity(path)
    }
}

fn path_identity(path: &Path) -> Result<u64, CoreError> {
    Handle::from_path(path)
        .map(|h| handle_id(&h))
        .map_err(|e| CoreError::io(path, e))
}

/// One input of a run (E3-4): the file or directory at `path`, entered under `name` (a
/// directory's entries below `name/`). The empty name is the contents of a directory, not
/// the directory itself (the single-directory form of `lpk a`; [`walk`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// Where the input is read from.
    pub path: PathBuf,
    /// The archive path it is entered under; empty for a directory's contents.
    pub name: String,
}

impl Root {
    /// The contents of the directory at `dir`.
    pub fn contents(dir: &Path) -> Self {
        Root {
            path: dir.to_path_buf(),
            name: String::new(),
        }
    }

    /// The input at `path` under its own name: a relative path is entered as given (`/`
    /// separators, `.` components dropped: `sub/photos` is `sub/photos/...`); an absolute
    /// path, or one leaving the current directory through `..`, is entered under its last
    /// component (`D:\x\photos` is `photos/...`). A path with no name to enter it under (`.`,
    /// `..`, a drive root) is `InvalidOption`; a name the format refuses is `UnportableName`.
    pub fn named(path: &Path) -> Result<Self, CoreError> {
        let mut parts: Vec<String> = Vec::new();
        let mut last_only = false;
        for c in path.components() {
            match c {
                Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    last_only = true;
                }
            }
        }
        if last_only {
            parts = parts.pop().into_iter().collect();
        }
        if parts.is_empty() {
            return Err(CoreError::InvalidOption(format!(
                "{}: no name to enter it under (give a file or a directory by name)",
                path.display()
            )));
        }
        let name = parts.join("/");
        validate_path(&name).map_err(|reason| CoreError::UnportableName {
            path: name.clone(),
            reason: reason.to_string(),
        })?;
        Ok(Root {
            path: path.to_path_buf(),
            name,
        })
    }
}

/// What a run archives: its roots, each under its name, minus the entries `exclude` matches
/// (a matching directory is not walked into).
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// The inputs, in the order given.
    pub roots: Vec<Root>,
    /// Entry paths left out.
    pub exclude: Selection,
}

impl Sources {
    /// The contents of one directory (the single-directory form), nothing excluded.
    pub fn contents_of(dir: &Path) -> Self {
        Sources {
            roots: vec![Root::contents(dir)],
            exclude: Selection::all(),
        }
    }

    /// Several roots: two that would be entered under the same name, or one under the other
    /// (`a` and `a/b`), are refused with `InvalidOption` naming both; a contents root (empty
    /// name) is refused beside any other.
    ///
    /// Also refused: two roots that are the same file-system object (two spellings of one
    /// path, a link and its target; compared by identity, `same_file`, when both can be
    /// opened), and, on a host whose file system is case-insensitive
    /// ([`policy::case_insensitive_target`](crate::policy::case_insensitive_target)), two
    /// names equal under simple case folding (`docs` and `DOCS`, which `lpk x` there could
    /// not tell apart); on a case-sensitive host those stay distinct inputs.
    ///
    /// The checks are hash-based, O(n · depth) over the roots (a list file may name every
    /// file of a tree).
    pub fn new(roots: Vec<Root>, exclude: Selection) -> Result<Self, CoreError> {
        let fold = crate::policy::case_insensitive_target(Path::new("."));
        let key = |name: &str| {
            if fold {
                name.to_lowercase()
            } else {
                name.to_string()
            }
        };
        let both = |a: &Root, b: &Root, what: &str| {
            CoreError::InvalidOption(format!(
                "{} and {}: {what}",
                a.path.display(),
                b.path.display()
            ))
        };
        // Per folded name, the root entered under it; per folded ancestor prefix of a name,
        // the first root whose name has it.
        let mut names: HashMap<String, usize> = HashMap::with_capacity(roots.len());
        let mut prefixes: HashMap<String, usize> = HashMap::new();
        let mut objects: HashMap<u64, usize> = HashMap::with_capacity(roots.len());
        if roots.len() > 1 {
            if let Some(r) = roots.iter().find(|r| r.name.is_empty()) {
                let other = roots.iter().find(|o| !std::ptr::eq(*o, r)).unwrap_or(r);
                return Err(both(
                    r,
                    other,
                    "a directory's contents cannot be archived beside another input",
                ));
            }
        }
        for (i, r) in roots.iter().enumerate() {
            if let Ok(h) = Handle::from_path(&r.path) {
                if let Some(&j) = objects.get(&handle_id(&h)) {
                    return Err(both(&roots[j], r, "the same input given twice"));
                }
                objects.insert(handle_id(&h), i);
            }
            let k = key(&r.name);
            if let Some(&j) = names.get(&k) {
                return Err(both(
                    &roots[j],
                    r,
                    &format!("would both be entered as {:?}", roots[j].name),
                ));
            }
            if let Some(&j) = prefixes.get(&k) {
                return Err(CoreError::InvalidOption(format!(
                    "{} ({:?}) lies under {} ({:?})",
                    roots[j].path.display(),
                    roots[j].name,
                    r.path.display(),
                    r.name
                )));
            }
            for (at, b) in k.bytes().enumerate() {
                if b != b'/' || at == 0 {
                    continue;
                }
                let p = &k[..at];
                if let Some(&j) = names.get(p) {
                    return Err(CoreError::InvalidOption(format!(
                        "{} ({:?}) lies under {} ({:?})",
                        r.path.display(),
                        r.name,
                        roots[j].path.display(),
                        roots[j].name
                    )));
                }
                prefixes.entry(p.to_string()).or_insert(i);
            }
            names.insert(k, i);
        }
        Ok(Sources { roots, exclude })
    }
}

/// Every directory, file and symlink under `root` (not `root` itself), sorted by path bytes.
pub fn walk(root: &Path, options: &IngestOptions) -> Result<Vec<Input>, CoreError> {
    walk_with(root, options, &crate::Monitor::default().reporter())
}

/// [`walk`], reporting a [`Stage::Walk`](crate::Stage::Walk) event per directory listed
/// (`files_done` the entries found so far) and polling the token before each directory.
pub(crate) fn walk_with(
    root: &Path,
    options: &IngestOptions,
    reporter: &crate::progress::Reporter,
) -> Result<Vec<Input>, CoreError> {
    walk_sources_with(&Sources::contents_of(root), options, reporter)
}

/// The inputs of every root of `sources`, each under its name, minus the excluded entries,
/// sorted by path bytes (two roots never give the same path: [`Sources::new`] refuses the
/// names that could). A directory root is an entry itself, then its tree below its name; a
/// file root is one entry (a symlink to a file is read through). The contents root
/// ([`Root::contents`]) must be a directory.
pub fn walk_sources(sources: &Sources, options: &IngestOptions) -> Result<Vec<Input>, CoreError> {
    walk_sources_with(sources, options, &crate::Monitor::default().reporter())
}

/// [`walk_sources`] with the progress reporting of [`walk_with`].
pub(crate) fn walk_sources_with(
    sources: &Sources,
    options: &IngestOptions,
    reporter: &crate::progress::Reporter,
) -> Result<Vec<Input>, CoreError> {
    let mut w = Walker {
        options,
        exclude: &sources.exclude,
        out: Vec::new(),
        visited: HashSet::new(),
        root_volume: None,
    };
    for root in &sources.roots {
        let path = root.path.as_path();
        let md = std::fs::metadata(path).map_err(|e| CoreError::io(path, e))?;
        if root.name.is_empty() {
            if !md.is_dir() {
                return Err(CoreError::NotADirectory {
                    path: path.to_path_buf(),
                });
            }
            w.descend(path, &md, "", reporter)?;
            continue;
        }
        if w.exclude.is_match(&root.name) {
            continue;
        }
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let flags = attribute_flags(&md, &file_name);
        if md.is_dir() {
            w.out.push(Input {
                path: root.name.clone(),
                kind: EntryKind::Directory,
                len: 0,
                mtime_ns: mtime_ns(&md),
                flags: strip_file_only(flags),
                source: path.to_path_buf(),
                symlink_target: None,
                identity: None,
            });
            w.descend(path, &md, &root.name, reporter)?;
        } else if md.is_file() {
            // A root that is a symlink to a file is read through it: the source is the
            // target, as the followed-link case of the walk records it.
            let linked = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
            let source = if linked {
                std::fs::canonicalize(path).map_err(|e| CoreError::io(path, e))?
            } else {
                path.to_path_buf()
            };
            w.out.push(Input {
                path: root.name.clone(),
                kind: EntryKind::File,
                len: md.len(),
                mtime_ns: mtime_ns(&md),
                flags,
                identity: Some(walked_identity(&source, &md)?),
                source,
                symlink_target: None,
            });
            reporter.add(crate::Stage::Walk, 1, 0, 0);
        } else {
            return Err(CoreError::SpecialFile {
                path: path.to_path_buf(),
            });
        }
    }
    reporter.check()?;
    reporter.done(crate::Stage::Walk);
    let mut out = w.out;
    out.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    for pair in out.windows(2) {
        if pair[0].path == pair[1].path {
            return Err(CoreError::DuplicatePath {
                path: pair[0].path.clone(),
            });
        }
    }
    Ok(out)
}

struct Walker<'a> {
    options: &'a IngestOptions,
    exclude: &'a Selection,
    out: Vec<Input>,
    visited: HashSet<u64>,
    root_volume: Option<String>,
}

struct Child {
    name: String,
    path: PathBuf,
    md: Metadata,
}

impl Walker<'_> {
    /// Walk the tree under the directory `root` (whose metadata is `md`), its entries below
    /// `prefix`, reporting per directory listed and polling the token before each.
    fn descend(
        &mut self,
        root: &Path,
        md: &Metadata,
        prefix: &str,
        reporter: &crate::progress::Reporter,
    ) -> Result<(), CoreError> {
        self.root_volume = volume_key(root, md).map_err(|e| CoreError::io(root, e))?;
        if self.options.follow_symlinks {
            self.visited.insert(path_identity(root)?);
        }
        let mut stack = vec![(root.to_path_buf(), prefix.to_string())];
        while let Some((dir, prefix)) = stack.pop() {
            reporter.check()?;
            let before = self.out.len();
            self.list(&dir, &prefix, &mut stack)?;
            reporter.add(crate::Stage::Walk, (self.out.len() - before) as u64, 0, 0);
        }
        Ok(())
    }

    fn list(
        &mut self,
        dir: &Path,
        prefix: &str,
        stack: &mut Vec<(PathBuf, String)>,
    ) -> Result<(), CoreError> {
        let mut children = Vec::new();
        let rd = std::fs::read_dir(dir).map_err(|e| CoreError::io(dir, e))?;
        for e in rd {
            let e = e.map_err(|e| CoreError::io(dir, e))?;
            let path = e.path();
            let rel = join(prefix, &e.file_name().to_string_lossy());
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                return Err(CoreError::UnportableName {
                    path: rel,
                    reason: "not utf-8".to_string(),
                });
            };
            let md = e.metadata().map_err(|e| CoreError::io(&path, e))?;
            children.push(Child { name, path, md });
        }
        if self.options.follow_symlinks {
            // Register the real subdirectories first, so that a link to a sibling directory
            // in this listing is recognised as a second way in.
            for c in &children {
                if c.md.file_type().is_dir() {
                    self.visited.insert(path_identity(&c.path)?);
                }
            }
        }
        for c in children {
            self.emit(prefix, c, stack)?;
        }
        Ok(())
    }

    fn emit(
        &mut self,
        prefix: &str,
        c: Child,
        stack: &mut Vec<(PathBuf, String)>,
    ) -> Result<(), CoreError> {
        let path = join(prefix, &c.name);
        let flags = attribute_flags(&c.md, &c.name);
        // An excluded entry (hidden, or matched by an exclude pattern; a matched directory
        // is not walked into) is never name-validated.
        if !self.options.include_hidden && flags.contains(EntryFlags::HIDDEN) {
            return Ok(());
        }
        if self.exclude.is_match(&path) {
            return Ok(());
        }
        validate_path(&path).map_err(|reason| CoreError::UnportableName {
            path: path.clone(),
            reason: reason.to_string(),
        })?;
        let ft = c.md.file_type();
        if ft.is_dir() {
            // A plain directory is always descended, except across a Unix mount point; no
            // path resolution is attempted that could fail and drop its contents silently.
            let crosses =
                self.options.one_file_system && plain_dir_crosses(&c.md, &self.root_volume);
            self.out.push(dir_input(path.clone(), &c, flags));
            if !crosses {
                stack.push((c.path, path));
            }
            Ok(())
        } else if ft.is_symlink() {
            if !self.options.follow_symlinks {
                return self.push_symlink(path, &c, flags);
            }
            match std::fs::metadata(&c.path) {
                Err(_) => self.push_symlink(path, &c, flags),
                Ok(m) if m.is_dir() => {
                    let other_volume = self.options.one_file_system
                        && self.root_volume.is_some()
                        && volume_key(&c.path, &m).map_err(|e| CoreError::io(&c.path, e))?
                            != self.root_volume;
                    if other_volume || !self.visited.insert(path_identity(&c.path)?) {
                        return self.push_symlink(path, &c, flags);
                    }
                    let flags = attribute_flags(&m, &c.name);
                    self.out.push(Input {
                        path: path.clone(),
                        kind: EntryKind::Directory,
                        len: 0,
                        mtime_ns: mtime_ns(&m),
                        flags: strip_file_only(flags),
                        source: c.path.clone(),
                        symlink_target: None,
                        identity: None,
                    });
                    stack.push((c.path, path));
                    Ok(())
                }
                Ok(m) if m.is_file() => {
                    let source =
                        std::fs::canonicalize(&c.path).map_err(|e| CoreError::io(&c.path, e))?;
                    let flags = attribute_flags(&m, &c.name);
                    self.out.push(Input {
                        path,
                        kind: EntryKind::File,
                        len: m.len(),
                        mtime_ns: mtime_ns(&m),
                        flags,
                        identity: Some(walked_identity(&source, &m)?),
                        source,
                        symlink_target: None,
                    });
                    Ok(())
                }
                Ok(_) => Err(CoreError::SpecialFile { path: c.path }),
            }
        } else if ft.is_file() {
            self.out.push(Input {
                path,
                kind: EntryKind::File,
                len: c.md.len(),
                mtime_ns: mtime_ns(&c.md),
                flags,
                identity: Some(walked_identity(&c.path, &c.md)?),
                source: c.path,
                symlink_target: None,
            });
            Ok(())
        } else {
            Err(CoreError::SpecialFile { path: c.path })
        }
    }

    fn push_symlink(
        &mut self,
        path: String,
        c: &Child,
        flags: EntryFlags,
    ) -> Result<(), CoreError> {
        let target = std::fs::read_link(&c.path).map_err(|e| CoreError::io(&c.path, e))?;
        let target = link_bytes(&target).ok_or_else(|| CoreError::UnportableName {
            path: path.clone(),
            reason: "symlink target not valid UTF-16".to_string(),
        })?;
        self.out.push(Input {
            path,
            kind: EntryKind::Symlink,
            len: target.len() as u64,
            mtime_ns: mtime_ns(&c.md),
            flags: strip_file_only(flags),
            source: c.path.clone(),
            symlink_target: Some(target),
            identity: None,
        });
        Ok(())
    }
}

fn dir_input(path: String, c: &Child, flags: EntryFlags) -> Input {
    Input {
        path,
        kind: EntryKind::Directory,
        len: 0,
        mtime_ns: mtime_ns(&c.md),
        flags: strip_file_only(flags),
        source: c.path.clone(),
        symlink_target: None,
        identity: None,
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
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

fn mtime_ns(md: &Metadata) -> i64 {
    let Ok(t) = md.modified() else {
        return i64::MIN;
    };
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    }
}

#[cfg(unix)]
fn link_bytes(p: &Path) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Some(p.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn link_bytes(p: &Path) -> Option<Vec<u8>> {
    p.to_str().map(|s| s.as_bytes().to_vec())
}

#[cfg(unix)]
fn volume_key(_path: &Path, md: &Metadata) -> std::io::Result<Option<String>> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some(md.dev().to_string()))
}

#[cfg(windows)]
fn volume_key(path: &Path, _md: &Metadata) -> std::io::Result<Option<String>> {
    use std::path::Component;
    let canon = std::fs::canonicalize(path)?;
    Ok(match canon.components().next() {
        Some(Component::Prefix(p)) => Some(p.as_os_str().to_string_lossy().to_uppercase()),
        _ => None,
    })
}

#[cfg(not(any(unix, windows)))]
fn volume_key(_path: &Path, _md: &Metadata) -> std::io::Result<Option<String>> {
    Ok(None)
}

/// Unix: a plain directory on another device is a mount point and is not descended. Windows:
/// mount points are reparse points, which Rust reports as symlinks, so a plain directory
/// never crosses.
#[cfg(unix)]
fn plain_dir_crosses(md: &Metadata, root_volume: &Option<String>) -> bool {
    use std::os::unix::fs::MetadataExt;
    root_volume
        .as_ref()
        .is_some_and(|v| *v != md.dev().to_string())
}

#[cfg(not(unix))]
fn plain_dir_crosses(_md: &Metadata, _root_volume: &Option<String>) -> bool {
    false
}

#[cfg(windows)]
fn attribute_flags(md: &Metadata, _name: &str) -> EntryFlags {
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
fn attribute_flags(md: &Metadata, name: &str) -> EntryFlags {
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
fn attribute_flags(_md: &Metadata, _name: &str) -> EntryFlags {
    EntryFlags::EMPTY
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    const MIB: usize = 1 << 20;

    /// A test that cannot run says so, and fails when `CI` is set so CI never passes vacuously.
    pub(crate) fn skip(why: &str) {
        eprintln!("skipped: {why}");
        assert!(
            std::env::var_os("CI").is_none(),
            "test skipped under CI: {why}"
        );
    }

    pub(crate) fn write(root: &Path, rel: &str, len: usize) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
        fs::write(p, data).unwrap();
    }

    pub(crate) fn clear_readonly(p: &Path) {
        let mut perm = fs::metadata(p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(p, perm).unwrap();
    }

    /// A file symlink; false when the platform refuses.
    pub(crate) fn file_link(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
    }

    /// A directory link (a symlink; on Windows a junction when symlinks are refused).
    pub(crate) fn dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).is_ok() || junction(target, link)
        }
    }

    #[cfg(windows)]
    pub(crate) fn junction(target: &Path, link: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(target.to_string_lossy().replace('/', "\\"))
            .output()
            .is_ok_and(|o| o.status.success())
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

    pub(crate) fn find<'a>(v: &'a [Input], p: &str) -> &'a Input {
        v.iter().find(|i| i.path == p).unwrap()
    }

    pub(crate) fn plain_input(path: &str, kind: EntryKind) -> Input {
        Input {
            path: path.to_string(),
            kind,
            len: 0,
            mtime_ns: 0,
            flags: EntryFlags::EMPTY,
            source: PathBuf::new(),
            symlink_target: None,
            identity: None,
        }
    }

    /// E3-4: the naming rule of an input given by path.
    #[test]
    fn a_root_is_named_by_its_relative_path_or_its_last_component() {
        let name = |p: &str| Root::named(Path::new(p)).map(|r| r.name);
        assert_eq!(name("notes.txt").unwrap(), "notes.txt");
        assert_eq!(name("sub/photos").unwrap(), "sub/photos");
        assert_eq!(name("./sub/./photos/").unwrap(), "sub/photos");
        assert_eq!(name("../x/photos").unwrap(), "photos");
        assert_eq!(name("a/../photos").unwrap(), "photos");
        #[cfg(windows)]
        {
            assert_eq!(name(r"D:\x\photos").unwrap(), "photos");
            assert_eq!(name(r"sub\photos").unwrap(), "sub/photos");
            assert!(matches!(name(r"C:\"), Err(CoreError::InvalidOption(_))));
        }
        #[cfg(unix)]
        assert_eq!(name("/x/photos").unwrap(), "photos");
        for p in [".", "..", "./"] {
            assert!(matches!(name(p), Err(CoreError::InvalidOption(_))), "{p}");
        }
        // A name the format refuses (a NUL byte) is an unportable name.
        assert!(matches!(
            name("a\u{0}b"),
            Err(CoreError::UnportableName { .. })
        ));
        // A path too long for the format (the limit is the format's, not the OS's).
        let long = "x".repeat(70_000);
        assert!(matches!(name(&long), Err(CoreError::UnportableName { .. })));
    }

    /// E3-4: two inputs under the same name, or one under the other, are refused naming both.
    #[test]
    fn conflicting_roots_are_refused_naming_both() {
        let r = |p: &str| Root::named(Path::new(p)).unwrap();
        let err = |roots: Vec<Root>| match Sources::new(roots, Selection::all()) {
            Err(CoreError::InvalidOption(m)) => m,
            other => panic!("{other:?}"),
        };
        let m = err(vec![r("docs"), r("../y/docs")]);
        assert!(
            m.contains("docs") && m.contains("y/docs") && m.contains("\"docs\""),
            "{m}"
        );
        // Different directories whose names differ are no conflict.
        assert!(Sources::new(vec![r("x/docs"), r("../y/docs")], Selection::all()).is_ok());
        let m = err(vec![r("docs/b.md"), r("docs")]);
        assert!(m.contains("docs/b.md") && m.contains("lies under"), "{m}");
        let m = err(vec![Root::contents(Path::new("d")), r("a.txt")]);
        assert!(m.contains("contents"), "{m}");
        // Different names that share a prefix string are fine.
        let s = Sources::new(vec![r("docs"), r("docs2"), r("doc")], Selection::all()).unwrap();
        assert_eq!(s.roots.len(), 3);
        // A prefix arriving after the name below it is caught too.
        let m = err(vec![r("a/b/c.txt"), r("x"), r("a/b")]);
        assert!(m.contains("lies under") && m.contains("a/b/c.txt"), "{m}");
    }

    /// Fix round 1 (a): two roots that are one file-system object (two spellings) are a
    /// duplicate input on every platform; (b) on a case-insensitive host two names equal
    /// under case folding are refused as well, on a case-sensitive one they stay distinct.
    #[test]
    fn the_same_object_twice_is_refused_and_case_folds_on_a_case_insensitive_host() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("docs")).unwrap();
        write(dir.path(), "notes.txt", 3);
        let spelled = |p: &str| Root {
            path: dir.path().join(p),
            name: p.to_string(),
        };
        let err = |roots: Vec<Root>| match Sources::new(roots, Selection::all()) {
            Err(CoreError::InvalidOption(m)) => m,
            other => panic!("{other:?}"),
        };
        // One object under two names: `docs` and `./docs` (a different name string).
        let m = err(vec![
            spelled("docs"),
            Root {
                path: dir.path().join("./docs"),
                name: "d2".to_string(),
            },
        ]);
        assert!(m.contains("given twice") && m.contains("docs"), "{m}");
        // A file through the same object.
        let m = err(vec![
            spelled("notes.txt"),
            Root {
                path: dir.path().join("./notes.txt"),
                name: "n2".to_string(),
            },
        ]);
        assert!(m.contains("given twice"), "{m}");
        // Case: refused where the host folds, allowed where it does not.
        let pair = || {
            vec![
                spelled("docs"),
                Root {
                    path: dir.path().join("other"),
                    name: "DOCS".to_string(),
                },
            ]
        };
        if crate::policy::case_insensitive_target(dir.path()) {
            let m = err(pair());
            assert!(m.contains("would both be entered"), "{m}");
            let m = err(vec![
                spelled("docs"),
                Root {
                    path: dir.path().join("other2"),
                    name: "DOCS/x.txt".to_string(),
                },
            ]);
            assert!(m.contains("lies under"), "{m}");
        } else {
            assert!(Sources::new(pair(), Selection::all()).is_ok());
        }
        // Paths that do not exist are not identity-checked (the walk reports them).
        assert!(Sources::new(
            vec![spelled("missing-a"), spelled("missing-b")],
            Selection::all()
        )
        .is_ok());
    }

    /// Fix round 1: the conflict check over a list-file-sized input set (20,000 roots) runs,
    /// and still catches a duplicate and a prefix among them.
    #[test]
    fn twenty_thousand_roots_are_checked_and_conflicts_among_them_found() {
        let r = |p: String| Root {
            path: PathBuf::from(&p),
            name: p,
        };
        let many = || -> Vec<Root> {
            (0..20_000)
                .map(|i| {
                    r(format!(
                        "tree/dir{:03}/sub{:02}/file{i:05}.txt",
                        i % 200,
                        i % 37
                    ))
                })
                .collect()
        };
        let s = Sources::new(many(), Selection::all()).unwrap();
        assert_eq!(s.roots.len(), 20_000);
        let mut dup = many();
        dup.push(r("tree/dir017/sub17/file00017.txt".to_string()));
        let e = Sources::new(dup, Selection::all()).unwrap_err();
        assert!(e.to_string().contains("would both be entered"), "{e}");
        let mut over = many();
        over.push(r("tree/dir100".to_string()));
        let e = Sources::new(over, Selection::all()).unwrap_err();
        assert!(e.to_string().contains("lies under"), "{e}");
        let mut under = vec![r("tree/dir050/sub13".to_string())];
        under.extend(many());
        let e = Sources::new(under, Selection::all()).unwrap_err();
        assert!(e.to_string().contains("lies under"), "{e}");
    }

    /// E3-4: several roots, each under its name; a directory root is an entry itself; the
    /// exclude prunes a subtree and leaves out matching files; the contents root is unchanged.
    #[test]
    fn several_roots_walk_under_their_names_and_excludes_prune() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        let expect = make_tree(&tree);
        write(dir.path(), "notes.txt", 4);
        let roots = vec![
            Root::named(&tree).unwrap(),
            Root::named(&dir.path().join("notes.txt")).unwrap(),
        ];
        let sources = Sources::new(roots.clone(), Selection::all()).unwrap();
        let got = walk_sources(&sources, &IngestOptions::default()).unwrap();
        let mut want: Vec<String> = expect.iter().map(|p| format!("tree/{p}")).collect();
        want.push("tree".to_string());
        want.push("notes.txt".to_string());
        want.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        let paths: Vec<&str> = got.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, want);
        assert_eq!(find(&got, "tree").kind, EntryKind::Directory);
        assert_eq!(find(&got, "notes.txt").kind, EntryKind::File);
        assert_eq!(find(&got, "notes.txt").len, 4);
        assert!(find(&got, "notes.txt").identity.is_some());
        // The same inputs listed once more give the same paths (the walk is stable; the
        // directory mtimes are not compared, NTFS settles them lazily).
        let again = walk_sources(&sources, &IngestOptions::default()).unwrap();
        let again: Vec<&str> = again.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(again, paths);
        // Excludes: a directory match prunes its subtree, `**` reaches every depth, and a
        // root's own name is subject to it too.
        let exclude = Selection::new(&["tree/a/b", "**/*.bin", "notes.txt"]).unwrap();
        let sources = Sources::new(roots, exclude).unwrap();
        let got = walk_sources(&sources, &IngestOptions::default()).unwrap();
        let paths: Vec<&str> = got.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"tree/a"), "{paths:?}");
        assert!(
            !paths.iter().any(|p| p.starts_with("tree/a/b")),
            "{paths:?}"
        );
        assert!(!paths.iter().any(|p| p.ends_with(".bin")), "{paths:?}");
        assert!(!paths.contains(&"notes.txt"), "{paths:?}");
        assert!(paths.contains(&"tree/ro.txt"), "{paths:?}");
        // The contents root with the exclude: the same tree minus the matches, no prefix.
        let sources = Sources {
            roots: vec![Root::contents(&tree)],
            exclude: Selection::new(&["a/b", "**/*.bin"]).unwrap(),
        };
        let got = walk_sources(&sources, &IngestOptions::default()).unwrap();
        let paths: Vec<&str> = got.iter().map(|i| i.path.as_str()).collect();
        let want: Vec<&str> = expect
            .iter()
            .map(String::as_str)
            .filter(|p| !p.starts_with("a/b") && !p.ends_with(".bin"))
            .collect();
        assert_eq!(paths, want);
        // A contents root that is a file, and a named root that is missing.
        let e = walk_sources(
            &Sources::contents_of(&dir.path().join("notes.txt")),
            &IngestOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(e, CoreError::NotADirectory { .. }), "{e}");
        let e = walk_sources(
            &Sources::new(
                vec![Root::named(&dir.path().join("missing")).unwrap()],
                Selection::all(),
            )
            .unwrap(),
            &IngestOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(e, CoreError::Io { .. }), "{e}");
        clear_readonly(&tree.join("ro.txt"));
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
            assert!(i.identity.is_some());
        }
        let ro = find(&got, "ro.txt");
        #[cfg(windows)]
        assert!(ro.flags.contains(EntryFlags::READ_ONLY));
        #[cfg(unix)]
        assert!(!ro.flags.contains(EntryFlags::EXECUTABLE));
        assert!(!find(&got, "one.bin").flags.contains(EntryFlags::READ_ONLY));
        clear_readonly(&ro.source);
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
            match validate_input(&plain_input(path, EntryKind::File)) {
                Err(CoreError::UnportableName { path: p, reason: r }) => {
                    assert_eq!(p, path);
                    assert_eq!(r, reason);
                }
                other => panic!("{other:?}"),
            }
        }
        let long = plain_input(&"x".repeat(70_000), EntryKind::File);
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
    fn unix_special_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let ok = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            skip("mkfifo unavailable");
            return;
        }
        assert!(matches!(
            walk(dir.path(), &IngestOptions::default()),
            Err(CoreError::SpecialFile { .. })
        ));
    }

    #[test]
    fn a_directory_whose_path_cannot_be_canonicalized_is_still_descended() {
        // Windows: `canonicalize` fails on a name ending in a dot (reachable only through a
        // verbatim path); the contents must still be listed. Elsewhere this just passes.
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(root.join("foo.")).unwrap();
        fs::write(root.join("foo.").join("f"), b"x").unwrap();
        let got = walk(&root, &IngestOptions::default()).unwrap();
        let paths: Vec<&str> = got.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["foo.", "foo./f"]);
    }

    #[cfg(unix)]
    #[test]
    fn unix_excluded_hidden_entry_is_not_name_validated() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".bad\\name", 1);
        write(dir.path(), "ok", 1);
        let opts = IngestOptions {
            include_hidden: false,
            ..IngestOptions::default()
        };
        let got = walk(dir.path(), &opts).unwrap();
        assert_eq!(got.len(), 1);
        assert!(walk(dir.path(), &IngestOptions::default()).is_err());
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

    #[cfg(unix)]
    #[test]
    fn unix_unreadable_directory_is_io_with_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "locked/f", 1);
        let locked = dir.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let can_read = fs::read_dir(&locked).is_ok();
        let r = walk(dir.path(), &IngestOptions::default());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if can_read {
            skip("running with privileges that ignore directory modes");
            return;
        }
        match r {
            Err(CoreError::Io { path, .. }) => assert!(path.ends_with("locked")),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(windows)]
    fn attrib(path: &Path, flags: &[&str]) -> bool {
        std::process::Command::new("attrib")
            .args(flags)
            .arg(path)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[cfg(windows)]
    #[test]
    fn windows_hidden_and_system_attributes() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "plain.txt", 1);
        write(dir.path(), "secret.txt", 1);
        if !attrib(&dir.path().join("secret.txt"), &["+h", "+s"]) {
            skip("attrib is not available");
            return;
        }
        let all = walk(dir.path(), &IngestOptions::default()).unwrap();
        let s = find(&all, "secret.txt");
        assert!(s.flags.contains(EntryFlags::HIDDEN));
        assert!(s.flags.contains(EntryFlags::SYSTEM));
        assert!(!find(&all, "plain.txt").flags.contains(EntryFlags::HIDDEN));
        let opts = IngestOptions {
            include_hidden: false,
            ..IngestOptions::default()
        };
        let some = walk(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = some.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["plain.txt"]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_junction_is_a_symlink_entry_and_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "real/f.txt", 3);
        let link = dir.path().join("junc");
        if !junction(&dir.path().join("real"), &link) {
            skip("mklink /J is not available");
            return;
        }
        let got = walk(dir.path(), &IngestOptions::default()).unwrap();
        assert_eq!(find(&got, "junc").kind, EntryKind::Symlink);
        assert!(got.iter().all(|i| !i.path.starts_with("junc/")));
        fs::remove_dir(&link).unwrap();
    }

    #[test]
    fn symlinks_are_recorded_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "real.txt", 4);
        if !file_link(Path::new("real.txt"), &dir.path().join("link")) {
            skip("cannot create symlinks without privilege");
            return;
        }
        let got = walk(dir.path(), &IngestOptions::default()).unwrap();
        let l = find(&got, "link");
        assert_eq!(l.kind, EntryKind::Symlink);
        assert_eq!(l.symlink_target.as_deref(), Some(&b"real.txt"[..]));
        assert_eq!(l.len, 8);
        assert_eq!(find(&got, "real.txt").kind, EntryKind::File);
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
    fn a_dangling_link_is_recorded_when_following() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f", 1);
        if !file_link(Path::new("nowhere"), &dir.path().join("dangling")) {
            skip("cannot create symlinks without privilege");
            return;
        }
        let opts = IngestOptions {
            follow_symlinks: true,
            ..IngestOptions::default()
        };
        let got = walk(dir.path(), &opts).unwrap();
        assert_eq!(find(&got, "dangling").kind, EntryKind::Symlink);
    }

    #[test]
    fn a_cycle_through_a_followed_link_is_recorded_as_a_link() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "d/file", 1);
        if !dir_link(dir.path(), &dir.path().join("d/loop")) {
            skip("cannot create directory links");
            return;
        }
        let opts = IngestOptions {
            follow_symlinks: true,
            ..IngestOptions::default()
        };
        let got = walk(dir.path(), &opts).unwrap();
        assert_eq!(find(&got, "d/loop").kind, EntryKind::Symlink);
        assert_eq!(find(&got, "d/file").kind, EntryKind::File);
        assert_eq!(got.len(), 3);
        #[cfg(windows)]
        let _ = fs::remove_dir(dir.path().join("d/loop"));
    }

    #[test]
    fn a_dag_of_links_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut nested = PathBuf::new();
        for i in 0..=20 {
            nested.push(format!("d{i}"));
            fs::create_dir_all(dir.path().join(&nested)).unwrap();
        }
        let mut cur = PathBuf::new();
        let mut links = Vec::new();
        for i in 0..20 {
            cur.push(format!("d{i}"));
            let target = dir.path().join(&cur).join(format!("d{}", i + 1));
            for name in ["a", "b"] {
                let link = dir.path().join(&cur).join(name);
                if !dir_link(&target, &link) {
                    skip("cannot create directory links");
                    return;
                }
                links.push(link);
            }
        }
        let opts = IngestOptions {
            follow_symlinks: true,
            ..IngestOptions::default()
        };
        let t = std::time::Instant::now();
        let got = walk(dir.path(), &opts).unwrap();
        assert!(t.elapsed().as_secs() < 10);
        let n_links = got.iter().filter(|i| i.kind == EntryKind::Symlink).count();
        let n_dirs = got
            .iter()
            .filter(|i| i.kind == EntryKind::Directory)
            .count();
        assert_eq!((n_dirs, n_links), (21, 40));
        #[cfg(windows)]
        for l in links {
            let _ = fs::remove_dir(l);
        }
        #[cfg(not(windows))]
        drop(links);
    }
}
