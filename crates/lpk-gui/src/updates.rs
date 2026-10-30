//! The update check (E5-7), the GUI's one network request: `tauri-plugin-updater` called from
//! Rust alone (no capability grants the page any plugin permission). The gate
//! (`lpk_gui::update::gate`) runs first: with the check off in the settings, or without a usable
//! public key in `tauri.conf.json` (E7-7 puts the owner's in), nothing is requested.
//!
//! The request carries the plugin's own User-Agent (`tauri-plugin-updater/<version>`), the
//! `Accept` header and nothing else; the endpoint uses none of the plugin's URL variables, so no
//! version, target or architecture is sent either. Installing an update is E7-7's.

use std::time::Duration;

use lpk_gui::update::{config, gate, Gate};
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::UpdaterExt;

/// The check's timeout (the plugin sets none by default; `docs/notes/e7-design.md` section 6).
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// What a check came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The user turned the check off: nothing was requested.
    Off,
    /// No usable public key: nothing was requested.
    NoKey,
    /// The endpoint knows no newer version.
    UpToDate,
    /// A newer version.
    Available(String),
    /// The request or its answer failed.
    Failed(String),
}

/// The configured public key (`plugins.updater.pubkey`).
fn pubkey<R: Runtime>(app: &AppHandle<R>) -> String {
    let plugins = serde_json::json!({ "plugins": &app.config().plugins.0 });
    config(&plugins).1
}

/// Check for an update, if the settings (`enabled`) and the key allow it.
pub async fn check<R: Runtime>(app: &AppHandle<R>, enabled: bool) -> Outcome {
    match gate(enabled, &pubkey(app)) {
        Gate::Off => return Outcome::Off,
        Gate::NoKey => return Outcome::NoKey,
        Gate::Check => {}
    }
    let updater = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
        Ok(u) => u,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    match updater.check().await {
        Ok(Some(update)) => Outcome::Available(update.version),
        Ok(None) => Outcome::UpToDate,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    //! A local server stands in for the endpoint (plain HTTP on localhost, allowed by these
    //! tests' configuration alone; the shipped one sets no `dangerous*` option,
    //! `tests/security.rs`).

    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// A structurally valid minisign public key made for these tests: random bytes, no private
    /// half exists (the same as `lpk_gui::update`'s tests).
    const TEST_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDQ2MkQ5NjQ3QTlDRTk0NDUKUldSRmxNNnBSNVl0UnVQbkVQMTlDd25BSGY4V083dHVYY3U0ajEralhBZ2RaazZKcE9RaWJGNVkK";

    fn app(endpoint: &str, key: &str) -> tauri::App<tauri::test::MockRuntime> {
        let mut ctx = tauri::test::mock_context(tauri::test::noop_assets());
        ctx.config_mut().plugins.0.insert(
            "updater".into(),
            serde_json::json!({
                "pubkey": key,
                "endpoints": [endpoint],
                "dangerousInsecureTransportProtocol": true,
            }),
        );
        tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(ctx)
            .expect("the mock app")
    }

    fn server() -> (TcpListener, String) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases/latest.json", l.local_addr().unwrap());
        (l, url)
    }

    /// Whether anything connected to `l` (its backlog holds a connection made before now).
    fn connected(l: &TcpListener) -> bool {
        l.set_nonblocking(true).unwrap();
        match l.accept() {
            Ok(_) => true,
            Err(e) if e.kind() == ErrorKind::WouldBlock => false,
            Err(e) => panic!("accept: {e}"),
        }
    }

    #[test]
    fn with_the_check_off_the_server_sees_nothing() {
        let (l, url) = server();
        let a = app(&url, TEST_KEY);
        let out = tauri::async_runtime::block_on(check(a.handle(), false));
        assert_eq!(out, Outcome::Off);
        assert!(!connected(&l), "a request was made with the check off");
    }

    #[test]
    fn without_a_usable_key_the_server_sees_nothing() {
        let (l, url) = server();
        for key in [lpk_gui::update::PLACEHOLDER_KEY, ""] {
            let a = app(&url, key);
            let out = tauri::async_runtime::block_on(check(a.handle(), true));
            assert_eq!(out, Outcome::NoKey);
        }
        assert!(!connected(&l), "a request was made without a key");
    }

    #[test]
    fn the_request_carries_no_user_or_host_identifier() {
        let (l, url) = server();
        let serve = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).unwrap();
                assert!(n > 0, "the request ended early");
                req.extend_from_slice(&buf[..n]);
            }
            s.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
            String::from_utf8(req).unwrap()
        });
        let a = app(&url, TEST_KEY);
        let out = tauri::async_runtime::block_on(check(a.handle(), true));
        assert_eq!(out, Outcome::UpToDate);
        let req = serve.join().unwrap();
        let mut lines = req.split("\r\n").filter(|l| !l.is_empty());
        // The endpoint's path as configured: no version, target or architecture added.
        assert_eq!(
            lines.next(),
            Some("GET /releases/latest.json HTTP/1.1"),
            "{req}"
        );
        let mut names = Vec::new();
        for h in lines {
            let (name, value) = h.split_once(':').unwrap();
            let name = name.trim().to_ascii_lowercase();
            if name == "user-agent" {
                assert_eq!(value.trim(), "tauri-plugin-updater/2.14.1");
            }
            names.push(name);
        }
        names.sort();
        for n in &names {
            assert!(
                ["accept", "accept-encoding", "host", "user-agent"].contains(&n.as_str()),
                "header {n} in {req}"
            );
        }
        let lower = req.to_lowercase();
        for var in ["USERNAME", "USER", "COMPUTERNAME", "USERDOMAIN", "HOSTNAME"] {
            if let Some(v) = std::env::var_os(var) {
                let v = v.to_string_lossy().to_lowercase();
                if v.len() >= 3 {
                    assert!(!lower.contains(&v), "{var} in the request: {req}");
                }
            }
        }
    }
}
