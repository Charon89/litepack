//! E5-4's command layer, per path (`docs/notes/e5-design.md` section 7): each destination with
//! All and with picked ids, each conflict answer, the password (right, wrong then right,
//! cancelled), the envelope refusal and "Allow", cancel, entry errors, the verbs queueing
//! several archives, the free-space refusal (injected) and the recents.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lpk_api::testing::archive_with;
use lpk_api::{
    CancelToken, Class, CreateSettings, Credentials, Monitor, Seal, Sources, Suite, Tier,
};
use lpk_gui::app::App;
use lpk_gui::browse::SelectAction;
use lpk_gui::extract::{
    Check, Conflict, Dest, ExtractResult, NoPrompt, Outcome, Prompter, Request, Scope, Verb,
    DISK_FIELD,
};
use lpk_gui::grants::{GrantKind, Granted};
use lpk_gui::pick::{Picker, Purpose};
use lpk_gui::GuiError;
use zeroize::Zeroizing;

const ENTRIES: &[(&str, Option<&[u8]>)] = &[
    ("a.txt", Some(b"alpha")),
    ("docs", None),
    ("docs/b.txt", Some(b"bravo")),
    ("docs/deep", None),
    ("docs/deep/c.txt", Some(b"charlie")),
];

fn plain(dir: &Path, name: &str, entries: &[(&str, Option<&[u8]>)]) -> PathBuf {
    let p = dir.join(name);
    archive_with(&p, entries).unwrap();
    p
}

/// An encrypted archive of one file; `listable` leaves the names in clear (the keyless open).
fn sealed(dir: &Path, listable: bool) -> PathBuf {
    let src = dir.join("sealsrc");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("secret.txt"), b"top secret").unwrap();
    let out = dir.join("sealed.lpk");
    let s = CreateSettings {
        tier: Tier::Store,
        seal: Some(Seal {
            suite: Suite::XChaCha,
            listable,
            credentials: Credentials::password(b"hunter2".to_vec()),
        }),
        ..CreateSettings::default()
    };
    lpk_api::create(&Sources::contents_of(&src), &out, &s, &Monitor::default()).unwrap();
    out
}

struct Opened {
    app: App,
    session: u64,
    generation: u32,
}

fn open(app: App, archive: &Path) -> Opened {
    let g = app.grants.mint(GrantKind::Archive, archive).grant;
    let o = app.open(&g, &CancelToken::new()).unwrap();
    Opened {
        app,
        session: o.session,
        generation: o.view.generation,
    }
}

impl Opened {
    fn pick(&self, name: &str) {
        let (_, _, rows) =
            lpk_gui::batch::decode(&self.app.rows(self.session, 0, 100).unwrap()).unwrap();
        let row = rows.iter().position(|r| r.name == name).unwrap() as u32;
        self.app
            .select(
                self.session,
                SelectAction::Toggle,
                Some(row),
                self.generation,
            )
            .unwrap();
    }

    fn run(&self, dest: Dest, scope: Scope, conflict: Conflict) -> Result<ExtractResult, GuiError> {
        self.run_with(dest, scope, conflict, None, &NoPrompt, &Monitor::default())
    }

    fn run_with(
        &self,
        dest: Dest,
        scope: Scope,
        conflict: Conflict,
        memory: Option<u64>,
        prompter: &dyn Prompter,
        monitor: &Monitor,
    ) -> Result<ExtractResult, GuiError> {
        self.app.extract_run(
            self.session,
            &Request {
                dest,
                scope,
                conflict,
                memory,
            },
            prompter,
            monitor,
        )
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn smart_puts_several_items_in_a_new_folder_and_a_lone_item_beside() {
    let d = tempfile::tempdir().unwrap();
    let a = plain(d.path(), "a.lpk", ENTRIES);
    let o = open(App::new(None), &a);
    let r = o.run(Dest::Smart, Scope::All, Conflict::Refuse).unwrap();
    assert_eq!(r.folder, "a");
    assert_eq!(read(&d.path().join("a/docs/deep/c.txt")), "charlie");
    assert_eq!(r.check, Some(Check::Blake3));
    // A second Smart finds `a` taken and makes `a (2)`: nothing existing is touched.
    let r = o.run(Dest::Smart, Scope::All, Conflict::Refuse).unwrap();
    assert_eq!(r.folder, "a (2)");
    // A lone top-level item whose name is free goes straight beside the archive.
    let lone = plain(
        d.path(),
        "lone.lpk",
        &[("solo", None), ("solo/x.txt", Some(b"x"))],
    );
    let o = open(App::new(None), &lone);
    o.run(Dest::Smart, Scope::All, Conflict::Refuse).unwrap();
    assert_eq!(read(&d.path().join("solo/x.txt")), "x");
    assert!(!d.path().join("lone").exists());
}

#[test]
fn new_folder_here_and_a_chosen_folder_with_all_and_with_picks() {
    let d = tempfile::tempdir().unwrap();
    let a = plain(d.path(), "a.lpk", ENTRIES);
    let o = open(App::new(None), &a);
    // New folder with a pick of `a.txt` alone.
    o.pick("a.txt");
    let r = o
        .run(Dest::NewFolder, Scope::Selected, Conflict::Refuse)
        .unwrap();
    assert_eq!((r.folder.as_str(), r.files), ("a", 1));
    assert!(d.path().join("a/a.txt").is_file());
    assert!(!d.path().join("a/docs").exists());
    // This folder: the archive's own, everything.
    let r = o.run(Dest::Here, Scope::All, Conflict::Refuse).unwrap();
    assert_eq!(r.files, 3);
    assert_eq!(read(&d.path().join("docs/b.txt")), "bravo");
    // A folder the user chose (a grant from the native dialog), with picks.
    let out = d.path().join("chosen");
    std::fs::create_dir(&out).unwrap();
    let grant = o.app.grants.mint(GrantKind::Folder, &out).grant;
    let r = o
        .run(Dest::Folder { grant }, Scope::Selected, Conflict::Refuse)
        .unwrap();
    assert_eq!(r.files, 1);
    assert_eq!(read(&out.join("a.txt")), "alpha");
    // An archive grant is not a destination.
    let wrong = o.app.grants.mint(GrantKind::Archive, &out).grant;
    let e = o
        .run(Dest::Folder { grant: wrong }, Scope::All, Conflict::Refuse)
        .unwrap_err();
    assert!(e.is(Class::Usage), "{e}");
    // Nothing picked under Selected is refused, not read as "all".
    let none = open(App::new(None), &a);
    assert!(none
        .run(Dest::NewFolder, Scope::Selected, Conflict::Refuse)
        .is_err());
}

#[test]
fn picks_the_filter_hides_are_left_out_and_say_so() {
    let d = tempfile::tempdir().unwrap();
    let a = plain(d.path(), "a.lpk", ENTRIES);
    let o = open(App::new(None), &a);
    o.pick("a.txt");
    o.pick("docs/b.txt");
    let request = lpk_gui::browse::ViewRequest {
        mode: "flat".into(),
        dir: None,
        key: "name".into(),
        descending: false,
        filter: "b.txt".into(),
        glob: false,
    };
    let v = o
        .app
        .view(o.session, &request, &CancelToken::new())
        .unwrap();
    assert_eq!(
        v.status.picked, 1,
        "the status bar counts the picks inside the view"
    );
    let plan = o
        .app
        .extract_plan(
            o.session,
            &Dest::NewFolder,
            Scope::Selected,
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!((plan.files, plan.left_out), (1, 1));
    let r = o
        .run(Dest::NewFolder, Scope::Selected, Conflict::Refuse)
        .unwrap();
    assert_eq!((r.files, r.left_out), (1, 1));
    assert!(d.path().join("a/docs/b.txt").is_file());
    assert!(!d.path().join("a/a.txt").exists());
}

#[test]
fn each_conflict_answer_against_existing_targets() {
    let d = tempfile::tempdir().unwrap();
    let a = plain(d.path(), "a.lpk", ENTRIES);
    let o = open(App::new(None), &a);
    o.run(Dest::Here, Scope::All, Conflict::Refuse).unwrap();
    std::fs::write(d.path().join("a.txt"), "mine").unwrap();
    // The preflight counts what exists, before any write.
    let plan = o
        .app
        .extract_plan(o.session, &Dest::Here, Scope::All, &CancelToken::new())
        .unwrap();
    assert!(plan.existing >= 3, "{plan:?}");
    // No answer: refused, nothing changed.
    let e = o.run(Dest::Here, Scope::All, Conflict::Refuse).unwrap_err();
    assert!(e.is(Class::Exists), "{e}");
    assert_eq!(read(&d.path().join("a.txt")), "mine");
    // Skip leaves the file.
    let r = o.run(Dest::Here, Scope::All, Conflict::Skip).unwrap();
    assert!(r.skipped >= 3);
    assert_eq!(read(&d.path().join("a.txt")), "mine");
    // Keep both writes beside it.
    let r = o.run(Dest::Here, Scope::All, Conflict::Rename).unwrap();
    assert!(r.renamed >= 3);
    assert_eq!(read(&d.path().join("a.txt")), "mine");
    assert_eq!(read(&d.path().join("a (1).txt")), "alpha");
    // Replace overwrites.
    o.run(Dest::Here, Scope::All, Conflict::Replace).unwrap();
    assert_eq!(read(&d.path().join("a.txt")), "alpha");
    // Smart and New folder never meet an existing target: no question.
    let plan = o
        .app
        .extract_plan(o.session, &Dest::Smart, Scope::All, &CancelToken::new())
        .unwrap();
    assert_eq!(plan.existing, 0);
}

/// Answers from a queue and records what it was asked.
struct Answers {
    queue: Mutex<VecDeque<Option<&'static str>>>,
    asked: Mutex<Vec<(String, bool)>>,
}

impl Answers {
    fn new(a: &[Option<&'static str>]) -> Self {
        Answers {
            queue: Mutex::new(a.iter().copied().collect()),
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl Prompter for Answers {
    fn password(&self, archive: &str, wrong: bool) -> Option<Zeroizing<String>> {
        self.asked
            .lock()
            .unwrap()
            .push((archive.to_string(), wrong));
        self.queue
            .lock()
            .unwrap()
            .pop_front()
            .flatten()
            .map(|s| Zeroizing::new(s.to_string()))
    }
}

#[test]
fn a_password_for_encrypted_names_wrong_then_right_and_cancelled() {
    let d = tempfile::tempdir().unwrap();
    let a = sealed(d.path(), false);
    let app = App::new(None);
    let g = app.grants.mint(GrantKind::Archive, &a).grant;
    // Without a prompt the archive stays refused.
    let e = app.open(&g, &CancelToken::new()).unwrap_err();
    assert!(e.is(Class::PasswordRequired), "{e}");
    // Cancel ends the open.
    let cancel = Answers::new(&[None]);
    let e = app.open_with(&g, &CancelToken::new(), &cancel).unwrap_err();
    assert!(e.is(Class::Cancelled), "{e}");
    // Wrong, then right: asked twice, the second time saying the first was wrong.
    let p = Answers::new(&[Some("nope"), Some("hunter2")]);
    let o = app.open_with(&g, &CancelToken::new(), &p).unwrap();
    assert_eq!(o.total, 1);
    let asked = p.asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        vec![
            ("sealed.lpk".to_string(), false),
            ("sealed.lpk".to_string(), true)
        ]
    );
    // The session is unlocked: extraction asks nothing more.
    let r = app
        .extract_run(
            o.session,
            &Request {
                dest: Dest::NewFolder,
                scope: Scope::All,
                conflict: Conflict::Refuse,
                memory: None,
            },
            &NoPrompt,
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!(read(&d.path().join("sealed/secret.txt")), "top secret");
    assert_eq!(r.check, Some(Check::Blake3));
}

#[test]
fn encrypted_content_asks_at_extraction_and_cancel_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let a = sealed(d.path(), true);
    let o = open(App::new(None), &a);
    // Cancelled: no password, no write.
    let cancel = Answers::new(&[None]);
    let e = o
        .run_with(
            Dest::NewFolder,
            Scope::All,
            Conflict::Refuse,
            None,
            &cancel,
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!(e.outcome, Outcome::CancelledBeforeStart);
    assert!(!d.path().join("sealed").exists(), "nothing written");
    // Wrong then right.
    let p = Answers::new(&[Some("bad"), Some("hunter2")]);
    o.run_with(
        Dest::NewFolder,
        Scope::All,
        Conflict::Refuse,
        None,
        &p,
        &Monitor::default(),
    )
    .unwrap();
    let asked = p.asked.lock().unwrap().clone();
    assert!(!asked[0].1 && asked.last().unwrap().1, "{asked:?}");
    assert_eq!(read(&d.path().join("sealed/secret.txt")), "top secret");
    // Unlocked once, unlocked: a second run asks nothing.
    o.run(Dest::NewFolder, Scope::All, Conflict::Refuse)
        .unwrap();
}

#[test]
fn an_envelope_refusal_carries_its_figures_and_allow_passes_the_memory() {
    use lpk_api::Refusal;
    let figures = Refusal {
        field: "--memory",
        needed: 3 << 30,
        allowed: 1 << 30,
    };
    let e = lpk_api::Error::format(lpk_format_refused(figures), None);
    let g = GuiError::from(e);
    let r = g.refusal.clone().expect("the figures reach the page");
    assert_eq!(
        (r.field.as_str(), r.needed, r.allowed),
        ("--memory", 3 << 30, 1 << 30)
    );
    assert!(serde_json::to_string(&g)
        .unwrap()
        .contains("\"needed\":3221225472"));
    // "Allow X": the page's request carries X, for that run alone; the request type has no
    // persistent form, and a run without it uses the default.
    let req: Request = serde_json::from_str(
        r#"{"dest":{"kind":"smart"},"scope":"all","conflict":"refuse","memory":3221225472}"#,
    )
    .unwrap();
    assert_eq!(req.memory, Some(3 << 30));
    let d = tempfile::tempdir().unwrap();
    let a = plain(d.path(), "a.lpk", ENTRIES);
    let o = open(App::new(None), &a);
    o.run_with(
        Dest::NewFolder,
        Scope::All,
        Conflict::Refuse,
        req.memory,
        &NoPrompt,
        &Monitor::default(),
    )
    .unwrap();
    assert!(d.path().join("a/a.txt").is_file());
}

/// `FormatError::Refused` without naming `lpk-format` in this crate's manifest.
fn lpk_format_refused(r: lpk_api::Refusal) -> lpk_api::FormatError {
    lpk_api::FormatError::Refused(r)
}

#[test]
fn cancel_before_the_run_ends_cancelled_and_leaves_nothing() {
    let d = tempfile::tempdir().unwrap();
    let a = plain(d.path(), "a.lpk", ENTRIES);
    let o = open(App::new(None), &a);
    let token = CancelToken::new();
    token.cancel();
    let monitor = Monitor::new(std::sync::Arc::new(|_: &lpk_api::Event| {}), token);
    let e = o
        .run_with(
            Dest::NewFolder,
            Scope::All,
            Conflict::Refuse,
            None,
            &NoPrompt,
            &monitor,
        )
        .unwrap();
    assert_eq!(e.outcome, Outcome::CancelledBeforeStart);
    assert!(!d.path().join("a").exists());
}

#[test]
fn entry_errors_reach_the_page_each_with_its_reason() {
    let mut e = lpk_api::Error::new(Class::Damaged, "Damaged", "two entries failed", None);
    e.entries = vec![
        lpk_api::EntryError {
            path: "docs/b.txt".into(),
            kind: "Crc".into(),
            message: "checksum mismatch".into(),
        },
        lpk_api::EntryError {
            path: "a.txt".into(),
            kind: "Io".into(),
            message: "access denied".into(),
        },
    ];
    let g = GuiError::from(e);
    assert_eq!(g.entries.len(), 2);
    assert_eq!(
        (g.entries[0].path.as_str(), g.entries[0].reason.as_str()),
        ("docs/b.txt", "checksum mismatch")
    );
    let json = serde_json::to_string(&g).unwrap();
    assert!(json.contains("\"entries\"") && json.contains("access denied"));
    // An error without entries sends none.
    let plain_error = GuiError::usage("x");
    assert!(!serde_json::to_string(&plain_error)
        .unwrap()
        .contains("entries"));
}

#[test]
fn the_verbs_queue_several_archives_each_with_its_own_result() {
    let d = tempfile::tempdir().unwrap();
    let app = App::new(None);
    let archives: Vec<PathBuf> = (0..3)
        .map(|i| plain(d.path(), &format!("q{i}.lpk"), ENTRIES))
        .collect();
    let grants: Vec<_> = archives
        .iter()
        .map(|p| app.grants.mint(GrantKind::Archive, p).grant)
        .collect();
    // Queued through the operations table: one heavy operation at a time.
    let mut results = Vec::new();
    for (i, g) in grants.iter().enumerate() {
        let guard = app.ops.begin(i as u64, true).unwrap();
        let monitor = Monitor::new(
            std::sync::Arc::new(|_: &lpk_api::Event| {}),
            guard.token.clone(),
        );
        let verb = if i == 2 { Verb::To } else { Verb::Here };
        results.push(
            app.extract_archive(g, verb, None, &NoPrompt, &monitor)
                .unwrap(),
        );
    }
    let names: Vec<_> = results
        .iter()
        .map(|r| (r.archive.as_str(), r.folder.as_str()))
        .collect();
    assert_eq!(
        names,
        [("q0.lpk", "q0"), ("q1.lpk", "q1"), ("q2.lpk", "q2")]
    );
    assert!(results.iter().all(|r| r.files == 3));
    assert_eq!(read(&d.path().join("q1/docs/b.txt")), "bravo");
    // `--extract-to` again beside the same archive never reuses the folder.
    let again = app
        .extract_archive(&grants[2], Verb::To, None, &NoPrompt, &Monitor::default())
        .unwrap();
    assert_eq!(again.folder, "q2 (2)");
    // A sealed archive asks for its password in the verb too.
    let s = sealed(d.path(), false);
    let g = app.grants.mint(GrantKind::Archive, &s).grant;
    let p = Answers::new(&[Some("hunter2")]);
    let r = app
        .extract_archive(&g, Verb::Here, None, &p, &Monitor::default())
        .unwrap();
    assert_eq!(r.files, 1);
}

#[test]
fn not_enough_free_space_is_refused_before_anything_is_written() {
    let d = tempfile::tempdir().unwrap();
    let a = plain(d.path(), "a.lpk", ENTRIES);
    // The injected figure: 3 bytes free for 17 bytes of files.
    let o = open(App::new(None).with_free_space(3), &a);
    let e = o
        .app
        .extract_plan(o.session, &Dest::NewFolder, Scope::All, &CancelToken::new())
        .unwrap_err();
    assert!(e.is(Class::DiskFull), "{e}");
    let r = e.refusal.unwrap();
    assert_eq!((r.field.as_str(), r.needed, r.allowed), (DISK_FIELD, 17, 3));
    assert!(
        e.detail.contains("17") && e.detail.contains('3'),
        "{}",
        e.detail
    );
    let e = o
        .run(Dest::NewFolder, Scope::All, Conflict::Refuse)
        .unwrap_err();
    assert!(e.is(Class::DiskFull), "{e}");
    assert!(!d.path().join("a").exists());
    // The real volume measure is reported when nothing is injected.
    let o = open(App::new(None), &a);
    let plan = o
        .app
        .extract_plan(o.session, &Dest::NewFolder, Scope::All, &CancelToken::new())
        .unwrap();
    assert!(plan.free.is_none_or(|f| f > 17));
}

struct Fake {
    files: Vec<PathBuf>,
    folder: Option<PathBuf>,
}

impl Picker for Fake {
    fn files(&self, _: &str, _: bool) -> std::io::Result<Vec<PathBuf>> {
        Ok(self.files.clone())
    }
    fn folder(&self, _: &str) -> std::io::Result<Option<PathBuf>> {
        Ok(self.folder.clone())
    }
}

#[test]
fn recents_come_from_dialogs_verbs_and_drops_only() {
    use lpk_gui::activation::activate;
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("recents.json");
    let a = plain(d.path(), "a.lpk", ENTRIES);
    let b = plain(d.path(), "b.lpk", ENTRIES);
    let c = plain(d.path(), "c.lpk", ENTRIES);
    let out = d.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let app = App::new(None).with_recents(lpk_gui::recents::Recents::open(Some(file.clone())));
    assert!(app.recents().archives.is_empty());
    // A native Open dialog records the archive; an input folder and files do not.
    let fake = Fake {
        files: vec![a.clone()],
        folder: Some(out.clone()),
    };
    app.pick(&fake, Purpose::Archive).unwrap();
    app.pick(&fake, Purpose::Folder).unwrap();
    app.pick(&fake, Purpose::Files).unwrap();
    assert_eq!(
        app.recents().destinations.len(),
        0,
        "an input folder is no destination"
    );
    // The destination dialog records the folder.
    app.pick(&fake, Purpose::Destination).unwrap();
    // A verb on the command line and a drop record archives.
    let verb = lpk_gui::cli::parse(
        ["--extract-here", b.to_str().unwrap()]
            .iter()
            .map(std::ffi::OsString::from),
        Some(d.path()),
    )
    .unwrap()
    .verb
    .unwrap();
    app.note_activation(&activate(&verb, &app.grants));
    let dropped: Vec<Granted> = vec![app.grants.mint(GrantKind::Archive, &c)];
    app.note_drop(&dropped);
    // A grant derived for "extract here" is no recent.
    let o = open(App::new(None), &a);
    o.run(Dest::Smart, Scope::All, Conflict::Refuse).unwrap();
    assert!(o.app.recents().destinations.is_empty());

    let lists = app.recents();
    let kinds: Vec<_> = lists.archives.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(kinds, ["c.lpk", "b.lpk", "a.lpk"], "newest first");
    assert_eq!(lists.destinations.len(), 1);
    assert_eq!(lists.destinations[0].name, "out");
    // The grants are minted when listed, of the right kinds, and no path leaves Rust.
    let json = serde_json::to_string(&lists).unwrap();
    assert!(!json.contains(&*d.path().to_string_lossy().replace('\\', "\\\\")));
    assert!(app
        .grants
        .resolve(&lists.destinations[0].grant, GrantKind::Folder)
        .is_ok());
    assert!(app
        .grants
        .resolve(&lists.archives[0].grant, GrantKind::Archive)
        .is_ok());
    // They persist across a restart, and one command clears both.
    let again = App::new(None).with_recents(lpk_gui::recents::Recents::open(Some(file)));
    assert_eq!(again.recents().archives.len(), 3);
    again.clear_recents();
    let lists = again.recents();
    assert!(lists.archives.is_empty() && lists.destinations.is_empty());
}

#[test]
fn a_picked_folder_extracts_its_subtree_and_every_figure_agrees() {
    let d = tempfile::tempdir().unwrap();
    let a = plain(d.path(), "a.lpk", ENTRIES);
    let o = open(App::new(None), &a);
    o.pick("docs");
    // The file and its folder both picked: its bytes count once.
    let (_, _, rows) = lpk_gui::batch::decode(&o.app.rows(o.session, 0, 100).unwrap()).unwrap();
    let row = rows.iter().position(|r| r.name == "docs/b.txt").unwrap() as u32;
    let status = o
        .app
        .select(o.session, SelectAction::Toggle, Some(row), o.generation)
        .unwrap();
    assert_eq!(status.picked, 2, "the count stays the picked rows");
    assert_eq!(status.picked_bytes, 12, "b.txt and deep/c.txt, once each");
    let plan = o
        .app
        .extract_plan(
            o.session,
            &Dest::NewFolder,
            Scope::Selected,
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!((plan.files, plan.bytes), (2, status.picked_bytes));
    let r = o
        .run(Dest::NewFolder, Scope::Selected, Conflict::Refuse)
        .unwrap();
    assert_eq!((r.files, r.bytes), (2, 12));
    assert_eq!(read(&d.path().join("a/docs/deep/c.txt")), "charlie");
    assert!(!d.path().join("a/a.txt").exists());
}

#[test]
fn the_check_line_follows_what_the_members_reported() {
    use lpk_api::{Extracted, ForeignSummary};
    let zip = |files, unchecked| {
        Extracted::Foreign(ForeignSummary {
            format: "zip".into(),
            files,
            unchecked,
            checked: files - unchecked,
            ..ForeignSummary::default()
        })
    };
    assert_eq!(lpk_gui::extract::check_of(&zip(4, 0)), Some(Check::Members));
    assert_eq!(
        lpk_gui::extract::check_of(&zip(4, 1)),
        Some(Check::Partial {
            matched: 3,
            none: 1
        })
    );
    assert_eq!(
        lpk_gui::extract::check_of(&zip(0, 0)),
        None,
        "no file, no claim"
    );
    let tar = Extracted::Foreign(ForeignSummary {
        format: "tar".into(),
        files: 2,
        ..ForeignSummary::default()
    });
    assert_eq!(lpk_gui::extract::check_of(&tar), Some(Check::Unverifiable));
}

/// An archive of `files` incompressible files of `each` bytes, stored.
fn big(dir: &Path, files: usize, each: usize) -> PathBuf {
    let src = dir.join("bigsrc");
    std::fs::create_dir_all(&src).unwrap();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for i in 0..files {
        let body: Vec<u8> = (0..each)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        std::fs::write(src.join(format!("f{i:04}.bin")), body).unwrap();
    }
    let out = dir.join("big.lpk");
    let s = CreateSettings {
        tier: Tier::Store,
        ..CreateSettings::default()
    };
    lpk_api::create(&Sources::contents_of(&src), &out, &s, &Monitor::default()).unwrap();
    out
}

/// Calls `then` once two entries exist in `dir` (polled every millisecond, at most 60 s).
fn when_files_appear(dir: PathBuf, then: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        for _ in 0..60_000 {
            if std::fs::read_dir(&dir).is_ok_and(|d| d.count() >= 2) {
                then();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    });
}

fn cancel_when_files_appear(dir: PathBuf, token: CancelToken) -> Monitor {
    let t = token.clone();
    when_files_appear(dir, move || t.cancel());
    Monitor::with_cancel(token)
}

#[test]
fn a_cancel_mid_run_keeps_the_finished_files_and_removes_the_unfinished_one() {
    let d = tempfile::tempdir().unwrap();
    let a = big(d.path(), 300, 1024 * 1024);
    let o = open(App::new(None), &a);
    let monitor = cancel_when_files_appear(d.path().join("big"), CancelToken::new());
    let r = o
        .run_with(
            Dest::NewFolder,
            Scope::All,
            Conflict::Refuse,
            None,
            &NoPrompt,
            &monitor,
        )
        .unwrap();
    assert_eq!(r.outcome, Outcome::CancelledAfterStart, "{r:?}");
    assert_eq!(r.check, None, "a cancelled run claims no check");
    let written: Vec<_> = std::fs::read_dir(d.path().join("big"))
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    assert!(
        !written.is_empty() && written.len() < 300,
        "{}",
        written.len()
    );
    for f in &written {
        assert_eq!(
            f.metadata().unwrap().len(),
            1024 * 1024,
            "{:?}: only finished files remain",
            f.file_name()
        );
    }
}

#[test]
fn allow_x_reruns_with_a_larger_budget_for_that_run_alone() {
    let d = tempfile::tempdir().unwrap();
    // A `.lpk` holding a ZIP of compressible text: the Deflate record's decode reserves memory.
    let text = d.path().join("text");
    std::fs::create_dir_all(&text).unwrap();
    let line = "the quick brown fox jumps over the lazy dog 0123456789\n";
    std::fs::write(text.join("t.txt"), line.repeat(40_000)).unwrap();
    let t = d.path().join("t.lpk");
    let store = CreateSettings {
        tier: Tier::Store,
        ..CreateSettings::default()
    };
    lpk_api::create(
        &Sources::contents_of(&text),
        &t,
        &store,
        &Monitor::default(),
    )
    .unwrap();
    let zip_dir = d.path().join("zipdir");
    std::fs::create_dir_all(&zip_dir).unwrap();
    let session = lpk_api::Session::open(&t, &Default::default()).unwrap();
    session
        .export(
            &lpk_api::Selection::All,
            &zip_dir.join("t.zip"),
            &lpk_api::ExportOptions {
                container: lpk_api::Container::Zip,
                method: lpk_api::Method::Deflate,
                password: None,
                force: false,
                threads: None,
                memory: None,
            },
            &Monitor::default(),
        )
        .unwrap();
    let a = d.path().join("a.lpk");
    let peel = CreateSettings {
        tier: Tier::Balanced,
        deflate_peel: Some(true),
        ..CreateSettings::default()
    };
    lpk_api::create(
        &Sources::contents_of(&zip_dir),
        &a,
        &peel,
        &Monitor::default(),
    )
    .unwrap();
    // The test seam lowers the default budget to the floor; a real run refuses and names both
    // figures.
    //
    // The engine refuses a budget below what one block needs (an `InvalidOption` naming the
    // figure); a decode record's `Refused` needs a record larger than its block, which no
    // fixture here builds (E5-4 review: the figures' path is `an_envelope_refusal_carries...`).
    let o = open(App::new(None).with_memory_budget(470_000), &a);
    let e = o
        .run(Dest::NewFolder, Scope::All, Conflict::Refuse)
        .unwrap_err();
    assert!(e.detail.contains("--memory"), "{e}");
    assert!(!d.path().join("a").exists(), "nothing written");
    // Allow X: that run alone, with the larger budget, extracts; nothing is kept, so the next
    // run is refused again.
    let ok = o
        .run_with(
            Dest::NewFolder,
            Scope::All,
            Conflict::Refuse,
            Some(64 << 20),
            &NoPrompt,
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!(ok.files, 1);
    assert!(o
        .run(Dest::NewFolder, Scope::All, Conflict::Refuse)
        .is_err());
}

#[test]
fn the_exit_cancels_and_joins_a_run_in_progress() {
    let d = tempfile::tempdir().unwrap();
    let a = big(d.path(), 300, 1024 * 1024);
    let app = std::sync::Arc::new(App::new(None));
    let grant = app.grants.mint(GrantKind::Archive, &a).grant;
    let (tx, rx) = std::sync::mpsc::channel();
    when_files_appear(d.path().join("big"), move || {
        let _ = tx.send(());
    });
    let worker = {
        let app = app.clone();
        std::thread::spawn(move || {
            let g = app.ops.begin(1, true).unwrap();
            let monitor = Monitor::with_cancel(g.token.clone());
            app.extract_archive(&grant, Verb::To, None, &NoPrompt, &monitor)
        })
    };
    rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap();
    let t = std::time::Instant::now();
    assert!(
        app.shutdown(std::time::Duration::from_secs(30)),
        "the run was joined"
    );
    assert!(t.elapsed() < std::time::Duration::from_secs(30));
    let r = worker.join().unwrap().unwrap();
    assert_eq!(r.outcome, Outcome::CancelledAfterStart);
}

/// A prompter whose user never answers, as the host's waits.
struct Hang(CancelToken);

impl Prompter for Hang {
    fn password(&self, _: &str, _: bool) -> Option<Zeroizing<String>> {
        let (_keep_open, rx) = std::sync::mpsc::channel::<()>();
        lpk_gui::extract::wait_for_answer(&rx, &self.0).map(|()| Zeroizing::new(String::new()))
    }
}

#[test]
fn a_prompt_nobody_answers_ends_with_the_cancel_promptly() {
    let d = tempfile::tempdir().unwrap();
    let a = sealed(d.path(), true);
    let o = open(App::new(None), &a);
    let token = CancelToken::new();
    let t2 = token.clone();
    let monitor = Monitor::with_cancel(token.clone());
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        t2.cancel();
    });
    let t = std::time::Instant::now();
    let r = o
        .run_with(
            Dest::NewFolder,
            Scope::All,
            Conflict::Refuse,
            None,
            &Hang(token),
            &monitor,
        )
        .unwrap();
    assert!(
        t.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(r.outcome, Outcome::CancelledBeforeStart);
    assert!(!d.path().join("sealed").exists());
}

#[test]
fn a_really_damaged_archive_names_its_failure() {
    let d = tempfile::tempdir().unwrap();
    let a = big(d.path(), 1, 300_000);
    let mut bytes = std::fs::read(&a).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&a, bytes).unwrap();
    let o = open(App::new(None), &a);
    let e = o
        .run(Dest::NewFolder, Scope::All, Conflict::Refuse)
        .unwrap_err();
    eprintln!(
        "damaged: class={} kind={} detail={} entries={:?}",
        e.class, e.kind, e.detail, e.entries
    );
    assert!(e.is(Class::Damaged), "{e}");
    assert_eq!(e.entries.len(), 1, "the failing entry is listed");
    assert!(!e.entries[0].reason.is_empty());
}

#[test]
fn a_path_sent_through_settings_is_still_refused() {
    let r: Result<lpk_gui::settings::Settings, _> =
        serde_json::from_str(r#"{"update_check":true,"recent":["C:\\x.lpk"]}"#);
    assert!(r.is_err());
    let r: Result<lpk_gui::settings::Settings, _> =
        serde_json::from_str(r#"{"update_check":true,"destination":"C:\\x"}"#);
    assert!(r.is_err());
}
