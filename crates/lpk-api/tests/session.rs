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
