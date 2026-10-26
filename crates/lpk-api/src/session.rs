//! [`Session`]: one opened archive, `.lpk` in process or foreign through E4's broker, and
//! every read and maintenance operation on it.
//!
//! **Sharing.** A `Session` is `Send + Sync`. Every read (`for_each`, `model`, `read_entry`,
//! `preflight`, `extract`, `export`, `test`, `check`, `repair`) forks the opened reader
//! (`Archive::fork`: nothing is read or checked again) onto a fresh file handle under a shared
//! guard and registers its cancel token for the duration. The exclusive operations
//! (`history`, `rollback(self)`, `append(self)`) cancel the registered tokens and wait for the
//! shared guards to drop before they run.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use lpk_core::foreign::{ForeignSummary, Listing};
use lpk_core::memory::Budget;
use lpk_core::{CancelToken, CoreError, Event, Monitor, Stage};
use lpk_format::{Archive, BlockStep, Credentials, Entry, EntryKind, FormatError, MemoryPriors};

use crate::error::{Class, Error};
use crate::model::{IdSet, ListModel};

/// What an operation applies to.
#[derive(Debug, Clone, Default)]
pub enum Selection {
    /// Every entry.
    #[default]
    All,
    /// The entries D-90's globs select (a directory's match selects its subtree).
    Globs(lpk_core::Selection),
    /// The entries picked by id (an index into the entry table; [`IdSet`]): picks are not
    /// rewritten as globs (D-90 (3): no escape), a million picks are not patterns.
    Ids(Arc<IdSet>),
}

/// Where an extraction writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// Into this directory (created if missing).
    Dir(PathBuf),
    /// The smart rule: a lone top-level item whose name is free goes beside the archive into
    /// `beside`; anything else into a fresh `<archive stem>` folder there (`<stem> (k)` when
    /// taken).
    Smart {
        /// The folder the archive is in (or the one chosen).
        beside: PathBuf,
    },
    /// Always into a fresh `<archive stem>` folder in `beside` (`<stem> (k)` when taken).
    NewFolder {
        /// The folder the new one is created in.
        beside: PathBuf,
    },
}

/// How [`Session::open`] opens an archive.
#[derive(Debug, Clone, Default)]
pub struct OpenOptions<'a> {
    /// The credentials of an encrypted archive; without them a sealed archive opens keyless
    /// (it lists when it is listable; [`Session::unlock`] adds them later).
    pub credentials: Option<&'a Credentials>,
    /// The reader's decode memory limit in bytes (an archive whose envelope needs more is
    /// refused at open); `None` is the extraction default (`min(4 GiB, half the physical
    /// memory)`).
    pub memory: Option<u64>,
    /// The contents of the prior files the archive may need.
    pub priors: Vec<Vec<u8>>,
    /// Open this earlier generation read-only (a view of the file's first bytes, as
    /// `rollback` would leave it); `None` is the latest.
    pub generation: Option<u64>,
    /// The broker's options for a foreign archive (E4's limits, the worker).
    pub foreign: lpk_core::foreign::Options,
}

/// What [`Session::info`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// The archive's path.
    pub path: PathBuf,
    /// `lpk`, or the foreign format's name.
    pub format: String,
    /// True for a foreign archive (read through the sandboxed worker).
    pub foreign: bool,
    /// The generation open (0 for a foreign archive).
    pub generation: u64,
    /// The latest generation of the file (0 for a foreign archive).
    pub latest_generation: u64,
    /// The archive is encrypted.
    pub encrypted: bool,
    /// The entry table is readable without the key.
    pub listable: bool,
    /// Opened without the key of an encrypted archive: only listing (when listable), `test`
    /// on frame hashes, `check` and `repair` work.
    pub keyless: bool,
    /// The bytes the open generation spans.
    pub length: u64,
}

/// One entry as [`Session::for_each`] hands it out.
#[derive(Debug, Clone, Copy)]
pub struct EntryRef<'a> {
    /// The entry's id: its index in the entry table (what [`Selection::Ids`] and
    /// [`Session::read_entry`] take).
    pub id: u32,
    /// The generation the entry belongs to (0 for a foreign archive).
    pub generation: u64,
    entry: &'a Entry,
}

impl<'a> EntryRef<'a> {
    /// The entry as the format stores it.
    pub fn entry(&self) -> &'a Entry {
        self.entry
    }

    /// The entry's archive path (`/`-separated).
    pub fn path(&self) -> &'a str {
        &self.entry.path
    }

    /// File, directory or symlink.
    pub fn kind(&self) -> EntryKind {
        self.entry.kind
    }

    /// The file's size in bytes (0 for a directory).
    pub fn size(&self) -> u64 {
        self.entry.size
    }

    /// The modification time in nanoseconds since the Unix epoch; `i64::MIN` when none.
    pub fn mtime_ns(&self) -> i64 {
        self.entry.mtime_ns
    }
}

/// How [`Session::extract`] runs.
#[derive(Debug, Clone, Copy)]
pub struct ExtractOptions {
    /// What an existing file at a target gets (E3-2).
    pub overwrite: lpk_core::Overwrite,
    /// Create symlink entries, or skip them with a warning (the default).
    pub symlinks: lpk_core::Symlinks,
    /// The total thread budget; `None` is the machine's logical cores.
    pub threads: Option<usize>,
    /// The memory budget; `None` is the default of the archive's envelope tier.
    pub memory: Option<u64>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            overwrite: lpk_core::Overwrite::Refuse,
            symlinks: lpk_core::Symlinks::Skip,
            threads: None,
            memory: None,
        }
    }
}

/// What an extraction did.
#[derive(Debug, Clone)]
pub struct ExtractReport {
    /// The directory the entries went into.
    pub dir: PathBuf,
    /// The engine's counts.
    pub outcome: Extracted,
    /// The symlink entries skipped (without `Symlinks::Create`), in entry order.
    pub skipped_symlinks: Vec<String>,
}

/// The engine's counts of an extraction.
#[derive(Debug, Clone)]
pub enum Extracted {
    /// An `.lpk` through the block-ordered engine.
    Lpk(lpk_core::ExtractSummary),
    /// A foreign archive through the sandboxed worker.
    Foreign(ForeignSummary),
}

/// What [`Session::preflight`] found before any write (D-54 (4)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preflight {
    /// The directory the extraction would write into.
    pub dir: PathBuf,
    /// Entries selected.
    pub entries: u64,
    /// File entries selected.
    pub files: u64,
    /// Their bytes.
    pub bytes: u64,
    /// Selected entries whose target already exists.
    pub existing: u64,
}

/// What `test` checked.
pub type TestReport = lpk_format::VerifySummary;
/// What `check` and `repair` found.
pub type RecoveryReport = lpk_format::RepairReport;

/// One generation of the archive's chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationInfo {
    /// The generation number: 0 for the first write.
    pub generation: u64,
    /// The bytes the archive spans with this generation the latest.
    pub end: u64,
}

impl From<lpk_format::Generation> for GenerationInfo {
    fn from(g: lpk_format::Generation) -> Self {
        GenerationInfo {
            generation: g.generation,
            end: g.end(),
        }
    }
}

/// A reader over the first `len` bytes of a file: the whole file, or an earlier generation.
#[derive(Debug)]
pub(crate) struct Window {
    file: File,
    len: u64,
    pos: u64,
}

impl Window {
    fn new(mut file: File, len: Option<u64>) -> std::io::Result<Self> {
        let end = file.seek(SeekFrom::End(0))?;
        let len = len.map_or(end, |l| l.min(end));
        file.seek(SeekFrom::Start(0))?;
        Ok(Window { file, len, pos: 0 })
    }
}

impl Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        let n = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        if n == 0 {
            return Ok(0);
        }
        let n = self.file.read(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Window {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(o) => Some(o),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        }
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before 0"))?;
        self.pos = self.file.seek(SeekFrom::Start(target))?;
        Ok(self.pos)
    }
}

struct LpkState {
    archive: Mutex<Archive<Window>>,
    /// The bytes of the open generation.
    len: u64,
}

struct ForeignState {
    format: lpk_core::foreign::Format,
    listing: Mutex<Option<Arc<Listing>>>,
}

enum Kind {
    Lpk(Box<LpkState>),
    Foreign(ForeignState),
}

/// An opened archive (see the module documentation).
pub struct Session {
    path: PathBuf,
    info: Info,
    kind: Kind,
    credentials: Option<Credentials>,
    options: SessionOptions,
    gate: RwLock<()>,
    readers: Mutex<Vec<(u64, CancelToken)>>,
    next_reader: AtomicU64,
    model: Mutex<Option<Arc<ListModel>>>,
}

/// What a re-open (`unlock`, after `append`) needs again.
#[derive(Debug, Clone, Default)]
struct SessionOptions {
    memory: Option<u64>,
    priors: Vec<Vec<u8>>,
    generation: Option<u64>,
    foreign: lpk_core::foreign::Options,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("info", &self.info)
            .field("credentials", &self.credentials.is_some())
            .finish_non_exhaustive()
    }
}

/// A read in progress: the shared guard and the registered token.
struct ReadGuard<'s> {
    session: &'s Session,
    id: u64,
    _guard: RwLockReadGuard<'s, ()>,
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut r) = self.session.readers.lock() {
            r.retain(|(id, _)| *id != self.id);
        }
    }
}

/// The exclusive operations' refusal on a foreign archive.
fn unsupported_foreign(what: &str, path: &Path) -> Error {
    Error::new(
        Class::Unsupported,
        "Unsupported",
        format!(
            "{}: {what} works on .lpk archives only; this is a foreign archive",
            path.display()
        ),
        Some(path),
    )
}

/// The reader's resources: `memory`, else the extraction default.
fn resources(memory: Option<u64>) -> lpk_format::Resources {
    lpk_format::Resources {
        memory: memory
            .unwrap_or_else(|| Budget::resolve(None, lpk_core::memory::EXTRACT_CEILING).bytes),
        ..lpk_format::Resources::default()
    }
}

/// Paces the events of a maintenance walk: the first, then at most one per
/// [`lpk_core::progress::MIN_INTERVAL`], and the last.
struct Pace<'m> {
    monitor: &'m Monitor,
    last: Option<Instant>,
}

impl<'m> Pace<'m> {
    fn new(monitor: &'m Monitor) -> Self {
        Pace {
            monitor,
            last: None,
        }
    }

    /// Report `step` and poll the token: `Break` when cancelled.
    fn step(&mut self, step: BlockStep) -> ControlFlow<()> {
        if self.monitor.cancel.is_cancelled() {
            return ControlFlow::Break(());
        }
        if let Some(p) = &self.monitor.progress {
            let done = step.done >= step.total;
            let due = self.last.is_none_or(|t| {
                t.elapsed() >= lpk_core::progress::MIN_INTERVAL.max(Duration::ZERO)
            });
            if done || due {
                self.last = Some(Instant::now());
                p.event(&Event {
                    stage: Stage::Extract,
                    files_done: 0,
                    files_total: None,
                    bytes_in: 0,
                    bytes_out: 0,
                    blocks_done: step.done,
                    blocks_total: Some(step.total),
                    done,
                });
            }
        }
        ControlFlow::Continue(())
    }
}

/// The glob that selects exactly `path` (each glob metacharacter as a one-character class:
/// `[*]` matches a literal `*`, D-90); `None` for a path a glob cannot spell (a backslash,
/// which a glob reads as a separator).
pub(crate) fn exact_glob(path: &str) -> Option<String> {
    if path.contains('\\') {
        return None;
    }
    let mut g = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        match c {
            '*' | '?' | '[' | '{' | '}' => {
                g.push('[');
                g.push(c);
                g.push(']');
            }
            _ => g.push(c),
        }
    }
    Some(g)
}

impl Session {
    /// Open the archive at `path`: a foreign archive (by its magic) through E4's broker,
    /// listed only when first asked; an `.lpk` in process with the credentials, the priors
    /// and the full reader (revision 1.1's `jpeg-reconstruct`), its envelope checked against
    /// the memory limit. A file that cannot be opened is `CoreError::Io` naming it.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let dir = tempfile::tempdir()?;
    /// std::fs::write(dir.path().join("a.txt"), b"hello")?;
    /// let archive = dir.path().join("a.lpk");
    /// let inputs = lpk_api::Sources::contents_of(dir.path());
    /// # let inputs = lpk_api::Sources::new(vec![lpk_api::Root::named(&dir.path().join("a.txt"))?], Default::default())?;
    /// lpk_api::create(&inputs, &archive, &lpk_api::CreateSettings::default(), &Default::default())?;
    /// let s = lpk_api::Session::open(&archive, &lpk_api::OpenOptions::default())?;
    /// assert_eq!(s.info().format, "lpk");
    /// # Ok(()) }
    /// ```
    pub fn open(path: &Path, o: &OpenOptions<'_>) -> Result<Session, Error> {
        if let Ok(Some(format)) = lpk_core::foreign::detect_file(path) {
            return Ok(Session::foreign(path, format, o));
        }
        let file = File::open(path).map_err(|e| {
            Error::core(
                CoreError::Io {
                    path: path.to_path_buf(),
                    source: e,
                },
                Some(path),
            )
        })?;
        Session::open_file(file, path, o)
    }

    /// [`Session::open`] of an `.lpk` the caller opened (`path` names it in errors and is
    /// where forks reopen it); a seek failure is `FormatError::Io`. No foreign detection.
    pub fn open_file(file: File, path: &Path, o: &OpenOptions<'_>) -> Result<Session, Error> {
        let options = SessionOptions {
            memory: o.memory,
            priors: o.priors.clone(),
            generation: o.generation,
            foreign: o.foreign.clone(),
        };
        let fail = |e: FormatError| Error::format(e, Some(path));
        let window = Window::new(file, None).map_err(|e| fail(FormatError::Io(e)))?;
        let mut archive = Self::open_archive(window, path, &options, o.credentials)?;
        let latest = archive.generation();
        if let Some(g) = o.generation.filter(|g| *g != latest) {
            let chain = archive.history().map_err(fail)?;
            let at = chain
                .iter()
                .find(|x| x.generation == g)
                .ok_or(FormatError::NoSuchGeneration {
                    requested: g,
                    latest,
                })
                .map_err(fail)?;
            let file = File::open(path).map_err(|e| fail(FormatError::Io(e)))?;
            let window = Window::new(file, Some(at.end())).map_err(|e| fail(FormatError::Io(e)))?;
            archive = Self::open_archive(window, path, &options, o.credentials)?;
        }
        let len = {
            let t = archive.trailer();
            t.index_offset + t.index_len + lpk_format::TRAILER_FRAME_LEN
        };
        let info = Info {
            path: path.to_path_buf(),
            format: "lpk".to_string(),
            foreign: false,
            generation: archive.generation(),
            latest_generation: latest,
            encrypted: archive.is_encrypted(),
            listable: archive.is_listable(),
            keyless: archive.is_keyless(),
            length: len,
        };
        Ok(Session {
            path: path.to_path_buf(),
            info,
            kind: Kind::Lpk(Box::new(LpkState {
                archive: Mutex::new(archive),
                len,
            })),
            credentials: o.credentials.cloned(),
            options,
            gate: RwLock::new(()),
            readers: Mutex::new(Vec::new()),
            next_reader: AtomicU64::new(0),
            model: Mutex::new(None),
        })
    }

    fn open_archive(
        window: Window,
        path: &Path,
        o: &SessionOptions,
        credentials: Option<&Credentials>,
    ) -> Result<Archive<Window>, Error> {
        let mut a = Archive::open_with(window, &resources(o.memory), credentials)
            .map_err(|e| Error::format(e, Some(path)))?;
        if !o.priors.is_empty() {
            let mut store = MemoryPriors::new();
            for p in &o.priors {
                store.insert(p.clone());
            }
            a.set_priors(Box::new(store));
        }
        lpk_core::register_full_reader(&mut a);
        Ok(a)
    }

    fn foreign(path: &Path, format: lpk_core::foreign::Format, o: &OpenOptions<'_>) -> Session {
        Session {
            path: path.to_path_buf(),
            info: Info {
                path: path.to_path_buf(),
                format: format.name().to_string(),
                foreign: true,
                generation: 0,
                latest_generation: 0,
                encrypted: false,
                listable: true,
                keyless: false,
                length: std::fs::metadata(path).map_or(0, |m| m.len()),
            },
            kind: Kind::Foreign(ForeignState {
                format,
                listing: Mutex::new(None),
            }),
            credentials: o.credentials.cloned(),
            options: SessionOptions {
                memory: o.memory,
                priors: o.priors.clone(),
                generation: o.generation,
                foreign: o.foreign.clone(),
            },
            gate: RwLock::new(()),
            readers: Mutex::new(Vec::new()),
            next_reader: AtomicU64::new(0),
            model: Mutex::new(None),
        }
    }

    /// Re-open with credentials (an archive opened keyless, or a foreign archive's password).
    pub fn unlock(&mut self, c: Credentials) -> Result<(), Error> {
        let o = OpenOptions {
            credentials: Some(&c),
            memory: self.options.memory,
            priors: self.options.priors.clone(),
            generation: self.options.generation,
            foreign: self.options.foreign.clone(),
        };
        *self = Session::open(&self.path, &o)?;
        Ok(())
    }

    /// What the archive is.
    pub fn info(&self) -> &Info {
        &self.info
    }

    /// The archive's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The foreign format, when the archive is foreign.
    pub fn foreign_format(&self) -> Option<lpk_core::foreign::Format> {
        match &self.kind {
            Kind::Foreign(f) => Some(f.format),
            Kind::Lpk(_) => None,
        }
    }

    fn read_guard(&self, cancel: &CancelToken) -> ReadGuard<'_> {
        let guard = self.gate.read().unwrap_or_else(|p| p.into_inner());
        let id = self.next_reader.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut r) = self.readers.lock() {
            r.push((id, cancel.clone()));
        }
        ReadGuard {
            session: self,
            id,
            _guard: guard,
        }
    }

    /// Cancel every registered read and wait for the shared guards to drop.
    fn exclusive(&self) -> std::sync::RwLockWriteGuard<'_, ()> {
        if let Ok(r) = self.readers.lock() {
            for (_, t) in r.iter() {
                t.cancel();
            }
        }
        self.gate.write().unwrap_or_else(|p| p.into_inner())
    }

    fn reopen(&self) -> std::io::Result<Window> {
        let len = match &self.kind {
            Kind::Lpk(l) => Some(l.len),
            Kind::Foreign(_) => None,
        };
        Window::new(File::open(&self.path)?, len)
    }

    /// A fork of the opened reader on a fresh handle.
    fn fork(&self) -> Result<Archive<Window>, Error> {
        let Kind::Lpk(l) = &self.kind else {
            return Err(unsupported_foreign("this operation", &self.path));
        };
        let w = self
            .reopen()
            .map_err(|e| Error::format(FormatError::Io(e), Some(&self.path)))?;
        let a = l.archive.lock().unwrap_or_else(|p| p.into_inner());
        Ok(a.fork(w))
    }

    fn listing(&self) -> Result<Arc<Listing>, Error> {
        let Kind::Foreign(f) = &self.kind else {
            return Err(Error::new(
                Class::Internal,
                "Internal",
                "not a foreign archive",
                Some(&self.path),
            ));
        };
        let mut slot = f.listing.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(l) = slot.as_ref() {
            return Ok(Arc::clone(l));
        }
        let l = Arc::new(
            lpk_core::foreign::list_file(
                &self.path,
                self.credentials.as_ref(),
                &self.options.foreign,
                &Monitor::default(),
            )
            .map_err(|e| Error::core(e, Some(&self.path)))?,
        );
        *slot = Some(Arc::clone(&l));
        Ok(l)
    }

    /// The broker's launch warnings of a foreign archive (it is listed now if it was not);
    /// none for an `.lpk`.
    pub fn warnings(&self) -> Result<Vec<String>, Error> {
        match &self.kind {
            Kind::Lpk(_) => Ok(Vec::new()),
            Kind::Foreign(_) => Ok(self.listing()?.warnings.clone()),
        }
    }

    /// Stream the selected entries to `f` in entry-table order, none collected: the memory is
    /// the reader's open plus the entry table (E3-1's bound; `lpk l` runs through this). A
    /// glob matching no entry is `NoMatch`, refused before `f` is called; `f` returning
    /// `Break` stops the walk.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = tempfile::tempdir()?;
    /// # std::fs::write(dir.path().join("a.txt"), b"hello")?;
    /// # let archive = dir.path().join("a.lpk");
    /// # let inputs = lpk_api::Sources::new(vec![lpk_api::Root::named(&dir.path().join("a.txt"))?], Default::default())?;
    /// # lpk_api::create(&inputs, &archive, &lpk_api::CreateSettings::default(), &Default::default())?;
    /// let s = lpk_api::Session::open(&archive, &Default::default())?;
    /// let mut names = Vec::new();
    /// s.for_each(&lpk_api::Selection::All, &mut |e| {
    ///     names.push(e.path().to_string());
    ///     std::ops::ControlFlow::Continue(())
    /// })?;
    /// assert_eq!(names, ["a.txt"]);
    /// # Ok(()) }
    /// ```
    pub fn for_each(
        &self,
        s: &Selection,
        f: &mut dyn FnMut(&EntryRef<'_>) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        let never = CancelToken::new();
        let _g = self.read_guard(&never);
        match &self.kind {
            Kind::Lpk(_) => {
                let fail = |e: FormatError| Error::format(e, Some(&self.path));
                let mut a = self.fork()?;
                let table = a.entry_table().map_err(fail)?;
                let generation = a.generation();
                if let Selection::Globs(g) = s {
                    if !g.is_all() {
                        let mut hits = g.hits();
                        for entry in table.table().map_err(fail)?.iter() {
                            hits.record(&entry.map_err(fail)?.path);
                        }
                        hits.check().map_err(|e| Error::core(e, Some(&self.path)))?;
                    }
                }
                for (i, entry) in table.table().map_err(fail)?.iter().enumerate() {
                    let e = entry.map_err(fail)?;
                    let id = u32::try_from(i).unwrap_or(u32::MAX);
                    if !selected(s, id, &e.path) {
                        continue;
                    }
                    if f(&EntryRef {
                        id,
                        generation,
                        entry: &e,
                    })
                    .is_break()
                    {
                        break;
                    }
                }
                Ok(())
            }
            Kind::Foreign(_) => {
                let listing = self.listing()?;
                if let Selection::Globs(g) = s {
                    if !g.is_all() {
                        let mut hits = g.hits();
                        for e in &listing.entries {
                            hits.record(&e.path);
                        }
                        hits.check().map_err(|e| Error::core(e, Some(&self.path)))?;
                    }
                }
                for (i, e) in listing.entries.iter().enumerate() {
                    let id = u32::try_from(i).unwrap_or(u32::MAX);
                    if !selected(s, id, &e.path) {
                        continue;
                    }
                    if f(&EntryRef {
                        id,
                        generation: 0,
                        entry: e,
                    })
                    .is_break()
                    {
                        break;
                    }
                }
                Ok(())
            }
        }
    }

    /// The listing model (sort, filter, pages, selection totals in Rust), built on the first
    /// call and shared after; its memory bound is [`ListModel::bound`]. Cancellable while it
    /// is built.
    pub fn model(&self, c: &CancelToken) -> Result<Arc<ListModel>, Error> {
        let mut slot = self.model.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(m) = slot.as_ref() {
            return Ok(Arc::clone(m));
        }
        let _g = self.read_guard(c);
        let m = match &self.kind {
            Kind::Lpk(_) => {
                let fail = |e: FormatError| Error::format(e, Some(&self.path));
                let mut a = self.fork()?;
                let table = a.entry_table().map_err(fail)?;
                let n = table.len();
                let mut names = 0u64;
                for e in table.table().map_err(fail)?.iter() {
                    names += e.map_err(fail)?.path.len() as u64;
                }
                let mut b = crate::model::Builder::new(n, names, a.generation())?;
                for (i, e) in table.table().map_err(fail)?.iter().enumerate() {
                    if i % 4096 == 0 {
                        c.check().map_err(|e| Error::core(e, Some(&self.path)))?;
                    }
                    b.push(&e.map_err(fail)?)?;
                }
                b.finish(c)?
            }
            Kind::Foreign(_) => {
                let listing = self.listing()?;
                let names = listing.entries.iter().map(|e| e.path.len() as u64).sum();
                let mut b = crate::model::Builder::new(listing.entries.len() as u64, names, 0)?;
                for e in &listing.entries {
                    b.push(e)?;
                }
                b.finish(c)?
            }
        };
        let m = Arc::new(m);
        *slot = Some(Arc::clone(&m));
        Ok(m)
    }

    /// The content of file entry `id`, at most `cap` bytes: a larger entry is refused with
    /// the cap named (`Refused`, field `preview cap`, class `TooLarge`) before anything is
    /// decoded. Streams through the export engine (E3-6) into a capped sink, polling `c` per
    /// block.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = tempfile::tempdir()?;
    /// # std::fs::write(dir.path().join("a.txt"), b"hello")?;
    /// # let archive = dir.path().join("a.lpk");
    /// # let inputs = lpk_api::Sources::new(vec![lpk_api::Root::named(&dir.path().join("a.txt"))?], Default::default())?;
    /// # lpk_api::create(&inputs, &archive, &lpk_api::CreateSettings::default(), &Default::default())?;
    /// let s = lpk_api::Session::open(&archive, &Default::default())?;
    /// let c = lpk_api::CancelToken::new();
    /// assert_eq!(s.read_entry(0, 1024, &c)?, b"hello");
    /// assert_eq!(s.read_entry(0, 2, &c).unwrap_err().class, lpk_api::Class::TooLarge);
    /// # Ok(()) }
    /// ```
    pub fn read_entry(&self, id: u32, cap: u64, c: &CancelToken) -> Result<Vec<u8>, Error> {
        if let Kind::Foreign(_) = &self.kind {
            return Err(unsupported_foreign("reading one entry", &self.path));
        }
        let _g = self.read_guard(c);
        let fail = |e: FormatError| Error::format(e, Some(&self.path));
        let mut a = self.fork()?;
        let table = a.entry_table().map_err(fail)?;
        let entry = table
            .table()
            .map_err(fail)?
            .iter()
            .nth(id as usize)
            .transpose()
            .map_err(fail)?
            .ok_or_else(|| {
                Error::new(
                    Class::NotFound,
                    "NotFound",
                    format!("{}: no entry {id}", self.path.display()),
                    Some(&self.path),
                )
            })?;
        drop(table);
        if entry.kind != EntryKind::File {
            return Err(Error::usage(format!(
                "{}: not a file ({})",
                entry.path,
                entry.kind.name()
            )));
        }
        if entry.size > cap {
            return Err(Error::format(
                FormatError::Refused(lpk_format::Refusal {
                    field: "preview cap",
                    needed: entry.size,
                    allowed: cap,
                }),
                Some(Path::new(&entry.path)),
            ));
        }
        let mut sink = CappedSink {
            cap,
            out: Vec::with_capacity(usize::try_from(entry.size).unwrap_or(0)),
        };
        match exact_glob(&entry.path) {
            Some(glob) => {
                let opts = lpk_core::ExtractOptions {
                    select: lpk_core::Selection::new(&[glob])
                        .map_err(|e| Error::core(e, Some(&self.path)))?,
                    threads: Some(1),
                    ..lpk_core::ExtractOptions::default()
                };
                lpk_core::export_archive_with(
                    &mut a,
                    || self.reopen(),
                    &mut sink,
                    &opts,
                    &Monitor::with_cancel(c.clone()),
                )
                .map_err(|e| Error::core(e, Some(&self.path)))?;
            }
            // A name a glob cannot spell: the format's reader (not cancellable mid-entry).
            None => {
                c.check().map_err(|e| Error::core(e, Some(&self.path)))?;
                a.extract(&entry, &mut sink.out).map_err(fail)?;
            }
        }
        Ok(sink.out)
    }

    /// The destination directory `to` resolves to (the smart rule needs the top level).
    pub fn destination(&self, s: &Selection, to: &Destination) -> Result<PathBuf, Error> {
        match to {
            Destination::Dir(d) => Ok(d.clone()),
            Destination::NewFolder { beside } => Ok(self.fresh_folder(beside)),
            Destination::Smart { beside } => {
                let mut tops: Vec<String> = Vec::new();
                self.for_each(s, &mut |e| {
                    let top = e.path().split('/').next().unwrap_or_default();
                    if !tops.iter().any(|t| t == top) {
                        tops.push(top.to_string());
                    }
                    if tops.len() > 1 {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                })?;
                match tops.as_slice() {
                    [one] if std::fs::symlink_metadata(beside.join(one)).is_err() => {
                        Ok(beside.clone())
                    }
                    _ => Ok(self.fresh_folder(beside)),
                }
            }
        }
    }

    /// `<archive stem>` in `beside`, or `<stem> (k)` with the smallest free k.
    fn fresh_folder(&self, beside: &Path) -> PathBuf {
        let stem = self
            .path
            .file_stem()
            .map_or_else(|| "archive".into(), |s| s.to_string_lossy().into_owned());
        let first = beside.join(&stem);
        if std::fs::symlink_metadata(&first).is_err() {
            return first;
        }
        (2u64..)
            .map(|k| beside.join(format!("{stem} ({k})")))
            .find(|p| std::fs::symlink_metadata(p).is_err())
            .unwrap_or(first)
    }

    /// Count what an extraction to `to` would write and what is already there, before any
    /// write (D-54 (4)).
    pub fn preflight(
        &self,
        s: &Selection,
        to: &Destination,
        c: &CancelToken,
    ) -> Result<Preflight, Error> {
        let dir = self.destination(s, to)?;
        let mut p = Preflight {
            dir: dir.clone(),
            ..Preflight::default()
        };
        let mut n = 0u64;
        self.for_each(s, &mut |e| {
            n += 1;
            if n.is_multiple_of(4096) && c.is_cancelled() {
                return ControlFlow::Break(());
            }
            p.entries += 1;
            if e.kind() == EntryKind::File {
                p.files += 1;
                p.bytes += e.size();
            }
            if std::fs::symlink_metadata(dir.join(e.path())).is_ok() {
                p.existing += 1;
            }
            ControlFlow::Continue(())
        })?;
        c.check().map_err(|e| Error::core(e, Some(&self.path)))?;
        Ok(p)
    }

    /// The engine's selection of `s` (ids spelled as exact globs, see [`exact_glob`]).
    fn core_selection(&self, s: &Selection) -> Result<lpk_core::Selection, Error> {
        match s {
            Selection::All => Ok(lpk_core::Selection::all()),
            Selection::Globs(g) => Ok(g.clone()),
            Selection::Ids(ids) => {
                let mut globs = Vec::new();
                let mut bad = None;
                self.for_each(&Selection::Ids(Arc::clone(ids)), &mut |e| {
                    match exact_glob(e.path()) {
                        Some(g) => globs.push(g),
                        None => bad = Some(e.path().to_string()),
                    }
                    ControlFlow::Continue(())
                })?;
                if let Some(p) = bad {
                    return Err(Error::new(
                        Class::Unsupported,
                        "Unsupported",
                        format!("{p}: a picked name with a backslash cannot be selected yet"),
                        Some(&self.path),
                    ));
                }
                if globs.is_empty() {
                    return Err(Error::usage("no entry picked"));
                }
                lpk_core::Selection::new(&globs).map_err(|e| Error::core(e, Some(&self.path)))
            }
        }
    }

    /// Extract the selected entries to `to` under E3-2's policy with `o`'s overwrite and
    /// symlink modes; progress and cancellation through `m` (D-87). A foreign archive goes
    /// through the sandboxed worker.
    pub fn extract(
        &self,
        s: &Selection,
        to: &Destination,
        o: &ExtractOptions,
        m: &Monitor,
    ) -> Result<ExtractReport, Error> {
        let select = self.core_selection(s)?;
        let dir = self.destination(s, to)?;
        let _g = self.read_guard(&m.cancel);
        let policy = lpk_core::Policy::new(o.overwrite, o.symlinks);
        let outcome = match &self.kind {
            Kind::Lpk(_) => {
                let mut a = self.fork()?;
                let opts = lpk_core::ExtractOptions {
                    select,
                    threads: o.threads,
                    memory: o.memory,
                    ..lpk_core::ExtractOptions::default()
                };
                let s = lpk_core::extract_archive_with(
                    &mut a,
                    || self.reopen(),
                    &dir,
                    &policy,
                    &opts,
                    m,
                )
                .map_err(|e| Error::core(e, Some(&self.path)))?;
                Extracted::Lpk(s)
            }
            Kind::Foreign(_) => Extracted::Foreign(
                lpk_core::foreign::extract_file(
                    &self.path,
                    &dir,
                    self.credentials.as_ref(),
                    &policy,
                    &select,
                    &self.options.foreign,
                    m,
                )
                .map_err(|e| Error::core(e, Some(&self.path)))?,
            ),
        };
        Ok(ExtractReport {
            dir,
            outcome,
            skipped_symlinks: policy.skipped_symlinks(),
        })
    }

    /// Export the selected entries into a ZIP or 7z file at `out` (E3-6): the target is
    /// refused when it is the archive or exists without `force` ([`check_export_target`]),
    /// created at the first entry and removed on failure.
    pub fn export(
        &self,
        s: &Selection,
        out: &Path,
        o: &crate::export::ExportOptions<'_>,
        m: &Monitor,
    ) -> Result<lpk_core::ExportSummary, Error> {
        if let Kind::Foreign(_) = &self.kind {
            return Err(unsupported_foreign("export", &self.path));
        }
        check_export_target(&self.path, out, o.force)?;
        let select = self.core_selection(s)?;
        let _g = self.read_guard(&m.cancel);
        let mut a = self.fork()?;
        let opts = lpk_core::ExtractOptions {
            select,
            threads: o.threads,
            memory: o.memory,
            ..lpk_core::ExtractOptions::default()
        };
        let settings = crate::export::Settings {
            method: o.method,
            password: o.password,
        };
        let out_path = out.to_path_buf();
        let opener = move || {
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&out_path)
        };
        let mut writer = crate::export::open(o.container, Box::new(opener), out, settings);
        let r = lpk_core::export_archive_with(&mut a, || self.reopen(), writer.as_mut(), &opts, m);
        // Only an output this export created (or truncated under `force`) is removed on
        // failure; an existing one a refusal left untouched stays as it was. (The trailer
        // opens the output when no entry did, so its failure always removes.)
        let created = writer.created();
        let r = r
            .map_err(|e| (e, created))
            .and_then(|s| writer.finish().map(|()| s).map_err(|e| (e, true)));
        match r {
            Ok(s) => Ok(s),
            Err((e, created)) => {
                if created {
                    let _ = std::fs::remove_file(out);
                }
                Err(Error::core(e, Some(&self.path)))
            }
        }
    }

    /// Verify every chunk against its hash (keyless: every frame hash), polling `m` per block.
    pub fn test(&self, m: &Monitor) -> Result<TestReport, Error> {
        let _g = self.read_guard(&m.cancel);
        let mut a = self.fork()?;
        let mut pace = Pace::new(m);
        a.verify_with(&mut |s| pace.step(s))
            .map_err(|e| Error::format(e, Some(&self.path)))?
            .ok_or_else(Error::cancelled)
    }

    /// Hash every recovery shard and count the damaged ones (`.lpk` only), polling `m` per
    /// recovery frame.
    pub fn check(&self, m: &Monitor) -> Result<RecoveryReport, Error> {
        if let Kind::Foreign(_) = &self.kind {
            return Err(unsupported_foreign("check", &self.path));
        }
        let _g = self.read_guard(&m.cancel);
        let mut a = self.fork()?;
        let mut pace = Pace::new(m);
        a.check_recovery_with(&mut |s| pace.step(s))
            .map_err(|e| Error::format(e, Some(&self.path)))?
            .ok_or_else(Error::cancelled)
    }

    /// Write a repaired copy to `out`, which must not exist (`.lpk` only): the copy carries
    /// every repair that was possible; a frame with more damage than it can rebuild is
    /// returned beside the report (the copy kept, D-58 (1)). Any other failure, or a cancel,
    /// removes the copy.
    pub fn repair(
        &self,
        out: &Path,
        m: &Monitor,
    ) -> Result<(RecoveryReport, Option<Error>), Error> {
        if let Kind::Foreign(_) = &self.kind {
            return Err(unsupported_foreign("repair", &self.path));
        }
        let fail = |e: FormatError| Error::format(e, Some(&self.path));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(out)
            .map_err(|e| fail(FormatError::Io(e)))?;
        let _g = self.read_guard(&m.cancel);
        let r = self.fork().and_then(|mut a| {
            let mut pace = Pace::new(m);
            a.repair_into(file, &mut |s| pace.step(s))
                .map_err(fail)?
                .ok_or_else(Error::cancelled)
        });
        match r {
            Ok((report, unrepairable)) => Ok((report, unrepairable.map(fail))),
            Err(e) => {
                let _ = std::fs::remove_file(out);
                Err(e)
            }
        }
    }

    /// The trailer chain, newest first (`.lpk` only). Exclusive: cancels and waits for the
    /// reads in flight.
    pub fn history(&self, c: &CancelToken) -> Result<Vec<GenerationInfo>, Error> {
        let Kind::Lpk(l) = &self.kind else {
            return Err(unsupported_foreign("history", &self.path));
        };
        let _x = self.exclusive();
        c.check().map_err(|e| Error::core(e, Some(&self.path)))?;
        let mut a = l.archive.lock().unwrap_or_else(|p| p.into_inner());
        Ok(a.history()
            .map_err(|e| Error::format(e, Some(&self.path)))?
            .into_iter()
            .map(GenerationInfo::from)
            .collect())
    }

    /// Truncate the archive to `generation` (`.lpk` only; D-59). Exclusive: cancels the reads
    /// in flight, waits for them, closes the session, then rolls back ([`crate::rollback`]).
    pub fn rollback(self, generation: u64) -> Result<GenerationInfo, Error> {
        if let Kind::Foreign(_) = &self.kind {
            return Err(unsupported_foreign("rollback", &self.path));
        }
        drop(self.exclusive());
        let path = self.path.clone();
        drop(self);
        crate::rollback(&path, generation)
    }

    /// Append `dir` as a new generation (`.lpk` only; the archive's own key seals it).
    /// Exclusive: cancels the reads in flight, waits, closes, appends
    /// ([`crate::append`]), and returns the session re-opened on the new generation.
    pub fn append(
        self,
        dir: &Path,
        s: &crate::CreateSettings,
        m: &Monitor,
    ) -> Result<(Session, lpk_core::RunSummary), Error> {
        if let Kind::Foreign(_) = &self.kind {
            return Err(unsupported_foreign("append", &self.path));
        }
        drop(self.exclusive());
        let path = self.path.clone();
        let credentials = self.credentials.clone();
        let options = self.options.clone();
        drop(self);
        let summary = crate::append(&path, dir, s, credentials.as_ref(), m)?;
        let o = OpenOptions {
            credentials: credentials.as_ref(),
            memory: options.memory,
            priors: options.priors,
            generation: None,
            foreign: options.foreign,
        };
        Ok((Session::open(&path, &o)?, summary))
    }

    /// Re-create this archive (foreign or `.lpk`) as an `.lpk` at `out` with `s`: E5-7's
    /// `--convert`. Phase 1 v0 refuses a foreign archive until E4-3's readers land
    /// (`Unsupported`): the broker streams members only to a directory today.
    pub fn convert(
        &self,
        out: &Path,
        s: &crate::CreateSettings,
        m: &Monitor,
    ) -> Result<lpk_core::RunSummary, Error> {
        let _ = (out, s, m);
        Err(unsupported_foreign("convert", &self.path))
    }
}

/// True when entry `id` at `path` is in `s`.
fn selected(s: &Selection, id: u32, path: &str) -> bool {
    match s {
        Selection::All => true,
        Selection::Globs(g) => g.matches(path),
        Selection::Ids(ids) => ids.contains(id),
    }
}

/// The refusals of an export target, before the archive is read: the output must not be the
/// archive (whatever the spelling of either path), and an existing one is refused unless
/// `force`.
pub fn check_export_target(archive: &Path, out: &Path, force: bool) -> Result<(), Error> {
    if std::fs::metadata(out).is_ok() {
        let same = match (std::fs::canonicalize(archive), std::fs::canonicalize(out)) {
            (Ok(a), Ok(o)) => a == o,
            _ => false,
        };
        if same {
            return Err(Error::new(
                Class::Exists,
                "Io",
                format!("{}: the output is the archive itself", out.display()),
                Some(out),
            ));
        }
        if !force {
            return Err(Error::new(
                Class::Exists,
                "Io",
                format!("{}: exists (--force replaces it)", out.display()),
                Some(out),
            ));
        }
    }
    Ok(())
}

/// The sink of `read_entry`: the one file's bytes, refused past the cap.
struct CappedSink {
    cap: u64,
    out: Vec<u8>,
}

impl lpk_core::Sink for CappedSink {
    fn directory(&mut self, _: &Entry) -> Result<(), CoreError> {
        Ok(())
    }

    fn symlink(&mut self, _: &Entry) -> Result<(), CoreError> {
        Ok(())
    }

    fn file(&mut self, entry: &Entry, content: &mut dyn Read) -> Result<(), CoreError> {
        let mut limited = content.take(self.cap.saturating_add(1));
        limited
            .read_to_end(&mut self.out)
            .map_err(|e| CoreError::Io {
                path: PathBuf::from(&entry.path),
                source: e,
            })?;
        if self.out.len() as u64 > self.cap {
            return Err(CoreError::Format(FormatError::Refused(
                lpk_format::Refusal {
                    field: "preview cap",
                    needed: entry.size,
                    allowed: self.cap,
                },
            )));
        }
        // The engine wants the content read to the end.
        std::io::copy(content, &mut std::io::sink()).map_err(|e| CoreError::Io {
            path: PathBuf::from(&entry.path),
            source: e,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_globs_match_only_their_path() {
        for p in ["a/b.txt", "we*ird?/[x]{y,z}!.md", "plain"] {
            let g = exact_glob(p).unwrap();
            let s = lpk_core::Selection::new(&[g]).unwrap();
            assert!(s.matches(p), "{p}");
            assert!(!s.matches("a/bxtxt"), "{p}");
        }
        assert!(exact_glob("a\\b").is_none());
    }
}
