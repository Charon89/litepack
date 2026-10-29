//! E5-11's driver harness: `lpk-gui.exe` under the Edge WebDriver opens an archive made by
//! `lpk`, its rows reach the DOM, and the page raised no error flag.
//!
//! The test talks to the Edge WebDriver itself, with the capabilities tauri-driver would make
//! (`browserName` `webview2`, the exe as `ms:edgeOptions.binary`) plus `excludeSwitches`:
//! tauri-driver 2.0.4 rebuilds `ms:edgeOptions` from `tauri:options` and drops that key, and
//! without it the driver's default Chrome switches reach the GUI, whose `webdriver` build
//! refuses every argument but the three the session needs (R111).
//!
//! Ignored unless the crate was built with `LPK_GUI_E2E=1` (build.rs); CI's `gui-e2e` job sets
//! it. E5-3's DOM-count check and E5-9's accessibility pass are added to this harness.
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{connect, gui_says, lpk_exe, page_state, start_driver, FLAGS, ROWS_WITHIN};
use fantoccini::{Client, Locator};

/// The Edge WebDriver's port for this test.
const DRIVER_PORT: u16 = 4444;

/// `<work>/smoke.lpk` of a folder and three files, made by `lpk` (as `crates/lpk-gui/smoke.sh`).
fn make_archive(work: &Path) -> PathBuf {
    let sub = work.join("in").join("sub");
    std::fs::create_dir_all(&sub).expect("create the input folder");
    for i in 1..=3 {
        std::fs::write(sub.join(format!("f{i}.txt")), format!("file {i}\n")).expect("write input");
    }
    let archive = work.join("smoke.lpk");
    // A kept scratch folder (`LPK_E2E_WORK`) holds the previous run's archive: `lpk a` would add
    // to it.
    match std::fs::remove_file(&archive) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("remove the previous {}: {e}", archive.display()),
    }
    let status = Command::new(lpk_exe())
        .arg("a")
        .arg(&archive)
        .arg(work.join("in"))
        .status()
        .expect("run lpk");
    assert!(status.success(), "lpk a failed: {status}");
    archive
}

async fn row_names(c: &Client) -> Vec<String> {
    let mut names = Vec::new();
    let rows = c
        .find_all(Locator::Css("#rows [role=row]"))
        .await
        .unwrap_or_default();
    for row in rows {
        let name = match row.find(Locator::Css("[role=gridcell]")).await {
            Ok(cell) => cell.text().await.unwrap_or_default(),
            Err(_) => String::new(),
        };
        names.push(name);
    }
    names
}

#[test]
#[cfg_attr(
    not(lpk_gui_e2e),
    ignore = "needs the Edge WebDriver; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn an_archive_given_on_the_command_line_opens_with_no_page_error() {
    // `LPK_E2E_WORK` keeps the scratch folder (CI reads it after a failure); else a temporary one.
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
        eprintln!("the GUI: {}", gui_says(&work));
        let outcome = check(&c).await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
    });
}

async fn check(c: &Client) -> Result<(), String> {
    // The rows reach the DOM.
    let deadline = Instant::now() + ROWS_WITHIN;
    let names = loop {
        let names = row_names(c).await;
        if !names.is_empty() {
            break names;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no [role=row] in #rows within {ROWS_WITHIN:?}; page: {}",
                page_state(c).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    eprintln!("rows: {names:?}");
    // The page's flags: present (the listeners ran) and all zero.
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
    // The listing is the archive's: the folder and its three files, the folder first.
    if names.len() != 4 || names[0] != "sub" {
        return Err(format!(
            "rows {names:?}, not the folder `sub` and its three files"
        ));
    }
    Ok(())
}
