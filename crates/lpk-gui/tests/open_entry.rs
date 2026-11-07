//! Opening an entry (E5-3; `docs/notes/e5-design.md` sections 4 and 7): `doc.pdf .cmd` and
//! `doc.pdf ` are refused before anything is written (`pdf ` is not `pdf`, and E3-2's rules
//! refuse the trailing space), so are names E3-2's path rules refuse (a colon, a device name);
//! the entry goes alone into a fresh folder (no sibling, no same-named folder); a request made
//! against an older view is refused; on Windows a zoned archive's zone reaches the opened file.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use lpk_api::testing::archive_with;
use lpk_api::{CancelToken, Monitor};
use lpk_gui::app::App;
use lpk_gui::browse::{SelectAction, ViewRequest, STALE_VIEW};
use lpk_gui::grants::GrantKind;
use lpk_gui::open::Opening;

const ENTRIES: &[(&str, Option<&[u8]>)] = &[
    ("doc", None),
    ("doc/inner.txt", Some(b"inner")),
    ("doc.pdf", Some(b"%PDF-1.7 body")),
    ("doc.pdf .cmd", Some(b"@echo off")),
    ("doc.pdf ", Some(b"%PDF trailing space")),
    ("other.txt", Some(b"other")),
];

/// Names E3-2's path rules refuse although their last extension is allow-listed.
const UNSAFE: &[&str] = &[
    "a.exe:x.pdf",
    "a.pdf::$DATA",
    "a.exe::$DATA.pdf",
    "CON.pdf",
    "nul.txt",
];

struct Open {
    app: App,
    session: u64,
    generation: u32,
    folder: PathBuf,
}

fn open_archive(dir: &Path, entries: &[(&str, Option<&[u8]>)]) -> Open {
    let archive = dir.join("a.lpk");
    archive_with(&archive, entries).unwrap();
    let folder = dir.join("Open");
    let app = App::new(None).with_open_dir(Some(folder.clone()));
    let g = app.grants.mint(GrantKind::Archive, &archive).grant;
    let o = app.open(&g, &CancelToken::new()).unwrap();
    Open {
        app,
        session: o.session,
        generation: o.view.generation,
        folder,
    }
}

impl Open {
    /// The current view's row of entry `path`.
    fn row_of(&self, path: &str) -> u32 {
        let (_, _, rows) =
            lpk_gui::batch::decode(&self.app.rows(self.session, 0, 100).unwrap()).unwrap();
        rows.iter().position(|r| r.name == path).unwrap() as u32
    }

    fn open(&self, path: &str) -> Result<Opening, lpk_gui::GuiError> {
        self.app.open_entry(
            self.session,
            self.row_of(path),
            self.generation,
            &Monitor::default(),
        )
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// Nothing at all under the `Open` folder: no file, no fresh folder.
fn nothing_written(folder: &Path) -> bool {
    std::fs::read_dir(folder).map_or(true, |mut rd| rd.next().is_none())
}

#[test]
fn a_second_extension_after_a_space_is_refused_before_anything_is_written() {
    let d = tempfile::tempdir().unwrap();
    let o = open_archive(d.path(), ENTRIES);
    let r = o.open("doc.pdf .cmd").unwrap();
    assert_eq!(
        r,
        Opening::Preview {
            name: "doc.pdf .cmd".into(),
            extension: "cmd".into()
        }
    );
    assert!(nothing_written(&o.folder));
}

#[test]
fn a_trailing_space_is_refused_before_anything_is_written() {
    let d = tempfile::tempdir().unwrap();
    let o = open_archive(d.path(), ENTRIES);
    let e = o.open("doc.pdf ").unwrap_err();
    assert!(e.is(lpk_api::Class::Unsafe), "{e}");
    assert!(nothing_written(&o.folder));
}

#[test]
fn names_the_path_rules_refuse_create_no_folder() {
    let d = tempfile::tempdir().unwrap();
    let entries: Vec<(&str, Option<&[u8]>)> = UNSAFE
        .iter()
        .map(|n| (*n, Some(&b"x"[..])))
        .chain([("ok.txt", Some(&b"ok"[..]))])
        .collect();
    let o = open_archive(d.path(), &entries);
    for name in UNSAFE {
        let e = o.open(name).unwrap_err();
        assert!(e.is(lpk_api::Class::Unsafe), "{name}: {e}");
        assert!(nothing_written(&o.folder), "{name}: something was written");
    }
    assert!(matches!(o.open("ok.txt").unwrap(), Opening::Shell { .. }));
}

#[test]
fn the_entry_goes_alone_into_a_fresh_folder_and_the_exit_removes_it() {
    let d = tempfile::tempdir().unwrap();
    let o = open_archive(d.path(), ENTRIES);
    let first = o.open("doc.pdf").unwrap();
    let Opening::Shell { name, file } = first else {
        panic!("{first:?}")
    };
    assert_eq!(name, "doc.pdf");
    assert_eq!(std::fs::read(&file).unwrap(), b"%PDF-1.7 body");
    let fresh = file.parent().unwrap().to_path_buf();
    assert_eq!(fresh.parent().unwrap(), o.folder);
    // Only that file: no sibling, no same-named folder `doc`.
    assert_eq!(files_under(&fresh), std::slice::from_ref(&file));
    assert_eq!(std::fs::read_dir(&fresh).unwrap().count(), 1);
    // Opening it again takes another fresh folder (nothing replaced).
    let Opening::Shell { file: again, .. } = o.open("doc.pdf").unwrap() else {
        panic!()
    };
    assert_ne!(again.parent(), file.parent());
    // A folder is not opened.
    let e = o.open("doc").unwrap_err();
    assert!(e.is(lpk_api::Class::Usage), "{e}");
    assert!(o.app.shutdown(std::time::Duration::from_secs(10)));
    assert!(files_under(&o.folder).is_empty(), "cleaned at exit");
}

/// The stale-view refusal itself (the page ignores exactly this one, `apps/gui/main.js`), not
/// another `Usage` error.
fn stale(e: &lpk_gui::GuiError) -> bool {
    e.is(lpk_api::Class::Usage) && e.detail.starts_with(STALE_VIEW)
}

#[test]
fn row_requests_against_an_older_view_are_refused_and_write_nothing() {
    let d = tempfile::tempdir().unwrap();
    let o = open_archive(d.path(), ENTRIES);
    let row = o.row_of("doc.pdf");
    // A re-sort the page has not seen yet: row `row` now names another entry.
    let request = ViewRequest {
        mode: "flat".into(),
        key: "size".into(),
        descending: true,
        ..ViewRequest::default()
    };
    let c = CancelToken::new();
    let v = o.app.view(o.session, &request, &c).unwrap();
    assert_ne!(v.generation, o.generation);
    let e = o
        .app
        .open_entry(o.session, row, o.generation, &Monitor::default())
        .unwrap_err();
    assert!(stale(&e), "{e}");
    assert!(nothing_written(&o.folder));
    let e = o
        .app
        .select(o.session, SelectAction::Only, Some(row), o.generation)
        .unwrap_err();
    assert!(stale(&e), "{e}");
    let e = o
        .app
        .select(o.session, SelectAction::Range, Some(row), o.generation)
        .unwrap_err();
    assert!(stale(&e), "{e}");
    // The current generation still works.
    let s = o
        .app
        .select(o.session, SelectAction::Only, Some(row), v.generation)
        .unwrap();
    assert_eq!(s.picked, 1);
    let fresh = Open {
        generation: v.generation,
        ..o
    };
    assert!(matches!(
        fresh.open("doc.pdf").unwrap(),
        Opening::Shell { .. }
    ));
}

#[cfg(windows)]
#[test]
fn a_zoned_archive_zones_the_opened_entry() {
    let d = tempfile::tempdir().unwrap();
    let archive = d.path().join("a.lpk");
    archive_with(&archive, ENTRIES).unwrap();
    let mut stream = archive.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
    let app = App::new(None).with_open_dir(Some(d.path().join("Open")));
    let g = app.grants.mint(GrantKind::Archive, &archive).grant;
    let opened = app.open(&g, &CancelToken::new()).unwrap();
    let o = Open {
        app,
        session: opened.session,
        generation: opened.view.generation,
        folder: d.path().join("Open"),
    };
    let Opening::Shell { file, .. } = o.open("other.txt").unwrap() else {
        panic!()
    };
    let mut zone = file.as_os_str().to_owned();
    zone.push(":Zone.Identifier");
    let text = std::fs::read_to_string(&zone).unwrap();
    assert!(text.contains("ZoneId=3"), "{text}");
}

#[cfg(windows)]
#[test]
fn an_opened_entry_is_zoned_with_the_opt_out_on() {
    // `ExtractOptions::opening`: Mark of the Web opted out for the session, and the opened
    // entry carries the archive's zone all the same.
    let d = tempfile::tempdir().unwrap();
    let archive = d.path().join("a.lpk");
    archive_with(&archive, ENTRIES).unwrap();
    let mut stream = archive.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
    let app = App::new(None)
        .with_open_dir(Some(d.path().join("Open")))
        .with_motw_off(true);
    let g = app.grants.mint(GrantKind::Archive, &archive).grant;
    let opened = app.open(&g, &CancelToken::new()).unwrap();
    let o = Open {
        app,
        session: opened.session,
        generation: opened.view.generation,
        folder: d.path().join("Open"),
    };
    let Opening::Shell { file, .. } = o.open("other.txt").unwrap() else {
        panic!()
    };
    let mut zone = file.as_os_str().to_owned();
    zone.push(":Zone.Identifier");
    let text = std::fs::read_to_string(&zone).unwrap();
    assert!(text.contains("ZoneId=3"), "{text}");
}
