//! E3-3: progress events and cancellation through the engine. A token tripped by the first
//! event of each stage cancels a create, an append, an extraction and an export, and nothing
//! partial remains: no new archive, an append's archive byte-identical to before, no
//! unfinished extracted file, a sink stopped short. A run with an observer writes the same
//! bytes as a run without one, and delivers an event per stage with the stage's last event
//! marked.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lpk_core::{
    export_file_with, extract_file_with, CancelToken, CoreError, DefaultPolicy, Event,
    ExtractOptions, FastOptions, Monitor, Pipeline, Progress, Sink, Stage,
};
use lpk_format::Entry;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// A tree with several directories, a JPEG (peeled by the Fast tier), text files and enough
/// incompressible data for several blocks at the block size below.
fn make_tree(root: &Path) {
    for d in ["a", "a/b", "c", "c/d", "e"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("a/photo.jpg"), fixture("baseline.jpg")).unwrap();
    for i in 0..12 {
        let text = format!("line {i} of a text file\n").repeat(500);
        std::fs::write(root.join(format!("a/b/t{i}.txt")), text).unwrap();
    }
    for i in 0..10u64 {
        std::fs::write(root.join(format!("c/d/n{i}.bin")), noise(i + 1, 400 << 10)).unwrap();
    }
    std::fs::write(root.join("e/empty"), b"").unwrap();
}

const BLOCK: u64 = 512 << 10;

/// Walk the tree once before comparing two archives of it: on NTFS the modification time a
/// directory listing reports for a subdirectory is updated only when a handle on that
/// subdirectory is closed, so the first walk after the tree is made can see a stale time.
fn settle(root: &Path) {
    let _ = lpk_core::walk(root, &lpk_core::IngestOptions::default()).unwrap();
}

fn fast() -> Pipeline {
    Pipeline::fast(FastOptions {
        block_size: BLOCK,
        ..FastOptions::default()
    })
}

/// Collects every event and trips the token on the first event of one stage.
struct Trip {
    events: Mutex<Vec<Event>>,
    stage: Option<Stage>,
    token: CancelToken,
}

impl Progress for Trip {
    fn event(&self, e: &Event) {
        self.events.lock().unwrap().push(*e);
        if self.stage == Some(e.stage) {
            self.token.cancel();
        }
    }
}

fn monitor(stage: Option<Stage>) -> (Arc<Trip>, Monitor) {
    let token = CancelToken::new();
    let trip = Arc::new(Trip {
        events: Mutex::new(Vec::new()),
        stage,
        token: token.clone(),
    });
    let m = Monitor::new(trip.clone(), token);
    (trip, m)
}

fn events(trip: &Trip) -> Vec<Event> {
    trip.events.lock().unwrap().clone()
}

fn stages(trip: &Trip) -> Vec<Stage> {
    let mut v: Vec<Stage> = Vec::new();
    for e in events(trip) {
        if v.last() != Some(&e.stage) {
            v.push(e.stage);
        }
    }
    v
}

/// Every file under `dir`, relative path to contents.
fn tree_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn an_observer_sees_every_stage_of_a_create_and_changes_no_byte() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    settle(dir.path());
    let out = tempfile::tempdir().unwrap();
    let plain = out.path().join("plain.lpk");
    fast().run_file(dir.path(), &plain).unwrap();
    let (trip, m) = monitor(None);
    let mut p = fast();
    p.monitor = m;
    let watched = out.path().join("watched.lpk");
    let s = p.run_file(dir.path(), &watched).unwrap();
    assert_eq!(
        std::fs::read(&plain).unwrap(),
        std::fs::read(&watched).unwrap(),
        "an observer changes no byte"
    );
    let seen = stages(&trip);
    assert_eq!(seen[0], Stage::Walk, "{seen:?}");
    assert!(seen.contains(&Stage::Peel), "{seen:?}");
    assert!(seen.contains(&Stage::Encode), "{seen:?}");
    assert_eq!(*seen.last().unwrap(), Stage::Seal, "{seen:?}");
    let all = events(&trip);
    let last = all.last().unwrap();
    assert!(last.done);
    assert_eq!(last.bytes_out, s.writer.archive_len);
    assert_eq!(last.blocks_done, s.writer.blocks);
    assert_eq!(last.files_total, Some(24));
    assert_eq!(last.files_done, 24);
    // The walk's last event is marked done and counts every entry found.
    let walk_done = all
        .iter()
        .find(|e| e.stage == Stage::Walk && e.done)
        .unwrap();
    assert_eq!(walk_done.files_done, 29, "24 files and 5 directories");
    // Bytes in grow monotonically across the run.
    for w in all.windows(2) {
        assert!(w[1].bytes_in >= w[0].bytes_in, "{w:?}");
    }
}

#[test]
fn a_create_cancelled_at_each_stage_leaves_no_archive() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let out = tempfile::tempdir().unwrap();
    for stage in [Stage::Walk, Stage::Peel, Stage::Encode, Stage::Seal] {
        let (trip, m) = monitor(Some(stage));
        let mut p = fast();
        p.monitor = m;
        let archive = out.path().join(format!("{}.lpk", stage.name()));
        let e = p.run_file(dir.path(), &archive).unwrap_err();
        assert!(matches!(e, CoreError::Cancelled), "{stage:?}: {e}");
        assert_eq!(e.to_string(), "cancelled");
        assert!(!archive.exists(), "{stage:?}: the archive was left behind");
        let seen = stages(&trip);
        assert_eq!(*seen.last().unwrap(), stage, "{seen:?}");
        // The stage's first event tripped the token; no later stage ran.
        let after: Vec<&Stage> = seen.iter().skip_while(|s| **s != stage).skip(1).collect();
        assert!(after.is_empty(), "{seen:?}");
    }
    // A cancelled run into a writer: the same error, through `run`.
    let (_, m) = monitor(Some(Stage::Walk));
    let mut p = fast();
    p.monitor = m;
    let mut bytes = Vec::new();
    let e = p.run(dir.path(), &mut bytes).unwrap_err();
    assert!(matches!(e, CoreError::Cancelled), "{e}");
}

#[test]
fn a_token_cancelled_before_the_run_refuses_before_the_archive_exists() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let out = tempfile::tempdir().unwrap();
    let token = CancelToken::new();
    token.cancel();
    let mut p = fast();
    p.monitor = Monitor::with_cancel(token);
    let archive = out.path().join("x.lpk");
    let e = p.run_file(dir.path(), &archive).unwrap_err();
    assert!(matches!(e, CoreError::Cancelled), "{e}");
    assert!(!archive.exists());
}

#[test]
fn an_append_cancelled_at_each_stage_leaves_the_archive_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let out = tempfile::tempdir().unwrap();
    let archive = out.path().join("gen.lpk");
    fast().run_file(dir.path(), &archive).unwrap();
    let before = std::fs::read(&archive).unwrap();
    // The next generation changes a file and adds one, so there is something to encode.
    std::fs::write(dir.path().join("a/b/t0.txt"), b"changed\n").unwrap();
    std::fs::write(dir.path().join("c/new.bin"), noise(99, 600 << 10)).unwrap();
    for stage in [Stage::Walk, Stage::Peel, Stage::Encode, Stage::Seal] {
        let (trip, m) = monitor(Some(stage));
        let mut p = fast();
        p.monitor = m;
        let e = p.append_file(dir.path(), &archive, None).unwrap_err();
        assert!(matches!(e, CoreError::Cancelled), "{stage:?}: {e}");
        assert_eq!(
            std::fs::read(&archive).unwrap(),
            before,
            "{stage:?}: the archive changed"
        );
        assert_eq!(*stages(&trip).last().unwrap(), stage);
    }
    // Without a trip the append completes and the observer saw the seal.
    let (trip, m) = monitor(None);
    let mut p = fast();
    p.monitor = m;
    let s = p.append_file(dir.path(), &archive, None).unwrap();
    assert_eq!(s.writer.generation, 1);
    assert_eq!(*stages(&trip).last().unwrap(), Stage::Seal);
}

#[test]
fn an_extraction_cancelled_after_its_first_event_leaves_no_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let source = tree_files(dir.path());
    let out = tempfile::tempdir().unwrap();
    let archive = out.path().join("x.lpk");
    let s = fast().run_file(dir.path(), &archive).unwrap();
    assert!(s.writer.blocks >= 4, "{} blocks", s.writer.blocks);
    for threads in [1usize, 8] {
        let (trip, m) = monitor(Some(Stage::Extract));
        let target = out.path().join(format!("t{threads}"));
        let opts = ExtractOptions {
            threads: Some(threads),
            ..ExtractOptions::default()
        };
        let e =
            extract_file_with(&archive, &target, &[], None, &DefaultPolicy, &opts, &m).unwrap_err();
        assert!(matches!(e, CoreError::Cancelled), "{threads}: {e}");
        let left = tree_files(&target);
        // Every file left is complete and correct; not every file was written.
        for (path, bytes) in &left {
            assert_eq!(
                Some(bytes),
                source.get(path),
                "{threads}: {path} is partial"
            );
        }
        assert!(
            left.len() < source.len(),
            "{threads}: every file was written before the cancellation took effect"
        );
        let all = events(&trip);
        assert!(!all.is_empty());
        assert_eq!(all[0].stage, Stage::Extract);
        assert_eq!(all[0].blocks_total, Some(s.writer.blocks));
        assert_eq!(all[0].files_total, Some(24));
        assert!(
            all.iter().all(|e| !e.done),
            "a cancelled extraction has no last event"
        );
    }
    // A token cancelled before the run refuses before any directory is made.
    let token = CancelToken::new();
    token.cancel();
    let target = out.path().join("none");
    let e = extract_file_with(
        &archive,
        &target,
        &[],
        None,
        &DefaultPolicy,
        &ExtractOptions::default(),
        &Monitor::with_cancel(token),
    )
    .unwrap_err();
    assert!(matches!(e, CoreError::Cancelled), "{e}");
    assert!(!target.join("a").exists());
    // A watched extraction completes with a last event and the right counts.
    let (trip, m) = monitor(None);
    let target = out.path().join("full");
    let s = extract_file_with(
        &archive,
        &target,
        &[],
        None,
        &DefaultPolicy,
        &ExtractOptions::default(),
        &m,
    )
    .unwrap();
    assert_eq!(tree_files(&target), source);
    let last = *events(&trip).last().unwrap();
    assert!(last.done);
    assert_eq!(last.files_done, s.files);
    assert_eq!(last.bytes_out, s.bytes);
    assert_eq!(last.blocks_done, s.blocks_needed);
}

/// A sink that collects the entries it is given.
#[derive(Default)]
struct Collect {
    entries: Vec<String>,
    bytes: u64,
}

impl Sink for Collect {
    fn directory(&mut self, e: &Entry) -> Result<(), CoreError> {
        self.entries.push(e.path.clone());
        Ok(())
    }
    fn symlink(&mut self, e: &Entry) -> Result<(), CoreError> {
        self.entries.push(e.path.clone());
        Ok(())
    }
    fn file(&mut self, e: &Entry, content: &mut dyn Read) -> Result<(), CoreError> {
        let mut v = Vec::new();
        content.read_to_end(&mut v).map_err(|err| CoreError::Io {
            path: PathBuf::from(&e.path),
            source: err,
        })?;
        self.bytes += v.len() as u64;
        self.entries.push(e.path.clone());
        Ok(())
    }
}

#[test]
fn an_export_cancelled_after_its_first_event_stops_the_sink_short() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let out = tempfile::tempdir().unwrap();
    let archive = out.path().join("x.lpk");
    fast().run_file(dir.path(), &archive).unwrap();
    for threads in [1usize, 8] {
        let (trip, m) = monitor(Some(Stage::Export));
        let opts = ExtractOptions {
            threads: Some(threads),
            ..ExtractOptions::default()
        };
        let mut sink = Collect::default();
        let e = export_file_with(&archive, &[], None, &mut sink, &opts, &m).unwrap_err();
        assert!(matches!(e, CoreError::Cancelled), "{threads}: {e}");
        assert!(sink.entries.len() < 29, "{threads}: {}", sink.entries.len());
        let all = events(&trip);
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].stage, Stage::Export);
        assert_eq!(all[0].files_total, Some(24));
    }
    let token = CancelToken::new();
    token.cancel();
    let mut sink = Collect::default();
    let e = export_file_with(
        &archive,
        &[],
        None,
        &mut sink,
        &ExtractOptions::default(),
        &Monitor::with_cancel(token),
    )
    .unwrap_err();
    assert!(matches!(e, CoreError::Cancelled), "{e}");
    assert!(sink.entries.is_empty(), "nothing reaches the sink");
    let (trip, m) = monitor(None);
    let mut sink = Collect::default();
    let s = export_file_with(
        &archive,
        &[],
        None,
        &mut sink,
        &ExtractOptions::default(),
        &m,
    )
    .unwrap();
    assert_eq!(sink.entries.len(), 29);
    let last = *events(&trip).last().unwrap();
    assert!(last.done);
    assert_eq!(last.files_done, s.files);
    assert_eq!(last.bytes_out, s.bytes);
    assert_eq!(last.blocks_done, s.blocks_decoded);
    assert_eq!(last.blocks_total, Some(s.blocks_decoded));
}

#[test]
fn the_token_is_shared_across_threads() {
    let token = CancelToken::new();
    let t2 = token.clone();
    let h = std::thread::spawn(move || t2.cancel());
    h.join().unwrap();
    assert!(token.is_cancelled());
    let f: Result<(), CoreError> = token.check();
    assert!(matches!(f, Err(CoreError::Cancelled)));
}
