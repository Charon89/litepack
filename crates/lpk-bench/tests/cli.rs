use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lpk-bench"))
}

#[test]
fn help_prints_subcommands() {
    let out = bin().arg("--help").output().expect("run");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for name in ["corpus", "run", "probe", "report"] {
        assert!(text.contains(name), "missing `{name}` in:\n{text}");
    }
}

#[test]
fn stubs_fail_loudly_naming_the_task() {
    for (arg, task) in [("run", "P0-3"), ("probe", "P0-4"), ("report", "P0-5")] {
        let out = bin().arg(arg).output().expect("run");
        assert!(!out.status.success(), "`{arg}` stub must exit non-zero");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(task), "stderr for `{arg}` lacks {task}: {err}");
    }
}

#[test]
fn corpus_scan_writes_a_private_manifest_and_fails_cleanly_on_a_missing_folder() {
    let tmp = tempfile::tempdir().expect("tmp");
    let missing = bin()
        .args(["corpus", "scan", "--private"])
        .arg(tmp.path().join("nope"))
        .arg("--out")
        .arg(tmp.path().join("out"))
        .output()
        .expect("run");
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("does not exist"));

    let dir = tmp.path().join("data");
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::write(dir.join("a.txt"), b"hello").expect("write");
    let out = tmp.path().join("out");
    let ok = bin()
        .args(["corpus", "scan", "--private"])
        .arg(&dir)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run");
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let manifest = std::fs::read_to_string(out.join("manifest.json")).expect("manifest");
    assert!(manifest.contains("\"profile\": \"private\""), "{manifest}");
    assert!(out.join("build-info.json").exists());
}

#[test]
fn corpus_build_with_missing_registry_fails_cleanly() {
    let dir = tempfile::tempdir().expect("tmp");
    let out = bin()
        .current_dir(dir.path())
        .args(["corpus", "build", "--profile", "small"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("corpus-sources.toml"), "{err}");
}

#[test]
fn documented_claude_md_commands_reach_the_stub_with_exit_1() {
    let documented: [&[&str]; 2] = [&["run", "--tools", "all"], &["report"]];
    for args in documented {
        let out = bin().args(args).output().expect("run");
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("not implemented yet"), "{args:?}: {err}");
    }
}
