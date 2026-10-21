//! E3-3's console rules: with stderr redirected (a pipe, as the benchmark runner's) no
//! progress byte is written by `a`, `append`, `x` and `export`; `--quiet` prints nothing but
//! errors; `--no-progress` is accepted; a cancelled command (the token `main`'s Ctrl-C
//! handler trips) exits 2 with `cancelled`, text and `--json`, and leaves no partial output.
#![allow(clippy::unwrap_used)]

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

use lpk_core::CancelToken;

fn lpk(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lpk"))
        .args(args)
        .output()
        .unwrap()
}

fn run(args: &[&str]) -> Output {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    lpk(&args)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn make_tree(root: &Path) {
    std::fs::create_dir_all(root.join("d/e")).unwrap();
    for i in 0..20 {
        std::fs::write(
            root.join(format!("d/e/f{i}.txt")),
            format!("file {i}\n").repeat(2000),
        )
        .unwrap();
    }
    std::fs::write(root.join("top.bin"), vec![0x5au8; 100_000]).unwrap();
}

/// No carriage return and no progress line (`<stage>: <n> files, ... in, ... out, ...
/// blocks`, the console's format) ever reaches a redirected stderr.
fn assert_no_progress(name: &str, stderr: &[u8]) {
    let s = text(stderr);
    assert!(
        !s.contains('\r'),
        "{name}: a progress rewrite reached the pipe: {s:?}"
    );
    let stages = [
        "walk: ",
        "peel: ",
        "encode: ",
        "seal: ",
        "extract: ",
        "export: ",
    ];
    for line in s.lines() {
        let progress = stages.iter().any(|st| line.starts_with(st))
            && line.contains(" files, ")
            && line.contains(" in, ")
            && line.contains(" out, ");
        assert!(
            !progress,
            "{name}: a progress line reached the pipe: {line:?}"
        );
    }
}

#[test]
fn with_stderr_redirected_no_progress_byte_is_written() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let out = tempfile::tempdir().unwrap();
    let arch = out.path().join("t.lpk");
    let arch_s = arch.to_str().unwrap();
    let a = run(&["a", arch_s, dir.path().to_str().unwrap()]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert!(a.stderr.is_empty(), "{:?}", text(&a.stderr));
    assert_no_progress("a", &a.stderr);
    // -v's stage report is unchanged: it is the only stderr output.
    let v = run(&["append", "-v", arch_s, dir.path().to_str().unwrap()]);
    assert_eq!(v.status.code(), Some(0), "{}", text(&v.stderr));
    assert_no_progress("append -v", &v.stderr);
    assert!(text(&v.stderr).contains("walk"), "{}", text(&v.stderr));
    let x = run(&["x", arch_s, out.path().join("x").to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert!(x.stderr.is_empty(), "{:?}", text(&x.stderr));
    assert_eq!(text(&x.stdout), "extracted 21 files, 2 directories\n");
    let zip = out.path().join("t.zip");
    let e = run(&["export", arch_s, zip.to_str().unwrap()]);
    assert_eq!(e.status.code(), Some(0), "{}", text(&e.stderr));
    assert!(e.stderr.is_empty(), "{:?}", text(&e.stderr));
    assert!(zip.exists());
    // --no-progress is accepted everywhere; nothing changes through a pipe.
    let n = run(&[
        "--no-progress",
        "x",
        arch_s,
        out.path().join("x2").to_str().unwrap(),
    ]);
    assert_eq!(n.status.code(), Some(0), "{}", text(&n.stderr));
    assert!(n.stderr.is_empty());
    assert_eq!(text(&n.stdout), "extracted 21 files, 2 directories\n");
}

#[test]
fn quiet_prints_nothing_but_errors() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let out = tempfile::tempdir().unwrap();
    let arch = out.path().join("q.lpk");
    let arch_s = arch.to_str().unwrap();
    let a = run(&["--quiet", "a", "-v", arch_s, dir.path().to_str().unwrap()]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert!(
        a.stdout.is_empty() && a.stderr.is_empty(),
        "{:?} {:?}",
        text(&a.stdout),
        text(&a.stderr)
    );
    let x = run(&[
        "--quiet",
        "x",
        "-v",
        arch_s,
        out.path().join("x").to_str().unwrap(),
    ]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert!(
        x.stdout.is_empty() && x.stderr.is_empty(),
        "{:?} {:?}",
        text(&x.stdout),
        text(&x.stderr)
    );
    // An error is still printed: the target exists (refused).
    let again = run(&[
        "--quiet",
        "x",
        arch_s,
        out.path().join("x").to_str().unwrap(),
    ]);
    assert_eq!(again.status.code(), Some(2));
    assert!(
        text(&again.stderr).starts_with("error: "),
        "{}",
        text(&again.stderr)
    );
    assert!(again.stdout.is_empty());
    // And the JSON error line under --quiet --json.
    let j = run(&[
        "--quiet",
        "--json",
        "x",
        arch_s,
        out.path().join("x").to_str().unwrap(),
    ]);
    assert_eq!(j.status.code(), Some(2));
    let line: serde_json::Value = serde_json::from_str(text(&j.stderr).trim()).unwrap();
    assert_eq!(line["error"]["code"], 2);
}

/// Run in process with a token already cancelled: what `main`'s Ctrl-C handler does, with
/// the cancellation taking effect at the engine's first poll.
fn cancelled(args: &[&str]) -> (i32, String, String) {
    let token = CancelToken::new();
    token.cancel();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut argv = vec!["lpk"];
    argv.extend_from_slice(args);
    let code = lpk_cli::run_with_cancel(argv, &mut out, &mut err, token);
    (code, text(&out), text(&err))
}

#[test]
fn a_cancelled_command_exits_2_with_cancelled_and_leaves_no_partial_output() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let out = tempfile::tempdir().unwrap();
    let arch = out.path().join("c.lpk");
    let arch_s = arch.to_str().unwrap();
    let input = dir.path().to_str().unwrap();
    // a: the archive is not left behind.
    let (code, o, e) = cancelled(&["a", arch_s, input]);
    assert_eq!(
        (code, o.as_str(), e.as_str()),
        (2, "", "error: cancelled\n")
    );
    assert!(!arch.exists());
    let (code, _, e) = cancelled(&["--json", "a", arch_s, input]);
    assert_eq!(code, 2);
    let line: serde_json::Value = serde_json::from_str(e.trim()).unwrap();
    assert_eq!(line["error"]["kind"], "Cancelled");
    assert_eq!(line["error"]["code"], 2);
    assert_eq!(line["error"]["message"], "cancelled");
    assert_eq!(line["error"]["path"], arch_s);
    assert!(!arch.exists());
    // A real archive for the rest.
    let a = run(&["a", arch_s, input]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let before = std::fs::read(&arch).unwrap();
    // append: the archive is as it was.
    let (code, _, e) = cancelled(&["append", arch_s, input]);
    assert_eq!((code, e.as_str()), (2, "error: cancelled\n"));
    assert_eq!(std::fs::read(&arch).unwrap(), before);
    // x: no output directory content.
    let target = out.path().join("x");
    let (code, o, e) = cancelled(&["x", arch_s, target.to_str().unwrap()]);
    assert_eq!(
        (code, o.as_str(), e.as_str()),
        (2, "", "error: cancelled\n")
    );
    assert!(!target.join("d").exists());
    // export: the output file is removed.
    let zip = out.path().join("c.zip");
    let (code, _, e) = cancelled(&["export", arch_s, zip.to_str().unwrap()]);
    assert_eq!((code, e.as_str()), (2, "error: cancelled\n"));
    assert!(!zip.exists());
    // --quiet keeps the error line.
    let (code, o, e) = cancelled(&["--quiet", "x", arch_s, target.to_str().unwrap()]);
    assert_eq!(
        (code, o.as_str(), e.as_str()),
        (2, "", "error: cancelled\n")
    );
}
