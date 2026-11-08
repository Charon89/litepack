//! The page's security settings as constants that the tests (`tests/security.rs`) hold the
//! configuration to (`docs/notes/e5-design.md` section 4).

/// The Content Security Policy (`tauri.conf.json`'s `app.security.csp`; Tauri adds the hashes
/// and nonces of its own scripts): nothing remote, no inline script or style, no `eval`, IPC
/// alone for connections, and Trusted Types with no policy, so that a string reaching an HTML
/// sink throws.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' blob:; font-src 'self'; connect-src ipc: http://ipc.localhost; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; require-trusted-types-for 'script'; trusted-types 'none'";

/// The main window's label (its capability file is `capabilities/main.json`).
pub const MAIN_WINDOW: &str = "main";

/// The environment variable naming a file the host writes, once the page reports that the
/// listing of the archive given on the command line reached the window, before it exits: CI's
/// check of the built exe (`.github/workflows/ci.yml`, job `gui`). Unset, nothing is written.
pub const SMOKE_REPORT_ENV: &str = "LPK_GUI_SMOKE_REPORT";

/// E7-7's update test hook (test 6, N updates to N+1; R327): compiled only with the cargo feature
/// `update-e2e`, which only CI's end-to-end update job turns on, together with the config overlay
/// `tauri.update-e2e.conf.json` (a localhost endpoint and the run's test key). No shipped build
/// has either: the `gui` job checks that the release exe holds neither [`update_e2e::UPDATE_E2E_ENV`]
/// nor the overlay's endpoint.
#[cfg(feature = "update-e2e")]
pub mod update_e2e {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    /// Set to a file path, the host presses "Install and restart" itself as soon as the check
    /// finds a newer version, and writes each step to that file (the job cannot read a windowed
    /// exe's stderr).
    pub const UPDATE_E2E_ENV: &str = "LPK_GUI_UPDATE_E2E";

    /// The local server the job runs in GitHub's place (the overlay's endpoint is under it).
    pub const LOCAL_SERVER: &str = "http://127.0.0.1:8737";

    /// The download prefix of this build: the release prefix's shape on [`LOCAL_SERVER`].
    pub const LOCAL_PREFIX: &str = "http://127.0.0.1:8737/Charon89/litepack/releases/download/";

    /// The log file the hook writes to, if the variable is set to a non-empty value.
    pub fn armed(var: Option<&OsStr>) -> Option<PathBuf> {
        var.filter(|v| !v.is_empty()).map(PathBuf::from)
    }

    /// Append `line` to the hook's log.
    pub fn log(file: &std::path::Path, line: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_overlay_points_at_the_local_server_alone() {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let conf: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join("tauri.update-e2e.conf.json")).unwrap(),
            )
            .unwrap();
            let u = &conf["plugins"]["updater"];
            assert_eq!(
                u["endpoints"],
                serde_json::json!([format!(
                    "{LOCAL_SERVER}/releases/latest/download/latest.json"
                )])
            );
            assert_eq!(u["dangerousInsecureTransportProtocol"], true);
            assert_eq!(u["requireSignedVersion"], true);
            assert!(LOCAL_PREFIX.starts_with(LOCAL_SERVER));
            assert!(LOCAL_PREFIX.ends_with(lpk_gui_prefix_tail()));
        }

        /// The shipped prefix's path, which the local one repeats.
        fn lpk_gui_prefix_tail() -> &'static str {
            crate::update::RELEASE_DOWNLOAD_PREFIX
                .strip_prefix("https://github.com")
                .unwrap()
        }

        #[test]
        fn the_hook_is_armed_only_by_a_non_empty_path() {
            assert_eq!(armed(None), None);
            assert_eq!(armed(Some(OsStr::new(""))), None);
            assert_eq!(
                armed(Some(OsStr::new("C:\\t\\update.log"))),
                Some(PathBuf::from("C:\\t\\update.log"))
            );
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("update.log");
            log(&f, "found 9.9.9");
            log(&f, "installing");
            assert_eq!(
                std::fs::read_to_string(&f).unwrap(),
                "found 9.9.9\ninstalling\n"
            );
        }
    }
}

/// E5-11's WebDriver switch (`docs/notes/e5-design.md` section 7, R111): compiled only with the
/// cargo feature `webdriver`, which CI's `gui-e2e` job turns on and no shipped build does (the
/// `gui` job checks that the release exe does not hold the string `LPK_GUI_WEBDRIVER`).
#[cfg(feature = "webdriver")]
pub mod webdriver {
    use std::ffi::OsStr;

    /// The environment variable that lets the Edge WebDriver attach (CI's `gui-e2e` job;
    /// `crates/lpk-gui-e2e`). The WebView2 runtime does not apply the arguments the driver puts
    /// in [`WEBVIEW2_ARGS_ENV`] when the app sets its own (wry always does), so the session is
    /// refused ("DevToolsActivePort file doesn't exist"). With this variable set to `1`, the host
    /// passes the driver's arguments itself, the allow-listed ones only ([`browser_args`]).
    pub const WEBDRIVER_ENV: &str = "LPK_GUI_WEBDRIVER";

    /// A file the host writes the outcome of [`browser_args`] to (the arguments it passed, or the
    /// refusal), for the harness, which cannot read a windowed exe's stderr.
    pub const WEBDRIVER_LOG_ENV: &str = "LPK_GUI_WEBDRIVER_LOG";

    /// The WebView2 runtime's variable of additional browser arguments, which the Edge WebDriver
    /// sets for the app it launches.
    pub const WEBVIEW2_ARGS_ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

    /// wry's default browser arguments (the "mini menu", the PDF and SmartScreen components off,
    /// and autoplay as Tauri asks for it): kept when the host passes the driver's arguments.
    pub const WRY_DEFAULT_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

    /// The driver's arguments the host accepts as they are; the only other one is a
    /// `--disable-features=` list, merged with wry's.
    pub const ALLOWED_ARGS: [&str; 2] = ["--remote-debugging-port=0", "--enable-automation"];

    const DISABLE_FEATURES: &str = "--disable-features=";

    /// Whether `arg` is one the driver needs: [`ALLOWED_ARGS`], or a `--disable-features=` list
    /// of feature names (letters, digits and `_`).
    fn allowed(arg: &str) -> bool {
        ALLOWED_ARGS.contains(&arg)
            || arg.strip_prefix(DISABLE_FEATURES).is_some_and(|list| {
                list.split(',').all(|f| {
                    !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
            })
    }

    /// The browser arguments for the window when [`WEBDRIVER_ENV`] is `1` and
    /// [`WEBVIEW2_ARGS_ENV`] is set: wry's defaults and the driver's, with their
    /// `--disable-features` lists merged into one switch (Chromium keeps only the last of a
    /// repeated switch). `Ok(None)` otherwise, and the window keeps wry's defaults.
    ///
    /// # Errors
    /// Every driver argument that is not one the driver needs ([`ALLOWED_ARGS`] and a
    /// `--disable-features=` list), each named in the message; also a value that is not Unicode.
    pub fn browser_args(
        gate: Option<&OsStr>,
        driver_args: Option<&OsStr>,
    ) -> Result<Option<String>, String> {
        if gate.is_none_or(|g| g != "1") {
            return Ok(None);
        }
        let Some(driver_args) = driver_args else {
            return Ok(None);
        };
        let driver_args = driver_args
            .to_str()
            .ok_or_else(|| format!("{WEBVIEW2_ARGS_ENV} is not Unicode; refused"))?;
        let refused: Vec<String> = driver_args
            .split_whitespace()
            .filter(|a| !allowed(a))
            .map(|a| format!("`{a}`"))
            .collect();
        if !refused.is_empty() {
            return Err(format!(
                "{WEBVIEW2_ARGS_ENV}: not arguments the WebDriver needs, refused: {}",
                refused.join(" ")
            ));
        }
        let mut features: Vec<&str> = Vec::new();
        let mut rest: Vec<&str> = Vec::new();
        for arg in WRY_DEFAULT_ARGS
            .split_whitespace()
            .chain(driver_args.split_whitespace())
        {
            match arg.strip_prefix(DISABLE_FEATURES) {
                Some(list) => {
                    for f in list.split(',').filter(|f| !f.is_empty()) {
                        if !features.contains(&f) {
                            features.push(f);
                        }
                    }
                }
                None => {
                    if !rest.contains(&arg) {
                        rest.push(arg);
                    }
                }
            }
        }
        let mut out = vec![format!("{DISABLE_FEATURES}{}", features.join(","))];
        out.extend(rest.into_iter().map(str::to_owned));
        Ok(Some(out.join(" ")))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const DRIVER: &str = "--disable-features=IgnoreDuplicateNavs,Prewarm --enable-automation --remote-debugging-port=0";

        fn args(gate: Option<&str>, driver: Option<&str>) -> Result<Option<String>, String> {
            browser_args(gate.map(OsStr::new), driver.map(OsStr::new))
        }

        #[test]
        fn without_the_gate_the_window_keeps_wrys_defaults() {
            assert_eq!(args(None, Some(DRIVER)), Ok(None));
            assert_eq!(args(Some("0"), Some(DRIVER)), Ok(None));
            assert_eq!(args(Some("1"), None), Ok(None));
            // Not even a refused argument matters without the gate.
            assert_eq!(args(None, Some("--no-sandbox")), Ok(None));
        }

        #[test]
        fn with_the_gate_the_drivers_arguments_join_wrys_and_the_feature_lists_merge() {
            let a = args(Some("1"), Some(DRIVER))
                .expect("allowed")
                .expect("arguments");
            assert_eq!(
                a,
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,IgnoreDuplicateNavs,Prewarm \
                 --autoplay-policy=no-user-gesture-required --enable-automation --remote-debugging-port=0"
            );
            assert_eq!(a.matches("--disable-features=").count(), 1);
        }

        #[test]
        fn the_allow_list_refuses_every_other_argument_and_names_it() {
            for bad in [
                "--remote-debugging-port=9222",
                "--remote-allow-origins=*",
                "--disable-web-security",
                "--no-sandbox",
                "--autoplay-policy=no-user-gesture-required",
                "--disable-features=",
                "--disable-features=A,,B",
                "--disable-features=A;--no-sandbox",
                "--enable-features=Foo",
                "--remote-debugging-port=0=",
            ] {
                let driver = format!("--enable-automation {bad} --remote-debugging-port=0");
                let err = args(Some("1"), Some(&driver)).expect_err(bad);
                assert!(err.contains(&format!("`{bad}`")), "{err}");
            }
        }

        #[test]
        fn each_allowed_argument_alone_passes() {
            for ok in [
                "--remote-debugging-port=0",
                "--enable-automation",
                "--disable-features=Prewarm",
                "",
            ] {
                assert!(matches!(args(Some("1"), Some(ok)), Ok(Some(_))), "{ok}");
            }
        }
    }
}
