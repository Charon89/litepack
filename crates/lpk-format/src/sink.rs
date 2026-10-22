//! Where a [`crate::Writer`]'s bytes go: straight to its output, or through a
//! write thread that owns the output ([`crate::Writer::with_write_thread`]).
//!
//! With a write thread the writer decides everything in order as before
//! (positions, sequence numbers, hashes, the recovery spool's bytes) and only
//! the `write` calls move: small writes are staged and sent with the next
//! frame body, a frame body (a block buffer) is moved, not copied, and comes
//! back for reuse once written. The queue holds [`WRITE_DEPTH`] messages
//! beyond the one being written.
//!
//! Errors: the first failed write stops all writing (later messages are
//! dropped, their buffers still returned) and is kept; every later call of
//! the writer fails with it ([`Sink::error`]), and a flush reports it. A panic
//! of the thread leaves a sticky error too, and is re-raised by the call that
//! finds it. Threads: the thread ends when the queue closes and is joined when
//! the sink is dropped; the output is dropped on that thread (a `BufWriter`
//! then flushes what it holds, as it would on the writer's own thread).

use std::any::Any;
use std::io::{ErrorKind, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

/// Frames queued for the write thread at most, beyond the one it writes.
pub const WRITE_DEPTH: usize = 1;

/// Staged small writes are sent once they reach this many bytes.
const STAGE_MAX: usize = 1 << 20;

/// The first failure of the write thread, kept for every later call.
type Sticky = Arc<Mutex<Option<(ErrorKind, String)>>>;

enum Msg {
    /// Write `small`, then `big` (returned when `back` is set, once written or
    /// once dropped after a failure; dropped otherwise).
    Write {
        small: Vec<u8>,
        big: Option<Vec<u8>>,
        back: bool,
    },
    /// Flush the output and answer with the outcome (the kept failure, if any).
    Flush(SyncSender<std::io::Result<()>>),
}

/// The output of a writer.
pub(crate) enum Sink<W> {
    /// Written on the writer's thread.
    Direct(W),
    /// Written by a write thread.
    Thread(WriteThread),
    /// Moved out while switching (never seen by a call).
    Gone,
}

/// A write thread that owns the output.
pub(crate) struct WriteThread {
    tx: Option<SyncSender<Msg>>,
    back: Receiver<Vec<u8>>,
    sticky: Sticky,
    handle: Option<JoinHandle<()>>,
    staged: Vec<u8>,
    /// Write messages sent and not yet written (the one being written included).
    queued: Arc<AtomicUsize>,
}

fn sticky_error(s: &Sticky) -> Option<std::io::Error> {
    let g = s.lock().unwrap_or_else(PoisonError::into_inner);
    g.as_ref()
        .map(|(kind, msg)| std::io::Error::new(*kind, msg.clone()))
}

fn set_sticky(s: &Sticky, kind: ErrorKind, msg: String) {
    let mut g = s.lock().unwrap_or_else(PoisonError::into_inner);
    if g.is_none() {
        *g = Some((kind, msg));
    }
}

impl<W: Write + Send + 'static> Sink<W> {
    /// Move a direct output onto a write thread. When the thread cannot be
    /// started the output stays direct.
    pub(crate) fn start_thread(&mut self) {
        let Sink::Direct(_) = self else {
            return;
        };
        let Sink::Direct(out) = std::mem::replace(self, Sink::Gone) else {
            return;
        };
        // Handed over through a slot, so a failed spawn gives the output back.
        let slot = Arc::new(Mutex::new(Some(out)));
        let theirs = slot.clone();
        let (tx, rx) = mpsc::sync_channel::<Msg>(WRITE_DEPTH);
        let (back_tx, back) = mpsc::channel::<Vec<u8>>();
        let sticky: Sticky = Arc::new(Mutex::new(None));
        let s = sticky.clone();
        let queued = Arc::new(AtomicUsize::new(0));
        let q = queued.clone();
        let spawned = std::thread::Builder::new()
            .name("lpk-write".to_string())
            .spawn(move || {
                let out = theirs.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(out) = out {
                    write_loop(out, &rx, &back_tx, &s, &q);
                }
            });
        *self = match spawned {
            Ok(handle) => Sink::Thread(WriteThread {
                tx: Some(tx),
                back,
                sticky,
                handle: Some(handle),
                staged: Vec::new(),
                queued,
            }),
            Err(_) => match slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
                Some(out) => Sink::Direct(out),
                None => Sink::Gone,
            },
        };
    }
}

impl<W: Write> Sink<W> {
    /// The write thread's kept failure, if any.
    pub(crate) fn error(&self) -> Option<std::io::Error> {
        match self {
            Sink::Thread(t) => sticky_error(&t.sticky),
            Sink::Direct(_) => None,
            Sink::Gone => Some(std::io::Error::other("the output is gone")),
        }
    }

    /// Write `buf` whole (staged with a write thread).
    pub(crate) fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            Sink::Direct(w) => {
                let mut writing = crate::profile::Timer::start(crate::profile::Stage::Write);
                writing.bytes = buf.len() as u64;
                w.write_all(buf)
            }
            Sink::Thread(t) => {
                if let Some(e) = sticky_error(&t.sticky) {
                    return Err(e);
                }
                t.staged.extend_from_slice(buf);
                if t.staged.len() >= STAGE_MAX {
                    t.send(None, false)?;
                }
                Ok(())
            }
            Sink::Gone => Err(std::io::Error::other("the output is gone")),
        }
    }

    /// Write `buf` whole, by value. With `back` it comes back for reuse: at
    /// once when written here, later through [`Sink::returned`] with a write
    /// thread; without, it is dropped once written (in both modes alike).
    pub(crate) fn write_owned(
        &mut self,
        buf: Vec<u8>,
        back: bool,
    ) -> std::io::Result<Option<Vec<u8>>> {
        match self {
            Sink::Direct(w) => {
                let mut writing = crate::profile::Timer::start(crate::profile::Stage::Write);
                writing.bytes = buf.len() as u64;
                w.write_all(&buf)?;
                drop(writing);
                Ok(back.then_some(buf))
            }
            Sink::Thread(t) => {
                t.send(Some(buf), back)?;
                Ok(None)
            }
            Sink::Gone => Err(std::io::Error::other("the output is gone")),
        }
    }

    /// Flush: with a write thread, everything sent so far is written and the
    /// output flushed before this returns.
    pub(crate) fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Direct(w) => w.flush(),
            Sink::Thread(t) => t.flush(),
            Sink::Gone => Err(std::io::Error::other("the output is gone")),
        }
    }

    /// Write messages queued for the write thread and not yet written (0
    /// without one).
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        match self {
            Sink::Thread(t) => t.queued.load(Ordering::SeqCst),
            _ => 0,
        }
    }

    /// Buffers the write thread wrote and gave back.
    pub(crate) fn returned(&mut self) -> Vec<Vec<u8>> {
        match self {
            Sink::Thread(t) => t.back.try_iter().collect(),
            _ => Vec::new(),
        }
    }
}

impl WriteThread {
    /// Send the staged bytes and `big`. A failure kept by the thread is
    /// returned first; a thread that is gone has panicked (its panic is
    /// re-raised once a sticky error marks the writer failed) or ended.
    fn send(&mut self, big: Option<Vec<u8>>, back: bool) -> std::io::Result<()> {
        if let Some(e) = sticky_error(&self.sticky) {
            return Err(e);
        }
        let small = std::mem::take(&mut self.staged);
        self.queued.fetch_add(1, Ordering::SeqCst);
        let sent = match &self.tx {
            Some(tx) => tx.send(Msg::Write { small, big, back }).is_ok(),
            None => false,
        };
        if sent {
            Ok(())
        } else {
            Err(self.gone())
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send(None, false)?;
        let (reply_tx, reply) = mpsc::sync_channel(1);
        let sent = match &self.tx {
            Some(tx) => tx.send(Msg::Flush(reply_tx)).is_ok(),
            None => false,
        };
        if !sent {
            return Err(self.gone());
        }
        match reply.recv() {
            Ok(r) => r,
            Err(_) => Err(self.gone()),
        }
    }

    /// The thread is gone: join it, mark the sink failed and re-raise its
    /// panic; without one, the error to report.
    fn gone(&mut self) -> std::io::Error {
        self.tx = None;
        let msg = "the write thread ended".to_string();
        set_sticky(&self.sticky, ErrorKind::Other, msg.clone());
        if let Some(h) = self.handle.take() {
            if let Err(p) = h.join() {
                set_sticky(
                    &self.sticky,
                    ErrorKind::Other,
                    "the write thread panicked".to_string(),
                );
                std::panic::resume_unwind(p);
            }
        }
        sticky_error(&self.sticky).unwrap_or_else(|| std::io::Error::other(msg))
    }
}

impl Drop for WriteThread {
    /// Close the queue and join the thread. A panic no call found before the
    /// drop is dropped here, not re-raised (a drop may run while unwinding,
    /// where a second panic aborts); the archive is abandoned by the drop
    /// anyway.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _: Result<(), Box<dyn Any + Send>> = h.join();
        }
    }
}

/// The write thread: every message in order until the queue closes.
fn write_loop<W: Write>(
    mut out: W,
    rx: &Receiver<Msg>,
    back: &Sender<Vec<u8>>,
    sticky: &Sticky,
    queued: &AtomicUsize,
) {
    let mut failed = false;
    for msg in rx {
        match msg {
            Msg::Write {
                small,
                big,
                back: give_back,
            } => {
                if !failed {
                    let mut writing = crate::profile::Timer::start(crate::profile::Stage::Write);
                    writing.bytes = (small.len() + big.as_ref().map_or(0, Vec::len)) as u64;
                    let r = out
                        .write_all(&small)
                        .and_then(|()| big.as_ref().map_or(Ok(()), |b| out.write_all(b)));
                    drop(writing);
                    if let Err(e) = r {
                        failed = true;
                        set_sticky(sticky, e.kind(), e.to_string());
                    }
                }
                if let Some(mut b) = big.filter(|_| give_back) {
                    // Restored to its full capacity here, off the writer's
                    // thread, so the pool hands it out without touching it
                    // (the writer's pending block keeps its full length).
                    b.resize(b.capacity(), 0);
                    // A writer that is gone no longer needs it.
                    let _ = back.send(b);
                }
                queued.fetch_sub(1, Ordering::SeqCst);
            }
            Msg::Flush(reply) => {
                let r = if failed {
                    Err(sticky_error(sticky)
                        .unwrap_or_else(|| std::io::Error::other("write failed")))
                } else {
                    out.flush().inspect_err(|e| {
                        failed = true;
                        set_sticky(sticky, e.kind(), e.to_string());
                    })
                };
                let _ = reply.send(r);
            }
        }
    }
}
