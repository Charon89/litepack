//! E5-7's acceptance: a second instance's verbs reach the first. `lpk-gui.exe` runs under the
//! Edge WebDriver with one archive open; a second `lpk-gui.exe --open <other>` started beside it
//! hands its verb to the first (`tauri-plugin-single-instance`) and exits at once, and the first
//! window's rows become the other archive's, with no page error.
//!
//! Ignored unless the crate was built with `LPK_GUI_E2E=1` (build.rs); CI's `gui-e2e` job sets
//! it.
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{connect, gui_exe, gui_says, lpk_exe, page_state, start_driver, FLAGS, ROWS_WITHIN};
use fantoccini::{Client, Locator};

/// The Edge WebDriver's port for this test (another than the other tests').
const DRIVER_PORT: u16 = 4446;

/// How long the second instance may take to hand over and exit.
const SECOND_EXITS_WITHIN: Duration = Duration::from_secs(60);

/// `<work>/<name>.lpk` of a folder `<folder>` holding `files` files, made by `lpk`.
fn make_archive(work: &Path, name: &str, folder: &str, files: usize) -> PathBuf {
    let input = work.join(format!("in-{name}"));
    let sub = input.join(folder);
    let _ = std::fs::remove_dir_all(&input);
    std::fs::create_dir_all(&sub).expect("create the input folder");
    for i in 1..=files {
        std::fs::write(sub.join(format!("f{i}.txt")), format!("file {i}\n")).expect("write input");
    }
    let archive = work.join(format!("{name}.lpk"));
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

async fn first_row(c: &Client) -> Option<String> {
    let row = c.find(Locator::Css("#rows [role=row]")).await.ok()?;
    let cell = row.find(Locator::Css("[role=gridcell]")).await.ok()?;
    cell.text().await.ok()
}

async fn wait_for_first_row(c: &Client, want: &str) -> Result<(), String> {
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        let got = first_row(c).await;
        if got.as_deref() == Some(want) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the first row is {got:?}, not {want:?}, after {ROWS_WITHIN:?}; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[test]
#[cfg_attr(
    not(lpk_gui_e2e),
    ignore = "needs the Edge WebDriver; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn a_second_instances_verb_reaches_the_first() {
    let temp = tempfile::tempdir().expect("scratch dir");
    let work = std::env::var_os("LPK_E2E_WORK")
        .map_or_else(|| temp.path().to_path_buf(), PathBuf::from)
        .join("single-instance");
    std::fs::create_dir_all(&work).expect("create the scratch folder");
    let first = make_archive(&work, "first", "one", 1);
    let second = make_archive(&work, "second", "two", 2);
    let mut driver = start_driver(&work, DRIVER_PORT);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let c = connect(&mut driver, &work, &first, DRIVER_PORT).await;
        eprintln!("the GUI: {}", gui_says(&work));
        let outcome = check(&c, &work, &second).await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
    });
}

async fn check(c: &Client, work: &Path, second: &Path) -> Result<(), String> {
    wait_for_first_row(c, "one").await?;
    // The second instance, from another folder with a relative path: the first resolves it
    // against the second's folder. The same state folders as the first (the driver's
    // environment), as a second double-click would have.
    let mut child = Command::new(gui_exe())
        .current_dir(second.parent().unwrap_or(work))
        .arg("--open")
        .arg(second.file_name().unwrap_or_default())
        .env("LOCALAPPDATA", work.join("local"))
        .env("APPDATA", work.join("roaming"))
        .env_remove("LPK_GUI_WEBDRIVER")
        .env_remove("LPK_GUI_SMOKE_REPORT")
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("start the second instance: {e}"))?;
    let deadline = Instant::now() + SECOND_EXITS_WITHIN;
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(format!(
                "the second instance still runs after {SECOND_EXITS_WITHIN:?}: it did not hand over"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    eprintln!("the second instance exited: {status}");
    if !status.success() {
        return Err(format!("the second instance exited with {status}"));
    }
    // Its verb reached the first window: the other archive's rows.
    wait_for_first_row(c, "two").await?;
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
                "page flag {name} is {v:?}, not \"0\"; page: {}",
                page_state(c).await
            ));
        }
    }
    Ok(())
}
