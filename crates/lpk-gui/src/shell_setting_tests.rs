//! Tests of `shell_setting.rs` over the in-memory registry: nothing here reaches the real one,
//! and the Settings launch is a recorder.

use std::sync::{Arc, Mutex};

use super::*;
use crate::register::{register, unregister};
use crate::register_fake::{Fake, Snapshot};

/// The fake, shared between the test and the seam's registries.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Fake>>);

impl Shared {
    fn with<T>(&self, f: impl FnOnce(&mut Fake) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
    fn snapshot(&self) -> Snapshot {
        self.with(|f| f.snapshot())
    }
    fn notifies(&self) -> usize {
        self.with(|f| f.notifies)
    }
}

impl Registry for Shared {
    fn key_exists(&self, p: &str) -> io::Result<bool> {
        self.with(|f| f.key_exists(p))
    }
    fn create_key(&mut self, p: &str) -> io::Result<()> {
        self.with(|f| f.create_key(p))
    }
    fn get_value(&self, p: &str, n: &str) -> io::Result<Option<RegValue>> {
        self.with(|f| f.get_value(p, n))
    }
    fn set_value(&mut self, p: &str, n: &str, v: &RegValue) -> io::Result<()> {
        self.with(|f| f.set_value(p, n, v))
    }
    fn delete_value(&mut self, p: &str, n: &str) -> io::Result<()> {
        self.with(|f| f.delete_value(p, n))
    }
    fn list_values(&self, p: &str) -> io::Result<Vec<(String, RegValue)>> {
        self.with(|f| f.list_values(p))
    }
    fn delete_key_if_empty(&mut self, p: &str) -> io::Result<bool> {
        self.with(|f| f.delete_key_if_empty(p))
    }
    fn notify_assoc_changed(&mut self) {
        self.with(|f| f.notify_assoc_changed());
    }
}

struct Rig {
    reg: Shared,
    launched: Arc<Mutex<Vec<String>>>,
    dir: tempfile::TempDir,
    setting: ShellSetting,
}

impl Rig {
    fn exe(&self) -> PathBuf {
        self.dir.path().join("lpk-gui.exe")
    }

    fn exe_str(&self) -> String {
        normalize_path(self.exe().to_str().unwrap())
    }

    fn dll_str(&self) -> String {
        normalize_path(self.dir.path().join("lpk_shell.dll").to_str().unwrap())
    }

    /// What the installer does: register this copy.
    fn install(&self, open_with: bool) {
        let mut reg = self.reg.clone();
        register(&mut reg, &self.exe_str(), &self.dll_str(), open_with).unwrap();
    }
}

fn rig(launch_fails: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lpk-gui.exe"), b"").unwrap();
    std::fs::write(dir.path().join("lpk_shell.dll"), b"").unwrap();
    let reg = Shared::default();
    let launched = Arc::new(Mutex::new(Vec::new()));
    let (r, l) = (reg.clone(), launched.clone());
    let setting = ShellSetting::new(
        dir.path().join("lpk-gui.exe"),
        Box::new(move || Box::new(r.clone())),
        Box::new(move |uri| {
            l.lock().unwrap().push(uri.to_string());
            if launch_fails {
                Err(io::Error::other("no shell"))
            } else {
                Ok(())
            }
        }),
    );
    Rig {
        reg,
        launched,
        dir,
        setting,
    }
}

fn opt_in_keys(reg: &Shared) -> Vec<String> {
    let exts = open_with_extensions();
    exts.iter()
        .filter(|e| {
            reg.with(|f| {
                f.value(
                    &format!("Software\\Classes\\.{e}\\OpenWithProgids"),
                    &format!("LitePack.{e}"),
                )
                .is_some()
            })
        })
        .map(|e| e.to_string())
        .collect()
}

#[test]
fn the_controls_are_hidden_when_nothing_is_registered() {
    let r = rig(false);
    let s = r.setting.status();
    assert!(!s.registered && !s.open_with);
    assert!(
        !s.formats.is_empty(),
        "the label's formats are always given"
    );
    assert!(r.setting.set_open_with(true).is_err());
    assert!(r.setting.open_default_apps().is_err());
    assert!(r.launched.lock().unwrap().is_empty());
}

#[test]
fn the_controls_show_for_the_registered_copy() {
    let r = rig(false);
    r.install(false);
    let s = r.setting.status();
    assert_eq!(
        s,
        ShellStatus {
            registered: true,
            open_with: false,
            formats: formats()
        }
    );
    // A different spelling of the same path is the same copy.
    let upper = r.exe_str().to_uppercase().replace('\\', "/");
    assert!(same_path(&upper, &r.exe_str()));
}

#[test]
fn a_portable_copy_never_shows_them() {
    let r = rig(false);
    r.install(false);
    std::fs::write(r.dir.path().join(PORTABLE_MARKER), b"").unwrap();
    let before = r.reg.snapshot();
    assert!(!r.setting.status().registered);
    assert!(r.setting.set_open_with(true).is_err());
    assert!(r.setting.open_default_apps().is_err());
    assert_eq!(r.reg.snapshot(), before);
    assert!(r.launched.lock().unwrap().is_empty());
}

#[test]
fn another_copys_registration_hides_them_and_is_left_alone() {
    let r = rig(false);
    let mut reg = r.reg.clone();
    register(
        &mut reg,
        "C:\\Other\\LitePack\\lpk-gui.exe",
        "C:\\Other\\LitePack\\lpk_shell.dll",
        true,
    )
    .unwrap();
    let before = r.reg.snapshot();
    let notified = r.reg.notifies();
    assert!(!r.setting.status().registered);
    let e = r.setting.set_open_with(false).unwrap_err();
    assert!(e.is(Class::Usage), "{e:?}");
    assert!(r.setting.open_default_apps().is_err());
    assert_eq!(r.reg.snapshot(), before, "nothing written");
    assert_eq!(r.reg.notifies(), notified, "nothing notified");
    assert!(r.launched.lock().unwrap().is_empty());
}

#[test]
fn toggling_writes_and_removes_exactly_the_opt_in_keys_and_notifies_once() {
    let r = rig(false);
    r.install(false);
    let base = r.reg.snapshot();
    assert!(opt_in_keys(&r.reg).is_empty());

    let n = r.reg.notifies();
    let on = r.setting.set_open_with(true).unwrap();
    assert!(on.registered && on.open_with);
    assert_eq!(r.reg.notifies(), n + 1, "one notification");
    assert_eq!(opt_in_keys(&r.reg).len(), formats().len());
    assert_ne!(r.reg.snapshot(), base);
    // The status reads the record, so a fresh read agrees.
    assert!(r.setting.status().open_with);

    let off = r.setting.set_open_with(false).unwrap();
    assert!(off.registered && !off.open_with);
    assert_eq!(r.reg.notifies(), n + 2);
    assert!(opt_in_keys(&r.reg).is_empty());
    assert_eq!(
        r.reg.snapshot(),
        base,
        "back to the registration without the opt-in"
    );

    // Turning it on twice leaves one registration.
    r.setting.set_open_with(true).unwrap();
    let once = r.reg.snapshot();
    r.setting.set_open_with(true).unwrap();
    assert_eq!(r.reg.snapshot(), once);
}

#[test]
fn a_failure_mid_way_is_an_error_and_the_record_stays_undoable() {
    let r = rig(false);
    r.install(false);
    for after in [0usize, 3, 10, 40] {
        r.reg.with(|f| f.fail_at = Some(f.writes + after));
        let e = r.setting.set_open_with(true).unwrap_err();
        assert!(e.is(Class::Internal), "{e:?}");
        r.reg.with(|f| f.fail_at = None);
        // Part 1's guarantee: the record names what was written, so undoing it leaves nothing.
        let mut reg = r.reg.clone();
        unregister(&mut reg).unwrap();
        assert!(
            r.reg.snapshot().is_empty(),
            "after {after}: {:?}",
            r.reg.snapshot()
        );
        r.install(false);
    }
}

#[test]
fn a_failure_before_guipath_is_rewritten_returns_the_cause_and_unregisters_the_copy() {
    let r = rig(false);
    r.install(true);
    // The writes the undo makes: the first failing write after them is the registration's own.
    let undo_writes = {
        let mut probe = r.reg.with(|f| f.clone());
        let before = probe.writes;
        unregister(&mut probe).unwrap();
        probe.writes - before
    };
    r.reg.with(|f| f.fail_at = Some(f.writes + undo_writes));
    let e = r.setting.set_open_with(false).unwrap_err();
    assert!(e.is(Class::Internal), "{e:?}");
    assert_eq!(e.kind, "RegisterFailed");
    assert!(e.detail.contains("injected"), "the cause is carried: {e:?}");
    r.reg.with(|f| f.fail_at = None);
    // GuiPath is gone with the undo, so the status now hides the controls: the page must show
    // the error somewhere that hiding them cannot hide.
    let s = r.setting.status();
    assert!(!s.registered, "{s:?}");
    assert!(r
        .reg
        .with(|f| f.value("Software\\LitePack", "GuiPath"))
        .is_none());
}

#[test]
fn a_refusal_names_why() {
    let r = rig(false);
    r.install(false);
    std::fs::remove_file(r.dir.path().join("lpk_shell.dll")).unwrap();
    let e = r.setting.set_open_with(true).unwrap_err();
    assert!(e.is(Class::Usage), "{e:?}");
    assert!(e.detail.contains("lpk_shell.dll"), "{e:?}");
}

#[test]
fn paths_are_compared_exactly_not_loosely() {
    let r = rig(false);
    let exe = r.exe_str();
    let dir = normalize_path(r.dir.path().to_str().unwrap());
    // Not the same file: a suffix, a sibling folder, the parent folder.
    assert!(!same_path(&format!("{exe}.bak"), &exe));
    assert!(!same_path(&format!("{dir}-other\\lpk-gui.exe"), &exe));
    assert!(!same_path(&dir, &exe));
    // Nothing exists there: the textual comparison, case-insensitive, `/` is `\`.
    assert!(same_path(
        "C:\\Apps\\LitePack\\LPK-GUI.EXE",
        "c:/apps/litepack/lpk-gui.exe"
    ));
    assert!(!same_path(
        "C:\\Apps\\LitePack\\lpk-gui.exe",
        "C:\\Apps\\LitePack\\lpk-gui.exe.bak"
    ));
    assert!(!same_path(
        "C:\\Apps\\LitePack\\lpk-gui.exe",
        "C:\\Apps\\LitePack"
    ));
    // Both exist: the canonical form decides (a `.` or `..` detour is the same file).
    std::fs::create_dir(r.dir.path().join("sub")).unwrap();
    let detour = r.dir.path().join("sub").join("..").join("lpk-gui.exe");
    assert!(same_path(detour.to_str().unwrap(), &exe));
    // Another existing file in the same folder is not.
    assert!(!same_path(&r.dll_str(), &exe));
}

#[test]
fn the_default_apps_button_opens_the_fixed_page_only_for_the_registered_copy() {
    let r = rig(false);
    r.install(true);
    r.setting.open_default_apps().unwrap();
    assert_eq!(
        *r.launched.lock().unwrap(),
        vec!["ms-settings:defaultapps?registeredAppUser=LitePack".to_string()]
    );

    let failing = rig(true);
    failing.install(false);
    let e = failing.setting.open_default_apps().unwrap_err();
    assert!(e.is(Class::Internal), "{e:?}");
}

#[test]
fn the_formats_follow_the_open_with_list() {
    let r = rig(false);
    assert_eq!(
        r.setting.status().formats,
        open_with_extensions()
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
}
