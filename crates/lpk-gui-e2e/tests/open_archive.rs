//! E5-11's driver harness: `lpk-gui.exe` under tauri-driver and the Edge WebDriver opens an
//! archive made by `lpk`, its rows reach the DOM, and the page raised no error flag.
//!
//! Ignored unless the crate was built with `LPK_GUI_E2E=1` (build.rs); CI's `gui-e2e` job sets
//! it. E5-3's DOM-count check and E5-9's accessibility pass are added to this harness.
#![cfg(windows)]
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fantoccini::error::NewSessionError;
use fantoccini::{Client, ClientBuilder, Locator};
use hyper_util::client::legacy::connect::HttpConnector;

/// tauri-driver's port and the native driver's (tauri-driver's defaults).
const DRIVER_PORT: u16 = 4444;
const NATIVE_PORT: u16 = 4445;
/// How long the driver may take to accept a session, and the page to show its rows.
const CONNECT_WITHIN: Duration = Duration::from_secs(90);
const ROWS_WITHIN: Duration = Duration::from_secs(90);

/// The page's flags (`apps/gui/main.js`): attributes of `<html>` holding counts, set to "0" when
/// the listeners are installed.
const FLAGS: &[&str] = &[
    "data-lpk-error",
    "data-lpk-rejection",
    "data-lpk-csp",
    "data-lpk-failure",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn exe_from_env(var: &str, default: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(var).map_or_else(default, PathBuf::from)
}

fn lpk_exe() -> PathBuf {
    exe_from_env("LPK_E2E_LPK", || {
        workspace_root().join("target/release/lpk.exe")
    })
}

fn gui_exe() -> PathBuf {
    exe_from_env("LPK_E2E_GUI", || {
        workspace_root().join("target/release/lpk-gui.exe")
    })
}

fn tauri_driver() -> PathBuf {
    exe_from_env("LPK_E2E_TAURI_DRIVER", || PathBuf::from("tauri-driver"))
}

fn native_driver() -> PathBuf {
    exe_from_env("LPK_E2E_NATIVE_DRIVER", || {
        let dir = std::env::var_os("EdgeWebDriver")
            .expect("set LPK_E2E_NATIVE_DRIVER or EdgeWebDriver (the folder of msedgedriver.exe)");
        PathBuf::from(dir).join("msedgedriver.exe")
    })
}

/// The tauri-driver process, killed with its whole tree (the Edge WebDriver, the GUI and its
/// webview) when the test ends however it ends, a panic included.
struct Driver(Child);

impl Driver {
    /// A failure message if tauri-driver has already exited.
    fn exited(&mut self) -> Option<String> {
        match self.0.try_wait() {
            Ok(Some(status)) => Some(format!("tauri-driver exited: {status}")),
            Ok(None) => None,
            Err(e) => Some(format!("tauri-driver's state is unknown: {e}")),
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID"])
            .arg(self.0.id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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

fn start_driver(work: &Path) -> Driver {
    let local = work.join("local");
    std::fs::create_dir_all(&local).expect("create the local data folder");
    let native = native_driver();
    assert!(native.is_file(), "no native driver at {}", native.display());
    let child = Command::new(tauri_driver())
        .arg("--port")
        .arg(DRIVER_PORT.to_string())
        .arg("--native-port")
        .arg(NATIVE_PORT.to_string())
        .arg("--native-driver")
        .arg(&native)
        // The GUI's state (journal, WebView2's data folder) goes to the scratch dir; the GUI
        // inherits this environment through tauri-driver and the native driver.
        .env("LOCALAPPDATA", &local)
        // The host passes the driver's browser arguments to its webview (WebView2 does not apply
        // them itself when the app sets its own; `lpk_gui::security::webdriver::WEBDRIVER_ENV`,
        // in a GUI built with the feature `webdriver`).
        .env("LPK_GUI_WEBDRIVER", "1")
        .env_remove("LPK_GUI_SMOKE_REPORT")
        .stdin(Stdio::null())
        .spawn()
        .expect("start tauri-driver");
    Driver(child)
}

async fn connect(driver: &mut Driver, archive: &Path) -> Client {
    let gui = gui_exe();
    assert!(gui.is_file(), "no GUI at {}", gui.display());
    let mut caps = serde_json::Map::new();
    caps.insert(
        "tauri:options".into(),
        serde_json::json!({
            "application": gui,
            // The Edge WebDriver turns each argument into a Chromium switch (a bare path becomes
            // `--d:\...`, lower-cased), so the archive goes as `--open=<path>`, whose value it keeps.
            "args": [format!("--open={}", archive.display())],
        }),
    );
    let url = format!("http://127.0.0.1:{DRIVER_PORT}");
    let deadline = Instant::now() + CONNECT_WITHIN;
    loop {
        // A dead tauri-driver fails the test at once, not after the whole wait.
        if let Some(e) = driver.exited() {
            panic!("no WebDriver session: {e}");
        }
        match ClientBuilder::new(HttpConnector::new())
            .capabilities(caps.clone())
            .connect(&url)
            .await
        {
            Ok(c) => return c,
            // Only an unreachable driver is waited for; a refused session fails at once.
            Err(
                e @ (NewSessionError::Failed(_)
                | NewSessionError::FailedC(_)
                | NewSessionError::Lost(_)),
            ) if Instant::now() < deadline => {
                eprintln!("waiting for tauri-driver: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => panic!("no WebDriver session within {CONNECT_WITHIN:?}: {e}"),
        }
    }
}

/// The `<html>` attributes the page sets, and the status bar's text, for a failure message.
async fn page_state(c: &Client) -> String {
    let mut out = Vec::new();
    match c.find(Locator::Css("html")).await {
        Ok(html) => {
            for name in FLAGS.iter().copied().chain(["data-lpk-last"]) {
                let v = html.attr(name).await.ok().flatten();
                out.push(format!("{name}={v:?}"));
            }
        }
        Err(e) => out.push(format!("no <html>: {e}")),
    }
    if let Ok(status) = c.find(Locator::Css("#status")).await {
        out.push(format!(
            "status={:?}",
            status.text().await.unwrap_or_default()
        ));
    }
    out.join(", ")
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
    ignore = "needs tauri-driver and the Edge WebDriver; build with LPK_GUI_E2E=1 (CI's gui-e2e job)"
)]
fn an_archive_given_on_the_command_line_opens_with_no_page_error() {
    // `LPK_E2E_WORK` keeps the scratch folder (CI reads it after a failure); else a temporary one.
    let temp = tempfile::tempdir().expect("scratch dir");
    let work =
        std::env::var_os("LPK_E2E_WORK").map_or_else(|| temp.path().to_path_buf(), PathBuf::from);
    let archive = make_archive(&work);
    let mut driver = start_driver(&work);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let c = connect(&mut driver, &archive).await;
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
