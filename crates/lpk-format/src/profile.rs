//! Where the writer's time goes, stage by stage: a diagnostic switched on by
//! the environment variable `LPK_PROFILE_STAGES` (any value). When set, the
//! writer's threads add the wall time of each stage to process-wide counters
//! and [`crate::Writer::finish`] prints the table to stderr. When it is not
//! set every site tests one cached bool and nothing else is done.
//!
//! The counters are process-wide (several writers in one process add to the
//! same counters); [`reset`] clears them when a writer starts. The table is
//! an engineering view, not a measurement: its figures never go into a report
//! that a harness run did not produce.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// The stages counted, in the order the table prints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Stage {
    /// The reader thread inside its `read` calls.
    ReaderRead,
    /// The reader thread waiting for a free buffer or for queue room.
    ReaderBlocked,
    /// The feeding thread waiting for the next segment.
    FeedWait,
    /// The chunker's cut.
    Cut,
    /// The chunks' BLAKE3 (the feeding thread's wall time, hash threads included).
    Hash,
    /// Inside `Hash`: the copies into the block, summed over the hash threads.
    CopyThreads,
    /// Inside `Hash`: the hashing itself, summed over the hash threads.
    HashThreads,
    /// Inside `Hash`: creating the hash threads.
    HashSpawn,
    /// Inside `Hash`: joining the hash threads after this thread's own share.
    HashJoin,
    /// The dedup lookups and the copies into the pending block.
    Add,
    /// Closing a block: handing it to a job, or encoding it here (one block in flight).
    BlockClose,
    /// Inside `BlockClose`: waiting for the oldest block's job and writing its frame.
    BlockWait,
    /// A pool thread's encode of one block.
    Encode,
    /// A pool thread's frame hash of one block.
    FrameHash,
    /// The write thread (or the writer's thread) inside its `write` calls.
    Write,
    /// `finish`: the tables, the index, the flushes and the syncs.
    Finish,
}

const STAGES: [Stage; 16] = [
    Stage::ReaderRead,
    Stage::ReaderBlocked,
    Stage::FeedWait,
    Stage::Cut,
    Stage::Hash,
    Stage::CopyThreads,
    Stage::HashThreads,
    Stage::HashSpawn,
    Stage::HashJoin,
    Stage::Add,
    Stage::BlockClose,
    Stage::BlockWait,
    Stage::Encode,
    Stage::FrameHash,
    Stage::Write,
    Stage::Finish,
];

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::ReaderRead => "reader: read calls",
            Stage::ReaderBlocked => "reader: blocked (buffers, queue)",
            Stage::FeedWait => "feed: waiting for a segment",
            Stage::Cut => "feed: chunker cut",
            Stage::Hash => "feed: chunk hashes + copy (wall)",
            Stage::CopyThreads => "feed:   copies (sum over hash threads)",
            Stage::HashThreads => "feed:   hashes (sum over hash threads)",
            Stage::HashSpawn => "feed:   spawning the hash threads",
            Stage::HashJoin => "feed:   joining them after own share",
            Stage::Add => "feed: dedup lookups + copy to block",
            Stage::BlockClose => "feed: block close (total)",
            Stage::BlockWait => "feed:   of which waiting/writing oldest",
            Stage::Encode => "pool: encode (sum over jobs)",
            Stage::FrameHash => "pool: frame hash (sum over jobs)",
            Stage::Write => "write: write calls",
            Stage::Finish => "finish: tables, index, flush, sync",
        }
    }
}

struct Counter {
    nanos: AtomicU64,
    bytes: AtomicU64,
}

impl Counter {
    const fn new() -> Self {
        Counter {
            nanos: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }
}

static COUNTERS: [Counter; 16] = [const { Counter::new() }; 16];
static ON: OnceLock<bool> = OnceLock::new();

/// True when `LPK_PROFILE_STAGES` is set (read once per process).
#[inline]
pub fn on() -> bool {
    *ON.get_or_init(|| std::env::var_os("LPK_PROFILE_STAGES").is_some())
}

/// Clear every counter (a writer's start).
pub fn reset() {
    for c in &COUNTERS {
        c.nanos.store(0, Ordering::Relaxed);
        c.bytes.store(0, Ordering::Relaxed);
    }
}

/// Add `since`'s elapsed time and `bytes` to `stage`'s counter. Call only when
/// [`on`] is true (the caller took the `Instant`).
#[inline]
pub fn add(stage: Stage, since: Instant, bytes: u64) {
    let c = &COUNTERS[stage as usize];
    c.nanos
        .fetch_add(since.elapsed().as_nanos() as u64, Ordering::Relaxed);
    c.bytes.fetch_add(bytes, Ordering::Relaxed);
}

/// A timer that adds to `stage` when dropped, when profiling is on; else a
/// no-op. `bytes` is set by the caller before the drop.
#[derive(Debug)]
pub struct Timer {
    stage: Stage,
    start: Option<Instant>,
    /// Bytes the stage handled; set by the caller.
    pub bytes: u64,
}

impl Timer {
    /// Start timing `stage` (no clock read when profiling is off).
    #[inline]
    pub fn start(stage: Stage) -> Self {
        Timer {
            stage,
            start: on().then(Instant::now),
            bytes: 0,
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(t) = self.start {
            add(self.stage, t, self.bytes);
        }
    }
}

/// The table: every stage's seconds, bytes and MB/s (bytes per second, MB =
/// 10^6 bytes; a stage without bytes shows none), with `total` as the wall
/// seconds of the whole writer and the bytes it was fed.
pub fn table(total: std::time::Duration, fed_bytes: u64) -> String {
    let mut s = String::new();
    s.push_str("stage profile (LPK_PROFILE_STAGES; local wall seconds, not a measurement)\n");
    s.push_str(&format!(
        "{:<44} {:>9} {:>14} {:>9}\n",
        "stage", "seconds", "bytes", "MB/s"
    ));
    let row = |s: &mut String, name: &str, secs: f64, bytes: u64| {
        let rate = if secs > 0.0 && bytes > 0 {
            format!("{:.1}", bytes as f64 / secs / 1e6)
        } else {
            "-".to_string()
        };
        s.push_str(&format!("{name:<44} {secs:>9.4} {bytes:>14} {rate:>9}\n"));
    };
    for stage in STAGES {
        let c = &COUNTERS[stage as usize];
        let secs = c.nanos.load(Ordering::Relaxed) as f64 / 1e9;
        row(&mut s, stage.name(), secs, c.bytes.load(Ordering::Relaxed));
    }
    row(
        &mut s,
        "writer total (wall)",
        total.as_secs_f64(),
        fed_bytes,
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_add_and_the_table_lists_every_stage() {
        reset();
        let t = Instant::now();
        add(Stage::Cut, t, 10);
        add(Stage::Cut, t, 5);
        let c = &COUNTERS[Stage::Cut as usize];
        assert_eq!(c.bytes.load(Ordering::Relaxed), 15);
        let table = table(std::time::Duration::from_millis(1), 15);
        for stage in STAGES {
            assert!(table.contains(stage.name()), "{}", stage.name());
        }
        assert!(table.contains("writer total (wall)"));
        reset();
        assert_eq!(c.bytes.load(Ordering::Relaxed), 0);
        // A timer off is a no-op; on, it adds its bytes.
        let mut timer = Timer::start(Stage::Hash);
        timer.bytes = 7;
        drop(timer);
        let hash = &COUNTERS[Stage::Hash as usize];
        assert_eq!(hash.bytes.load(Ordering::Relaxed), if on() { 7 } else { 0 });
    }
}
