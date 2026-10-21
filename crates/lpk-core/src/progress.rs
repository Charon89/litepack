//! Progress events and cancellation for the long-running engine entry points (create,
//! append, extract, export): a [`Progress`] observer that receives rate-limited [`Event`]s,
//! and a [`CancelToken`] the engine polls between files and between blocks.
//!
//! **Events.** An event names the [`Stage`] the engine is in and carries the run's counters
//! so far: files done (and the total when known), bytes in and out, blocks done (and the
//! total when known). The engine, not the observer, limits the rate: the first event of a
//! stage and the stage's last event (`done`) are always delivered, the rest at most one per
//! [`MIN_INTERVAL`] per stage, so an observer that writes a console line never has to drop
//! anything itself. Events are delivered on the engine's coordinating thread (the feeding
//! thread of a create or append, the dispatching thread of an extraction or export), never
//! from a worker, so an observer sees them in order; it must still be `Send + Sync`, as the
//! engine's scoped threads borrow the monitor.
//!
//! **Cancellation.** A [`CancelToken`] is cloneable and thread-safe (a shared flag); the
//! engine polls it between files and between blocks in every long loop, and its decode
//! workers poll it before each block. A cancelled run fails with [`CoreError::Cancelled`] and
//! cleans up exactly as a failure does: a new archive is removed, an append's generation is
//! cut off (D-59), an extraction keeps its finished files and removes the unfinished ones
//! (D-54), an export's sink is left to its caller (the command line removes the output file).
//! Work already handed to a thread — a block being encoded or decoded, a file being peeled —
//! completes before the engine joins the thread; nothing outlives the call.
//!
//! No event and no token changes a byte of any archive: the counters are read from the
//! writer's position and the loops' own counts.

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

/// A flag the engine polls: cloneable, thread-safe, set once by [`cancel`](Self::cancel).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel: every clone sees it; the engine stops at its next poll.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether [`cancel`](Self::cancel) was called.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
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

    /// The engine's side: counters and the rate limit.
    pub(crate) fn reporter(&self) -> Reporter<'_> {
        Reporter {
            monitor: self,
            state: Mutex::new(State::default()),
        }
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
    bytes_out: u64,
    blocks_done: u64,
    blocks_total: Option<u64>,
    last: [Option<Instant>; 6],
}

/// The engine's counters and the rate limit over a [`Monitor`]; shared by reference with the
/// run's threads (every method takes `&self`).
pub(crate) struct Reporter<'m> {
    monitor: &'m Monitor,
    state: Mutex<State>,
}

impl Reporter<'_> {
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

    /// Set the totals once they are known.
    pub(crate) fn totals(&self, files: Option<u64>, blocks: Option<u64>) {
        if let Ok(mut s) = self.state.lock() {
            s.files_total = files;
            s.blocks_total = blocks;
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

    /// Set the counters from absolute figures and deliver an event of `stage` if the rate
    /// limit allows.
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
            s.bytes_out = bytes_out;
            s.blocks_done = blocks_done;
        });
    }

    /// Deliver the stage's last event, whatever the rate limit.
    pub(crate) fn done(&self, stage: Stage) {
        self.update(stage, true, |_| {});
    }

    fn update(&self, stage: Stage, done: bool, f: impl FnOnce(&mut State)) {
        let Some(progress) = &self.monitor.progress else {
            return;
        };
        let event = {
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
            Event {
                stage,
                files_done: s.files_done,
                files_total: s.files_total,
                bytes_in: s.bytes_in,
                bytes_out: s.bytes_out,
                blocks_done: s.blocks_done,
                blocks_total: s.blocks_total,
                done,
            }
        };
        progress.event(&event);
    }
}

#[cfg(test)]
mod tests {
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
