//! E5-6's acceptance in the real window: the preview pane shows a text entry's text and an
//! image entry through a `blob:` URL, and entries carrying scripts (`evil.html`, `evil.svg`, an
//! HTML file named `evil.png`) never execute: the sentinel they would set on `window` stays unset,
//! nothing in the pane is markup of the archive, and the page's error, rejection and CSP flags
//! stay at zero.
//!
//! Ignored unless the crate was built with `LPK_GUI_E2E=1` (build.rs); CI's `gui-e2e` job sets it.
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{connect, lpk_exe, page_state, start_driver, FLAGS, ROWS_WITHIN};
use fantoccini::{Client, Locator};

/// The Edge WebDriver's port for this test (the others use 4444 to 4447).
const DRIVER_PORT: u16 = 4448;
/// The most a preview takes to show.
const SHOWN_WITHIN: Duration = Duration::from_secs(30);

/// A valid 1x1 PNG.
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15, 0, 2,
    135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const EVIL_HTML: &str = "<html><body><script>window.__lpkEvil = 'html';</script>\
    <img src=x onerror=\"window.__lpkEvil = 'onerror'\"></body></html>";
const EVIL_SVG: &str =
    "<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"window.__lpkEvil = 'svg-onload'\">\
    <script>window.__lpkEvil = 'svg';</script></svg>";

/// `<work>/preview.lpk` of the fixture entries, made by `lpk`.
fn make_archive(work: &Path) -> PathBuf {
    let input = work.join("preview-in");
    std::fs::create_dir_all(&input).expect("create the input folder");
    std::fs::write(input.join("note.txt"), "hello preview pane\n").expect("write the fixture");
    std::fs::write(input.join("pic.png"), PNG).expect("write the fixture");
    std::fs::write(input.join("evil.html"), EVIL_HTML).expect("write the fixture");
    std::fs::write(input.join("evil.svg"), EVIL_SVG).expect("write the fixture");
    // An HTML file named like an image: judged by its content, not its name.
    std::fs::write(input.join("evil.png"), EVIL_HTML).expect("write the fixture");
    let archive = work.join("preview.lpk");
    match std::fs::remove_file(&archive) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("remove the previous {}: {e}", archive.display()),
    }
    let status = Command::new(lpk_exe())
        .arg("a")
        .arg(&archive)
        .arg(&input)
        .status()
        .expect("run lpk");
    assert!(status.success(), "lpk a failed: {status}");
    archive
}

/// Click the row named `name`.
async fn click_row(c: &Client, name: &str) -> Result<(), String> {
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        let rows = c
            .find_all(Locator::Css("#rows [role=row]"))
            .await
            .unwrap_or_default();
        for row in rows {
            let cell = row.find(Locator::Css("[role=gridcell]")).await;
            if let Ok(cell) = cell {
                if cell.text().await.unwrap_or_default() == name {
                    return row.click().await.map_err(|e| format!("click {name}: {e}"));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("no row {name}; page: {}", page_state(c).await));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Run `script` (a function body returning a value) until it returns a truthy value.
async fn wait_for(c: &Client, what: &str, script: &str) -> Result<serde_json::Value, String> {
    let deadline = Instant::now() + SHOWN_WITHIN;
    loop {
        let v = c
            .execute(script, vec![])
            .await
            .map_err(|e| format!("{what}: {e}"))?;
        let truthy = match &v {
            serde_json::Value::Null | serde_json::Value::Bool(false) => false,
            serde_json::Value::String(s) => !s.is_empty(),
            _ => true,
        };
        if truthy {
            return Ok(v);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{what}: not shown within {SHOWN_WITHIN:?}; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Nothing ran: the sentinel is unset, the pane holds no script, frame or markup of the entry,
/// and the page's flags are zero.
async fn assert_inert(c: &Client, at: &str) -> Result<(), String> {
    let v = c
        .execute(
            "return [typeof window.__lpkEvil, \
             document.querySelectorAll('#preview script, #preview iframe, #preview object, #preview embed, #preview svg').length];",
            vec![],
        )
        .await
        .map_err(|e| format!("{at}: {e}"))?;
    if v != serde_json::json!(["undefined", 0]) {
        return Err(format!("{at}: sentinel and elements {v}"));
    }
    let html = c
        .find(Locator::Css("html"))
        .await
        .map_err(|e| format!("no <html>: {e}"))?;
    for name in FLAGS {
        let v = html.attr(name).await.map_err(|e| format!("{name}: {e}"))?;
        if v.as_deref() != Some("0") {
            return Err(format!(
                "{at}: page flag {name} is {v:?}; page: {}",
                page_state(c).await
            ));
        }
    }
    Ok(())
}

/// The text the pane shows once it holds `needle`.
async fn shows_text(c: &Client, needle: &str) -> Result<(), String> {
    let script = format!(
        "const p = document.querySelector('#preview-body pre'); \
         return p && p.textContent.includes({}) ? p.textContent : null;",
        serde_json::json!(needle)
    );
    wait_for(c, &format!("text {needle:?}"), &script)
        .await
        .map(|_| ())
}

async fn check(c: &Client) -> Result<(), String> {
    // A text entry: its text.
    click_row(c, "note.txt").await?;
    shows_text(c, "hello preview pane").await?;
    assert_inert(c, "note.txt").await?;

    // An image entry: an <img> whose src is a blob: URL, decoded by the webview.
    click_row(c, "pic.png").await?;
    let v = wait_for(
        c,
        "the image",
        "const i = document.querySelector('#preview-body img'); \
         return i && i.src.startsWith('blob:') && i.complete && i.naturalWidth > 0 ? i.src : null;",
    )
    .await?;
    eprintln!("image src: {v}");
    assert_inert(c, "pic.png").await?;

    // Entries carrying scripts show their source as text; the sentinel stays unset.
    for (name, needle) in [
        ("evil.html", "window.__lpkEvil = 'html'"),
        ("evil.svg", "window.__lpkEvil = 'svg'"),
        ("evil.png", "window.__lpkEvil = 'html'"),
    ] {
        click_row(c, name).await?;
        shows_text(c, needle).await?;
        // Give a script that did run time to set its sentinel.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_inert(c, name).await?;
    }
    Ok(())
}

#[test]
#[cfg_attr(
    not(lpk_gui_e2e),
    ignore = "needs the Edge WebDriver; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn entries_carrying_scripts_never_execute_in_the_preview() {
    let temp = tempfile::tempdir().expect("scratch dir");
    let work =
        std::env::var_os("LPK_E2E_WORK").map_or_else(|| temp.path().to_path_buf(), PathBuf::from);
    let archive = make_archive(&work);
    let mut driver = start_driver(&work, DRIVER_PORT);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let c = connect(&mut driver, &work, &archive, DRIVER_PORT).await;
        let outcome = check(&c).await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
    });
}
