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
    for (arg, task) in [
        ("corpus", "P0-2"),
        ("run", "P0-3"),
        ("probe", "P0-4"),
        ("report", "P0-5"),
    ] {
        let out = bin().arg(arg).output().expect("run");
        assert!(!out.status.success(), "`{arg}` stub must exit non-zero");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(task), "stderr for `{arg}` lacks {task}: {err}");
    }
}

#[test]
fn documented_claude_md_commands_reach_the_stub_with_exit_1() {
    let documented: [&[&str]; 3] = [
        &["corpus", "build", "--profile", "small"],
        &["run", "--tools", "all"],
        &["report"],
    ];
    for args in documented {
        let out = bin().args(args).output().expect("run");
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("not implemented yet"), "{args:?}: {err}");
    }
}
