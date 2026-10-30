//! E5-4 under the WebDriver: the extract wizard opens the fixture archive's dialog, takes the
//! Smart destination (no native dialog), waits for the result and compares every extracted file
//! with its source; the page's error and CSP flags stay unset. Ignored unless built with
//! `LPK_GUI_E2E=1` (CI's `gui-e2e` job).
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{connect, lpk_exe, page_state, start_driver, FLAGS, ROWS_WITHIN};
use fantoccini::{Client, Locator};

const DRIVER_PORT: u16 = 4447;

fn make_archive(work: &Path) -> PathBuf {
    let sub = work.join("in").join("sub");
    std::fs::create_dir_all(&sub).expect("create the input folder");
    for i in 1..=3 {
        std::fs::write(sub.join(format!("f{i}.txt")), format!("file {i}\n")).expect("write input");
    }
    let archive = work.join("smoke.lpk");
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

async fn wait_for(
    c: &Client,
    css: &str,
    what: &str,
) -> Result<fantoccini::elements::Element, String> {
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        if let Ok(e) = c.find(Locator::Css(css)).await {
            return Ok(e);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no {what} within {ROWS_WITHIN:?}; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[test]
#[cfg_attr(
    not(lpk_gui_e2e),
    ignore = "needs the Edge WebDriver; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn the_wizard_extracts_a_fixture_with_the_smart_destination() {
    let temp = tempfile::tempdir().expect("scratch dir");
    let work = temp.path().to_path_buf();
    let archive = make_archive(&work);
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
    });
    // Smart: the lone top-level folder `sub` went beside the archive; every file matches.
    for i in 1..=3 {
        let got =
            std::fs::read(work.join("sub").join(format!("f{i}.txt"))).expect("extracted file");
        let want =
            std::fs::read(work.join("in").join("sub").join(format!("f{i}.txt"))).expect("source");
        assert_eq!(got, want, "f{i}.txt");
    }
}

async fn run(c: &Client) -> Result<(), String> {
    wait_for(c, "#rows [role=row]", "row").await?;
    // The command bar's button opens the dialog; the default destination is Smart, so the
    // primary action extracts (nothing exists, so no question).
    let button = wait_for(
        c,
        "#action-extract:not([disabled])",
        "enabled Extract button",
    )
    .await?;
    button.click().await.map_err(|e| e.to_string())?;
    let go = wait_for(
        c,
        "dialog.wizard[open] button.primary",
        "dialog's primary button",
    )
    .await?;
    go.click().await.map_err(|e| e.to_string())?;
    // The result carries the check line.
    let check = wait_for(c, "dialog.wizard .check", "result's check line").await?;
    let text = check.text().await.map_err(|e| e.to_string())?;
    if !text.contains("BLAKE3") {
        return Err(format!("check line: {text:?}"));
    }
    let html = c
        .find(Locator::Css("html"))
        .await
        .map_err(|e| e.to_string())?;
    for name in FLAGS {
        let v = html.attr(name).await.map_err(|e| e.to_string())?;
        if v.as_deref() != Some("0") {
            return Err(format!(
                "page flag {name} is {v:?}; page: {}",
                page_state(c).await
            ));
        }
    }
    Ok(())
}
