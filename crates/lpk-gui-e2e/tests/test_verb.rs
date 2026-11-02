//! E5-8 under the WebDriver: the app starts with `--test=<fixture archive>` (the inline form the
//! Edge driver needs), the test dialog queues the archive, the result says it passed and that
//! every chunk was decoded, and the page's error, rejection and CSP flags stay unset. Ignored
//! unless built with `LPK_GUI_E2E=1` (CI's `gui-e2e` job).
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{connect_with, lpk_exe, page_state, start_driver, FLAGS, ROWS_WITHIN};
use fantoccini::{Client, Locator};

const DRIVER_PORT: u16 = 4450;

fn make_archive(work: &Path) -> PathBuf {
    let sub = work.join("in").join("sub");
    std::fs::create_dir_all(&sub).expect("create the input folder");
    for i in 1..=3 {
        std::fs::write(sub.join(format!("f{i}.txt")), format!("file {i}\n")).expect("write input");
    }
    let archive = work.join("fixture.lpk");
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
fn the_test_verb_reports_that_the_fixture_passed() {
    let temp = tempfile::tempdir().expect("scratch dir");
    let work = temp.path().to_path_buf();
    let archive = make_archive(&work);
    let mut driver = start_driver(&work, DRIVER_PORT);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let c = connect_with(
            &mut driver,
            &work,
            format!("--test={}", archive.display()),
            DRIVER_PORT,
        )
        .await;
        let outcome = run(&c).await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
    });
}

async fn run(c: &Client) -> Result<(), String> {
    // The result's first line says the archive passed; the second names the check made.
    let passed = wait_for(c, "dialog.wizard[open] .results li p", "a result").await?;
    let text = passed.text().await.map_err(|e| e.to_string())?;
    if !text.contains("passed") {
        return Err(format!("result: {text:?}; page: {}", page_state(c).await));
    }
    let check = wait_for(c, "dialog.wizard .results .check", "the check line").await?;
    let text = check.text().await.map_err(|e| e.to_string())?;
    if !text.contains("decoded") {
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
