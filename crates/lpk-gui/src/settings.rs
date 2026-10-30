//! The GUI's settings (E5-7; `docs/notes/e5-design.md` sections 4 and 5): a versioned JSON file
//! ([`crate::paths::settings_file`]) written temp-then-rename. A missing file gives the
//! defaults; an unreadable, unknown or newer one gives the defaults and one warning, never a
//! crash, and is left as it is until the user changes a setting.
//!
//! The page reads and writes [`Settings`], which has no path field and refuses unknown fields:
//! recent archives and destinations are Rust's to record (section 4), so a path sent through
//! `set_settings` is refused at deserialization.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::GuiError;

/// The file format's version.
pub const VERSION: u32 = 1;

/// What the user sets (and all the page may read or write).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The update check, the GUI's one network request (decision (d): on by default).
    pub update_check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { update_check: true }
    }
}

/// The file: the version beside the settings.
#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    #[serde(flatten)]
    settings: &'a Settings,
}

/// Settings read from disk, with the warning to show once if the file was not usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// The settings in force.
    pub settings: Settings,
    /// Why the defaults are in force instead of the file, if they are.
    pub warning: Option<String>,
}

/// Read the settings at `path`.
pub fn load(path: &Path) -> Loaded {
    let fallback = |why: String| Loaded {
        settings: Settings::default(),
        warning: Some(format!("{}: {why}; the defaults are used", path.display())),
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::default(),
                warning: None,
            }
        }
        Err(e) => return fallback(e.to_string()),
    };
    let mut value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return fallback(format!("not valid JSON ({e})")),
    };
    let version = value
        .as_object_mut()
        .and_then(|o| o.remove("version"))
        .and_then(|v| v.as_u64());
    match version {
        Some(v) if v == u64::from(VERSION) => {}
        Some(v) => return fallback(format!("version {v}, this LitePack reads {VERSION}")),
        None => return fallback("no version".into()),
    }
    match serde_json::from_value::<Settings>(value) {
        Ok(settings) => Loaded {
            settings,
            warning: None,
        },
        Err(e) => fallback(e.to_string()),
    }
}

/// Write `settings` to `path`: a temporary file beside it, flushed, then renamed over it.
pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let body = serde_json::to_vec_pretty(&FileOut {
        version: VERSION,
        settings,
    })
    .map_err(std::io::Error::other)?;
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The settings of a running GUI: what is in force and where it is saved.
#[derive(Debug)]
pub struct Store {
    path: Option<PathBuf>,
    current: Mutex<Settings>,
}

impl Store {
    /// The settings at `path` (none: the defaults, kept in memory only), and the load's warning.
    pub fn open(path: Option<PathBuf>) -> (Self, Option<String>) {
        let loaded = path.as_deref().map(load).unwrap_or(Loaded {
            settings: Settings::default(),
            warning: None,
        });
        (
            Store {
                path,
                current: Mutex::new(loaded.settings),
            },
            loaded.warning,
        )
    }

    /// The settings in force.
    pub fn get(&self) -> Settings {
        #[allow(clippy::unwrap_used)]
        *self.current.lock().unwrap()
    }

    /// Change the settings: saved first, in force once saved.
    pub fn set(&self, settings: Settings) -> Result<(), GuiError> {
        #[allow(clippy::unwrap_used)]
        let mut current = self.current.lock().unwrap();
        if let Some(p) = &self.path {
            save(p, &settings).map_err(|e| crate::io_error(e, p))?;
        }
        *current = settings;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing_and_the_check_is_on() {
        let d = tempfile::tempdir().unwrap();
        let l = load(&d.path().join("settings.json"));
        assert_eq!(l.settings, Settings::default());
        assert!(l.settings.update_check);
        assert_eq!(l.warning, None);
    }

    #[test]
    fn a_save_round_trips_through_a_temporary_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("settings.json");
        let off = Settings {
            update_check: false,
        };
        save(&p, &off).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], VERSION);
        assert_eq!(v["update_check"], false);
        assert_eq!(load(&p).settings, off);
        // Replacing an existing file; no temporary file left behind.
        save(&p, &Settings::default()).unwrap();
        assert_eq!(load(&p).settings, Settings::default());
        let left: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1);
    }

    #[test]
    fn unusable_files_give_the_defaults_and_one_warning() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        for body in [
            "{not json",
            "[]",
            r#"{"update_check": false}"#,
            r#"{"version": 2, "update_check": false}"#,
            r#"{"version": 1, "update_check": "no"}"#,
            r#"{"version": 1, "update_check": false, "future": 1}"#,
        ] {
            std::fs::write(&p, body).unwrap();
            let l = load(&p);
            assert_eq!(l.settings, Settings::default(), "{body}");
            assert!(l.warning.is_some(), "{body}");
        }
        let (store, warning) = Store::open(Some(p.clone()));
        assert!(warning.unwrap().contains("defaults"));
        assert!(store.get().update_check);
        // The unusable file stays until the user changes a setting.
        assert!(std::fs::read_to_string(&p).unwrap().contains("future"));
        store
            .set(Settings {
                update_check: false,
            })
            .unwrap();
        assert!(!load(&p).settings.update_check);
        assert!(!store.get().update_check);
    }

    #[test]
    fn the_page_cannot_send_a_path() {
        for sent in [
            serde_json::json!({"update_check": true, "recent": "C:\\Users\\x\\a.lpk"}),
            serde_json::json!({"update_check": true, "destination": "/etc"}),
            serde_json::json!({"update_check": true, "version": 1}),
        ] {
            assert!(
                serde_json::from_value::<Settings>(sent.clone()).is_err(),
                "{sent}"
            );
        }
        assert!(
            serde_json::from_value::<Settings>(serde_json::json!({"update_check": false})).is_ok()
        );
    }
}
