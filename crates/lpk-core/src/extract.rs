//! Block-ordered extraction (E2-19): plan, decode every needed block once (in parallel under a
//! memory bound), write each chunk to every place that needs it, each file by one writer, in block order.
//!
//! **The plan.** The entry table is read and every entry passes the [`ExtractPolicy`] before
//! anything is written (path, device name, symlink, then the format tool's conflicting-name
//! rule, then the overwrite hook for targets that already exist). Every file's chunk list is
//! resolved against the chunk table (indices in range, lengths adding up to the file size) and
//! turned into placements — (offset and length in the block, file, offset in the file) — filed
//! under the block that holds the chunk. A deduplicated chunk is simply placed several times;
//! a file whose chunks lie in several blocks receives each chunk when its block comes.
//!
//! **The pool.** The blocks some file needs are decoded in index order by a pool of workers,
//! each with its own reader of the archive ([`Archive::fork`]) and the archive's registry (so
//! revision 1.1's `jpeg-reconstruct` runs in the workers and independent JPEG streams decode in
//! parallel). A block is decoded with [`Archive::decode_block_checked`]: the reader's own checks,
//! and every chunk compared with its record, so a damaged chunk is `ChunkMismatch` before a byte
//! of its block is written. A worker takes the next block only with a permit; a permit is
//! returned when the writer has written the block, so at most `in_flight` decoded blocks exist
//! at any time (decoding, waiting or being written).
//!
//! **The memory bound.** One block in flight costs at most the envelope's `max_block_plain`
//! (its plain bytes) plus the envelope's `decode_memory` (the decoder's working memory); the
//! budget is [`ExtractOptions::memory`] (default: the reader's `Resources::memory`). The pool
//! has `min(threads, budget / cost, needed blocks)` workers, at least one, and as many permits;
//! with one worker the blocks are decoded on the writer's thread, exactly as a sequential
//! extraction would.
//!
//! **The write path.** Files are dealt to `min(threads, files)` writer threads (file `i` to writer `i mod n`); each writer receives every block in index order and writes its own files' chunks, and a block's permit returns when the last writer has written it (with one thread, the writer is the decoding thread). A file is created on its first chunk
//! (create-new, so nothing is ever replaced; set to its final length when it has more than one
//! chunk), written at offsets, and closed with its modification time set as soon as its last
//! chunk is written; files with no chunk are created during the plan. At most
//! [`ExtractOptions::max_open_files`] files are open; when the set is full the file opened
//! longest ago is closed and reopened (without create, without truncation) when its next chunk
//! comes. Entry flags are not applied (the format tool applies none either); directory times are
//! not restored.
//!
//! **Failure.** The first error stops the extraction: the workers are cancelled, every file this
//! extraction created and did not finish is removed, and the error is returned. Finished files
//! and the directories stay, as the format tool keeps the files it finished; no partial file is
//! ever left behind silently.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
}

/// The format tool's refusals, called from `lpk_format::cli`: unsafe paths
/// (`check_extraction_path`), Windows device names (`check_reserved_device`), every symlink
/// (`refuse_symlink`) and every existing file (`check_no_overwrite`). Conflicting names
/// (`check_conflicting_names`) are refused by the plan for every policy.
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
}

/// How an extraction runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractOptions {
    /// Decode workers; `None` is the machine's logical cores. Capped by the memory bound.
    pub threads: Option<usize>,
    /// The decode memory budget in bytes; `None` is the reader's `Resources::memory`.
    pub memory: Option<u64>,
    /// Most files open at once (at least one).
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
    /// Calls that decoded a block (one per needed block).
    pub blocks_decoded: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    NotCreated,
    Partial,
    Complete,
}

#[derive(Debug)]
struct Target {
    path: PathBuf,
    size: u64,
    chunks: u64,
    remaining: u64,
    mtime_ns: i64,
    state: State,
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

/// The files being written: the open set and every target's state.
struct Out {
    targets: Vec<Target>,
    open: HashMap<usize, (File, u64)>,
    order: VecDeque<usize>,
    max_open: usize,
    files: u64,
    bytes: u64,
    reopened: u64,
}

impl Out {
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
            let t = &mut self.targets[file];
            let f = if t.state == State::NotCreated {
                let f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&t.path)
                    .map_err(|e| io(&t.path, e))?;
                t.state = State::Partial;
                if t.chunks > 1 {
                    f.set_len(t.size).map_err(|e| io(&t.path, e))?;
                }
                f
            } else {
                self.reopened += 1;
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

    fn finish(&mut self, file: usize, f: &File) -> Result<(), CoreError> {
        let t = &mut self.targets[file];
        if let Some(m) = mtime(t.mtime_ns) {
            f.set_modified(m).map_err(|e| io(&t.path, e))?;
        }
        t.state = State::Complete;
        self.files += 1;
        Ok(())
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
            let path_err = |s: &Self, e| io(&s.targets[p.file].path, e);
            let (f, pos) = self.handle(p.file)?;
            let mut r = Ok(());
            if *pos != p.file_off {
                r = f.seek(SeekFrom::Start(p.file_off)).map(|_| ());
            }
            r = r.and_then(|()| f.write_all(data));
            *pos = p.file_off + p.len as u64;
            if let Err(e) = r {
                return Err(path_err(self, e));
            }
            self.bytes += p.len as u64;
            let t = &mut self.targets[p.file];
            t.remaining -= 1;
            if t.remaining == 0 {
                if let Some((f, _)) = self.open.remove(&p.file) {
                    self.finish(p.file, &f)?;
                }
            }
        }
        Ok(())
    }

    /// Remove every file created and not finished (after an error).
    fn clean_up(&mut self) {
        self.open.clear();
        for t in &self.targets {
            if t.state == State::Partial {
                let _ = std::fs::remove_file(&t.path);
            }
        }
    }
}

/// A counting semaphore that can be cancelled.
struct Permits {
    free: Mutex<usize>,
    cv: Condvar,
    cancelled: AtomicBool,
}

impl Permits {
    fn acquire(&self) -> bool {
        let Ok(mut n) = self.free.lock() else {
            return false;
        };
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
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
    out: Out,
    per_block: Vec<Vec<Place>>,
    needed: Vec<usize>,
    directories: u64,
    placements: u64,
}

fn plan<R: Read + Seek>(
    archive: &mut Archive<R>,
    dir: &Path,
    policy: &dyn ExtractPolicy,
    max_open: usize,
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
    let table = archive.chunks();
    let blocks = archive.index().blocks.len();
    let mut per_block: Vec<Vec<Place>> = vec![Vec::new(); blocks];
    let mut targets = Vec::new();
    let mut dirs = Vec::new();
    let mut placements = 0u64;
    for e in &entries {
        let path = join_components(dir, &e.path);
        match e.kind {
            EntryKind::Directory => dirs.push(path),
            EntryKind::Symlink => {}
            EntryKind::File => {
                if !fresh && std::fs::symlink_metadata(&path).is_ok() {
                    policy.overwrite(&path)?;
                }
                let file = targets.len();
                let mut off = 0u64;
                for &c in &e.chunks {
                    let len = table.len();
                    let place = table
                        .locate(c)
                        .ok_or(FormatError::ChunkIndexOutOfRange { chunk: c, len })?;
                    let block = place.block;
                    let bad = || FormatError::BlockLengthMismatch { block };
                    let list = per_block.get_mut(block).ok_or_else(bad)?;
                    list.push(Place {
                        at: usize::try_from(place.offset_in_block).map_err(|_| bad())?,
                        len: usize::try_from(place.plain_len).map_err(|_| bad())?,
                        file,
                        file_off: off,
                    });
                    off = off.saturating_add(place.plain_len);
                    if off == u64::MAX {
                        break;
                    }
                }
                if off != e.size {
                    return Err(FormatError::FileSizeMismatch {
                        expected: e.size,
                        found: off,
                    }
                    .into());
                }
                placements += e.chunks.len() as u64;
                targets.push(Target {
                    path,
                    size: e.size,
                    chunks: e.chunks.len() as u64,
                    remaining: e.chunks.len() as u64,
                    mtime_ns: e.mtime_ns,
                    state: State::NotCreated,
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
    let mut out = Out {
        targets,
        open: HashMap::new(),
        order: VecDeque::new(),
        max_open: max_open.max(1),
        files: 0,
        bytes: 0,
        reopened: 0,
    };
    // Files with no chunk are complete as soon as they exist.
    for i in 0..out.targets.len() {
        if out.targets[i].chunks == 0 {
            let t = &mut out.targets[i];
            let f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&t.path)
                .map_err(|e| io(&t.path, e))?;
            t.state = State::Partial;
            out.finish(i, &f)?;
        }
    }
    Ok(Plan {
        out,
        per_block,
        needed,
        directories: dirs.len() as u64,
        placements,
    })
}

/// The pool for `archive` under `opts`: decode workers, decoded blocks in flight, file writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pool {
    /// Decode workers: `min(threads, budget / cost, needed blocks)`, at least one.
    pub workers: usize,
    /// Decoded blocks in flight (decoding, waiting or being written): as many as the workers.
    pub in_flight: usize,
    /// File writers: `min(threads, files with content)`, at least one; each file is written by
    /// one writer, every writer in block order.
    pub writers: usize,
}

/// The pool size for `archive` under `opts`, for `needed` blocks and `files` files with content.
pub fn pool_size<R: Read + Seek>(
    archive: &Archive<R>,
    opts: &ExtractOptions,
    needed: usize,
    files: usize,
) -> Pool {
    let env = archive.index().envelope;
    let cost = env.max_block_plain.saturating_add(env.decode_memory).max(1);
    let budget = opts.memory.unwrap_or(archive.resources().memory);
    let by_memory = usize::try_from(budget / cost).unwrap_or(usize::MAX).max(1);
    let threads = opts
        .threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1);
    let workers = threads.min(by_memory).min(needed.max(1));
    Pool {
        workers,
        in_flight: workers,
        writers: threads.min(files.max(1)),
    }
}

/// Extract every entry of `archive` under `dir`. `reopen` gives another reader of the same
/// archive bytes for each decode worker (called only when the pool has more than one thread).
/// See the module documentation for the plan, the pool and the failure rule.
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
        mut out,
        per_block,
        needed,
        directories,
        placements,
    } = plan(archive, dir, policy, opts.max_open_files)?;
    let with_content = out.targets.iter().filter(|t| t.chunks > 0).count();
    let pool = pool_size(archive, opts, needed.len(), with_content);
    let decoded = AtomicU64::new(0);
    let (r, mut outs) = if pool.workers <= 1 && pool.writers <= 1 {
        let mut r = Ok(());
        for &b in &needed {
            let plain = archive.decode_block_checked(b);
            decoded.fetch_add(1, Ordering::Relaxed);
            r = plain
                .map_err(CoreError::from)
                .and_then(|p| out.write_block(b, &per_block[b], &p));
            if r.is_err() {
                break;
            }
        }
        (r, vec![out])
    } else {
        let forks = (0..pool.workers)
            .map(|_| reopen().map(|reader| archive.fork(reader)))
            .collect::<std::io::Result<Vec<_>>>();
        match forks {
            Ok(forks) => parallel(forks, &needed, per_block, out, pool, &decoded),
            Err(e) => (Err(FormatError::from(e).into()), vec![out]),
        }
    };
    if let Err(e) = r {
        for o in &mut outs {
            o.clean_up();
        }
        return Err(e);
    }
    Ok(ExtractSummary {
        files: outs.iter().map(|o| o.files).sum(),
        directories,
        bytes: outs.iter().map(|o| o.bytes).sum(),
        blocks: archive.index().blocks.len() as u64,
        blocks_needed: needed.len() as u64,
        blocks_decoded: decoded.load(Ordering::Relaxed),
        placements,
        workers: pool.workers,
        in_flight: pool.in_flight,
        writers: pool.writers,
        reopened: outs.iter().map(|o| o.reopened).sum(),
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

impl Out {
    /// Deal the targets to `n` writers: target `i` goes to writer `i % n` as its target `i / n`.
    fn split(self, n: usize) -> Vec<Out> {
        let mut outs: Vec<Out> = (0..n)
            .map(|_| Out {
                targets: Vec::new(),
                open: HashMap::new(),
                order: VecDeque::new(),
                max_open: (self.max_open / n).max(1),
                files: 0,
                bytes: 0,
                reopened: 0,
            })
            .collect();
        if let Some(first) = outs.first_mut() {
            first.files = self.files;
            first.bytes = self.bytes;
        }
        for (i, t) in self.targets.into_iter().enumerate() {
            outs[i % n].targets.push(t);
        }
        outs
    }
}

const WRITER_STOPPED: &str = "a file writer stopped";

fn parallel<R: Read + Seek + Send>(
    forks: Vec<Archive<R>>,
    needed: &[usize],
    per_block: Vec<Vec<Place>>,
    out: Out,
    pool: Pool,
    decoded: &AtomicU64,
) -> (Result<(), CoreError>, Vec<Out>) {
    let n = pool.writers.max(1);
    // The placements of writer w in block b, with the writer's own target numbers.
    let mut split: Vec<Vec<Vec<Place>>> = vec![vec![Vec::new(); per_block.len()]; n];
    for (b, places) in per_block.into_iter().enumerate() {
        for p in places {
            split[p.file % n][b].push(Place {
                file: p.file / n,
                ..p
            });
        }
    }
    let outs = out.split(n);
    let permits = Permits {
        free: Mutex::new(pool.in_flight),
        cv: Condvar::new(),
        cancelled: AtomicBool::new(false),
    };
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<Decoded>();
    std::thread::scope(|s| {
        for mut fork in forks {
            let tx = tx.clone();
            let (permits, next) = (&permits, &next);
            s.spawn(move || loop {
                if !permits.acquire() {
                    break;
                }
                let k = next.fetch_add(1, Ordering::SeqCst);
                let Some(&b) = needed.get(k) else {
                    permits.release();
                    break;
                };
                let r = fork.decode_block_checked(b);
                decoded.fetch_add(1, Ordering::Relaxed);
                if tx.send((k, r)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut senders = Vec::with_capacity(n);
        let mut handles = Vec::with_capacity(n);
        for (mut o, places) in outs.into_iter().zip(&split) {
            let (wtx, wrx) = mpsc::channel::<Arc<Held<'_>>>();
            senders.push(wtx);
            let permits = &permits;
            handles.push(s.spawn(move || {
                let mut r = Ok(());
                for held in wrx {
                    r = o.write_block(held.block, &places[held.block], &held.plain);
                    if r.is_err() {
                        permits.cancel();
                        break;
                    }
                }
                (o, r)
            }));
        }
        let r = dispatch_in_order(&rx, needed, &split, &senders, &permits);
        if r.is_err() {
            permits.cancel();
        }
        drop(senders);
        drop(rx);
        let mut outs = Vec::with_capacity(n);
        let mut writer_err = None;
        for h in handles {
            match h.join() {
                Ok((o, wr)) => {
                    outs.push(o);
                    if let (Err(e), None) = (wr, &writer_err) {
                        writer_err = Some(e);
                    }
                }
                Err(_) => {
                    writer_err.get_or_insert(CoreError::Extract(WRITER_STOPPED));
                }
            }
        }
        // A writer's own error explains an internal stop of the dispatcher; a decode error
        // (or any other) stands.
        let r = match (r, writer_err) {
            (Err(CoreError::Extract(_)) | Ok(()), Some(w)) => Err(w),
            (r, _) => r,
        };
        (r, outs)
    })
}

fn dispatch_in_order<'p>(
    rx: &mpsc::Receiver<Decoded>,
    needed: &[usize],
    split: &[Vec<Vec<Place>>],
    senders: &[mpsc::Sender<Arc<Held<'p>>>],
    permits: &'p Permits,
) -> Result<(), CoreError> {
    let mut pending: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    for (want, &b) in needed.iter().enumerate() {
        let plain = loop {
            if let Some(p) = pending.remove(&want) {
                break p;
            }
            let (k, r) = rx
                .recv()
                .map_err(|_| CoreError::Extract("a decode worker stopped"))?;
            pending.insert(k, r?);
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
