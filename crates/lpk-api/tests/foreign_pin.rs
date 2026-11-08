//! A foreign archive is pinned while a session holds it (Windows): the session opens it with
//! `FILE_SHARE_READ` only, so it cannot be replaced, deleted or renamed between the zone read
//! and the worker's open by path. On NTFS (Windows 11) renaming the parent directory is refused
//! as well while the archive is held.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::{Path, PathBuf};

use lpk_api::{OpenOptions, Session};

fn zip_at(path: &Path) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    z.start_file("note.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    z.write_all(b"hello").unwrap();
    z.finish().unwrap();
}

fn fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let a = dir.join("a.zip");
    zip_at(&a);
    let other = dir.join("other.zip");
    zip_at(&other);
    (a, other)
}

#[test]
fn a_held_zip_cannot_be_replaced_or_deleted_until_the_session_is_dropped() {
    let d = tempfile::tempdir().unwrap();
    let (a, other) = fixture(d.path());
    let s = Session::open(&a, &OpenOptions::default()).unwrap();
    assert!(s.info().foreign);
    // Reading stays possible while it is pinned.
    assert!(std::fs::read(&a).is_ok());
    assert!(
        std::fs::rename(&other, &a).is_err(),
        "a file renamed over a pinned archive"
    );
    assert!(
        std::fs::remove_file(&a).is_err(),
        "a pinned archive deleted"
    );
    assert!(
        std::fs::OpenOptions::new().write(true).open(&a).is_err(),
        "a pinned archive opened for writing"
    );
    assert!(
        std::fs::rename(&a, d.path().join("moved.zip")).is_err(),
        "a pinned archive renamed"
    );
    // A second session on the same archive opens beside the first.
    let s2 = Session::open(&a, &OpenOptions::default()).unwrap();
    drop(s2);
    drop(s);
    std::fs::rename(&other, &a).unwrap();
    std::fs::remove_file(&a).unwrap();
}

#[test]
fn a_zip_another_program_is_writing_is_refused_with_a_clear_error() {
    let d = tempfile::tempdir().unwrap();
    let (a, _) = fixture(d.path());
    let w = std::fs::OpenOptions::new().write(true).open(&a).unwrap();
    let e = Session::open(&a, &OpenOptions::default()).unwrap_err();
    assert_eq!(e.class, lpk_api::Class::AccessDenied, "{e}");
    assert!(e.to_string().contains("another program"), "{e}");
    drop(w);
    assert!(Session::open(&a, &OpenOptions::default()).is_ok());
}

/// What Windows does when the pinned archive's parent directory is renamed. The pin does not
/// cover the directory: if the rename succeeds, the path the broker later opens no longer
/// names the pinned file, and the window stays open through the directory. This test records
/// the behaviour (it asserts what was observed) so a change shows up.
#[test]
fn renaming_the_parent_directory_of_a_held_zip_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let parent = d.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let (a, _) = fixture(&parent);
    let s = Session::open(&a, &OpenOptions::default()).unwrap();
    let r = std::fs::rename(&parent, d.path().join("renamed"));
    eprintln!("renaming the parent directory while pinned: {r:?}");
    assert!(r.is_err(), "the parent directory was renamed: {r:?}");
    drop(s);
    std::fs::rename(&parent, d.path().join("renamed")).unwrap();
}
