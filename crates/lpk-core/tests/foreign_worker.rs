//! E4-2 end to end: the broker drives the test worker (`lpk-test-worker`, the worker loop over
//! the test fixture with faults) started through `lpk-sandbox-sys` — LPAC on Windows (a profile
//! per test, deleted at its end; the worker copied into a directory the package SID may read,
//! as the installer will grant it), unsandboxed on Linux (decision (d)).
//!
//! The fault tests set short limits and assert the elapsed time against the limit plus
//! [`SLACK`], which covers the launch (an AppContainer start), the 50 ms polling of the broker
//! and the kill.
#![cfg(any(windows, target_os = "linux"))]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lpk_core::foreign::{self, Limits, Options};
use lpk_core::{CancelToken, CoreError, ForeignError, Monitor, Selection, WorkerError};
use lpk_format::{Credentials, FormatError};
use lpk_proto::fixture::{self, Member};
use lpk_proto::RecordKind;

/// What a fault test allows beyond its limit.
const SLACK: Duration = Duration::from_secs(5);

/// The test worker, built beside this test's executable (`target/<profile>/`).
fn test_worker() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).unwrap();
    let w = dir.join(if cfg!(windows) {
        "lpk-test-worker.exe"
    } else {
        "lpk-test-worker"
    });
    assert!(
        w.is_file(),
        "{} missing: build the workspace (cargo build --workspace, or cargo nextest run --workspace)",
        w.display()
    );
    w
}

/// A staged worker and its AppContainer profile (Windows), removed at the end.
struct Staged {
    exe: PathBuf,
    profile: Option<String>,
    _dir: tempfile::TempDir,
}

impl Drop for Staged {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(p) = &self.profile {
            let _ = lpk_sandbox_sys::delete_appcontainer_profile(p);
        }
    }
}

fn stage() -> Staged {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join(test_worker().file_name().unwrap());
    std::fs::copy(test_worker(), &exe).unwrap();
    #[cfg(windows)]
    let profile = {
        const READ_EXECUTE: u32 = 0x0012_0089 | 0x0012_00A0;
        let name = format!(
            "LitePack.Test.Broker.{}.{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        lpk_sandbox_sys::create_appcontainer_profile(&name).unwrap();
        let sid = lpk_sandbox_sys::appcontainer_sid_string(&name).unwrap();
        lpk_sandbox_sys::grant_path_access(dir.path(), &sid, READ_EXECUTE, true).unwrap();
        Some(name)
    };
    #[cfg(not(windows))]
    let profile = {
        let _ = N.fetch_add(1, Ordering::Relaxed);
        None
    };
    Staged {
        exe,
        profile,
        _dir: dir,
    }
}

impl Staged {
    fn options(&self, fault: Option<&str>, limits: Limits) -> Options {
        Options {
            worker: Some(self.exe.clone()),
            appcontainer: self.profile.clone(),
            limits,
            worker_args: fault
                .map(|f| vec![format!("--fault={f}")])
                .unwrap_or_default(),
            ..Options::default()
        }
    }
}

fn members() -> Vec<Member> {
    let big: Vec<u8> = (0..700_000u32).map(|i| (i * 7 % 251) as u8).collect();
    vec![
        Member::dir("d"),
        Member::file("d/a.txt", b"hello"),
        Member::file("d/big.bin", &big),
        Member::file("empty", b""),
        Member::link(RecordKind::HardLink, "d/copy.txt", "d/a.txt"),
    ]
}

fn write_fixture(dir: &Path, members: &[Member], password: Option<&[u8]>) -> PathBuf {
    let p = dir.join("archive.fx");
    std::fs::write(&p, fixture::write(members, password)).unwrap();
    p
}

fn short() -> Limits {
    Limits {
        hello: Duration::from_secs(10),
        no_progress: Duration::from_secs(1),
        cancel_grace: Duration::from_millis(500),
        ..Limits::default()
    }
}

#[test]
fn a_foreign_archive_lists_and_extracts_through_the_sandboxed_worker() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let members = members();
    let archive = write_fixture(work.path(), &members, None);
    let opts = s.options(None, Limits::default());
    let monitor = Monitor::default();
    let listing = foreign::list_file(&archive, None, &opts, &monitor).unwrap();
    assert_eq!(listing.format, fixture::FORMAT);
    let names: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(names, ["d", "d/a.txt", "d/big.bin", "empty", "d/copy.txt"]);
    let out = work.path().join("out");
    let sum = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &Selection::all(),
        &opts,
        &monitor,
    )
    .unwrap();
    assert_eq!((sum.files, sum.directories), (4, 1));
    // The test worker's reader stores no checksum: every member ended `checked: false`, and
    // the host counts them (the GUI's check line says "K had none to check").
    assert_eq!((sum.unchecked, sum.checked), (3, 0));
    for m in &members {
        if m.kind == RecordKind::File {
            let p = out.join(std::str::from_utf8(&m.name).unwrap());
            assert_eq!(std::fs::read(&p).unwrap(), m.data, "{}", p.display());
        }
    }
    assert_eq!(std::fs::read(out.join("d/copy.txt")).unwrap(), b"hello");
    // A selection, into a second directory.
    let out2 = work.path().join("out2");
    let sum = foreign::extract_file(
        &archive,
        &out2,
        None,
        &lpk_core::DefaultPolicy,
        &Selection::new(&["d/a.txt"]).unwrap(),
        &opts,
        &monitor,
    )
    .unwrap();
    assert_eq!(sum.files, 1);
    assert!(!out2.join("d/big.bin").exists());
}

#[test]
fn passwords_go_through_open_only() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write_fixture(work.path(), &[Member::file("a", b"x")], Some(b"pw"));
    let opts = s.options(None, Limits::default());
    let m = Monitor::default();
    let e = foreign::list_file(&archive, None, &opts, &m).unwrap_err();
    assert!(
        matches!(e, CoreError::Format(FormatError::PasswordRequired)),
        "{e}"
    );
    let wrong = Credentials::password(b"no".to_vec());
    let e = foreign::list_file(&archive, Some(&wrong), &opts, &m).unwrap_err();
    assert!(matches!(e, CoreError::Format(FormatError::WrongKey)), "{e}");
    let right = Credentials::password(b"pw".to_vec());
    assert_eq!(
        foreign::list_file(&archive, Some(&right), &opts, &m)
            .unwrap()
            .entries
            .len(),
        1
    );
}

/// Run one fault; the error and the elapsed time.
fn fault(name: &str, limits: Limits) -> (CoreError, Duration, PathBuf, tempfile::TempDir) {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write_fixture(work.path(), &members(), None);
    let out = work.path().join("out");
    let t = Instant::now();
    let e = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &Selection::all(),
        &s.options(Some(name), limits),
        &Monitor::default(),
    )
    .unwrap_err();
    (e, t.elapsed(), out, work)
}

#[test]
fn a_worker_answering_garbage_is_a_protocol_error() {
    let (e, took, out, _w) = fault("garbage", short());
    assert!(
        matches!(e, CoreError::Worker(WorkerError::Protocol { .. })),
        "{e}"
    );
    assert!(took < SLACK, "{took:?}");
    assert!(!out.exists());
}

#[test]
fn a_hanging_worker_times_out_within_the_limit() {
    let lim = short();
    let (e, took, out, _w) = fault("hang", lim.clone());
    assert!(
        matches!(e, CoreError::Worker(WorkerError::Timeout { .. })),
        "{e}"
    );
    assert!(took >= lim.no_progress, "{took:?}");
    assert!(took < lim.no_progress + SLACK, "{took:?}");
    assert!(!out.exists());
}

/// Empty `Data` frames move nothing forward: refused as a protocol error at once (and the
/// watchdog runs on every frame, so frames without progress cannot hold it off).
#[test]
fn a_worker_sending_empty_data_is_stopped() {
    let lim = short();
    let (e, took, out, _w) = fault("empty-data", lim.clone());
    assert!(
        matches!(e, CoreError::Worker(WorkerError::Protocol { .. })),
        "{e}"
    );
    assert!(took < lim.no_progress + SLACK, "{took:?}");
    assert!(!out.join("d/a.txt").exists());
}

/// A worker that stops reading requests (its `ReadMember` never read) cannot block the
/// broker: the request goes through a writer thread and the watchdog stops the worker.
#[test]
fn a_worker_not_reading_its_requests_times_out() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let many: Vec<Member> = (0..3000)
        .map(|i| Member::file(&format!("f{i:04}"), b"x"))
        .collect();
    let archive = write_fixture(work.path(), &many, None);
    let out = work.path().join("out");
    let lim = short();
    let t = Instant::now();
    let e = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &Selection::all(),
        &s.options(Some("hang-read-member"), lim.clone()),
        &Monitor::default(),
    )
    .unwrap_err();
    let took = t.elapsed();
    assert!(
        matches!(e, CoreError::Worker(WorkerError::Timeout { .. })),
        "{e}"
    );
    assert!(took < lim.no_progress + SLACK, "{took:?}");
    assert!(!out.join("f0000").exists());
}

#[test]
fn a_worker_closing_its_pipe_is_a_pipe_error() {
    let (e, took, out, _w) = fault("close", short());
    assert!(
        matches!(e, CoreError::Worker(WorkerError::Pipe { .. })),
        "{e}"
    );
    assert!(took < SLACK, "{took:?}");
    assert!(!out.exists());
}

#[test]
fn a_worker_paging_forever_hits_the_entries_cap() {
    let lim = Limits {
        max_entries: Some(1000),
        ..short()
    };
    let (e, took, out, _w) = fault("endless-more", lim);
    assert!(
        matches!(
            e,
            CoreError::Foreign(ForeignError::LimitExceeded {
                limit: "entries",
                ..
            })
        ),
        "{e}"
    );
    assert!(took < SLACK, "{took:?}");
    assert!(!out.exists());
}

#[test]
fn a_panicking_worker_is_named() {
    let (e, took, _out, _w) = fault("panic", short());
    match e {
        CoreError::Worker(WorkerError::Panicked { message }) => {
            assert!(message.contains("fault: panic at open"), "{message}")
        }
        other => panic!("{other}"),
    }
    assert!(took < SLACK, "{took:?}");
}

#[test]
fn cancelling_mid_stream_leaves_no_partial_file() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let big = vec![9u8; 4 << 20];
    let archive = write_fixture(work.path(), &[Member::file("big", &big)], None);
    let out = work.path().join("out");
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let progress = Arc::new(move |e: &lpk_core::Event| {
        if e.bytes_out > 0 {
            c.cancel();
        }
    });
    let monitor = Monitor::new(progress, cancel);
    let lim = short();
    let t = Instant::now();
    let e = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &Selection::all(),
        &s.options(Some("slow-data"), lim.clone()),
        &monitor,
    )
    .unwrap_err();
    assert!(matches!(e, CoreError::Cancelled), "{e}");
    assert!(t.elapsed() < lim.cancel_grace + SLACK, "{:?}", t.elapsed());
    assert!(out.is_dir(), "the directories stay");
    assert!(!out.join("big").exists(), "a partial file was left");
}

/// D-97 (4): a file a worker leaves in the AppContainer's shared temporary folder is gone at
/// the next launch.
#[cfg(windows)]
#[test]
fn the_appcontainer_temp_folder_is_emptied_before_each_launch() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write_fixture(work.path(), &[Member::file("a", b"x")], None);
    let temp = foreign::appcontainer_temp(s.profile.as_deref()).unwrap();
    let litter = temp.join("lpk-litter.txt");
    let m = Monitor::default();
    foreign::list_file(
        &archive,
        None,
        &s.options(Some("litter"), Limits::default()),
        &m,
    )
    .unwrap();
    assert!(
        litter.is_file(),
        "the litter fault left nothing at {}",
        litter.display()
    );
    foreign::list_file(&archive, None, &s.options(None, Limits::default()), &m).unwrap();
    assert!(
        !litter.exists(),
        "{} survived the next launch",
        litter.display()
    );
}

/// A directory junction made with `mklink /J` (no privilege needed).
#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let st = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(st.status.success(), "{st:?}");
}

/// The cleanup removes a junction a worker planted in `Temp` as a link: what it points to
/// (outside the sandbox's reach, the user's) is untouched.
#[cfg(windows)]
#[test]
fn a_junction_planted_in_the_temp_folder_is_not_followed() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write_fixture(work.path(), &[Member::file("a", b"x")], None);
    let m = Monitor::default();
    // A first launch creates the profile's folders.
    foreign::list_file(&archive, None, &s.options(None, Limits::default()), &m).unwrap();
    let temp = foreign::appcontainer_temp(s.profile.as_deref()).unwrap();
    std::fs::create_dir_all(&temp).unwrap();
    let victim = work.path().join("victim");
    std::fs::create_dir(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), b"precious").unwrap();
    let link = temp.join("planted");
    junction(&link, &victim);
    let listing =
        foreign::list_file(&archive, None, &s.options(None, Limits::default()), &m).unwrap();
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the junction stayed"
    );
    assert_eq!(std::fs::read(victim.join("keep.txt")).unwrap(), b"precious");
}

/// When `Temp` itself is a reparse point the cleanup refuses to run; that is a warning, and
/// the extraction goes on.
#[cfg(windows)]
#[test]
fn a_cleanup_that_cannot_run_is_a_warning() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write_fixture(work.path(), &[Member::file("a", b"x")], None);
    let m = Monitor::default();
    foreign::list_file(&archive, None, &s.options(None, Limits::default()), &m).unwrap();
    let temp = foreign::appcontainer_temp(s.profile.as_deref()).unwrap();
    let real = temp.with_file_name("Temp.real");
    if temp.exists() {
        std::fs::rename(&temp, &real).unwrap();
    } else {
        std::fs::create_dir_all(&real).unwrap();
    }
    let victim = work.path().join("victim");
    std::fs::create_dir(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), b"precious").unwrap();
    junction(&temp, &victim);
    let out = work.path().join("out");
    let r = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &Selection::all(),
        &s.options(None, Limits::default()),
        &m,
    );
    // Undo the junction before anything else (the profile's deletion must not meet it).
    std::fs::remove_dir(&temp).unwrap();
    std::fs::rename(&real, &temp).unwrap();
    let sum = r.unwrap();
    assert_eq!(sum.files, 1);
    assert!(
        sum.warnings.iter().any(|w| w.contains("reparse point")),
        "{:?}",
        sum.warnings
    );
    assert_eq!(std::fs::read(victim.join("keep.txt")).unwrap(), b"precious");
}

/// An id selection numbered against `listing`, as the session builds it.
fn ids(listing: &foreign::Listing, picks: impl IntoIterator<Item = u32>) -> Selection {
    Selection::from_ids(picks, listing.entries.len() as u32).with_fingerprint(listing.fingerprint())
}

/// E5-4: an id set picks listing positions (`lpk_api::EntryRef::id` is the position in
/// `list_file`'s entries), not paths.
#[test]
fn an_id_set_picks_foreign_members_by_listing_position() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write_fixture(work.path(), &members(), None);
    let opts = s.options(None, Limits::default());
    let monitor = Monitor::default();
    let listing = foreign::list_file(&archive, None, &opts, &monitor).unwrap();
    let id_of = |name: &str| listing.entries.iter().position(|e| e.path == name).unwrap() as u32;
    let out = work.path().join("out");
    let sum = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &ids(&listing, [id_of("d/a.txt"), id_of("empty")]),
        &opts,
        &monitor,
    )
    .unwrap();
    assert_eq!(sum.files, 2);
    assert_eq!(std::fs::read(out.join("d/a.txt")).unwrap(), b"hello");
    assert!(out.join("empty").is_file());
    assert!(!out.join("d/big.bin").exists() && !out.join("d/copy.txt").exists());
    // A member named with a backslash (a ZIP may): picking it by id selects it (a glob could
    // not spell it); the policy then refuses its name on every platform, while picking the
    // others leaves it alone.
    let odd = vec![
        Member::file(r"a\b.txt", b"odd"),
        Member::file("ok.txt", b"ok"),
    ];
    let archive = write_fixture(work.path(), &odd, None);
    let listing = foreign::list_file(&archive, None, &opts, &monitor).unwrap();
    let odd_id = listing
        .entries
        .iter()
        .position(|e| e.path == r"a\b.txt")
        .unwrap() as u32;
    let sel = ids(&listing, [odd_id]);
    assert!(sel.selects(odd_id as usize, r"a\b.txt"));
    let e = foreign::extract_file(
        &archive,
        &work.path().join("out4"),
        None,
        &lpk_core::DefaultPolicy,
        &sel,
        &opts,
        &monitor,
    )
    .unwrap_err();
    assert!(
        matches!(e, CoreError::Format(FormatError::UnsafePath { .. })),
        "{e}"
    );
    let ok_id = 1 - odd_id;
    let out5 = work.path().join("out5");
    foreign::extract_file(
        &archive,
        &out5,
        None,
        &lpk_core::DefaultPolicy,
        &ids(&listing, [ok_id]),
        &opts,
        &monitor,
    )
    .unwrap();
    assert_eq!(std::fs::read(out5.join("ok.txt")).unwrap(), b"ok");
    // Two members with one path: the listing numbers both, but extraction refuses the
    // archive (a second member would meet the first's file), whatever is picked.
    let dup = vec![
        Member::file("x.txt", b"one"),
        Member::file("y.txt", b"y"),
        Member::file("x.txt", b"two"),
    ];
    let archive = write_fixture(work.path(), &dup, None);
    let listing = foreign::list_file(&archive, None, &opts, &monitor).unwrap();
    assert_eq!(listing.entries.len(), 3, "ids 0 and 2 are both x.txt");
    let e = foreign::extract_file(
        &archive,
        &work.path().join("out3"),
        None,
        &lpk_core::DefaultPolicy,
        &ids(&listing, [2]),
        &opts,
        &monitor,
    )
    .unwrap_err();
    assert!(matches!(e, CoreError::DuplicatePath { .. }), "{e}");
}

/// E5-4 review: ids numbered against one archive are refused, before anything is written,
/// when the archive at the path was replaced by another.
#[test]
fn an_id_set_is_refused_when_the_archive_changed_since_the_listing() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let a = vec![Member::file("a.txt", b"A"), Member::file("b.txt", b"B")];
    let archive = write_fixture(work.path(), &a, None);
    let opts = s.options(None, Limits::default());
    let monitor = Monitor::default();
    let listing = foreign::list_file(&archive, None, &opts, &monitor).unwrap();
    let sel = ids(&listing, [0]);
    // Same count and names, one size differs: still another archive.
    let b = vec![Member::file("a.txt", b"AA"), Member::file("b.txt", b"B")];
    write_fixture(work.path(), &b, None);
    let out = work.path().join("out");
    let e = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &sel,
        &opts,
        &monitor,
    )
    .unwrap_err();
    assert!(
        matches!(e, CoreError::Foreign(ForeignError::ArchiveChanged)),
        "{e}"
    );
    assert!(!out.exists(), "nothing written");
    // A different member order: refused too.
    let c = vec![Member::file("b.txt", b"B"), Member::file("a.txt", b"A")];
    write_fixture(work.path(), &c, None);
    let e = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &sel,
        &opts,
        &monitor,
    )
    .unwrap_err();
    assert!(matches!(
        e,
        CoreError::Foreign(ForeignError::ArchiveChanged)
    ));
    assert!(!out.exists());
    // The unchanged archive extracts.
    write_fixture(work.path(), &a, None);
    foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &sel,
        &opts,
        &monitor,
    )
    .unwrap();
    assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"A");
    assert!(!out.join("b.txt").exists());
}

/// E5-4 review: a foreign listing may put a directory after its children; the id numbers are
/// positions, and the api's closure (every entry under a picked directory) is by path. Here the
/// engine's side: the picked ids select exactly those positions, children included.
#[test]
fn an_id_set_with_a_directory_after_its_children_extracts_what_it_names() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let m = vec![
        Member::file("d/x.txt", b"x"),
        Member::file("d/y.txt", b"y"),
        Member::dir("d"),
        Member::file("z.txt", b"z"),
    ];
    let archive = write_fixture(work.path(), &m, None);
    let opts = s.options(None, Limits::default());
    let monitor = Monitor::default();
    let listing = foreign::list_file(&archive, None, &opts, &monitor).unwrap();
    let names: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(names, ["d/x.txt", "d/y.txt", "d", "z.txt"]);
    // The directory (id 2) and its closure under it (ids 0, 1), as `close_ids` builds it.
    let out = work.path().join("out");
    let sum = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &ids(&listing, [0, 1, 2]),
        &opts,
        &monitor,
    )
    .unwrap();
    assert_eq!((sum.files, sum.directories), (2, 1));
    assert_eq!(std::fs::read(out.join("d/y.txt")).unwrap(), b"y");
    assert!(!out.join("z.txt").exists());
}

fn read_one(
    archive: &Path,
    id: u32,
    fp: Option<&lpk_core::Fingerprint>,
    cap: u64,
    opts: &Options,
    m: &Monitor,
) -> Result<Vec<u8>, CoreError> {
    foreign::read_member_file(archive, id, fp, cap, None, opts, m)
}

fn cap_refused(e: &CoreError, cap: u64) -> bool {
    matches!(
        e,
        CoreError::Format(FormatError::Refused(r)) if r.field == "preview cap" && r.allowed == cap
    )
}

/// E5-6: one member by listing position, read exactly, nothing written beside the archive.
#[test]
fn one_member_is_read_by_listing_position_with_the_fingerprint_checked() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let ms = members();
    let archive = write_fixture(work.path(), &ms, None);
    let opts = s.options(None, Limits::default());
    let m = Monitor::default();
    let listing = foreign::list_file(&archive, None, &opts, &m).unwrap();
    let fp = listing.fingerprint();
    assert_eq!(
        read_one(&archive, 1, Some(&fp), 100, &opts, &m).unwrap(),
        b"hello"
    );
    let big = read_one(&archive, 2, Some(&fp), 1 << 20, &opts, &m).unwrap();
    assert_eq!(big, ms[2].data);
    assert_eq!(read_one(&archive, 3, Some(&fp), 0, &opts, &m).unwrap(), b"");
    // Exactly at the cap is fine; one under is refused naming the cap.
    let n = ms[2].data.len() as u64;
    assert!(read_one(&archive, 2, Some(&fp), n, &opts, &m).is_ok());
    let e = read_one(&archive, 2, Some(&fp), n - 1, &opts, &m).unwrap_err();
    assert!(cap_refused(&e, n - 1), "{e}");
    // A directory, a hard link, and a position past the end are refused.
    for id in [0, 4, 9] {
        let e = read_one(&archive, id, Some(&fp), 1 << 20, &opts, &m).unwrap_err();
        assert!(
            matches!(e, CoreError::Foreign(ForeignError::Unsupported { .. })),
            "{id}: {e}"
        );
    }
    let names: Vec<_> = std::fs::read_dir(work.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
}

/// A declared size over the cap is refused before a `ReadMember` goes out: a worker that
/// would hang on one never gets the chance.
#[test]
fn a_member_declared_over_the_cap_is_refused_before_any_data_is_read() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write_fixture(work.path(), &members(), None);
    let lim = short();
    let opts = s.options(Some("hang-read-member"), lim.clone());
    let t = Instant::now();
    let e = read_one(&archive, 2, None, 1000, &opts, &Monitor::default()).unwrap_err();
    assert!(cap_refused(&e, 1000), "{e}");
    assert!(t.elapsed() < lim.no_progress + SLACK, "{:?}", t.elapsed());
}

#[test]
fn a_changed_archive_is_refused_before_any_member_data_is_read() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write_fixture(work.path(), &members(), None);
    let opts = s.options(None, Limits::default());
    let m = Monitor::default();
    let fp = foreign::list_file(&archive, None, &opts, &m)
        .unwrap()
        .fingerprint();
    write_fixture(
        work.path(),
        &[Member::file("other", b"hello"), Member::file("x", b"y")],
        None,
    );
    let hang = s.options(Some("hang-read-member"), short());
    let t = Instant::now();
    let e = read_one(&archive, 0, Some(&fp), 100, &hang, &m).unwrap_err();
    assert!(
        matches!(e, CoreError::Foreign(ForeignError::ArchiveChanged)),
        "{e}"
    );
    assert!(t.elapsed() < short().no_progress + SLACK);
}

#[test]
fn cancelling_a_member_read_ends_the_worker_promptly() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let big = vec![9u8; 4 << 20];
    let archive = write_fixture(work.path(), &[Member::file("big", &big)], None);
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c.cancel();
    });
    let lim = short();
    let t = Instant::now();
    let e = read_one(
        &archive,
        0,
        None,
        8 << 20,
        &s.options(Some("slow-data"), lim.clone()),
        &Monitor::with_cancel(cancel),
    )
    .unwrap_err();
    h.join().unwrap();
    assert!(matches!(e, CoreError::Cancelled), "{e}");
    assert!(t.elapsed() < lim.cancel_grace + SLACK, "{:?}", t.elapsed());
}
