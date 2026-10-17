//! Reading ahead: a reader thread reads the contents of a list of files, in
//! order, into recycled read buffers handed to the feeding thread through a
//! bounded queue, so reading overlaps cutting ([`crate::Writer::add_file_segments`]).
//!
//! The reader opens and reads the files in the order given. A file that fails
//! to open, or a read that fails, ends the reader after it queued that failure:
//! the feeding thread meets it when it reaches that file, at the segment that
//! failed, as a sequential read would have reported it; nothing after it is read.
//!
//! Threads: one reader, started by [`Prefetch::spawn`] and joined when the
//! [`Prefetch`] is dropped (on success, error or panic alike). Dropping it first
//! closes both queues, so a reader blocked on a full queue or waiting for a free
//! buffer wakes up and ends; a reader blocked inside a read of the input holds
//! the drop until that read returns. A panic of the reader reaches the feeding
//! thread as [`SegmentError::Panicked`] (inside the writer, which marks itself
//! failed and re-raises it) or is re-raised by [`Prefetch::next_file`].
//!
//! Memory: at most [`PrefetchOptions::buffers`] read buffers of
//! [`PrefetchOptions::segment`] bytes each, allocated once and recycled.

use std::any::Any;
use std::io::Read;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread::JoinHandle;

/// Opens one file for the reader thread.
pub type OpenFn<E> = Box<dyn FnOnce() -> Result<Box<dyn Read + Send>, E> + Send>;

/// Where a file's next segment comes from: the feeding side of a read-ahead.
pub trait Segments {
    /// The next bytes of the file, in order, and whether they are known to be
    /// its last (`None` at its end, also right after a last segment). A
    /// returned buffer goes back with [`Segments::give_back`] once its bytes
    /// are used.
    fn next_segment(&mut self) -> Result<Option<(Vec<u8>, bool)>, SegmentError>;
    /// Return a buffer [`Segments::next_segment`] handed out.
    fn give_back(&mut self, buf: Vec<u8>);
}

/// Why a file's next segment is missing.
pub enum SegmentError {
    /// The read failed.
    Io(std::io::Error),
    /// The reader thread panicked; the payload, to re-raise.
    Panicked(Box<dyn Any + Send>),
}

impl std::fmt::Debug for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SegmentError::Io(e) => write!(f, "Io({e:?})"),
            SegmentError::Panicked(_) => write!(f, "Panicked"),
        }
    }
}

/// Sizes of a read-ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefetchOptions {
    /// Bytes per segment (a read buffer's size); at least 1.
    pub segment: usize,
    /// Read buffers at most; at least 2 (one being cut, one being filled).
    pub buffers: usize,
}

impl Default for PrefetchOptions {
    /// Segments of 8 MiB, 4 buffers.
    fn default() -> Self {
        PrefetchOptions {
            segment: 8 << 20,
            buffers: 4,
        }
    }
}

/// What the reader queues, in order.
enum Msg<E> {
    /// File `id` opened; its segments follow.
    Opened(usize),
    /// The next file did not open; the reader has stopped.
    OpenFailed(E),
    /// The next bytes of the open file, and whether the read met its end.
    Data(Vec<u8>, bool),
    /// A read of the open file failed; the reader has stopped.
    ReadFailed(std::io::Error),
    /// The open file ended after this many bytes.
    End(u64),
}

/// A reader thread reading files ahead; see the module documentation.
pub struct Prefetch<E: Send + 'static> {
    rx: Option<Receiver<Msg<E>>>,
    back: Option<Sender<Vec<u8>>>,
    handle: Option<JoinHandle<()>>,
    /// Tests only: every buffer given back is filled with this byte first.
    #[cfg(test)]
    poison: Option<u8>,
}

impl<E: Send + 'static> std::fmt::Debug for Prefetch<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prefetch")
            .field("running", &self.handle.is_some())
            .finish()
    }
}

impl<E: Send + 'static> Prefetch<E> {
    /// Start the reader over `files`, each with the id the feeding side checks
    /// ([`PrefetchedFile::id`]). The queue holds `buffers - 1` segments, so
    /// with the one being cut no more than `buffers` exist.
    pub fn spawn(
        files: Vec<(usize, OpenFn<E>)>,
        options: PrefetchOptions,
    ) -> std::io::Result<Self> {
        let segment = options.segment.max(1);
        let buffers = options.buffers.max(2);
        let (tx, rx) = mpsc::sync_channel::<Msg<E>>(buffers - 1);
        let (back, free) = mpsc::channel::<Vec<u8>>();
        let handle = std::thread::Builder::new()
            .name("lpk-read".to_string())
            .spawn(move || read_files(files, &tx, &free, segment, buffers))?;
        Ok(Prefetch {
            rx: Some(rx),
            back: Some(back),
            handle: Some(handle),
            #[cfg(test)]
            poison: None,
        })
    }

    /// Wait for the next file: its segments, or the error opening it. A reader
    /// that panicked is joined and its panic re-raised here.
    ///
    /// # Panics
    /// When the reader thread panicked (its payload, re-raised), or when called
    /// past the last file.
    pub fn next_file(&mut self) -> Result<PrefetchedFile<'_, E>, E> {
        match self.recv() {
            Ok(Msg::Opened(id)) => Ok(PrefetchedFile {
                prefetch: self,
                id,
                read: 0,
                done: false,
            }),
            Ok(Msg::OpenFailed(e)) => Err(e),
            Ok(_) => panic!("read-ahead out of step: segments outside a file"),
            Err(SegmentError::Panicked(p)) => std::panic::resume_unwind(p),
            Err(SegmentError::Io(e)) => panic!("read-ahead ended early: {e}"),
        }
    }

    /// The next message; a closed queue means the reader ended: its panic, or
    /// (a reader that ended without one) an error.
    fn recv(&mut self) -> Result<Msg<E>, SegmentError> {
        let r = match &self.rx {
            Some(rx) => rx.recv().ok(),
            None => None,
        };
        match r {
            Some(m) => Ok(m),
            None => {
                self.rx = None;
                if let Some(h) = self.handle.take() {
                    if let Err(p) = h.join() {
                        return Err(SegmentError::Panicked(p));
                    }
                }
                Err(SegmentError::Io(std::io::Error::other(
                    "the read-ahead thread ended",
                )))
            }
        }
    }

    /// Return a read buffer to the reader.
    fn give_back(&mut self, mut buf: Vec<u8>) {
        #[cfg(test)]
        if let Some(b) = self.poison {
            buf.clear();
            buf.resize(buf.capacity(), b);
        }
        buf.clear();
        if let Some(back) = &self.back {
            // A reader that ended no longer needs it.
            let _ = back.send(buf);
        }
    }

    /// Tests only: fill every buffer given back with `byte`.
    #[cfg(test)]
    pub(crate) fn poison(&mut self, byte: u8) {
        self.poison = Some(byte);
    }
}

impl<E: Send + 'static> Drop for Prefetch<E> {
    /// Close both queues, then join the reader (its panic, if any, dropped).
    fn drop(&mut self) {
        self.rx = None;
        self.back = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// One file of a read-ahead: its segments, in order ([`Segments`]).
pub struct PrefetchedFile<'a, E: Send + 'static> {
    prefetch: &'a mut Prefetch<E>,
    id: usize,
    read: u64,
    done: bool,
}

impl<E: Send + 'static> std::fmt::Debug for PrefetchedFile<'_, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrefetchedFile")
            .field("id", &self.id)
            .field("read", &self.read)
            .finish()
    }
}

impl<E: Send + 'static> PrefetchedFile<'_, E> {
    /// The id the file was given at [`Prefetch::spawn`].
    pub fn id(&self) -> usize {
        self.id
    }

    /// Bytes handed out so far (the file's length once it ended).
    pub fn bytes_read(&self) -> u64 {
        self.read
    }
}

impl<E: Send + 'static> Segments for PrefetchedFile<'_, E> {
    fn next_segment(&mut self) -> Result<Option<(Vec<u8>, bool)>, SegmentError> {
        if self.done {
            return Ok(None);
        }
        match self.prefetch.recv()? {
            Msg::Data(buf, last) => {
                self.read += buf.len() as u64;
                Ok(Some((buf, last)))
            }
            Msg::End(n) => {
                debug_assert_eq!(n, self.read);
                self.done = true;
                Ok(None)
            }
            Msg::ReadFailed(e) => {
                self.done = true;
                Err(SegmentError::Io(e))
            }
            Msg::Opened(_) | Msg::OpenFailed(_) => Err(SegmentError::Io(std::io::Error::other(
                "read-ahead out of step: a file inside a file",
            ))),
        }
    }

    fn give_back(&mut self, buf: Vec<u8>) {
        self.prefetch.give_back(buf);
    }
}

/// The reader thread: every file in order, until the end, a failure or a
/// closed queue.
fn read_files<E>(
    files: Vec<(usize, OpenFn<E>)>,
    tx: &SyncSender<Msg<E>>,
    free: &Receiver<Vec<u8>>,
    segment: usize,
    buffers: usize,
) {
    let mut made = 0usize;
    let mut spare: Option<Vec<u8>> = None;
    for (id, open) in files {
        let mut input = match open() {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(Msg::OpenFailed(e));
                return;
            }
        };
        if tx.send(Msg::Opened(id)).is_err() {
            return;
        }
        let mut total = 0u64;
        loop {
            // A buffer: one kept back, one returned, a new one while fewer
            // than `buffers` exist, else wait for one to come back.
            let mut buf = match spare.take() {
                Some(b) => b,
                None => match free.try_recv() {
                    Ok(b) => b,
                    Err(TryRecvError::Empty) if made < buffers => {
                        made += 1;
                        Vec::with_capacity(segment)
                    }
                    Err(TryRecvError::Empty) => match free.recv() {
                        Ok(b) => b,
                        Err(_) => return,
                    },
                    Err(TryRecvError::Disconnected) => return,
                },
            };
            buf.clear();
            let n = match (&mut input).take(segment as u64).read_to_end(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.send(Msg::ReadFailed(e));
                    return;
                }
            };
            total += n as u64;
            if n == 0 {
                spare = Some(buf);
                break;
            }
            // A short read met the end (`read_to_end` stops only there or at
            // the segment's size).
            let last = n < segment;
            if tx.send(Msg::Data(buf, last)).is_err() {
                return;
            }
            if last {
                break;
            }
        }
        if tx.send(Msg::End(total)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn opener(data: Vec<u8>) -> OpenFn<String> {
        Box::new(move || Ok(Box::new(Cursor::new(data)) as Box<dyn Read + Send>))
    }

    fn collect(f: &mut PrefetchedFile<'_, String>) -> Result<Vec<u8>, SegmentError> {
        let mut out = Vec::new();
        while let Some((b, _)) = f.next_segment()? {
            out.extend_from_slice(&b);
            f.give_back(b);
        }
        Ok(out)
    }

    #[test]
    fn files_come_back_whole_and_in_order() {
        let files: Vec<Vec<u8>> = (0..7u8)
            .map(|i| {
                (0..(i as usize * 1000 + 3))
                    .map(|j| (j as u8) ^ i)
                    .collect()
            })
            .collect();
        let list = files
            .iter()
            .enumerate()
            .map(|(i, d)| (i, opener(d.clone())))
            .collect();
        let opts = PrefetchOptions {
            segment: 1000,
            buffers: 2,
        };
        let mut p = Prefetch::spawn(list, opts).unwrap();
        for (i, want) in files.iter().enumerate() {
            let mut f = p.next_file().unwrap();
            assert_eq!(f.id(), i);
            assert_eq!(&collect(&mut f).unwrap(), want);
            assert_eq!(f.bytes_read(), want.len() as u64);
        }
    }

    #[test]
    fn an_open_error_comes_at_its_file() {
        let list: Vec<(usize, OpenFn<String>)> = vec![
            (0, opener(vec![1; 10])),
            (1, Box::new(|| Err("gone".to_string()))),
            (2, opener(vec![2; 10])),
        ];
        let mut p = Prefetch::spawn(list, PrefetchOptions::default()).unwrap();
        let mut f = p.next_file().unwrap();
        assert_eq!(collect(&mut f).unwrap(), vec![1; 10]);
        assert_eq!(p.next_file().unwrap_err(), "gone");
    }

    /// A reader that yields `ok` bytes of a pattern, then fails or panics; it
    /// raises `dropped` when the reader thread lets go of it.
    struct Faulty {
        ok: usize,
        at: usize,
        panic: bool,
        dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Read for Faulty {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.at >= self.ok {
                if self.panic {
                    panic!("reader panics mid-file");
                }
                return Err(std::io::Error::other("disk gone mid-file"));
            }
            let n = buf.len().min(self.ok - self.at).min(777);
            for b in &mut buf[..n] {
                *b = (self.at % 251) as u8;
                self.at += 1;
            }
            Ok(n)
        }
    }

    impl Drop for Faulty {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn faulty(
        ok: usize,
        panic: bool,
    ) -> (
        OpenFn<String>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d = dropped.clone();
        let open: OpenFn<String> = Box::new(move || {
            Ok(Box::new(Faulty {
                ok,
                at: 0,
                panic,
                dropped: d,
            }) as Box<dyn Read + Send>)
        });
        (open, dropped)
    }

    const SMALL: PrefetchOptions = PrefetchOptions {
        segment: 4096,
        buffers: 2,
    };

    #[test]
    fn a_read_error_comes_at_its_segment_and_the_reader_ends() {
        let (open, dropped) = faulty(10_000, false);
        let mut p = Prefetch::spawn(vec![(0, opener(vec![9; 5000])), (1, open)], SMALL).unwrap();
        let mut f = p.next_file().unwrap();
        assert_eq!(collect(&mut f).unwrap(), vec![9; 5000]);
        let mut f = p.next_file().unwrap();
        let mut got = 0usize;
        let e = loop {
            match f.next_segment() {
                Ok(Some((b, _))) => {
                    got += b.len();
                    f.give_back(b);
                }
                Ok(None) => panic!("no error"),
                Err(e) => break e,
            }
        };
        assert!(matches!(e, SegmentError::Io(ref e) if e.to_string() == "disk gone mid-file"));
        // The whole segments before the failing one came through.
        assert_eq!(got, 8192);
        drop(p);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn a_reader_panic_reaches_the_feeding_side_once_joined() {
        let (open, _) = faulty(6000, true);
        let mut p = Prefetch::spawn(vec![(0, open)], SMALL).unwrap();
        let mut f = p.next_file().unwrap();
        let e = loop {
            match f.next_segment() {
                Ok(Some((b, _))) => f.give_back(b),
                Ok(None) => panic!("no panic"),
                Err(e) => break e,
            }
        };
        assert!(matches!(e, SegmentError::Panicked(_)));
        assert!(p.handle.is_none(), "the reader is joined");
        // A panic while opening is re-raised by `next_file`.
        let open: OpenFn<String> = Box::new(|| panic!("open panics"));
        let mut p = Prefetch::spawn(vec![(0, open)], SMALL).unwrap();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = p.next_file();
        }));
        assert!(r.is_err());
        assert!(p.handle.is_none());
    }

    #[test]
    fn dropping_with_a_full_queue_joins_the_waiting_reader() {
        // Never consumed: the reader fills both buffers and the queue, then waits.
        let (open, dropped) = faulty(1 << 20, false);
        let p = Prefetch::spawn(vec![(0, open)], SMALL).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(p);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        // Dropped half-way through a file, with a segment held by the feeding side.
        let (open, dropped) = faulty(1 << 20, false);
        let mut p = Prefetch::spawn(vec![(0, open)], SMALL).unwrap();
        let mut f = p.next_file().unwrap();
        let held = f.next_segment().unwrap();
        drop(p);
        drop(held);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn poisoned_buffers_given_back_never_reach_a_later_segment() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 200) as u8).collect();
        let mut p = Prefetch::spawn(vec![(0, opener(data.clone()))], SMALL).unwrap();
        p.poison(0xA5);
        let mut f = p.next_file().unwrap();
        assert_eq!(collect(&mut f).unwrap(), data);
    }
}
