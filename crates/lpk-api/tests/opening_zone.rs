//! E5-3's opened entries (`ExtractOptions::opening`, `docs/notes/e5-design.md` section 4): a
//! zoned archive opened with the Mark of the Web opt-out on still zones an entry opened with the
//! shell, while a plain extraction under the opt-out writes no zone. Without `opening` the first
//! assertion fails (the opt-out would apply). The zoneless foreign archive's Internet zone is the
//! unit test `an_opened_entry_is_zoned_whatever_the_opt_out_and_a_zoneless_foreign_one_is_internet`
//! in `src/session.rs` (a foreign extraction needs the sandboxed worker, which this crate's tests
//! do not stage).
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::path::Path;

use lpk_api::{
    CreateSettings, Destination, ExtractOptions, Monitor, OpenOptions, Selection, Session, Sources,
    Tier,
};

fn zoned_archive(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("note.txt"), b"hello").unwrap();
    let out = dir.join("z.lpk");
    let s = CreateSettings {
        tier: Tier::Store,
        ..CreateSettings::default()
    };
    lpk_api::create(&Sources::contents_of(&src), &out, &s, &Monitor::default()).unwrap();
    let mut stream = out.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
    out
}

fn zone_of(file: &Path) -> Option<String> {
    let mut stream = file.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    std::fs::read_to_string(&stream).ok()
}

#[test]
fn an_opened_entry_is_zoned_even_with_the_opt_out_on() {
    let d = tempfile::tempdir().unwrap();
    let archive = zoned_archive(d.path());
    let o = OpenOptions {
        motw_off: true,
        ..OpenOptions::default()
    };
    let session = Session::open(&archive, &o).unwrap();
    let opened = d.path().join("opened");
    let report = session
        .extract(
            &Selection::All,
            &Destination::Dir(opened.clone()),
            &ExtractOptions {
                opening: true,
                ..ExtractOptions::default()
            },
            &Monitor::default(),
        )
        .unwrap();
    assert!(report.zone.is_some());
    let text = zone_of(&opened.join("note.txt")).expect("a Zone.Identifier stream");
    assert!(text.contains("ZoneId=3"), "{text}");

    // The same session's plain extraction honours the opt-out.
    let plain = d.path().join("plain");
    session
        .extract(
            &Selection::All,
            &Destination::Dir(plain.clone()),
            &ExtractOptions::default(),
            &Monitor::default(),
        )
        .unwrap();
    assert_eq!(zone_of(&plain.join("note.txt")), None);
}
