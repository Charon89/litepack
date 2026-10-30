//! E5-1: the session's operations end to end, the per-block hooks' cancellation, the error
//! classes.
#![allow(clippy::unwrap_used)]

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lpk_api::{
    CancelToken, Class, CreateSettings, Destination, ExtractOptions, Monitor, Selection, Session,
    Sources,
};

fn archive(dir: &std::path::Path, recovery: u8) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    for i in 0..20 {
        std::fs::write(
            src.join(format!("sub/f{i}.txt")),
            vec![b'a' + i as u8; 50_000],
        )
        .unwrap();
    }
    std::fs::write(src.join("top.txt"), b"top").unwrap();
    let out = dir.join("a.lpk");
    let s = CreateSettings {
        recovery,
        ..CreateSettings::default()
    };
    lpk_api::create(&Sources::contents_of(&src), &out, &s, &Monitor::default()).unwrap();
    out
}

#[test]
fn test_check_repair_and_their_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let a = archive(dir.path(), 10);
    let s = Session::open(&a, &Default::default()).unwrap();
    let events = Arc::new(AtomicU64::new(0));
    let e2 = Arc::clone(&events);
    let m = Monitor::new(
        Arc::new(move |_: &lpk_api::Event| {
            e2.fetch_add(1, Ordering::Relaxed);
        }),
        CancelToken::new(),
    );
    let t = s.test(&m).unwrap();
    assert!(t.chunks_checked);
    assert!(events.load(Ordering::Relaxed) >= 2, "first and last events");
    let r = s.check(&Monitor::default()).unwrap();
    assert!(r.frames > 0);
    assert_eq!(r.shards_damaged, 0);
    let copy = dir.path().join("copy.lpk");
    let (r2, unrepairable) = s.repair(&copy, &Monitor::default()).unwrap();
    assert!(unrepairable.is_none());
    assert_eq!(r2.frames, r.frames);
    assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(&a).unwrap());

    let cancel = CancelToken::new();
    cancel.cancel();
    let m = Monitor::with_cancel(cancel);
    assert_eq!(s.test(&m).unwrap_err().class, Class::Cancelled);
    assert_eq!(s.check(&m).unwrap_err().class, Class::Cancelled);
    let copy2 = dir.path().join("copy2.lpk");
    assert_eq!(s.repair(&copy2, &m).unwrap_err().class, Class::Cancelled);
    assert!(!copy2.exists(), "a cancelled repair removes its copy");
}

#[test]
fn extract_smart_destination_ids_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let a = archive(dir.path(), 0);
    let s = Session::open(&a, &Default::default()).unwrap();
    let m = s.model(&CancelToken::new()).unwrap();
    let mut picks = m.new_selection();
    let top = (0..m.len() as u32)
        .find(|&i| m.path(i) == "top.txt")
        .unwrap();
    picks.insert(top);
    let beside = dir.path().join("out");
    std::fs::create_dir(&beside).unwrap();
    let r = s
        .extract(
            &Selection::Ids(Arc::new(picks)),
            &Destination::Smart {
                beside: beside.clone(),
            },
            &ExtractOptions::default(),
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!(r.dir, beside, "a lone free item goes beside the archive");
    assert_eq!(std::fs::read(beside.join("top.txt")).unwrap(), b"top");
    // Two top-level items: a fresh `<stem>` folder.
    let p = s
        .preflight(
            &Selection::All,
            &Destination::Smart {
                beside: beside.clone(),
            },
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(p.dir, beside.join("a"));
    assert_eq!(p.existing, 0);
    let h = s.history(&CancelToken::new()).unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].generation, 0);
    let mut n = 0;
    s.for_each(&Selection::All, &mut |_| {
        n += 1;
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(n, m.len());
}

#[test]
fn append_then_open_the_earlier_generation_and_roll_back() {
    let dir = tempfile::tempdir().unwrap();
    let a = archive(dir.path(), 0);
    let more = dir.path().join("more");
    std::fs::create_dir(&more).unwrap();
    std::fs::write(more.join("new.txt"), b"new").unwrap();
    let s = Session::open(&a, &Default::default()).unwrap();
    let (s, _) = s
        .append(&more, &CreateSettings::default(), &Monitor::default())
        .unwrap();
    assert_eq!(s.info().generation, 1);
    let old = Session::open(
        &a,
        &lpk_api::OpenOptions {
            generation: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(old.info().generation, 0);
    assert!(old.test(&Monitor::default()).is_ok());
    drop(old);
    let g = s.rollback(0).unwrap();
    assert_eq!(g.generation, 0);
    let back = Session::open(&a, &Default::default()).unwrap();
    assert_eq!(back.info().generation, 0);
}

#[test]
fn every_class_has_a_message_and_only_usage_exits_1() {
    for c in Class::ALL {
        assert!(!c.message().is_empty(), "{c:?}");
        assert_eq!(c.exit_code(), if c == Class::Usage { 1 } else { 2 });
        assert_eq!(format!("{c:?}"), c.name());
    }
    let missing = Session::open(std::path::Path::new("no/such.lpk"), &Default::default());
    let e = missing.unwrap_err();
    assert_eq!((e.class, e.kind.as_str()), (Class::NotFound, "Io"));
}

/// A monitor whose first event signals `started` and then holds the walk for `hold`, so
/// another thread acts while the read is in flight.
fn holding_monitor(
    token: CancelToken,
    started: std::sync::mpsc::Sender<()>,
    hold: std::time::Duration,
) -> Monitor {
    let started = std::sync::Mutex::new(Some(started));
    Monitor::new(
        Arc::new(move |_: &lpk_api::Event| {
            if let Some(tx) = started.lock().unwrap().take() {
                let _ = tx.send(());
                std::thread::sleep(hold);
            }
        }),
        token,
    )
}

#[test]
fn history_cancels_a_read_in_flight_and_leaves_the_callers_token_alone() {
    let dir = tempfile::tempdir().unwrap();
    let a = archive(dir.path(), 0);
    let s = Arc::new(Session::open(&a, &Default::default()).unwrap());
    let caller = CancelToken::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let m = holding_monitor(caller.clone(), tx, std::time::Duration::from_millis(300));
    let reader = {
        let s = Arc::clone(&s);
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            (s.test(&m).map(|_| ()).map_err(|e| e.class), t)
        })
    };
    rx.recv().unwrap();
    let h = s.history(&CancelToken::new()).unwrap();
    assert_eq!(h.len(), 1, "history completes once the read ended");
    let (r, started) = reader.join().unwrap();
    assert_eq!(r, Err(Class::Cancelled));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the read stopped promptly"
    );
    assert!(
        !caller.is_cancelled(),
        "only the read's own token was cancelled"
    );
    // The caller's token is reused: the next read runs to the end.
    assert!(s.test(&Monitor::with_cancel(caller)).is_ok());
}

#[test]
fn the_callers_token_cancels_its_read_and_close_refuses_later_calls() {
    let dir = tempfile::tempdir().unwrap();
    let a = archive(dir.path(), 0);
    let s = Arc::new(Session::open(&a, &Default::default()).unwrap());
    let caller = CancelToken::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let m = holding_monitor(caller.clone(), tx, std::time::Duration::from_millis(300));
    let reader = {
        let s = Arc::clone(&s);
        std::thread::spawn(move || s.test(&m).map(|_| ()).map_err(|e| e.class))
    };
    rx.recv().unwrap();
    caller.cancel();
    assert_eq!(reader.join().unwrap(), Err(Class::Cancelled));
    s.close().unwrap();
    let e = s.test(&Monitor::default()).unwrap_err();
    assert_eq!((e.class, e.kind.as_str()), (Class::Usage, "SessionClosed"));
    assert_eq!(s.close().unwrap_err().kind, "SessionClosed");
    // Closed, the free rollback runs on the file.
    assert_eq!(lpk_api::rollback(&a, 0).unwrap().generation, 0);
}

#[test]
fn picking_a_directory_picks_its_subtree_and_all_picks_are_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let a = archive(dir.path(), 0);
    let s = Session::open(&a, &Default::default()).unwrap();
    let m = s.model(&CancelToken::new()).unwrap();
    let sub = (0..m.len() as u32).find(|&i| m.path(i) == "sub").unwrap();
    let mut picks = m.new_selection();
    picks.insert(sub);
    let out = dir.path().join("x1");
    let r = s
        .extract(
            &Selection::Ids(Arc::new(picks)),
            &Destination::Dir(out.clone()),
            &ExtractOptions::default(),
            &Monitor::default(),
        )
        .unwrap();
    let lpk_api::Extracted::Lpk(sum) = r.outcome else {
        panic!("an .lpk")
    };
    assert_eq!(sum.files, 20);
    assert!(!out.join("top.txt").exists());
    let mut all = m.new_selection();
    for i in 0..m.len() as u32 {
        all.insert(i);
    }
    let out2 = dir.path().join("x2");
    s.extract(
        &Selection::Ids(Arc::new(all)),
        &Destination::Dir(out2.clone()),
        &ExtractOptions::default(),
        &Monitor::default(),
    )
    .unwrap();
    assert!(out2.join("top.txt").exists());
}

// ---- E5-4: the engine's id-set selection ----

fn count(s: &Session, sel: &Selection) -> usize {
    let mut n = 0;
    s.for_each(sel, &mut |_| {
        n += 1;
        ControlFlow::Continue(())
    })
    .unwrap();
    n
}

fn id_of(m: &lpk_api::ListModel, path: &str) -> u32 {
    (0..m.len() as u32).find(|&i| m.path(i) == path).unwrap()
}

#[test]
fn more_than_ten_thousand_separate_picks_extract_exactly_the_picked_files() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("many")).unwrap();
    for i in 0..12_000u32 {
        std::fs::write(src.join(format!("many/f{i:05}.txt")), i.to_string()).unwrap();
    }
    let a = dir.path().join("many.lpk");
    let st = CreateSettings {
        tier: lpk_api::Tier::Store,
        ..CreateSettings::default()
    };
    lpk_api::create(&Sources::contents_of(&src), &a, &st, &Monitor::default()).unwrap();
    let s = Session::open(&a, &Default::default()).unwrap();
    let m = s.model(&CancelToken::new()).unwrap();
    let mut picks = m.new_selection();
    for i in (0..12_000u32).step_by(2) {
        picks.insert(id_of(&m, &format!("many/f{i:05}.txt")));
    }
    let sel = Selection::Ids(Arc::new(picks));
    let out = dir.path().join("out");
    let p = s
        .preflight(&sel, &Destination::Dir(out.clone()), &CancelToken::new())
        .unwrap();
    assert_eq!((p.entries, p.files), (6_000, 6_000));
    assert_eq!(count(&s, &sel), 6_000);
    s.extract(
        &sel,
        &Destination::Dir(out.clone()),
        &ExtractOptions::default(),
        &Monitor::default(),
    )
    .unwrap();
    let mut got: Vec<String> = std::fs::read_dir(out.join("many"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    got.sort();
    let want: Vec<String> = (0..12_000u32)
        .step_by(2)
        .map(|i| format!("f{i:05}.txt"))
        .collect();
    assert_eq!(got, want);
    for i in [0u32, 5_998, 11_998] {
        let b = std::fs::read(out.join(format!("many/f{i:05}.txt"))).unwrap();
        assert_eq!(b, i.to_string().as_bytes());
    }
}

#[test]
fn a_picked_directory_is_its_subtree_and_not_a_sibling_that_sorts_between() {
    // The table is sorted by path bytes: `sub`, `sub.d` ('.' < '/'), `sub/a.txt`. A directory
    // always sorts before its children (a prefix), so no valid table lists it after them;
    // the closure does not depend on the order, and the sibling in between is not caught.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::create_dir_all(src.join("sub.d")).unwrap();
    std::fs::write(src.join("sub/a.txt"), b"a").unwrap();
    std::fs::write(src.join("sub/b.txt"), b"b").unwrap();
    std::fs::write(src.join("sub.d/c.txt"), b"c").unwrap();
    std::fs::write(src.join("top.txt"), b"top").unwrap();
    let a = dir.path().join("t.lpk");
    lpk_api::create(
        &Sources::contents_of(&src),
        &a,
        &CreateSettings::default(),
        &Monitor::default(),
    )
    .unwrap();
    let s = Session::open(&a, &Default::default()).unwrap();
    let m = s.model(&CancelToken::new()).unwrap();
    let mut picks = m.new_selection();
    picks.insert(id_of(&m, "sub"));
    let sel = Selection::Ids(Arc::new(picks));
    let mut seen = Vec::new();
    s.for_each(&sel, &mut |e| {
        seen.push(e.path().to_string());
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(seen, ["sub", "sub/a.txt", "sub/b.txt"]);
    let out = dir.path().join("out");
    let p = s
        .preflight(&sel, &Destination::Dir(out.clone()), &CancelToken::new())
        .unwrap();
    assert_eq!((p.entries, p.files), (3, 2));
    s.extract(
        &sel,
        &Destination::Dir(out.clone()),
        &ExtractOptions::default(),
        &Monitor::default(),
    )
    .unwrap();
    assert_eq!(std::fs::read(out.join("sub/b.txt")).unwrap(), b"b");
    assert!(!out.join("sub.d").exists() && !out.join("top.txt").exists());
}

#[test]
fn export_of_an_id_selection_exports_exactly_the_picked_entries() {
    let dir = tempfile::tempdir().unwrap();
    let a = archive(dir.path(), 0);
    let s = Session::open(&a, &Default::default()).unwrap();
    let m = s.model(&CancelToken::new()).unwrap();
    let mut picks = m.new_selection();
    for p in ["top.txt", "sub/f3.txt", "sub/f7.txt"] {
        picks.insert(id_of(&m, p));
    }
    let out = dir.path().join("o.zip");
    let o = lpk_api::ExportOptions {
        container: lpk_api::Container::Zip,
        method: lpk_api::Method::Store,
        password: None,
        force: false,
        threads: Some(1),
        memory: None,
    };
    let sum = s
        .export(
            &Selection::Ids(Arc::new(picks)),
            &out,
            &o,
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!(sum.files, 3);
    let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut names: Vec<String> = (0..z.len())
        .map(|i| z.by_index(i).unwrap().name().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["sub/f3.txt", "sub/f7.txt", "top.txt"]);
}
