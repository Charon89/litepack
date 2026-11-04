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
    for e in ["zip", "7z", "tar", "tgz"] {
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
    for e in ["zip", "7z", "tar", "tgz"] {
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
    for e in ["zip", "7z", "tar", "tgz"] {
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
