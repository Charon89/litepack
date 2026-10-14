//! Parallel block encoding (`WriterOptions::encode_threads`, E2-4b): the archive written with
//! blocks in flight is byte for byte the one a single thread writes; at most N blocks are in
//! flight; a job's error or panic abandons the archive; a pool block may not name a record.
#![allow(clippy::unwrap_used)]

mod common;

use lpk_format::{
    Archive, BlockEncoder, EncodeJob, Encoded, EntryFlags, FormatError, Graph, JobState,
    JpegRecord, PrimitiveId, Record, RecordBody, Resources, Step, StoreEncoder, Writer,
    WriterOptions,
};
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const CHUNK: u64 = 4096;
const BLOCK: u64 = 16384;

fn options(threads: usize, encoder: Box<dyn BlockEncoder>, dedup: bool) -> WriterOptions {
    WriterOptions {
        chunk_size: CHUNK,
        block_size: BLOCK,
        archive_id: [4; 16],
        encoder,
        dedup,
        encode_threads: threads,
        ..WriterOptions::default()
    }
}

/// An encoder with per-block state: block `n` (in hand-over order) is zstd-coded when `n` is
/// even and stored when odd; the job captures `n` and gives back the encoded length, which the
/// encoder sums in `absorbed`.
struct Alternating {
    next: u64,
    absorbed: Arc<AtomicUsize>,
    jobs: bool,
}

struct AlternatingJob {
    n: u64,
}

fn alternate(n: u64, plain: &[u8]) -> Encoded {
    if n.is_multiple_of(2) {
        let mut z = common::ZstdTestEncoder::new(3, 20, None);
        z.encode(plain).unwrap()
    } else {
        StoreEncoder.encode(plain).unwrap()
    }
}

impl EncodeJob for AlternatingJob {
    fn run(self: Box<Self>, plain: &[u8]) -> Result<(Encoded, Option<JobState>), FormatError> {
        let e = alternate(self.n, plain);
        let len = e.bytes.len();
        Ok((e, Some(Box::new(len))))
    }
}

impl BlockEncoder for Alternating {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let e = alternate(self.next, plain);
        self.next += 1;
        self.absorbed.fetch_add(e.bytes.len(), Ordering::SeqCst);
        Ok(e)
    }

    fn job(&mut self) -> Option<Box<dyn EncodeJob>> {
        if !self.jobs {
            return None;
        }
        let n = self.next;
        self.next += 1;
        Some(Box::new(AlternatingJob { n }))
    }

    fn absorb(&mut self, state: JobState) {
        let len = *state.downcast::<usize>().unwrap();
        self.absorbed.fetch_add(len, Ordering::SeqCst);
    }
}

fn file(w: &mut Writer<&mut Vec<u8>>, name: &str, seed: u64, len: usize) {
    let d = common::pattern(seed, len);
    w.add_file(name, EntryFlags::EMPTY, 1, &mut &d[..]).unwrap();
}

/// A mixed archive: files of several blocks, a duplicate (dedup), given blocks interleaved
/// (one plain, one naming a record), a forced close.
fn mixed(threads: usize, encoder: Box<dyn BlockEncoder>) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(threads, encoder, true), 1).unwrap();
    file(&mut w, "a", 1, 3 * BLOCK as usize + 100);
    file(&mut w, "b", 2, BLOCK as usize);
    w.close_block().unwrap();
    // A given block of its own, a plain graph.
    let part = common::pattern(3, 2 * CHUNK as usize);
    w.begin_entry("c", EntryFlags::EMPTY, 1).unwrap();
    w.add_part_encoded(0, &part, StoreEncoder.encode(&part).unwrap(), 0)
        .unwrap();
    w.end_entry().unwrap();
    file(&mut w, "d", 4, 5 * CHUNK as usize);
    // The same bytes again: deduplicated.
    file(&mut w, "a-again", 1, 3 * BLOCK as usize + 100);
    // A given block naming a record (a stand-in reconstruction graph).
    let p = common::pattern(5, 1000);
    w.begin_entry("e", EntryFlags::EMPTY, 1).unwrap();
    let id = w.add_record(record(1000, 1000, Vec::new())).unwrap();
    w.add_part_encoded(0, &p, recon_encoded(id as u8, &p), 0)
        .unwrap();
    w.add_part(1000, &mut &common::pattern(6, 7000)[..])
        .unwrap();
    w.end_entry().unwrap();
    file(&mut w, "f", 7, 2 * BLOCK as usize + 10);
    let s = w.finish().unwrap();
    assert!(s.deduped_chunks > 0);
    (out, s.blocks as usize)
}

fn record(original_len: u64, primary_len: u64, chunks: Vec<u64>) -> Record {
    Record::new(RecordBody::Jpeg(JpegRecord {
        original_len,
        primary_len,
        trailing: Vec::new(),
        nested_trailing_chunks: chunks,
        gainmaps: Vec::new(),
        lepton_version: 0,
        original_hash: [0; 32],
    }))
}

fn recon_encoded(record: u8, primary: &[u8]) -> Encoded {
    Encoded {
        graph: Graph {
            steps: vec![Step {
                primitive: PrimitiveId::JpegReconstruct,
                params: vec![record],
            }],
        },
        bytes: primary.iter().rev().copied().collect(),
        resources: Default::default(),
    }
}

#[test]
fn the_store_encoder_writes_the_same_bytes_with_one_thread_and_four() {
    let (one, blocks) = mixed(1, Box::new(StoreEncoder));
    let (four, _) = mixed(4, Box::new(StoreEncoder));
    assert!(blocks >= 6, "{blocks} blocks");
    assert_eq!(one, four);
    // The reader has no reconstruction decoder here: the structure is checked, not decoded.
    let a = Archive::open(Cursor::new(four), &Resources::default()).unwrap();
    assert_eq!(a.index().blocks.len(), blocks);
}

#[test]
fn a_stateful_encoder_writes_the_same_bytes_and_absorbs_the_same_counts() {
    let make = |jobs: bool| {
        let absorbed = Arc::new(AtomicUsize::new(0));
        let e = Alternating {
            next: 0,
            absorbed: Arc::clone(&absorbed),
            jobs,
        };
        (Box::new(e) as Box<dyn BlockEncoder>, absorbed)
    };
    let (e1, a1) = make(true);
    let (one, _) = mixed(1, e1);
    let (e4, a4) = make(true);
    let (four, _) = mixed(4, e4);
    assert_eq!(one, four);
    assert_eq!(a1.load(Ordering::SeqCst), a4.load(Ordering::SeqCst));
    assert!(a1.load(Ordering::SeqCst) > 0);
    // An encoder without the extension runs sequentially under four threads.
    let (e0, a0) = make(false);
    let (seq, _) = mixed(4, e0);
    assert_eq!(one, seq);
    assert_eq!(a0.load(Ordering::SeqCst), a1.load(Ordering::SeqCst));
}

#[test]
fn zero_threads_count_as_one() {
    let (one, _) = mixed(1, Box::new(StoreEncoder));
    let (zero, _) = mixed(0, Box::new(StoreEncoder));
    assert_eq!(one, zero);
}

/// A gate: jobs wait on it until released.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
    started: AtomicUsize,
}

impl Gate {
    fn wait(&self) {
        self.started.fetch_add(1, Ordering::SeqCst);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.cv.wait(open).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

struct Blocking(Arc<Gate>);

impl EncodeJob for Blocking {
    fn run(self: Box<Self>, plain: &[u8]) -> Result<(Encoded, Option<JobState>), FormatError> {
        self.0.wait();
        Ok((StoreEncoder.encode(plain)?, None))
    }
}

impl BlockEncoder for Blocking {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        StoreEncoder.encode(plain)
    }

    fn job(&mut self) -> Option<Box<dyn EncodeJob>> {
        Some(Box::new(Blocking(Arc::clone(&self.0))))
    }
}

fn settle(check: impl Fn() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the condition did not come about");
}

#[test]
fn at_most_n_blocks_are_in_flight_and_finish_drains_them_in_order() {
    const N: usize = 2;
    let gate = Arc::new(Gate::default());
    let accepted = Arc::new(AtomicUsize::new(0));
    let (g, a) = (Arc::clone(&gate), Arc::clone(&accepted));
    let writer = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut w = Writer::new(&mut out, options(N, Box::new(Blocking(g)), false)).unwrap();
        for i in 0..5u64 {
            let d = common::pattern(i + 1, BLOCK as usize);
            w.add_file(&format!("f{i}"), EntryFlags::EMPTY, 1, &mut &d[..])
                .unwrap();
            w.close_block().unwrap();
            a.fetch_add(1, Ordering::SeqCst);
        }
        w.finish().unwrap();
        out
    });
    // N blocks close without waiting: N jobs have started, N files were accepted.
    settle(|| gate.started.load(Ordering::SeqCst) == N && accepted.load(Ordering::SeqCst) == N);
    // The (N+1)th close awaits the oldest job, which is held: nothing more is accepted.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(accepted.load(Ordering::SeqCst), N);
    assert_eq!(gate.started.load(Ordering::SeqCst), N);
    gate.release();
    let parallel = writer.join().unwrap();
    assert_eq!(accepted.load(Ordering::SeqCst), 5);
    // Drained in order: the same bytes as one thread writes.
    let mut out = Vec::new();
    let mut w = Writer::new(
        &mut out,
        options(1, Box::new(Blocking(Arc::clone(&gate))), false),
    )
    .unwrap();
    for i in 0..5u64 {
        let d = common::pattern(i + 1, BLOCK as usize);
        w.add_file(&format!("f{i}"), EntryFlags::EMPTY, 1, &mut &d[..])
            .unwrap();
        w.close_block().unwrap();
    }
    w.finish().unwrap();
    assert_eq!(parallel, out);
    let mut a = Archive::open(Cursor::new(parallel), &Resources::default()).unwrap();
    a.verify().unwrap();
    let blocks = &a.index().blocks;
    assert_eq!(blocks.len(), 5);
    assert!(blocks.windows(2).all(|p| p[0].sequence < p[1].sequence
        && p[0].first_chunk < p[1].first_chunk
        && p[0].frame_offset < p[1].frame_offset));
}

/// Fails (or panics) on the block whose hand-over index is `at`.
struct Failing {
    next: u64,
    at: u64,
    panic: bool,
}

struct FailingJob {
    fail: bool,
    panic: bool,
}

impl EncodeJob for FailingJob {
    fn run(self: Box<Self>, plain: &[u8]) -> Result<(Encoded, Option<JobState>), FormatError> {
        if self.fail {
            if self.panic {
                panic!("the test job panics");
            }
            return Err(FormatError::BadGraph {
                reason: "the test job fails",
            });
        }
        Ok((StoreEncoder.encode(plain)?, None))
    }
}

impl BlockEncoder for Failing {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        StoreEncoder.encode(plain)
    }

    fn job(&mut self) -> Option<Box<dyn EncodeJob>> {
        let fail = self.next == self.at;
        self.next += 1;
        Some(Box::new(FailingJob {
            fail,
            panic: self.panic,
        }))
    }
}

#[test]
fn a_failing_job_abandons_the_archive_with_the_errors_own_class() {
    let mut out = Vec::new();
    let enc = Failing {
        next: 0,
        at: 1,
        panic: false,
    };
    let mut w = Writer::new(&mut out, options(2, Box::new(enc), false)).unwrap();
    let d = common::pattern(1, BLOCK as usize);
    let mut seen = None;
    for i in 0..6u64 {
        let r = w
            .add_file(&format!("f{i}"), EntryFlags::EMPTY, 1, &mut &d[..])
            .and_then(|()| w.close_block());
        if let Err(e) = r {
            seen = Some(e);
            break;
        }
    }
    let seen = seen.unwrap_or_else(|| w.close_block().unwrap_err());
    assert!(
        matches!(
            seen,
            FormatError::BadGraph {
                reason: "the test job fails"
            }
        ),
        "{seen:?}"
    );
    // Every later call reports the archive abandoned.
    match w.add_file("later", EntryFlags::EMPTY, 1, &mut &d[..]) {
        Err(FormatError::Io(e)) => {
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
            assert!(e.to_string().contains("the test job fails"));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(w.finish(), Err(FormatError::Io(_))));
}

#[test]
fn a_panicking_job_is_re_raised_after_the_writer_is_marked_failed() {
    let mut out = Vec::new();
    let enc = Failing {
        next: 0,
        at: 0,
        panic: true,
    };
    let mut w = Writer::new(&mut out, options(2, Box::new(enc), false)).unwrap();
    let d = common::pattern(1, BLOCK as usize);
    let r = catch_unwind(AssertUnwindSafe(|| {
        for i in 0..6u64 {
            w.add_file(&format!("f{i}"), EntryFlags::EMPTY, 1, &mut &d[..])
                .unwrap();
            w.close_block().unwrap();
        }
        w.close_block().unwrap();
    }));
    let payload = r.unwrap_err();
    assert_eq!(
        payload.downcast_ref::<&str>().copied(),
        Some("the test job panics")
    );
    match w.add_file("later", EntryFlags::EMPTY, 1, &mut &d[..]) {
        Err(FormatError::Io(e)) => {
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
            assert!(e.to_string().contains("panicked"));
        }
        other => panic!("{other:?}"),
    }
}

/// An encoder whose blocks name record 0 through a job.
struct NamesRecord;

impl EncodeJob for NamesRecord {
    fn run(self: Box<Self>, plain: &[u8]) -> Result<(Encoded, Option<JobState>), FormatError> {
        Ok((recon_encoded(0, plain), None))
    }
}

impl BlockEncoder for NamesRecord {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        Ok(recon_encoded(0, plain))
    }

    fn job(&mut self) -> Option<Box<dyn EncodeJob>> {
        Some(Box::new(NamesRecord))
    }
}

#[test]
fn a_pool_block_naming_a_record_is_refused() {
    let d = common::pattern(1, 1000);
    // Sequentially such a block is accepted (the encoder may name a record).
    let mut out = Vec::new();
    let mut o = options(1, Box::new(NamesRecord), false);
    o.records = vec![record(1000, 1000, Vec::new())];
    let mut w = Writer::new_revision(&mut out, o, 1).unwrap();
    w.add_file("a", EntryFlags::EMPTY, 1, &mut &d[..]).unwrap();
    w.finish().unwrap();
    // Through the pool it is refused when the encoding completes.
    let mut out = Vec::new();
    let mut o = options(2, Box::new(NamesRecord), false);
    o.records = vec![record(1000, 1000, Vec::new())];
    let mut w = Writer::new_revision(&mut out, o, 1).unwrap();
    w.add_file("a", EntryFlags::EMPTY, 1, &mut &d[..]).unwrap();
    let r = w.close_block().and_then(|()| w.finish());
    assert!(
        matches!(
            r,
            Err(FormatError::BadRecord {
                record: 0,
                reason: "pool-encoded block names a record"
            })
        ),
        "{r:?}"
    );
}

#[test]
fn a_given_reconstruction_block_closing_behind_pool_blocks_keeps_the_earlier_block_rule() {
    let gate = Arc::new(Gate::default());
    // Two pool blocks are held in flight; the record of the given block names a chunk of the
    // first (earlier, plain: accepted) and then a chunk of the given block itself (refused).
    // The record's nested chunk is chunk 0 of file `a` (one `CHUNK` long) or chunk 8.
    for (chunks, ok) in [(vec![0u64], true), (vec![8u64], false)] {
        let mut out = Vec::new();
        let mut w = Writer::new_revision(
            &mut out,
            options(3, Box::new(Blocking(Arc::clone(&gate))), true),
            1,
        )
        .unwrap();
        let d = common::pattern(1, BLOCK as usize);
        w.add_file("a", EntryFlags::EMPTY, 1, &mut &d[..]).unwrap();
        w.close_block().unwrap();
        let d = common::pattern(2, BLOCK as usize);
        w.add_file("b", EntryFlags::EMPTY, 1, &mut &d[..]).unwrap();
        w.close_block().unwrap();
        let p = common::pattern(3, 1000);
        w.begin_entry("c", EntryFlags::EMPTY, 1).unwrap();
        let id = w.add_record(record(1000 + CHUNK, 1000, chunks)).unwrap();
        let r = w.add_part_encoded(0, &p, recon_encoded(id as u8, &p), 0);
        if ok {
            r.unwrap();
            // A copy of the given block's bytes stored afterwards is not deduplicated against
            // the reconstruction block for a record's part (the block is classified already).
            let part = w.add_part(1000, &mut &p[..]).unwrap();
            assert_eq!(part, vec![9]);
            w.end_entry().unwrap();
            gate.release();
            let s = w.finish().unwrap();
            assert_eq!(s.blocks, 4);
            let a = Archive::open(Cursor::new(out), &Resources::default()).unwrap();
            assert_eq!(a.index().blocks.len(), 4);
        } else {
            assert!(
                matches!(
                    r,
                    Err(FormatError::BadRecord {
                        reason: "chunk order",
                        ..
                    })
                ),
                "{r:?}"
            );
            gate.release();
        }
    }
}

#[test]
fn sealed_and_recovery_archives_are_identical_across_threads() {
    use lpk_format::{Argon2Params, Credentials, RecoveryOptions, SealOptions, Suite};
    use rand::SeedableRng;
    let build = |threads: usize| {
        let mut out = Vec::new();
        let o = WriterOptions {
            seal: Some(SealOptions {
                suite: Suite::AesGcm,
                argon2: Argon2Params::default(),
                listable: false,
                credentials: Credentials::password("pw"),
            }),
            recovery: RecoveryOptions {
                percent: 10,
                ..RecoveryOptions::default()
            },
            ..options(threads, Box::new(StoreEncoder), true)
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let mut w = Writer::new_with_rng(&mut out, o, &mut rng).unwrap();
        for i in 0..4u64 {
            let d = common::pattern(i + 1, 3 * BLOCK as usize);
            w.add_file(&format!("f{i}"), EntryFlags::EMPTY, 1, &mut &d[..])
                .unwrap();
        }
        w.finish().unwrap();
        out
    };
    assert_eq!(build(1), build(3));
}
