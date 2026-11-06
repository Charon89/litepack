//! E5-12's label band: `open_archive` says how big the archive file is and how big its contents
//! are, so the page can draw the gauge; either is `None` when it is not known.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use lpk_api::testing::archive_with;
use lpk_api::{CancelToken, CreateSettings, Credentials, Monitor, Seal, Sources, Suite, Tier};
use lpk_gui::app::{App, Opened};
use lpk_gui::grants::GrantKind;

fn open(archive: &Path) -> Opened {
    let app = App::new(None);
    let g = app.grants.mint(GrantKind::Archive, archive).grant;
    app.open(&g, &CancelToken::new()).unwrap()
}

#[test]
fn a_plain_archive_reports_its_file_size_and_the_sum_of_its_entries() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("plain.lpk");
    archive_with(
        &p,
        &[
            ("a.txt", Some(b"alpha")),
            ("docs", None),
            ("docs/b.txt", Some(b"bravo!")),
        ],
    )
    .unwrap();
    let o = open(&p);
    assert_eq!(o.archive_bytes, Some(std::fs::metadata(&p).unwrap().len()));
    assert_eq!(o.content_bytes, Some(11));
}

#[test]
fn an_encrypted_archive_opened_without_its_key_has_no_content_size() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("secret.txt"), b"top secret").unwrap();
    let p = d.path().join("sealed.lpk");
    let s = CreateSettings {
        tier: Tier::Store,
        seal: Some(Seal {
            suite: Suite::XChaCha,
            listable: true,
            credentials: Credentials::password(b"hunter2".to_vec()),
        }),
        ..CreateSettings::default()
    };
    lpk_api::create(&Sources::contents_of(&src), &p, &s, &Monitor::default()).unwrap();
    let o = open(&p);
    assert!(o.keyless && o.encrypted);
    assert_eq!(o.archive_bytes, Some(std::fs::metadata(&p).unwrap().len()));
    assert_eq!(o.content_bytes, None);
}
