//! The WebDriver test's path (`crates/lpk-gui-e2e/tests/preview.rs`) at the command layer: an
//! archive made with the default settings from five real files, the batch the page gets, a click
//! (`select` only) on row 3, then `preview` with that row and the batch's generation.
#![allow(clippy::unwrap_used)]

use lpk_api::{CancelToken, CreateSettings, Monitor, Sources};
use lpk_gui::app::App;
use lpk_gui::browse::SelectAction;
use lpk_gui::extract::NoPrompt;
use lpk_gui::grants::GrantKind;
use lpk_gui::preview::Kind;

/// The page's magic is the message's (the cause of CI's first gui-e2e failure: `0x5650504c`).
#[test]
fn the_pages_magic_is_the_messages() {
    let js = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/gui/preview.js"),
    )
    .unwrap();
    let hex = js
        .split("const MAGIC = 0x")
        .nth(1)
        .and_then(|r| r.split(';').next())
        .unwrap();
    let want = u32::from_le_bytes(*lpk_gui::preview::MAGIC);
    assert_eq!(u32::from_str_radix(hex, 16).unwrap(), want);
}

#[test]
fn a_click_then_the_preview_of_that_row_gives_its_text() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("preview-in");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("note.txt"), "hello preview pane\n").unwrap();
    std::fs::write(src.join("pic.png"), b"\x89PNG\r\n\x1a\nxx").unwrap();
    for n in ["evil.html", "evil.svg", "evil.png"] {
        std::fs::write(src.join(n), "<html><script>window.x=1</script></html>").unwrap();
    }
    let a = d.path().join("preview.lpk");
    lpk_api::create(
        &Sources::contents_of(&src),
        &a,
        &CreateSettings::default(),
        &Monitor::default(),
    )
    .unwrap();
    let app = App::new(None);
    let g = app.grants.mint(GrantKind::Archive, &a).grant;
    let o = app.open(&g, &CancelToken::new()).unwrap();
    let batch = app.rows(o.session, 0, 100).unwrap();
    let (generation, total, first, _) = lpk_gui::batch::header(&batch).unwrap();
    assert_eq!((total, first), (5, 0));
    assert_eq!(generation, o.view.generation);
    let (_, _, rows) = lpk_gui::batch::decode(&batch).unwrap();
    let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names[3], "note.txt", "{names:?}");
    app.select(o.session, SelectAction::Only, Some(3), generation)
        .unwrap();
    for unlock in [false, true] {
        let p = app
            .preview(
                o.session,
                3,
                generation,
                &CancelToken::new(),
                &NoPrompt,
                unlock,
            )
            .unwrap();
        assert_eq!(p.kind, Kind::Text);
        assert_eq!(p.body, b"hello preview pane\n");
    }
}
