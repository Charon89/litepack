//! E5-8's command layer on the conformance vectors (`docs/spec/CONFORMANCE.md`,
//! `crates/lpk-format/tests/vectors/`): test (keyless and unlocked, cancelled, the verb queue),
//! check and repair with the copy's name, history, Browse and rollback, and append.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lpk_api::{CancelToken, Class, CreateSettings, Monitor, Sources, Tier};
use lpk_gui::app::{App, Opened};
use lpk_gui::create::TierChoice;
use lpk_gui::extract::{NoPrompt, Prompter};
use lpk_gui::grants::{Grant, GrantKind};
use lpk_gui::maintain::{AppendRequest, Confirm, RunState, TestResult, TestState};
use zeroize::Zeroizing;

fn vectors() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-format/tests/vectors")
}

/// A copy of vector `name` in `dir`, under `as_name`.
fn copy_vector(dir: &Path, name: &str, as_name: &str) -> PathBuf {
    let to = dir.join(as_name);
    std::fs::copy(vectors().join(name), &to).unwrap();
    to
}

fn table(file: &str) -> toml::Table {
    std::fs::read_to_string(vectors().join(file))
        .unwrap()
        .parse()
        .unwrap()
}

/// Answers the password prompts from a list.
struct Answers(Mutex<VecDeque<Option<&'static str>>>, AtomicUsize);

impl Answers {
    fn new(a: &[Option<&'static str>]) -> Self {
        Answers(Mutex::new(a.iter().copied().collect()), AtomicUsize::new(0))
    }
    fn asked(&self) -> usize {
        self.1.load(Ordering::SeqCst)
    }
}

impl Prompter for Answers {
    fn password(&self, _: &str, _: bool) -> Option<Zeroizing<String>> {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .flatten()
            .map(|s| Zeroizing::new(s.to_string()))
    }
}

struct Yes(bool, Mutex<Vec<String>>);

impl Confirm for Yes {
    fn confirm(&self, _title: &str, text: &str) -> bool {
        self.1.lock().unwrap().push(text.to_string());
        self.0
    }
}

/// A monitor that cancels on its `n`th progress event: the run is cancelled mid-write.
fn cancel_on(n: usize) -> (Monitor, Arc<AtomicUsize>) {
    let token = CancelToken::new();
    let seen = Arc::new(AtomicUsize::new(0));
    let (t, s) = (token.clone(), seen.clone());
    let m = Monitor::new(
        Arc::new(move |_: &lpk_api::Event| {
            if s.fetch_add(1, Ordering::SeqCst) + 1 >= n {
                t.cancel();
            }
        }),
        token,
    );
    (m, seen)
}

/// A monitor that cancels on the first `Encode` event with more than `floor` bytes out, that
/// is once the run has written into the archive (the walk's events come first).
fn cancel_when_writing(floor: u64) -> (Monitor, Arc<AtomicUsize>) {
    let token = CancelToken::new();
    let seen = Arc::new(AtomicUsize::new(0));
    let (t, s) = (token.clone(), seen.clone());
    let m = Monitor::new(
        Arc::new(move |e: &lpk_api::Event| {
            if e.stage == lpk_api::Stage::Encode && e.bytes_out > floor {
                s.fetch_add(1, Ordering::SeqCst);
                t.cancel();
            }
        }),
        token,
    );
    (m, seen)
}

/// Bytes that do not compress, so that a run spans several blocks.
fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 11) as u8
        })
        .collect()
}

fn grant(app: &App, p: &Path) -> Grant {
    app.grants.mint(GrantKind::Archive, p).grant
}

fn open(app: &App, p: &Path) -> Opened {
    app.open(&grant(app, p), &CancelToken::new()).unwrap()
}

fn names(app: &App, o: &Opened) -> Vec<String> {
    let (_, _, rows) = lpk_gui::batch::decode(&app.rows(o.session, 0, 1000).unwrap()).unwrap();
    rows.into_iter().map(|r| r.name).collect()
}

fn test_vector(app: &App, p: &Path, unlock: bool, pw: &dyn Prompter) -> TestResult {
    app.test_archive(&grant(app, p), unlock, pw, &Monitor::default())
        .unwrap()
}

#[test]
fn test_passes_on_every_vector_with_the_counts_the_vector_states() {
    let app = App::new(None);
    let expected = table("expected.toml");
    let mut ran = 0;
    for (name, v) in &expected {
        let Some(verify) = v.get("verify").and_then(|x| x.as_str()) else {
            continue;
        };
        let r = test_vector(&app, &vectors().join(name), false, &NoPrompt);
        if name == "zstd-dict.lpk" {
            // Needs a prior file the GUI has no way to give (D-110's `priors` is the API's):
            // a failure that names it, and no offer to repair what is not damaged.
            assert_eq!(r.state, TestState::Failed);
            let e = r.error.unwrap();
            assert_eq!(e.kind, "MissingPrior");
            assert!(!r.offer_repair);
            continue;
        }
        assert_eq!(r.state, TestState::Passed, "{name}: {:?}", r.error);
        assert!(r.error.is_none() && !r.offer_repair);
        // A keyless open of a sealed archive cannot count what it cannot read.
        if r.chunks_checked {
            let want = format!(
                "ok: {} entries, {} chunks, {} blocks
",
                r.entries, r.chunks, r.blocks
            );
            assert_eq!(verify, want, "{name}");
        }
        // Sealed vectors, keyless: the frames only, and the dialog offers Unlock.
        assert_eq!(r.chunks_checked, !name.starts_with("sealed-"), "{name}");
        assert_eq!(r.offer_unlock, name.starts_with("sealed-"), "{name}");
        ran += 1;
    }
    assert!(ran >= 12, "{ran} vectors tested");
}

#[test]
fn test_keyless_then_unlocked_through_the_prompter() {
    let app = App::new(None);
    let pw = table("vectors.toml");
    for name in [
        "sealed-aes.lpk",
        "sealed-xchacha.lpk",
        "sealed-listable.lpk",
    ] {
        let password: &'static str = Box::leak(
            pw[name]["password"]
                .as_str()
                .unwrap()
                .to_string()
                .into_boxed_str(),
        );
        let p = vectors().join(name);
        // The verb, keyless: never a prompt by itself.
        let ask = Answers::new(&[Some(password)]);
        let r = test_vector(&app, &p, false, &ask);
        assert!(!r.chunks_checked && r.offer_unlock, "{name}");
        assert_eq!(ask.asked(), 0, "{name}: no prompt without Unlock");
        // Unlock: a wrong password asks again, then the full check runs.
        let ask = Answers::new(&[Some("not it"), Some(password)]);
        let r = test_vector(&app, &p, true, &ask);
        assert_eq!(r.state, TestState::Passed, "{name}: {:?}", r.error);
        assert!(r.chunks_checked && !r.offer_unlock, "{name}");
        assert_eq!(ask.asked(), 2, "{name}");
        // Cancel at the prompt: Cancelled, nothing claimed.
        let r = test_vector(&app, &p, true, &Answers::new(&[None]));
        assert_eq!(r.state, TestState::Cancelled);
        assert!(!r.chunks_checked);
    }
}

#[test]
fn an_open_sessions_unlock_keeps_it_unlocked() {
    let d = tempfile::tempdir().unwrap();
    let p = copy_vector(d.path(), "sealed-listable.lpk", "s.lpk");
    let app = App::new(None);
    let o = open(&app, &p);
    let m = Monitor::default();
    let keyless = app.test_run(o.session, false, &NoPrompt, &m).unwrap();
    assert!(!keyless.chunks_checked && keyless.offer_unlock);
    let ask = Answers::new(&[Some("list me")]);
    let r = app.test_run(o.session, true, &ask, &m).unwrap();
    assert!(r.chunks_checked && r.state == TestState::Passed);
    // Stays unlocked: the next plain test is a full one, with no prompt.
    let again = app.test_run(o.session, false, &NoPrompt, &m).unwrap();
    assert!(again.chunks_checked);
}

#[test]
fn a_test_cancelled_mid_run_ends_cancelled() {
    let app = App::new(None);
    let p = vectors().join("recovery-groups.lpk");
    let token = CancelToken::new();
    let t = token.clone();
    let m = Monitor::new(Arc::new(move |_: &lpk_api::Event| t.cancel()), token);
    let r = app
        .test_archive(&grant(&app, &p), false, &NoPrompt, &m)
        .unwrap();
    assert_eq!(r.state, TestState::Cancelled);
    assert!(r.error.is_none() && !r.offer_repair);
    // Through an open session too.
    let o = open(&app, &p);
    let token = CancelToken::new();
    token.cancel();
    let r = app
        .test_run(
            o.session,
            false,
            &NoPrompt,
            &Monitor::new(Arc::new(|_: &lpk_api::Event| {}), token),
        )
        .unwrap();
    assert_eq!(r.state, TestState::Cancelled);
}

#[test]
fn the_test_verb_queues_several_archives_one_at_a_time() {
    let app = Arc::new(App::new(None));
    let paths: Vec<PathBuf> = ["zstd-basic.lpk", "bwt-text.lpk", "recovery-groups.lpk"]
        .iter()
        .map(|n| vectors().join(n))
        .collect();
    let live = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let (app, live, most) = (app.clone(), live.clone(), most.clone());
            std::thread::spawn(move || {
                let g = app.ops.begin(i as u64, true).unwrap();
                let n = live.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(n, Ordering::SeqCst);
                let r = app
                    .test_archive(
                        &app.grants.mint(GrantKind::Archive, &p).grant,
                        false,
                        &NoPrompt,
                        &Monitor::new(Arc::new(|_: &lpk_api::Event| {}), g.token.clone()),
                    )
                    .unwrap();
                live.fetch_sub(1, Ordering::SeqCst);
                r
            })
        })
        .collect();
    for t in threads {
        assert_eq!(t.join().unwrap().state, TestState::Passed);
    }
    assert_eq!(most.load(Ordering::SeqCst), 1);
    assert!(!app.ops.busy());
}

#[test]
fn a_damaged_archive_fails_test_check_counts_and_repair_writes_the_named_copy() {
    let d = tempfile::tempdir().unwrap();
    // recovery-groups.lpk with one byte flipped in a data shard (the vector
    // malformed-recovery-damaged.lpk is the same file).
    let good = std::fs::read(vectors().join("recovery-groups.lpk")).unwrap();
    let mut bytes = good.clone();
    bytes[good.len() / 2] ^= 0xff;
    let p = d.path().join("data.lpk");
    std::fs::write(&p, &bytes).unwrap();
    assert_ne!(good, bytes);
    let app = App::new(None);
    let m = Monitor::default();

    let t = test_vector(&app, &p, false, &NoPrompt);
    assert_eq!(t.state, TestState::Failed, "{t:?}");
    assert!(t.offer_repair);
    let e = t.error.unwrap();
    assert!(!e.detail.is_empty());

    let o = open(&app, &p);
    let c = app.check_run(o.session, &m).unwrap();
    assert_eq!(c.state, RunState::Done);
    assert!(c.has_recovery && c.damaged);
    assert_eq!(c.frames, 3);
    assert!(c.shards_damaged >= 1, "{c:?}");

    let r = app.repair_run(o.session, None, &m).unwrap();
    assert_eq!(r.copy.as_deref(), Some("data (repaired).lpk"));
    assert!(r.unrepairable.is_none(), "{r:?}");
    assert_eq!(r.shards_repaired, c.shards_damaged);
    let copy = d.path().join("data (repaired).lpk");
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        good,
        "the copy is the original archive"
    );
    assert_eq!(
        std::fs::read(&p).unwrap(),
        bytes,
        "the original is never written"
    );
    assert_eq!(r.test.as_ref().unwrap().state, TestState::Passed);
    assert_eq!(
        app.grants
            .resolve(&r.grant.unwrap().grant, GrantKind::Archive)
            .unwrap(),
        copy
    );
    // The copy passes `test`; a second repair never overwrites: `(repaired 2)`.
    assert_eq!(
        test_vector(&app, &copy, false, &NoPrompt).state,
        TestState::Passed
    );
    let r2 = app.repair_run(o.session, None, &m).unwrap();
    assert_eq!(r2.copy.as_deref(), Some("data (repaired 2).lpk"));
    let r3 = app.repair_run(o.session, None, &m).unwrap();
    assert_eq!(r3.copy.as_deref(), Some("data (repaired 3).lpk"));
    // Into a chosen folder.
    let other = d.path().join("elsewhere");
    std::fs::create_dir(&other).unwrap();
    let f = app.grants.mint(GrantKind::Folder, &other).grant;
    let r4 = app.repair_run(o.session, Some(&f), &m).unwrap();
    assert_eq!(r4.copy.as_deref(), Some("data (repaired).lpk"));
    assert!(other.join("data (repaired).lpk").exists());
}

#[test]
fn the_conformance_damaged_vector_repairs_to_its_good_twin() {
    let d = tempfile::tempdir().unwrap();
    let p = copy_vector(d.path(), "malformed-recovery-damaged.lpk", "bad.lpk");
    let app = App::new(None);
    let m = Monitor::default();
    let o = open(&app, &p);
    let c = app.check_run(o.session, &m).unwrap();
    assert_eq!((c.frames, c.frames_unusable, c.shards_damaged), (3, 0, 1));
    let r = app.repair_run(o.session, None, &m).unwrap();
    assert_eq!(r.shards_repaired, 1);
    assert_eq!(
        std::fs::read(d.path().join("bad (repaired).lpk")).unwrap(),
        std::fs::read(vectors().join("recovery-groups.lpk")).unwrap()
    );
    assert_eq!(r.test.unwrap().state, TestState::Passed);
}

#[test]
fn check_says_so_when_there_are_no_recovery_records_and_a_repair_cancels_clean() {
    let d = tempfile::tempdir().unwrap();
    let p = copy_vector(d.path(), "zstd-basic.lpk", "plain.lpk");
    let app = App::new(None);
    let o = open(&app, &p);
    let c = app.check_run(o.session, &Monitor::default()).unwrap();
    assert!(!c.has_recovery && !c.damaged && c.frames == 0);
    // Nothing to emit without recovery frames: a token cancelled up front.
    let token = CancelToken::new();
    token.cancel();
    let m = Monitor::new(Arc::new(|_: &lpk_api::Event| {}), token);
    let c = app.check_run(o.session, &m).unwrap();
    assert_eq!(c.state, RunState::Cancelled);
}

#[test]
fn a_repair_cancelled_mid_run_leaves_no_copy_and_no_partial() {
    let d = tempfile::tempdir().unwrap();
    let p = copy_vector(d.path(), "malformed-recovery-damaged.lpk", "bad.lpk");
    let before = std::fs::read(&p).unwrap();
    let app = App::new(None);
    let o = open(&app, &p);
    let (m, seen) = cancel_on(1);
    let r = app.repair_run(o.session, None, &m).unwrap();
    assert!(seen.load(Ordering::SeqCst) >= 1, "the run emitted events");
    assert_eq!(r.state, RunState::Cancelled);
    let left: Vec<String> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["bad.lpk"], "no copy and no partial");
    assert_eq!(std::fs::read(&p).unwrap(), before);
    // The next repair still gets the first name.
    let r = app
        .repair_run(o.session, None, &Monitor::default())
        .unwrap();
    assert_eq!(r.copy.as_deref(), Some("bad (repaired).lpk"));
    assert!(!d.path().join("bad (repaired).lpk.partial").exists());
}

#[test]
fn a_damaged_zip_fails_its_test_with_no_offer_to_repair() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("broken.zip");
    // A stored ZIP of one file whose central directory was cut off.
    let data = b"hello zip";
    let mut zip = vec![0x50, 0x4B, 3, 4, 20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0];
    zip.extend_from_slice(&0u32.to_le_bytes());
    zip.extend_from_slice(&(data.len() as u32).to_le_bytes());
    zip.extend_from_slice(&(data.len() as u32).to_le_bytes());
    zip.extend_from_slice(&5u16.to_le_bytes());
    zip.extend_from_slice(&[0, 0]);
    zip.extend_from_slice(b"a.txt");
    zip.extend_from_slice(data);
    std::fs::write(&p, zip).unwrap();
    let app = App::new(None);
    let r = test_vector(&app, &p, false, &NoPrompt);
    assert_eq!(r.state, TestState::Failed);
    assert!(!r.offer_repair && !r.offer_unlock, "{r:?}");
}

#[test]
fn rollback_refuses_an_archive_that_changed_behind_the_session() {
    let d = tempfile::tempdir().unwrap();
    let p = copy_vector(d.path(), "journal-3gen.lpk", "gen.lpk");
    let app = App::new(None);
    let o = open(&app, &p);
    assert_eq!(o.latest_generation, 2);
    // Another process appends generation 3 while the session shows generation 2.
    let extra = folder_with(d.path(), "extra", &[("x.txt", b"more")]);
    let s = CreateSettings {
        tier: Tier::Store,
        ..CreateSettings::default()
    };
    lpk_api::append(&p, &extra, &s, None, &Monitor::default()).unwrap();
    let after = std::fs::read(&p).unwrap();
    let yes = Yes(true, Mutex::new(Vec::new()));
    let r = app
        .rollback_run(o.session, 1, &yes, &NoPrompt, &CancelToken::new())
        .unwrap();
    assert_eq!(r.state, RunState::Failed);
    let e = r.error.unwrap();
    assert_eq!(e.kind, "ArchiveChanged", "{e}");
    assert_eq!(std::fs::read(&p).unwrap(), after, "nothing was truncated");
    assert_eq!(r.lost, 0);
    // The archive reopened at what it is now.
    assert_eq!(r.opened.unwrap().latest_generation, 3);

    // Rolled back behind the session and appended again under the same number, with other
    // content: the number matches, the length does not.
    let q = copy_vector(d.path(), "journal-3gen.lpk", "gen2.lpk");
    let o = open(&app, &q);
    lpk_api::rollback(&q, 1).unwrap();
    let other = folder_with(d.path(), "other", &[("y.txt", b"a different generation 2")]);
    lpk_api::append(&q, &other, &s, None, &Monitor::default()).unwrap();
    assert_eq!(lpk_api::committed_generation(&q).unwrap(), 2);
    let after = std::fs::read(&q).unwrap();
    let r = app
        .rollback_run(o.session, 1, &yes, &NoPrompt, &CancelToken::new())
        .unwrap();
    assert_eq!(r.state, RunState::Failed);
    assert_eq!(r.error.unwrap().kind, "ArchiveChanged");
    assert_eq!(std::fs::read(&q).unwrap(), after);
}

#[test]
fn history_lists_generations_browse_opens_one_and_rollback_truncates_a_copy() {
    let d = tempfile::tempdir().unwrap();
    let p = copy_vector(d.path(), "journal-3gen.lpk", "gen.lpk");
    let app = App::new(None);
    let cancel = CancelToken::new();
    let o = open(&app, &p);
    assert_eq!((o.generation, o.latest_generation), (2, 2));
    let h = app.history(o.session, &cancel).unwrap();
    let gens: Vec<u64> = h.generations.iter().map(|g| g.generation).collect();
    assert_eq!(gens, [2, 1, 0]);
    assert!(h.generations[0].latest && !h.generations[1].latest);
    assert_eq!(h.generations[0].size, std::fs::metadata(&p).unwrap().len());
    assert!(h.generations[2].size < h.generations[1].size);
    let latest_names = names(&app, &o);
    assert!(latest_names.contains(&"e.txt".to_string()));

    // Browse generation 0: its entries, read-only.
    let b = app
        .history_browse(o.session, 0, &cancel, &NoPrompt)
        .unwrap();
    assert_eq!((b.generation, b.latest_generation), (0, 2));
    let old = names(&app, &b);
    assert!(old.contains(&"c.txt".to_string()) && !old.contains(&"e.txt".to_string()));
    assert!(!latest_names.contains(&"c.txt".to_string()));
    // The actions that write refuse a view; the history of a view is the whole chain.
    let yes = Yes(true, Mutex::new(Vec::new()));
    let e = app
        .rollback_run(b.session, 0, &yes, &NoPrompt, &cancel)
        .unwrap_err();
    assert!(e.is(Class::Usage), "{e}");
    assert!(app.append_plan(b.session).is_err());
    assert!(app
        .repair_run(b.session, None, &Monitor::default())
        .is_err());
    assert_eq!(
        app.history(b.session, &cancel).unwrap().generations.len(),
        3
    );
    assert!(
        yes.1.lock().unwrap().is_empty(),
        "no question for a refused view"
    );
    // Restore: the extract dialog works on the view (E5-4's paths).
    // A generation past the end is refused.
    assert!(app
        .history_browse(o.session, 9, &cancel, &NoPrompt)
        .is_err());

    // Declined: nothing happens.
    let before = std::fs::read(&p).unwrap();
    let no = Yes(false, Mutex::new(Vec::new()));
    let r = app
        .rollback_run(o.session, 1, &no, &NoPrompt, &cancel)
        .unwrap();
    assert_eq!(r.state, RunState::Declined);
    assert_eq!(std::fs::read(&p).unwrap(), before);
    let text = no.1.lock().unwrap()[0].clone();
    assert!(
        text.contains("generations 2 to 2") && text.contains("cannot be undone"),
        "{text}"
    );

    // Confirmed: the session closes, the file is truncated, the archive reopens at 1.
    let r = app
        .rollback_run(o.session, 1, &yes, &NoPrompt, &cancel)
        .unwrap();
    assert_eq!((r.state, r.generation, r.lost), (RunState::Done, 1, 1));
    let reopened = r.opened.unwrap();
    assert_eq!((reopened.generation, reopened.latest_generation), (1, 1));
    assert_eq!(std::fs::metadata(&p).unwrap().len(), h.generations[1].size);
    assert_eq!(r.size, h.generations[1].size);
    assert!(
        app.rows(o.session, 0, 10).is_err(),
        "the old session is closed"
    );
    let h2 = app.history(reopened.session, &cancel).unwrap();
    let gens: Vec<u64> = h2.generations.iter().map(|g| g.generation).collect();
    assert_eq!(gens, [1, 0]);
    assert_eq!(
        test_vector(&app, &p, false, &NoPrompt).state,
        TestState::Passed
    );
    // The vector itself is untouched.
    assert_eq!(
        std::fs::metadata(vectors().join("journal-3gen.lpk"))
            .unwrap()
            .len(),
        before.len() as u64
    );
}

fn folder_with(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let f = dir.join(name);
    std::fs::create_dir_all(&f).unwrap();
    for (n, b) in files {
        std::fs::write(f.join(n), b).unwrap();
    }
    f
}

fn create_from(src: &Path, out: &Path, seal: Option<lpk_api::Seal>) {
    let s = CreateSettings {
        tier: Tier::Store,
        seal,
        ..CreateSettings::default()
    };
    lpk_api::create(&Sources::contents_of(src), out, &s, &Monitor::default()).unwrap();
}

fn request(app: &App, folder: &Path) -> AppendRequest {
    AppendRequest {
        folder: app.grants.mint(GrantKind::Input, folder).grant,
        tier: TierChoice::Fast,
        recovery: 5,
        test: true,
    }
}

#[test]
fn append_mirrors_the_folder_tests_it_and_cancel_leaves_the_archive_alone() {
    let d = tempfile::tempdir().unwrap();
    let first = folder_with(
        d.path(),
        "one",
        &[("keep.txt", b"keep me"), ("gone.txt", b"to be dropped")],
    );
    let second = folder_with(
        d.path(),
        "two",
        &[("keep.txt", b"keep me"), ("new.txt", b"brand new")],
    );
    // Several blocks of data that do not compress, so that a cancel lands mid-write.
    for i in 0..3u32 {
        std::fs::write(second.join(format!("big{i}.bin")), noise(3 << 20, i)).unwrap();
    }
    let p = d.path().join("a.lpk");
    create_from(&first, &p, None);
    let app = App::new(Some(&d.path().join("journal")));
    let o = open(&app, &p);
    let plan = app.append_plan(o.session).unwrap();
    assert_eq!(
        (
            plan.tier,
            plan.recovery,
            plan.test,
            plan.encrypted,
            plan.generation
        ),
        (TierChoice::Balanced, 5, true, false, 0)
    );

    // A cancelled append leaves the archive as it was; the archive reopens.
    let before = std::fs::read(&p).unwrap();
    let (cancelled, seen) = cancel_when_writing(0);
    let r = app
        .append_run(o.session, &request(&app, &second), &NoPrompt, &cancelled)
        .unwrap();
    assert!(
        seen.load(Ordering::SeqCst) >= 1,
        "cancelled on an Encode event with bytes written"
    );
    assert_eq!(r.state, RunState::Cancelled);
    assert_eq!(
        std::fs::read(&p).unwrap(),
        before,
        "the archive is as it was"
    );
    assert!(
        app.recover().is_empty(),
        "no record of the cancelled append is left"
    );
    let o = r.opened.expect("reopened after a cancel");
    assert_eq!(o.generation, 0);

    // The append: a new generation that mirrors the folder.
    let r = app
        .append_run(
            o.session,
            &request(&app, &second),
            &NoPrompt,
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!((r.state, r.generation), (RunState::Done, 1));
    assert!(r.entries >= 2 && r.archive_len > before.len() as u64);
    assert_eq!(r.test.as_ref().unwrap().state, TestState::Passed);
    let o = r.opened.unwrap();
    assert_eq!((o.generation, o.latest_generation), (1, 1));
    let now = names(&app, &o);
    assert!(now.contains(&"new.txt".to_string()), "{now:?}");
    assert!(now.contains(&"keep.txt".to_string()), "{now:?}");
    assert!(!now.contains(&"gone.txt".to_string()), "{now:?}");
    // The journal holds no record of the finished append.
    assert!(app.recover().is_empty());
    // A folder grant of another kind is refused.
    let bad = AppendRequest {
        folder: app.grants.mint(GrantKind::Archive, &second).grant,
        ..request(&app, &second)
    };
    assert!(app
        .append_run(o.session, &bad, &NoPrompt, &Monitor::default())
        .is_err());
}

#[test]
fn append_to_an_encrypted_archive_asks_for_the_password_before_writing() {
    let d = tempfile::tempdir().unwrap();
    let first = folder_with(d.path(), "one", &[("a.txt", b"alpha")]);
    let second = folder_with(d.path(), "two", &[("b.txt", b"bravo")]);
    let p = d.path().join("s.lpk");
    create_from(
        &first,
        &p,
        Some(lpk_api::Seal {
            suite: lpk_api::Suite::XChaCha,
            listable: true,
            credentials: lpk_api::Credentials::password(b"hunter2".to_vec()),
        }),
    );
    let app = App::new(None);
    let o = open(&app, &p);
    assert!(app.append_plan(o.session).unwrap().encrypted);
    let before = std::fs::read(&p).unwrap();
    // No one to ask: refused, nothing written, the session is still open.
    let e = app
        .append_run(
            o.session,
            &request(&app, &second),
            &NoPrompt,
            &Monitor::default(),
        )
        .unwrap_err();
    assert!(e.is(Class::PasswordRequired), "{e}");
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert!(app.rows(o.session, 0, 10).is_ok());
    // Cancel at the prompt: nothing written.
    let r = app
        .append_run(
            o.session,
            &request(&app, &second),
            &Answers::new(&[None]),
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!(r.state, RunState::Cancelled);
    assert_eq!(std::fs::read(&p).unwrap(), before);
    // A wrong password asks again; the right one appends and the archive reopens without a
    // second question (the typed password serves the listing too).
    let ask = Answers::new(&[Some("wrong"), Some("hunter2")]);
    let r = app
        .append_run(
            o.session,
            &request(&app, &second),
            &ask,
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!(
        (r.state, r.generation),
        (RunState::Done, 1),
        "{:?}",
        r.reopen_error
    );
    assert!(r.test.unwrap().chunks_checked);
    assert_eq!(ask.asked(), 2);
    let o = r.opened.unwrap();
    assert!(names(&app, &o).contains(&"b.txt".to_string()));
}
