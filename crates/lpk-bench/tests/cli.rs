use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lpk-bench"))
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn list_tools_prints_one_line_per_catalogue_tool() {
    let out = bin()
        .current_dir(repo_root())
        .args(["run", "--list-tools"])
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let ids = [
        "store",
        "7z",
        "rar",
        "zstd",
        "xz",
        "zpaqfranz",
        "tsaur",
        "wzzip",
        "pacl",
    ];
    assert_eq!(text.lines().count(), ids.len(), "{text}");
    for (line, id) in text.lines().zip(ids) {
        assert!(line.starts_with(id), "{line}");
        assert!(
            line.contains(" found ") || line.contains("skipped: "),
            "{line}"
        );
    }
}

#[test]
fn list_tools_filters_and_rejects_unknown_ids() {
    let out = bin()
        .current_dir(repo_root())
        .args(["run", "--list-tools", "--tools", "wzzip,pacl"])
        .output()
        .expect("run");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.lines().count(), 2, "{text}");
    let bad = bin()
        .current_dir(repo_root())
        .args(["run", "--list-tools", "--tools", "nope"])
        .output()
        .expect("run");
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown tool `nope`"));
}

#[test]
fn validate_reports_problems_by_file_and_field() {
    let tmp = tempfile::tempdir().expect("tmp");
    let dir = tmp.path().join("2026-10-01-testbox");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("host.json"), "{}").expect("host");
    let out = bin()
        .args(["run", "--validate"])
        .arg(&dir)
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("host.json: (root): ") && err.contains("os_version"),
        "{err}"
    );
    assert!(err.contains("tools.json: (root): file is missing"), "{err}");
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
