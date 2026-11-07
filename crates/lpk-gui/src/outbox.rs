//! The notices Rust pushes to the page (drops, a second instance's verbs, an update): sent to
//! every subscribed channel, or held until the page subscribes (E5-7). The subscribers and the
//! held notices sit under one lock, and both `push` and `subscribe` decide inside it, so a
//! notice pushed while the page subscribes is delivered exactly once, in order. At most
//! [`MAX_HELD`] notices are held; one more, the caller's overflow notice, says the rest were
//! dropped.
//!
//! Generic over the channel and the notice (the host's are Tauri's `Channel` and its `Notice`),
//! so that it is tested without a webview.

use std::sync::Mutex;

/// The most notices held before the page subscribes.
pub const MAX_HELD: usize = 64;

struct State<C, N> {
    subscribers: Vec<C>,
    held: Vec<N>,
    overflowed: bool,
}

/// The page's notices: subscribers, and what waits for the first one.
pub struct Outbox<C, N> {
    state: Mutex<State<C, N>>,
}

impl<C, N> std::fmt::Debug for Outbox<C, N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outbox").finish_non_exhaustive()
    }
}

impl<C, N> Default for Outbox<C, N> {
    fn default() -> Self {
        Outbox {
            state: Mutex::new(State {
                subscribers: Vec::new(),
                held: Vec::new(),
                overflowed: false,
            }),
        }
    }
}

impl<C, N: Clone> Outbox<C, N> {
    /// Nothing held, no subscriber.
    pub fn new() -> Self {
        Self::default()
    }

    /// Send `notice` to every subscriber with `send`, or hold it (past [`MAX_HELD`], hold
    /// `overflow()` once instead).
    pub fn push(&self, notice: N, overflow: impl FnOnce() -> N, send: impl Fn(&C, N)) {
        // A send that panicked under the lock leaves the lists whole (nothing is half-updated
        // around a send): carry on with them, as the crate does elsewhere.
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if s.subscribers.is_empty() {
            if s.held.len() < MAX_HELD {
                s.held.push(notice);
            } else if !s.overflowed {
                s.held.push(overflow());
                s.overflowed = true;
            }
            return;
        }
        for c in &s.subscribers {
            send(c, notice.clone());
        }
    }

    /// Add `channel`: the held notices go to it first, in order, with `send`.
    pub fn subscribe(&self, channel: C, send: impl Fn(&C, N)) {
        // Poisoned: the lists are still whole (see `push`), so the channel is still subscribed.
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        for n in std::mem::take(&mut s.held) {
            send(&channel, n);
        }
        s.overflowed = false;
        s.subscribers.push(channel);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    type Sink = Arc<Mutex<Vec<u32>>>;

    fn send(c: &Sink, n: u32) {
        c.lock().unwrap().push(n);
    }

    const OVERFLOW: u32 = u32::MAX;

    #[test]
    fn held_notices_reach_the_first_subscriber_once_in_order() {
        let o: Outbox<Sink, u32> = Outbox::new();
        for n in 0..3 {
            o.push(n, || OVERFLOW, send);
        }
        let a = Sink::default();
        o.subscribe(a.clone(), send);
        o.push(3, || OVERFLOW, send);
        let b = Sink::default();
        o.subscribe(b.clone(), send);
        o.push(4, || OVERFLOW, send);
        assert_eq!(*a.lock().unwrap(), [0, 1, 2, 3, 4]);
        assert_eq!(*b.lock().unwrap(), [4]);
    }

    #[test]
    fn a_push_racing_the_subscribe_is_neither_lost_nor_doubled() {
        for _ in 0..300 {
            let o: Arc<Outbox<Sink, u32>> = Arc::new(Outbox::new());
            let sink = Sink::default();
            let pusher = {
                let o = o.clone();
                std::thread::spawn(move || {
                    for n in 0..40 {
                        o.push(n, || OVERFLOW, send);
                    }
                })
            };
            let subscriber = {
                let (o, sink) = (o.clone(), sink.clone());
                std::thread::spawn(move || o.subscribe(sink, send))
            };
            pusher.join().unwrap();
            subscriber.join().unwrap();
            assert_eq!(*sink.lock().unwrap(), (0..40).collect::<Vec<_>>());
        }
    }

    #[test]
    fn the_cap_holds_64_and_one_overflow_notice() {
        let o: Outbox<Sink, u32> = Outbox::new();
        for n in 0..100 {
            o.push(n, || OVERFLOW, send);
        }
        let a = Sink::default();
        o.subscribe(a.clone(), send);
        let got = a.lock().unwrap().clone();
        assert_eq!(got.len(), MAX_HELD + 1);
        assert_eq!(got[..MAX_HELD], (0..64).collect::<Vec<_>>()[..]);
        assert_eq!(got[MAX_HELD], OVERFLOW);
    }
}
