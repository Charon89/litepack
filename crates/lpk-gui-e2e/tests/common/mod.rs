//! The driver harness shared by the WebDriver tests (E5-11; E5-3's DOM count): the Edge
//! WebDriver's process, the session's capabilities, the page's flags. Each test passes its own
//! driver port, so that tests run in parallel processes do not collide.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fantoccini::error::NewSessionError;
use fantoccini::{Client, ClientBuilder, Locator};
use hyper_util::client::legacy::connect::HttpConnector;

/// The Edge WebDriver's default Chrome switches the session does not need: excluded, so that the
/// GUI is handed only `--remote-debugging-port=0`, `--enable-automation` and a
/// `--disable-features=` list (`lpk_gui::security::webdriver::ALLOWED_ARGS`). The list is what
/// msedgedriver 155 put in `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` (CI's gui-e2e log on 6627ae5).
pub(crate) const EXCLUDED_SWITCHES: &[&str] = &[
    "allow-pre-commit-input",
    "disable-background-networking",
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
    "disable-client-side-phishing-detection",
    "disable-default-apps",
    "disable-hang-monitor",
    "disable-popup-blocking",
    "disable-prompt-on-repost",
    "disable-sync",
    "enable-logging",
    "log-level",
    "no-first-run",
    "no-service-autorun",
    "password-store",
    "test-type",
    "use-mock-keychain",
];
/// How long the driver may take to accept a session, and the page to show its rows.
pub(crate) const CONNECT_WITHIN: Duration = Duration::from_secs(90);
pub(crate) const ROWS_WITHIN: Duration = Duration::from_secs(90);

/// The page's flags (`apps/gui/main.js`): attributes of `<html>` holding counts, set to "0" when
/// the listeners are installed.
pub(crate) const FLAGS: &[&str] = &[
    "data-lpk-error",
    "data-lpk-rejection",
    "data-lpk-csp",
    "data-lpk-failure",
];

pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(crate) fn exe_from_env(var: &str, default: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(var).map_or_else(default, PathBuf::from)
}

pub(crate) fn lpk_exe() -> PathBuf {
    exe_from_env("LPK_E2E_LPK", || {
        workspace_root().join("target/release/lpk.exe")
    })
}

pub(crate) fn gui_exe() -> PathBuf {
    exe_from_env("LPK_E2E_GUI", || {
        workspace_root().join("target/release/lpk-gui.exe")
    })
}

pub(crate) fn native_driver() -> PathBuf {
    exe_from_env("LPK_E2E_NATIVE_DRIVER", || {
        let dir = std::env::var_os("EdgeWebDriver")
            .expect("set LPK_E2E_NATIVE_DRIVER or EdgeWebDriver (the folder of msedgedriver.exe)");
        PathBuf::from(dir).join("msedgedriver.exe")
    })
}

/// The Edge WebDriver's process, killed with its whole tree (the GUI and its webview) when the
/// test ends however it ends, a panic included.
pub(crate) struct Driver(pub(crate) Child);

impl Driver {
    /// A failure message if the driver has already exited.
    pub(crate) fn exited(&mut self) -> Option<String> {
        match self.0.try_wait() {
            Ok(Some(status)) => Some(format!("the Edge WebDriver exited: {status}")),
            Ok(None) => None,
            Err(e) => Some(format!("the Edge WebDriver's state is unknown: {e}")),
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

/// The file where the GUI says which of the driver's browser arguments it passed or refused
/// (`lpk_gui::security::webdriver::WEBDRIVER_LOG_ENV`).
pub(crate) fn webdriver_log(work: &Path) -> PathBuf {
    work.join("webdriver.log")
}

/// The GUI's word on the driver's arguments, for messages.
pub(crate) fn gui_says(work: &Path) -> String {
    std::fs::read_to_string(webdriver_log(work)).map_or_else(
        |e| format!("no {}: {e}", webdriver_log(work).display()),
        |s| s.trim().to_owned(),
    )
}

pub(crate) fn start_driver(work: &Path, port: u16) -> Driver {
    // A kept scratch folder holds the previous run's word.
    let _ = std::fs::remove_file(webdriver_log(work));
    let local = work.join("local");
    std::fs::create_dir_all(&local).expect("create the local data folder");
    let native = native_driver();
    assert!(native.is_file(), "no native driver at {}", native.display());
    let child = Command::new(&native)
        .arg(format!("--port={port}"))
        // The GUI's state (journal, WebView2's data folder) goes to the scratch dir; the GUI
        // inherits this environment through the driver.
        .env("LOCALAPPDATA", &local)
        // The host passes the driver's browser arguments to its webview (WebView2 does not apply
        // them itself when the app sets its own; `lpk_gui::security::webdriver::WEBDRIVER_ENV`,
        // in a GUI built with the feature `webdriver`).
        .env("LPK_GUI_WEBDRIVER", "1")
        .env("LPK_GUI_WEBDRIVER_LOG", webdriver_log(work))
        .env_remove("LPK_GUI_SMOKE_REPORT")
        .stdin(Stdio::null())
        .spawn()
        .expect("start the Edge WebDriver");
    Driver(child)
}

pub(crate) async fn connect(driver: &mut Driver, work: &Path, archive: &Path, port: u16) -> Client {
    let gui = gui_exe();
    assert!(gui.is_file(), "no GUI at {}", gui.display());
    let mut caps = serde_json::Map::new();
    caps.insert("browserName".into(), "webview2".into());
    caps.insert("ms:edgeChromium".into(), true.into());
    caps.insert(
        "ms:edgeOptions".into(),
        serde_json::json!({
            "binary": gui,
            // The Edge WebDriver turns each argument into a Chromium switch (a bare path becomes
            // `--d:\...`, lower-cased), so the archive goes as `--open=<path>`, whose value it keeps.
            "args": [format!("--open={}", archive.display())],
            "excludeSwitches": EXCLUDED_SWITCHES,
        }),
    );
    let url = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + CONNECT_WITHIN;
    loop {
        // A dead driver fails the test at once, not after the whole wait.
        if let Some(e) = driver.exited() {
            panic!("no WebDriver session: {e}; the GUI: {}", gui_says(work));
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
                eprintln!("waiting for the Edge WebDriver: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => panic!(
                "no WebDriver session within {CONNECT_WITHIN:?}: {e}; the GUI: {}",
                gui_says(work)
            ),
        }
    }
}

/// The `<html>` attributes the page sets, and the status bar's text, for a failure message.
pub(crate) async fn page_state(c: &Client) -> String {
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
