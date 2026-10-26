//! Block-ordered extraction (E2-19): plan, decode every needed block once (in parallel under a
//! memory bound), write each chunk to every place that needs it; each file is written by one
//! writer, every writer in block order.
//!
//! **The plan.** The entry table is read and every entry passes the [`ExtractPolicy`] before
//! anything is written: path, device name, symlink, then the conflicting-name rule and (on a
//! case-insensitive target) the case-collision rule of [`crate::policy`], then — when the
//! output directory already exists — the linked-parent hook for every existing parent directory
//! below it that is a symlink or a reparse point (a junction), the existing hook for a parent
//! that is not a directory, and the existing hook for every file (or symlink to create) whose
//! target already exists, whose answer skips the entry, renames it beside the target or marks
//! the target for replacement at the file's creation (E3-2). Symlink entries the policy lets
//! pass are created after the directories and before any file, or skipped and counted. Every file's chunk list is resolved
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
//! **The memory bound** (E2-20, ruling R19). The budget is [`ExtractOptions::memory`] when given
//! (`lpk x --memory`), else the ceiling of the tier the archive's envelope shows
//! ([`crate::memory::EnvelopeTier::of`]) resolved like the compress side's
//! ([`crate::memory::Budget::resolve`]: the smaller of the ceiling and half the physical memory).
//! The engine counts ([`ExtractMemory`]):
//!
//! - per block in flight (`per_block`): the encoded frame its worker reads (`encoded`: the
//!   largest whole frame length among the needed blocks, which the index gives; the frame is
//!   read into one buffer reserved at its length; twice that for an encrypted archive, whose
//!   frame is opened into a second buffer), its plain bytes (`plain`: the envelope's
//!   `max_block_plain`), the decoder's working memory beyond them (`decoder`: `decode_memory -
//!   max_block_plain`), a step's input while the next step decodes (`step_input`:
//!   `max_block_plain`, for every archive: the decoder keeps a step's output while the next step
//!   builds its own for any graph of more than one step, and the index does not say which blocks
//!   have one, so a graph from any writer is covered), and in an archive with records (`nested`)
//!   the lower block a reconstruction reads (its plain bytes and the largest frame of any block,
//!   which may lie outside the needed ones). A store block's plain bytes are a copy of its
//!   frame's, so both are counted. Each block also counts [`BLOCK_ALLOWANCE`] for its
//!   bookkeeping, so the count has a margin over what the buffers above hold to the byte;
//! - `fixed`: the opened archive's index and chunk table, the plan (per placement its record
//!   in the block lists and in the writers' split, each list grown by doubling; per file its
//!   target, name, path, state and chunk counter; per block the lists' headers), each worker's
//!   reader and copy of the index (`per_worker`: the block, recovery, prior and generation
//!   lists, plus [`WORKER_ALLOWANCE`] for its frame reader and file handle), the writers'
//!   open-file sets ([`WRITER_ALLOWANCE`] each), the dispatcher's channel and reordering map
//!   ([`DISPATCH_ALLOWANCE`]; the blocks it holds are counted per block), and the parsed records
//!   (four times the records frame).
//!
//! The decode workers are `(budget - fixed_shared) / (per_block + per_worker)`, at least one,
//! capped by the threads and the needed blocks; `fixed` is `fixed_shared` plus `per_worker` for
//! each worker. A worker holds a permit from before it reads a frame until the last writer has
//! written the block. Only a decoding block holds the decode terms: a decoded block waiting for
//! its turn at the dispatcher (which hands blocks on in index order) or for the writers holds
//! its plain bytes and its bookkeeping (`held`: `plain +` [`BLOCK_ALLOWANCE`]). So the permits
//! (`in_flight`) are the workers plus as many held blocks as the budget left after
//! `total(workers)` pays for, at most [`HELD_PER_WORKER`] per worker in all and at most the
//! needed blocks (E2-25: a slow block, a large photograph, no longer stalls the other workers
//! once they have finished the blocks after it); at most `workers` blocks decode at once, each
//! at most `per_block`, and the others hold at most `held` each, so the count is
//! `total(workers) + (in_flight - workers) * held` ([`ExtractMemory::total_in_flight`]). The
//! entry table, read and dropped by the plan before any block is decoded, is
//! not counted. An encrypted archive's key derivation (the key slot's Argon2 memory,
//! `key_derivation`) runs at open and is freed before the plan, so the process peak is
//! `max(key_derivation, total_in_flight(workers, in_flight))`
//! ([`ExtractMemory::peak_in_flight`]). The derivation cannot be
//! held to the tier's budget: a sealed archive's index, and with it the envelope that shows the
//! tier, is readable only with the derived key, so at the derivation the only limit known is the
//! reader's (`--memory`, else [`crate::memory::EXTRACT_CEILING`] resolved), against which the
//! key slot's `argon2_m` is checked (an archive asking more is refused at open). A given
//! budget below one block in flight (`total(1)`: `fixed_shared + per_block + per_worker`) is
//! refused with that figure before any directory or file is created, as the compress side
//! refuses (D-67 (1)); a default budget that small runs one worker.
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
//! **Progress and cancellation** ([`crate::progress`]; [`extract_archive_with`]). An
//! `Extract` event per block dispatched (files finished, encoded bytes decoded, file bytes
//! written, blocks dispatched of the blocks needed), rate-limited. The token is polled after
//! the plan (before any directory is made), before each block on the dispatching thread, by
//! each decode worker before each block and by each writer between blocks; a cancelled run
//! stops the pool, joins every thread and cleans up as a failure does (the unfinished files
//! removed, the finished ones and the directories kept), then fails with
//! [`CoreError::Cancelled`].
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

use lpk_format::{Archive, Credentials, Entry, EntryKind, FormatError, MemoryPriors};

use crate::memory::{Budget, BudgetSource, EnvelopeTier};
use crate::policy::{self, Existing, Found, SymlinkAction, Wanted};
pub use crate::policy::{DefaultPolicy, ExtractPolicy};
use crate::progress::{Monitor, Reporter, Stage};
use crate::select::Selection;
use crate::CoreError;

/// How an extraction runs.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// The entries to extract ([`Selection`]: globs over entry paths, a directory's match
    /// selecting its subtree; the default is everything). Only the blocks the selected
    /// entries need are decoded; a pattern matching nothing is [`CoreError::NoMatch`],
    /// refused before any output.
    pub select: Selection,
    /// The total thread budget (decoders plus writers); `None` is the machine's logical cores.
    /// 1 is the sequential path.
    pub threads: Option<usize>,
    /// The extraction's memory budget in bytes; `None` is the ceiling of the tier the
    /// archive's envelope shows, resolved on this machine (see the module documentation).
    /// [`extract_file`] also opens the archive with it as the reader's `Resources::memory` (with
    /// `None`, [`crate::memory::EXTRACT_CEILING`] resolved likewise), so an archive whose
    /// envelope needs more is refused before any output exists.
    pub memory: Option<u64>,
    /// Most files open at once (at least one); also caps the writers.
    pub max_open_files: usize,
    /// Test hook: the dispatcher panics before handing on the `n`-th needed block (0-based), so
    /// a test can check that a panic on the calling thread neither hangs nor leaves a partial
    /// file. Always `None` outside tests.
    #[doc(hidden)]
    pub panic_in_dispatch_at: Option<usize>,
    /// Test hook: the dispatcher hands on no block until every permit is taken (the workers
    /// have decoded the blocks after the first and hold them), so a test sees blocks held past
    /// the workers whatever the timing. Always `false` outside tests.
    #[doc(hidden)]
    pub fill_permits_before_dispatch: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            select: Selection::all(),
            threads: None,
            memory: None,
            max_open_files: 128,
            panic_in_dispatch_at: None,
            fill_permits_before_dispatch: false,
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
    /// Entries left out because something existed at their target and the policy answered
    /// [`Existing::Skip`] (files, symlink entries to create, and a directory with everything
    /// below it when a file is in its way).
    pub skipped: u64,
    /// Entries written beside an existing target under a new name ([`Existing::Rename`]).
    pub renamed: u64,
    /// Symlink entries created.
    pub symlinks: u64,
    /// Symlink entries the policy let pass and did not create ([`SymlinkAction::Skip`]).
    pub symlinks_skipped: u64,
    /// Most decoded blocks held at once, measured (permits taken and not yet returned): never
    /// above `in_flight`. 1 on the sequential path when a block was decoded, else 0.
    pub peak_in_flight: usize,
    /// The memory budget and what the engine counts against it.
    pub memory: ExtractMemory,
}

/// The extraction's memory budget and the terms counted against it (module documentation,
/// "The memory bound"). Every figure is in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractMemory {
    /// The budget and where it came from.
    pub budget: Budget,
    /// The tier the envelope shows (whose ceiling is the default budget).
    pub tier: EnvelopeTier,
    /// Per block in flight: `encoded + plain + decoder + step_input + nested +`
    /// [`BLOCK_ALLOWANCE`].
    pub per_block: u64,
    /// The largest encoded frame a worker reads (doubled for an encrypted archive).
    pub encoded: u64,
    /// The envelope's `max_block_plain`.
    pub plain: u64,
    /// The decoder's working memory beyond the plain bytes: `decode_memory - max_block_plain`.
    pub decoder: u64,
    /// A step's input while the next step decodes: `max_block_plain`, for every archive.
    pub step_input: u64,
    /// With records: the nested lower block (its plain bytes and frame); else 0.
    pub nested: u64,
    /// A decoded block held past its decode (waiting for the dispatcher or the writers): its
    /// plain bytes and its bookkeeping, `plain +` [`BLOCK_ALLOWANCE`].
    pub held: u64,
    /// Held whatever the workers: the opened archive, the plan, the writers, the dispatcher.
    pub fixed_shared: u64,
    /// Each decode worker's reader and copy of the index.
    pub per_worker: u64,
    /// `fixed_shared + per_worker * workers`.
    pub fixed: u64,
    /// A sealed archive's key derivation at open: the key slot's Argon2 memory (`argon2_m`, in
    /// bytes); 0 for an archive that is not encrypted. Freed before the plan, so not part of
    /// [`ExtractMemory::total`]; the process peak is [`ExtractMemory::peak_in_flight`].
    pub key_derivation: u64,
}

impl Default for ExtractMemory {
    fn default() -> Self {
        ExtractMemory {
            budget: Budget {
                bytes: 0,
                source: BudgetSource::Given,
            },
            tier: EnvelopeTier::Store,
            per_block: 0,
            encoded: 0,
            plain: 0,
            decoder: 0,
            step_input: 0,
            nested: 0,
            held: 0,
            fixed_shared: 0,
            per_worker: 0,
            fixed: 0,
            key_derivation: 0,
        }
    }
}

impl ExtractMemory {
    /// What `workers` decoding blocks and their workers hold: `fixed_shared + workers *
    /// (per_block + per_worker)`. This is the whole count only when every permit is a worker's
    /// (the sequential path, or a budget with no room for a held block); it is the yardstick of
    /// "one block in flight" in the `--memory` refusal and the base the held term adds to. The
    /// count of a run is [`ExtractMemory::total_in_flight`].
    pub fn total(&self, workers: usize) -> u64 {
        let w = u64::try_from(workers).unwrap_or(u64::MAX);
        self.fixed_shared
            .saturating_add(w.saturating_mul(self.per_block.saturating_add(self.per_worker)))
    }

    /// What `workers` workers and `in_flight` permits hold: [`ExtractMemory::total`] of the
    /// workers plus `held` for each permit beyond them (a decoded block waiting to be written).
    pub fn total_in_flight(&self, workers: usize, in_flight: usize) -> u64 {
        let extra = u64::try_from(in_flight.saturating_sub(workers)).unwrap_or(u64::MAX);
        self.total(workers)
            .saturating_add(extra.saturating_mul(self.held))
    }

    /// The process's counted peak with `workers` workers and `in_flight` permits: the larger
    /// of the key derivation at open and [`ExtractMemory::total_in_flight`] (the two never
    /// overlap).
    pub fn peak_in_flight(&self, workers: usize, in_flight: usize) -> u64 {
        self.key_derivation
            .max(self.total_in_flight(workers, in_flight))
    }
}

/// The most permits per decode worker (decoding plus decoded blocks held for the dispatcher
/// and the writers), when the budget pays for them. The dispatcher hands blocks on in index
/// order, so a block that decodes slowly holds back the blocks after it; held permits let the
/// other workers keep decoding meanwhile. The cap bounds how far ahead of the dispatch the
/// workers run (the reorder map's size).
pub const HELD_PER_WORKER: usize = 8;

/// Each decode worker's frame reader (its `BufReader` and the frame header's reads), file
/// handle and thread bookkeeping: an allowance.
pub const WORKER_ALLOWANCE: u64 = 64 << 10;
/// Each block in flight's bookkeeping beyond its buffers: one frame-reader chunk (64 KiB, the
/// format's read step: the scratch of a frame read without a location, or a read buffer a
/// platform's std allocates differently) plus 64 KiB for the block header and graph, the
/// decoder's per-call state outside `decode_memory`, the `Held` block and its `Arc`, and its
/// channel and reordering-map nodes. An allowance, so that the count has a margin over the
/// buffers it names instead of matching them to the byte.
pub const BLOCK_ALLOWANCE: u64 = 128 << 10;
/// Each file writer's open-file set, order and channel: an allowance.
pub const WRITER_ALLOWANCE: u64 = 64 << 10;
/// The dispatcher's channel and reordering map, beyond the blocks they hold: an allowance.
pub const DISPATCH_ALLOWANCE: u64 = 64 << 10;

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
    /// What is at `path` is removed (as a link when it is one) just before the file is created.
    replace: bool,
}

/// A symlink entry to create, after the directories and before the files.
#[derive(Debug)]
pub(crate) struct Link {
    pub(crate) path: PathBuf,
    pub(crate) target: Vec<u8>,
    pub(crate) replace: bool,
    /// The target names a directory of the archive (a directory entry, a parent implied by
    /// another entry, or the destination itself): a directory link on Windows. Decided from
    /// the archive alone, never from the file system.
    pub(crate) dir: bool,
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

    /// Create a target's file (create-new; what a replaced target held is removed first).
    fn create(&self, file: usize) -> Result<File, CoreError> {
        let t = &self.targets[file];
        if t.replace {
            policy::remove_existing(&t.path).map_err(|e| io(&t.path, e))?;
        }
        let f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&t.path)
            .map_err(|e| io(&t.path, e))?;
        self.set(file, PARTIAL);
        Ok(f)
    }

    /// Create and finish a file with no chunk.
    fn create_empty(&self, file: usize) -> Result<(), CoreError> {
        let f = self.create(file)?;
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
pub(crate) fn join_components(dir: &Path, path: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for c in path.split('/') {
        p.push(c);
    }
    p
}

pub(crate) fn mtime(ns: i64) -> Option<SystemTime> {
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

/// A symlink; on Windows std reports symlinks and junctions (mount points) as symlinks, while
/// other reparse points (cloud placeholders, dedup files) are not links and are not refused.
fn is_link(meta: &std::fs::Metadata) -> bool {
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
                let f = s.create(file)?;
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

/// A counting semaphore that can be cancelled (shared with [`crate::export`]).
pub(crate) struct Permits {
    free: Mutex<usize>,
    cv: Condvar,
    cancelled: AtomicBool,
    /// Permits in all.
    cap: usize,
    /// Most permits out at once.
    pub(crate) peak: AtomicUsize,
}

impl Permits {
    pub(crate) fn new(n: usize) -> Self {
        Permits {
            free: Mutex::new(n),
            cv: Condvar::new(),
            cancelled: AtomicBool::new(false),
            cap: n,
            peak: AtomicUsize::new(0),
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) fn acquire(&self) -> bool {
        let Ok(mut n) = self.free.lock() else {
            return false;
        };
        loop {
            if self.is_cancelled() {
                return false;
            }
            if *n > 0 {
                *n -= 1;
                self.peak
                    .fetch_max(self.cap.saturating_sub(*n), Ordering::SeqCst);
                return true;
            }
            n = match self.cv.wait(n) {
                Ok(n) => n,
                Err(_) => return false,
            };
        }
    }

    pub(crate) fn release(&self) {
        if let Ok(mut n) = self.free.lock() {
            *n += 1;
        }
        self.cv.notify_one();
    }

    pub(crate) fn cancel(&self) {
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
    /// The targets' name and path bytes.
    name_bytes: u64,
    /// The directories to create, the output directory first.
    mkdirs: Vec<PathBuf>,
    /// The symlink entries to create, in entry order.
    links: Vec<Link>,
    /// Entries skipped because of what existed at their target.
    skipped: u64,
    /// Entries written under a new name.
    renamed: u64,
    /// Symlink entries let pass and not created.
    symlinks_skipped: u64,
}

/// What the plan decided for one entry's target after looking at what exists there.
enum Decision {
    /// Nothing there, or the policy said write (create-new, as before).
    Write,
    Skip,
    /// The path to write instead.
    Rename(PathBuf),
    Replace,
}

/// The plan's view of the output directory while it decides each entry: the existing parents
/// already looked at, those found blocked (a file where a directory goes, skipped by the
/// policy: everything below is skipped), and the paths this extraction will write (built when a
/// rename needs a free name).
struct OutDir<'p> {
    policy: &'p dyn ExtractPolicy,
    dir: &'p Path,
    fresh: bool,
    looked: HashSet<PathBuf>,
    blocked: HashSet<PathBuf>,
    planned: Option<HashSet<PathBuf>>,
    /// The target is case-insensitive: `planned` holds folded paths and is looked up folded,
    /// as the disk compares (the folding of `policy::check_case_collisions`).
    fold: bool,
}

/// `p` as `planned` keys it: lower-cased on a case-insensitive target, else itself.
fn folded(p: &Path, fold: bool) -> PathBuf {
    if fold {
        PathBuf::from(p.to_string_lossy().to_lowercase())
    } else {
        p.to_path_buf()
    }
}

impl OutDir<'_> {
    /// The policy's answer for `wanted` at `path`, when something is there.
    fn ask(&self, path: &Path, wanted: Wanted) -> Result<Option<Existing>, CoreError> {
        if self.fresh {
            return Ok(None);
        }
        match std::fs::symlink_metadata(path) {
            // An existing directory where a directory goes is reused, as before: no question.
            Ok(m) if Found::of(&m) == Found::Directory && wanted == Wanted::Directory => Ok(None),
            Ok(m) => self.policy.existing(path, Found::of(&m), wanted).map(Some),
            Err(_) => Ok(None),
        }
    }

    /// Every existing parent below the output directory, once each: a link is the policy's
    /// linked-parent question; a file (or anything but a directory) the policy's existing
    /// question for a directory, whose `Skip` blocks everything below it. `Ok(false)` when
    /// the entry is to be skipped.
    fn parents_ok(&mut self, path: &Path) -> Result<bool, CoreError> {
        if self.fresh {
            return Ok(true);
        }
        let mut blocked_by_parent = false;
        for p in path.ancestors().skip(1) {
            if p == self.dir || !p.starts_with(self.dir) {
                break;
            }
            if self.blocked.contains(p) {
                blocked_by_parent = true;
                break;
            }
            if !self.looked.insert(p.to_path_buf()) {
                continue;
            }
            if let Ok(m) = std::fs::symlink_metadata(p) {
                if is_link(&m) {
                    self.policy.linked_parent(p)?;
                } else if !m.is_dir() {
                    match self.policy.existing(p, Found::of(&m), Wanted::Directory)? {
                        Existing::Skip => {
                            self.blocked.insert(p.to_path_buf());
                            blocked_by_parent = true;
                        }
                        Existing::Write | Existing::Rename | Existing::Replace => {}
                    }
                }
            }
        }
        Ok(!blocked_by_parent)
    }

    /// Decide an entry at `path` (a file or a symlink to create).
    fn decide(
        &mut self,
        path: &Path,
        wanted: Wanted,
        entries: &[Entry],
    ) -> Result<Decision, CoreError> {
        if !self.parents_ok(path)? {
            return Ok(Decision::Skip);
        }
        let d = match self.ask(path, wanted)? {
            None | Some(Existing::Write) => Decision::Write,
            Some(Existing::Skip) => Decision::Skip,
            Some(Existing::Replace) => Decision::Replace,
            Some(Existing::Rename) => {
                // A free name is one that neither the disk nor any entry of the archive
                // (earlier or later, of any kind) nor an earlier rename takes. Built on the
                // first rename only, so an extraction without one costs nothing more.
                let (dir, fold) = (self.dir, self.fold);
                let planned = self.planned.get_or_insert_with(|| {
                    entries
                        .iter()
                        .map(|e| folded(&join_components(dir, &e.path), fold))
                        .collect()
                });
                let renamed = policy::rename_target(path, |p| {
                    planned.contains(&folded(p, fold)) || std::fs::symlink_metadata(p).is_ok()
                });
                planned.insert(folded(&renamed, fold));
                Decision::Rename(renamed)
            }
        };
        Ok(d)
    }
}

/// Where one selected entry goes after the policy pass.
#[derive(Debug)]
pub(crate) enum Placed {
    /// A directory entry to create (or reuse).
    Dir(PathBuf),
    /// A symlink entry to create.
    Link(Link),
    /// A file to create at `path` (create-new; what is there is removed first when `replace`).
    File { path: PathBuf, replace: bool },
    /// Left out: skipped by the policy, or a symlink it lets pass without creating.
    Skipped,
}

/// The policy pass's result: the selected entries and, for each, where it goes.
#[derive(Debug)]
pub(crate) struct PolicyPass {
    /// The entries the selection kept, in entry order.
    pub(crate) entries: Vec<Entry>,
    /// Parallel to `entries`.
    pub(crate) placed: Vec<Placed>,
    /// Directory entries placed.
    pub(crate) directories: u64,
    /// Entries skipped because of what existed at their target.
    pub(crate) skipped: u64,
    /// Entries written under a new name.
    pub(crate) renamed: u64,
    /// Symlink entries let pass and not created.
    pub(crate) symlinks_skipped: u64,
}

impl PolicyPass {
    /// The directories to create, the output directory first: `dir`, every directory entry,
    /// then the files' parents, then the links'; each once.
    pub(crate) fn mkdirs(&self, dir: &Path) -> Vec<PathBuf> {
        let mut made: HashSet<PathBuf> = HashSet::new();
        let mut mkdirs = vec![dir.to_path_buf()];
        let dirs = self.placed.iter().filter_map(|p| match p {
            Placed::Dir(d) => Some(d.as_path()),
            _ => None,
        });
        let files = self.placed.iter().filter_map(|p| match p {
            Placed::File { path, .. } => path.parent(),
            _ => None,
        });
        let links = self.placed.iter().filter_map(|p| match p {
            Placed::Link(l) => l.path.parent(),
            _ => None,
        });
        for d in dirs.chain(files).chain(links) {
            if made.insert(d.to_path_buf()) {
                mkdirs.push(d.to_path_buf());
            }
        }
        mkdirs
    }
}

/// The archive's own namespace, for symlink targets (E6-0): its directories (directory
/// entries and the parents other entries imply) and its symlink entries, folded on a
/// case-insensitive target as the disk compares.
struct Namespace {
    dirs: HashSet<String>,
    links: HashSet<String>,
    fold: bool,
}

impl Namespace {
    fn new(entries: &[Entry], fold: bool) -> Self {
        let key = |p: &str| {
            if fold {
                p.to_lowercase()
            } else {
                p.to_string()
            }
        };
        let mut dirs = HashSet::new();
        let mut links = HashSet::new();
        for e in entries {
            match e.kind {
                EntryKind::Directory => {
                    dirs.insert(key(&e.path));
                }
                EntryKind::Symlink => {
                    links.insert(key(&e.path));
                }
                EntryKind::File => {}
            }
            let mut at = 0;
            while let Some(i) = e.path[at..].find('/') {
                dirs.insert(key(&e.path[..at + i]));
                at += i + 1;
            }
        }
        Namespace { dirs, links, fold }
    }

    fn key(&self, p: &str) -> String {
        if self.fold {
            p.to_lowercase()
        } else {
            p.to_string()
        }
    }

    /// Whether the link at archive path `link` with `target` is a directory link, or why the
    /// target is refused ([`policy::resolve_symlink_target`]). No file-system call.
    fn resolve(&self, link: &str, target: &[u8]) -> Result<bool, policy::TargetRefusal> {
        let to =
            policy::resolve_symlink_target(link, target, |p| self.links.contains(&self.key(p)))?;
        Ok(to.is_empty() || self.dirs.contains(&self.key(&to)))
    }
}

/// E3-2's pass over `entries` before anything is written, shared by `.lpk` extraction and the
/// foreign-archive broker (`crate::foreign`): the selection, then per entry the symlink rule,
/// the path and device rules, then the conflicting-name rule and (on a case-insensitive
/// target) the case-collision rule, then — when `dir` exists — the linked-parent and existing
/// hooks that skip, rename or replace.
pub(crate) fn policy_pass(
    entries: Vec<Entry>,
    dir: &Path,
    policy: &dyn ExtractPolicy,
    select: &Selection,
) -> Result<PolicyPass, CoreError> {
    // The archive's namespace for link targets, from every entry (a link may name an entry
    // the selection leaves out).
    let namespace = Namespace::new(&entries, policy::case_insensitive_target(dir));
    // The selection first: the policy's rules and the placements concern only what is
    // extracted, and a pattern without a match is refused here, before any output.
    let entries = select.filter(entries, |e| e.path.as_str())?;
    for e in &entries {
        if e.kind == EntryKind::Symlink {
            policy.symlink(e)?;
        }
        policy.check_path(&e.path)?;
        policy.device(&e.path)?;
    }
    policy::check_conflicting_names(&entries)?;
    if policy::case_insensitive_target(dir) {
        policy::check_case_collisions(&entries)?;
    }
    // A directory this extraction creates holds nothing yet: only an existing one is looked in.
    let mut existing = OutDir {
        policy,
        dir,
        fresh: std::fs::symlink_metadata(dir).is_err(),
        looked: HashSet::new(),
        blocked: HashSet::new(),
        planned: None,
        fold: policy::case_insensitive_target(dir),
    };
    let mut placed = Vec::with_capacity(entries.len());
    let (mut directories, mut skipped, mut renamed, mut symlinks_skipped) = (0u64, 0, 0, 0);
    for e in &entries {
        let path = join_components(dir, &e.path);
        let p = match e.kind {
            EntryKind::Directory => {
                if !existing.parents_ok(&path)? {
                    skipped += 1;
                    Placed::Skipped
                } else {
                    match existing.ask(&path, Wanted::Directory)? {
                        Some(Existing::Skip) => {
                            existing.blocked.insert(path);
                            skipped += 1;
                            Placed::Skipped
                        }
                        _ => {
                            directories += 1;
                            Placed::Dir(path)
                        }
                    }
                }
            }
            EntryKind::Symlink if policy.symlink_action(e) == SymlinkAction::Skip => {
                symlinks_skipped += 1;
                Placed::Skipped
            }
            EntryKind::Symlink | EntryKind::File => {
                // A link to create: its target is judged from the archive alone, before
                // anything at the link's path is looked at; a refused one is skipped.
                let link_dir = if e.kind == EntryKind::Symlink {
                    let target = e.symlink_target.as_deref().unwrap_or_default();
                    match namespace.resolve(&e.path, target) {
                        Ok(dir) => dir,
                        Err(refusal) => {
                            policy.symlink_target_refused(e, refusal);
                            symlinks_skipped += 1;
                            placed.push(Placed::Skipped);
                            continue;
                        }
                    }
                } else {
                    false
                };
                let wanted = if e.kind == EntryKind::Symlink {
                    Wanted::Symlink
                } else {
                    Wanted::File
                };
                let decided = match existing.decide(&path, wanted, &entries)? {
                    Decision::Write => Some((path, false)),
                    Decision::Replace => Some((path, true)),
                    Decision::Rename(p) => {
                        renamed += 1;
                        Some((p, false))
                    }
                    Decision::Skip => {
                        skipped += 1;
                        None
                    }
                };
                match decided {
                    None => Placed::Skipped,
                    Some((path, replace)) if e.kind == EntryKind::Symlink => Placed::Link(Link {
                        path,
                        target: e.symlink_target.clone().unwrap_or_default(),
                        replace,
                        dir: link_dir,
                    }),
                    Some((path, replace)) => Placed::File { path, replace },
                }
            }
        };
        placed.push(p);
    }
    Ok(PolicyPass {
        entries,
        placed,
        directories,
        skipped,
        renamed,
        symlinks_skipped,
    })
}

fn plan<R: Read + Seek>(
    archive: &mut Archive<R>,
    dir: &Path,
    policy: &dyn ExtractPolicy,
    select: &Selection,
) -> Result<Plan, CoreError> {
    let entries: Vec<Entry> = archive
        .entry_table()?
        .table()?
        .iter()
        .collect::<Result<_, _>>()?;
    let mut pass = policy_pass(entries, dir, policy, select)?;
    // Directories: created by `make_dirs` after the memory check, so a refused budget leaves
    // nothing behind.
    let mkdirs = pass.mkdirs(dir);
    let table = archive.chunks();
    let blocks = archive.index().blocks.len();
    let mut per_block: Vec<Vec<Place>> = vec![Vec::new(); blocks];
    let mut targets: Vec<Target> = Vec::new();
    let mut links: Vec<Link> = Vec::new();
    let mut placements = 0u64;
    for (e, p) in pass.entries.iter().zip(std::mem::take(&mut pass.placed)) {
        match p {
            Placed::Dir(_) | Placed::Skipped => {}
            Placed::Link(l) => links.push(l),
            Placed::File { path, replace } => {
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
                    replace,
                });
            }
        }
    }
    let needed = (0..blocks).filter(|&b| !per_block[b].is_empty()).collect();
    let name_bytes = targets
        .iter()
        .map(|t| (t.name.len() + t.path.as_os_str().len()) as u64)
        .sum();
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
        directories: pass.directories,
        placements,
        name_bytes,
        mkdirs,
        links,
        skipped: pass.skipped,
        renamed: pass.renamed,
        symlinks_skipped: pass.symlinks_skipped,
    })
}

/// Create the plan's directories, the output directory first.
pub(crate) fn make_dirs(mkdirs: &[PathBuf]) -> Result<(), CoreError> {
    for d in mkdirs {
        std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
    }
    Ok(())
}

/// Create the plan's symlinks, in entry order (after the directories, before the files). Every
/// target here passed [`policy::resolve_symlink_target`] in the policy pass; a refused one
/// never reaches this phase. On Windows the target is read as UTF-8 and the link kind is the
/// plan's ([`Link::dir`], from the archive); nothing reads the target on any system.
pub(crate) fn make_links(links: &[Link]) -> Result<(), CoreError> {
    for l in links {
        if l.replace {
            policy::remove_existing(&l.path).map_err(|e| io(&l.path, e))?;
        }
        #[cfg(test)]
        seam::record(&l.target);
        make_link(&l.path, &l.target, l.dir).map_err(|e| io(&l.path, e))?;
    }
    Ok(())
}

/// The test seam of the link phase: every target handed to the operating system by
/// [`make_links`] (the only code of the extraction that passes a target to a system call) is
/// recorded per thread, so a test can prove a refused target was never touched.
#[cfg(test)]
pub(crate) mod seam {
    use std::cell::RefCell;

    thread_local! {
        static SEEN: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(target: &[u8]) {
        SEEN.with(|s| s.borrow_mut().push(target.to_vec()));
    }

    /// The targets recorded on this thread since the last call.
    pub(crate) fn take() -> Vec<Vec<u8>> {
        SEEN.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }
}

#[cfg(unix)]
fn make_link(path: &Path, target: &[u8], _dir: bool) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), path)
}

#[cfg(windows)]
fn make_link(path: &Path, target: &[u8], dir: bool) -> std::io::Result<()> {
    let target = std::str::from_utf8(target).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "symlink target is not UTF-8",
        )
    })?;
    if dir {
        std::os::windows::fs::symlink_dir(target, path)
    } else {
        std::os::windows::fs::symlink_file(target, path)
    }
}

#[cfg(not(any(unix, windows)))]
fn make_link(_path: &Path, _target: &[u8], _dir: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks are not supported on this platform",
    ))
}

/// The pool for `archive` under `opts`: decode workers, decoded blocks in flight, file writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pool {
    /// The thread budget N (decoders plus writers never exceed it).
    pub threads: usize,
    /// Decode workers: `min(N - writers, (budget - fixed_shared) / (per_block + per_worker),
    /// needed blocks)`, at least one.
    pub workers: usize,
    /// Decoded blocks in flight (decoding, waiting or being written): the workers plus the held
    /// blocks the budget pays for (module documentation, "The memory bound").
    pub in_flight: usize,
    /// File writers: `clamp(N / 4, 1, 4)`, capped by the open-file bound and the files.
    pub writers: usize,
    /// The budget and the terms counted against it.
    pub memory: ExtractMemory,
}

/// What the plan gives the pool's sizing.
pub(crate) struct Shape<'a> {
    /// The needed blocks.
    pub(crate) needed: &'a [usize],
    /// File targets, and those with content.
    pub(crate) targets: usize,
    pub(crate) with_content: usize,
    /// Chunk placements.
    pub(crate) placements: u64,
    /// The targets' name and path bytes.
    pub(crate) name_bytes: u64,
    /// A cap on the writers: `usize::MAX` for an extraction (the module's rule applies), 1 for
    /// an export, whose one consumer is the calling thread ([`crate::export`]).
    pub(crate) max_writers: usize,
}

/// The bytes of `index` outside the shared chunk table: what each fork copies.
fn index_bytes(index: &lpk_format::Index) -> u64 {
    use std::mem::size_of;
    let n = |len: usize, each: usize| (len as u64).saturating_mul(each as u64);
    (size_of::<lpk_format::Index>() as u64)
        .saturating_add(n(
            index.blocks.len(),
            size_of::<lpk_format::BlockLocation>(),
        ))
        .saturating_add(n(
            index.recovery.len(),
            size_of::<lpk_format::FrameLocation>(),
        ))
        .saturating_add(n(index.priors.len(), 32))
        .saturating_add(n(
            index.generations.len(),
            size_of::<lpk_format::GenerationInfo>(),
        ))
}

/// The pool for `archive` under `opts` and the plan's shape, with its memory model (module
/// documentation, "The memory bound").
pub(crate) fn pool_size<R: Read + Seek>(
    archive: &Archive<R>,
    opts: &ExtractOptions,
    shape: &Shape,
) -> Pool {
    use std::mem::size_of;
    let threads = opts
        .threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
        .max(1);
    let index = archive.index();
    let env = index.envelope;
    let tier = EnvelopeTier::of(&env);
    let budget = Budget::resolve(opts.memory, tier.ceiling());
    // Per block in flight.
    let copies = if archive.is_encrypted() { 2 } else { 1 };
    let frame = |b: &usize| index.blocks.get(*b).map_or(0, |l| l.frame_len);
    let encoded = shape
        .needed
        .iter()
        .map(frame)
        .max()
        .unwrap_or(0)
        .saturating_mul(copies);
    let plain = env.max_block_plain;
    let decoder = env.decode_memory.saturating_sub(plain);
    let step_input = plain;
    let nested = if index.records.is_some() {
        let largest = index.blocks.iter().map(|l| l.frame_len).max().unwrap_or(0);
        plain.saturating_add(largest.saturating_mul(copies))
    } else {
        0
    };
    let per_block = encoded
        .saturating_add(plain)
        .saturating_add(decoder)
        .saturating_add(step_input)
        .saturating_add(nested)
        .saturating_add(BLOCK_ALLOWANCE);
    // Fixed.
    let writers = if threads == 1 {
        1
    } else {
        (threads / 4)
            .clamp(1, 4)
            .min(opts.max_open_files.max(1))
            .min(shape.with_content.max(1))
            .min(shape.max_writers.max(1))
    };
    let index_copy = index_bytes(index);
    let per_worker = index_copy.saturating_add(WORKER_ALLOWANCE);
    let n = |len: u64, each: usize| len.saturating_mul(each as u64);
    let blocks = index.blocks.len() as u64;
    let opened = index_copy
        .saturating_add(index.chunk_table.len() as u64)
        .saturating_add(n(archive.chunks().len(), 2 * size_of::<u64>()))
        .saturating_add(n(blocks, size_of::<u64>()));
    let place = size_of::<Place>();
    let lists = size_of::<Vec<Place>>() * (writers + 1) + size_of::<usize>();
    let target = 2 * size_of::<Target>() + size_of::<AtomicU8>() + size_of::<u64>();
    let plan = n(shape.placements, 4 * place)
        .saturating_add(n(blocks, lists))
        .saturating_add(n(shape.needed.len() as u64, size_of::<usize>()))
        .saturating_add(n(shape.targets as u64, target))
        .saturating_add(shape.name_bytes.saturating_mul(2));
    let records = index.records.map_or(0, |r| r.len.saturating_mul(4));
    let fixed_shared = opened
        .saturating_add(plan)
        .saturating_add((writers as u64).saturating_mul(WRITER_ALLOWANCE))
        .saturating_add(DISPATCH_ALLOWANCE)
        .saturating_add(records);
    let held = plain.saturating_add(BLOCK_ALLOWANCE);
    let workers = if threads == 1 {
        1
    } else {
        let by_memory =
            budget.bytes.saturating_sub(fixed_shared) / per_block.saturating_add(per_worker).max(1);
        let by_memory = usize::try_from(by_memory).unwrap_or(usize::MAX).max(1);
        (threads - writers)
            .min(by_memory)
            .min(shape.needed.len().max(1))
            .max(1)
    };
    let in_flight = if threads == 1 {
        1
    } else {
        let w = u64::try_from(workers).unwrap_or(u64::MAX);
        let paid =
            fixed_shared.saturating_add(w.saturating_mul(per_block.saturating_add(per_worker)));
        let extra = budget.bytes.saturating_sub(paid) / held.max(1);
        let extra = usize::try_from(extra).unwrap_or(usize::MAX);
        let cap = workers
            .saturating_mul(HELD_PER_WORKER)
            .min(shape.needed.len())
            .max(workers);
        workers.saturating_add(extra).min(cap)
    };
    let memory = ExtractMemory {
        budget,
        tier,
        per_block,
        encoded,
        plain,
        decoder,
        step_input,
        nested,
        held,
        fixed_shared,
        per_worker,
        fixed: fixed_shared.saturating_add((workers as u64).saturating_mul(per_worker)),
        key_derivation: archive
            .key_slot()
            .map_or(0, |k| u64::from(k.argon2.m_kib).saturating_mul(1024)),
    };
    Pool {
        threads,
        workers,
        in_flight,
        writers,
        memory,
    }
}

/// How a run ended.
pub(crate) enum Outcome {
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
    extract_archive_with(archive, reopen, dir, policy, opts, &Monitor::default())
}

/// [`extract_archive`] with a progress observer and a cancel token (see the module
/// documentation).
pub fn extract_archive_with<R, F>(
    archive: &mut Archive<R>,
    reopen: F,
    dir: &Path,
    policy: &dyn ExtractPolicy,
    opts: &ExtractOptions,
    monitor: &Monitor,
) -> Result<ExtractSummary, CoreError>
where
    R: Read + Seek + Send,
    F: Fn() -> std::io::Result<R>,
{
    // An encrypted archive opened without its key has no index to plan from: refused here,
    // before anything is created, so nothing is written.
    if archive.is_keyless() {
        return Err(CoreError::Format(FormatError::PasswordRequired));
    }
    let rep = monitor.reporter();
    let Plan {
        shared,
        per_block,
        needed,
        directories,
        placements,
        name_bytes,
        mkdirs,
        links,
        skipped,
        renamed,
        symlinks_skipped,
    } = plan(archive, dir, policy, &opts.select)?;
    let shape = Shape {
        needed: &needed,
        targets: shared.targets.len(),
        with_content: shared.targets.iter().filter(|t| t.chunks > 0).count(),
        placements,
        name_bytes,
        max_writers: usize::MAX,
    };
    let pool = pool_size(archive, opts, &shape);
    // A given budget below one block in flight is refused with the figure, before anything is
    // written, as the compress side refuses (D-67 (1)); a default budget that small runs one.
    let m = pool.memory;
    if m.budget.source == BudgetSource::Given && m.budget.bytes < m.total(1) {
        return Err(CoreError::InvalidOption(format!(
            "--memory {} is below what one block in flight needs: {} bytes ({} per block, {} fixed)",
            m.budget.bytes,
            m.total(1),
            m.per_block,
            m.fixed_shared.saturating_add(m.per_worker)
        )));
    }
    // Cancelled before anything exists: nothing to clean up.
    rep.check()?;
    rep.totals(Some(shared.targets.len() as u64), Some(needed.len() as u64));
    let frame_len: Vec<u64> = archive.index().blocks.iter().map(|b| b.frame_len).collect();
    make_dirs(&mkdirs)?;
    // Links are created after every directory and before any file: a failure here (no
    // privilege on Windows, a target that is not UTF-8) fails the extraction with nothing to
    // clean up, and the order is load-bearing: a directory that exists before any link is
    // made can never be a link this extraction created, so no file is written through one
    // (the plan's checks hold whatever the entry order or the target's case rules).
    make_links(&links)?;
    let first_file: Vec<usize> = per_block
        .iter()
        .map(|p| p.first().map_or(usize::MAX, |p| p.file))
        .collect();
    let decoded = AtomicU64::new(0);
    let reads = AtomicU64::new(0);
    let peak = AtomicUsize::new(0);
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
                // One thread in all: a JPEG stream decodes on this thread alone (E2-25).
                let one = Some(crate::peel::jpeg::DecodeThreads::new(1));
                let r = catch_unwind(AssertUnwindSafe(|| {
                    crate::peel::jpeg::with_decode_threads(one, || {
                        let mut out = Out::new(shared, 0, 1, opts.max_open_files);
                        let mut bytes_in = 0;
                        for (k, &b) in needed.iter().enumerate() {
                            rep.check()?;
                            let plain = archive.decode_block_checked(b);
                            decoded.fetch_add(1, Ordering::Relaxed);
                            peak.store(1, Ordering::Relaxed);
                            let plain = plain.map_err(|e| decode_error(b, e))?;
                            out.write_block(b, &per_block[b], &plain)?;
                            bytes_in += frame_len.get(b).copied().unwrap_or(0);
                            rep.set(
                                Stage::Extract,
                                shared.files.load(Ordering::Relaxed),
                                bytes_in,
                                shared.bytes.load(Ordering::Relaxed),
                                k as u64 + 1,
                            );
                        }
                        Ok(())
                    })
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
                        hooks: Hooks {
                            panic_at: opts.panic_in_dispatch_at,
                            fill_first: opts.fill_permits_before_dispatch,
                        },
                        decoded: &decoded,
                        reads: &reads,
                        peak: &peak,
                        decode_error: &decode_error,
                        rep: &rep,
                        frame_len: &frame_len,
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
    // The writers ran behind the dispatch: the last event carries the final counts.
    rep.set(
        Stage::Extract,
        shared.files.load(Ordering::Relaxed),
        needed
            .iter()
            .map(|&b| frame_len.get(b).copied().unwrap_or(0))
            .sum(),
        shared.bytes.load(Ordering::Relaxed),
        needed.len() as u64,
    );
    rep.done(Stage::Extract);
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
        skipped,
        renamed,
        symlinks: links.len() as u64,
        symlinks_skipped,
        peak_in_flight: peak.load(Ordering::Relaxed),
        memory: pool.memory,
    })
}

type Decoded = (usize, Result<Vec<u8>, FormatError>);

/// A decoded block on its way to the writers; its permit is returned when the last writer
/// drops it.
pub(crate) struct Held<'p> {
    pub(crate) block: usize,
    pub(crate) plain: Vec<u8>,
    pub(crate) permits: &'p Permits,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.permits.release();
    }
}

const WRITER_STOPPED: &str = "a file writer stopped";

/// The parallel path's test hooks ([`ExtractOptions::panic_in_dispatch_at`] and
/// [`ExtractOptions::fill_permits_before_dispatch`]).
#[derive(Clone, Copy)]
struct Hooks {
    panic_at: Option<usize>,
    fill_first: bool,
}
pub(crate) const DECODER_STOPPED: &str = "a decode worker stopped";

/// Everything a parallel run needs.
struct Run<'a, R: Read + Seek> {
    shared: &'a Shared,
    forks: Vec<Archive<R>>,
    needed: &'a [usize],
    per_block: Vec<Vec<Place>>,
    pool: Pool,
    max_open: usize,
    hooks: Hooks,
    decoded: &'a AtomicU64,
    reads: &'a AtomicU64,
    /// Receives the most permits out at once.
    peak: &'a AtomicUsize,
    decode_error: &'a (dyn Fn(usize, FormatError) -> CoreError + Sync),
    rep: &'a Reporter,
    /// Every block's frame length, for the bytes-in figure.
    frame_len: &'a [u64],
}

/// Keep the first panic payload.
pub(crate) fn keep(slot: &Mutex<Option<Box<dyn Any + Send>>>, p: Box<dyn Any + Send>) {
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
        hooks,
        decoded,
        reads,
        peak,
        decode_error,
        rep,
        frame_len,
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
    let outcome = parallel_with(
        shared,
        forks,
        needed,
        split,
        &pool,
        max_open,
        hooks,
        decoded,
        reads,
        decode_error,
        &permits,
        rep,
        frame_len,
    );
    peak.store(permits.peak.load(Ordering::SeqCst), Ordering::Relaxed);
    outcome
}

#[allow(clippy::too_many_arguments)]
fn parallel_with<R: Read + Seek + Send>(
    shared: &Shared,
    forks: Vec<Archive<R>>,
    needed: &[usize],
    split: Vec<Vec<Vec<Place>>>,
    pool: &Pool,
    max_open: usize,
    hooks: Hooks,
    decoded: &AtomicU64,
    reads: &AtomicU64,
    decode_error: &(dyn Fn(usize, FormatError) -> CoreError + Sync),
    permits: &Permits,
    rep: &Reporter,
    frame_len: &[u64],
) -> Outcome {
    let n = pool.writers.max(1);
    let panic: Mutex<Option<Box<dyn Any + Send>>> = Mutex::new(None);
    let next = AtomicUsize::new(0);
    let per_writer_open = (max_open / n).max(1);
    let token = rep.token();
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
                    if permits.is_cancelled() || token.is_cancelled() {
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
        // The decode threads never exceed N: N tokens shared by the workers and the JPEG
        // streams' partition threads (E2-25; the writers, which wait on I/O, are not counted).
        let budget = crate::peel::jpeg::DecodeThreads::new(pool.threads);
        let mut decoders = Vec::with_capacity(forks.len());
        if spawn_error.is_none() {
            for (i, mut fork) in forks.into_iter().enumerate() {
                let tx = tx.clone();
                let budget = Arc::clone(&budget);
                let job = move || -> u64 {
                    let lend = Some(Arc::clone(&budget));
                    crate::peel::jpeg::with_decode_threads(lend, || loop {
                        // The token is seen here too, so a worker stops before its next
                        // block whichever thread noticed the cancellation first.
                        if token.is_cancelled() {
                            permits.cancel();
                            break;
                        }
                        if !permits.acquire() {
                            break;
                        }
                        let k = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&b) = needed.get(k) else {
                            permits.release();
                            break;
                        };
                        let r = catch_unwind(AssertUnwindSafe(|| {
                            let _token = budget.hold();
                            fork.decode_block_checked(b)
                        }));
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
                    });
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
            // The dispatcher is this thread: a panic here must not unwind into the scope while
            // the workers wait for permits, so it is caught, cancels, and is resumed after the
            // joins and the cleanup like a worker's.
            None => catch_unwind(AssertUnwindSafe(|| {
                dispatch(
                    &rx,
                    needed,
                    split,
                    &senders,
                    permits,
                    decode_error,
                    hooks,
                    rep,
                    frame_len,
                    shared,
                )
            }))
            .unwrap_or_else(|p| {
                keep(panic, p);
                Err(CoreError::Extract("the dispatcher stopped"))
            }),
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
        // (or any other) stands; an internal stop while the token is cancelled is the
        // cancellation (a worker or a writer saw the token before the dispatcher did).
        match (r, writer_error) {
            (Err(CoreError::Extract(_)) | Ok(()), Some(w)) => Outcome::Failed(w),
            (Err(CoreError::Extract(_)), None) if token.is_cancelled() => {
                Outcome::Failed(CoreError::Cancelled)
            }
            (Err(e), _) => Outcome::Failed(e),
            (Ok(()), None) if token.is_cancelled() => Outcome::Failed(CoreError::Cancelled),
            (Ok(()), None) => Outcome::Done,
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn dispatch<'p>(
    rx: &mpsc::Receiver<Decoded>,
    needed: &[usize],
    split: &[Vec<Vec<Place>>],
    senders: &[mpsc::Sender<Arc<Held<'p>>>],
    permits: &'p Permits,
    decode_error: &dyn Fn(usize, FormatError) -> CoreError,
    hooks: Hooks,
    rep: &Reporter,
    frame_len: &[u64],
    shared: &Shared,
) -> Result<(), CoreError> {
    let mut pending: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut bytes_in = 0;
    // Test hook: wait until every permit holds a decoded block (none is released before its
    // dispatch, so the workers stop exactly there).
    while hooks.fill_first && pending.len() < permits.cap.min(needed.len()) {
        let (k, r) = rx.recv().map_err(|_| CoreError::Extract(DECODER_STOPPED))?;
        match r {
            Ok(p) => {
                pending.insert(k, p);
            }
            Err(e) => {
                let block = needed.get(k).copied().unwrap_or(0);
                return Err(decode_error(block, e));
            }
        }
    }
    for (want, &b) in needed.iter().enumerate() {
        assert!(
            hooks.panic_at != Some(want),
            "test hook: dispatcher panics at block {want}"
        );
        if rep.is_cancelled() {
            permits.cancel();
            return Err(CoreError::Cancelled);
        }
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
        bytes_in += frame_len.get(b).copied().unwrap_or(0);
        // The files and bytes are the writers' counts so far (they run behind the dispatch).
        rep.set(
            Stage::Extract,
            shared.files.load(Ordering::Relaxed),
            bytes_in,
            shared.bytes.load(Ordering::Relaxed),
            want as u64 + 1,
        );
    }
    Ok(())
}

/// Open the archive file at `path` with the full reader (revision 1.1's `jpeg-reconstruct`) and
/// the given prior files' contents, and extract it under `dir` (each decode worker opens the
/// file again). `credentials` open an encrypted archive: a wrong password is `WrongKey` at the
/// key slot and a missing one `PasswordRequired`, both before any output file or directory
/// exists; credentials given for a plain archive are ignored.
pub fn extract_file(
    path: &Path,
    dir: &Path,
    priors: &[Vec<u8>],
    credentials: Option<&Credentials>,
    policy: &dyn ExtractPolicy,
    opts: &ExtractOptions,
) -> Result<ExtractSummary, CoreError> {
    extract_file_with(
        path,
        dir,
        priors,
        credentials,
        policy,
        opts,
        &Monitor::default(),
    )
}

/// [`extract_file`] with a progress observer and a cancel token ([`extract_archive_with`]).
pub fn extract_file_with(
    path: &Path,
    dir: &Path,
    priors: &[Vec<u8>],
    credentials: Option<&Credentials>,
    policy: &dyn ExtractPolicy,
    opts: &ExtractOptions,
    monitor: &Monitor,
) -> Result<ExtractSummary, CoreError> {
    let file = File::open(path).map_err(|e| io(path, e))?;
    // `opts.memory` is the reader's decode memory too: an archive whose envelope needs more is
    // refused here, before any output exists.
    // Without `--memory` the reader is limited by the largest tier ceiling resolved on this
    // machine (the envelope, which gives the extraction's own budget, is not known yet).
    let resources = lpk_format::Resources {
        memory: opts
            .memory
            .unwrap_or_else(|| Budget::resolve(None, crate::memory::EXTRACT_CEILING).bytes),
        ..lpk_format::Resources::default()
    };
    let mut a = Archive::open_with(file, &resources, credentials)?;
    if !priors.is_empty() {
        let mut store = MemoryPriors::new();
        for p in priors {
            store.insert(p.clone());
        }
        a.set_priors(Box::new(store));
    }
    crate::register_full_reader(&mut a);
    extract_archive_with(&mut a, || File::open(path), dir, policy, opts, monitor)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::policy::{Overwrite, Policy, Symlinks, TargetRefusal as R};
    use lpk_format::EntryFlags;

    fn entry(kind: EntryKind, path: &str, target: Option<&[u8]>) -> Entry {
        Entry {
            kind,
            flags: EntryFlags::EMPTY,
            path: path.to_string(),
            mtime_ns: 0,
            size: target.map_or(0, |t| t.len() as u64),
            symlink_target: target.map(<[u8]>::to_vec),
            chunks: Vec::new(),
        }
    }

    /// E6-0, through the policy pass and the link phase (the path `.lpk` extraction and the
    /// foreign-archive broker share): every refused target is skipped, counted and recorded,
    /// never placed as a link, and never handed to the operating system — the link phase's
    /// seam ([`seam`]) records every target `make_links` passes on, and none of the refused
    /// ones is among them. The pass itself judges targets with a pure function (no path, no
    /// file-system call). In-tree links get their kind from the archive: a directory entry, an
    /// implied parent or the destination gives a directory link, anything else a file link,
    /// whatever exists on disk (here a directory named like the file entry's target exists
    /// before the extraction and is still not a directory link's reason).
    #[test]
    fn refused_targets_never_reach_the_link_phase_and_kinds_come_from_the_archive() {
        let refused: [(&str, &[u8], R); 7] = [
            ("unc", b"\\\\192.0.2.1\\share\\x", R::Unc),
            ("abs", b"C:\\x", R::Absolute),
            ("drive", b"C:x", R::DriveRelative),
            ("rooted", b"\\x", R::Rooted),
            ("slash", b"/x", R::Rooted),
            ("sub/esc", b"../../x", R::Escapes),
            ("dev", b"\\\\?\\UNC\\192.0.2.1\\share", R::DevicePath),
        ];
        let mut entries = vec![
            entry(EntryKind::File, "a.txt", None),
            entry(EntryKind::Directory, "d", None),
            entry(EntryKind::File, "e/f", None),
        ];
        for (p, t, _) in &refused {
            entries.push(entry(EntryKind::Symlink, p, Some(t)));
        }
        let fine: [(&str, &[u8], bool); 5] = [
            ("to_file", b"a.txt", false),
            ("to_dir", b"d", true),
            ("to_implied", b"e", true),
            ("sub/to_root", b"..", true),
            ("sub/missing", b"../nothing", false),
        ];
        for (p, t, _) in &fine {
            entries.push(entry(EntryKind::Symlink, p, Some(t)));
        }
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        // Something on disk named like a file entry's target: ignored for the kind.
        std::fs::create_dir_all(out.join("nothing")).unwrap();
        let policy = Policy::new(Overwrite::Refuse, Symlinks::Create);
        let pass = policy_pass(entries, &out, &policy, &Selection::all()).unwrap();
        assert_eq!(pass.symlinks_skipped, refused.len() as u64);
        let want: Vec<(String, R)> = refused
            .iter()
            .map(|(p, _, r)| (p.to_string(), *r))
            .collect();
        assert_eq!(policy.refused_symlink_targets(), want);
        let links: Vec<&Link> = pass
            .placed
            .iter()
            .filter_map(|p| match p {
                Placed::Link(l) => Some(l),
                _ => None,
            })
            .collect();
        let kinds: Vec<(&[u8], bool)> = links.iter().map(|l| (&l.target[..], l.dir)).collect();
        let want: Vec<(&[u8], bool)> = fine.iter().map(|(_, t, d)| (*t, *d)).collect();
        assert_eq!(kinds, want);
        // The link phase over the plan: only in-tree targets reach the operating system.
        make_dirs(&pass.mkdirs(&out)).unwrap();
        let owned: Vec<Link> = pass
            .placed
            .into_iter()
            .filter_map(|p| match p {
                Placed::Link(l) => Some(l),
                _ => None,
            })
            .collect();
        let _ = seam::take();
        let made = make_links(&owned);
        let seen = seam::take();
        for (_, t, _) in &refused {
            assert!(!seen.iter().any(|s| s == t), "{seen:?}");
        }
        for (p, _, _) in &refused {
            assert!(std::fs::symlink_metadata(out.join(p)).is_err(), "{p}");
        }
        match made {
            Ok(()) => assert_eq!(seen.len(), fine.len()),
            // Windows without the privilege: the first creation fails; it was in-tree.
            Err(e) => {
                if !cfg!(windows) {
                    panic!("{e:?}");
                }
                assert_eq!(seen, vec![fine[0].1.to_vec()]);
                println!("SKIPPED: link creation refused on this machine ({e})");
            }
        }
    }
}
