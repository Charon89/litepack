//! Entry-ordered export (E3-6): the archive's entries handed to a [`Sink`] in table order, each
//! file's content streamed through a reader, the blocks decoded by [`crate::extract`]'s pool
//! under its memory bound and kept in a cache scheduled from the whole plan, so a container
//! that is written entry by entry (ZIP, 7z) is fed without a tree on disk and without a whole
//! file in memory.
//!
//! **The plan.** The entry table is read once. Every file's chunk list is resolved against the
//! chunk table as the extraction engine resolves it (indices in range, lengths adding up to the
//! file size) into *places* — (block, offset in the block, length) — in entry order; the whole
//! sequence of places is cut into *runs*, one per maximal stretch of places in one block. A run
//! is one visit of a block; the entries are served in order, so the runs are visited in order.
//! The entries are kept (the sink needs their names, times and flags) with their chunk lists
//! dropped, so what the plan holds is the entry table minus the chunks plus the places.
//!
//! **Decoding.** Block order is the extraction engine's order (D-54: each block once); entry
//! order is not — a deduplicated chunk may point back to a block visited long before. The
//! export holds `held` decoded blocks at once (the extraction engine's memory bound,
//! [`crate::extract::ExtractMemory`]: `(budget - fixed_shared) / (per_block + per_worker)`,
//! at least one, at most the distinct blocks needed) and decides *before decoding anything*
//! which visits hit the cache and which block each miss evicts, by Belady's rule over the
//! known sequence of runs (evict the block whose next visit is farthest; optimal for a given
//! capacity). A miss is decoded once per miss, so `blocks_decoded >= blocks_needed`, the
//! excess (`redecodes`) being the back references the cache could not hold.
//!
//! **Threads.** [`ExtractOptions::threads`] is the thread budget: 1 is the sequential path (the
//! calling thread decodes each miss when its run comes and keeps `held` blocks in the cache);
//! from 2 on, `min(N - 1, held)` decode workers (each with its own reader of the archive,
//! [`Archive::fork`]) decode the misses in order ahead of the calling thread, which runs the
//! sink; a worker takes the next miss only with a permit, returned when the block leaves the
//! cache (or, for a block the cache does not keep, when its run is over), so at most `held`
//! decoded blocks exist at any time (decoding, waiting, cached). The cache then keeps
//! `held - 1` blocks, so one permit is always free for the miss the calling thread waits for.
//! On the sequential path the cache keeps `held` blocks; a block used for the last time is
//! held outside it for its run, so when the cache is full such a run evicts a block first
//! (the schedule's rule, [`schedule`]) — the permit it needs cannot come from anywhere else.
//! A JPEG stream's partitions decode under the same thread budget as `lpk x` (E2-25,
//! [`crate::peel::jpeg::DecodeThreads`]): N tokens shared by the decode workers and the
//! partition threads on the parallel path; on the sequential path the calling thread alone.
//!
//! **The sink.** [`Sink::file`] receives the entry and a [`Read`] that yields the file's bytes
//! in order; the sink must read it to the end (an export where it did not is an error, since
//! the places it left are the next entry's cue). A sink's error ends the export; a decode error
//! met inside the reader is reported as the engine's [`CoreError::Decode`], not as the sink's
//! wrapping of it. The sink's own memory (its compressor) is outside the engine's count.
//!
//! **Failure.** As the extraction engine's: the first error or panic cancels the pool, every
//! thread is joined, then the error is returned or the panic resumed. What the sink wrote is
//! the sink's to remove (the command line removes the output file).
//!
//! **Progress and cancellation** ([`crate::progress`]; [`export_archive_with`]). An `Export`
//! event per entry written (files and bytes streamed, encoded bytes and blocks decoded of the
//! decodes scheduled), rate-limited. The token is polled after the plan (before the sink is
//! touched), before each entry and before each decode on the calling thread, and by each
//! decode worker before each block; a cancelled export stops the pool, joins every worker
//! and fails with [`CoreError::Cancelled`], the sink's output being its caller's to remove.
//!
//! No extraction policy applies: the export writes nothing on the user's disk tree; the
//! container's names are the archive's paths as the format validated them.

use std::any::Any;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::io::{Read, Seek};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use lpk_format::{Archive, Credentials, Entry, EntryKind, FormatError, MemoryPriors};

use crate::extract::{
    keep, pool_size, ExtractMemory, ExtractOptions, Held, Outcome, Permits, Shape, DECODER_STOPPED,
};
use crate::memory::{Budget, BudgetSource};
use crate::progress::{Monitor, Reporter, Stage};
use crate::CoreError;

/// Where an export's entries go, in entry order.
pub trait Sink {
    /// A directory entry.
    fn directory(&mut self, entry: &Entry) -> Result<(), CoreError>;
    /// A symlink entry (`entry.symlink_target` holds the target).
    fn symlink(&mut self, entry: &Entry) -> Result<(), CoreError>;
    /// A file entry; `content` yields its `entry.size` bytes and must be read to the end.
    /// `entry.chunks` is empty here: the content comes through the reader.
    fn file(&mut self, entry: &Entry, content: &mut dyn Read) -> Result<(), CoreError>;
}

/// What an export did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportSummary {
    /// File entries written.
    pub files: u64,
    /// Directory entries written.
    pub directories: u64,
    /// Symlink entries written.
    pub symlinks: u64,
    /// File bytes streamed.
    pub bytes: u64,
    /// Blocks in the archive.
    pub blocks: u64,
    /// Distinct blocks some file needs.
    pub blocks_needed: u64,
    /// Blocks decoded: one per cache miss (`>= blocks_needed`).
    pub blocks_decoded: u64,
    /// Decodes beyond one per needed block: back references the cache could not hold.
    pub redecodes: u64,
    /// Lower blocks decoded again inside a reconstruction (a JPEG record's parts).
    pub nested_decodes: u64,
    /// Chunk placements (a deduplicated chunk counts once per place).
    pub placements: u64,
    /// The bytes of every entry's path (what a container's directory holds per name).
    pub name_bytes: u64,
    /// Block visits: maximal stretches of consecutive places in one block.
    pub visits: u64,
    /// Decode workers (1 on the sequential path).
    pub workers: usize,
    /// Decoded blocks held at once at most: the cache and the blocks in transit.
    pub in_flight: usize,
    /// Most decoded blocks held at once, measured.
    pub peak_in_flight: usize,
    /// The memory budget and what the engine counts against it (`fixed` with this export's
    /// workers; the held blocks are `in_flight`, not `workers`, so the counted total is
    /// `fixed + in_flight * per_block`).
    pub memory: ExtractMemory,
}

impl ExportSummary {
    /// What the export counts against its budget: `fixed + in_flight * per_block`.
    pub fn counted(&self) -> u64 {
        let m = &self.memory;
        m.fixed
            .saturating_add((self.in_flight as u64).saturating_mul(m.per_block))
    }
}

/// One chunk's place: its block and its bytes in the block.
#[derive(Debug, Clone, Copy)]
struct Place {
    block: usize,
    at: usize,
    len: usize,
}

/// One visit of a block: the places `[start of the run, end)` lie in it. `hit` and `evict`
/// are the schedule's decisions (filled by [`schedule`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    block: usize,
    /// The index past the run's last place.
    end: usize,
    /// The block is in the cache when the run comes.
    hit: bool,
    /// The cached block to drop before this run's block is decoded.
    evict: Option<usize>,
    /// No later run uses the block: it is dropped after this run instead of cached, so its
    /// permit goes back to the decoders.
    last: bool,
}

struct Plan {
    /// The entries, chunk lists dropped.
    entries: Vec<Entry>,
    /// Per entry, its places `[start, end)` (empty for a directory or a symlink).
    ranges: Vec<(usize, usize)>,
    places: Vec<Place>,
    runs: Vec<Run>,
    /// The distinct blocks needed, in index order.
    needed: Vec<usize>,
    /// Per block, the first entry with a place in it (for a decode error's name).
    first_entry: Vec<usize>,
    files: usize,
    with_content: usize,
    name_bytes: u64,
}

fn plan<R: Read + Seek>(
    archive: &mut Archive<R>,
    select: &crate::select::Selection,
) -> Result<Plan, CoreError> {
    let entries: Vec<Entry> = archive
        .entry_table()?
        .table()?
        .iter()
        .collect::<Result<_, _>>()?;
    // The selection first (as `x` plans): a pattern without a match is refused here.
    let mut entries = select.filter(entries, |e| e.path.as_str())?;
    let table = archive.chunks();
    let blocks = archive.index().blocks.len();
    let mut places = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut ranges = Vec::with_capacity(entries.len());
    let mut first_entry = vec![usize::MAX; blocks];
    let (mut files, mut with_content) = (0, 0);
    let mut name_bytes = 0u64;
    for (i, e) in entries.iter_mut().enumerate() {
        name_bytes += e.path.len() as u64;
        let start = places.len();
        match e.kind {
            EntryKind::Directory | EntryKind::Symlink => {}
            EntryKind::File => {
                files += 1;
                if !e.chunks.is_empty() {
                    with_content += 1;
                }
                let mut off = 0u64;
                for &c in &e.chunks {
                    let len = table.len();
                    let place = table
                        .locate(c)
                        .ok_or(FormatError::ChunkIndexOutOfRange { chunk: c, len })?;
                    let block = place.block;
                    let bad = || FormatError::BlockLengthMismatch { block };
                    if block >= blocks {
                        return Err(bad().into());
                    }
                    if first_entry[block] == usize::MAX {
                        first_entry[block] = i;
                    }
                    places.push(Place {
                        block,
                        at: usize::try_from(place.offset_in_block).map_err(|_| bad())?,
                        len: usize::try_from(place.plain_len).map_err(|_| bad())?,
                    });
                    match runs.last_mut() {
                        Some(r) if r.block == block => r.end = places.len(),
                        _ => runs.push(Run {
                            block,
                            end: places.len(),
                            hit: false,
                            evict: None,
                            last: false,
                        }),
                    }
                    off =
                        off.checked_add(place.plain_len)
                            .ok_or(FormatError::FileSizeMismatch {
                                expected: e.size,
                                found: u64::MAX,
                            })?;
                }
                if off != e.size {
                    return Err(FormatError::FileSizeMismatch {
                        expected: e.size,
                        found: off,
                    }
                    .into());
                }
                e.chunks = Vec::new();
            }
        }
        ranges.push((start, places.len()));
    }
    let needed: Vec<usize> = (0..blocks)
        .filter(|&b| first_entry[b] != usize::MAX)
        .collect();
    Ok(Plan {
        entries,
        ranges,
        places,
        runs,
        needed,
        first_entry,
        files,
        with_content,
        name_bytes,
    })
}

/// Belady's schedule over `runs` for a cache of `capacity` blocks among `held` permits
/// (`held >= capacity`): marks each run a hit or a miss, names the block a miss evicts,
/// marks a run after which its block is never used again (dropped, not cached), and returns
/// the misses' blocks in order (what the decoders decode). With capacity 0 every run is a
/// miss and nothing is kept.
///
/// A last-use miss is not cached, but its block holds a permit for its run; when the cache
/// already holds every permit (`capacity == held`, the sequential path), such a run evicts
/// the farthest block too — else the decode would wait for a permit only the cache can
/// return. The invariant the engine relies on: at every run, the cached blocks plus the
/// run's block (when it is not one of them) never exceed `held`.
fn schedule(runs: &mut [Run], capacity: usize, held: usize) -> Vec<usize> {
    debug_assert!(held >= capacity);
    // The next run of each block after run i (`usize::MAX`: none).
    let mut next_use = vec![usize::MAX; runs.len()];
    let mut later: HashMap<usize, usize> = HashMap::new();
    for i in (0..runs.len()).rev() {
        if let Some(&j) = later.get(&runs[i].block) {
            next_use[i] = j;
        }
        later.insert(runs[i].block, i);
    }
    let mut cached: HashMap<usize, usize> = HashMap::new();
    // (next use, block); an entry is stale when the block's recorded next use differs.
    let mut heap: BinaryHeap<(usize, usize)> = BinaryHeap::new();
    let mut misses = Vec::new();
    for (i, r) in runs.iter_mut().enumerate() {
        r.hit = false;
        r.evict = None;
        r.last = next_use[i] == usize::MAX;
        if let Some(nu) = cached.get_mut(&r.block) {
            r.hit = true;
            if r.last {
                cached.remove(&r.block);
            } else {
                *nu = next_use[i];
                heap.push((next_use[i], r.block));
            }
            continue;
        }
        misses.push(r.block);
        // The run's block takes a permit of its own unless this miss caches it, so the room
        // the cache must leave is one of `held` for a last-use block (held for its run only)
        // and one of `capacity` for a block the cache keeps.
        let room = if r.last { held } else { capacity };
        if cached.len() >= room {
            while let Some((nu, b)) = heap.pop() {
                if cached.get(&b) == Some(&nu) {
                    cached.remove(&b);
                    r.evict = Some(b);
                    break;
                }
            }
        }
        if capacity == 0 || r.last {
            continue;
        }
        cached.insert(r.block, next_use[i]);
        heap.push((next_use[i], r.block));
    }
    misses
}

type Decoded = (usize, Result<Vec<u8>, FormatError>);

/// Where the calling thread gets a missed block from.
enum Source<'a, R: Read + Seek> {
    /// The sequential path: decoded here.
    Sequential(&'a mut Archive<R>),
    /// The pool: received in miss order, reordered when the workers finish out of order.
    Parallel {
        rx: &'a mpsc::Receiver<Decoded>,
        pending: BTreeMap<usize, Vec<u8>>,
        /// The next miss's position.
        pos: usize,
        misses: &'a [usize],
    },
}

/// What the plan holds for the calling thread, whichever path runs.
struct Fixed<'a> {
    rep: &'a Reporter,
    /// Every block's frame length, for the bytes-in figure.
    frame_len: &'a [u64],
    bytes_in: &'a AtomicU64,
    places: &'a [Place],
    runs: &'a [Run],
    capacity: usize,
    decoded: &'a AtomicU64,
    first_entry: &'a [usize],
    entries: &'a [Entry],
}

impl<'a> Fixed<'a> {
    fn stream<'p, R: Read + Seek>(
        &self,
        source: Source<'a, R>,
        permits: &'p Permits,
    ) -> Stream<'a, 'p, R> {
        Stream {
            source,
            permits,
            rep: self.rep,
            frame_len: self.frame_len,
            bytes_in: self.bytes_in,
            places: self.places,
            runs: self.runs,
            capacity: self.capacity,
            cache: HashMap::new(),
            current: None,
            run_i: 0,
            loaded: false,
            place_i: 0,
            off: 0,
            decoded: self.decoded,
            first_entry: self.first_entry,
            entries: self.entries,
            failed: None,
        }
    }
}

/// A decode error named with its block and the first entry placed in it.
fn decode_error(
    first_entry: &[usize],
    entries: &[Entry],
    block: usize,
    source: FormatError,
) -> CoreError {
    CoreError::Decode {
        block,
        path: first_entry
            .get(block)
            .and_then(|&i| entries.get(i))
            .map(|e| e.path.clone())
            .unwrap_or_default(),
        source,
    }
}

/// The calling thread's view of the decoded blocks: the schedule, the cache, the current
/// run's block, and the cursor over the places.
struct Stream<'a, 'p, R: Read + Seek> {
    source: Source<'a, R>,
    permits: &'p Permits,
    rep: &'a Reporter,
    frame_len: &'a [u64],
    bytes_in: &'a AtomicU64,
    places: &'a [Place],
    runs: &'a [Run],
    capacity: usize,
    cache: HashMap<usize, Arc<Held<'p>>>,
    current: Option<Arc<Held<'p>>>,
    /// The run `current` belongs to, and whether it is loaded.
    run_i: usize,
    loaded: bool,
    /// The next place to serve and the bytes of it already served.
    place_i: usize,
    off: usize,
    decoded: &'a AtomicU64,
    first_entry: &'a [usize],
    entries: &'a [Entry],
    /// The first engine error met inside a sink's read (returned in place of the sink's).
    failed: Option<CoreError>,
}

impl<R: Read + Seek> Stream<'_, '_, R> {
    /// The decoded block of a miss, holding one permit. The token is polled first: between
    /// blocks on the calling thread.
    fn fetch(&mut self, block: usize) -> Result<Vec<u8>, CoreError> {
        self.rep.check()?;
        self.bytes_in.fetch_add(
            self.frame_len.get(block).copied().unwrap_or(0),
            Ordering::Relaxed,
        );
        let (permits, first_entry, entries) = (self.permits, self.first_entry, self.entries);
        match &mut self.source {
            Source::Sequential(archive) => {
                if !permits.acquire() {
                    return Err(CoreError::Extract("the export was cancelled"));
                }
                let r = archive.decode_block_checked(block);
                self.decoded.fetch_add(1, Ordering::Relaxed);
                r.map_err(|e| {
                    permits.release();
                    decode_error(first_entry, entries, block, e)
                })
            }
            Source::Parallel {
                rx,
                pending,
                pos,
                misses,
            } => {
                let want = *pos;
                *pos += 1;
                if misses.get(want) != Some(&block) {
                    return Err(CoreError::Extract("the misses are out of order"));
                }
                loop {
                    if let Some(p) = pending.remove(&want) {
                        return Ok(p);
                    }
                    if permits.is_cancelled() {
                        return Err(CoreError::Extract(DECODER_STOPPED));
                    }
                    let (k, r) = rx.recv().map_err(|_| CoreError::Extract(DECODER_STOPPED))?;
                    match r {
                        Ok(p) => {
                            pending.insert(k, p);
                        }
                        Err(e) => {
                            let b = misses.get(k).copied().unwrap_or(block);
                            return Err(decode_error(first_entry, entries, b, e));
                        }
                    }
                }
            }
        }
    }

    /// Make the run's block current: from the cache on a hit, else decoded (after the
    /// scheduled eviction) and cached when the cache keeps blocks.
    fn load(&mut self, run: usize) -> Result<(), CoreError> {
        // A block the cache does not keep is held for its run only: its permit goes back
        // before the next miss is fetched.
        self.current = None;
        let r = self.runs[run];
        let held = if r.hit {
            let held = Arc::clone(
                self.cache
                    .get(&r.block)
                    .ok_or(CoreError::Extract("a scheduled hit is not in the cache"))?,
            );
            if r.last {
                self.cache.remove(&r.block);
            }
            held
        } else {
            if let Some(x) = r.evict {
                self.cache.remove(&x);
            }
            let plain = self.fetch(r.block)?;
            let held = Arc::new(Held {
                block: r.block,
                plain,
                permits: self.permits,
            });
            if self.capacity > 0 && !r.last {
                self.cache.insert(r.block, Arc::clone(&held));
            }
            held
        };
        self.current = Some(held);
        self.loaded = true;
        Ok(())
    }

    /// Copy the next bytes of the places before `end` into `buf`; 0 at `end`.
    fn read_into(&mut self, end: usize, buf: &mut [u8]) -> Result<usize, CoreError> {
        if self.place_i >= end || buf.is_empty() {
            return Ok(0);
        }
        let p = self.places[self.place_i];
        while self.place_i >= self.runs[self.run_i].end {
            self.run_i += 1;
            self.loaded = false;
        }
        if !self.loaded {
            self.load(self.run_i)?;
        }
        let n = {
            let held = self
                .current
                .as_ref()
                .ok_or(CoreError::Extract("no current block"))?;
            let data =
                p.at.checked_add(p.len)
                    .and_then(|e| held.plain.get(p.at..e))
                    .ok_or(FormatError::BlockLengthMismatch { block: p.block })?;
            let rest = &data[self.off..];
            let n = rest.len().min(buf.len());
            buf[..n].copy_from_slice(&rest[..n]);
            n
        };
        self.off += n;
        if self.off == p.len {
            self.place_i += 1;
            self.off = 0;
        }
        Ok(n)
    }
}

/// A file's content as the sink reads it.
struct Content<'s, 'a, 'p, R: Read + Seek> {
    stream: &'s mut Stream<'a, 'p, R>,
    end: usize,
}

impl<R: Read + Seek> Read for Content<'_, '_, '_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.stream.read_into(self.end, buf) {
            Ok(n) => Ok(n),
            Err(e) => {
                let text = e.to_string();
                self.stream.failed.get_or_insert(e);
                Err(std::io::Error::other(text))
            }
        }
    }
}

/// The counts the calling thread keeps.
#[derive(Default)]
struct Counts {
    files: u64,
    bytes: u64,
    directories: u64,
    symlinks: u64,
}

fn consume<R: Read + Seek>(
    stream: &mut Stream<'_, '_, R>,
    sink: &mut dyn Sink,
    ranges: &[(usize, usize)],
    counts: &mut Counts,
) -> Result<(), CoreError> {
    let entries = stream.entries;
    for (i, e) in entries.iter().enumerate() {
        // Between entries on the calling thread.
        stream.rep.check()?;
        match e.kind {
            EntryKind::Directory => {
                sink.directory(e)?;
                counts.directories += 1;
            }
            EntryKind::Symlink => {
                sink.symlink(e)?;
                counts.symlinks += 1;
            }
            EntryKind::File => {
                let (start, end) = ranges[i];
                if stream.place_i != start || stream.off != 0 {
                    return Err(CoreError::Extract("the places are out of order"));
                }
                let r = {
                    let mut content = Content {
                        stream: &mut *stream,
                        end,
                    };
                    sink.file(e, &mut content)
                };
                if let Some(engine) = stream.failed.take() {
                    return Err(engine);
                }
                r?;
                if stream.place_i != end {
                    return Err(CoreError::Extract("the sink did not read the whole file"));
                }
                counts.files += 1;
                counts.bytes += e.size;
            }
        }
        stream.rep.set(
            Stage::Export,
            counts.files,
            stream.bytes_in.load(Ordering::Relaxed),
            counts.bytes,
            stream.decoded.load(Ordering::Relaxed),
        );
    }
    // The last run's block goes back before the pool is joined.
    stream.current = None;
    stream.cache.clear();
    Ok(())
}

/// Export every entry of `archive` into `sink`, in entry order. `reopen` gives another
/// reader of the same archive bytes for each decode worker (called only when the thread
/// budget is more than one). See the module documentation.
pub fn export_archive<R, F>(
    archive: &mut Archive<R>,
    reopen: F,
    sink: &mut dyn Sink,
    opts: &ExtractOptions,
) -> Result<ExportSummary, CoreError>
where
    R: Read + Seek + Send,
    F: Fn() -> std::io::Result<R>,
{
    export_archive_with(archive, reopen, sink, opts, &Monitor::default())
}

/// [`export_archive`] with a progress observer and a cancel token (see the module
/// documentation).
pub fn export_archive_with<R, F>(
    archive: &mut Archive<R>,
    reopen: F,
    sink: &mut dyn Sink,
    opts: &ExtractOptions,
    monitor: &Monitor,
) -> Result<ExportSummary, CoreError>
where
    R: Read + Seek + Send,
    F: Fn() -> std::io::Result<R>,
{
    if archive.is_keyless() {
        return Err(CoreError::Format(FormatError::PasswordRequired));
    }
    let rep = monitor.reporter();
    let Plan {
        entries,
        ranges,
        places,
        mut runs,
        needed,
        first_entry,
        files,
        with_content,
        name_bytes,
    } = plan(archive, &opts.select)?;
    let shape = Shape {
        needed: &needed,
        targets: files,
        with_content,
        placements: places.len() as u64,
        name_bytes,
        max_writers: 1,
    };
    let pool = pool_size(archive, opts, &shape);
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
    let by_memory = m.budget.bytes.saturating_sub(m.fixed_shared)
        / m.per_block.saturating_add(m.per_worker).max(1);
    let parallel = pool.threads >= 2 && !needed.is_empty();
    // On the parallel path one held block is the one in transit, so a cache of every needed
    // block takes one more.
    let held = usize::try_from(by_memory)
        .unwrap_or(usize::MAX)
        .clamp(1, needed.len().max(1) + usize::from(parallel));
    let workers = if parallel {
        (pool.threads - 1).min(held).max(1)
    } else {
        1
    };
    let capacity = if parallel { held - 1 } else { held };
    let misses = schedule(&mut runs, capacity, held);
    let memory = ExtractMemory {
        fixed: m
            .fixed_shared
            .saturating_add((workers as u64).saturating_mul(m.per_worker)),
        ..m
    };
    // Cancelled before the sink is touched: nothing was written.
    rep.check()?;
    rep.totals(Some(files as u64), Some(misses.len() as u64));
    let frame_len: Vec<u64> = archive.index().blocks.iter().map(|b| b.frame_len).collect();
    let token = rep.token();
    let permits = Permits::new(held);
    let decoded = AtomicU64::new(0);
    let reads = AtomicU64::new(0);
    let bytes_in = AtomicU64::new(0);
    let mut counts = Counts::default();
    let fixed = Fixed {
        rep: &rep,
        frame_len: &frame_len,
        bytes_in: &bytes_in,
        places: &places,
        runs: &runs,
        capacity,
        decoded: &decoded,
        first_entry: &first_entry,
        entries: &entries,
    };
    let outcome = if !parallel {
        let before = archive.decode_count();
        // One thread in all: a JPEG stream decodes on the calling thread alone (E2-25).
        let one = Some(crate::peel::jpeg::DecodeThreads::new(1));
        let r = catch_unwind(AssertUnwindSafe(|| {
            crate::peel::jpeg::with_decode_threads(one, || {
                let mut stream = fixed.stream(Source::Sequential(&mut *archive), &permits);
                consume(&mut stream, sink, &ranges, &mut counts)
            })
        }));
        reads.fetch_add(archive.decode_count() - before, Ordering::Relaxed);
        match r {
            Ok(Ok(())) => Outcome::Done,
            Ok(Err(e)) => Outcome::Failed(e),
            Err(p) => Outcome::Panicked(p),
        }
    } else {
        let forks = (0..workers)
            .map(|_| reopen().map(|reader| archive.fork(reader)))
            .collect::<std::io::Result<Vec<_>>>();
        match forks {
            Err(e) => Outcome::Failed(FormatError::from(e).into()),
            Ok(forks) => {
                let panic: Mutex<Option<Box<dyn Any + Send>>> = Mutex::new(None);
                let next = AtomicUsize::new(0);
                std::thread::scope(|s| {
                    let (permits, panic, next, misses, decoded, fixed) =
                        (&permits, &panic, &next, &misses[..], &decoded, &fixed);
                    let (tx, rx) = mpsc::channel::<Decoded>();
                    // The decode threads never exceed N: N tokens shared by the workers and
                    // the JPEG streams' partition threads, as `lpk x` (E2-25).
                    let budget = crate::peel::jpeg::DecodeThreads::new(pool.threads);
                    let mut decoders = Vec::with_capacity(forks.len());
                    let mut spawn_error = None;
                    for (i, mut fork) in forks.into_iter().enumerate() {
                        let tx = tx.clone();
                        let budget = Arc::clone(&budget);
                        let job = move || -> u64 {
                            let lend = Some(Arc::clone(&budget));
                            crate::peel::jpeg::with_decode_threads(lend, || loop {
                                if token.is_cancelled() {
                                    permits.cancel();
                                    break;
                                }
                                if !permits.acquire() {
                                    break;
                                }
                                let k = next.fetch_add(1, Ordering::SeqCst);
                                let Some(&b) = misses.get(k) else {
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
                            .name(format!("lpk-export-decode-{i}"))
                            .spawn_scoped(s, job)
                        {
                            Ok(h) => decoders.push(h),
                            Err(e) => {
                                spawn_error = Some(e);
                                break;
                            }
                        }
                    }
                    drop(tx);
                    let r = match spawn_error {
                        Some(e) => Err(FormatError::from(e).into()),
                        None => catch_unwind(AssertUnwindSafe(|| {
                            let source = Source::<R>::Parallel {
                                rx: &rx,
                                pending: BTreeMap::new(),
                                pos: 0,
                                misses,
                            };
                            let mut stream = fixed.stream(source, permits);
                            consume(&mut stream, sink, &ranges, &mut counts)
                        }))
                        .unwrap_or_else(|p| {
                            keep(panic, p);
                            Err(CoreError::Extract("the export stopped"))
                        }),
                    };
                    // Stop whatever still runs (on success the decoders have ended), then
                    // join every worker.
                    permits.cancel();
                    drop(rx);
                    for h in decoders {
                        match h.join() {
                            Ok(count) => {
                                reads.fetch_add(count, Ordering::Relaxed);
                            }
                            Err(p) => keep(panic, p),
                        }
                    }
                    if let Some(p) = panic.lock().ok().and_then(|mut p| p.take()) {
                        return Outcome::Panicked(p);
                    }
                    match r {
                        // A worker saw the token first: its stop is the cancellation.
                        Err(CoreError::Extract(_)) if token.is_cancelled() => {
                            Outcome::Failed(CoreError::Cancelled)
                        }
                        Ok(()) => Outcome::Done,
                        Err(e) => Outcome::Failed(e),
                    }
                })
            }
        }
    };
    match outcome {
        Outcome::Done => {}
        Outcome::Failed(e) => return Err(e),
        Outcome::Panicked(p) => resume_unwind(p),
    }
    rep.done(Stage::Export);
    let decoded = decoded.load(Ordering::Relaxed);
    Ok(ExportSummary {
        files: counts.files,
        directories: counts.directories,
        symlinks: counts.symlinks,
        bytes: counts.bytes,
        blocks: archive.index().blocks.len() as u64,
        blocks_needed: needed.len() as u64,
        blocks_decoded: decoded,
        redecodes: decoded.saturating_sub(needed.len() as u64),
        nested_decodes: reads.load(Ordering::Relaxed).saturating_sub(decoded),
        placements: places.len() as u64,
        name_bytes,
        visits: runs.len() as u64,
        workers,
        in_flight: held,
        peak_in_flight: permits.peak.load(Ordering::SeqCst),
        memory,
    })
}

/// Open the archive file at `path` with the full reader (revision 1.1's `jpeg-reconstruct`)
/// and the given prior files' contents, as [`export_file`] does before exporting: a wrong
/// password is `WrongKey` at the key slot and a missing one `PasswordRequired`; `opts.memory`
/// is the reader's decode memory too, so an archive whose envelope needs more is refused
/// here. Nothing is written by this step.
pub fn open_file(
    path: &Path,
    priors: &[Vec<u8>],
    credentials: Option<&Credentials>,
    opts: &ExtractOptions,
) -> Result<Archive<std::fs::File>, CoreError> {
    let file = std::fs::File::open(path).map_err(|e| CoreError::io(path, e))?;
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
    Ok(a)
}

/// [`open_file`], then [`export_archive`] into `sink` (each decode worker opens the file
/// again).
pub fn export_file(
    path: &Path,
    priors: &[Vec<u8>],
    credentials: Option<&Credentials>,
    sink: &mut dyn Sink,
    opts: &ExtractOptions,
) -> Result<ExportSummary, CoreError> {
    export_file_with(path, priors, credentials, sink, opts, &Monitor::default())
}

/// [`export_file`] with a progress observer and a cancel token ([`export_archive_with`]).
pub fn export_file_with(
    path: &Path,
    priors: &[Vec<u8>],
    credentials: Option<&Credentials>,
    sink: &mut dyn Sink,
    opts: &ExtractOptions,
    monitor: &Monitor,
) -> Result<ExportSummary, CoreError> {
    let mut a = open_file(path, priors, credentials, opts)?;
    export_archive_with(&mut a, || std::fs::File::open(path), sink, opts, monitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(blocks: &[usize]) -> Vec<Run> {
        blocks
            .iter()
            .enumerate()
            .map(|(i, &b)| Run {
                block: b,
                end: i + 1,
                hit: false,
                evict: None,
                last: false,
            })
            .collect()
    }

    /// Replay a schedule with a real cache set: every hit is cached, every eviction names a
    /// cached block, the cache never exceeds the capacity, and the permits out at a run (the
    /// cache plus the run's block when the cache does not hold it) never exceed `held`.
    fn replay(runs: &[Run], capacity: usize, held: usize) -> usize {
        let mut cache: std::collections::HashSet<usize> = Default::default();
        let mut misses = 0;
        for (i, r) in runs.iter().enumerate() {
            let used_later = runs[i + 1..].iter().any(|l| l.block == r.block);
            assert_eq!(r.last, !used_later, "run {i}");
            if r.hit {
                assert!(cache.contains(&r.block), "hit on {} not cached", r.block);
                assert_eq!(r.evict, None);
                if r.last {
                    cache.remove(&r.block);
                }
                continue;
            }
            misses += 1;
            assert!(!cache.contains(&r.block));
            if let Some(x) = r.evict {
                assert!(cache.remove(&x), "evicting {x} not cached");
            }
            let kept = capacity > 0 && !r.last;
            if kept {
                cache.insert(r.block);
            }
            assert!(cache.len() <= capacity, "run {i}");
            assert!(
                cache.len() + usize::from(!kept) <= held,
                "run {i}: {} cached and the run's block held, {held} permits",
                cache.len()
            );
        }
        misses
    }

    #[test]
    fn capacity_zero_misses_every_run_and_keeps_nothing() {
        let mut r = runs(&[0, 1, 0, 1]);
        let misses = schedule(&mut r, 0, 1);
        assert_eq!(misses, [0, 1, 0, 1]);
        assert_eq!(replay(&r, 0, 1), 4);
    }

    #[test]
    fn belady_evicts_the_block_used_farthest_away() {
        // With two slots: 0 and 1 are cached; 2 is never used again, so it is dropped after
        // its run and evicts nothing; 0 and 1 then hit (each its last use: dropped).
        let mut r = runs(&[0, 1, 2, 0, 1]);
        let misses = schedule(&mut r, 2, 3);
        assert_eq!(misses, [0, 1, 2]);
        assert_eq!(r[2].evict, None);
        assert!(r[2].last);
        assert!(r[3].hit && r[3].last);
        assert!(r[4].hit && r[4].last);
        assert_eq!(replay(&r, 2, 3), 3);
        // With one slot the same sequence: block 1 is held while its run is written, so it
        // evicts 0 (used again at run 3); 2 is dropped after its run; 0 misses again (its last
        // use: dropped) and 1 hits.
        let mut r = runs(&[0, 1, 2, 0, 1]);
        let misses = schedule(&mut r, 1, 2);
        assert_eq!(misses, [0, 1, 2, 0]);
        assert_eq!(r[1].evict, Some(0));
        assert!(r[4].hit && r[4].last);
        assert_eq!(replay(&r, 1, 2), 4);
    }

    #[test]
    fn a_last_use_miss_evicts_when_the_cache_holds_every_permit() {
        // The sequential path: `held == capacity`. Two slots, 0 and 1 cached and holding
        // both permits; 2 is used once, so it is not cached, but it needs a permit for its
        // run: the farthest block (1, next used at run 4) goes. Before this rule the run
        // waited for a permit only the cache could return (the hang of E3-6's tests).
        let mut r = runs(&[0, 1, 2, 0, 1]);
        let misses = schedule(&mut r, 2, 2);
        assert_eq!(misses, [0, 1, 2, 1]);
        assert!(r[2].last && !r[2].hit);
        assert_eq!(r[2].evict, Some(1));
        assert!(r[3].hit && r[3].last);
        assert!(!r[4].hit && r[4].last);
        assert_eq!(replay(&r, 2, 2), 4);
        // One slot, one permit: every run is a miss and the last-use runs evict the one
        // cached block first.
        let mut r = runs(&[0, 1, 2, 0, 1]);
        let misses = schedule(&mut r, 1, 1);
        assert_eq!(misses, [0, 1, 2, 0, 1]);
        assert_eq!(r[1].evict, Some(0));
        assert_eq!(r[2].evict, Some(1));
        assert_eq!(r[3].evict, None);
        assert_eq!(replay(&r, 1, 1), 5);
        // A last-use miss into a cache with room evicts nothing.
        let mut r = runs(&[0, 2, 0]);
        let misses = schedule(&mut r, 2, 2);
        assert_eq!(misses, [0, 2]);
        assert_eq!(r[1].evict, None);
        assert_eq!(replay(&r, 2, 2), 2);
    }

    #[test]
    fn a_cache_large_enough_decodes_each_block_once() {
        let seq = [3usize, 1, 3, 2, 1, 0, 3, 2];
        let mut r = runs(&seq);
        let misses = schedule(&mut r, 4, 5);
        assert_eq!(misses, [3, 1, 2, 0]);
        assert_eq!(replay(&r, 4, 5), 4);
        // And a smaller cache never beats the number of distinct blocks, with the permits
        // one more than the cache (the parallel path) or as many (the sequential path).
        for c in 1..4 {
            for held in [c + 1, c] {
                let mut r = runs(&seq);
                let m = schedule(&mut r, c, held);
                assert_eq!(m.len(), replay(&r, c, held));
                assert!(m.len() >= 4);
            }
        }
    }

    #[test]
    fn a_hit_refreshes_the_next_use_so_it_is_not_evicted_early() {
        // Capacity 2: 0, 1, then 0 hits (its next use moves to run 4); 2 is never used
        // again, so it is dropped and evicts nothing; 0 and 1 hit on their last uses.
        let mut r = runs(&[0, 1, 0, 2, 0, 1]);
        let misses = schedule(&mut r, 2, 3);
        assert_eq!(misses, [0, 1, 2]);
        assert!(r[2].hit && !r[2].last);
        assert_eq!(r[3].evict, None, "2 is last: not cached, nothing evicted");
        assert!(r[4].hit && r[4].last);
        assert!(r[5].hit && r[5].last);
        assert_eq!(replay(&r, 2, 3), 3);
        // With one slot: 0, 1 evicts 0, 0 evicts 1, 2 (last, dropped), 0 hit and last, 1 miss.
        let mut r = runs(&[0, 1, 0, 2, 0, 1]);
        let misses = schedule(&mut r, 1, 2);
        assert_eq!(misses, [0, 1, 0, 2, 1]);
        assert_eq!(replay(&r, 1, 2), 5);
    }
}
