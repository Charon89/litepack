//! Where the GUI keeps its state (`docs/notes/e5-design.md` section 5): the journal of
//! interrupted writes and WebView2's data folder under `%LOCALAPPDATA%\LitePack` on Windows;
//! `$XDG_STATE_HOME/litepack` (or `~/.local/state/litepack`) elsewhere, for the command
//! layer's tests and tools. No `dirs` crate (its Unix side carries MPL-2.0 `option-ext`).

use std::path::PathBuf;

/// The state folder, if the environment names one.
pub fn state_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("LitePack"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .map(|d| d.join("litepack"))
    }
}

/// The journal of interrupted writes.
pub fn journal_dir() -> Option<PathBuf> {
    state_dir().map(|d| d.join("journal"))
}

/// WebView2's user data folder.
pub fn webview_dir() -> Option<PathBuf> {
    state_dir().map(|d| d.join("WebView2"))
}
