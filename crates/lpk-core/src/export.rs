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

fn plan<R: Read + Seek>(archive: &mut Archive<R>) -> Result<Plan, CoreError> {
    let mut entries: Vec<Entry> = archive
        .entry_table()?
        .table()?
        .iter()
        .collect::<Result<_, _>>()?;
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

/// Belady's schedule over `runs` for a cache of `capacity` blocks: marks each run a hit or a
/// miss, names the block a miss evicts, and returns the misses' blocks in order (what the
/// decoders decode). With capacity 0 every run is a miss and nothing is kept.
fn schedule(runs: &mut [Run], capacity: usize) -> Vec<usize> {
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
        if cached.contains_key(&r.block) {
            r.hit = true;
            cached.insert(r.block, next_use[i]);
            heap.push((next_use[i], r.block));
            continue;
        }
        misses.push(r.block);
        if capacity == 0 {
            continue;
        }
        if cached.len() >= capacity {
            while let Some((nu, b)) = heap.pop() {
                if cached.get(&b) == Some(&nu) {
                    cached.remove(&b);
                    r.evict = Some(b);
                    break;
                }
            }
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
    /// The decoded block of a miss, holding one permit.
    fn fetch(&mut self, block: usize) -> Result<Vec<u8>, CoreError> {
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
            Arc::clone(
                self.cache
                    .get(&r.block)
                    .ok_or(CoreError::Extract("a scheduled hit is not in the cache"))?,
            )
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
            if self.capacity > 0 {
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
    if archive.is_keyless() {
        return Err(CoreError::Format(FormatError::PasswordRequired));
    }
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
    } = plan(archive)?;
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
    let misses = schedule(&mut runs, capacity);
    let memory = ExtractMemory {
        fixed: m
            .fixed_shared
            .saturating_add((workers as u64).saturating_mul(m.per_worker)),
        ..m
    };
    let permits = Permits::new(held);
    let decoded = AtomicU64::new(0);
    let reads = AtomicU64::new(0);
    let mut counts = Counts::default();
    let fixed = Fixed {
        places: &places,
        runs: &runs,
        capacity,
        decoded: &decoded,
        first_entry: &first_entry,
        entries: &entries,
    };
    let outcome = if !parallel {
        let before = archive.decode_count();
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut stream = fixed.stream(Source::Sequential(&mut *archive), &permits);
            consume(&mut stream, sink, &ranges, &mut counts)
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
                    let mut decoders = Vec::with_capacity(forks.len());
                    let mut spawn_error = None;
                    for (i, mut fork) in forks.into_iter().enumerate() {
                        let tx = tx.clone();
                        let job = move || -> u64 {
                            loop {
                                if !permits.acquire() {
                                    break;
                                }
                                let k = next.fetch_add(1, Ordering::SeqCst);
                                let Some(&b) = misses.get(k) else {
                                    permits.release();
                                    break;
                                };
                                let r =
                                    catch_unwind(AssertUnwindSafe(|| fork.decode_block_checked(b)));
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
        visits: runs.len() as u64,
        workers,
        in_flight: held,
        peak_in_flight: permits.peak.load(Ordering::SeqCst),
        memory,
    })
}

/// Open the archive file at `path` with the full reader and the given prior files' contents,
/// and export it into `sink` (each decode worker opens the file again). `credentials` open an
/// encrypted archive as [`crate::extract_file`] does: a wrong password is `WrongKey`, a
/// missing one `PasswordRequired`, both before the sink is called.
pub fn export_file(
    path: &Path,
    priors: &[Vec<u8>],
    credentials: Option<&Credentials>,
    sink: &mut dyn Sink,
    opts: &ExtractOptions,
) -> Result<ExportSummary, CoreError> {
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
    export_archive(&mut a, || std::fs::File::open(path), sink, opts)
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
            })
            .collect()
    }

    /// Replay a schedule with a real cache set: every hit is cached, every eviction names a
    /// cached block, the cache never exceeds the capacity.
    fn replay(runs: &[Run], capacity: usize) -> usize {
        let mut cache: std::collections::HashSet<usize> = Default::default();
        let mut misses = 0;
        for r in runs {
            if r.hit {
                assert!(cache.contains(&r.block), "hit on {} not cached", r.block);
                assert_eq!(r.evict, None);
                continue;
            }
            misses += 1;
            assert!(!cache.contains(&r.block));
            if let Some(x) = r.evict {
                assert!(cache.remove(&x), "evicting {x} not cached");
            }
            if capacity > 0 {
                cache.insert(r.block);
            }
            assert!(cache.len() <= capacity);
        }
        misses
    }

    #[test]
    fn capacity_zero_misses_every_run_and_keeps_nothing() {
        let mut r = runs(&[0, 1, 0, 1]);
        let misses = schedule(&mut r, 0);
        assert_eq!(misses, [0, 1, 0, 1]);
        assert_eq!(replay(&r, 0), 4);
    }

    #[test]
    fn belady_evicts_the_block_used_farthest_away() {
        // With two slots: 0, 1, then 2 evicts 1 (0 is used next), 0 hits, 1 misses.
        let mut r = runs(&[0, 1, 2, 0, 1]);
        let misses = schedule(&mut r, 2);
        assert_eq!(misses, [0, 1, 2, 1]);
        assert_eq!(r[2].evict, Some(1));
        assert!(r[3].hit);
        assert_eq!(r[4].evict, Some(2));
        assert_eq!(replay(&r, 2), 4);
    }

    #[test]
    fn a_cache_large_enough_decodes_each_block_once() {
        let seq = [3usize, 1, 3, 2, 1, 0, 3, 2];
        let mut r = runs(&seq);
        let misses = schedule(&mut r, 4);
        assert_eq!(misses, [3, 1, 2, 0]);
        assert_eq!(replay(&r, 4), 4);
        // And a smaller cache never beats the number of distinct blocks.
        for c in 1..4 {
            let mut r = runs(&seq);
            let m = schedule(&mut r, c);
            assert_eq!(m.len(), replay(&r, c));
            assert!(m.len() >= 4);
        }
    }

    #[test]
    fn a_hit_refreshes_the_next_use_so_it_is_not_evicted_early() {
        // Capacity 2: 0, 1, 0 (hit: 0's next use is now run 4), 2 must evict 1 (next use 5)
        // rather than 0 (next use 4)? Belady evicts the farthest: 1 at 5. Then 0 hits, 1 misses.
        let mut r = runs(&[0, 1, 0, 2, 0, 1]);
        let misses = schedule(&mut r, 2);
        assert_eq!(misses, [0, 1, 2, 1]);
        assert_eq!(r[3].evict, Some(1));
        assert!(r[4].hit);
        assert_eq!(replay(&r, 2), 4);
    }
}
