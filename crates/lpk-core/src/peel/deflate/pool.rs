//! The peel pool (design `docs/notes/e2-10-peel-pool-design.md`, sections 1, 2 and 4): W
//! worker threads read the files the run peels and run the pure half of their peel on them
//! ahead of the feeding thread ([`prepare`] for the Deflate peel, the stage's `peel` for the
//! JPEG peel, on a copy of the stage with one Lepton processor thread), which takes each
//! result in input order and commits it ([`super::stage::commit`]; the JPEG peel's dedup and
//! writes), so every writer-dependent decision and every write stays on the feeding thread in
//! input order and the archive does not depend on the thread count.
//!
//! A JPEG file holds `2 × len(f) +` [`crate::peel::MODEL_ALLOWANCE`] from its admission to the
//! end of its commit and reserves no stream.
//!
//! **Memory.** A file holds `h(f) = 2 × len(f)` from its admission to the end of its commit; a
//! stream reserves `r(p) = max(memory_term(p, 0), p + part.memory_for(p))` (p its plain bound,
//! at most R) from its analysis to the end of its encode. Admission is a byte semaphore over P:
//! file k enters, in input order, when `used + h(k) + R ≤ P` and fewer than `4 × W` files are
//! admitted and uncommitted; a stream of any file but the oldest uncommitted one reserves when
//! `used + r + R ≤ P`, the oldest file's when `used + r ≤ P`, so the oldest file always makes
//! progress (no deadlock; `P ≥ 2 × largest file + R`).
//!
//! **Errors and panics** stay attached to their file or stream and are raised by the commit
//! only where the sequential run would have met them. Dropping the pool (a finished, failed or
//! unwinding run) cancels the workers, which stop at their next stream (waiting at most one
//! encode each), and joins them. A pool none of whose threads could start is not built: the run
//! falls back to the sequential path.

use std::collections::{BTreeMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;

use super::stage::{
    fresh_hook, prepare, stream_key, Failure, Hooks, Prepared, Published, Settings, StreamKey,
};
use super::{memory_term, PartEncoder};
use crate::error::CoreError;
use crate::ingest::Input;
use crate::peel::{Fallback, PeelPlan, PeelStage, MODEL_ALLOWANCE};

/// One file of the pool's work list: the Deflate peel's (`stage` `None`) or a peel stage's
/// (the index of its worker copy, [`Pool::start`]).
pub(crate) struct Job {
    pub(crate) input: Input,
    pub(crate) stage: Option<usize>,
}

/// The pool's figures and counters (outside every summary's equality).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolSummary {
    /// Workers started (0: no pool).
    pub workers: usize,
    /// P, the bytes the pool may hold.
    pub pool_bytes: u64,
    /// R, the bytes kept for the oldest file's stream.
    pub reserve: u64,
    /// Files the workers prepared.
    pub files: u64,
    /// The most bytes reserved at once.
    pub peak_bytes: u64,
    /// Times a worker waited for admission or for a stream's reservation.
    pub worker_waits: u64,
    /// Times the feeding thread waited for the next file's result.
    pub feeding_waits: u64,
    /// Files a hint let a worker skip (examined by no one unless the commit missed them).
    pub hinted_files: u64,
    /// Streams a hint let a worker leave unencoded.
    pub hinted_streams: u64,
    /// Work the commits did not use.
    pub waste: super::stage::Waste,
}

/// What a worker hands the feeding thread for one file.
pub(crate) enum Outcome {
    /// The read failed (raised before anything of the file is written, as the sequential read).
    Read(Failure),
    /// The bytes and the prepared result (or its failure, raised after whole-file dedup).
    Ready(Vec<u8>, Result<Prepared, Failure>),
    /// A peel stage's file: the bytes and the stage's result (or its panic, raised after
    /// whole-file dedup).
    Peeled(Vec<u8>, Result<Result<PeelPlan, Fallback>, Failure>),
}

struct State {
    /// The next file to admit.
    next_admit: usize,
    /// Files committed: the oldest uncommitted file's index.
    committed: usize,
    used: u64,
    peak: u64,
    results: BTreeMap<usize, Outcome>,
    cancelled: bool,
    files: u64,
    worker_waits: u64,
    feeding_waits: u64,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    work: Vec<Job>,
    /// The peel stages' worker copies (one Lepton processor thread).
    stages: Vec<Option<Box<dyn PeelStage>>>,
    /// The largest peeled part (one block).
    max_part: u64,
    settings: Settings,
    pool_bytes: u64,
    reserve: u64,
    window: usize,
    alive: AtomicUsize,
    /// What the commits published (`None`: hints off).
    hints: Option<RwLock<Hints>>,
    hinted_files: AtomicU64,
    hinted_streams: AtomicU64,
}

/// What the commits wrote that later copies can be found by (design section 1, hints): only
/// ever added to, as the writer's chunks are.
#[derive(Default)]
struct Hints {
    files: HashSet<([u8; 32], u64)>,
    streams: HashSet<StreamKey>,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read(h: &RwLock<Hints>) -> std::sync::RwLockReadGuard<'_, Hints> {
    h.read().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn h(&self, k: usize) -> u64 {
        let j = &self.work[k];
        let h = j.input.len.saturating_mul(2);
        match j.stage {
            None => h,
            Some(_) => h.saturating_add(MODEL_ALLOWANCE),
        }
    }

    fn wait<'a>(&self, g: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.cv.wait(g).unwrap_or_else(|e| e.into_inner())
    }

    fn take_bytes(st: &mut State, n: u64) {
        st.used = st.used.saturating_add(n);
        st.peak = st.peak.max(st.used);
    }

    fn release(&self, n: u64) {
        if n > 0 {
            let mut st = lock(&self.state);
            st.used = st.used.saturating_sub(n);
            drop(st);
            self.cv.notify_all();
        }
    }

    /// Admit the next file, in input order; `None` when there is none left or the run is
    /// cancelled.
    fn admit(&self) -> Option<usize> {
        let mut st = lock(&self.state);
        let mut waited = false;
        loop {
            if st.cancelled || st.next_admit >= self.work.len() {
                return None;
            }
            let k = st.next_admit;
            let fits = st
                .used
                .saturating_add(self.h(k))
                .saturating_add(self.reserve)
                <= self.pool_bytes;
            // The oldest file always enters (P ≥ its h + R when nothing else is held).
            if (fits || k == st.committed) && k < st.committed + self.window {
                st.next_admit += 1;
                Self::take_bytes(&mut st, self.h(k));
                return Some(k);
            }
            if !waited {
                st.worker_waits += 1;
                waited = true;
            }
            st = self.wait(st);
        }
    }

    /// Reserve `r` bytes for a stream of file `k`; false when the run is cancelled.
    fn reserve_stream(&self, k: usize, r: u64) -> bool {
        let mut st = lock(&self.state);
        let mut waited = false;
        loop {
            if st.cancelled {
                return false;
            }
            let keep = if k == st.committed { 0 } else { self.reserve };
            if st.used.saturating_add(r).saturating_add(keep) <= self.pool_bytes {
                Self::take_bytes(&mut st, r);
                return true;
            }
            if !waited {
                st.worker_waits += 1;
                waited = true;
            }
            st = self.wait(st);
        }
    }
}

/// A worker's hooks for one file: the stream reservations and the cancellation.
struct PoolHooks<'a> {
    shared: &'a Shared,
    sizer: &'a PartEncoder,
    file: usize,
    held: u64,
}

impl Hooks for PoolHooks<'_> {
    fn analysing(&mut self, plain: u64) -> bool {
        delay(self.file as u64 ^ plain);
        let r = memory_term(plain, 0)
            .max(plain.saturating_add(self.sizer.memory_for(plain)))
            .min(self.shared.reserve);
        if !self.shared.reserve_stream(self.file, r) {
            return false;
        }
        self.held = r;
        true
    }

    fn finished(&mut self) {
        self.shared.release(std::mem::take(&mut self.held));
    }
}

impl Drop for PoolHooks<'_> {
    fn drop(&mut self) {
        self.finished();
    }
}

/// Test hook: a non-zero seed makes workers sleep pseudo-randomly (up to a few milliseconds)
/// at their steps, so results complete in shuffled orders.
pub(crate) static DELAY_SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Test hook (for the integration tests): a non-zero `seed` makes the pool's workers sleep
/// pseudo-randomly at their steps, in every run of the process until it is set back to 0.
#[doc(hidden)]
pub fn set_delay_seed(seed: u64) {
    DELAY_SEED.store(seed, Ordering::Relaxed);
}

fn delay(salt: u64) {
    let seed = DELAY_SEED.load(Ordering::Relaxed);
    if seed != 0 {
        let mut x = (seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        std::thread::sleep(std::time::Duration::from_micros(x % 4000));
    }
}

struct Alive<'a>(&'a Shared);

impl Drop for Alive<'_> {
    fn drop(&mut self) {
        // Under the lock: `take` checks `alive` under it before it waits (no lost wakeup).
        let st = lock(&self.0.state);
        self.0.alive.fetch_sub(1, Ordering::SeqCst);
        drop(st);
        self.0.cv.notify_all();
    }
}

fn worker(shared: &Shared, mut encoders: Option<(PartEncoder, PartEncoder)>) {
    let _alive = Alive(shared);
    while let Some(k) = shared.admit() {
        delay(k as u64);
        let job = &shared.work[k];
        let input = &job.input;
        let bytes = catch_unwind(AssertUnwindSafe(|| {
            crate::pipeline::read_all(&crate::source::Source::new(), input)
        }));
        let outcome = match (bytes, job.stage, encoders.as_mut()) {
            (Ok(Ok(data)), Some(i), _) => {
                let r = match shared.stages.get(i).and_then(Option::as_deref) {
                    Some(stage) => {
                        catch_unwind(AssertUnwindSafe(|| stage.peel(&data, shared.max_part)))
                            .map_err(Failure::Panic)
                    }
                    // Not reachable: jobs name only stages with a worker copy.
                    None => Err(Failure::Error(CoreError::InvalidOption(
                        "a peel job without its stage".into(),
                    ))),
                };
                Outcome::Peeled(data, r)
            }
            (Ok(Ok(data)), None, None) => {
                // Not reachable: Deflate jobs come only with a part encoder.
                Outcome::Ready(
                    data,
                    Err(Failure::Error(CoreError::InvalidOption(
                        "a Deflate job without a part encoder".into(),
                    ))),
                )
            }
            (Ok(Ok(data)), None, Some((encoder, sizer))) => {
                let mut hooks = PoolHooks {
                    shared,
                    sizer: &*sizer,
                    file: k,
                    held: 0,
                };
                let r = catch_unwind(AssertUnwindSafe(|| {
                    // The hints (design section 1): a file the commits wrote whole is not
                    // examined, a stream they wrote or referenced is not encoded; the commit
                    // checks both.
                    if let Some(h) = &shared.hints {
                        let key = (*blake3::hash(&data).as_bytes(), data.len() as u64);
                        if read(h).files.contains(&key) {
                            shared.hinted_files.fetch_add(1, Ordering::Relaxed);
                            return Prepared::hinted_whole();
                        }
                    }
                    let mut oracle = |d: &[u8], spans: &[(usize, usize)]| {
                        let Some(h) = &shared.hints else {
                            return false;
                        };
                        let hit = read(h).streams.contains(&stream_key(d, spans));
                        if hit {
                            shared.hinted_streams.fetch_add(1, Ordering::Relaxed);
                        }
                        hit
                    };
                    prepare(
                        &data,
                        &input.path,
                        &shared.settings,
                        &mut *encoder,
                        &mut oracle,
                        &mut hooks,
                    )
                }));
                drop(hooks);
                let prepared = match r {
                    Ok(p) if p.stopped => return,
                    Ok(p) => Ok(p),
                    Err(p) => {
                        // No state of a panicked prepare is carried into the next file.
                        match fresh_hook(encoder) {
                            Ok(e) => *encoder = e,
                            Err(_) => {
                                let mut st = lock(&shared.state);
                                st.results
                                    .insert(k, Outcome::Ready(data, Err(Failure::Panic(p))));
                                drop(st);
                                shared.cv.notify_all();
                                return;
                            }
                        }
                        Err(Failure::Panic(p))
                    }
                };
                Outcome::Ready(data, prepared)
            }
            (Ok(Err(e)), _, _) => Outcome::Read(Failure::Error(e)),
            (Err(p), _, _) => Outcome::Read(Failure::Panic(p)),
        };
        delay(!(k as u64));
        let mut st = lock(&shared.state);
        st.files += 1;
        st.results.insert(k, outcome);
        drop(st);
        shared.cv.notify_all();
    }
}

/// The pool of one run (see the module text). Dropping it cancels and joins the workers.
pub(crate) struct Pool {
    shared: Arc<Shared>,
    handles: Vec<JoinHandle<()>>,
}

#[cfg(test)]
thread_local! {
    /// Test hook: the live-worker count of the last pool this thread started.
    pub(crate) static LAST_ALIVE: std::cell::RefCell<Option<Arc<dyn Fn() -> usize>>> =
        const { std::cell::RefCell::new(None) };
}

impl Pool {
    /// Start up to `workers` workers over `work` (the files the run peels, in loop order).
    /// `None` when no thread could start or the workers' encoders could not be built (the run
    /// then peels sequentially).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        work: Vec<Job>,
        stages: Vec<Option<Box<dyn PeelStage>>>,
        max_part: u64,
        settings: Settings,
        template: Option<&PartEncoder>,
        workers: usize,
        pool_bytes: u64,
        reserve: u64,
        hints: bool,
    ) -> Option<Pool> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                next_admit: 0,
                committed: 0,
                used: 0,
                peak: 0,
                results: BTreeMap::new(),
                cancelled: false,
                files: 0,
                worker_waits: 0,
                feeding_waits: 0,
            }),
            cv: Condvar::new(),
            work,
            stages,
            max_part,
            settings,
            pool_bytes,
            reserve,
            window: workers.saturating_mul(4).max(1),
            alive: AtomicUsize::new(0),
            hints: hints.then(|| RwLock::new(Hints::default())),
            hinted_files: AtomicU64::new(0),
            hinted_streams: AtomicU64::new(0),
        });
        // Registered before any thread could start, so a test sees every path's threads.
        #[cfg(test)]
        {
            let s = Arc::clone(&shared);
            LAST_ALIVE.with(|l| {
                *l.borrow_mut() = Some(Arc::new(move || s.alive.load(Ordering::SeqCst)));
            });
        }
        // Every encoder is built before any thread starts, so a failure leaves nothing running.
        let mut encoders = Vec::with_capacity(workers);
        for _ in 0..workers {
            let pair = match template {
                Some(t) => match (fresh_hook(t), fresh_hook(t)) {
                    (Ok(e), Ok(s)) => Some((e, s)),
                    // The workers cannot be equipped: the run peels sequentially (the feeding
                    // thread has its own encoder).
                    _ => return None,
                },
                None => None,
            };
            encoders.push(pair);
        }
        let mut handles = Vec::new();
        for (i, encoders) in encoders.into_iter().enumerate() {
            let s = Arc::clone(&shared);
            s.alive.fetch_add(1, Ordering::SeqCst);
            let spawned = std::thread::Builder::new()
                .name(format!("lpk-peel-{i}"))
                .spawn(move || worker(&s, encoders));
            match spawned {
                Ok(h) => handles.push(h),
                Err(_) => {
                    shared.alive.fetch_sub(1, Ordering::SeqCst);
                    break;
                }
            }
        }
        if handles.is_empty() {
            return None;
        }
        Some(Pool { shared, handles })
    }

    /// Workers started.
    pub(crate) fn workers(&self) -> usize {
        self.handles.len()
    }

    /// The result of file `k`, the next in input order; `None` when no worker is left to
    /// prepare it (the feeding thread then peels it itself).
    pub(crate) fn take(&self, k: usize) -> Option<Outcome> {
        let sh = &*self.shared;
        let mut st = lock(&sh.state);
        let mut waited = false;
        loop {
            if let Some(o) = st.results.remove(&k) {
                return Some(o);
            }
            if sh.alive.load(Ordering::SeqCst) == 0 {
                if st.next_admit <= k {
                    // Never admitted: keep the order and the accounting consistent.
                    st.next_admit = k + 1;
                    Shared::take_bytes(&mut st, sh.h(k));
                }
                return None;
            }
            if !waited {
                st.feeding_waits += 1;
                waited = true;
            }
            st = sh.wait(st);
        }
    }

    /// Whether the pool keeps hints (the commits then publish what they wrote).
    pub(crate) fn hints(&self) -> bool {
        self.shared.hints.is_some()
    }

    /// Publish what a commit wrote, for the workers' hints.
    pub(crate) fn publish(&self, p: Published) {
        if let Some(h) = &self.shared.hints {
            let mut h = h.write().unwrap_or_else(|e| e.into_inner());
            if let Some(w) = p.whole {
                h.files.insert(w);
            }
            h.streams.extend(p.streams);
        }
    }

    /// File `k` is committed: its bytes are released and the next file is the oldest.
    pub(crate) fn committed(&self, k: usize) {
        let sh = &*self.shared;
        let mut st = lock(&sh.state);
        if k < st.next_admit {
            st.used = st.used.saturating_sub(sh.h(k));
        }
        st.committed = k + 1;
        drop(st);
        sh.cv.notify_all();
    }

    /// The figures and counters so far.
    pub(crate) fn summary(&self) -> PoolSummary {
        let st = lock(&self.shared.state);
        PoolSummary {
            workers: self.handles.len(),
            pool_bytes: self.shared.pool_bytes,
            reserve: self.shared.reserve,
            files: st.files,
            peak_bytes: st.peak,
            worker_waits: st.worker_waits,
            feeding_waits: st.feeding_waits,
            hinted_files: self.shared.hinted_files.load(Ordering::Relaxed),
            hinted_streams: self.shared.hinted_streams.load(Ordering::Relaxed),
            waste: Default::default(),
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        lock(&self.shared.state).cancelled = true;
        self.shared.cv.notify_all();
        for h in self.handles.drain(..) {
            // A worker catches every panic of its work; a join error has nothing left to clean.
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::stage::{
        DeflatePeel, FAIL_ON, FORCE_RERUN, FRESH_FAILS_AFTER, FRESH_FAIL_ON,
    };
    use super::super::testkit::{every_kind, gzip, text, zip_with};
    use super::*;
    use crate::{BalancedOptions, Pipeline, RunSummary};

    /// Text repeating a 40 KiB passage with one change per copy: beyond Deflate's window,
    /// within the model's dictionary, so its streams are kept.
    fn repetitive(seed: u32, len: usize) -> Vec<u8> {
        let base = text(40_000, seed);
        let mut out = Vec::with_capacity(len + base.len());
        let mut copy = 0usize;
        while out.len() < len {
            let at = out.len();
            out.extend_from_slice(&base);
            out[at + (copy * 7919) % base.len()] = b'#';
            copy += 1;
        }
        out.truncate(len);
        out
    }

    fn zip_of(members: &[(&str, Vec<u8>, bool)]) -> Vec<u8> {
        use std::io::Write as _;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data, deflated) in members {
            let method = if *deflated {
                zip::CompressionMethod::Deflated
            } else {
                zip::CompressionMethod::Stored
            };
            let o = zip::write::SimpleFileOptions::default()
                .compression_method(method)
                .last_modified_time(zip::DateTime::default());
            w.start_file(*name, o).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// Every walker kind, kept streams, a stream stored by an earlier file, a whole duplicate,
    /// and enough files for the window to turn over; `tag` prefixes the names (the failure hook
    /// is keyed by path).
    fn tree(root: &Path, tag: &str) {
        for (ext, bytes) in every_kind() {
            if ext == "gz" {
                // Its streams gain nothing, so it goes whole and its copy is a whole duplicate.
                std::fs::write(root.join(format!("{tag}kind2.gz")), &bytes).unwrap();
            }
            std::fs::write(root.join(format!("{tag}kind.{ext}")), bytes).unwrap();
        }
        let member = repetitive(91, 90_000);
        let a = zip_of(&[("same.xml", member.clone(), true)]);
        std::fs::write(root.join(format!("{tag}a.zip")), &a).unwrap();
        std::fs::write(
            root.join(format!("{tag}b.zip")),
            zip_of(&[
                ("first.txt", text(2_000, 92), false),
                ("same.xml", member, true),
            ]),
        )
        .unwrap();
        std::fs::write(root.join(format!("{tag}c.zip")), &a).unwrap();
        for i in 0..24u32 {
            std::fs::write(
                root.join(format!("{tag}g{i:02}.gz")),
                gzip(&repetitive(100 + i, 60_000 + 7_000 * i as usize)),
            )
            .unwrap();
        }
        std::fs::write(
            root.join(format!("{tag}office.docx")),
            zip_with(&["[Content_Types].xml", "word/document.xml"], 50_000),
        )
        .unwrap();
        // Fixed modification times: the runs compare archives byte for byte.
        for e in std::fs::read_dir(root).unwrap() {
            let f = std::fs::File::options()
                .write(true)
                .open(e.unwrap().path())
                .unwrap();
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
                .unwrap();
        }
    }

    fn pipeline(threads: usize, memory: Option<u64>) -> Pipeline {
        let mut p = Pipeline::balanced(BalancedOptions {
            block_size: 4 << 20,
            dict_size: 1 << 20,
            sample_len: 1 << 16,
            threads: Some(threads),
            ..BalancedOptions::default()
        });
        p.deflate = Some(DeflatePeel {
            record_files: true,
            ..DeflatePeel::default()
        });
        p.seal.memory = memory;
        p
    }

    /// A run with the exact skips and the hints on or off.
    fn run_with(
        root: &Path,
        threads: usize,
        memory: Option<u64>,
        skips: bool,
        hints: bool,
    ) -> (Vec<u8>, RunSummary) {
        let mut p = pipeline(threads, memory);
        if let Some(d) = p.deflate.as_mut() {
            d.exact_skips = skips;
            d.hints = hints;
        }
        let mut out = Vec::new();
        let s = p.run(root, &mut out).unwrap();
        (out, s)
    }

    fn run(root: &Path, threads: usize, memory: Option<u64>) -> (Vec<u8>, RunSummary) {
        let mut out = Vec::new();
        let s = pipeline(threads, memory).run(root, &mut out).unwrap();
        (out, s)
    }

    /// Design task 2: identical archives and equal `PeelSummary` at 1, 2, 3, 8 and 24 threads,
    /// workers delayed pseudo-randomly, at the minimum budget and a wide one.
    #[test]
    fn archives_are_identical_at_every_width_with_shuffled_workers() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path(), "w-");
        let (one, s1) = run(dir.path(), 1, None);
        let d = &s1.peel.deflate;
        assert!(
            d.outcomes.kept >= 10 && d.outcomes.already_stored >= 1,
            "{:?}",
            d.outcomes
        );
        assert!(d.deduplicated.files >= 1, "{:?}", d.deduplicated);
        assert_eq!(s1.peel_pool.workers, 0);
        let minimum = s1.memory.minimum();
        DELAY_SEED.store(0x5EED, Ordering::Relaxed);
        for threads in [2, 3, 8, 24] {
            for memory in [Some(minimum), Some(1 << 40)] {
                let (bytes, s) = run(dir.path(), threads, memory);
                assert!(
                    bytes == one,
                    "{threads} threads, {memory:?}: the archives differ"
                );
                assert_eq!(s.peel, s1.peel, "{threads} threads, {memory:?}");
                let p = &s.peel_pool;
                assert_eq!(p.workers, threads - s.blocks_in_flight.max(1));
                assert_eq!(
                    (s.memory.peel_workers, s.memory.peel_pool),
                    (p.workers, p.pool_bytes)
                );
                assert!(p.workers >= 1 && p.files >= 30, "{p:?}");
                assert!(p.peak_bytes <= p.pool_bytes, "{p:?}");
                // The whole duplicate was prepared and then referenced; the streams of b.zip and
                // c.zip were encoded and then found stored, or hinted and not encoded.
                assert!(
                    p.waste.files + p.hinted_files >= 1 && p.waste.encodes + p.hinted_streams >= 2,
                    "{p:?}"
                );
                assert_eq!(p.waste.reruns, 0, "{p:?}");
                if memory == Some(minimum) {
                    // P binds at the minimum budget: the workers waited for its bytes.
                    assert!(p.worker_waits > 0, "{p:?}");
                }
            }
        }
        DELAY_SEED.store(0, Ordering::Relaxed);
    }

    /// A pool result the commit does not take (the forced miss) is prepared again on the
    /// feeding thread: the same bytes.
    #[test]
    fn a_forced_miss_prepares_again_with_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path(), "m-");
        let (one, s1) = run(dir.path(), 1, None);
        FORCE_RERUN.with(|c| c.set(true));
        let (four, s4) = run(dir.path(), 4, Some(1 << 40));
        FORCE_RERUN.with(|c| c.set(false));
        assert!(four == one, "a forced miss changed the bytes");
        assert_eq!(s4.peel, s1.peel);
        assert!(s4.peel_pool.waste.reruns >= 30, "{:?}", s4.peel_pool);
    }

    fn fail_on(path: &str, panic: bool) {
        FAIL_ON
            .lock()
            .unwrap()
            .push((path.to_string(), Vec::new(), panic));
    }

    fn clear(prefix: &str) {
        FAIL_ON
            .lock()
            .unwrap()
            .retain(|(p, _, _)| !p.starts_with(prefix));
    }

    /// Design task 2: an injected encoder error or panic surfaces only on a stream the commit
    /// encodes. b.zip's member is stored by a.zip and c.zip is a whole copy of a.zip: failing
    /// every encode of theirs changes nothing at any width (the workers encode them, the commit
    /// finds them stored); failing a.zip's fails the run at every width, an error as an error
    /// and a panic as a panic.
    #[test]
    fn injected_failures_surface_only_on_streams_the_commit_encodes() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path(), "e-");
        let (clean, s1) = run(dir.path(), 1, None);
        for panic in [false, true] {
            fail_on("e-b.zip", panic);
            fail_on("e-c.zip", panic);
            DELAY_SEED.store(0xE44, Ordering::Relaxed);
            for threads in [1, 2, 8, 24] {
                // Hints off: the workers encode the stored streams and meet the failures.
                let (bytes, s) = run_with(dir.path(), threads, Some(1 << 40), true, false);
                assert!(bytes == clean, "{threads} threads, panic {panic}");
                assert_eq!(s.peel, s1.peel);
            }
            fail_on("e-a.zip", panic);
            for threads in [1, 2, 8, 24] {
                let r = std::panic::catch_unwind(|| {
                    let mut out = Vec::new();
                    pipeline(threads, Some(1 << 40)).run(dir.path(), &mut out)
                });
                match (r, panic) {
                    (Ok(Err(e)), false) => {
                        assert!(e.to_string().contains("injected part encoder error"), "{e}")
                    }
                    (Err(p), true) => {
                        let msg = p.downcast_ref::<&str>().copied().unwrap_or_default();
                        assert_eq!(msg, "injected part encoder panic");
                    }
                    (r, _) => panic!(
                        "{threads} threads, panic {panic}: {:?}",
                        r.map(|r| r.is_ok())
                    ),
                }
            }
            DELAY_SEED.store(0, Ordering::Relaxed);
            clear("e-");
        }
    }

    /// Design task 2: no worker outlives a run that fails while the pool runs.
    #[test]
    fn no_worker_outlives_a_failed_run() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path(), "d-");
        fail_on("d-g00.gz", false);
        for threads in [2, 8, 24] {
            LAST_ALIVE.with(|l| *l.borrow_mut() = None);
            let mut out = Vec::new();
            let r = pipeline(threads, Some(1 << 40)).run(dir.path(), &mut out);
            assert!(r.is_err());
            let alive = LAST_ALIVE.with(|l| l.borrow().clone()).unwrap();
            assert_eq!(alive(), 0, "{threads} threads");
        }
        clear("d-");
    }

    /// No worker outlives a run whose feeding thread panics (a worker's panic resumed by the
    /// commit), and none starts when building their encoders fails (review B2).
    #[test]
    fn no_worker_outlives_a_feeding_thread_panic_or_an_encoder_failure() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path(), "q-");
        fail_on("q-g03.gz", true);
        for threads in [2, 8, 24] {
            LAST_ALIVE.with(|l| *l.borrow_mut() = None);
            let r = std::panic::catch_unwind(|| {
                let mut out = Vec::new();
                pipeline(threads, Some(1 << 40)).run(dir.path(), &mut out)
            });
            assert!(r.is_err(), "{threads} threads: the run did not panic");
            let alive = LAST_ALIVE.with(|l| l.borrow().clone()).unwrap();
            assert_eq!(alive(), 0, "{threads} threads");
        }
        clear("q-");
        // Building the workers' encoders fails after some succeed (in the old order, after
        // some workers had started): the run falls back to the sequential path (review T2)
        // with the blocks in flight chosen without the pool, writes the same bytes, and no
        // worker is left (review T1).
        let (one, s1) = run(dir.path(), 1, None);
        for (threads, successes) in [(2, 1), (8, 5), (24, 20)] {
            let (_, plain) = {
                let mut p = pipeline(threads, Some(1 << 40));
                p.deflate = None;
                let mut out = Vec::new();
                let s = p.run(dir.path(), &mut out).unwrap();
                (out, s)
            };
            LAST_ALIVE.with(|l| *l.borrow_mut() = None);
            FRESH_FAILS_AFTER.with(|f| f.set(Some(successes)));
            let r = std::panic::catch_unwind(|| run(dir.path(), threads, Some(1 << 40)));
            FRESH_FAILS_AFTER.with(|f| f.set(None));
            let (bytes, s) = r.unwrap();
            let alive = LAST_ALIVE.with(|l| l.borrow().clone()).unwrap();
            assert_eq!(
                alive(),
                0,
                "{threads} threads: a worker outlived the failure"
            );
            assert!(bytes == one, "{threads} threads");
            assert_eq!(s.peel, s1.peel);
            assert_eq!(s.peel_pool.workers, 0);
            assert_eq!((s.memory.peel_workers, s.memory.peel_pool), (0, 0));
            // D-67's blocks in flight: those of the same budget and threads with no pool (the
            // JPEG-free tree's plan without the Deflate stage has the same B here).
            assert_eq!(
                s.blocks_in_flight, plain.blocks_in_flight,
                "{threads} threads"
            );
            assert_eq!(s.blocks_in_flight, threads, "{threads} threads");
        }
    }

    /// A replacement encoder that fails after a failed encode on a stream the commit finds
    /// stored does not fail the file's later streams (review L2): the later stream is left
    /// unencoded, and the commit prepares the file again; the bytes are the clean run's.
    #[test]
    fn a_failed_replacement_encoder_does_not_fail_later_streams() {
        let dir = tempfile::tempdir().unwrap();
        let m = repetitive(71, 90_000);
        std::fs::write(
            dir.path().join("l-a.zip"),
            zip_of(&[("same.xml", m.clone(), true)]),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("l-b.zip"),
            zip_of(&[
                ("same.xml", m.clone(), true),
                ("new.xml", repetitive(72, 90_000), true),
            ]),
        )
        .unwrap();
        let (clean, s1) = run(dir.path(), 1, None);
        assert!(s1.peel.deflate.outcomes.already_stored >= 1);
        FAIL_ON
            .lock()
            .unwrap()
            .push(("l-b.zip".into(), m[..64].to_vec(), false));
        for threads in [1, 2, 4] {
            FRESH_FAIL_ON.lock().unwrap().push(("l-b.zip".into(), 1));
            // Hints off: the worker encodes the stored stream and meets the failure.
            let (bytes, s) = run_with(dir.path(), threads, Some(1 << 40), true, false);
            FRESH_FAIL_ON
                .lock()
                .unwrap()
                .retain(|(p, _)| !p.starts_with("l-"));
            assert!(bytes == clean, "{threads} threads");
            assert_eq!(s.peel, s1.peel);
            if threads > 1 {
                assert!(s.peel_pool.waste.reruns >= 1, "{:?}", s.peel_pool);
            }
        }
        clear("l-");
    }

    /// Design task 3: the exact skips and the hints change no byte. The bound files (a large
    /// file with small streams, and a file whose streams cannot pass the file gate) go whole
    /// without encodes; the mixed case (a large file with a stored stream and a new one) is
    /// prepared again; forced misses give the same bytes; at every width.
    #[test]
    fn exact_skips_and_hints_change_no_byte() {
        use super::super::testkit::{noise, zlib};
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path(), "s-");
        let x = repetitive(81, 80_000);
        // Its stream is kept (a small file), so a later copy of it is stored.
        std::fs::write(
            dir.path().join("s-mixed-a.bin"),
            [b"MZ".to_vec(), zlib(&x)].concat(),
        )
        .unwrap();
        // 32 × (its streams' lengths) is below its length: bound by the share rule.
        let big = |tail: &[u8]| {
            [
                b"MZ".to_vec(),
                noise(600_000, 3),
                zlib(&x),
                tail.to_vec(),
                noise(600_000, 4),
            ]
            .concat()
        };
        std::fs::write(
            dir.path().join("s-mixed-b.bin"),
            big(&zlib(&repetitive(82, 80_000))),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("s-share-c.bin"),
            [noise(900_000, 5), zlib(&repetitive(83, 80_000))].concat(),
        )
        .unwrap();
        // The gate bound (review M1): a file holding one zlib stream of 248 to 256 bytes is
        // bound by G ≤ 256. Its stream is left unencoded (`file_bound`) only when its record and
        // the overhead stay below its length; with preflate-rs 0.7.6 the corrections of such a
        // stream are never below 21 bytes here (the record 108 bytes, the cost 268 > 256), so
        // the pre-encode gate decides it first. Whichever holds, the bytes and the summary must
        // not depend on the width or the hints; a later copy of the stream sits in a large file.
        // A short incompressible passage repeated past the scan's 1 KiB minimum, at every zlib
        // level: the stream is its literals plus a few matches.
        let candidates: Vec<(Vec<u8>, bool)> = (1..=9u32)
            .flat_map(|level| (0..8u32).map(move |seed| (level, seed)))
            .flat_map(|(level, seed)| (100..260usize).map(move |k| (level, seed, k)))
            .map(|(level, seed, k)| {
                use std::io::Write as _;
                let base = noise(k, seed + 100);
                let mut e =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
                e.write_all(&base.repeat(1100 / k + 1)).unwrap();
                e.finish().unwrap()
            })
            .filter(|z| (248 + 6..=256 + 6).contains(&z.len()))
            .filter_map(|z| {
                let mut passes = None;
                super::super::examine(&z, "bin", &super::super::Limits::standard(), |r| {
                    let len = r.deflate_len as u64;
                    let rec = lpk_format::Record::new(lpk_format::RecordBody::Deflate(
                        lpk_format::DeflateRecord {
                            original_len: len,
                            plain_len: r.plain.len() as u64,
                            corrections: r.corrections.clone(),
                            library: 0,
                            original_hash: [0; 32],
                        },
                    ));
                    let cost = super::super::stage::record_len(&rec)
                        + super::super::stage::STREAM_OVERHEAD;
                    if (248..=super::super::stage::FILE_GATE).contains(&len) {
                        passes = Some(cost < len);
                    }
                });
                passes.map(|p| (z, p))
            })
            .collect();
        let (gate, reachable) = candidates
            .iter()
            .find(|(_, p)| *p)
            .or(candidates.first())
            .cloned()
            .expect("a zlib stream of 248 to 256 bytes");
        std::fs::write(dir.path().join("s-gate-d.bin"), &gate).unwrap();
        std::fs::write(
            dir.path().join("s-gate-e.bin"),
            [noise(400_000, 6), gate.clone(), noise(400_000, 7)].concat(),
        )
        .unwrap();
        let (off, s_off) = run_with(dir.path(), 1, None, false, false);
        let (on, s_on) = run_with(dir.path(), 1, None, true, true);
        let gate_file = s_on
            .peel
            .deflate
            .files
            .as_ref()
            .unwrap()
            .iter()
            .find(|f| f.path == "s-gate-d.bin")
            .unwrap();
        println!(
            "gate case: {} candidates, reachable {reachable}, {gate_file:?}",
            candidates.len()
        );
        if reachable {
            assert_eq!(gate_file.outcomes.file_bound, 1, "{gate_file:?}");
        } else {
            assert_eq!(
                (gate_file.outcomes.no_gain, gate_file.outcomes.file_bound),
                (1, 0),
                "{gate_file:?}"
            );
        }
        assert!(on == off, "the skips changed the bytes at one thread");
        assert_eq!(s_off.peel.deflate.outcomes.file_bound, 0);
        let d = &s_on.peel.deflate;
        assert!(
            d.file_bound >= 1 && d.outcomes.file_bound >= 1,
            "{:?}",
            d.outcomes
        );
        assert_eq!(d.outcomes.total(), d.streams.reconstructed);
        assert!(
            d.sample_bytes < s_off.peel.deflate.sample_bytes,
            "the skips saved encodes"
        );
        for threads in [2, 4, 24] {
            for (skips, hints) in [(true, true), (true, false), (false, true)] {
                let (bytes, s) = run_with(dir.path(), threads, Some(1 << 40), skips, hints);
                assert!(
                    bytes == off,
                    "{threads} threads, skips {skips}, hints {hints}"
                );
                if skips && !hints {
                    // The mixed file: its stored stream and its bound one mix.
                    assert!(s.peel_pool.waste.reruns >= 1, "{:?}", s.peel_pool);
                }
                // The summary is a function of the input and the settings, not of the hints'
                // timing (review S1): the one-thread summary with the same skips.
                let want = if skips { &s_on.peel } else { &s_off.peel };
                assert_eq!(
                    &s.peel, want,
                    "{threads} threads, skips {skips}, hints {hints}"
                );
            }
            FORCE_RERUN.with(|c| c.set(true));
            let (bytes, _) = run_with(dir.path(), threads, Some(1 << 40), true, true);
            FORCE_RERUN.with(|c| c.set(false));
            assert!(bytes == off, "{threads} threads, forced misses");
        }
    }

    /// The sizing of P: at least the one-file set, and what the blocks leave of a wide budget.
    #[test]
    fn the_pool_gets_what_the_blocks_leave() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path(), "p-");
        let (_, wide) = run(dir.path(), 8, Some(1 << 40));
        let m = wide.memory;
        assert_eq!(m.blocks_in_flight, 3);
        assert_eq!(m.peel_workers, 5);
        // What the blocks leave, the records' term (twice the records cap) kept in `fixed`.
        let records = 2 * super::super::stage::RECORDS_CAP;
        assert!(m.peel_pool + records >= (1 << 40) - m.fixed() - 3 * m.per_block);
        assert!(m.peel_pool <= (1 << 40) - 3 * m.per_block);
        let (_, narrow) = run(dir.path(), 8, Some(wide.memory.minimum()));
        let p = &narrow.peel_pool;
        assert_eq!(narrow.blocks_in_flight, 1);
        assert_eq!(p.workers, 7);
        assert!(
            p.pool_bytes >= p.reserve && p.peak_bytes <= p.pool_bytes,
            "{p:?}"
        );
    }
}
