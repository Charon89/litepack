//! Block-ordered extraction (E2-19): plan, decode every needed block once (in parallel under a
//! memory bound), write each chunk to every place that needs it; each file is written by one
//! writer, every writer in block order.
//!
//! **The plan.** The entry table is read and every entry passes the [`ExtractPolicy`] before
//! anything is written: path, device name, symlink, then the format tool's conflicting-name
//! rule, then — when the output directory already exists — the linked-parent hook for every
//! existing parent directory below it that is a symlink or a reparse point (a junction), and the
//! overwrite hook for every file target that already exists. Every file's chunk list is resolved
//! against the chunk table (indices in range, lengths adding up to the file size) and turned
//! into placements — (offset and length in the block, file, offset in the file) — filed under
//! the block that holds the chunk. A deduplicated chunk is simply placed several times; a file
//! whose chunks lie in several blocks receives each chunk when its block comes.
//!
//! **Threads.** [`ExtractOptions::threads`] (`lpk x --threads N`) is the extraction's total
//! thread budget: decode workers plus file writers never exceed N (the dispatching thread, which
//! only hands blocks on, is not counted). N = 1 is the sequential path: one thread decodes each
//! block and writes it. For N >= 2 there are `clamp(N / 4, 1, 4)` writers (also capped by the
//! open-file bound and the number of files) and the remaining threads decode, capped by the
//! memory bound and the number of blocks needed.
//!
//! **The pool.** The needed blocks are decoded in index order by the decode workers, each with
//! its own reader of the archive ([`Archive::fork`]) and the archive's registry (so revision
//! 1.1's `jpeg-reconstruct` runs in the workers and independent JPEG streams decode in parallel).
//! A block is decoded with [`Archive::decode_block_checked`]: the reader's own checks, and every
//! chunk compared with its record, so a damaged chunk fails before a byte of its block is
//! written. A worker takes the next block only with a permit; a permit is returned when the last
//! writer has written the block, so at most `in_flight` decoded blocks exist at any time
//! (decoding, waiting or being written). Each needed block is decoded once by the pool
//! (`blocks_decoded`); a JPEG block's reconstruction also reads the lower blocks holding its
//! record's parts through the worker's reader, counted apart (`nested_decodes`).
//!
//! **The memory bound.** One block in flight costs at most the envelope's `max_block_plain`
//! (its plain bytes) plus the decode allowance (the smaller of the envelope's `decode_memory`
//! and the reader's `Resources::memory`); in an archive with records a worker may hold a nested
//! lower block as well, so the cost there is two blocks plus the allowance. The budget is
//! [`ExtractOptions::memory`] (default: the reader's `Resources::memory`); the workers are at
//! most `budget / cost`, at least one, with as many permits. Outside the bound: each worker's
//! copy of the index (its block and generation lists; the chunk table is shared) and the plan
//! itself (one placement per chunk reference).
//!
//! **The write path.** Files are dealt to the writers (file `i` to writer `i mod n`); a
//! dispatcher reorders the decoded blocks and hands each, in index order, to every writer with a
//! placement in it. A file is created on its first chunk (create-new, so nothing is ever
//! replaced; set to its final length when it has more than one chunk), written at offsets, and
//! closed with its modification time set as soon as its last chunk is written; files with no
//! chunk are created during the plan. At most [`ExtractOptions::max_open_files`] files are open
//! (split over the writers); when a writer's set is full the file it opened longest ago is
//! closed and reopened (without create, without truncation) when its next chunk comes. Entry
//! flags are not applied (the format tool applies none either); directory times are not
//! restored.
//!
//! **Failure.** The first error — a decode error (named with its block and the first file
//! placed in it), a writer's I/O error, a thread that cannot be started, or a panic in any
//! thread — cancels the pool; every thread is joined; every file this extraction created and did
//! not finish is removed (the states live outside the threads, so this holds whichever thread
//! failed); then the error is returned, or the panic resumed. Finished files and the directories
//! stay, as the format tool keeps the files it finished; no partial file is ever left behind.
//!
//! The linked-parent rule checks the existing parent chain once, before writing; opening every
//! target relative to a verified directory handle is the stronger rule, left to the policy tasks
//! (E3/E6).

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lpk_format::{cli, Archive, Entry, EntryKind, FormatError, MemoryPriors};

use crate::CoreError;

/// What an extraction may write. Every hook is called before anything is written; an `Err`
/// refuses the whole extraction with that error.
pub trait ExtractPolicy: Sync {
    /// The archive path of every entry (the format's own path rules were checked when the entry
    /// table was parsed).
    fn check_path(&self, path: &str) -> Result<(), FormatError>;
    /// Every symlink entry.
    fn symlink(&self, entry: &Entry) -> Result<(), FormatError>;
    /// A file target that already exists. Returning `Ok` accepts it, but the file is still
    /// opened create-new: a policy that replaces must remove the target itself.
    fn overwrite(&self, target: &Path) -> Result<(), FormatError>;
    /// The archive path of every entry, for device names.
    fn device(&self, path: &str) -> Result<(), FormatError>;
    /// An existing directory below the output directory, on the way to a target, that is a
    /// symlink or a reparse point (writing through it could leave the output directory).
    fn linked_parent(&self, dir: &Path) -> Result<(), FormatError>;
}

/// The format tool's refusals, called from `lpk_format::cli`: unsafe paths
/// (`check_extraction_path`), Windows device names (`check_reserved_device`), every symlink
/// (`refuse_symlink`) and every existing file (`check_no_overwrite`); and every linked parent
/// (`UnsafePath` with the reason "linked parent"). Conflicting names (`check_conflicting_names`)
/// are refused by the plan for every policy.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPolicy;

impl ExtractPolicy for DefaultPolicy {
    fn check_path(&self, path: &str) -> Result<(), FormatError> {
        cli::check_extraction_path(path)
    }

    fn symlink(&self, entry: &Entry) -> Result<(), FormatError> {
        cli::refuse_symlink(entry)
    }

    fn overwrite(&self, target: &Path) -> Result<(), FormatError> {
        cli::check_no_overwrite(target)
    }

    fn device(&self, path: &str) -> Result<(), FormatError> {
        cli::check_reserved_device(path)
    }

    fn linked_parent(&self, dir: &Path) -> Result<(), FormatError> {
        Err(FormatError::UnsafePath {
            path: dir.display().to_string(),
            reason: "linked parent",
        })
    }
}

/// How an extraction runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractOptions {
    /// The total thread budget (decoders plus writers); `None` is the machine's logical cores.
    /// 1 is the sequential path.
    pub threads: Option<usize>,
    /// The decode memory budget in bytes; `None` is the reader's `Resources::memory`.
    pub memory: Option<u64>,
    /// Most files open at once (at least one); also caps the writers.
    pub max_open_files: usize,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            threads: None,
            memory: None,
            max_open_files: 128,
        }
    }
}

/// What an extraction did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractSummary {
    /// Files written.
    pub files: u64,
    /// Directory entries created.
    pub directories: u64,
    /// File bytes written.
    pub bytes: u64,
    /// Blocks in the archive.
    pub blocks: u64,
    /// Blocks some file needs (the others are not read).
    pub blocks_needed: u64,
    /// Blocks the pool decoded (one per needed block).
    pub blocks_decoded: u64,
    /// Lower blocks decoded again inside a reconstruction (a JPEG record's parts).
    pub nested_decodes: u64,
    /// Chunk placements (a deduplicated chunk counts once per place).
    pub placements: u64,
    /// Decode workers.
    pub workers: usize,
    /// Most decoded blocks in flight.
    pub in_flight: usize,
    /// File writers.
    pub writers: usize,
    /// Times a file closed by the open-file bound was opened again.
    pub reopened: u64,
}

/// One chunk's destination.
#[derive(Debug, Clone, Copy)]
struct Place {
    at: usize,
    len: usize,
    file: usize,
    file_off: u64,
}

const NOT_CREATED: u8 = 0;
const PARTIAL: u8 = 1;
const COMPLETE: u8 = 2;

#[derive(Debug)]
struct Target {
    name: String,
    path: PathBuf,
    size: u64,
    chunks: u64,
    mtime_ns: i64,
}

/// What every thread sees: the targets, their states and the counters. Cleanup reads the
/// states from here, whichever thread failed.
struct Shared {
    targets: Vec<Target>,
    states: Vec<AtomicU8>,
    files: AtomicU64,
    bytes: AtomicU64,
    reopened: AtomicU64,
}

impl Shared {
    fn state(&self, file: usize) -> u8 {
        self.states[file].load(Ordering::SeqCst)
    }

    fn set(&self, file: usize, s: u8) {
        self.states[file].store(s, Ordering::SeqCst);
    }

    /// Remove every file created and not finished.
    fn clean_up(&self) {
        for (i, t) in self.targets.iter().enumerate() {
            if self.state(i) == PARTIAL {
                let _ = std::fs::remove_file(&t.path);
            }
        }
    }

    /// Create and finish a file with no chunk.
    fn create_empty(&self, file: usize) -> Result<(), CoreError> {
        let t = &self.targets[file];
        let f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&t.path)
            .map_err(|e| io(&t.path, e))?;
        self.set(file, PARTIAL);
        finish(self, file, &f)
    }
}

fn finish(shared: &Shared, file: usize, f: &File) -> Result<(), CoreError> {
    let t = &shared.targets[file];
    if let Some(m) = mtime(t.mtime_ns) {
        f.set_modified(m).map_err(|e| io(&t.path, e))?;
    }
    shared.set(file, COMPLETE);
    shared.files.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// `dir` with the archive path's components pushed one at a time (a `/` inside one `join`
/// argument is not converted under a `\\?\` prefix on Windows).
fn join_components(dir: &Path, path: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for c in path.split('/') {
        p.push(c);
    }
    p
}

fn mtime(ns: i64) -> Option<SystemTime> {
    if ns == i64::MIN {
        return None;
    }
    let d = Duration::from_nanos(ns.unsigned_abs());
    if ns >= 0 {
        UNIX_EPOCH.checked_add(d)
    } else {
        UNIX_EPOCH.checked_sub(d)
    }
}

fn io(path: &Path, e: std::io::Error) -> CoreError {
    CoreError::io(path, e)
}

/// A symlink, or on Windows any reparse point (a junction included).
fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// One writer's files (the targets `i` with `i % n == w`): their remaining chunks and the open
/// set.
struct Out<'s> {
    shared: &'s Shared,
    n: usize,
    remaining: Vec<u64>,
    open: HashMap<usize, (File, u64)>,
    order: VecDeque<usize>,
    max_open: usize,
}

impl<'s> Out<'s> {
    fn new(shared: &'s Shared, w: usize, n: usize, max_open: usize) -> Self {
        let remaining = (w..shared.targets.len())
            .step_by(n)
            .map(|i| shared.targets[i].chunks)
            .collect();
        Out {
            shared,
            n,
            remaining,
            open: HashMap::new(),
            order: VecDeque::new(),
            max_open: max_open.max(1),
        }
    }

    fn handle(&mut self, file: usize) -> Result<&mut (File, u64), CoreError> {
        if !self.open.contains_key(&file) {
            while self.open.len() >= self.max_open {
                match self.order.pop_front() {
                    Some(old) => {
                        self.open.remove(&old);
                    }
                    None => break,
                }
            }
            let s = self.shared;
            let t = &s.targets[file];
            let f = if s.state(file) == NOT_CREATED {
                let f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&t.path)
                    .map_err(|e| io(&t.path, e))?;
                s.set(file, PARTIAL);
                if t.chunks > 1 {
                    f.set_len(t.size).map_err(|e| io(&t.path, e))?;
                }
                f
            } else {
                s.reopened.fetch_add(1, Ordering::Relaxed);
                OpenOptions::new()
                    .write(true)
                    .open(&t.path)
                    .map_err(|e| io(&t.path, e))?
            };
            self.open.insert(file, (f, 0));
            self.order.push_back(file);
        }
        self.open
            .get_mut(&file)
            .ok_or(CoreError::Extract("open file set"))
    }

    fn write_block(
        &mut self,
        block: usize,
        places: &[Place],
        plain: &[u8],
    ) -> Result<(), CoreError> {
        for p in places {
            let data =
                p.at.checked_add(p.len)
                    .and_then(|end| plain.get(p.at..end))
                    .ok_or(FormatError::BlockLengthMismatch { block })?;
            let (f, pos) = self.handle(p.file)?;
            let mut r = Ok(());
            if *pos != p.file_off {
                r = f.seek(SeekFrom::Start(p.file_off)).map(|_| ());
            }
            r = r.and_then(|()| f.write_all(data));
            *pos = p.file_off + p.len as u64;
            if let Err(e) = r {
                return Err(io(&self.shared.targets[p.file].path, e));
            }
            self.shared.bytes.fetch_add(p.len as u64, Ordering::Relaxed);
            let left = &mut self.remaining[p.file / self.n];
            *left -= 1;
            if *left == 0 {
                if let Some((f, _)) = self.open.remove(&p.file) {
                    // The order holds only open files, so it stays as small as the set.
                    self.order.retain(|&x| x != p.file);
                    finish(self.shared, p.file, &f)?;
                }
            }
        }
        Ok(())
    }
}

/// A counting semaphore that can be cancelled.
struct Permits {
    free: Mutex<usize>,
    cv: Condvar,
    cancelled: AtomicBool,
}

impl Permits {
    fn new(n: usize) -> Self {
        Permits {
            free: Mutex::new(n),
            cv: Condvar::new(),
            cancelled: AtomicBool::new(false),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn acquire(&self) -> bool {
        let Ok(mut n) = self.free.lock() else {
            return false;
        };
        loop {
            if self.is_cancelled() {
                return false;
            }
            if *n > 0 {
                *n -= 1;
                return true;
            }
            n = match self.cv.wait(n) {
                Ok(n) => n,
                Err(_) => return false,
            };
        }
    }

    fn release(&self) {
        if let Ok(mut n) = self.free.lock() {
            *n += 1;
        }
        self.cv.notify_one();
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        // Take the lock so a worker between its check and its wait sees the flag.
        drop(self.free.lock());
        self.cv.notify_all();
    }
}

/// The plan: every target, the placements of every needed block, the directory count.
struct Plan {
    shared: Shared,
    per_block: Vec<Vec<Place>>,
    needed: Vec<usize>,
    directories: u64,
    placements: u64,
}

fn plan<R: Read + Seek>(
    archive: &mut Archive<R>,
    dir: &Path,
    policy: &dyn ExtractPolicy,
) -> Result<Plan, CoreError> {
    let entries: Vec<Entry> = archive
        .entry_table()?
        .table()?
        .iter()
        .collect::<Result<_, _>>()?;
    for e in &entries {
        if e.kind == EntryKind::Symlink {
            policy.symlink(e)?;
        }
        policy.check_path(&e.path)?;
        policy.device(&e.path)?;
    }
    cli::check_conflicting_names(&entries)?;
    // A directory this extraction creates holds nothing yet: only an existing one is looked in.
    let fresh = std::fs::symlink_metadata(dir).is_err();
    let mut looked: HashSet<PathBuf> = HashSet::new();
    let table = archive.chunks();
    let blocks = archive.index().blocks.len();
    let mut per_block: Vec<Vec<Place>> = vec![Vec::new(); blocks];
    let mut targets = Vec::new();
    let mut dirs = Vec::new();
    let mut placements = 0u64;
    for e in &entries {
        let path = join_components(dir, &e.path);
        if !fresh && e.kind != EntryKind::Symlink {
            // Every existing parent below the output directory, once each.
            for p in path.ancestors().skip(1) {
                if p == dir || !p.starts_with(dir) || !looked.insert(p.to_path_buf()) {
                    break;
                }
                if let Ok(m) = std::fs::symlink_metadata(p) {
                    if is_link(&m) {
                        policy.linked_parent(p)?;
                    }
                }
            }
        }
        match e.kind {
            EntryKind::Directory => dirs.push(path),
            EntryKind::Symlink => {}
            EntryKind::File => {
                if !fresh && std::fs::symlink_metadata(&path).is_ok() {
                    policy.overwrite(&path)?;
                }
                let file = targets.len();
                let mut off = Some(0u64);
                for &c in &e.chunks {
                    let len = table.len();
                    let place = table
                        .locate(c)
                        .ok_or(FormatError::ChunkIndexOutOfRange { chunk: c, len })?;
                    let block = place.block;
                    let bad = || FormatError::BlockLengthMismatch { block };
                    let Some(at) = off else { break };
                    let list = per_block.get_mut(block).ok_or_else(bad)?;
                    list.push(Place {
                        at: usize::try_from(place.offset_in_block).map_err(|_| bad())?,
                        len: usize::try_from(place.plain_len).map_err(|_| bad())?,
                        file,
                        file_off: at,
                    });
                    off = at.checked_add(place.plain_len);
                }
                if off != Some(e.size) {
                    return Err(FormatError::FileSizeMismatch {
                        expected: e.size,
                        found: off.unwrap_or(u64::MAX),
                    }
                    .into());
                }
                placements += e.chunks.len() as u64;
                targets.push(Target {
                    name: e.path.clone(),
                    path,
                    size: e.size,
                    chunks: e.chunks.len() as u64,
                    mtime_ns: e.mtime_ns,
                });
            }
        }
    }
    // Directories: the root, every directory entry, every file's parent; each created once.
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let mut made: HashSet<PathBuf> = HashSet::new();
    let parents = targets.iter().filter_map(|t| t.path.parent());
    for d in dirs.iter().map(PathBuf::as_path).chain(parents) {
        if made.insert(d.to_path_buf()) {
            std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
        }
    }
    let needed = (0..blocks).filter(|&b| !per_block[b].is_empty()).collect();
    let states = targets.iter().map(|_| AtomicU8::new(NOT_CREATED)).collect();
    let shared = Shared {
        targets,
        states,
        files: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        reopened: AtomicU64::new(0),
    };
    Ok(Plan {
        shared,
        per_block,
        needed,
        directories: dirs.len() as u64,
        placements,
    })
}

/// The pool for `archive` under `opts`: decode workers, decoded blocks in flight, file writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pool {
    /// The thread budget N (decoders plus writers never exceed it).
    pub threads: usize,
    /// Decode workers: `min(N - writers, budget / cost, needed blocks)`, at least one.
    pub workers: usize,
    /// Decoded blocks in flight (decoding, waiting or being written): as many as the workers.
    pub in_flight: usize,
    /// File writers: `clamp(N / 4, 1, 4)`, capped by the open-file bound and the files.
    pub writers: usize,
}

/// The pool size for `archive` under `opts`, for `needed` blocks and `files` files with content.
pub fn pool_size<R: Read + Seek>(
    archive: &Archive<R>,
    opts: &ExtractOptions,
    needed: usize,
    files: usize,
) -> Pool {
    let threads = opts
        .threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1);
    if threads == 1 {
        return Pool {
            threads,
            workers: 1,
            in_flight: 1,
            writers: 1,
        };
    }
    let env = archive.index().envelope;
    let allowance = env.decode_memory.min(archive.resources().memory);
    let blocks_held = if archive.index().records.is_some() {
        2
    } else {
        1
    };
    let cost = env
        .max_block_plain
        .saturating_mul(blocks_held)
        .saturating_add(allowance)
        .max(1);
    let budget = opts.memory.unwrap_or(archive.resources().memory);
    let by_memory = usize::try_from(budget / cost).unwrap_or(usize::MAX).max(1);
    let writers = (threads / 4)
        .clamp(1, 4)
        .min(opts.max_open_files.max(1))
        .min(files.max(1));
    let workers = (threads - writers).min(by_memory).min(needed.max(1)).max(1);
    Pool {
        threads,
        workers,
        in_flight: workers,
        writers,
    }
}

/// How a run ended.
enum Outcome {
    Done,
    Failed(CoreError),
    Panicked(Box<dyn Any + Send>),
}

/// Extract every entry of `archive` under `dir`. `reopen` gives another reader of the same
/// archive bytes for each decode worker (called only when the thread budget is more than one).
/// See the module documentation for the plan, the threads, the pool and the failure rule.
pub fn extract_archive<R, F>(
    archive: &mut Archive<R>,
    reopen: F,
    dir: &Path,
    policy: &dyn ExtractPolicy,
    opts: &ExtractOptions,
) -> Result<ExtractSummary, CoreError>
where
    R: Read + Seek + Send,
    F: Fn() -> std::io::Result<R>,
{
    let Plan {
        shared,
        per_block,
        needed,
        directories,
        placements,
    } = plan(archive, dir, policy)?;
    let with_content = shared.targets.iter().filter(|t| t.chunks > 0).count();
    let pool = pool_size(archive, opts, needed.len(), with_content);
    let first_file: Vec<usize> = per_block
        .iter()
        .map(|p| p.first().map_or(usize::MAX, |p| p.file))
        .collect();
    let decoded = AtomicU64::new(0);
    let reads = AtomicU64::new(0);
    let decode_error = |block: usize, source: FormatError| CoreError::Decode {
        block,
        path: shared
            .targets
            .get(first_file[block])
            .map(|t| t.name.clone())
            .unwrap_or_default(),
        source,
    };
    let outcome = {
        let shared = &shared;
        let run = || -> Result<(), CoreError> {
            for i in 0..shared.targets.len() {
                if shared.targets[i].chunks == 0 {
                    shared.create_empty(i)?;
                }
            }
            Ok(())
        };
        match run() {
            Err(e) => Outcome::Failed(e),
            Ok(()) if pool.threads == 1 => {
                let before = archive.decode_count();
                let r = catch_unwind(AssertUnwindSafe(|| {
                    let mut out = Out::new(shared, 0, 1, opts.max_open_files);
                    for &b in &needed {
                        let plain = archive.decode_block_checked(b);
                        decoded.fetch_add(1, Ordering::Relaxed);
                        let plain = plain.map_err(|e| decode_error(b, e))?;
                        out.write_block(b, &per_block[b], &plain)?;
                    }
                    Ok(())
                }));
                reads.fetch_add(archive.decode_count() - before, Ordering::Relaxed);
                match r {
                    Ok(Ok(())) => Outcome::Done,
                    Ok(Err(e)) => Outcome::Failed(e),
                    Err(p) => Outcome::Panicked(p),
                }
            }
            Ok(()) => {
                let forks = (0..pool.workers)
                    .map(|_| reopen().map(|reader| archive.fork(reader)))
                    .collect::<std::io::Result<Vec<_>>>();
                match forks {
                    Ok(forks) => parallel(Run {
                        shared,
                        forks,
                        needed: &needed,
                        per_block,
                        pool,
                        max_open: opts.max_open_files,
                        decoded: &decoded,
                        reads: &reads,
                        decode_error: &decode_error,
                    }),
                    Err(e) => Outcome::Failed(FormatError::from(e).into()),
                }
            }
        }
    };
    // Every target finished, or the run failed.
    let outcome = match outcome {
        Outcome::Done => match (0..shared.targets.len()).find(|&i| shared.state(i) != COMPLETE) {
            Some(_) => Outcome::Failed(CoreError::Extract("a file was not finished")),
            None => Outcome::Done,
        },
        other => other,
    };
    match outcome {
        Outcome::Done => {}
        Outcome::Failed(e) => {
            shared.clean_up();
            return Err(e);
        }
        Outcome::Panicked(p) => {
            shared.clean_up();
            resume_unwind(p);
        }
    }
    let decoded = decoded.load(Ordering::Relaxed);
    Ok(ExtractSummary {
        files: shared.files.load(Ordering::Relaxed),
        directories,
        bytes: shared.bytes.load(Ordering::Relaxed),
        blocks: archive.index().blocks.len() as u64,
        blocks_needed: needed.len() as u64,
        blocks_decoded: decoded,
        nested_decodes: reads.load(Ordering::Relaxed).saturating_sub(decoded),
        placements,
        workers: pool.workers,
        in_flight: pool.in_flight,
        writers: pool.writers,
        reopened: shared.reopened.load(Ordering::Relaxed),
    })
}

type Decoded = (usize, Result<Vec<u8>, FormatError>);

/// A decoded block on its way to the writers; its permit is returned when the last writer
/// drops it.
struct Held<'p> {
    block: usize,
    plain: Vec<u8>,
    permits: &'p Permits,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.permits.release();
    }
}

const WRITER_STOPPED: &str = "a file writer stopped";
const DECODER_STOPPED: &str = "a decode worker stopped";

/// Everything a parallel run needs.
struct Run<'a, R: Read + Seek> {
    shared: &'a Shared,
    forks: Vec<Archive<R>>,
    needed: &'a [usize],
    per_block: Vec<Vec<Place>>,
    pool: Pool,
    max_open: usize,
    decoded: &'a AtomicU64,
    reads: &'a AtomicU64,
    decode_error: &'a (dyn Fn(usize, FormatError) -> CoreError + Sync),
}

/// Keep the first panic payload.
fn keep(slot: &Mutex<Option<Box<dyn Any + Send>>>, p: Box<dyn Any + Send>) {
    if let Ok(mut s) = slot.lock() {
        if s.is_none() {
            *s = Some(p);
        }
    }
}

fn parallel<R: Read + Seek + Send>(run: Run<'_, R>) -> Outcome {
    let Run {
        shared,
        forks,
        needed,
        per_block,
        pool,
        max_open,
        decoded,
        reads,
        decode_error,
    } = run;
    let n = pool.writers.max(1);
    // The placements of writer w in block b.
    let mut split: Vec<Vec<Vec<Place>>> = vec![vec![Vec::new(); per_block.len()]; n];
    for (b, places) in per_block.into_iter().enumerate() {
        for p in places {
            split[p.file % n][b].push(p);
        }
    }
    let permits = Permits::new(pool.in_flight);
    let panic: Mutex<Option<Box<dyn Any + Send>>> = Mutex::new(None);
    let next = AtomicUsize::new(0);
    let per_writer_open = (max_open / n).max(1);
    std::thread::scope(|s| {
        let (permits, panic, next, split) = (&permits, &panic, &next, &split);
        let mut spawn_error = None;
        let mut senders = Vec::with_capacity(n);
        let mut writers = Vec::with_capacity(n);
        for (w, places) in split.iter().enumerate() {
            let (wtx, wrx) = mpsc::channel::<Arc<Held<'_>>>();
            let job = move || -> Result<(), CoreError> {
                let mut out = Out::new(shared, w, n, per_writer_open);
                for held in wrx {
                    if permits.is_cancelled() {
                        break;
                    }
                    let r = catch_unwind(AssertUnwindSafe(|| {
                        out.write_block(held.block, &places[held.block], &held.plain)
                    }));
                    match r {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            permits.cancel();
                            return Err(e);
                        }
                        Err(p) => {
                            keep(panic, p);
                            permits.cancel();
                            return Err(CoreError::Extract(WRITER_STOPPED));
                        }
                    }
                }
                Ok(())
            };
            match std::thread::Builder::new()
                .name(format!("lpk-write-{w}"))
                .spawn_scoped(s, job)
            {
                Ok(h) => {
                    senders.push(wtx);
                    writers.push(h);
                }
                Err(e) => {
                    spawn_error = Some(e);
                    break;
                }
            }
        }
        let (tx, rx) = mpsc::channel::<Decoded>();
        let mut decoders = Vec::with_capacity(forks.len());
        if spawn_error.is_none() {
            for (i, mut fork) in forks.into_iter().enumerate() {
                let tx = tx.clone();
                let job = move || -> u64 {
                    loop {
                        if !permits.acquire() {
                            break;
                        }
                        let k = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&b) = needed.get(k) else {
                            permits.release();
                            break;
                        };
                        let r = catch_unwind(AssertUnwindSafe(|| fork.decode_block_checked(b)));
                        decoded.fetch_add(1, Ordering::Relaxed);
                        match r {
                            Ok(r) => {
                                if tx.send((k, r)).is_err() {
                                    break;
                                }
                            }
                            Err(p) => {
                                keep(panic, p);
                                permits.cancel();
                                break;
                            }
                        }
                    }
                    fork.decode_count()
                };
                match std::thread::Builder::new()
                    .name(format!("lpk-decode-{i}"))
                    .spawn_scoped(s, job)
                {
                    Ok(h) => decoders.push(h),
                    Err(e) => {
                        spawn_error = Some(e);
                        break;
                    }
                }
            }
        }
        drop(tx);
        let r = match spawn_error {
            Some(e) => Err(FormatError::from(e).into()),
            None => dispatch(&rx, needed, split, &senders, permits, decode_error),
        };
        // On failure stop whatever still runs (on success the decoders have ended and the
        // writers drain their queues); then join every thread.
        if r.is_err() {
            permits.cancel();
        }
        drop(senders);
        drop(rx);
        for h in decoders {
            match h.join() {
                Ok(count) => {
                    reads.fetch_add(count, Ordering::Relaxed);
                }
                Err(p) => keep(panic, p),
            }
        }
        let mut writer_error = None;
        for h in writers {
            match h.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    writer_error.get_or_insert(e);
                }
                Err(p) => keep(panic, p),
            }
        }
        if let Some(p) = panic.lock().ok().and_then(|mut p| p.take()) {
            return Outcome::Panicked(p);
        }
        // A writer's own error explains an internal stop of the dispatcher; a decode error
        // (or any other) stands.
        match (r, writer_error) {
            (Err(CoreError::Extract(_)) | Ok(()), Some(w)) => Outcome::Failed(w),
            (Err(e), _) => Outcome::Failed(e),
            (Ok(()), None) => Outcome::Done,
        }
    })
}

fn dispatch<'p>(
    rx: &mpsc::Receiver<Decoded>,
    needed: &[usize],
    split: &[Vec<Vec<Place>>],
    senders: &[mpsc::Sender<Arc<Held<'p>>>],
    permits: &'p Permits,
    decode_error: &dyn Fn(usize, FormatError) -> CoreError,
) -> Result<(), CoreError> {
    let mut pending: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    for (want, &b) in needed.iter().enumerate() {
        let plain = loop {
            if let Some(p) = pending.remove(&want) {
                break p;
            }
            if permits.is_cancelled() {
                return Err(CoreError::Extract(WRITER_STOPPED));
            }
            let (k, r) = rx.recv().map_err(|_| CoreError::Extract(DECODER_STOPPED))?;
            match r {
                Ok(p) => {
                    pending.insert(k, p);
                }
                Err(e) => {
                    let block = needed.get(k).copied().unwrap_or(b);
                    return Err(decode_error(block, e));
                }
            }
        };
        let held = Arc::new(Held {
            block: b,
            plain,
            permits,
        });
        for (w, tx) in senders.iter().enumerate() {
            if !split[w][b].is_empty() && tx.send(Arc::clone(&held)).is_err() {
                return Err(CoreError::Extract(WRITER_STOPPED));
            }
        }
    }
    Ok(())
}

/// Open the archive file at `path` with the full reader (revision 1.1's `jpeg-reconstruct`) and
/// the given prior files' contents, and extract it under `dir` (each decode worker opens the
/// file again).
pub fn extract_file(
    path: &Path,
    dir: &Path,
    priors: &[Vec<u8>],
    policy: &dyn ExtractPolicy,
    opts: &ExtractOptions,
) -> Result<ExtractSummary, CoreError> {
    let file = File::open(path).map_err(|e| io(path, e))?;
    let mut a = Archive::open(file, &lpk_format::Resources::default())?;
    if !priors.is_empty() {
        let mut store = MemoryPriors::new();
        for p in priors {
            store.insert(p.clone());
        }
        a.set_priors(Box::new(store));
    }
    crate::register_full_reader(&mut a);
    extract_archive(&mut a, || File::open(path), dir, policy, opts)
}
