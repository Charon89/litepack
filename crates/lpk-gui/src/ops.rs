//! Running operations (`docs/notes/e5-design.md` section 1). Each long command registers an
//! operation under the id the page chose, with its own [`CancelToken`]; `cancel` sets it and
//! the engine cleans up (D-87 (2)). A heavy operation (create, append, extract, test) waits
//! for the one before it: each may take half the memory (D-67 (1)). The exit cancels every
//! token and joins every run before the process ends.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lpk_api::CancelToken;

use crate::GuiError;

#[derive(Debug, Default)]
struct Inner {
    running: HashMap<u64, CancelToken>,
    heavy_busy: bool,
    closing: bool,
}

/// The process's operations.
#[derive(Debug, Default)]
pub struct Ops {
    inner: Mutex<Inner>,
    changed: Condvar,
}

/// A registered operation: its token; dropping it ends the operation.
#[derive(Debug)]
pub struct OpGuard {
    ops: Arc<Ops>,
    id: u64,
    heavy: bool,
    /// The operation's token (cancelled by [`Ops::cancel`] or [`Ops::cancel_all`]).
    pub token: CancelToken,
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        let mut g = self.ops.lock();
        g.running.remove(&self.id);
        if self.heavy {
            g.heavy_busy = false;
        }
        drop(g);
        self.ops.changed.notify_all();
    }
}

impl Ops {
    /// No operation.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Register operation `id`. A heavy one waits until no other heavy one runs (a cancelled
    /// wait or the exit ends it with `Cancelled`); an id already running is `Usage`.
    pub fn begin(self: &Arc<Self>, id: u64, heavy: bool) -> Result<OpGuard, GuiError> {
        let token = CancelToken::new();
        let mut g = self.lock();
        if g.closing {
            return Err(lpk_api::Error::cancelled().into());
        }
        if g.running.contains_key(&id) {
            return Err(GuiError::usage(format!(
                "operation {id} is already running"
            )));
        }
        // Registered while it waits, so that it can be cancelled in the queue.
        g.running.insert(id, token.clone());
        if heavy {
            while g.heavy_busy && !g.closing && !token.is_cancelled() {
                g = self.changed.wait(g).unwrap_or_else(|p| p.into_inner());
            }
            if g.closing || token.is_cancelled() {
                g.running.remove(&id);
                drop(g);
                self.changed.notify_all();
                return Err(lpk_api::Error::cancelled().into());
            }
            g.heavy_busy = true;
        }
        Ok(OpGuard {
            ops: Arc::clone(self),
            id,
            heavy,
            token,
        })
    }

    /// Cancel operation `id`; false when none runs under it.
    pub fn cancel(&self, id: u64) -> bool {
        let g = self.lock();
        let found = g.running.get(&id).map(CancelToken::cancel).is_some();
        drop(g);
        self.changed.notify_all();
        found
    }

    /// The number of operations registered (running or queued).
    pub fn running(&self) -> usize {
        self.lock().running.len()
    }

    /// Whether any operation is registered.
    pub fn busy(&self) -> bool {
        self.running() > 0
    }

    /// The exit: refuse new operations and cancel every token.
    pub fn cancel_all(&self) {
        let mut g = self.lock();
        g.closing = true;
        for t in g.running.values() {
            t.cancel();
        }
        drop(g);
        self.changed.notify_all();
    }

    /// Wait until every operation has ended, at most `timeout`; true when none is left.
    pub fn join(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        let mut g = self.lock();
        while !g.running.is_empty() {
            let now = Instant::now();
            if now >= end {
                return false;
            }
            g = self
                .changed
                .wait_timeout(g, end - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn one_heavy_operation_at_a_time() {
        let ops = Ops::new();
        let live = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..4)
            .map(|i| {
                let (ops, live, most) = (ops.clone(), live.clone(), most.clone());
                std::thread::spawn(move || {
                    let _g = ops.begin(i, true).unwrap();
                    let n = live.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    live.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(most.load(Ordering::SeqCst), 1);
        assert!(!ops.busy());
    }

    #[test]
    fn light_operations_run_beside_a_heavy_one() {
        let ops = Ops::new();
        let _h = ops.begin(1, true).unwrap();
        let _l = ops.begin(2, false).unwrap();
        assert_eq!(ops.running(), 2);
        assert!(ops.begin(2, false).is_err(), "an id runs once");
    }

    #[test]
    fn cancel_reaches_the_operations_token_and_a_queued_one() {
        let ops = Ops::new();
        let g = ops.begin(7, true).unwrap();
        assert!(ops.cancel(7));
        assert!(g.token.is_cancelled());
        assert!(!ops.cancel(8));
        // A heavy operation queued behind 7 is cancelled while it waits.
        let ops2 = ops.clone();
        let queued = std::thread::spawn(move || ops2.begin(9, true).map(|_| ()));
        while ops.running() < 2 {
            std::thread::yield_now();
        }
        ops.cancel(9);
        let e = queued.join().unwrap().unwrap_err();
        assert!(e.is(lpk_api::Class::Cancelled), "{e}");
        drop(g);
    }

    #[test]
    fn exit_cancels_every_token_joins_and_refuses_new_work() {
        let ops = Ops::new();
        let workers: Vec<_> = (0..3)
            .map(|i| {
                let g = ops.begin(i, i == 0).unwrap();
                std::thread::spawn(move || {
                    // An engine loop: polls its token.
                    while !g.token.is_cancelled() {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    drop(g);
                })
            })
            .collect();
        ops.cancel_all();
        assert!(ops.join(Duration::from_secs(10)));
        for w in workers {
            w.join().unwrap();
        }
        let e = ops.begin(5, false).unwrap_err();
        assert!(e.is(lpk_api::Class::Cancelled), "{e}");
    }

    #[test]
    fn join_times_out_while_an_operation_ignores_its_token() {
        let ops = Ops::new();
        let _g = ops.begin(1, false).unwrap();
        ops.cancel_all();
        assert!(!ops.join(Duration::from_millis(20)));
    }
}
