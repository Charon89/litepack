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
