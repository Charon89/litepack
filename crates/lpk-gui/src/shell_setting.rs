//! The settings area's "Open with" registration (E6-4 part 2): the command layer behind the
//! checkbox and the "Default apps…" button. The installer (or `--register-shell`) registers a
//! copy; this module only lets *that* copy flip the opt-in "Open with" entries and open the
//! Settings page where the user may choose LitePack as a default.
//!
//! Both controls exist only when this copy is the registered one: the record
//! (`HKCU\Software\LitePack\Registration`) exists, the `GuiPath` it wrote equals the running
//! executable (a Windows path, compared case-insensitively) and no `portable` marker lies beside
//! it. A portable copy, a development build, another copy's registration or none at all: the
//! status says `registered: false` and every other call is refused, so the GUI never registers a
//! copy the installer did not.
//!
//! The registry and the Settings launch are reached through the seam [`ShellSetting`] holds; the
//! host builds the real one (`ShellSetting::system`), tests inject an in-memory registry.

use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use lpk_api::Class;
use serde::Serialize;

use crate::paths::PORTABLE_MARKER;
use crate::register::{
    self, encode_value, normalize_path, open_with_extensions, RegValue, RegisterError, Registry,
    CLASSES, LITEPACK, RECORD,
};
use crate::GuiError;

/// The page `open_default_apps` opens: fixed, nothing from the page goes into it. Windows 11
/// shows LitePack's own page; Windows 10 may open the general one (E6-7 records it).
pub const DEFAULT_APPS_URI: &str = "ms-settings:defaultapps?registeredAppUser=LitePack";

/// What the page learns: whether to show the controls, and the checkbox's state and label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellStatus {
    /// This copy is the registered one (the controls show only then).
    pub registered: bool,
    /// The record holds the opt-in "Open with" entries.
    pub open_with: bool,
    /// The extensions the opt-in covers (`register::open_with_extensions`).
    pub formats: Vec<String>,
}

/// Opens the registry: called once per command, on its blocking thread.
pub type OpenRegistry = Box<dyn Fn() -> Box<dyn Registry> + Send + Sync>;
/// Opens a Settings URI.
pub type Launch = Box<dyn Fn(&str) -> io::Result<()> + Send + Sync>;

/// The seam: the running executable, the registry and the Settings launch.
pub struct ShellSetting {
    exe: PathBuf,
    registry: OpenRegistry,
    launch: Launch,
    /// One re-registration at a time.
    busy: Mutex<()>,
}

impl std::fmt::Debug for ShellSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellSetting")
            .field("exe", &self.exe)
            .finish_non_exhaustive()
    }
}

/// Whether two paths name the same file: when both exist, their canonical forms (so an 8.3 name
/// and the long one match, and the file system decides the case); otherwise a comparison of the
/// text as a Windows path, case-insensitive.
fn same_path(a: &str, b: &str) -> bool {
    if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return ca == cb;
    }
    let norm = |p: &str| {
        normalize_path(p)
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

fn formats() -> Vec<String> {
    open_with_extensions()
        .into_iter()
        .map(String::from)
        .collect()
}

impl ShellSetting {
    /// A seam over `registry` and `launch` for the executable at `exe`.
    pub fn new(exe: PathBuf, registry: OpenRegistry, launch: Launch) -> Self {
        Self {
            exe,
            registry,
            launch,
            busy: Mutex::new(()),
        }
    }

    /// The real one: `HKEY_CURRENT_USER`, `ShellExecuteExW`, the running executable. The only
    /// constructor that reaches the real registry. `None` when the executable is unknown.
    #[cfg(windows)]
    pub fn system() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        Some(Self::new(
            exe,
            Box::new(register::real_registry),
            Box::new(lpk_gui_sys::open_settings_uri),
        ))
    }

    /// `Some(open_with)` when this copy is the registered one.
    fn read(&self) -> Option<bool> {
        let dir = self.exe.parent()?;
        if dir.join(PORTABLE_MARKER).is_file() {
            return None;
        }
        let reg = (self.registry)();
        if !reg.key_exists(RECORD).ok()? {
            return None;
        }
        let Some(RegValue::Sz(gui)) = reg.get_value(LITEPACK, "GuiPath").ok()? else {
            return None;
        };
        if !same_path(&gui, self.exe.to_str()?) {
            return None;
        }
        let record: Vec<String> = reg
            .list_values(RECORD)
            .ok()?
            .into_iter()
            .filter_map(|(_, v)| match v {
                RegValue::Sz(t) => Some(t),
                _ => None,
            })
            .collect();
        let exts = open_with_extensions();
        let open_with = !exts.is_empty()
            && exts.iter().all(|ext| {
                let entry = encode_value(
                    &format!("{CLASSES}\\.{ext}\\OpenWithProgids"),
                    &format!("LitePack.{ext}"),
                    &RegValue::None,
                );
                record.contains(&entry)
            });
        Some(open_with)
    }

    /// The status (blocking: it reads the registry).
    pub fn status(&self) -> ShellStatus {
        let read = self.read();
        ShellStatus {
            registered: read.is_some(),
            open_with: read == Some(true),
            formats: formats(),
        }
    }

    /// Re-register in process with `--register-shell [--open-with]`'s code path (undo the
    /// record, register afresh, notify), only when this copy is the registered one. A failure
    /// leaves the record consistent (`register`'s guarantee: `--unregister-shell` undoes it).
    pub fn set_open_with(&self, on: bool) -> Result<ShellStatus, GuiError> {
        let _one = self
            .busy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.read().is_none() {
            return Err(GuiError::usage(
                "this copy of LitePack is not the registered one",
            ));
        }
        let mut reg = (self.registry)();
        match register::register_for(&self.exe, on, &mut *reg) {
            Ok(()) => {}
            Err(RegisterError::Refused(why)) => {
                return Err(GuiError::usage(format!(
                    "the registration was refused: {why}"
                )));
            }
            // The cause goes to the page in the detail (a release build has no console); the
            // record names what was written, so `--unregister-shell` or a reinstall repairs it.
            Err(RegisterError::Failed(e)) => {
                return Err(lpk_api::Error::new(
                    Class::Internal,
                    "RegisterFailed",
                    e.to_string(),
                    None,
                )
                .into());
            }
        }
        drop(reg);
        Ok(self.status())
    }

    /// Open the default-apps page of Settings, only when this copy is the registered one.
    pub fn open_default_apps(&self) -> Result<(), GuiError> {
        if self.read().is_none() {
            return Err(GuiError::usage(
                "this copy of LitePack is not the registered one",
            ));
        }
        (self.launch)(DEFAULT_APPS_URI).map_err(|e| {
            lpk_api::Error::new(
                Class::Internal,
                "ShellSetting",
                format!("Settings could not be opened: {e}"),
                None,
            )
            .into()
        })
    }
}

#[cfg(test)]
#[path = "shell_setting_tests.rs"]
mod tests;
