//! E5-9's keyboard operation in the real window: F6 and Shift+F6 move the focus through the panes
//! in order and wrap; Tab and Shift+Tab inside an open dialog never leave it; Enter takes a
//! dialog's primary action from a control that does not keep it; Esc closes the dialog and the
//! focus returns to the control that opened it; the command keys open the dialogs. Ignored unless
//! the crate was built with `LPK_GUI_E2E=1` (build.rs); CI's `gui-e2e` job sets it.
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{connect, connect_with, lpk_exe, page_state, start_driver, ROWS_WITHIN};
use fantoccini::{Client, Locator};

/// The Edge WebDriver's port for this test (the others use 4444 to 4451).
const DRIVER_PORT: u16 = 4452;

const TAB: &str = "\u{E004}";
const SHIFT: &str = "\u{E008}";
const ENTER: &str = "\u{E007}";
const ESCAPE: &str = "\u{E00C}";
const F6: &str = "\u{E036}";
const CONTROL: &str = "\u{E009}";
const RELEASE: &str = "\u{E000}";

fn make_archive(work: &Path) -> PathBuf {
    let sub = work.join("in").join("sub");
    std::fs::create_dir_all(&sub).expect("create the input folder");
    for i in 1..=3 {
        std::fs::write(sub.join(format!("f{i}.txt")), format!("file {i}\n")).expect("write input");
    }
    let archive = work.join("keys.lpk");
    let _ = std::fs::remove_file(&archive);
    let status = Command::new(lpk_exe())
        .arg("a")
        .arg(&archive)
        .arg(work.join("in"))
        .status()
        .expect("run lpk");
    assert!(status.success(), "lpk a failed: {status}");
    archive
}

async fn wait_for(c: &Client, css: &str, what: &str) -> Result<(), String> {
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        if c.find(Locator::Css(css)).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("no {what}; page: {}", page_state(c).await));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Press `keys` on the element that has the focus.
async fn press(c: &Client, keys: &str) -> Result<(), String> {
    c.active_element()
        .await
        .map_err(|e| e.to_string())?
        .send_keys(keys)
        .await
        .map_err(|e| e.to_string())
}

/// What has the focus: the pane's name (`data-pane` element's id or class), the dialog, or other.
async fn where_is_focus(c: &Client) -> Result<String, String> {
    let v = c
        .execute(
            "const a = document.activeElement; \
             if (a.closest('dialog[open]')) return 'dialog'; \
             const p = a.closest('[data-pane]'); \
             return p ? (p.id || p.className) : 'other:' + a.tagName + '#' + a.id;",
            vec![],
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(v.as_str().unwrap_or("").to_string())
}

async fn focused_id(c: &Client) -> Result<String, String> {
    let v = c
        .execute("return document.activeElement.id;", vec![])
        .await
        .map_err(|e| e.to_string())?;
    Ok(v.as_str().unwrap_or("").to_string())
}

async fn expect_pane(c: &Client, want: &str, after: &str) -> Result<(), String> {
    let got = where_is_focus(c).await?;
    if got.split_whitespace().next() == Some(want) {
        Ok(())
    } else {
        Err(format!(
            "after {after}: focus is in {got:?}, wanted {want:?}"
        ))
    }
}

#[test]
#[cfg_attr(
    not(lpk_gui_e2e),
    ignore = "needs the Edge WebDriver; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn f6_cycles_the_panes_and_dialogs_keep_the_focus_and_answer_the_keys() {
    let temp = tempfile::tempdir().expect("scratch dir");
    let work = temp.path().to_path_buf();
    let archive = make_archive(&work);
    let create = work.join("in");
    let mut driver = start_driver(&work, DRIVER_PORT);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let c = connect(&mut driver, &work, &archive, DRIVER_PORT).await;
        let outcome = run(&c).await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
        // The create wizard (through --create, no native picker): the trap holds there too.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let c = connect_with(
            &mut driver,
            &work,
            format!("--create={}", create.display()),
            DRIVER_PORT,
        )
        .await;
        let outcome = async {
            wait_for(&c, "dialog.create[open] .inputs li", "the create wizard").await?;
            trap(&c, "the create wizard").await
        }
        .await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
    });
}

async fn run(c: &Client) -> Result<(), String> {
    wait_for(c, "#rows [role=row]", "a row").await?;
    // The opened archive's list has the focus. The panes, in order: the command bar, the
    // path bar and filter, the list, the preview.
    expect_pane(c, "list", "opening").await?;
    for want in ["preview", "actions", "bar", "list"] {
        press(c, F6).await?;
        expect_pane(c, want, "F6").await?;
    }
    for want in ["bar", "actions", "preview", "list"] {
        press(c, &format!("{SHIFT}{F6}{RELEASE}")).await?;
        expect_pane(c, want, "Shift+F6").await?;
    }
    // Ctrl+F: the filter.
    press(c, &format!("{CONTROL}f{RELEASE}")).await?;
    if focused_id(c).await? != "filter" {
        return Err("Ctrl+F did not focus the filter".into());
    }

    // The command key opens the dialog, Tab and Shift+Tab stay in it, Esc closes it and the
    // focus returns to where it was.
    press(c, F6).await?;
    expect_pane(c, "list", "F6 to the list").await?;
    press(c, &format!("{CONTROL}e{RELEASE}")).await?;
    wait_for(
        c,
        "dialog.wizard[open] button.primary",
        "the extract dialog (Ctrl+E)",
    )
    .await?;
    trap(c, "the extract dialog").await?;
    press(c, ESCAPE).await?;
    wait_closed(c).await?;
    expect_pane(c, "list", "Esc closing the dialog").await?;

    // A maintenance dialog by its command key: the trap holds there too, Esc closes it.
    press(c, &format!("{CONTROL}{SHIFT}h{RELEASE}")).await?;
    wait_for(
        c,
        "dialog.wizard[open] input[name=generation]",
        "the history dialog (Ctrl+Shift+H)",
    )
    .await?;
    trap(c, "the history dialog").await?;
    press(c, ESCAPE).await?;
    wait_closed(c).await?;

    // From the button: Enter opens, Esc closes, the focus is the button again.
    let button = c
        .find(Locator::Css("#action-extract"))
        .await
        .map_err(|e| e.to_string())?;
    button.send_keys(ENTER).await.map_err(|e| e.to_string())?;
    wait_for(
        c,
        "dialog.wizard[open] button.primary",
        "the extract dialog (Enter)",
    )
    .await?;
    press(c, ESCAPE).await?;
    wait_closed(c).await?;
    let back = focused_id(c).await?;
    if back != "action-extract" {
        return Err(format!("the focus did not return to the button: {back:?}"));
    }

    // Enter on a radio button (not a button, not a text field) takes the primary action: the
    // extraction runs beside the archive and the result has its check line.
    button.send_keys(ENTER).await.map_err(|e| e.to_string())?;
    wait_for(
        c,
        "dialog.wizard[open] input[name=dest]",
        "the destination choices",
    )
    .await?;
    // Mark the dialog's status node: it must be the same node after every phase.
    c.execute(
        "document.querySelector('dialog.wizard[open] [role=status].sr-only').dataset.marker = 'kept'; return true;",
        vec![],
    )
    .await
    .map_err(|e| e.to_string())?;
    c.find(Locator::Css("dialog.wizard[open] input[name=dest]:checked"))
        .await
        .map_err(|e| e.to_string())?
        .send_keys(ENTER)
        .await
        .map_err(|e| e.to_string())?;
    wait_for(c, "dialog.wizard[open] .check", "the result after Enter").await?;
    // The same status node survived the phases (never detached) and now holds the result's
    // first line (set a moment after the render).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let v = c
            .execute(
                "const s = document.querySelector('dialog.wizard[open] [role=status].sr-only');                  const p = document.querySelector('dialog.wizard[open] .wizard-body p');                  return !!s && s.isConnected && s.dataset.marker === 'kept' && !!p                      && s.textContent !== '' && s.textContent === p.textContent;",
                vec![],
            )
            .await
            .map_err(|e| e.to_string())?;
        if v.as_bool() == Some(true) {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the status node was replaced or does not hold the result's first line; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Enter takes the primary action of the result (Close) from the dialog itself.
    press(c, ENTER).await?;
    wait_closed(c).await?;
    settings(c).await
}

/// The settings dialog (E5-12): the gear opens it from the keyboard, Tab and Shift+Tab stay
/// inside, Esc closes it and the focus is the gear again.
async fn settings(c: &Client) -> Result<(), String> {
    let gear = c
        .find(Locator::Css("#settings-open"))
        .await
        .map_err(|e| e.to_string())?;
    gear.send_keys(ENTER).await.map_err(|e| e.to_string())?;
    wait_for(
        c,
        "dialog#settings[open] #update-check",
        "the settings dialog",
    )
    .await?;
    trap(c, "the settings dialog").await?;
    press(c, ESCAPE).await?;
    wait_closed(c).await?;
    let back = focused_id(c).await?;
    if back != "settings-open" {
        return Err(format!("the focus did not return to the gear: {back:?}"));
    }
    Ok(())
}

async fn wait_closed(c: &Client) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let v = c
            .execute("return !document.querySelector('dialog[open]');", vec![])
            .await
            .map_err(|e| e.to_string())?;
        if v.as_bool() == Some(true) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the dialog did not close; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Tab and Shift+Tab around the open dialog: the focus never leaves it (more presses than it
/// has stops, so that it wraps both ways).
async fn trap(c: &Client, what: &str) -> Result<(), String> {
    for i in 0..30 {
        press(c, TAB).await?;
        let at = where_is_focus(c).await?;
        if at != "dialog" {
            return Err(format!("{what}: Tab #{i} left the dialog: {at}"));
        }
    }
    for i in 0..30 {
        press(c, &format!("{SHIFT}{TAB}{RELEASE}")).await?;
        let at = where_is_focus(c).await?;
        if at != "dialog" {
            return Err(format!("{what}: Shift+Tab #{i} left the dialog: {at}"));
        }
    }
    Ok(())
}
