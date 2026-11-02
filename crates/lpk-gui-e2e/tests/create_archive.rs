//! E5-5 under the WebDriver: the app starts with `--create=<fixture folder>` (so no native picker
//! is needed), the wizard lists the input and shows an estimate on every tier card, Create runs,
//! the summary appears, and the harness's `lpk t` passes on the created archive with the entry
//! count of the fixture; the page's error, rejection and CSP flags stay unset. Ignored unless
//! built with `LPK_GUI_E2E=1` (CI's `gui-e2e` job).
#![cfg(windows)]
#![forbid(unsafe_code)]

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{connect_with, lpk_exe, page_state, start_driver, FLAGS, ROWS_WITHIN};
use fantoccini::{Client, Locator};

const DRIVER_PORT: u16 = 4449;
const FILES: usize = 3;

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
fn the_wizard_creates_an_archive_that_lpk_t_passes() {
    let temp = tempfile::tempdir().expect("scratch dir");
    let work = temp.path().to_path_buf();
    let input = work.join("fixture");
    std::fs::create_dir_all(input.join("sub")).expect("create the input folder");
    for i in 1..=FILES {
        std::fs::write(
            input.join("sub").join(format!("f{i}.txt")),
            format!("file {i}\n"),
        )
        .expect("write input");
    }
    let mut driver = start_driver(&work, DRIVER_PORT);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let c = connect_with(
            &mut driver,
            &work,
            format!("--create={}", input.display()),
            DRIVER_PORT,
        )
        .await;
        let outcome = run(&c).await;
        let _ = c.close().await;
        if let Err(e) = outcome {
            panic!("{e}");
        }
    });
    // The default folder is the input's parent and the name its folder's: fixture.lpk.
    let archive = work.join("fixture.lpk");
    assert!(archive.is_file(), "no {}", archive.display());
    check_with_lpk(&archive);
}

fn check_with_lpk(archive: &Path) {
    let out = Command::new(lpk_exe())
        .arg("t")
        .arg(archive)
        .output()
        .expect("run lpk t");
    assert!(
        out.status.success(),
        "lpk t failed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The entries: the files and the `sub` folder.
    let l = Command::new(lpk_exe())
        .arg("l")
        .arg(archive)
        .output()
        .expect("run lpk l");
    let text = String::from_utf8_lossy(&l.stdout);
    for i in 1..=FILES {
        assert!(
            text.contains(&format!("f{i}.txt")),
            "f{i}.txt not listed: {text}"
        );
    }
}

async fn run(c: &Client) -> Result<(), String> {
    wait_for(c, "dialog.create[open] .inputs li", "listed input").await?;
    // Every tier card shows an estimate once Rust's calibration has run.
    let deadline = Instant::now() + ROWS_WITHIN;
    loop {
        let cards = c
            .find_all(Locator::Css("dialog.create .card .estimate"))
            .await
            .map_err(|e| e.to_string())?;
        let mut known = 0;
        for card in &cards {
            let t = card.text().await.map_err(|e| e.to_string())?;
            if t.contains("about") || t.contains("No estimate") {
                known += 1;
            }
        }
        if cards.len() == 3 && known == 3 {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!("estimates missing; page: {}", page_state(c).await));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let go = wait_for(
        c,
        "dialog.create[open] button.primary:not([disabled])",
        "enabled Create button",
    )
    .await?;
    go.click().await.map_err(|e| e.to_string())?;
    // The summary carries the command and the test line.
    let check = wait_for(c, "dialog.create .check", "summary's test line").await?;
    let text = check.text().await.map_err(|e| e.to_string())?;
    if !text.contains("passed") {
        return Err(format!("test line: {text:?}"));
    }
    wait_for(c, "dialog.create input.command", "Copy as command field").await?;
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
