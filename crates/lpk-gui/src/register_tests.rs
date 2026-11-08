//! Tests of `register.rs` over an in-memory registry: no test here touches the real one.

use super::*;
use crate::register_fake::Fake;

const GUI: &str = "C:\\Apps\\LitePack\\lpk-gui.exe";
const DLL: &str = "C:\\Apps\\LitePack\\lpk_shell.dll";

fn sz(s: &str) -> RegValue {
    RegValue::Sz(s.to_string())
}

/// A registry with other programs' keys and values where ours go.
fn foreign() -> Fake {
    Fake::default()
        .with("Software\\Classes", "", sz("x"))
        .with("Software\\Classes\\.lpk", "", sz("Other.lpk"))
        .with("Software\\Classes\\.lpk", "PerceivedType", sz("text"))
        .with(
            "Software\\Classes\\.zip\\OpenWithProgids",
            "Other.zip",
            RegValue::None,
        )
        .with("Software\\Classes\\.zip", "", sz("CompressedFolder"))
        .with("Software\\Classes\\*\\shell\\Other", "", sz("Other"))
        .with(
            "Software\\RegisteredApplications",
            "Other",
            sz("Software\\Other\\Capabilities"),
        )
        .with("Software\\Microsoft", "Keep", RegValue::Other(4))
}

#[test]
fn register_then_unregister_leaves_an_empty_registry_as_found() {
    for open_with in [false, true] {
        let mut reg = Fake::default();
        register(&mut reg, GUI, DLL, open_with).unwrap();
        assert!(reg.key_exists("Software\\LitePack\\Registration").unwrap());
        unregister(&mut reg).unwrap();
        assert_eq!(reg.snapshot(), Fake::default().snapshot(), "{open_with}");
        assert_eq!(reg.notifies, 2);
    }
}

#[test]
fn register_then_unregister_leaves_a_foreign_registry_as_found() {
    let before = foreign();
    let mut reg = before.clone();
    register(&mut reg, GUI, DLL, true).unwrap();
    // Someone else's `.lpk` default and `PerceivedType` stay; ours is added beside theirs.
    assert_eq!(
        reg.value("Software\\Classes\\.lpk", ""),
        Some(sz("Other.lpk"))
    );
    assert_eq!(
        reg.value("Software\\Classes\\.lpk", "PerceivedType"),
        Some(sz("text"))
    );
    assert_eq!(
        reg.value("Software\\Classes\\.lpk\\OpenWithProgids", "LitePack.lpk"),
        Some(RegValue::None)
    );
    assert_eq!(
        reg.value("Software\\Classes\\.zip", ""),
        Some(sz("CompressedFolder"))
    );
    unregister(&mut reg).unwrap();
    assert_eq!(reg.snapshot(), before.snapshot());
}

#[test]
fn an_lpk_default_that_is_absent_is_ours_and_goes_again() {
    let before = Fake::default().with("Software\\Classes\\.lpk", "Other", sz("x"));
    let mut reg = before.clone();
    register(&mut reg, GUI, DLL, false).unwrap();
    assert_eq!(
        reg.value("Software\\Classes\\.lpk", ""),
        Some(sz("LitePack.lpk"))
    );
    assert_eq!(
        reg.value("Software\\Classes\\.lpk", "PerceivedType"),
        Some(sz("compressed"))
    );
    unregister(&mut reg).unwrap();
    assert_eq!(reg.snapshot(), before.snapshot());
}

/// The exact values registration writes (the record apart), as `(key, name, value)`.
fn written(reg: &Fake) -> Vec<(String, String, RegValue)> {
    let mut out = Vec::new();
    for (k, vs) in &reg.keys {
        if k.starts_with("software\\litepack\\registration") {
            continue;
        }
        for (name, v) in vs.values() {
            out.push((k.clone(), name.clone(), v.clone()));
        }
    }
    out
}

#[test]
fn registration_writes_exactly_these_keys_and_values() {
    let mut reg = Fake::default();
    register(&mut reg, GUI, DLL, true).unwrap();
    let c = "software\\classes";
    let clsid = format!("{c}\\clsid\\{}", CLASSIC_CLSID.to_lowercase());
    let icon = format!("\"{GUI}\",0");
    let cmd = format!("\"{GUI}\" --open \"%1\"");
    let mut want: Vec<(String, String, RegValue)> = Vec::new();
    let mut add = |k: String, n: &str, v: RegValue| want.push((k, n.to_string(), v));
    add("software\\litepack".into(), "GuiPath", sz(GUI));
    add(clsid.clone(), "", sz("LitePack"));
    add(format!("{clsid}\\inprocserver32"), "", sz(DLL));
    add(
        format!("{clsid}\\inprocserver32"),
        "ThreadingModel",
        sz("Apartment"),
    );
    for t in ["*", "directory", "directory\\background"] {
        let k = format!("{c}\\{t}\\shell\\litepack");
        add(k.clone(), "ExplorerCommandHandler", sz(CLASSIC_CLSID));
        add(k, "MultiSelectModel", sz("Player"));
    }
    let mut ids = vec![("lpk", "LitePack archive".to_string())];
    for e in ["zip", "7z", "tar", "tgz", "rar"] {
        ids.push((e, format!("{} archive (LitePack)", e.to_uppercase())));
    }
    for (e, friendly) in &ids {
        let k = format!("{c}\\litepack.{e}");
        add(k.clone(), "", sz(friendly));
        add(format!("{k}\\defaulticon"), "", sz(&icon));
        add(format!("{k}\\shell\\open\\command"), "", sz(&cmd));
        add(
            format!("{c}\\.{e}\\openwithprogids"),
            &format!("LitePack.{e}"),
            RegValue::None,
        );
    }
    add(format!("{c}\\.lpk"), "", sz("LitePack.lpk"));
    add(format!("{c}\\.lpk"), "PerceivedType", sz("compressed"));
    let cap = "software\\litepack\\capabilities";
    add(cap.into(), "ApplicationName", sz("LitePack"));
    add(
        cap.into(),
        "ApplicationDescription",
        sz("LitePack archiver: .lpk archives and other archive formats"),
    );
    add(
        format!("{cap}\\fileassociations"),
        ".lpk",
        sz("LitePack.lpk"),
    );
    for e in ["zip", "7z", "tar", "tgz", "rar"] {
        add(
            format!("{cap}\\fileassociations"),
            &format!(".{e}"),
            sz(&format!("LitePack.{e}")),
        );
    }
    add(
        "software\\registeredapplications".into(),
        "LitePack",
        sz("Software\\LitePack\\Capabilities"),
    );
    want.sort();
    let mut got = written(&reg);
    got.sort();
    for g in &got {
        assert!(want.contains(g), "unexpected {g:?}");
    }
    for w in &want {
        assert!(got.contains(w), "missing {w:?}");
    }
    assert_eq!(got.len(), want.len());
    // The keys too, empty ones included: every ancestor of a key holding a value, and nothing else
    // (the record's own key apart).
    let mut want_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (k, _, _) in &want {
        let mut prefix = String::new();
        for part in k.split('\\') {
            if !prefix.is_empty() {
                prefix.push('\\');
            }
            prefix.push_str(part);
            want_keys.insert(prefix.clone());
        }
    }
    let got_keys: std::collections::BTreeSet<String> = reg
        .keys
        .keys()
        .filter(|k| !k.starts_with("software\\litepack\\registration"))
        .cloned()
        .collect();
    assert_eq!(got_keys, want_keys);
    // Nothing near the user's choice: no such key exists at all.
    for k in reg.keys.keys() {
        assert!(!k.contains("fileexts") && !k.contains("userchoice"), "{k}");
    }
    // The three shell keys carry no `.lpk`-style default and no extension default but `.lpk`'s.
    for e in ["zip", "7z", "tar", "tgz", "rar"] {
        assert_eq!(reg.value(&format!("{c}\\.{e}"), ""), None);
    }
}

#[test]
fn without_open_with_no_foreign_extension_is_touched() {
    let mut reg = Fake::default();
    register(&mut reg, GUI, DLL, false).unwrap();
    for (k, n, _) in written(&reg) {
        for e in ["zip", "7z", "tar", "tgz", "rar"] {
            assert!(
                !k.contains(&format!(".{e}")) && !n.contains(&format!(".{e}")),
                "{k} {n}"
            );
            assert!(!k.contains(&format!("litepack.{e}")), "{k}");
        }
    }
}

#[test]
fn a_value_changed_by_someone_else_survives_unregistering() {
    let mut reg = foreign();
    register(&mut reg, GUI, DLL, true).unwrap();
    reg.set_value(
        "Software\\LitePack",
        "GuiPath",
        &sz("D:\\Moved\\lpk-gui.exe"),
    )
    .unwrap();
    reg.set_value(
        "Software\\Classes\\LitePack.zip\\shell\\open\\command",
        "",
        &sz("other"),
    )
    .unwrap();
    unregister(&mut reg).unwrap();
    assert_eq!(
        reg.value("Software\\LitePack", "GuiPath"),
        Some(sz("D:\\Moved\\lpk-gui.exe"))
    );
    assert_eq!(
        reg.value("Software\\Classes\\LitePack.zip\\shell\\open\\command", ""),
        Some(sz("other"))
    );
    // Everything of ours that was left alone is gone, and so is the record.
    assert!(!reg.key_exists("Software\\LitePack\\Registration").unwrap());
    assert_eq!(
        reg.value("Software\\RegisteredApplications", "LitePack"),
        None
    );
    assert_eq!(
        reg.value("Software\\Classes\\.zip\\OpenWithProgids", "LitePack.zip"),
        None
    );
}

#[test]
fn a_failure_after_n_writes_is_undone_by_unregister() {
    let before = foreign();
    let mut probe = before.clone();
    register(&mut probe, GUI, DLL, true).unwrap();
    let total = probe.writes;
    assert!(total > 50);
    for n in 0..=total {
        let mut reg = before.clone();
        reg.fail_at = Some(n);
        let r = register(&mut reg, GUI, DLL, true);
        assert_eq!(r.is_err(), n < total, "n = {n}");
        reg.fail_at = None;
        unregister(&mut reg).unwrap();
        assert_eq!(reg.snapshot(), before.snapshot(), "failed after {n} writes");
    }
}

#[test]
fn a_failure_while_unregistering_can_be_retried() {
    let before = foreign();
    let mut reg = before.clone();
    register(&mut reg, GUI, DLL, true).unwrap();
    let start = reg.writes;
    let mut probe = reg.clone();
    unregister(&mut probe).unwrap();
    let steps = probe.writes - start;
    for n in 0..steps {
        let mut r = reg.clone();
        r.fail_at = Some(start + n);
        assert!(unregister(&mut r).is_err());
        r.fail_at = None;
        unregister(&mut r).unwrap();
        // Also when the last key (`Software\LitePack`, empty) failed to delete: the retry finds no
        // record and clears that residue.
        assert_eq!(r.snapshot(), before.snapshot(), "failed after {n} writes");
    }
}

#[test]
fn registering_again_and_toggling_open_with_leave_one_registration() {
    let before = foreign();
    let mut fresh = before.clone();
    register(&mut fresh, GUI, DLL, false).unwrap();

    let mut reg = before.clone();
    register(&mut reg, GUI, DLL, false).unwrap();
    register(&mut reg, GUI, DLL, false).unwrap();
    assert_eq!(reg.snapshot(), fresh.snapshot());

    register(&mut reg, GUI, DLL, true).unwrap();
    register(&mut reg, GUI, DLL, false).unwrap();
    assert_eq!(reg.snapshot(), fresh.snapshot());
    let record = reg.list_values("Software\\LitePack\\Registration").unwrap();
    assert_eq!(
        record.len(),
        fresh
            .list_values("Software\\LitePack\\Registration")
            .unwrap()
            .len()
    );

    register(&mut reg, GUI, DLL, true).unwrap();
    unregister(&mut reg).unwrap();
    assert_eq!(reg.snapshot(), before.snapshot());
}

#[test]
fn unregistering_with_nothing_registered_changes_nothing() {
    let before = foreign();
    let mut reg = before.clone();
    unregister(&mut reg).unwrap();
    assert_eq!(reg.snapshot(), before.snapshot());
    assert_eq!(reg.notifies, 1);
}

fn args(v: &[&str]) -> Vec<OsString> {
    v.iter().map(OsString::from).collect()
}

#[test]
fn the_shell_verbs_are_read_from_the_literal_command_line_only() {
    assert_eq!(parse_shell_verb(&args(&[])), None);
    assert_eq!(parse_shell_verb(&args(&["--open", "a.lpk"])), None);
    assert_eq!(parse_shell_verb(&args(&["@list.txt"])), None);
    assert_eq!(
        parse_shell_verb(&args(&["--register-shell"])),
        Some(Ok(ShellVerb::Register { open_with: false }))
    );
    assert_eq!(
        parse_shell_verb(&args(&["--register-shell", "--open-with"])),
        Some(Ok(ShellVerb::Register { open_with: true }))
    );
    assert_eq!(
        parse_shell_verb(&args(&["--unregister-shell"])),
        Some(Ok(ShellVerb::Unregister))
    );
    for bad in [
        &["--register-shell", "extra"][..],
        &["--unregister-shell", "--open-with"],
        &["--register-shell", "--unregister-shell"],
        &["--open", "a.lpk", "--register-shell"],
        &["--register-shell", "--open-with", "--open-with"],
        &["--register-shell=1", "--unregister-shell"],
    ] {
        assert!(
            matches!(parse_shell_verb(&args(bad)), Some(Err(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn an_argument_that_is_not_unicode_is_refused() {
    #[cfg(windows)]
    let odd = {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[0xD800, 0x61])
    };
    #[cfg(unix)]
    let odd = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![0xff, 0x61])
    };
    let a = vec![OsString::from("--register-shell"), odd];
    assert!(matches!(parse_shell_verb(&a), Some(Err(_))));
}

#[test]
fn a_percent_sign_in_the_install_path_is_refused_before_any_write() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("100%done");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join(SHELL_DLL), b"").unwrap();
    let exe = sub.join("lpk-gui.exe");
    std::fs::write(&exe, b"").unwrap();
    let mut reg = Fake::default();
    let verb = ShellVerb::Register { open_with: false };
    assert_eq!(run_verb(verb, &exe, &mut reg), exit::REFUSED);
    assert_eq!(reg.writes, 0);
    assert_eq!(reg.notifies, 0);
}

#[test]
fn verbatim_prefixes_are_normalized() {
    assert_eq!(normalize_path("C:\\a\\b.exe"), "C:\\a\\b.exe");
    assert_eq!(normalize_path("\\\\?\\C:\\a\\b.exe"), "C:\\a\\b.exe");
    assert_eq!(
        normalize_path("\\\\?\\UNC\\srv\\share\\b.exe"),
        "\\\\srv\\share\\b.exe"
    );
    assert_eq!(
        normalize_path("\\\\srv\\share\\b.exe"),
        "\\\\srv\\share\\b.exe"
    );
}

fn install(portable: bool, dll: bool) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("lpk-gui.exe");
    std::fs::write(&exe, b"").unwrap();
    if dll {
        std::fs::write(dir.path().join(SHELL_DLL), b"").unwrap();
    }
    if portable {
        std::fs::write(dir.path().join(PORTABLE_MARKER), b"").unwrap();
    }
    (dir, exe)
}

#[test]
fn the_verbs_exit_with_their_codes() {
    let register_all = ShellVerb::Register { open_with: true };
    // A portable copy never registers, a missing DLL is found before anything is written.
    for (portable, dll) in [(true, true), (false, false)] {
        let (_dir, exe) = install(portable, dll);
        let mut reg = Fake::default();
        assert_eq!(run_verb(register_all, &exe, &mut reg), exit::REFUSED);
        assert_eq!(reg.writes, 0);
        assert_eq!(reg.notifies, 0);
    }
    let (_dir, exe) = install(false, true);
    let mut reg = Fake::default();
    assert_eq!(run_verb(register_all, &exe, &mut reg), exit::DONE);
    assert_eq!(
        reg.value("Software\\LitePack", "GuiPath"),
        Some(sz(exe.to_str().unwrap()))
    );
    assert_eq!(run_verb(ShellVerb::Unregister, &exe, &mut reg), exit::DONE);
    assert_eq!(reg.snapshot(), Fake::default().snapshot());
    // A failure is 1, and unregistering is allowed from a portable copy.
    let mut bad = Fake {
        fail_at: Some(5),
        ..Fake::default()
    };
    assert_eq!(run_verb(register_all, &exe, &mut bad), exit::FAILED);
    bad.fail_at = None;
    let (_p, portable_exe) = install(true, false);
    assert_eq!(
        run_verb(ShellVerb::Unregister, &portable_exe, &mut bad),
        exit::DONE
    );
    assert_eq!(bad.snapshot(), Fake::default().snapshot());
}

#[test]
fn the_classic_class_id_is_the_one_lpk_shell_serves() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-shell/src/com.rs"),
    )
    .unwrap();
    let id = |name: &str| -> String {
        let line = src
            .lines()
            .find(|l| l.starts_with(&format!("pub const {name}: GUID")))
            .unwrap_or_else(|| panic!("{name} in lpk-shell"));
        let hex = line.split("0x").nth(1).unwrap().split(')').next().unwrap();
        let h: String = hex.chars().filter(|c| *c != '_').collect();
        assert_eq!(h.len(), 32, "{line}");
        format!(
            "{{{}-{}-{}-{}-{}}}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        )
        .to_uppercase()
    };
    assert_eq!(id("CLSID_LPK_SHELL_CLASSIC"), CLASSIC_CLSID);
    assert_ne!(id("CLSID_LPK_SHELL"), CLASSIC_CLSID);
}

#[test]
fn the_open_with_list_is_parsed() {
    let l = open_with_extensions();
    assert!(l.contains(&"zip") && l.contains(&"tgz"));
    assert!(l
        .iter()
        .all(|e| !e.is_empty() && !e.contains('.') && *e == e.to_lowercase()));
}

#[test]
fn path_entries_are_appended_once_and_removed_by_value() {
    let bin = "C:\\Users\\u\\AppData\\Local\\LitePack\\bin";
    // A separator only when one is needed; a duplicate (any case, a trailing backslash) is not
    // added.
    assert_eq!(path_with_entry("", bin), Some((bin.to_string(), false)));
    assert_eq!(
        path_with_entry("A;B", bin),
        Some((format!("A;B;{bin}"), true))
    );
    assert_eq!(
        path_with_entry("A;", bin),
        Some((format!("A;{bin}"), false))
    );
    let upper = format!("A;{}\\;B", bin.to_uppercase());
    assert_eq!(path_with_entry(&upper, bin), None);
    // Removed exactly, the rest byte for byte.
    for before in ["", "A;B", "A;", "%X%\\y;;Z", ";"] {
        let (with, sep) = path_with_entry(before, bin).unwrap();
        assert_eq!(path_without_entry(&with, bin, sep).as_deref(), Some(before));
    }
    // Something appended after ours by someone else stays.
    let (with, sep) = path_with_entry("A", bin).unwrap();
    assert_eq!(
        path_without_entry(&format!("{with};C"), bin, sep).as_deref(),
        Some("A;C")
    );
    assert_eq!(path_without_entry("A;B", bin, true), None);
}

#[test]
fn register_puts_bin_on_path_keeping_its_type_and_unregister_restores_it() {
    let bin = "C:\\Apps\\LitePack\\bin";
    let found = [
        RegValue::ExpandSz("%USERPROFILE%\\.cargo\\bin;C:\\Tools".to_string()),
        sz("C:\\Tools;"),
    ];
    for value in found {
        let before = foreign().with(ENVIRONMENT, PATH_VALUE, value.clone());
        let mut reg = before.clone();
        register_with_path(&mut reg, GUI, DLL, Some(bin), false).unwrap();
        let after = reg.value(ENVIRONMENT, PATH_VALUE).unwrap();
        match (&value, &after) {
            (RegValue::ExpandSz(a), RegValue::ExpandSz(b)) | (RegValue::Sz(a), RegValue::Sz(b)) => {
                assert!(b.starts_with(a.as_str()) && b.ends_with(bin), "{b}");
            }
            _ => panic!("type changed: {after:?}"),
        }
        // Registering again (an upgrade, the settings toggle) leaves one entry.
        register_with_path(&mut reg, GUI, DLL, Some(bin), true).unwrap();
        assert_eq!(reg.value(ENVIRONMENT, PATH_VALUE), Some(after));
        unregister(&mut reg).unwrap();
        assert_eq!(reg.snapshot(), before.snapshot());
    }
    // An entry already there (the user's) is neither added nor removed.
    let mine = foreign().with(ENVIRONMENT, PATH_VALUE, sz("C:\\apps\\litepack\\BIN\\"));
    let mut reg = mine.clone();
    register_with_path(&mut reg, GUI, DLL, Some(bin), false).unwrap();
    assert_eq!(
        reg.value(ENVIRONMENT, PATH_VALUE),
        Some(sz("C:\\apps\\litepack\\BIN\\"))
    );
    unregister(&mut reg).unwrap();
    assert_eq!(reg.snapshot(), mine.snapshot());
    // No `Path` and no `Environment`: created, then removed with the key.
    let mut reg = Fake::default();
    register_with_path(&mut reg, GUI, DLL, Some(bin), false).unwrap();
    assert_eq!(
        reg.value(ENVIRONMENT, PATH_VALUE),
        Some(RegValue::ExpandSz(bin.to_string()))
    );
    unregister(&mut reg).unwrap();
    assert_eq!(reg.snapshot(), Fake::default().snapshot());
    // A `Path` that is not a string is left as found.
    let odd = foreign().with(ENVIRONMENT, PATH_VALUE, RegValue::Other(3));
    let mut reg = odd.clone();
    register_with_path(&mut reg, GUI, DLL, Some(bin), false).unwrap();
    assert_eq!(reg.value(ENVIRONMENT, PATH_VALUE), Some(RegValue::Other(3)));
    unregister(&mut reg).unwrap();
    assert_eq!(reg.snapshot(), odd.snapshot());
}

#[test]
fn a_failure_while_adding_to_path_is_undone_by_unregister() {
    let before = foreign().with(ENVIRONMENT, PATH_VALUE, sz("A"));
    let mut n = 0;
    loop {
        let mut reg = before.clone();
        reg.fail_at = Some(n);
        let failed = register_with_path(&mut reg, GUI, DLL, Some("C:\\L\\bin"), true).is_err();
        reg.fail_at = None;
        unregister(&mut reg).unwrap();
        assert_eq!(reg.snapshot(), before.snapshot(), "failure after write {n}");
        if !failed {
            break;
        }
        n += 1;
    }
    assert!(n > 10, "{n}");
}

#[test]
fn the_installed_layout_registers_its_versioned_dll_and_bin() {
    let (dir, exe) = install(false, true);
    // Development layout: the DLL beside the exe, no `bin`.
    assert_eq!(shell_dll_path(dir.path()), dir.path().join(SHELL_DLL));
    assert_eq!(installed_bin(dir.path()), None);
    // A folder of another version is not ours.
    let other = dir.path().join(SHELL_DIR).join("0.0.0-other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join(SHELL_DLL), b"").unwrap();
    assert_eq!(shell_dll_path(dir.path()), dir.path().join(SHELL_DLL));
    // Installed layout: `shell\<version>\lpk_shell.dll` and `bin\lpk.exe`.
    let versioned = dir.path().join(SHELL_DIR).join(APP_VERSION);
    std::fs::create_dir_all(&versioned).unwrap();
    std::fs::write(versioned.join(SHELL_DLL), b"").unwrap();
    let bin = dir.path().join(BIN_DIR);
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join(BIN_TOOL), b"").unwrap();
    std::fs::remove_file(dir.path().join(SHELL_DLL)).unwrap();
    assert_eq!(shell_dll_path(dir.path()), versioned.join(SHELL_DLL));
    assert_eq!(installed_bin(dir.path()), Some(bin.clone()));
    let mut reg = Fake::default();
    register_for(&exe, false, &mut reg).unwrap();
    let inproc = format!("{CLASSES}\\CLSID\\{CLASSIC_CLSID}\\InProcServer32");
    assert_eq!(
        reg.value(&inproc, ""),
        Some(sz(versioned.join(SHELL_DLL).to_str().unwrap()))
    );
    assert_eq!(
        reg.value(ENVIRONMENT, PATH_VALUE),
        Some(RegValue::ExpandSz(bin.to_str().unwrap().to_string()))
    );
    unregister(&mut reg).unwrap();
    assert_eq!(reg.snapshot(), Fake::default().snapshot());
    // A portable copy of the installed layout still refuses.
    std::fs::write(dir.path().join(PORTABLE_MARKER), b"").unwrap();
    assert!(matches!(
        register_for(&exe, false, &mut reg),
        Err(RegisterError::Refused(_))
    ));
}

#[test]
fn the_app_version_is_the_tauri_config_version() {
    let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    // Built without an overlay (cargo test sets no TAURI_CONFIG).
    assert_eq!(conf["version"].as_str(), Some(APP_VERSION));
}

#[test]
fn the_uninstaller_removes_the_jump_list_of_this_identifier() {
    // Windows names an app's jump list files by the CRC-64 (polynomial 0x92C64265D32139A4,
    // reflected, initial value all ones) of its AppUserModelID, upper-cased, in UTF-16LE.
    fn crc64(data: &[u8]) -> u64 {
        let mut c = u64::MAX;
        for &b in data {
            c ^= u64::from(b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    (c >> 1) ^ 0x92C6_4265_D321_39A4
                } else {
                    c >> 1
                };
            }
        }
        c
    }
    let explorer: Vec<u8> = "MICROSOFT.WINDOWS.EXPLORER"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert_eq!(format!("{:016x}", crc64(&explorer)), "f01b4d95cf55d32a");
    let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    let id: Vec<u8> = conf["identifier"]
        .as_str()
        .unwrap()
        .to_uppercase()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let file = format!("{:016x}.automaticDestinations-ms", crc64(&id));
    assert!(include_str!("../installer.nsh").contains(&file), "{file}");
}

#[test]
fn the_installer_overlay_places_the_dll_in_this_versions_shell_folder() {
    let overlay: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.nsis.conf.json")).unwrap();
    let resources = overlay["bundle"]["resources"].as_object().unwrap();
    let shell: Vec<&str> = resources
        .values()
        .filter_map(|v| v.as_str())
        .filter(|v| v.starts_with("shell/"))
        .collect();
    assert_eq!(shell, [format!("shell/{APP_VERSION}/{SHELL_DLL}")]);
    // The overlay sets no version of its own: the installer is the workspace's version.
    assert!(overlay.get("version").is_none());
    for tool in [
        "lpk.exe",
        "lpk-check.exe",
        "lpk-worker.exe",
        "lpk-worker-libarchive.exe",
    ] {
        assert!(
            resources
                .values()
                .any(|v| v.as_str() == Some(&format!("bin/{tool}"))),
            "{tool}"
        );
    }
}
