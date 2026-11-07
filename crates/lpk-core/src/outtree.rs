//! The output tree of an extraction (E6-6; `docs/notes/e6-design.md` section 5): every
//! directory, file and link both writers (`.lpk` extraction and the foreign-archive broker)
//! create, every reopen, every cleanup and every replacement goes through a [`Tree`].
//!
//! **Windows.** The destination is opened once, as chosen (it may be reached through a link
//! the user picked). Every directory below it is opened or created one component at a time
//! relative to its verified parent's handle (`lpk_fs_sys`: `NtCreateFile` with a root
//! directory handle, `FILE_OPEN_REPARSE_POINT`, `OBJ_DONT_REPARSE`, `OBJ_CASE_INSENSITIVE`); a
//! name-surrogate reparse tag met on the way (a symlink, a junction, a mount point) is refused
//! as D-54's "linked parent", other tags (cloud placeholders, dedup) pass. The verified
//! directory handles are held without `FILE_SHARE_DELETE` while the tree lives, so no verified
//! directory can be renamed or replaced under the extraction; past [`MAX_HELD`] the oldest is
//! closed, and reopening it (again component by component) compares its file ID with the one
//! recorded. Files are created with `FILE_CREATE` relative to the held parent; reopens,
//! cleanup and a hard link's source are opened relative to the same parent and checked
//! against the file ID recorded at creation; a replaced target is removed through a handle
//! opened relative to the parent (a link as a link, never a directory). Symlinks are created
//! on a node made relative to the held parent, with `FSCTL_SET_REPARSE_POINT`, only when the
//! process token holds `SeCreateSymbolicLinkPrivilege` ([`can_create_symlinks`]; the policy
//! pass skips links otherwise).
//!
//! **Elsewhere** the tree keeps the path code of before (no `openat2` in Phase 1): the plan's
//! linked-parent check is the guard, made once before writing.
//!
//! The plan-time checks remain the refusals before output (D-79); on Windows the relative
//! opens make them binding.

use std::fs::File;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicBool, Ordering};

use crate::extract::Link;
use crate::motw::Zone;
use crate::CoreError;

/// Directory handles held at once on Windows; past this the oldest is closed and its reopening
/// compares the file ID.
pub const MAX_HELD: usize = 4096;

/// A file this tree created: what a reopen, a cleanup or a copy needs.
#[derive(Debug, Clone)]
pub(crate) struct Created {
    /// The full path (for messages; never opened by it on Windows).
    pub(crate) path: PathBuf,
    #[cfg(windows)]
    id: lpk_fs_sys::FileId,
}

/// Whether symlink entries can be created by this process: on Windows the token holds
/// `SeCreateSymbolicLinkPrivilege` (Developer Mode alone does not count); elsewhere always.
/// A test build may force the answer per thread ([`seam::force_symlink_privilege`]).
pub fn can_create_symlinks() -> bool {
    #[cfg(test)]
    if let Some(v) = seam::forced_symlink_privilege() {
        return v;
    }
    #[cfg(windows)]
    {
        lpk_fs_sys::symlink_privilege_held()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// [`MAX_HELD`], or a test's smaller cap.
#[cfg_attr(not(windows), allow(dead_code))]
fn max_held() -> usize {
    #[cfg(test)]
    if let Some(n) = seam::forced_max_held() {
        return n;
    }
    MAX_HELD
}

fn io(path: &Path, e: std::io::Error) -> CoreError {
    CoreError::io(path, e)
}

/// The output tree of one extraction (see the module documentation).
#[derive(Debug)]
pub(crate) struct Tree {
    root: PathBuf,
    #[cfg(windows)]
    win: win::Held,
    /// Mark of the Web (E6-5, [`crate::motw`]): the zone every created file gets before its
    /// first byte, if any.
    zone: Option<Zone>,
    /// Set when a file could not carry the zone (no named streams, or the write failed).
    unmarked: AtomicBool,
}

impl Tree {
    /// Create (or reuse) the destination `dest` and every directory of `dirs` (full paths
    /// below `dest`, as the plan built them; `dest` itself may be among them), in order.
    pub(crate) fn create(dest: &Path, dirs: &[PathBuf]) -> Result<Tree, CoreError> {
        std::fs::create_dir_all(dest).map_err(|e| io(dest, e))?;
        #[cfg(windows)]
        let tree = Tree {
            root: dest.to_path_buf(),
            win: win::Held::open(dest)?,
            zone: None,
            unmarked: AtomicBool::new(false),
        };
        #[cfg(not(windows))]
        let tree = Tree {
            root: dest.to_path_buf(),
            zone: None,
            unmarked: AtomicBool::new(false),
        };
        for d in dirs {
            tree.make_dir(d)?;
        }
        Ok(tree)
    }

    /// Give every file created from now on the `Zone.Identifier` stream of `zone` (Mark of
    /// the Web, E6-5), written relative to the new file's handle before its first byte. The
    /// destination's volume is probed for named streams once, here: every output is on that
    /// volume (a link or a mount point below the destination is refused). A volume without
    /// them, or a failed probe, marks nothing and sets [`Tree::unmarked`] (one warning).
    pub(crate) fn set_zone(&mut self, zone: Option<Zone>) {
        self.zone = zone.filter(|_| {
            let ok = self.holds_streams();
            if !ok {
                self.unmarked.store(true, Ordering::Relaxed);
            }
            ok
        });
    }

    /// Whether the destination's volume holds named streams (probed through its held handle).
    fn holds_streams(&self) -> bool {
        #[cfg(test)]
        if let Some(v) = crate::motw::seam::forced_streams() {
            return v;
        }
        #[cfg(windows)]
        {
            self.win.root_streams()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Whether a file was created that could not carry the zone (the caller's one warning).
    pub(crate) fn unmarked(&self) -> bool {
        self.unmarked.load(Ordering::Relaxed)
    }

    /// Mark a file just created (still empty) with the tree's zone, if any.
    fn mark(&self, f: &File) {
        if let Some(z) = self.zone {
            if !crate::motw::write(f, z) {
                self.unmarked.store(true, Ordering::Relaxed);
            }
        }
    }

    /// `path` relative to the destination, as components.
    fn rel<'p>(&self, path: &'p Path) -> Result<&'p Path, CoreError> {
        path.strip_prefix(&self.root)
            .map_err(|_| CoreError::Extract("an output path outside the destination"))
    }

    fn make_dir(&self, d: &Path) -> Result<(), CoreError> {
        let rel = self.rel(d)?;
        #[cfg(windows)]
        {
            self.win.dir(&self.root, rel, true).map(drop)
        }
        #[cfg(not(windows))]
        {
            let _ = rel;
            std::fs::create_dir_all(d).map_err(|e| io(d, e))
        }
    }

    /// Create the plan's symlinks, in entry order (after the directories, before the files).
    /// Every target here passed [`crate::policy::resolve_symlink_target`] and the privilege
    /// check in the policy pass.
    pub(crate) fn make_links(&self, links: &[Link]) -> Result<(), CoreError> {
        for l in links {
            #[cfg(test)]
            crate::extract::seam::record(&l.target);
            self.make_link(l)?;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn make_link(&self, l: &Link) -> Result<(), CoreError> {
        let target = std::str::from_utf8(&l.target).map_err(|_| {
            io(
                &l.path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "symlink target is not UTF-8",
                ),
            )
        })?;
        let target = crate::extract::windows_link_target(target);
        let (parent, name) = self
            .win
            .parent_of(&self.root, self.rel(&l.path)?, &l.path)?;
        if l.replace {
            win::remove_existing(&parent, name, &l.path)?;
        }
        parent
            .symlink(name, &target, l.dir)
            .map_err(|e| win::fs_err(&l.path, e))
    }

    #[cfg(unix)]
    fn make_link(&self, l: &Link) -> Result<(), CoreError> {
        use std::os::unix::ffi::OsStrExt;
        // A Unix link has no kind: the plan's `dir` (from the archive) matters on Windows only.
        let _ = l.dir;
        if l.replace {
            crate::policy::remove_existing(&l.path).map_err(|e| io(&l.path, e))?;
        }
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&l.target), &l.path)
            .map_err(|e| io(&l.path, e))
    }

    #[cfg(not(any(unix, windows)))]
    fn make_link(&self, l: &Link) -> Result<(), CoreError> {
        let _ = l.dir;
        Err(io(
            &l.path,
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlinks are not supported on this platform",
            ),
        ))
    }

    /// Create a file at `path` (create-new; what is there is removed first when `replace`,
    /// a link as a link, never a directory).
    pub(crate) fn create_file(
        &self,
        path: &Path,
        replace: bool,
    ) -> Result<(File, Created), CoreError> {
        #[cfg(windows)]
        {
            let (parent, name) = self.win.parent_of(&self.root, self.rel(path)?, path)?;
            if replace {
                win::remove_existing(&parent, name, path)?;
            }
            #[cfg(test)]
            seam::before_create(path);
            let (f, id) = parent.create_file(name).map_err(|e| win::fs_err(path, e))?;
            self.mark(&f);
            Ok((
                f,
                Created {
                    path: path.to_path_buf(),
                    id,
                },
            ))
        }
        #[cfg(not(windows))]
        {
            self.rel(path)?;
            if replace {
                crate::policy::remove_existing(path).map_err(|e| io(path, e))?;
            }
            #[cfg(test)]
            seam::before_create(path);
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| io(path, e))?;
            self.mark(&f);
            Ok((
                f,
                Created {
                    path: path.to_path_buf(),
                },
            ))
        }
    }

    /// Reopen a file this tree created, for writing (without create, without truncation).
    pub(crate) fn reopen(&self, c: &Created) -> Result<File, CoreError> {
        self.open_created(c, true)
    }

    /// Open a file this tree created (and finished), for reading.
    pub(crate) fn open_read(&self, c: &Created) -> Result<File, CoreError> {
        self.open_created(c, false)
    }

    fn open_created(&self, c: &Created, write: bool) -> Result<File, CoreError> {
        #[cfg(windows)]
        {
            let (parent, name) = self
                .win
                .parent_of(&self.root, self.rel(&c.path)?, &c.path)?;
            parent
                .open_file(name, c.id, write)
                .map_err(|e| win::fs_err(&c.path, e))
        }
        #[cfg(not(windows))]
        {
            std::fs::OpenOptions::new()
                .read(!write)
                .write(write)
                .open(&c.path)
                .map_err(|e| io(&c.path, e))
        }
    }

    /// Remove a file this tree created and did not finish (best effort: cleanup after a
    /// failure). On Windows only the file with the recorded ID is removed.
    pub(crate) fn remove(&self, c: &Created) {
        #[cfg(windows)]
        {
            if let Ok(rel) = self.rel(&c.path) {
                if let Ok((parent, name)) = self.win.parent_of(&self.root, rel, &c.path) {
                    let _ = parent.remove(name, Some(c.id));
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = std::fs::remove_file(&c.path);
        }
    }
}

#[cfg(windows)]
mod win {
    use std::collections::{HashMap, VecDeque};
    use std::ffi::OsStr;
    use std::path::{Component, Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use lpk_fs_sys::{Dir, FileId, FsError};

    use crate::CoreError;

    /// An `lpk_fs_sys` error at `path` as the engine names it: a link on the way is D-54's
    /// "linked parent"; a directory or file that changed under the extraction an I/O error.
    pub(super) fn fs_err(path: &Path, e: FsError) -> CoreError {
        match e {
            FsError::Linked => crate::policy::refuse_linked_parent(path)
                .err()
                .map_or(CoreError::Extract("linked parent"), CoreError::from),
            FsError::Changed => CoreError::io(
                path,
                std::io::Error::other("replaced during the extraction (file ID changed)"),
            ),
            FsError::Io(e) => CoreError::io(path, e),
        }
    }

    /// Remove what is at `name` in `parent` so an entry can be written there: a link as a
    /// link, a file as a file; a directory is an error (`AlreadyExists`).
    pub(super) fn remove_existing(
        parent: &Dir,
        name: &OsStr,
        path: &Path,
    ) -> Result<(), CoreError> {
        match parent.remove(name, None) {
            Ok(()) => Ok(()),
            Err(FsError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(fs_err(path, e)),
        }
    }

    #[derive(Debug, Default)]
    struct Cache {
        /// The held directories by relative path (the destination is the empty path).
        open: HashMap<PathBuf, Arc<Dir>>,
        /// Held directories in the order they were opened (the destination is never closed).
        order: VecDeque<PathBuf>,
        /// The file ID of every directory verified so far, held or not.
        ids: HashMap<PathBuf, FileId>,
    }

    /// The verified directory handles of a tree.
    #[derive(Debug)]
    pub(super) struct Held {
        cache: Mutex<Cache>,
    }

    impl Held {
        pub(super) fn open(dest: &Path) -> Result<Held, CoreError> {
            let root = Dir::open_path(dest).map_err(|e| fs_err(dest, e))?;
            let mut cache = Cache::default();
            cache.ids.insert(PathBuf::new(), root.id());
            cache.open.insert(PathBuf::new(), Arc::new(root));
            Ok(Held {
                cache: Mutex::new(cache),
            })
        }

        /// The held directory at `rel` (components below the destination), opened or (with
        /// `create`) created one component at a time from the nearest held ancestor.
        pub(super) fn dir(
            &self,
            root: &Path,
            rel: &Path,
            create: bool,
        ) -> Result<Arc<Dir>, CoreError> {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| CoreError::Extract("the output tree's lock"))?;
            if let Some(d) = cache.open.get(rel) {
                return Ok(Arc::clone(d));
            }
            let names: Vec<&OsStr> = rel
                .components()
                .map(|c| match c {
                    Component::Normal(n) => Ok(n),
                    _ => Err(CoreError::Extract(
                        "an output path with a non-normal component",
                    )),
                })
                .collect::<Result<_, _>>()?;
            // The nearest held ancestor (the destination at worst).
            let mut at = names.len();
            let mut cur = loop {
                let p: PathBuf = names[..at].iter().collect();
                if let Some(d) = cache.open.get(&p) {
                    break Arc::clone(d);
                }
                if at == 0 {
                    return Err(CoreError::Extract("the destination is not held"));
                }
                at -= 1;
            };
            let mut full = root.to_path_buf();
            for n in &names[..at] {
                full.push(n);
            }
            for i in at..names.len() {
                full.push(names[i]);
                #[cfg(test)]
                super::seam::before_open_dir(&full);
                let d = cur
                    .open_dir(names[i], create)
                    .map_err(|e| fs_err(&full, e))?;
                let key: PathBuf = names[..=i].iter().collect();
                match cache.ids.get(&key) {
                    Some(&id) if id != d.id() => return Err(fs_err(&full, FsError::Changed)),
                    Some(_) => {}
                    None => {
                        cache.ids.insert(key.clone(), d.id());
                    }
                }
                let d = Arc::new(d);
                cache.open.insert(key.clone(), Arc::clone(&d));
                cache.order.push_back(key);
                while cache.order.len() > super::max_held() {
                    if let Some(old) = cache.order.pop_front() {
                        cache.open.remove(&old);
                    }
                }
                cur = d;
            }
            Ok(cur)
        }

        /// Whether the destination's volume holds named streams (a failed probe: no).
        pub(super) fn root_streams(&self) -> bool {
            let root = self
                .cache
                .lock()
                .ok()
                .and_then(|c| c.open.get(Path::new("")).cloned());
            root.is_some_and(|d| d.named_streams().unwrap_or(false))
        }

        /// The held parent of `rel` and its last component.
        pub(super) fn parent_of<'r>(
            &self,
            root: &Path,
            rel: &'r Path,
            full: &Path,
        ) -> Result<(Arc<Dir>, &'r OsStr), CoreError> {
            let name = rel
                .file_name()
                .ok_or_else(|| CoreError::io(full, std::io::Error::other("no file name")))?;
            let parent = rel.parent().unwrap_or(Path::new(""));
            Ok((self.dir(root, parent, false)?, name))
        }
    }
}

/// Test hooks (`cfg(test)` only, so absent from every other build, as D-99 (8)'s hooks): a
/// per-thread function called just before a directory is opened and just before a file is
/// created (after its parent was verified), and a per-thread override of the symlink
/// privilege probe.
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod seam {
    use std::cell::{Cell, RefCell};
    use std::path::Path;

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        static BEFORE_CREATE: RefCell<Option<Hook>> = RefCell::new(None);
        static BEFORE_OPEN_DIR: RefCell<Option<Hook>> = RefCell::new(None);
        static PRIVILEGE: Cell<Option<bool>> = const { Cell::new(None) };
        static MAX_HELD: Cell<Option<usize>> = const { Cell::new(None) };
    }

    /// Hold at most `n` directories on this thread's trees (`None`: [`super::MAX_HELD`]).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn force_max_held(n: Option<usize>) {
        MAX_HELD.with(|c| c.set(n));
    }

    pub(super) fn forced_max_held() -> Option<usize> {
        MAX_HELD.with(Cell::get)
    }

    /// Call `f` with a file's full path just before it is created (its parent verified).
    pub(crate) fn on_before_create(f: Option<Hook>) {
        BEFORE_CREATE.with(|h| *h.borrow_mut() = f);
    }

    /// Call `f` with a directory's full path just before it is opened relative to its parent
    /// (Windows only: elsewhere directories are made by path).
    pub(crate) fn on_before_open_dir(f: Option<Hook>) {
        BEFORE_OPEN_DIR.with(|h| *h.borrow_mut() = f);
    }

    pub(crate) fn before_create(p: &Path) {
        BEFORE_CREATE.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(p);
            }
        });
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn before_open_dir(p: &Path) {
        BEFORE_OPEN_DIR.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(p);
            }
        });
    }

    /// Force [`super::can_create_symlinks`]'s answer on this thread (`None`: the real probe).
    pub(crate) fn force_symlink_privilege(v: Option<bool>) {
        PRIVILEGE.with(|c| c.set(v));
    }

    pub(super) fn forced_symlink_privilege() -> Option<bool> {
        PRIVILEGE.with(Cell::get)
    }
}
