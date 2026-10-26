//! Progress events and cancellation for the long-running engine entry points (create,
//! append, extract, export): a [`Progress`] observer that receives rate-limited [`Event`]s,
//! and a [`CancelToken`] the engine polls between files and between blocks.
//!
//! **Events.** An event names the [`Stage`] the engine is in and carries the run's counters
//! so far: files done (and the total when known), bytes in and out, blocks done (and the
//! total when known). The engine, not the observer, limits the rate: the first event of a
//! stage and the stage's last event (`done`) are always delivered, the rest at most one per
//! [`MIN_INTERVAL`] per stage, so an observer that writes a console line never has to drop
//! anything itself. Events are delivered one at a time, in order: from the engine's
//! coordinating thread (the feeding thread of a create or append, the dispatching thread of
//! an extraction or export) and, while a file is being read, from the thread reading it (an
//! `Encode` event as the bytes of a large file go by); the observer must be `Send + Sync`.
//!
//! **Cancellation.** A [`CancelToken`] is cloneable and thread-safe (a shared flag); the
//! engine polls it between files and between blocks in every long loop, before every read
//! of a file being archived (so a file of many gigabytes is cut short), and its decode
//! workers poll it before each block. A cancelled run fails with [`CoreError::Cancelled`] and
//! cleans up exactly as a failure does: a new archive is removed, an append's generation is
//! cut off (D-59), an extraction keeps its finished files and removes the unfinished ones
//! (D-54), an export's sink is left to its caller (the command line removes the output file).
//! Work already handed to a thread — a block being encoded or decoded, a file being peeled —
//! completes before the engine joins the thread; nothing outlives the call.
//!
//! No event and no token changes a byte of any archive: the counters are read from the
//! writer's position and the loops' own counts.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::CoreError;

/// Where the engine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Walking the tree (create, append): `files_done` counts the entries found.
    Walk,
    /// A file read whole and peeled (the JPEG or Deflate peel) and written.
    Peel,
    /// A file streamed to the writer, or a block closed.
    Encode,
    /// Closing the archive: index, records, recovery, trailer, sync.
    Seal,
    /// Extracting: blocks decoded and files written.
    Extract,
    /// Exporting: blocks decoded and entries written to the sink.
    Export,
}

impl Stage {
    /// The stage's lower-case name (`walk`, `peel`, `encode`, `seal`, `extract`, `export`).
    pub fn name(self) -> &'static str {
        match self {
            Stage::Walk => "walk",
            Stage::Peel => "peel",
            Stage::Encode => "encode",
            Stage::Seal => "seal",
            Stage::Extract => "extract",
            Stage::Export => "export",
        }
    }

    /// Every stage, in the order a create and then an extraction or export runs them.
    pub const ALL: [Stage; 6] = [
        Stage::Walk,
        Stage::Peel,
        Stage::Encode,
        Stage::Seal,
        Stage::Extract,
        Stage::Export,
    ];

    fn index(self) -> usize {
        match self {
            Stage::Walk => 0,
            Stage::Peel => 1,
            Stage::Encode => 2,
            Stage::Seal => 3,
            Stage::Extract => 4,
            Stage::Export => 5,
        }
    }
}

/// One progress event: the stage and the run's counters so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Where the engine is.
    pub stage: Stage,
    /// Files (entries, for the walk) done so far.
    pub files_done: u64,
    /// Files in all, when known (after the walk; from the plan of an extraction or export).
    pub files_total: Option<u64>,
    /// Bytes read so far: input file bytes fed (create, append), encoded block bytes decoded
    /// (extract, export).
    pub bytes_in: u64,
    /// Bytes written so far: the archive's position (create, append), file bytes written
    /// (extract) or streamed to the sink (export).
    pub bytes_out: u64,
    /// Blocks closed (create, append) or decoded (extract, export) so far.
    pub blocks_done: u64,
    /// Blocks in all, when known (an extraction's needed blocks, an export's decodes).
    pub blocks_total: Option<u64>,
    /// The stage's last event.
    pub done: bool,
}

/// An observer of [`Event`]s.
pub trait Progress: Send + Sync {
    /// Called by the engine, on its coordinating thread, with each event it delivers.
    fn event(&self, event: &Event);
}

impl<F: Fn(&Event) + Send + Sync> Progress for F {
    fn event(&self, event: &Event) {
        self(event)
    }
}

/// A flag the engine polls: cloneable, thread-safe, set once by [`cancel`](Self::cancel). A
/// [`child`](Self::child) token is cancelled by its own `cancel` or by its parent's; its
/// `cancel` leaves the parent alone (E5-1: a front end's read is cancelled by its caller's
/// token or by the session, without the session cancelling the caller's token).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<TokenInner>);

#[derive(Debug, Default)]
struct TokenInner {
    flag: AtomicBool,
    parent: Option<CancelToken>,
}

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// A token cancelled when this one is (or when it is cancelled itself); cancelling it
    /// does not cancel this one.
    pub fn child(&self) -> CancelToken {
        CancelToken(Arc::new(TokenInner {
            flag: AtomicBool::new(false),
            parent: Some(self.clone()),
        }))
    }

    /// Cancel: every clone (and every child) sees it; the engine stops at its next poll. A
    /// parent is not cancelled.
    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::SeqCst);
    }

    /// Whether [`cancel`](Self::cancel) was called on this token or an ancestor.
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
            || self
                .0
                .parent
                .as_ref()
                .is_some_and(CancelToken::is_cancelled)
    }

    /// [`CoreError::Cancelled`] when cancelled, else `Ok`.
    pub fn check(&self) -> Result<(), CoreError> {
        if self.is_cancelled() {
            Err(CoreError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// What an engine call reports to and polls: an optional observer and a token. The default
/// observes nothing and is never cancelled.
#[derive(Clone, Default)]
pub struct Monitor {
    /// The observer of the run's events, if any.
    pub progress: Option<Arc<dyn Progress>>,
    /// The token the run polls.
    pub cancel: CancelToken,
}

impl std::fmt::Debug for Monitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Monitor")
            .field("progress", &self.progress.is_some())
            .field("cancel", &self.cancel)
            .finish()
    }
}

impl Monitor {
    /// An observer and a token.
    pub fn new(progress: Arc<dyn Progress>, cancel: CancelToken) -> Self {
        Monitor {
            progress: Some(progress),
            cancel,
        }
    }

    /// A token alone.
    pub fn with_cancel(cancel: CancelToken) -> Self {
        Monitor {
            progress: None,
            cancel,
        }
    }

    /// The engine's side: counters and the rate limit, shared by the run's threads.
    pub(crate) fn reporter(&self) -> Arc<Reporter> {
        Arc::new(Reporter {
            monitor: self.clone(),
            state: Mutex::new(State::default()),
        })
    }
}

/// The least time between two delivered events of one stage, apart from its first and its
/// last.
pub const MIN_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Default)]
struct State {
    files_done: u64,
    files_total: Option<u64>,
    bytes_in: u64,
    /// Bytes read of the file in progress ([`Reporter::reading`]), on top of `bytes_in`.
    in_progress: u64,
    /// The largest bytes-in figure shown, so the figure never goes back.
    shown_in: u64,
    bytes_out: u64,
    blocks_done: u64,
    blocks_total: Option<u64>,
    last: [Option<Instant>; 6],
}

/// The engine's counters and the rate limit over a [`Monitor`]; shared by reference (or by
/// `Arc`, with the readers it watches) with the run's threads: every method takes `&self`
/// and events are delivered one at a time under the state lock, so an observer never sees
/// two at once, whichever thread reads.
pub(crate) struct Reporter {
    monitor: Monitor,
    state: Mutex<State>,
}

impl Reporter {
    /// The token's [`check`](CancelToken::check).
    pub(crate) fn check(&self) -> Result<(), CoreError> {
        self.monitor.cancel.check()
    }

    /// Whether the token is cancelled.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.monitor.cancel.is_cancelled()
    }

    /// The token, for a worker to poll.
    pub(crate) fn token(&self) -> &CancelToken {
        &self.monitor.cancel
    }

    /// Set the totals once they are known; the files-done count starts over (the walk's
    /// entries found are not the files fed).
    pub(crate) fn totals(&self, files: Option<u64>, blocks: Option<u64>) {
        if let Ok(mut s) = self.state.lock() {
            s.files_total = files;
            s.blocks_total = blocks;
            s.files_done = 0;
        }
    }

    /// Add to the counters and deliver an event of `stage` if the rate limit allows.
    pub(crate) fn add(&self, stage: Stage, files: u64, bytes_in: u64, blocks: u64) {
        self.update(stage, false, |s| {
            s.files_done += files;
            s.bytes_in += bytes_in;
            s.blocks_done += blocks;
        });
    }

    /// Set the counters from absolute figures (a file completed: its bytes in progress are
    /// folded into `bytes_in`) and deliver an event of `stage` if the rate limit allows.
    pub(crate) fn set(
        &self,
        stage: Stage,
        files_done: u64,
        bytes_in: u64,
        bytes_out: u64,
        blocks_done: u64,
    ) {
        self.update(stage, false, |s| {
            s.files_done = files_done;
            s.bytes_in = bytes_in;
            s.in_progress = 0;
            s.bytes_out = bytes_out;
            s.blocks_done = blocks_done;
        });
    }

    /// `n` more bytes of the file in progress were read (a watched reader, on whichever
    /// thread reads): an `Encode` event if the rate limit allows, so a file of many
    /// gigabytes reports while it is read.
    fn reading(&self, n: u64) {
        self.update(Stage::Encode, false, |s| s.in_progress += n);
    }

    /// Deliver the stage's last event, whatever the rate limit.
    pub(crate) fn done(&self, stage: Stage) {
        self.update(stage, true, |_| {});
    }

    /// [`done`](Self::done) for a stage that delivered an event already (nothing for one
    /// that never ran).
    pub(crate) fn done_if_started(&self, stage: Stage) {
        let started = self
            .state
            .lock()
            .map(|s| s.last[stage.index()].is_some())
            .unwrap_or(false);
        if started {
            self.done(stage);
        }
    }

    /// Wrap a file's reader so that every read polls the token (a cancelled read fails with
    /// an I/O error [`map_cancelled`](Self::map_cancelled) turns into
    /// [`CoreError::Cancelled`]) and counts toward the bytes in progress.
    pub(crate) fn watch(self: &Arc<Self>, inner: Box<dyn Read + Send>) -> Box<dyn Read + Send> {
        Box::new(Watched {
            inner,
            rep: Arc::clone(self),
        })
    }

    /// An I/O error met while the token is cancelled is the cancellation (a watched reader
    /// failed its read, whichever variant wrapped it); any other error stands.
    pub(crate) fn map_cancelled(&self, e: CoreError) -> CoreError {
        if !self.is_cancelled() {
            return e;
        }
        match &e {
            CoreError::Io { .. } | CoreError::Format(lpk_format::FormatError::Io(_)) => {
                CoreError::Cancelled
            }
            _ => e,
        }
    }

    fn update(&self, stage: Stage, done: bool, f: impl FnOnce(&mut State)) {
        let Some(progress) = &self.monitor.progress else {
            return;
        };
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        f(&mut s);
        let i = stage.index();
        let now = Instant::now();
        let due = match s.last[i] {
            None => true,
            Some(t) => done || now.duration_since(t) >= MIN_INTERVAL,
        };
        if !due {
            return;
        }
        s.last[i] = Some(now);
        s.shown_in = s.shown_in.max(s.bytes_in.saturating_add(s.in_progress));
        let event = Event {
            stage,
            files_done: s.files_done,
            files_total: s.files_total,
            bytes_in: s.shown_in,
            bytes_out: s.bytes_out,
            blocks_done: s.blocks_done,
            blocks_total: s.blocks_total,
            done,
        };
        // Delivered under the lock: one event at a time, in order.
        progress.event(&event);
    }
}

/// The error a watched reader fails with once the token is cancelled.
#[derive(Debug)]
struct CancelledRead;

impl std::fmt::Display for CancelledRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for CancelledRead {}

/// A reader that polls the token before each read and counts what it reads.
struct Watched {
    inner: Box<dyn Read + Send>,
    rep: Arc<Reporter>,
}

impl Read for Watched {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.rep.is_cancelled() {
            return Err(std::io::Error::other(CancelledRead));
        }
        let n = self.inner.read(buf)?;
        self.rep.reading(n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_parent_cancel_reaches_the_child_and_a_child_cancel_stays_in_it() {
        let parent = super::CancelToken::new();
        let child = parent.child();
        let grandchild = child.child();
        child.cancel();
        assert!(child.is_cancelled() && grandchild.is_cancelled());
        assert!(!parent.is_cancelled(), "a child's cancel leaves the parent");
        let other = parent.child();
        assert!(!other.is_cancelled());
        parent.cancel();
        assert!(other.is_cancelled() && other.check().is_err());
        assert!(other.clone().is_cancelled(), "clones share the state");
    }

    use super::*;

    /// Collects every event.
    struct Collect(Mutex<Vec<Event>>);

    impl Progress for Collect {
        fn event(&self, e: &Event) {
            self.0.lock().unwrap().push(*e);
        }
    }

    #[test]
    fn the_token_is_shared_by_its_clones() {
        let t = CancelToken::new();
        let c = t.clone();
        assert!(!c.is_cancelled());
        assert!(t.check().is_ok());
        t.cancel();
        assert!(c.is_cancelled());
        assert!(matches!(c.check(), Err(CoreError::Cancelled)));
    }

    #[test]
    fn the_first_and_the_last_event_of_a_stage_pass_and_the_rest_are_rate_limited() {
        let collect = Arc::new(Collect(Mutex::new(Vec::new())));
        let m = Monitor::new(collect.clone(), CancelToken::new());
        let r = m.reporter();
        r.totals(Some(10), None);
        // A stage that never ran gets no last event.
        r.done_if_started(Stage::Peel);
        assert!(collect.0.lock().unwrap().is_empty());
        for _ in 0..49 {
            r.add(Stage::Encode, 1, 100, 0);
        }
        r.set(Stage::Encode, 50, 5000, 7, 0);
        r.done(Stage::Encode);
        let events = collect.0.lock().unwrap().clone();
        // 50 adds in well under the interval: the first one, then the forced last.
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].files_done, 1);
        assert_eq!(events[0].files_total, Some(10));
        assert!(!events[0].done);
        assert_eq!(events[1].files_done, 50);
        assert_eq!(events[1].bytes_in, 5000);
        assert_eq!(events[1].bytes_out, 7);
        assert!(events[1].done);
        // Another stage has its own first event.
        r.add(Stage::Seal, 0, 0, 0);
        assert_eq!(collect.0.lock().unwrap().len(), 3);
        // Past the interval an event passes again.
        std::thread::sleep(MIN_INTERVAL + Duration::from_millis(20));
        r.add(Stage::Seal, 0, 0, 1);
        assert_eq!(collect.0.lock().unwrap().len(), 4);
    }

    #[test]
    fn without_an_observer_nothing_is_delivered_and_nothing_is_counted() {
        let m = Monitor::default();
        let r = m.reporter();
        r.add(Stage::Walk, 1, 1, 1);
        r.done(Stage::Walk);
        assert_eq!(r.state.lock().unwrap().files_done, 0);
        assert!(!r.is_cancelled());
    }

    #[test]
    fn a_watched_reader_counts_and_fails_once_the_token_is_cancelled() {
        let collect = Arc::new(Collect(Mutex::new(Vec::new())));
        let token = CancelToken::new();
        let m = Monitor::new(collect.clone(), token.clone());
        let r = m.reporter();
        let mut w = r.watch(Box::new(std::io::Cursor::new(vec![7u8; 10_000])));
        let mut buf = [0u8; 4096];
        assert_eq!(w.read(&mut buf).unwrap(), 4096);
        let first = collect.0.lock().unwrap()[0];
        assert_eq!((first.stage, first.bytes_in), (Stage::Encode, 4096));
        // The file completes: its bytes fold into the absolute figure, never going back.
        r.set(Stage::Encode, 1, 10_000, 0, 0);
        r.done(Stage::Encode);
        assert_eq!(collect.0.lock().unwrap().last().unwrap().bytes_in, 10_000);
        token.cancel();
        let e = w.read(&mut buf).unwrap_err();
        assert_eq!(e.to_string(), "cancelled");
        let mapped = r.map_cancelled(CoreError::Io {
            path: "f".into(),
            source: e,
        });
        assert!(matches!(mapped, CoreError::Cancelled));
        let kept = r.map_cancelled(CoreError::Extract("x"));
        assert!(matches!(kept, CoreError::Extract("x")));
        // Without a cancelled token an I/O error stands.
        let r2 = Monitor::default().reporter();
        let kept = r2.map_cancelled(CoreError::Io {
            path: "f".into(),
            source: std::io::Error::other("disk"),
        });
        assert!(matches!(kept, CoreError::Io { .. }));
    }

    #[test]
    fn a_closure_is_an_observer_and_every_stage_has_a_name() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let observer: Arc<dyn Progress> = Arc::new(move |e: &Event| {
            s2.lock().unwrap().push(e.stage.name());
        });
        let m = Monitor::new(observer, CancelToken::new());
        let r = m.reporter();
        for stage in Stage::ALL {
            r.done(stage);
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["walk", "peel", "encode", "seal", "extract", "export"]
        );
        assert!(format!("{m:?}").contains("progress: true"));
    }
}
