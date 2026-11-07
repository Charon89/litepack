//! One worker for the verbs second instances forward (E5-7): the event loop hands each over
//! without blocking, and one long-lived thread parses, mints and pushes them in arrival order
//! (parsing reads `@file`s, perhaps on a slow share, so it never runs on the event loop; one
//! thread rather than a task each, so two quick second instances cannot overtake each other).

use std::sync::mpsc::{channel, Sender};
use std::thread::JoinHandle;

/// The worker and its queue.
#[derive(Debug)]
pub struct Forwarder<T> {
    tx: Sender<T>,
    worker: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Forwarder<T> {
    /// Start the worker, which runs `handle` on each item in the order they were sent.
    pub fn start(mut handle: impl FnMut(T) + Send + 'static) -> std::io::Result<Self> {
        let (tx, rx) = channel::<T>();
        let worker = std::thread::Builder::new()
            .name("lpk-gui-forward".into())
            .spawn(move || {
                for item in rx {
                    handle(item);
                }
            })?;
        Ok(Forwarder {
            tx,
            worker: Some(worker),
        })
    }

    /// Queue `item` (never blocks).
    pub fn send(&self, item: T) {
        let _ = self.tx.send(item);
    }

    /// Stop taking items and wait until the worker has handled every queued one.
    pub fn close(mut self) {
        let worker = self.worker.take();
        drop(self);
        if let Some(w) = worker {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn items_are_handled_in_arrival_order() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let f = Forwarder::start(move |n: u32| {
            // Uneven work, as parses of different command lines are.
            if n.is_multiple_of(7) {
                std::thread::yield_now();
            }
            s.lock().unwrap().push(n);
        })
        .unwrap();
        for n in 0..500 {
            f.send(n);
        }
        f.close();
        assert_eq!(*seen.lock().unwrap(), (0..500).collect::<Vec<_>>());
    }
}
