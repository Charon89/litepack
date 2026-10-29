//! Opening an entry (E5-3; `docs/notes/e5-design.md` sections 4 and 7): `doc.pdf .cmd` is
//! refused before anything is written, so is `doc.pdf ` (`pdf ` is not `pdf`; E3-2's policy
//! would refuse the trailing space too), the entry goes
//! alone into a fresh folder (no sibling, no same-named folder), and on Windows a zoned archive's
//! zone reaches the opened file even with the Mark of the Web opt-out.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use lpk_api::testing::archive_with;
use lpk_api::{CancelToken, Monitor};
use lpk_gui::app::App;
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

fn setup(dir: &Path) -> (App, u64, PathBuf) {
    let archive = dir.join("a.lpk");
    archive_with(&archive, ENTRIES).unwrap();
    let open = dir.join("Open");
    let app = App::new(None).with_open_dir(Some(open.clone()));
    let g = app.grants.mint(GrantKind::Archive, &archive).grant;
    let o = app.open(&g, &CancelToken::new()).unwrap();
    (app, o.session, open)
}

/// The flat view's row of entry `path`.
fn row_of(app: &App, session: u64, path: &str) -> u32 {
    let (_, _, rows) = lpk_gui::batch::decode(&app.rows(session, 0, 100).unwrap()).unwrap();
    rows.iter().position(|r| r.name == path).unwrap() as u32
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

#[test]
fn a_second_extension_after_a_space_is_refused_before_anything_is_written() {
    let d = tempfile::tempdir().unwrap();
    let (app, s, open) = setup(d.path());
    let r = app
        .open_entry(s, row_of(&app, s, "doc.pdf .cmd"), &Monitor::default())
        .unwrap();
    assert_eq!(
        r,
        Opening::Preview {
            name: "doc.pdf .cmd".into(),
            extension: "cmd".into()
        }
    );
    assert!(files_under(&open).is_empty(), "nothing written");
}

#[test]
fn a_trailing_space_is_off_the_allow_list() {
    let d = tempfile::tempdir().unwrap();
    let (app, s, open) = setup(d.path());
    // `pdf ` is not `pdf`: off the allow-list, refused first.
    let r = app
        .open_entry(s, row_of(&app, s, "doc.pdf "), &Monitor::default())
        .unwrap();
    assert!(matches!(r, Opening::Preview { .. }), "{r:?}");
    assert!(files_under(&open).is_empty());
}

#[test]
fn the_entry_goes_alone_into_a_fresh_folder_and_the_exit_removes_it() {
    let d = tempfile::tempdir().unwrap();
    let (app, s, open) = setup(d.path());
    let first = app
        .open_entry(s, row_of(&app, s, "doc.pdf"), &Monitor::default())
        .unwrap();
    let Opening::Shell { name, file } = first else {
        panic!("{first:?}")
    };
    assert_eq!(name, "doc.pdf");
    assert_eq!(std::fs::read(&file).unwrap(), b"%PDF-1.7 body");
    let folder = file.parent().unwrap().to_path_buf();
    assert_eq!(folder.parent().unwrap(), open);
    // Only that file: no sibling, no same-named folder `doc`.
    assert_eq!(files_under(&folder), std::slice::from_ref(&file));
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
    // Opening it again takes another fresh folder (nothing replaced).
    let Opening::Shell { file: again, .. } = app
        .open_entry(s, row_of(&app, s, "doc.pdf"), &Monitor::default())
        .unwrap()
    else {
        panic!()
    };
    assert_ne!(again.parent(), file.parent());
    // A folder is not opened.
    let e = app
        .open_entry(s, row_of(&app, s, "doc"), &Monitor::default())
        .unwrap_err();
    assert!(e.is(lpk_api::Class::Usage), "{e}");
    assert!(app.shutdown(std::time::Duration::from_secs(10)));
    assert!(files_under(&open).is_empty(), "cleaned at exit");
}

#[cfg(windows)]
#[test]
fn a_zoned_archive_zones_the_opened_entry() {
    let d = tempfile::tempdir().unwrap();
    let archive = d.path().join("z.lpk");
    archive_with(&archive, ENTRIES).unwrap();
    let mut stream = archive.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
    let app = App::new(None).with_open_dir(Some(d.path().join("Open")));
    let g = app.grants.mint(GrantKind::Archive, &archive).grant;
    let o = app.open(&g, &CancelToken::new()).unwrap();
    let Opening::Shell { file, .. } = app
        .open_entry(
            o.session,
            row_of(&app, o.session, "other.txt"),
            &Monitor::default(),
        )
        .unwrap()
    else {
        panic!()
    };
    let mut zone = file.as_os_str().to_owned();
    zone.push(":Zone.Identifier");
    let text = std::fs::read_to_string(&zone).unwrap();
    assert!(text.contains("ZoneId=3"), "{text}");
}
