//! The pipelined store path (E2-18 round 2): a reader thread ahead of the cut
//! ([`crate::prefetch`]), a write thread behind it ([`Writer::with_write_thread`])
//! and the block buffer pool — byte identity with the sequential path, buffer
//! recycling, and errors, panics and drops with every thread joined.

use super::*;
use crate::crypto::{Argon2Params, Credentials, Suite};
use crate::envelope::Resources;
use crate::prefetch::{OpenFn, Prefetch, PrefetchOptions};
use crate::recovery::RecoveryOptions;
use rand::SeedableRng;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len + 8);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// A mixed tree: empty, small, compressible, incompressible, a segment's exact
/// multiple, several blocks long, and duplicates (whole, partial, and new bytes
/// around a duplicate run, so a segment's leading chunks are new and a later
/// one is already stored).
fn tree() -> Vec<(String, Vec<u8>)> {
    let big = noise(1, 5 << 20);
    let mut mixed = noise(5, (1 << 20) + 4321);
    mixed.extend_from_slice(&big[(1 << 20)..(3 << 20)]);
    mixed.extend_from_slice(&noise(6, 1 << 20));
    vec![
        ("empty".into(), Vec::new()),
        ("tiny.txt".into(), b"hello".to_vec()),
        ("text.txt".into(), b"the quick brown fox ".repeat(40_000)),
        ("big.bin".into(), big.clone()),
        ("exact.bin".into(), noise(2, 3 * SEGMENT)),
        ("odd.bin".into(), noise(3, 777_777)),
        ("dup.bin".into(), big.clone()),
        ("part.bin".into(), big[..(2 << 20) + 12_345].to_vec()),
        ("mixed.bin".into(), mixed),
        ("last.bin".into(), noise(4, 1_234_567)),
    ]
}

/// Segment size of the read-ahead in these tests: not a multiple of the chunk
/// size, so chunks straddle segments.
const SEGMENT: usize = 100_003;

/// An output shared with the test, written by whichever thread owns it.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Case {
    threads: usize,
    sealed: bool,
    recovery: u8,
    poison: bool,
    /// The read-ahead's segment size.
    segment: usize,
}

fn options(c: Case) -> WriterOptions {
    WriterOptions {
        chunk_size: 64 << 10,
        block_size: 1 << 20,
        dedup: true,
        encode_threads: c.threads,
        recovery: RecoveryOptions {
            percent: c.recovery,
            ..RecoveryOptions::default()
        },
        seal: c.sealed.then(|| SealOptions {
            suite: Suite::AesGcm,
            argon2: Argon2Params {
                t: 1,
                m_kib: 8192,
                p: 1,
            },
            listable: false,
            credentials: Credentials::password("pw"),
        }),
        ..WriterOptions::default()
    }
}

/// A given (peeled) block, added between files.
fn add_given<W: Write>(w: &mut Writer<W>) {
    let given = noise(9, 300_000);
    w.begin_entry("given.bin", EntryFlags::default(), 0)
        .unwrap();
    w.add_part_encoded(0, &given, StoreEncoder::wrap(given.clone()), 0)
        .unwrap();
    w.end_entry().unwrap();
}

/// The sequential path: `add_file` over readers, written on this thread.
fn sequential(c: Case) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let mut w = Writer::new_with_rng(&mut out, options(c), &mut rng).unwrap();
    for (i, (path, data)) in tree().iter().enumerate() {
        if i == 4 {
            add_given(&mut w);
        }
        w.add_file(path, EntryFlags::default(), 0, &mut &data[..])
            .unwrap();
    }
    w.finish().unwrap();
    out
}

fn openers(files: &[(String, Vec<u8>)]) -> Vec<(usize, OpenFn<String>)> {
    files
        .iter()
        .enumerate()
        .map(|(i, (_, d))| {
            let d = d.clone();
            let open: OpenFn<String> =
                Box::new(move || Ok(Box::new(Cursor::new(d)) as Box<dyn Read + Send>));
            (i, open)
        })
        .collect()
}

/// The pipelined path: a reader thread, `add_file_segments`, a write thread.
fn pipelined(c: Case) -> Vec<u8> {
    let out = Shared::default();
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let files = tree();
    let mut reads = Prefetch::spawn(
        openers(&files),
        PrefetchOptions {
            segment: c.segment,
            buffers: 3,
        },
    )
    .unwrap();
    if c.poison {
        reads.poison(POISON);
    }
    let mut w = Writer::new_with_rng(out.clone(), options(c), &mut rng)
        .unwrap()
        .with_write_thread();
    assert!(matches!(w.out.inner, Sink::Thread(_)));
    w.poison_pool = c.poison;
    for (i, (path, data)) in files.iter().enumerate() {
        if i == 4 {
            add_given(&mut w);
        }
        let mut f = reads.next_file().unwrap();
        assert_eq!(f.id(), i);
        w.add_file_segments(path, EntryFlags::default(), 0, &mut f, data.len() as u64)
            .unwrap();
        assert_eq!(f.bytes_read(), data.len() as u64);
    }
    w.finish().unwrap();
    let bytes = out.0.lock().unwrap().clone();
    bytes
}

fn case(threads: usize) -> Case {
    Case {
        threads,
        sealed: false,
        recovery: 0,
        poison: false,
        segment: SEGMENT,
    }
}

#[test]
fn the_pipelined_path_writes_the_sequential_paths_bytes() {
    for threads in [1usize, 4] {
        let want = sequential(case(threads));
        assert!(want == pipelined(case(threads)), "threads {threads}");
        let mut a = Archive::open(Cursor::new(want), &Resources::default()).unwrap();
        a.verify().unwrap();
    }
    // Threads 1 and N agree with each other too.
    assert!(pipelined(case(1)) == pipelined(case(4)));
    for threads in [1usize, 4] {
        let c = Case {
            recovery: 10,
            ..case(threads)
        };
        assert!(sequential(c) == pipelined(c), "recovery, threads {threads}");
        let c = Case { sealed: true, ..c };
        assert!(sequential(c) == pipelined(c), "sealed, threads {threads}");
        let c = Case { recovery: 0, ..c };
        assert!(
            sequential(c) == pipelined(c),
            "sealed alone, threads {threads}"
        );
    }
}

/// Segments large enough for the chunk hashes to fan out to the hashing
/// threads (E2-18 round 3): the bytes are still the sequential path's, at 1
/// thread (no fan-out) and at 2, 4 and 24 blocks in flight, with dedup, a
/// given block, recovery and sealing.
#[test]
fn hashing_chunks_on_several_threads_keeps_the_sequential_bytes() {
    for segment in [(3 << 20) + 7, 8 << 20] {
        let base = Case { segment, ..case(1) };
        let want = sequential(base);
        assert!(want == pipelined(base), "segment {segment} threads 1");
        for threads in [2usize, 4, 24] {
            let c = Case { threads, ..base };
            let got = pipelined(c);
            assert!(sequential(c) == got, "segment {segment} threads {threads}");
            assert!(want == got, "segment {segment} threads {threads} vs 1");
            if threads == 4 {
                let c = Case {
                    recovery: 10,
                    sealed: true,
                    ..c
                };
                assert!(sequential(c) == pipelined(c), "sealed+recovery {segment}");
            }
        }
    }
}

/// Every buffer returned to the block pool or to the reader is filled with a
/// pattern first: a buffer handed out while still referenced would put the
/// pattern into a frame. Ownership already rules that out (buffers move by
/// value to jobs, the write thread and the reader, and come back through
/// channels); what this can catch is a logic error reading stale bytes of a
/// cleared buffer. The bound on buffers is tested apart
/// (`files_ending_on_a_segment_boundary_keep_the_read_buffers_bounded`).
#[test]
fn recycled_buffers_are_never_reused_while_referenced() {
    for threads in [1usize, 4] {
        let c = Case {
            poison: true,
            ..case(threads)
        };
        let got = pipelined(c);
        assert!(got == sequential(case(threads)), "threads {threads}");
        let run = [POISON; 64];
        assert!(
            !got.windows(run.len()).any(|w| w == run),
            "a written frame holds the pool's pattern"
        );
    }
}

/// An output that fails, or panics, once `limit` bytes were written; `dropped`
/// is raised when the output is dropped (with a write thread: when that thread
/// ends). `delay` slows every write, so the queue fills.
struct Faulty {
    written: usize,
    limit: usize,
    panic: bool,
    delay: std::time::Duration,
    dropped: Arc<AtomicBool>,
}

impl Write for Faulty {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::thread::sleep(self.delay);
        if self.written + buf.len() > self.limit {
            if self.panic {
                panic!("the output panics");
            }
            return Err(std::io::Error::other("disk full"));
        }
        self.written += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Faulty {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

fn faulty(limit: usize, panic: bool, delay_ms: u64) -> (Faulty, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    (
        Faulty {
            written: 0,
            limit,
            panic,
            delay: std::time::Duration::from_millis(delay_ms),
            dropped: dropped.clone(),
        },
        dropped,
    )
}

fn small_writer<W: Write + Send + 'static>(out: W, threads: usize) -> Writer<W> {
    Writer::new(out, options(case(threads)))
        .unwrap()
        .with_write_thread()
}

#[test]
fn a_write_error_mid_archive_fails_a_later_call_and_abandons_the_archive() {
    for threads in [1usize, 4] {
        let (out, dropped) = faulty(3 << 20, false, 0);
        let mut w = small_writer(out, threads);
        let mut first = None;
        for i in 0..20 {
            let data = noise(100 + i, 1 << 20);
            if let Err(e) = w.add_file(&format!("f{i}"), EntryFlags::default(), 0, &mut &data[..]) {
                first = Some(e);
                break;
            }
        }
        let e = first.or_else(|| w.close_block().err()).unwrap_or_else(|| {
            // Surfaced at the latest when everything is flushed.
            w.flush_block()
                .and_then(|()| w.drain())
                .and_then(|()| w.out.flush().map_err(FormatError::Io))
                .unwrap_err()
        });
        assert!(
            matches!(&e, FormatError::Io(e) if e.to_string() == "disk full"),
            "{e:?}"
        );
        // Sticky: every later call fails with it.
        let again = w
            .add_file("late", EntryFlags::default(), 0, &mut &b"x"[..])
            .unwrap_err();
        assert!(matches!(again, FormatError::Io(_)), "{again:?}");
        assert!(w.finish().is_err());
        assert!(dropped.load(Ordering::SeqCst), "the write thread is joined");
    }
}

#[test]
fn a_write_thread_panic_is_re_raised_with_the_writer_failed() {
    let (out, dropped) = faulty(2 << 20, true, 0);
    let mut w = small_writer(out, 1);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for i in 0..20 {
            let data = noise(200 + i, 1 << 20);
            let _ = w.add_file(&format!("f{i}"), EntryFlags::default(), 0, &mut &data[..]);
        }
        let _ = w.close_block();
        let _ = w.flush_block().and_then(|()| w.drain());
        let _ = w.out.flush();
    }));
    assert!(r.is_err(), "the panic is re-raised");
    assert!(w
        .add_file("late", EntryFlags::default(), 0, &mut &b"x"[..])
        .is_err());
    assert!(dropped.load(Ordering::SeqCst), "the write thread is joined");
    drop(w);
}

/// A reader that yields `ok` bytes, then fails or panics; raises `dropped`
/// when the reader thread lets go of it.
struct FaultyRead {
    at: usize,
    ok: usize,
    panic: bool,
    dropped: Arc<AtomicBool>,
}

impl Read for FaultyRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at >= self.ok {
            if self.panic {
                panic!("the reader panics");
            }
            return Err(std::io::Error::other("read failed"));
        }
        let n = buf.len().min(self.ok - self.at);
        buf[..n].fill(3);
        self.at += n;
        Ok(n)
    }
}

impl Drop for FaultyRead {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

fn faulty_reads(ok: usize, panic: bool) -> (Prefetch<String>, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    let d = dropped.clone();
    let open: OpenFn<String> = Box::new(move || {
        Ok(Box::new(FaultyRead {
            at: 0,
            ok,
            panic,
            dropped: d,
        }) as Box<dyn Read + Send>)
    });
    let p = Prefetch::spawn(
        vec![(0, open)],
        PrefetchOptions {
            segment: SEGMENT,
            buffers: 2,
        },
    )
    .unwrap();
    (p, dropped)
}

#[test]
fn a_read_error_mid_file_is_reported_for_that_file_and_abandons_the_archive() {
    let (mut reads, read_dropped) = faulty_reads(2 << 20, false);
    let (out, out_dropped) = faulty(usize::MAX, false, 0);
    let mut w = small_writer(out, 4);
    let mut f = reads.next_file().unwrap();
    let e = w
        .add_file_segments("f", EntryFlags::default(), 0, &mut f, 4 << 20)
        .unwrap_err();
    assert!(
        matches!(&e, FormatError::Io(e) if e.to_string() == "read failed"),
        "{e:?}"
    );
    assert!(w
        .add_file("late", EntryFlags::default(), 0, &mut &b"x"[..])
        .is_err());
    drop(reads);
    drop(w);
    assert!(read_dropped.load(Ordering::SeqCst), "the reader is joined");
    assert!(
        out_dropped.load(Ordering::SeqCst),
        "the write thread is joined"
    );
}

#[test]
fn a_reader_panic_is_re_raised_with_the_writer_failed() {
    let (mut reads, read_dropped) = faulty_reads(1 << 20, true);
    let (out, out_dropped) = faulty(usize::MAX, false, 0);
    let mut w = small_writer(out, 1);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut f = reads.next_file().unwrap();
        let _ = w.add_file_segments("f", EntryFlags::default(), 0, &mut f, 0);
    }));
    assert!(r.is_err(), "the panic is re-raised");
    assert!(w.failed.is_some(), "the writer is marked failed first");
    assert!(w
        .add_file("late", EntryFlags::default(), 0, &mut &b"x"[..])
        .is_err());
    drop(reads);
    drop(w);
    assert!(read_dropped.load(Ordering::SeqCst));
    assert!(out_dropped.load(Ordering::SeqCst));
}

/// A hashing thread's panic (E2-18 round 3): every hashing thread is joined
/// inside the call, the writer is marked failed and the panic re-raised; the
/// reader and the write thread are joined on drop as for any other panic.
#[test]
fn a_hashing_thread_panic_is_re_raised_with_the_writer_failed() {
    let (out, out_dropped) = faulty(usize::MAX, false, 0);
    let mut w = small_writer(out, 4);
    let data = noise(77, 6 << 20);
    let files = vec![("f".to_string(), data.clone())];
    let mut reads = Prefetch::spawn(
        openers(&files),
        PrefetchOptions {
            segment: 8 << 20,
            buffers: 2,
        },
    )
    .unwrap();
    crate::chunkhash::HASH_FAULT.with(|f| f.set(true));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut f = reads.next_file().unwrap();
        let _ = w.add_file_segments("f", EntryFlags::default(), 0, &mut f, data.len() as u64);
    }));
    crate::chunkhash::HASH_FAULT.with(|f| f.set(false));
    let p = r.err().map(|p| p.downcast_ref::<&str>().copied());
    assert_eq!(p, Some(Some("a hashing thread fails")), "re-raised");
    assert!(w.failed.is_some(), "the writer is marked failed first");
    assert!(w
        .add_file("late", EntryFlags::default(), 0, &mut &b"x"[..])
        .is_err());
    drop(reads);
    drop(w);
    assert!(out_dropped.load(Ordering::SeqCst));
}

/// The writer and the reader dropped mid-archive with both queues full: one
/// encode thread (each block written as it closes) behind an output that
/// takes a while per write, so the write queue holds `WRITE_DEPTH` frames plus
/// the one being written; the read queue full of unread segments. Both drop
/// orders; both threads end and are joined; nothing deadlocks.
fn drop_with_full_queues(writer_first: bool) {
    let (mut reads, read_dropped) = faulty_reads(64 << 20, false);
    let (out, out_dropped) = faulty(usize::MAX, false, 50);
    let mut w = small_writer(out, 1);
    let mut i = 0u64;
    while w.out.inner.queued() < WRITE_DEPTH + 1 {
        let data = noise(300 + i, 1 << 20);
        w.add_file(&format!("f{i}"), EntryFlags::default(), 0, &mut &data[..])
            .unwrap();
        i += 1;
        assert!(i < 50, "the write queue never filled");
    }
    assert_eq!(w.out.inner.queued(), WRITE_DEPTH + 1);
    // One segment taken, the rest of the read queue left full.
    let mut f = reads.next_file().unwrap();
    let seg = crate::prefetch::Segments::next_segment(&mut f).unwrap();
    assert!(seg.is_some());
    let t = std::time::Instant::now();
    if writer_first {
        drop(w);
        drop(reads);
    } else {
        drop(reads);
        drop(w);
    }
    assert!(read_dropped.load(Ordering::SeqCst));
    assert!(out_dropped.load(Ordering::SeqCst));
    assert!(t.elapsed() < std::time::Duration::from_secs(30));
}

#[test]
fn dropping_mid_archive_with_both_queues_full_joins_every_thread() {
    drop_with_full_queues(true);
    drop_with_full_queues(false);
}

/// Many files that end on a segment boundary (empty, exactly one segment, two)
/// between longer ones, through the writer: the reader never allocates more
/// than its buffers, and the archive is the sequential path's.
#[test]
fn files_ending_on_a_segment_boundary_keep_the_read_buffers_bounded() {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..300u64 {
        let data = match i % 5 {
            0 | 1 => Vec::new(),
            2 => noise(i, SEGMENT),
            3 => noise(i, 2 * SEGMENT),
            _ => noise(i, 250_000),
        };
        files.push((format!("f{i:03}"), data));
    }
    let opts = PrefetchOptions {
        segment: SEGMENT,
        buffers: 3,
    };
    let mut reads = Prefetch::spawn(openers(&files), opts).unwrap();
    let out = Shared::default();
    let mut w = Writer::new(out.clone(), options(case(1)))
        .unwrap()
        .with_write_thread();
    for (path, data) in &files {
        let mut f = reads.next_file().unwrap();
        w.add_file_segments(path, EntryFlags::default(), 0, &mut f, data.len() as u64)
            .unwrap();
    }
    w.finish().unwrap();
    assert!(
        reads.buffers_allocated() <= opts.buffers,
        "{}",
        reads.buffers_allocated()
    );
    let mut want = Vec::new();
    let mut w = Writer::new(&mut want, options(case(1))).unwrap();
    for (path, data) in &files {
        w.add_file(path, EntryFlags::default(), 0, &mut &data[..])
            .unwrap();
    }
    w.finish().unwrap();
    assert!(want == *out.0.lock().unwrap());
}

/// Threads the pipelined writer and reader start are all gone after a run:
/// every output and reader they owned is dropped (they are dropped on their
/// thread only when it ends).
#[test]
fn a_finished_run_leaves_no_thread() {
    static OPEN: AtomicUsize = AtomicUsize::new(0);
    struct Count(Shared);
    impl Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.write(b)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Drop for Count {
        fn drop(&mut self) {
            OPEN.fetch_sub(1, Ordering::SeqCst);
        }
    }
    OPEN.fetch_add(1, Ordering::SeqCst);
    let mut w = small_writer(Count(Shared::default()), 4);
    w.add_file("a", EntryFlags::default(), 0, &mut &noise(8, 2 << 20)[..])
        .unwrap();
    w.finish().unwrap();
    assert_eq!(OPEN.load(Ordering::SeqCst), 0);
}
