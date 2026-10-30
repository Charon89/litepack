//! E5-6's command layer (`docs/notes/e5-design.md` sections 2 and 4): the capped read (16 MiB
//! refused naming the cap, exactly 16 MiB previewed), the classification of what was read, a
//! superseded preview cancelled, an encrypted entry asking for its password through a
//! `Prompter`, a folder's count, and a foreign archive giving the API's message.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use lpk_api::testing::archive_with;
use lpk_api::{
    CancelToken, Class, CreateSettings, Credentials, Monitor, Seal, Sources, Suite, Tier,
};
use lpk_gui::app::App;
use lpk_gui::extract::{NoPrompt, Prompter};
use lpk_gui::grants::GrantKind;
use lpk_gui::preview::{Kind, Preview, PREVIEW_CAP};
use zeroize::Zeroizing;

struct Open {
    app: App,
    session: u64,
    generation: u32,
}

fn open(archive: &Path) -> Open {
    let app = App::new(None);
    let g = app.grants.mint(GrantKind::Archive, archive).grant;
    let o = app.open(&g, &CancelToken::new()).unwrap();
    Open {
        app,
        session: o.session,
        generation: o.view.generation,
    }
}

impl Open {
    fn row_of(&self, name: &str) -> u32 {
        let (_, _, rows) =
            lpk_gui::batch::decode(&self.app.rows(self.session, 0, 100).unwrap()).unwrap();
        rows.iter().position(|r| r.name == name).unwrap() as u32
    }

    fn preview(&self, name: &str) -> Result<Preview, lpk_gui::GuiError> {
        self.preview_with(name, &CancelToken::new(), &NoPrompt)
    }

    fn preview_with(
        &self,
        name: &str,
        token: &CancelToken,
        prompter: &dyn Prompter,
    ) -> Result<Preview, lpk_gui::GuiError> {
        self.app.preview(
            self.session,
            self.row_of(name),
            self.generation,
            token,
            prompter,
            true,
        )
    }

    /// A read that rested on the row: never prompts.
    fn rest(&self, name: &str, prompter: &dyn Prompter) -> Result<Preview, lpk_gui::GuiError> {
        self.app.preview(
            self.session,
            self.row_of(name),
            self.generation,
            &CancelToken::new(),
            prompter,
            false,
        )
    }
}

#[test]
fn an_entry_over_the_cap_is_refused_naming_it_and_exactly_the_cap_is_previewed() {
    assert_eq!(PREVIEW_CAP, 16 * 1024 * 1024);
    let d = tempfile::tempdir().unwrap();
    let big = vec![b'a'; PREVIEW_CAP as usize + 1];
    let exact = vec![b'b'; PREVIEW_CAP as usize];
    let a = d.path().join("a.lpk");
    archive_with(&a, &[("big.txt", Some(&big)), ("exact.txt", Some(&exact))]).unwrap();
    let o = open(&a);
    let e = o.preview("big.txt").unwrap_err();
    assert!(e.is(Class::TooLarge), "{e}");
    let text = e.to_string();
    assert!(
        text.contains("preview cap") && text.contains(&PREVIEW_CAP.to_string()),
        "{text}"
    );
    let p = o.preview("exact.txt").unwrap();
    assert_eq!((p.kind, p.size), (Kind::Text, PREVIEW_CAP));
    assert_eq!(p.body.len(), PREVIEW_CAP as usize);
}

#[test]
fn each_kind_is_classified_from_the_content_not_the_name() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a.lpk");
    let bidi = format!("a\u{202E}b{}", "z".repeat(200));
    archive_with(
        &a,
        &[
            ("docs", None),
            ("docs/x.txt", Some(b"x")),
            ("docs/y.txt", Some(b"y")),
            ("pic.png", Some(b"\x89PNG\r\n\x1a\nrest")),
            (
                "evil.png",
                Some(b"<html><script>window.x=1</script></html>"),
            ),
            ("evil.svg", Some(b"<svg><script>window.x=1</script></svg>")),
            ("bin.dat", Some(b"\x00\x01\x02")),
            ("bidi.txt", Some(bidi.as_bytes())),
        ],
    )
    .unwrap();
    let o = open(&a);
    let p = o.preview("pic.png").unwrap();
    assert_eq!((p.kind, p.mime), (Kind::Image, "image/png"));
    for name in ["evil.png", "evil.svg"] {
        assert_eq!(o.preview(name).unwrap().kind, Kind::Text, "{name}");
    }
    assert_eq!(o.preview("bin.dat").unwrap().kind, Kind::Hex);
    assert!(o
        .preview("bidi.txt")
        .unwrap()
        .body
        .starts_with(b"a\\u{202E}b"));
    let f = o.preview("docs").unwrap();
    assert_eq!((f.kind, f.size, f.body.len()), (Kind::Folder, 2, 0));
    // A stale view is refused.
    let e = o
        .app
        .preview(
            o.session,
            0,
            o.generation + 1,
            &CancelToken::new(),
            &NoPrompt,
            true,
        )
        .unwrap_err();
    assert!(e.is(Class::Usage), "{e}");
}

fn sealed(dir: &Path) -> PathBuf {
    let src = dir.join("sealsrc");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("secret.txt"), b"top secret").unwrap();
    let out = dir.join("sealed.lpk");
    let s = CreateSettings {
        tier: Tier::Store,
        seal: Some(Seal {
            suite: Suite::XChaCha,
            listable: true,
            credentials: Credentials::password(b"hunter2".to_vec()),
        }),
        ..CreateSettings::default()
    };
    lpk_api::create(&Sources::contents_of(&src), &out, &s, &Monitor::default()).unwrap();
    out
}

struct Answers(Mutex<Vec<(Option<&'static str>, bool)>>);

impl Prompter for Answers {
    fn password(&self, _: &str, wrong: bool) -> Option<Zeroizing<String>> {
        let mut q = self.0.lock().unwrap();
        q.push((None, wrong));
        match q.len() {
            1 => Some(Zeroizing::new("nope".into())),
            _ => Some(Zeroizing::new("hunter2".into())),
        }
    }
}

#[test]
fn a_rest_on_a_locked_entry_never_prompts_and_unlock_asks_once() {
    let d = tempfile::tempdir().unwrap();
    let o = open(&sealed(d.path()));
    let p = Answers(Mutex::new(Vec::new()));
    for _ in 0..3 {
        let got = o.rest("secret.txt", &p).unwrap();
        assert_eq!((got.kind, got.body.len()), (Kind::Locked, 0));
    }
    assert!(p.0.lock().unwrap().is_empty(), "no prompt from focus alone");
    // Unlock: the prompt (wrong, then right) is the user's action.
    let got = o
        .preview_with("secret.txt", &CancelToken::new(), &p)
        .unwrap();
    assert_eq!(got.body, b"top secret");
    let asks = p.0.lock().unwrap().len();
    assert_eq!(asks, 2);
    // Unlocked: a rest reads it, and nothing more is asked.
    assert_eq!(o.rest("secret.txt", &p).unwrap().body, b"top secret");
    assert_eq!(p.0.lock().unwrap().len(), asks);
}

#[test]
fn the_session_is_swapped_only_when_the_cell_still_holds_the_one_read() {
    use std::sync::{Arc, Mutex};
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a.lpk");
    archive_with(&a, &[("a.txt", Some(b"alpha"))]).unwrap();
    let open_one = || lpk_api::Session::open(&a, &Default::default()).unwrap();
    let first = Arc::new(open_one());
    let cell = Mutex::new(first.clone());
    // The cell holds what was read: swapped.
    let now = lpk_gui::extract::swap_unlocked(&cell, &first, open_one());
    assert!(!Arc::ptr_eq(&now, &first));
    assert!(Arc::ptr_eq(&cell.lock().unwrap(), &now));
    // Another reader swapped first: the stale `first` no longer matches, the cell is kept.
    let kept = lpk_gui::extract::swap_unlocked(&cell, &first, open_one());
    assert!(Arc::ptr_eq(&kept, &now));
    assert!(Arc::ptr_eq(&cell.lock().unwrap(), &now));
    // The kept session still reads (it was not closed).
    assert_eq!(
        now.read_entry(0, 100, &CancelToken::new()).unwrap(),
        b"alpha"
    );
}

#[test]
fn text_with_many_hidden_characters_is_shown_as_hex() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a.lpk");
    let ones = vec![1u8; PREVIEW_CAP as usize];
    // 200 characters: 2 hidden is 1%, 3 is over.
    let mut at = "x".repeat(198);
    at.push_str("\u{202E}\u{202E}");
    let mut over = "x".repeat(197);
    over.push_str("\u{202E}\u{202E}\u{202E}");
    archive_with(
        &a,
        &[
            ("ones.txt", Some(&ones)),
            ("at.txt", Some(at.as_bytes())),
            ("over.txt", Some(over.as_bytes())),
        ],
    )
    .unwrap();
    let o = open(&a);
    let p = o.preview("ones.txt").unwrap();
    assert_eq!((p.kind, p.body.len()), (Kind::Hex, ones.len()));
    assert_eq!(o.preview("at.txt").unwrap().kind, Kind::Text);
    assert_eq!(o.preview("over.txt").unwrap().kind, Kind::Hex);
}

#[test]
fn an_encrypted_entry_asks_for_the_password_once_and_stays_unlocked() {
    let d = tempfile::tempdir().unwrap();
    let o = open(&sealed(d.path()));
    let e = o.preview("secret.txt").unwrap_err();
    assert!(e.is(Class::PasswordRequired), "{e}");
    let p = Answers(Mutex::new(Vec::new()));
    let got = o
        .preview_with("secret.txt", &CancelToken::new(), &p)
        .unwrap();
    assert_eq!(got.body, b"top secret");
    // Wrong, then right: the second ask says the first was wrong.
    let asked: Vec<bool> = p.0.lock().unwrap().iter().map(|a| a.1).collect();
    assert_eq!(asked, vec![false, true]);
    // Unlocked: no further question.
    assert_eq!(o.preview("secret.txt").unwrap().body, b"top secret");
}

/// Blocks in the password prompt until its token is cancelled.
struct Hang {
    token: CancelToken,
    asked: AtomicBool,
}

impl Prompter for Hang {
    fn password(&self, _: &str, _: bool) -> Option<Zeroizing<String>> {
        self.asked.store(true, Ordering::SeqCst);
        while !self.token.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        None
    }
}

#[test]
fn a_newer_preview_cancels_the_one_before_it() {
    let d = tempfile::tempdir().unwrap();
    let locked = open(&sealed(d.path()));
    let plain = d.path().join("plain.lpk");
    archive_with(&plain, &[("a.txt", Some(b"alpha"))]).unwrap();
    let app = &locked.app;
    let g = app.grants.mint(GrantKind::Archive, &plain).grant;
    let o2 = app.open(&g, &CancelToken::new()).unwrap();
    let first = CancelToken::new();
    let hang = Hang {
        token: first.clone(),
        asked: AtomicBool::new(false),
    };
    std::thread::scope(|s| {
        let h = s.spawn(|| locked.preview_with("secret.txt", &first, &hang));
        while !hang.asked.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // The second preview supersedes the first, which ends Cancelled; the second reads.
        let p = app
            .preview(
                o2.session,
                0,
                o2.view.generation,
                &CancelToken::new(),
                &NoPrompt,
                true,
            )
            .unwrap();
        assert_eq!(p.body, b"alpha");
        let e = h.join().unwrap().unwrap_err();
        assert!(e.is(Class::Cancelled), "{e}");
    });
    assert!(first.is_cancelled());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

/// A stored ZIP of one file.
fn zip(path: &Path, name: &str, data: &[u8]) {
    let mut out = Vec::new();
    let crc = crc32(data);
    let n = name.len() as u16;
    out.extend_from_slice(&[0x50, 0x4B, 3, 4, 20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(data);
    let cd = out.len() as u32;
    out.extend_from_slice(&[0x50, 0x4B, 1, 2, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&[0; 12]);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(name.as_bytes());
    let cd_len = out.len() as u32 - cd;
    out.extend_from_slice(&[0x50, 0x4B, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0]);
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    std::fs::write(path, out).unwrap();
}

#[test]
fn a_foreign_archive_gives_the_apis_answer() {
    let d = tempfile::tempdir().unwrap();
    let z = d.path().join("a.zip");
    zip(&z, "hello.txt", b"hello");
    // Listing a foreign archive runs in the sandboxed worker; a test binary without it beside
    // it (a plain `cargo test`) cannot list one, and the API's own answer is not reachable.
    let app = App::new(None);
    let g = app.grants.mint(GrantKind::Archive, &z).grant;
    let opened = match app.open(&g, &CancelToken::new()) {
        Ok(o) => o,
        // The worker is missing (Class::Sandbox): not reachable here. Any other error is a bug.
        Err(e) if e.is(Class::Sandbox) => {
            eprintln!("no sandboxed worker beside the test binary: foreign preview not exercised");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let o = Open {
        app,
        session: opened.session,
        generation: opened.view.generation,
    };
    // The member's bytes, read through the worker (part 1 of E5-6).
    let p = o.preview("hello.txt").unwrap();
    assert_eq!((p.kind, p.body.as_slice()), (Kind::Text, &b"hello"[..]));
}
