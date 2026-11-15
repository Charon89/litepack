//! The updater (E5-7's check, E7-7's download and install), the GUI's one network request:
//! `tauri-plugin-updater` called from Rust alone (no capability grants the page any plugin
//! permission). Compiled only with the `updater` feature (on by default; the Store build turns
//! it off). The gate (`lpk_gui::update::gate`) runs first: with the check off in the settings,
//! or without a usable public key in `tauri.conf.json` (E7-1 supplies the owner's), nothing is
//! requested.
//!
//! The request carries the plugin's own User-Agent (`tauri-plugin-updater/<version>`), the
//! `Accept` header and nothing else; the endpoint uses none of the plugin's URL variables, so no
//! version, target or architecture is sent either.
//!
//! The install (`docs/notes/e7-design.md` section 6): the `url` is refused outside the
//! repository's release prefix before any download; the download has its own timeout; the
//! plugin verifies the payload's minisign signature with the compiled-in key and, with
//! `requireSignedVersion`, that the signature was made for the announced version; then
//! `install` starts the NSIS installer with `/P /UPDATE` and ends the process.

use std::path::Path;
use std::time::Duration;

use lpk_gui::update::{config, download_allowed, gate, Gate, State};
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};

/// The check's timeout (the plugin sets none by default; `docs/notes/e7-design.md` section 6).
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// The download's timeout, the whole transfer of the installer (some MB).
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// What a check came to.
pub enum Outcome {
    /// The user turned the check off: nothing was requested.
    Off,
    /// No usable public key: nothing was requested.
    NoKey,
    /// Already checked today: nothing was requested.
    NotDue,
    /// The day could not be recorded, so nothing was requested (at most once a day stays true).
    Unrecorded(String),
    /// The endpoint knows no newer version.
    UpToDate,
    /// A newer version, but the one the user skipped (or older than it).
    Skipped(String),
    /// A newer version.
    Available(Box<Update>),
    /// The request or its answer failed.
    Failed(String),
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Off => f.write_str("Off"),
            Outcome::NoKey => f.write_str("NoKey"),
            Outcome::NotDue => f.write_str("NotDue"),
            Outcome::Unrecorded(e) => write!(f, "Unrecorded({e})"),
            Outcome::UpToDate => f.write_str("UpToDate"),
            Outcome::Skipped(v) => write!(f, "Skipped({v})"),
            Outcome::Available(u) => write!(f, "Available({})", u.version),
            Outcome::Failed(e) => write!(f, "Failed({e})"),
        }
    }
}

/// Why a download was refused.
#[derive(Debug)]
pub enum DownloadError {
    /// The `url` is outside the release prefix: nothing was downloaded.
    OffPrefix(String),
    /// The plugin's download or its verification failed (a bad signature, a tampered payload,
    /// a signature made for another version).
    Plugin(tauri_plugin_updater::Error),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::OffPrefix(url) => {
                write!(
                    f,
                    "the update's download address is not a LitePack release: {url}"
                )
            }
            DownloadError::Plugin(e) => write!(f, "the update could not be verified: {e}"),
        }
    }
}

/// The configured public key (`plugins.updater.pubkey`).
fn pubkey<R: Runtime>(app: &AppHandle<R>) -> String {
    let plugins = serde_json::json!({ "plugins": &app.config().plugins.0 });
    config(&plugins).1
}

/// Check for an update, if the settings (`enabled`) and the key allow it. `before_exit` runs
/// just before the installer starts (the GUI's quit path closes the sessions there). `restart`:
/// whether the installer starts LitePack again (`/R`), which replays the launch's arguments, so
/// the GUI passes `lpk_gui::update::restart_after_install` of its command line.
pub async fn check<R: Runtime>(
    app: &AppHandle<R>,
    enabled: bool,
    restart: bool,
    before_exit: impl Fn() + Send + Sync + 'static,
) -> Outcome {
    match gate(enabled, &pubkey(app)) {
        Gate::Off => return Outcome::Off,
        Gate::NoKey => return Outcome::NoKey,
        Gate::Check => {}
    }
    let updater = match app
        .updater_builder()
        .timeout(CHECK_TIMEOUT)
        .restart_after_install(restart)
        .on_before_exit(before_exit)
        .build()
    {
        Ok(u) => u,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    match updater.check().await {
        Ok(Some(update)) => Outcome::Available(Box::new(update)),
        Ok(None) => Outcome::UpToDate,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// The check as the GUI runs it after the window opens: at most once a day. The day is recorded
/// in `state_file` before the request (so a failed check is not retried until tomorrow), and
/// when it cannot be recorded (no state folder, a failed write) no request is made. A skipped
/// version is not offered again. With the check off nothing is requested or written.
pub async fn scheduled<R: Runtime>(
    app: &AppHandle<R>,
    enabled: bool,
    state_file: Option<&Path>,
    today: u64,
    restart: bool,
    before_exit: impl Fn() + Send + Sync + 'static,
) -> Outcome {
    if !enabled {
        return Outcome::Off;
    }
    let Some(file) = state_file else {
        return Outcome::Unrecorded("LitePack has no state folder".into());
    };
    let mut state = State::load(file);
    if !state.due(today) {
        return Outcome::NotDue;
    }
    if gate(enabled, &pubkey(app)) == Gate::Check {
        state.last_check_day = Some(today);
        if let Err(e) = state.save(file) {
            return Outcome::Unrecorded(format!("{}: {e}", file.display()));
        }
    }
    match check(app, enabled, restart, before_exit).await {
        Outcome::Available(u) if !state.offers(&u.version) => Outcome::Skipped(u.version.clone()),
        other => other,
    }
}

/// Why "Install and restart" installed nothing.
#[derive(Debug)]
pub enum FlowError {
    /// A portable copy never downloads or installs: nothing was requested.
    Portable,
    /// The download was refused or failed verification: the quit path did not run.
    Download(DownloadError),
    /// The installer did not start, after the quit path ran (the GUI then exits).
    Install(String),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::Portable => f.write_str(
                "a portable copy does not update itself; download the new version from the release page",
            ),
            FlowError::Download(e) => e.fmt(f),
            FlowError::Install(e) => write!(f, "the installer did not start: {e}"),
        }
    }
}

/// "Install and restart", in its order (R323): a portable copy stops at once; otherwise the
/// download and its verification first, and only then `quit` (the quit path: ask while an
/// operation runs, then cancel), so a failed download leaves the GUI as it was; then `install`.
/// `Ok(false)` when `quit` declined (the user kept an operation running): nothing installs and
/// nothing was cancelled.
pub async fn install_flow<Q, QF, I, IF>(
    update: &Update,
    prefix: &str,
    portable: bool,
    quit: Q,
    install: I,
    mut step: impl FnMut(&str),
) -> Result<bool, FlowError>
where
    Q: FnOnce() -> QF,
    QF: std::future::Future<Output = bool>,
    I: FnOnce(Vec<u8>) -> IF,
    IF: std::future::Future<Output = Result<(), String>>,
{
    if portable {
        return Err(FlowError::Portable);
    }
    step("download");
    let bytes = download(update, prefix)
        .await
        .map_err(FlowError::Download)?;
    step("verified");
    step("quit");
    if !quit().await {
        step("kept");
        return Ok(false);
    }
    step("install");
    install(bytes).await.map_err(FlowError::Install)?;
    Ok(true)
}

/// Download `update`'s installer, refusing a `url` outside `prefix`
/// (`lpk_gui::update::RELEASE_DOWNLOAD_PREFIX` in the GUI) before any request, with
/// [`DOWNLOAD_TIMEOUT`]; the plugin verifies the bytes it returns.
pub async fn download(update: &Update, prefix: &str) -> Result<Vec<u8>, DownloadError> {
    if !download_allowed(update.download_url.as_str(), prefix) {
        return Err(DownloadError::OffPrefix(update.download_url.to_string()));
    }
    let mut update = update.clone();
    update.timeout = Some(DOWNLOAD_TIMEOUT);
    update
        .download(|_, _| {}, || {})
        .await
        .map_err(DownloadError::Plugin)
}

#[cfg(test)]
mod tests {
    //! A local server on 127.0.0.1 stands in for GitHub Releases (plain HTTP on localhost,
    //! allowed by these tests' configuration alone; the shipped one sets no `dangerous*` option,
    //! `lpk_gui::update`'s tests and `tests/security.rs`). Every signing key is generated here,
    //! at test time, and never written anywhere (R318).

    use std::collections::BTreeMap;
    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;
    use tauri_plugin_updater::Error as PluginError;

    /// A structurally valid minisign public key: random bytes, no private half exists (the same
    /// as `lpk_gui::update`'s tests).
    const RANDOM_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDQ2MkQ5NjQ3QTlDRTk0NDUKUldSRmxNNnBSNVl0UnVQbkVQMTlDd25BSGY4V083dHVYY3U0ajEralhBZ2RaazZKcE9RaWJGNVkK";

    /// The mock app's own version (`tauri::test::mock_context`).
    const CURRENT: &str = "0.1.0";

    /// Standard base64 with padding.
    fn b64(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in data.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                if i <= c.len() {
                    out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// A key pair made for one test, in memory only.
    struct TestKey(minisign::KeyPair);

    impl TestKey {
        fn new() -> TestKey {
            TestKey(minisign::KeyPair::generate_unencrypted_keypair().unwrap())
        }

        /// The public key as `tauri.conf.json` carries it.
        fn public(&self) -> String {
            b64(self.0.pk.to_box().unwrap().into_string().as_bytes())
        }

        /// The `.sig` content as `latest.json` carries it: `data` signed for `version`, the
        /// trusted comment written the way tauri-cli 2.12 writes it.
        fn sign(&self, data: &[u8], version: &str) -> String {
            let trusted = format!("timestamp:0\tfile:LitePack-setup.exe\tversion:{version}");
            let sig = minisign::sign(
                None,
                &self.0.sk,
                std::io::Cursor::new(data),
                Some(&trusted),
                Some("signature from the test key"),
            )
            .unwrap();
            b64(sig.into_string().as_bytes())
        }
    }

    /// The local server: the bodies it serves by path, and the paths it was asked for.
    struct Server {
        base: String,
        listener: Option<TcpListener>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        fn new() -> Server {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            Server {
                base: format!("http://{}", l.local_addr().unwrap()),
                listener: Some(l),
                seen: Arc::default(),
            }
        }

        /// The prefix the tests' downloads must start with (the shipped one's shape).
        fn prefix(&self) -> String {
            format!("{}/Charon89/litepack/releases/download/", self.base)
        }

        fn endpoint(&self) -> String {
            format!("{}/releases/latest/download/latest.json", self.base)
        }

        /// Start answering: each path in `bodies` with its bytes, anything else with a 404.
        fn serve(&mut self, bodies: BTreeMap<String, Vec<u8>>) {
            let l = self.listener.take().unwrap();
            let seen = self.seen.clone();
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { return };
                    let mut req = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let req = String::from_utf8_lossy(&req).to_string();
                    let path = req.split(' ').nth(1).unwrap_or_default().to_owned();
                    seen.lock().unwrap().push(path.clone());
                    let _ = match bodies.get(&path) {
                        Some(b) => s
                            .write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    b.len()
                                )
                                .as_bytes(),
                            )
                            .and_then(|()| s.write_all(b)),
                        None => s.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        ),
                    };
                }
            });
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn app(endpoint: &str, key: &str) -> tauri::App<tauri::test::MockRuntime> {
        let mut ctx = tauri::test::mock_context(tauri::test::noop_assets());
        assert_eq!(ctx.package_info().version.to_string(), CURRENT);
        ctx.config_mut().plugins.0.insert(
            "updater".into(),
            serde_json::json!({
                "pubkey": key,
                "endpoints": [endpoint],
                "requireSignedVersion": true,
                "dangerousInsecureTransportProtocol": true,
            }),
        );
        tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(ctx)
            .expect("the mock app")
    }

    /// One release on the local server: `latest.json` announcing `version` at `url_path`, whose
    /// signature is `signature`; `payload` served at `url_path`.
    fn release(
        server: &mut Server,
        version: &str,
        url_path: &str,
        signature: &str,
        payload: &[u8],
    ) {
        let url = format!("{}{url_path}", server.base);
        let platform = serde_json::json!({ "url": url, "signature": signature });
        let manifest = serde_json::json!({
            "version": version,
            "notes": "Test notes",
            "pub_date": "2026-11-08T00:00:00Z",
            "platforms": {
                "windows-x86_64": platform,
                "windows-aarch64": platform,
            },
        });
        let mut bodies = BTreeMap::new();
        bodies.insert(
            "/releases/latest/download/latest.json".to_owned(),
            manifest.to_string().into_bytes(),
        );
        bodies.insert(url_path.to_owned(), payload.to_vec());
        server.serve(bodies);
    }

    const ASSET: &str = "/Charon89/litepack/releases/download/v9.9.9/LitePack_9.9.9_x64-setup.exe";

    /// Check, then download: what the download came to and the paths the server saw.
    fn check_and_download(
        server: &Server,
        key: &str,
    ) -> (Result<Vec<u8>, DownloadError>, Vec<String>) {
        let a = app(&server.endpoint(), key);
        let out = tauri::async_runtime::block_on(check(a.handle(), true, true, || {}));
        let Outcome::Available(update) = out else {
            panic!("no update: {out:?}");
        };
        assert_eq!(update.version, "9.9.9");
        assert_eq!(update.body.as_deref(), Some("Test notes"));
        let got = tauri::async_runtime::block_on(download(&update, &server.prefix()));
        (got, server.seen())
    }

    #[test]
    fn a_correctly_signed_newer_release_downloads_and_verifies() {
        let key = TestKey::new();
        let payload = b"the installer of 9.9.9".to_vec();
        let mut s = Server::new();
        release(
            &mut s,
            "9.9.9",
            ASSET,
            &key.sign(&payload, "9.9.9"),
            &payload,
        );
        let (got, seen) = check_and_download(&s, &key.public());
        assert_eq!(got.unwrap(), payload);
        assert_eq!(seen.len(), 2, "{seen:?}");
    }

    #[test]
    fn a_bad_signature_in_the_manifest_is_refused() {
        let key = TestKey::new();
        let other = TestKey::new();
        let payload = b"the installer of 9.9.9".to_vec();
        for bad in [
            // A well-formed signature by another key.
            other.sign(&payload, "9.9.9"),
            // Not a signature at all.
            b64(b"untrusted comment: x\nnot a signature\n"),
        ] {
            let mut s = Server::new();
            release(&mut s, "9.9.9", ASSET, &bad, &payload);
            let (got, _) = check_and_download(&s, &key.public());
            assert!(
                matches!(
                    got,
                    Err(DownloadError::Plugin(
                        PluginError::Minisign(_) | PluginError::Base64(_)
                    ))
                ),
                "{got:?}"
            );
        }
    }

    #[test]
    fn a_tampered_payload_is_refused() {
        let key = TestKey::new();
        let payload = b"the installer of 9.9.9".to_vec();
        let mut s = Server::new();
        let sig = key.sign(&payload, "9.9.9");
        release(
            &mut s,
            "9.9.9",
            ASSET,
            &sig,
            b"the installer of 9.9.9, changed",
        );
        let (got, seen) = check_and_download(&s, &key.public());
        assert!(
            matches!(got, Err(DownloadError::Plugin(PluginError::Minisign(_)))),
            "{got:?}"
        );
        assert_eq!(seen.len(), 2, "{seen:?}");
    }

    #[test]
    fn a_validly_signed_older_release_announced_as_newer_is_refused() {
        let key = TestKey::new();
        let old = b"the installer of 0.0.1".to_vec();
        let mut s = Server::new();
        // Signed (validly) for 0.0.1, announced as 9.9.9 (CVE-2026-95625).
        release(&mut s, "9.9.9", ASSET, &key.sign(&old, "0.0.1"), &old);
        let (got, _) = check_and_download(&s, &key.public());
        assert!(
            matches!(
                got,
                Err(DownloadError::Plugin(
                    PluginError::SignedVersionMismatch { .. }
                ))
            ),
            "{got:?}"
        );
    }

    #[test]
    fn a_url_off_the_release_prefix_is_refused_before_any_download() {
        let key = TestKey::new();
        let payload = b"the installer of 9.9.9".to_vec();
        for path in [
            "/Charon89/other/releases/download/v9.9.9/LitePack_9.9.9_x64-setup.exe",
            "/charon89/litepack/releases/download/v9.9.9/LitePack_9.9.9_x64-setup.exe",
            "/Charon89/litepack/archive/LitePack_9.9.9_x64-setup.exe",
        ] {
            let mut s = Server::new();
            release(
                &mut s,
                "9.9.9",
                path,
                &key.sign(&payload, "9.9.9"),
                &payload,
            );
            let (got, seen) = check_and_download(&s, &key.public());
            assert!(matches!(got, Err(DownloadError::OffPrefix(_))), "{got:?}");
            assert_eq!(
                seen,
                ["/releases/latest/download/latest.json"],
                "the payload was requested"
            );
        }
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

    fn quiet_server() -> (TcpListener, String) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases/latest.json", l.local_addr().unwrap());
        (l, url)
    }

    #[test]
    fn with_the_check_off_the_server_sees_nothing() {
        let (l, url) = quiet_server();
        let key = TestKey::new();
        let a = app(&url, &key.public());
        let out = tauri::async_runtime::block_on(check(a.handle(), false, true, || {}));
        assert!(matches!(out, Outcome::Off), "{out:?}");
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("update.json");
        let out = tauri::async_runtime::block_on(scheduled(
            a.handle(),
            false,
            Some(&state),
            20_000,
            true,
            || {},
        ));
        assert!(matches!(out, Outcome::Off), "{out:?}");
        assert!(!connected(&l), "a request was made with the check off");
        assert!(!state.exists(), "the state was written with the check off");
    }

    #[test]
    fn without_a_recorded_day_the_server_sees_nothing() {
        let (l, url) = quiet_server();
        let key = TestKey::new();
        let a = app(&url, &key.public());
        // No state folder.
        let out =
            tauri::async_runtime::block_on(scheduled(a.handle(), true, None, 20_000, true, || {}));
        assert!(matches!(out, Outcome::Unrecorded(_)), "{out:?}");
        // A state file that cannot be written: its folder is a file.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("state");
        std::fs::write(&blocker, b"a file, not a folder").unwrap();
        let file = blocker.join("update.json");
        let out = tauri::async_runtime::block_on(scheduled(
            a.handle(),
            true,
            Some(&file),
            20_000,
            true,
            || {},
        ));
        assert!(matches!(out, Outcome::Unrecorded(_)), "{out:?}");
        assert!(!connected(&l), "a request was made without a recorded day");
    }

    /// The check's update from a server holding `payload` signed (for 9.9.9) as `signature`.
    fn found(
        server: &mut Server,
        key: &TestKey,
        signature: &str,
        payload: &[u8],
    ) -> (tauri::App<tauri::test::MockRuntime>, Box<Update>) {
        release(server, "9.9.9", ASSET, signature, payload);
        let a = app(&server.endpoint(), &key.public());
        let out = tauri::async_runtime::block_on(check(a.handle(), true, true, || {}));
        let Outcome::Available(update) = out else {
            panic!("no update: {out:?}");
        };
        (a, update)
    }

    /// Run the install flow, recording whether the quit path and the install ran.
    fn flow(
        server: &Server,
        update: &Update,
        portable: bool,
        quit_answer: bool,
    ) -> (Result<bool, FlowError>, bool, Option<Vec<u8>>) {
        let quit_ran = Arc::new(Mutex::new(false));
        let installed = Arc::new(Mutex::new(None));
        let (q, i) = (quit_ran.clone(), installed.clone());
        let mut steps: Vec<String> = Vec::new();
        let out = tauri::async_runtime::block_on(install_flow(
            update,
            &server.prefix(),
            portable,
            move || async move {
                *q.lock().unwrap() = true;
                quit_answer
            },
            move |bytes| async move {
                *i.lock().unwrap() = Some(bytes);
                Ok(())
            },
            |s| steps.push(s.to_owned()),
        ));
        let quit = *quit_ran.lock().unwrap();
        let bytes = installed.lock().unwrap().clone();
        // The order the end-to-end job checks in its log too.
        let want: &[&str] = match (&out, quit_answer) {
            (Err(FlowError::Portable), _) => &[],
            (Err(FlowError::Download(_)), _) => &["download"],
            (_, false) => &["download", "verified", "quit", "kept"],
            (_, true) => &["download", "verified", "quit", "install"],
        };
        assert_eq!(steps, want);
        (out, quit, bytes)
    }

    #[test]
    fn a_portable_copy_never_downloads_or_quits() {
        let key = TestKey::new();
        let payload = b"the installer of 9.9.9".to_vec();
        let mut s = Server::new();
        let (_a, update) = found(&mut s, &key, &key.sign(&payload, "9.9.9"), &payload);
        let (out, quit, installed) = flow(&s, &update, true, true);
        assert!(matches!(out, Err(FlowError::Portable)), "{out:?}");
        assert!(!quit && installed.is_none());
        assert_eq!(s.seen(), ["/releases/latest/download/latest.json"]);
    }

    #[test]
    fn a_failed_download_leaves_the_quit_path_unrun() {
        let key = TestKey::new();
        let payload = b"the installer of 9.9.9".to_vec();
        let mut s = Server::new();
        let sig = key.sign(&payload, "9.9.9");
        let (_a, update) = found(&mut s, &key, &sig, b"tampered");
        let (out, quit, installed) = flow(&s, &update, false, true);
        assert!(matches!(out, Err(FlowError::Download(_))), "{out:?}");
        assert!(
            !quit,
            "the quit path ran (and cancelled) before a verified download"
        );
        assert!(installed.is_none());
    }

    #[test]
    fn the_download_is_verified_before_the_quit_path_and_a_kept_operation_installs_nothing() {
        let key = TestKey::new();
        let payload = b"the installer of 9.9.9".to_vec();
        let mut s = Server::new();
        let (_a, update) = found(&mut s, &key, &key.sign(&payload, "9.9.9"), &payload);
        // The user keeps the operation running: nothing installs.
        let (out, quit, installed) = flow(&s, &update, false, false);
        assert!(matches!(out, Ok(false)), "{out:?}");
        assert!(quit && installed.is_none());
        // The user lets it go: the verified bytes are installed.
        let (out, quit, installed) = flow(&s, &update, false, true);
        assert!(matches!(out, Ok(true)), "{out:?}");
        assert!(quit);
        assert_eq!(installed.as_deref(), Some(&payload[..]));
    }

    #[test]
    fn without_a_usable_key_the_server_sees_nothing() {
        let (l, url) = quiet_server();
        for key in [lpk_gui::update::PLACEHOLDER_KEY, ""] {
            let a = app(&url, key);
            let out = tauri::async_runtime::block_on(check(a.handle(), true, true, || {}));
            assert!(matches!(out, Outcome::NoKey), "{out:?}");
        }
        assert!(!connected(&l), "a request was made without a key");
    }

    #[test]
    fn the_check_runs_once_a_day_and_a_skipped_version_is_not_offered() {
        let key = TestKey::new();
        let payload = b"the installer of 9.9.9".to_vec();
        let mut s = Server::new();
        release(
            &mut s,
            "9.9.9",
            ASSET,
            &key.sign(&payload, "9.9.9"),
            &payload,
        );
        let a = app(&s.endpoint(), &key.public());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("update.json");
        let run = |day| {
            tauri::async_runtime::block_on(scheduled(
                a.handle(),
                true,
                Some(&file),
                day,
                true,
                || {},
            ))
        };
        assert!(matches!(run(20_000), Outcome::Available(_)));
        // The same day: no request.
        assert!(matches!(run(20_000), Outcome::NotDue));
        assert_eq!(s.seen().len(), 1);
        // Skipped: the next day's check asks, and does not offer it.
        let mut state = State::load(&file);
        assert_eq!(state.last_check_day, Some(20_000));
        state.skipped = Some("9.9.9".into());
        state.save(&file).unwrap();
        assert!(matches!(run(20_001), Outcome::Skipped(v) if v == "9.9.9"));
        assert_eq!(s.seen().len(), 2);
    }

    #[test]
    fn the_request_carries_no_user_or_host_identifier() {
        let (l, url) = quiet_server();
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
        let a = app(&url, RANDOM_KEY);
        let out = tauri::async_runtime::block_on(check(a.handle(), true, true, || {}));
        assert!(matches!(out, Outcome::UpToDate), "{out:?}");
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

    #[test]
    fn the_test_key_is_a_usable_key() {
        assert!(lpk_gui::update::key_is_usable(&TestKey::new().public()));
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
    }
}
