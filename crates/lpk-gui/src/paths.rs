//! Where the GUI keeps its state and settings (`docs/notes/e5-design.md` section 5,
//! `docs/notes/e7-design.md` sections 3 and 5):
//! - the local state (the journal of interrupted writes, WebView2's data folder, opened
//!   entries) in Tauri's `app_local_data_dir`, `%LOCALAPPDATA%\<identifier>`, out of the
//!   per-user install folder `%LOCALAPPDATA%\LitePack` (E7-5);
//! - the settings in `%APPDATA%\LitePack\settings.json` (`config_dir`, not `app_config_dir`);
//! - with an empty `portable` marker beside `lpk-gui.exe`, both under `<that folder>\data`.
//!
//! Elsewhere (the command layer's tests and tools) `$XDG_STATE_HOME/litepack` and
//! `$XDG_CONFIG_HOME/litepack`. No `dirs` crate (its Unix side carries MPL-2.0 `option-ext`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Tauri's identifier (`tauri.conf.json`; a test compares them): the local state's folder name.
pub const IDENTIFIER: &str = "org.litepack.gui";

/// The settings folder's name under `%APPDATA%`.
pub const SETTINGS_FOLDER: &str = "LitePack";

/// The settings file's name.
pub const SETTINGS_FILE: &str = "settings.json";

/// The marker beside the executable that makes a copy portable.
pub const PORTABLE_MARKER: &str = "portable";

/// The folder a portable copy keeps everything in, beside the executable.
pub const PORTABLE_DATA: &str = "data";

/// The GUI's folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// The local state.
    pub state: Option<PathBuf>,
    /// The settings file.
    pub settings: Option<PathBuf>,
    /// Whether the copy is portable.
    pub portable: bool,
}

/// The folders for an executable in `exe_dir` and the environment `var` reads.
pub fn locate(exe_dir: Option<&Path>, var: impl Fn(&str) -> Option<OsString>) -> Dirs {
    if let Some(dir) = exe_dir.filter(|d| d.join(PORTABLE_MARKER).is_file()) {
        let data = dir.join(PORTABLE_DATA);
        return Dirs {
            state: Some(data.clone()),
            settings: Some(data.join(SETTINGS_FILE)),
            portable: true,
        };
    }
    let home = |rel: &str| var("HOME").map(|h| PathBuf::from(h).join(rel));
    let (state, config) = if cfg!(windows) {
        (
            var("LOCALAPPDATA").map(|d| PathBuf::from(d).join(IDENTIFIER)),
            var("APPDATA").map(|d| PathBuf::from(d).join(SETTINGS_FOLDER)),
        )
    } else {
        (
            var("XDG_STATE_HOME")
                .map(PathBuf::from)
                .or_else(|| home(".local/state"))
                .map(|d| d.join("litepack")),
            var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| home(".config"))
                .map(|d| d.join("litepack")),
        )
    };
    Dirs {
        state,
        settings: config.map(|c| c.join(SETTINGS_FILE)),
        portable: false,
    }
}

/// This process's folders.
pub fn dirs() -> Dirs {
    let exe = std::env::current_exe().ok();
    locate(exe.as_deref().and_then(Path::parent), |k| {
        std::env::var_os(k)
    })
}

/// The state folder, if the environment names one.
pub fn state_dir() -> Option<PathBuf> {
    dirs().state
}

/// The settings file.
pub fn settings_file() -> Option<PathBuf> {
    dirs().settings
}

/// The journal of interrupted writes.
pub fn journal_dir() -> Option<PathBuf> {
    state_dir().map(|d| d.join("journal"))
}

/// The recent archives and destinations (E5-4), Rust's alone.
pub fn recents_file() -> Option<PathBuf> {
    state_dir().map(|d| d.join("recents.json"))
}

/// The updater's state (E7-7): the last check's day and the skipped version, Rust's alone.
pub fn update_state_file() -> Option<PathBuf> {
    state_dir().map(|d| d.join("update.json"))
}

/// WebView2's user data folder.
pub fn webview_dir() -> Option<PathBuf> {
    state_dir().map(|d| d.join("WebView2"))
}

/// The folder opened entries are extracted into, each into a fresh folder of its own
/// (`docs/notes/e5-design.md` section 4), removed at exit.
pub fn open_dir() -> Option<PathBuf> {
    state_dir().map(|d| d.join("Open"))
}

/// The probe file [`check_writable`] writes and removes.
const PROBE: &str = ".litepack-write-check";

/// The probe's size: a few KiB, so that a full disk fails the check (an empty file can fit in
/// the folder's own record).
const PROBE_SIZE: usize = 4096;

/// A portable copy's rule (`docs/notes/e7-design.md` section 5): its `data` folder must exist
/// or be creatable, and be writable. Creates the folder, writes and removes a 4 KiB probe file in
/// it (a `data` junction is followed: where it points is the user's choice);
/// the error names the folder. The caller exits on an error, never falling back to the
/// profile's folders.
pub fn check_writable(data: &Path) -> Result<(), String> {
    let fail = |what: &str, e: std::io::Error| format!("{what} {}: {e}", data.display());
    std::fs::create_dir_all(data).map_err(|e| fail("cannot create", e))?;
    if !data.is_dir() {
        return Err(format!("{} is not a folder", data.display()));
    }
    let probe = data.join(PROBE);
    std::fs::write(&probe, [0u8; PROBE_SIZE]).map_err(|e| fail("cannot write in", e))?;
    std::fs::remove_file(&probe).map_err(|e| fail("cannot write in", e))?;
    Ok(())
}

/// A portable copy's `data` folder when it cannot be used (the error to show); `None` for an
/// installed copy or a usable folder.
pub fn portable_data_error(dirs: &Dirs) -> Option<String> {
    if !dirs.portable {
        return None;
    }
    let data = dirs.state.as_deref()?;
    check_writable(data).err()
}

/// What a portable copy whose `data` folder is unusable writes to the smoke report
/// (`LPK_GUI_SMOKE_REPORT`) instead of showing its box, so that CI never waits on a dialog.
pub fn data_error_report(text: &str) -> String {
    serde_json::json!({ "portable_data_error": text }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_usable_data_folder_is_created_and_left_empty() {
        let exe = tempfile::tempdir().unwrap();
        std::fs::write(exe.path().join(PORTABLE_MARKER), b"").unwrap();
        let d = locate(Some(exe.path()), env(&[]));
        assert_eq!(portable_data_error(&d), None);
        let data = exe.path().join(PORTABLE_DATA);
        assert!(data.is_dir());
        assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0);
    }

    #[test]
    fn a_data_file_in_place_of_the_folder_is_refused() {
        let exe = tempfile::tempdir().unwrap();
        std::fs::write(exe.path().join(PORTABLE_MARKER), b"").unwrap();
        std::fs::write(exe.path().join(PORTABLE_DATA), b"not a folder").unwrap();
        let d = locate(Some(exe.path()), env(&[]));
        let e = portable_data_error(&d).expect("refused");
        assert!(e.contains(PORTABLE_DATA), "{e}");
        // Nothing replaced the file, and nothing else was made.
        assert!(exe.path().join(PORTABLE_DATA).is_file());
        assert_eq!(std::fs::read_dir(exe.path()).unwrap().count(), 2);
    }

    #[test]
    fn the_report_carries_the_error() {
        let r: serde_json::Value = serde_json::from_str(&data_error_report("x \"y\"")).unwrap();
        assert_eq!(r["portable_data_error"], "x \"y\"");
    }

    #[test]
    fn an_installed_copy_is_never_checked() {
        let exe = tempfile::tempdir().unwrap();
        let d = locate(Some(exe.path()), env(&[("LOCALAPPDATA", "L")]));
        assert_eq!(portable_data_error(&d), None);
        assert_eq!(std::fs::read_dir(exe.path()).unwrap().count(), 0);
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn installed_state_and_settings_live_apart() {
        let vars = env(&[
            ("LOCALAPPDATA", "L"),
            ("APPDATA", "R"),
            ("XDG_STATE_HOME", "S"),
            ("XDG_CONFIG_HOME", "C"),
        ]);
        let exe = tempfile::tempdir().unwrap();
        let d = locate(Some(exe.path()), vars);
        assert!(!d.portable);
        if cfg!(windows) {
            assert_eq!(d.state, Some(Path::new("L").join(IDENTIFIER)));
            assert_eq!(
                d.settings,
                Some(Path::new("R").join("LitePack").join("settings.json"))
            );
        } else {
            assert_eq!(d.state, Some(Path::new("S").join("litepack")));
            assert_eq!(
                d.settings,
                Some(Path::new("C/litepack/settings.json").into())
            );
        }
        assert_eq!(locate(None, env(&[])).state, None);
    }

    #[test]
    fn a_portable_copy_keeps_everything_beside_itself() {
        let exe = tempfile::tempdir().unwrap();
        std::fs::write(exe.path().join(PORTABLE_MARKER), b"").unwrap();
        let d = locate(
            Some(exe.path()),
            env(&[("LOCALAPPDATA", "L"), ("APPDATA", "R")]),
        );
        assert!(d.portable);
        assert_eq!(d.state, Some(exe.path().join("data")));
        assert_eq!(
            d.settings,
            Some(exe.path().join("data").join("settings.json"))
        );
    }

    #[test]
    fn the_identifier_is_tauris() {
        let conf: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(conf["identifier"], IDENTIFIER);
    }
}
