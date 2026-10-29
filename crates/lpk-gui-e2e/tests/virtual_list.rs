//! E5-3's DOM bound (`docs/notes/e5-design.md` sections 3 and 7): `lpk-gui.exe` opens E3-1's
//! generated million-entry archive under the Edge WebDriver, the list is scrolled from top to
//! bottom (the viewport's `scrollTop` in steps, then the keyboard's Home and End), and at every
//! step the DOM holds at most `visible + 2·MARGIN_ROWS` rows (`[role=row]` in `#rows`), the
//! bottom row is reached, and no page flag is set.
//!
//! Ignored unless the crate was built with `LPK_GUI_E2E=1` (build.rs); CI's `gui-e2e` job sets it.
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{connect, gui_says, page_state, start_driver, FLAGS, ROWS_WITHIN};
use fantoccini::key::Key;
use fantoccini::{Client, Locator};

/// The Edge WebDriver's port for this test (another than `open_archive.rs`'s).
const DRIVER_PORT: u16 = 4445;
/// E3-1's archive.
const ENTRIES: u64 = 1_000_000;
/// `lpk_gui::browse::MARGIN_ROWS` (this crate names no `lpk-gui` item; the page reports its own
/// value in `data-margin-rows`, checked against this one).
const MARGIN_ROWS: usize = 64;
/// Scroll positions from the top to the bottom.
const STEPS: u64 = 60;
/// How long the page may take to settle after a scroll.
const SETTLE_WITHIN: Duration = Duration::from_secs(30);

async fn list_attr(c: &Client, name: &str) -> Result<Option<String>, String> {
    let list = c
        .find(Locator::Css("#list"))
        .await
        .map_err(|e| format!("no #list: {e}"))?;
    list.attr(name)
        .await
        .map_err(|e| format!("reading {name}: {e}"))
}

/// Wait until no batch is in flight (`data-pending` = 0) and a frame has passed.
async fn settle(c: &Client) -> Result<(), String> {
    tokio::time::sleep(Duration::from_millis(150)).await;
    let deadline = Instant::now() + SETTLE_WITHIN;
    loop {
        if list_attr(c, "data-pending").await?.as_deref() == Some("0") {
            tokio::time::sleep(Duration::from_millis(50)).await;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the list did not settle within {SETTLE_WITHIN:?}; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The rows in the DOM against the bound; the largest `aria-rowindex` among them.
async fn check_rows(c: &Client, at: &str) -> Result<u64, String> {
    let visible: usize = list_attr(c, "data-visible-rows")
        .await?
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("{at}: no data-visible-rows"))?;
    let rows = c
        .find_all(Locator::Css("#rows [role=row]"))
        .await
        .map_err(|e| format!("{at}: rows: {e}"))?;
    let bound = visible + 2 * MARGIN_ROWS;
    if rows.is_empty() || rows.len() > bound {
        return Err(format!(
            "{at}: {} rows in the DOM, the bound is {visible} visible + 2·{MARGIN_ROWS} = {bound}",
            rows.len()
        ));
    }
    let mut last = 0u64;
    if let Some(r) = rows.last() {
        last = r
            .attr("aria-rowindex")
            .await
            .map_err(|e| format!("{at}: aria-rowindex: {e}"))?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
    }
    eprintln!(
        "{at}: {} rows in the DOM (bound {bound}), last aria-rowindex {last}",
        rows.len()
    );
    Ok(last)
}

async fn check_flags(c: &Client, at: &str) -> Result<(), String> {
    let html = c
        .find(Locator::Css("html"))
        .await
        .map_err(|e| format!("no <html>: {e}"))?;
    for name in FLAGS {
        let v = html
            .attr(name)
            .await
            .map_err(|e| format!("reading {name}: {e}"))?;
        if v.as_deref() != Some("0") {
            return Err(format!(
                "{at}: page flag {name} is {v:?}, not \"0\"; page: {}",
                page_state(c).await
            ));
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(
    not(lpk_gui_e2e),
    ignore = "needs the Edge WebDriver; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn scrolling_a_million_entries_keeps_the_dom_within_the_window_and_its_margins() {
    let temp = tempfile::tempdir().expect("scratch dir");
    let work =
        std::env::var_os("LPK_E2E_WORK").map_or_else(|| temp.path().to_path_buf(), PathBuf::from);
    std::fs::create_dir_all(&work).expect("create the scratch dir");
    let archive = work.join("million.lpk");
    lpk_api::testing::million_entry_archive(&archive, ENTRIES as usize)
        .expect("write the million-entry archive");
    let mut driver = start_driver(&work, DRIVER_PORT);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let c = connect(&mut driver, &work, &archive, DRIVER_PORT).await;
        eprintln!("the GUI: {}", gui_says(&work));
        let outcome = scroll(&c).await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
    });
}

async fn scroll(c: &Client) -> Result<(), String> {
    // The listing reaches the DOM.
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        let n = c
            .find_all(Locator::Css("#rows [role=row]"))
            .await
            .map(|r| r.len())
            .unwrap_or(0);
        if n > 0 {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no [role=row] in #rows within {ROWS_WITHIN:?}; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    settle(c).await?;
    let rowcount = list_attr(c, "aria-rowcount").await?;
    if rowcount.as_deref() != Some(&(ENTRIES + 1).to_string()) {
        return Err(format!(
            "aria-rowcount {rowcount:?}, not the entries and the header"
        ));
    }
    let margin = list_attr(c, "data-margin-rows").await?;
    if margin.as_deref() != Some(&MARGIN_ROWS.to_string()) {
        return Err(format!("data-margin-rows {margin:?}, not {MARGIN_ROWS}"));
    }
    check_rows(c, "top").await?;
    check_flags(c, "top").await?;

    // The viewport's scroll range, top to bottom in steps (a user dragging the scroll bar).
    let range = c
        .execute(
            "const v = document.getElementById('viewport'); return v.scrollHeight - v.clientHeight;",
            vec![],
        )
        .await
        .map_err(|e| format!("reading the scroll range: {e}"))?
        .as_f64()
        .ok_or("the scroll range is not a number")?;
    if range <= 0.0 {
        return Err(format!("the viewport does not scroll (range {range})"));
    }
    let mut last = 0;
    for step in 1..=STEPS {
        let top = range * step as f64 / STEPS as f64;
        c.execute(
            "document.getElementById('viewport').scrollTop = arguments[0]; return null;",
            vec![serde_json::json!(top)],
        )
        .await
        .map_err(|e| format!("scrolling to {top}: {e}"))?;
        settle(c).await?;
        last = check_rows(c, &format!("scrollTop {top:.0}")).await?;
    }
    if last != ENTRIES + 1 {
        return Err(format!(
            "at the bottom the last row is aria-rowindex {last}, not {}",
            ENTRIES + 1
        ));
    }
    check_flags(c, "bottom").await?;

    // The keyboard: Home, then End (focus moves, the list follows).
    let list = c
        .find(Locator::Css("#list"))
        .await
        .map_err(|e| format!("no #list: {e}"))?;
    list.click().await.map_err(|e| format!("focus: {e}"))?;
    for (key, name) in [(Key::Home, "Home"), (Key::End, "End")] {
        list.send_keys(&char::from(key).to_string())
            .await
            .map_err(|e| format!("{name}: {e}"))?;
        settle(c).await?;
        last = check_rows(c, name).await?;
    }
    if last != ENTRIES + 1 {
        return Err(format!(
            "after End the last row is aria-rowindex {last}, not {}",
            ENTRIES + 1
        ));
    }
    check_flags(c, "keyboard").await
}
