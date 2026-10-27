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

/// The environment variable that lets the Edge WebDriver attach (E5-11's driver harness, CI's
/// `gui-e2e` job; `crates/lpk-gui-e2e`). The WebView2 runtime does not apply the arguments the
/// driver puts in [`WEBVIEW2_ARGS_ENV`] when the app sets its own (wry always does), so the
/// session is refused ("DevToolsActivePort file doesn't exist"). With this variable set to `1`,
/// the host passes them itself ([`webdriver_browser_args`]). Unset, nothing changes.
pub const WEBDRIVER_ENV: &str = "LPK_GUI_WEBDRIVER";

/// The WebView2 runtime's variable of additional browser arguments, which the Edge WebDriver sets
/// for the app it launches (`--remote-debugging-port=0` among them).
pub const WEBVIEW2_ARGS_ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

/// wry's default browser arguments (the "mini menu", the PDF and SmartScreen components off, and
/// autoplay as Tauri asks for it): kept when the host passes the driver's arguments.
pub const WRY_DEFAULT_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

/// The browser arguments for the window when [`WEBDRIVER_ENV`] is `1` and [`WEBVIEW2_ARGS_ENV`]
/// is set: wry's defaults and the driver's, with their `--disable-features` lists merged into
/// one switch (Chromium keeps only the last of a repeated switch). `None` otherwise, and the
/// window keeps wry's defaults.
pub fn webdriver_browser_args(
    gate: Option<&std::ffi::OsStr>,
    driver_args: Option<&std::ffi::OsStr>,
) -> Option<String> {
    if gate.is_none_or(|g| g != "1") {
        return None;
    }
    let driver_args = driver_args?.to_str()?;
    let mut features: Vec<&str> = Vec::new();
    let mut rest: Vec<&str> = Vec::new();
    for arg in WRY_DEFAULT_ARGS
        .split_whitespace()
        .chain(driver_args.split_whitespace())
    {
        match arg.strip_prefix("--disable-features=") {
            Some(list) => {
                for f in list.split(',').filter(|f| !f.is_empty()) {
                    if !features.contains(&f) {
                        features.push(f);
                    }
                }
            }
            None => rest.push(arg),
        }
    }
    let mut out = vec![format!("--disable-features={}", features.join(","))];
    out.extend(rest.into_iter().map(str::to_owned));
    Some(out.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    const DRIVER: &str =
        "--disable-features=IgnoreDuplicateNavs,Prewarm --enable-automation --remote-debugging-port=0";

    #[test]
    fn without_the_gate_the_window_keeps_wrys_defaults() {
        assert_eq!(webdriver_browser_args(None, Some(OsStr::new(DRIVER))), None);
        assert_eq!(
            webdriver_browser_args(Some(OsStr::new("0")), Some(OsStr::new(DRIVER))),
            None
        );
        assert_eq!(webdriver_browser_args(Some(OsStr::new("1")), None), None);
    }

    #[test]
    fn with_the_gate_the_drivers_arguments_join_wrys_and_the_feature_lists_merge() {
        let args = webdriver_browser_args(Some(OsStr::new("1")), Some(OsStr::new(DRIVER)))
            .expect("arguments");
        assert_eq!(
            args,
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,IgnoreDuplicateNavs,Prewarm \
             --autoplay-policy=no-user-gesture-required --enable-automation --remote-debugging-port=0"
        );
        assert_eq!(args.matches("--disable-features=").count(), 1);
    }
}
