//! `lpk-gui.exe --register-shell [--open-with]` and `--unregister-shell` (E6-4;
//! `docs/notes/e6-design.md` section 3): the per-user registration of Explorer's classic menu
//! (route C), the `.lpk` association, the opt-in "Open with" entries and Default Programs, and its
//! undoing, leaving the registry as found.
//!
//! The model is safe code over the small [`Registry`] trait. Its keys are relative to a base the
//! caller chose: the real `HKEY_CURRENT_USER` only in [`handle`] (Windows), an in-memory fake in
//! the unit tests. Nothing here writes `UserChoice` or anything under `FileExts`.
//!
//! **As found.** `Software\LitePack\Registration` records every key registration created and
//! every value it wrote with its data, each *before* it is created or set (write ahead), so a run
//! that fails halfway is undone by `--unregister-shell`. Unregistering deletes each recorded
//! value whose data is still the recorded data, then each recorded key, deepest first, only when
//! it holds no value and no subkey, then the record. A value that is not LitePack's (`.lpk`'s
//! default, its `PerceivedType`) is never overwritten when it holds someone else's data.
//!
//! The check for an installed Store package ("one entry per menu", design section 3) is E6-3's and
//! E7-4's; this module registers route C unconditionally.

use std::ffi::OsString;
use std::io;
use std::path::Path;

use crate::paths::PORTABLE_MARKER;

/// The classic route's class ID, `{...}` in upper case: distinct from the package's
/// `CLSID_LPK_SHELL`; `lpk-shell` serves both (`crates/lpk-shell/src/com.rs`, which a test
/// compares with this text).
pub const CLASSIC_CLSID: &str = "{01E42821-7D51-4B9B-AD9F-FFCA769A6025}";

/// The DLL beside `lpk-gui.exe` that registration points the classic class at.
pub const SHELL_DLL: &str = "lpk_shell.dll";

/// The foreign extensions registration may list in "Open with" (`--open-with`), one per line:
/// the formats the engine opens today (a test in `lpk-api` keeps the file equal to them).
pub const OPEN_WITH_EXTENSIONS: &str = include_str!("open_with_extensions.txt");

pub(crate) const LITEPACK: &str = "Software\\LitePack";
pub(crate) const RECORD: &str = "Software\\LitePack\\Registration";
const CAPABILITIES: &str = "Software\\LitePack\\Capabilities";
const REGISTERED_APPS: &str = "Software\\RegisteredApplications";
pub(crate) const CLASSES: &str = "Software\\Classes";
const PROGID_LPK: &str = "LitePack.lpk";

/// A registry value, as registration sees one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegValue {
    /// `REG_SZ`.
    Sz(String),
    /// `REG_NONE`, no data (what `OpenWithProgids` entries are).
    None,
    /// Any other type; never written, never equal to ours.
    Other(u32),
}

/// The registry calls registration makes; keys are paths from the base the implementation was
/// built on (`Software\Classes\...`), the empty `name` is a key's default value.
pub trait Registry {
    /// Whether the key exists.
    fn key_exists(&self, path: &str) -> io::Result<bool>;
    /// Create the key and any missing ancestor.
    fn create_key(&mut self, path: &str) -> io::Result<()>;
    /// A value; `None` when the key or value is absent.
    fn get_value(&self, path: &str, name: &str) -> io::Result<Option<RegValue>>;
    /// Set a value (creating the key).
    fn set_value(&mut self, path: &str, name: &str, value: &RegValue) -> io::Result<()>;
    /// Delete a value; absent is fine.
    fn delete_value(&mut self, path: &str, name: &str) -> io::Result<()>;
    /// A key's values; none when it is absent.
    fn list_values(&self, path: &str) -> io::Result<Vec<(String, RegValue)>>;
    /// Delete the key only when it has no value and no subkey; `Ok(true)` when it is gone.
    fn delete_key_if_empty(&mut self, path: &str) -> io::Result<bool>;
    /// `SHChangeNotify(SHCNE_ASSOCCHANGED)`.
    fn notify_assoc_changed(&mut self);
}

/// The extensions of [`OPEN_WITH_EXTENSIONS`].
pub fn open_with_extensions() -> Vec<&'static str> {
    OPEN_WITH_EXTENSIONS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// What to do with a value that already exists with other data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    /// LitePack's own: replaced.
    Own,
    /// Someone else's may be there: left alone.
    WhenAbsent,
}

struct Writer<'a, R: Registry + ?Sized> {
    reg: &'a mut R,
    seq: u32,
}

impl<R: Registry + ?Sized> Writer<'_, R> {
    fn record(&mut self, entry: &str) -> io::Result<()> {
        self.seq += 1;
        let name = format!("e{:06}", self.seq);
        self.reg
            .set_value(RECORD, &name, &RegValue::Sz(entry.to_string()))
    }

    /// Create `path` and each ancestor missing, recording each before creating it.
    fn ensure_key(&mut self, path: &str) -> io::Result<()> {
        let mut prefix = String::new();
        for part in path.split('\\') {
            if !prefix.is_empty() {
                prefix.push('\\');
            }
            prefix.push_str(part);
            if !self.reg.key_exists(&prefix)? {
                self.record(&format!("K\t{prefix}"))?;
                self.reg.create_key(&prefix)?;
            }
        }
        Ok(())
    }

    fn put(&mut self, key: &str, name: &str, value: RegValue, policy: Policy) -> io::Result<()> {
        self.ensure_key(key)?;
        match self.reg.get_value(key, name)? {
            Some(old) if old == value => return Ok(()),
            Some(_) if policy == Policy::WhenAbsent => return Ok(()),
            _ => {}
        }
        self.record(&encode_value(key, name, &value))?;
        self.reg.set_value(key, name, &value)
    }

    fn sz(&mut self, key: &str, name: &str, data: &str) -> io::Result<()> {
        self.put(key, name, RegValue::Sz(data.to_string()), Policy::Own)
    }

    fn progid(&mut self, progid: &str, friendly: &str, gui: &str) -> io::Result<()> {
        let key = format!("{CLASSES}\\{progid}");
        self.sz(&key, "", friendly)?;
        self.sz(&format!("{key}\\DefaultIcon"), "", &format!("\"{gui}\",0"))?;
        self.sz(
            &format!("{key}\\shell\\open\\command"),
            "",
            &format!("\"{gui}\" --open \"%1\""),
        )
    }

    fn with_progid(&mut self, ext: &str, progid: &str) -> io::Result<()> {
        self.put(
            &format!("{CLASSES}\\.{ext}\\OpenWithProgids"),
            progid,
            RegValue::None,
            Policy::Own,
        )
    }
}

pub(crate) fn encode_value(key: &str, name: &str, value: &RegValue) -> String {
    match value {
        RegValue::Sz(d) => format!("V\tS\t{key}\t{name}\t{d}"),
        _ => format!("V\tN\t{key}\t{name}\t"),
    }
}

fn decode_value(rest: &str) -> Option<(String, String, RegValue)> {
    let mut it = rest.splitn(4, '\t');
    let kind = it.next()?;
    let key = it.next()?.to_string();
    let name = it.next()?.to_string();
    let data = it.next()?;
    let value = match kind {
        "S" => RegValue::Sz(data.to_string()),
        "N" => RegValue::None,
        _ => return None,
    };
    Some((key, name, value))
}

/// Register for the current user: undo the previous registration, then write the keys of the
/// module's header for the executable at `gui` and the shell DLL at `dll` (absolute paths), with
/// the "Open with" entries for [`open_with_extensions`] when `open_with`. Ends with the
/// association-changed notification, also after a failure (what was written is in the record).
pub fn register<R: Registry + ?Sized>(
    reg: &mut R,
    gui: &str,
    dll: &str,
    open_with: bool,
) -> io::Result<()> {
    let result = undo(reg).and_then(|()| apply(reg, gui, dll, open_with));
    reg.notify_assoc_changed();
    result
}

/// Undo the recorded registration, then notify.
pub fn unregister<R: Registry + ?Sized>(reg: &mut R) -> io::Result<()> {
    let result = undo(reg);
    reg.notify_assoc_changed();
    result
}

fn apply<R: Registry + ?Sized>(
    reg: &mut R,
    gui: &str,
    dll: &str,
    open_with: bool,
) -> io::Result<()> {
    // The record cannot note, before it exists, the keys it needs: the missing ones of
    // `Software\LitePack` are noted in it as soon as it does.
    let mut missing = Vec::new();
    let mut prefix = String::new();
    for part in LITEPACK.split('\\') {
        if !prefix.is_empty() {
            prefix.push('\\');
        }
        prefix.push_str(part);
        if !reg.key_exists(&prefix)? {
            missing.push(prefix.clone());
        }
    }
    reg.create_key(RECORD)?;
    let mut w = Writer { reg, seq: 0 };
    if !missing.is_empty() {
        // One entry for all of them, so that no run notes only some.
        w.record(&format!("K	{}", missing.join("	")))?;
    }
    w.sz(LITEPACK, "GuiPath", gui)?;

    // Route C: the classic menu's class and the three shell keys.
    let clsid = format!("{CLASSES}\\CLSID\\{CLASSIC_CLSID}");
    w.sz(&clsid, "", "LitePack")?;
    let inproc = format!("{clsid}\\InProcServer32");
    w.sz(&inproc, "", dll)?;
    w.sz(&inproc, "ThreadingModel", "Apartment")?;
    for target in ["*", "Directory", "Directory\\Background"] {
        let key = format!("{CLASSES}\\{target}\\shell\\LitePack");
        w.sz(&key, "ExplorerCommandHandler", CLASSIC_CLSID)?;
        w.sz(&key, "MultiSelectModel", "Player")?;
    }

    // `.lpk`.
    w.progid(PROGID_LPK, "LitePack archive", gui)?;
    let lpk = format!("{CLASSES}\\.lpk");
    w.put(
        &lpk,
        "",
        RegValue::Sz(PROGID_LPK.to_string()),
        Policy::WhenAbsent,
    )?;
    w.put(
        &lpk,
        "PerceivedType",
        RegValue::Sz("compressed".to_string()),
        Policy::WhenAbsent,
    )?;
    w.with_progid("lpk", PROGID_LPK)?;

    // The opt-in "Open with" entries: never a default.
    let exts = if open_with {
        open_with_extensions()
    } else {
        Vec::new()
    };
    for ext in &exts {
        let progid = format!("LitePack.{ext}");
        let friendly = format!("{} archive (LitePack)", ext.to_uppercase());
        w.progid(&progid, &friendly, gui)?;
        w.with_progid(ext, &progid)?;
    }

    // Default Programs.
    w.sz(CAPABILITIES, "ApplicationName", "LitePack")?;
    w.sz(
        CAPABILITIES,
        "ApplicationDescription",
        "LitePack archiver: .lpk archives and other archive formats",
    )?;
    let assoc = format!("{CAPABILITIES}\\FileAssociations");
    w.sz(&assoc, ".lpk", PROGID_LPK)?;
    for ext in &exts {
        w.sz(&assoc, &format!(".{ext}"), &format!("LitePack.{ext}"))?;
    }
    w.sz(REGISTERED_APPS, "LitePack", CAPABILITIES)
}

fn undo<R: Registry + ?Sized>(reg: &mut R) -> io::Result<()> {
    let mut entries = reg.list_values(RECORD)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut values = Vec::new();
    let mut keys = Vec::new();
    for (_, v) in &entries {
        let RegValue::Sz(text) = v else { continue };
        if let Some(paths) = text.strip_prefix("K\t") {
            keys.extend(paths.split('\t').map(String::from));
        } else if let Some(rest) = text.strip_prefix("V\t") {
            values.extend(decode_value(rest));
        }
    }
    // Our values first (newest first), only where the data is still ours.
    for (key, name, value) in values.iter().rev() {
        if reg.get_value(key, name)?.as_ref() == Some(value) {
            reg.delete_value(key, name)?;
        }
    }
    // Deepest keys first; a key with anything in it stays.
    keys.sort_by_key(|k| std::cmp::Reverse(k.matches('\\').count()));
    for key in &keys {
        reg.delete_key_if_empty(key)?;
    }
    // The record last, so that a failure above can be retried; then what only it was holding up.
    // Newest first: the first entry, the one that names `Software\LitePack`, goes last.
    for (name, _) in entries.iter().rev() {
        reg.delete_value(RECORD, name)?;
    }
    reg.delete_key_if_empty(RECORD)?;
    for key in &keys {
        reg.delete_key_if_empty(key)?;
    }
    if entries.is_empty() {
        // No record, or one with nothing in it (a run that failed between creating the record and
        // its first entry, or a retry after the record's last key failed to delete): LitePack's
        // own key, if empty, is residue.
        reg.delete_key_if_empty(LITEPACK)?;
    }
    Ok(())
}

/// A shell verb of the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellVerb {
    /// `--register-shell`, with `--open-with` or not.
    Register {
        /// Also list the foreign formats in "Open with".
        open_with: bool,
    },
    /// `--unregister-shell`.
    Unregister,
}

/// The exit codes of the verbs (docs/API.md).
pub mod exit {
    /// Done.
    pub const DONE: i32 = 0;
    /// Failed: what was written is in the record, `--unregister-shell` undoes it.
    pub const FAILED: i32 = 1;
    /// Refused: nothing was written.
    pub const REFUSED: i32 = 2;
}

/// Look for a shell verb in the literal command line (`args` without the program name; `@file`
/// contents are never read here). `None` when no argument is one, so that the GUI starts as
/// usual; `Some(Err(..))` when one is mixed with anything but its own `--open-with`.
pub fn parse_shell_verb(args: &[OsString]) -> Option<Result<ShellVerb, String>> {
    let is_verb = |a: &OsString| a == "--register-shell" || a == "--unregister-shell";
    if !args.iter().any(is_verb) {
        return None;
    }
    let Some(strs) = args
        .iter()
        .map(|a| a.to_str())
        .collect::<Option<Vec<&str>>>()
    else {
        return Some(Err("an argument is not valid Unicode".to_string()));
    };
    let verb =
        match strs.as_slice() {
            ["--register-shell"] => ShellVerb::Register { open_with: false },
            ["--register-shell", "--open-with"] | ["--open-with", "--register-shell"] => {
                ShellVerb::Register { open_with: true }
            }
            ["--unregister-shell"] => ShellVerb::Unregister,
            _ => return Some(Err(
                "usage: lpk-gui --register-shell [--open-with] | --unregister-shell (nothing else)"
                    .to_string(),
            )),
        };
    Some(Ok(verb))
}

/// Why [`register_for`] did not register.
#[derive(Debug)]
pub enum RegisterError {
    /// Refused before anything was written (the reason).
    Refused(&'static str),
    /// Failed part-way: what was written is in the record, `--unregister-shell` undoes it.
    Failed(io::Error),
}

/// `--register-shell [--open-with]` for the executable at `exe`, returning the cause instead of
/// printing it: the checks of [`run_verb`] (no folder, a portable copy, `lpk_shell.dll` missing,
/// a path that is not Unicode or holds `%`), then [`register`].
pub fn register_for<R: Registry + ?Sized>(
    exe: &Path,
    open_with: bool,
    reg: &mut R,
) -> Result<(), RegisterError> {
    let Some(dir) = exe.parent() else {
        return Err(RegisterError::Refused("the executable has no folder"));
    };
    if dir.join(PORTABLE_MARKER).is_file() {
        return Err(RegisterError::Refused(
            "a portable copy does not register itself with the shell",
        ));
    }
    let dll = dir.join(SHELL_DLL);
    if !dll.is_file() {
        return Err(RegisterError::Refused(
            "lpk_shell.dll is missing beside the executable",
        ));
    }
    let (Some(gui), Some(dll)) = (exe.to_str(), dll.to_str()) else {
        return Err(RegisterError::Refused(
            "the installation path is not valid Unicode",
        ));
    };
    let (gui, dll) = (normalize_path(gui), normalize_path(dll));
    // Explorer substitutes `%1`, `%L`, `%D` and the like in a verb's command line.
    if gui.contains('%') {
        return Err(RegisterError::Refused(
            "the installation path contains '%'; it cannot be registered",
        ));
    }
    register(reg, &gui, &dll, open_with).map_err(RegisterError::Failed)
}

/// Run `verb` for the executable at `exe` over `reg`; the exit code. A portable copy (the
/// `portable` marker beside the exe) never registers, and `lpk_shell.dll` must lie beside the exe:
/// both are checked before anything is written. Notifies Explorer through `reg`.
pub fn run_verb<R: Registry + ?Sized>(verb: ShellVerb, exe: &Path, reg: &mut R) -> i32 {
    match verb {
        ShellVerb::Unregister => match unregister(reg) {
            Ok(()) => exit::DONE,
            Err(e) => {
                eprintln!("lpk-gui: --unregister-shell failed: {e}");
                exit::FAILED
            }
        },
        ShellVerb::Register { open_with } => match register_for(exe, open_with, reg) {
            Ok(()) => exit::DONE,
            Err(RegisterError::Refused(why)) => {
                eprintln!("lpk-gui: {why}");
                exit::REFUSED
            }
            Err(RegisterError::Failed(e)) => {
                eprintln!("lpk-gui: --register-shell failed: {e}");
                exit::FAILED
            }
        },
    }
}

/// A path as a registry value should hold it: without the verbatim prefix, and a verbatim UNC
/// path (`\\?\UNC\srv\share`) as `\\srv\share`.
pub(crate) fn normalize_path(p: &str) -> String {
    if let Some(unc) = p.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        p.strip_prefix("\\\\?\\").unwrap_or(p).to_string()
    }
}

/// The two entry points of the real registry, called first in `host::run()` (before the Tauri
/// builder and the single-instance plugin): `Some(exit code)` when the command line is a shell
/// verb, `None` otherwise. The only place that names the real `HKEY_CURRENT_USER`.
#[cfg(windows)]
pub fn handle(args: &[OsString]) -> Option<i32> {
    let verb = match parse_shell_verb(args)? {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lpk-gui: {e}");
            return Some(exit::REFUSED);
        }
    };
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("lpk-gui: cannot find the executable");
        return Some(exit::REFUSED);
    };
    let mut reg = WinRegistry(lpk_gui_sys::registry::RegBase::current_user());
    Some(run_verb(verb, &exe, &mut reg))
}

/// The real `HKEY_CURRENT_USER`, for the settings area's commands (`shell_setting`), which call
/// this only after the registered-copy check of the record.
#[cfg(windows)]
pub fn real_registry() -> Box<dyn Registry> {
    Box::new(WinRegistry(lpk_gui_sys::registry::RegBase::current_user()))
}

/// The real registry through `lpk-gui-sys`.
#[cfg(windows)]
#[derive(Debug)]
struct WinRegistry(lpk_gui_sys::registry::RegBase);

#[cfg(windows)]
mod win {
    use super::{RegValue, Registry, WinRegistry};
    use lpk_gui_sys::registry::{self as sys, RegValue as Sys};
    use std::io;

    fn to_sys(v: &RegValue) -> Sys {
        match v {
            RegValue::Sz(s) => Sys::Sz(s.clone()),
            RegValue::None => Sys::None,
            RegValue::Other(t) => Sys::Other(*t),
        }
    }

    fn from_sys(v: Sys) -> RegValue {
        match v {
            Sys::Sz(s) => RegValue::Sz(s),
            Sys::None => RegValue::None,
            Sys::Other(t) => RegValue::Other(t),
        }
    }

    impl Registry for WinRegistry {
        fn key_exists(&self, path: &str) -> io::Result<bool> {
            self.0.key_exists(path)
        }
        fn create_key(&mut self, path: &str) -> io::Result<()> {
            self.0.create_key(path)
        }
        fn get_value(&self, path: &str, name: &str) -> io::Result<Option<RegValue>> {
            Ok(self.0.get_value(path, name)?.map(from_sys))
        }
        fn set_value(&mut self, path: &str, name: &str, value: &RegValue) -> io::Result<()> {
            self.0.set_value(path, name, &to_sys(value))
        }
        fn delete_value(&mut self, path: &str, name: &str) -> io::Result<()> {
            self.0.delete_value(path, name)
        }
        fn list_values(&self, path: &str) -> io::Result<Vec<(String, RegValue)>> {
            Ok(self
                .0
                .list_values(path)?
                .into_iter()
                .map(|(n, v)| (n, from_sys(v)))
                .collect())
        }
        fn delete_key_if_empty(&mut self, path: &str) -> io::Result<bool> {
            self.0.delete_key_if_empty(path)
        }
        fn notify_assoc_changed(&mut self) {
            sys::notify_assoc_changed();
        }
    }
}

#[cfg(test)]
#[path = "register_tests.rs"]
mod tests;
