//! E3-5 through the binary: the password prompt refused through a pipe, the unneeded
//! keyfile, the reference commands' own error kinds, a pseudo-terminal where the OS offers
//! one, and no output ever carrying a password.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lpk"))
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// No output of a credential test carries the password's bytes (stdout and stderr).
fn assert_no_secret(r: &Output, secret: &[u8]) {
    for (name, bytes) in [("stdout", &r.stdout), ("stderr", &r.stderr)] {
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret),
            "the password appears on {name}: {}",
            text(bytes)
        );
    }
}

fn json_error(r: &Output) -> serde_json::Value {
    let err = text(&r.stderr);
    assert_eq!(err.lines().count(), 1, "exactly one line: {err}");
    let v: serde_json::Value = serde_json::from_str(&err).unwrap();
    v["error"].clone()
}

fn make_tree(root: &Path) {
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(
        root.join("a.txt"),
        b"the quick brown fox jumps over the lazy dog\n",
    )
    .unwrap();
    fs::write(root.join("sub/b.bin"), vec![7u8; 5000]).unwrap();
}

fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    for e in walkdir(root) {
        v.push((
            e.strip_prefix(root).unwrap().to_string_lossy().into_owned(),
            fs::read(&e).unwrap(),
        ));
    }
    v.sort();
    v
}

fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else {
            out.push(p);
        }
    }
    out
}

const NOT_A_TERMINAL: &str =
    "error: --password-prompt needs a terminal: stdin and stderr are not both terminals\n";

/// Through a pipe (every test here: stdin is not a terminal) `--password-prompt` is a usage
/// error before anything is read or written, in both output forms; without the flag the
/// sealed archive is `PasswordRequired` (exit 2), never a prompt, as D-48 requires of the
/// measured tool.
#[test]
fn the_password_prompt_is_refused_through_a_pipe_and_nothing_is_prompted_without_it() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let s = src.path().to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("p.lpk");
    let archive = arch.to_str().unwrap();
    let new = work.path().join("new.lpk");
    let a = run(&["a", archive, s, "--password", "sesame-7"]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert_no_secret(&a, b"sesame-7");
    let out = work.path().join("out");
    let o = out.to_str().unwrap();
    let exported = work.path().join("e.zip");
    for args in [
        vec!["a", new.to_str().unwrap(), s, "--password-prompt"],
        vec!["x", archive, o, "--password-prompt"],
        vec!["t", archive, "--password-prompt"],
        vec!["l", archive, "--password-prompt"],
        vec!["info", archive, "--password-prompt"],
        vec!["check", archive, "--password-prompt"],
        vec![
            "repair",
            archive,
            new.to_str().unwrap(),
            "--password-prompt",
        ],
        vec!["append", archive, s, "--password-prompt"],
        vec![
            "export",
            archive,
            exported.to_str().unwrap(),
            "--password-prompt",
        ],
    ] {
        let r = run(&args);
        assert_eq!(r.status.code(), Some(1), "{args:?}: {}", text(&r.stderr));
        assert!(r.stdout.is_empty(), "{args:?}");
        assert_eq!(text(&r.stderr), NOT_A_TERMINAL, "{args:?}");
        let mut json = vec!["--json"];
        json.extend(args.iter());
        let r = run(&json);
        assert_eq!(r.status.code(), Some(1));
        let e = json_error(&r);
        assert_eq!(e["kind"], "Usage", "{e}");
    }
    assert!(!new.exists() && !out.exists() && !exported.exists());
    // `rollback` needs no key: its credential flags, the prompt included, are accepted and
    // ignored, so nothing is asked for and nothing is refused through the pipe.
    let r = run(&["rollback", archive, "0", "--password-prompt"]);
    assert_ne!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(
        !text(&r.stderr).contains("--password-prompt"),
        "{}",
        text(&r.stderr)
    );
    // The flag conflicts with the other password forms (clap: exit 1).
    let r = run(&["t", archive, "--password-prompt", "--password", "x"]);
    assert_eq!(r.status.code(), Some(1));
    let r = run(&["t", archive, "--password-prompt", "--password-file", "x"]);
    assert_eq!(r.status.code(), Some(1));
    // Without the flag and without a terminal: no prompt, the format's own refusal.
    let r = run(&["--json", "x", archive, o]);
    assert_eq!(r.status.code(), Some(2));
    assert_eq!(json_error(&r)["kind"], "PasswordRequired");
    assert!(!out.exists());
    let r = run(&["--json", "l", archive]);
    assert_eq!(json_error(&r)["kind"], "PasswordRequired");
    let r = run(&["--json", "export", archive, exported.to_str().unwrap()]);
    assert_eq!(json_error(&r)["kind"], "PasswordRequired");
    assert!(!exported.exists());
    let r = run(&["--json", "append", archive, s]);
    assert_eq!(json_error(&r)["kind"], "AppendNeedsCredentials");
}

/// A `--keyfile` on an archive sealed with a password alone is refused (exit 2,
/// `UnneededKeyfile`, the archive as the path) before the command runs, on every command
/// that opens the archive; the password alone still opens it; a plain archive ignores
/// credentials as before; a credential file that cannot be read names that file; the
/// reference tool's own variants reach the JSON line.
#[test]
fn an_unneeded_keyfile_is_refused_on_a_password_only_archive() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let s = src.path().to_str().unwrap();
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("k.lpk");
    let archive = arch.to_str().unwrap();
    let keyfile = work.path().join("key.bin");
    fs::write(&keyfile, vec![0x5au8; 64]).unwrap();
    let kf = keyfile.to_str().unwrap();
    let a = run(&["a", archive, s, "--password", "only-a-password"]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let out = work.path().join("out");
    let o = out.to_str().unwrap();
    let repaired = work.path().join("r.lpk");
    let exported = work.path().join("e.zip");
    let pw = ["--password", "only-a-password"];
    for args in [
        vec!["x", archive, o, pw[0], pw[1], "--keyfile", kf],
        vec!["t", archive, pw[0], pw[1], "--keyfile", kf],
        vec!["l", archive, "--keyfile", kf],
        vec!["info", archive, "--keyfile", kf],
        vec!["check", archive, "--keyfile", kf],
        vec![
            "repair",
            archive,
            repaired.to_str().unwrap(),
            "--keyfile",
            kf,
        ],
        vec!["append", archive, s, pw[0], pw[1], "--keyfile", kf],
        vec![
            "export",
            archive,
            exported.to_str().unwrap(),
            pw[0],
            pw[1],
            "--keyfile",
            kf,
        ],
    ] {
        let r = run(&args);
        assert_eq!(r.status.code(), Some(2), "{args:?}: {}", text(&r.stderr));
        assert_no_secret(&r, b"only-a-password");
        let err = text(&r.stderr);
        assert!(
            err.contains("the archive's key slot takes no keyfile; --keyfile is not needed"),
            "{args:?}: {err}"
        );
        let mut json = vec!["--json"];
        json.extend(args.iter());
        let r = run(&json);
        let e = json_error(&r);
        assert_eq!(e["kind"], "UnneededKeyfile", "{e}");
        assert_eq!(e["path"], archive);
    }
    assert!(!out.exists() && !repaired.exists() && !exported.exists());
    let x = run(&["x", archive, o, pw[0], pw[1]]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree(&out), tree(src.path()));
    let plain = work.path().join("plain.lpk");
    let a = run(&["a", plain.to_str().unwrap(), s]);
    assert_eq!(a.status.code(), Some(0));
    let t = run(&["t", plain.to_str().unwrap(), "--keyfile", kf]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    let gone = work.path().join("no-such-key");
    let t = run(&["--json", "t", archive, "--keyfile", gone.to_str().unwrap()]);
    assert_eq!(t.status.code(), Some(2));
    let e = json_error(&t);
    assert_eq!(e["kind"], "Io", "{e}");
    assert_eq!(e["path"], gone.to_str().unwrap());
    let r = run(&["--json", "rollback", archive, "7"]);
    assert_eq!(r.status.code(), Some(2));
    assert_eq!(json_error(&r)["kind"], "NoSuchGeneration");
    let r = run(&["--json", "t", archive, "--password", "wrong-one"]);
    assert_eq!(r.status.code(), Some(2));
    assert_no_secret(&r, b"wrong-one");
    assert_eq!(json_error(&r)["kind"], "WrongKey");
}

/// On Linux, where `script` (util-linux) offers a pseudo-terminal: `--password-prompt` reads
/// the password without echo on `a` (confirmed) and the fallback prompts on `l` without the
/// flag; a mismatched confirmation is a usage error; the password never appears in the
/// terminal's output. Skipped where `script` is not installed. (No simple pty on Windows:
/// the prompt path is unit-tested there with an injected reader in `lpk_cli::prompt`.)
#[cfg(target_os = "linux")]
#[test]
fn the_prompt_reads_from_a_pseudo_terminal_without_echo() {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    if Command::new("script").arg("--version").output().is_err() {
        eprintln!("skipped: `script` is not installed");
        return;
    }
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("pty.lpk");
    let archive = arch.to_str().unwrap();
    let lpk = env!("CARGO_BIN_EXE_lpk");
    // Run `lpk` under a pty; once the first prompt shows, type the lines.
    let under_pty = |args: &[&str], typed: &str| -> (i32, String) {
        let mut cmd = format!("'{lpk}'");
        for a in args {
            cmd.push_str(&format!(" '{a}'"));
        }
        let mut child = Command::new("script")
            .args(["-qec", &cmd, "/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let prompted = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&prompted);
        let reader = std::thread::spawn(move || {
            let mut all = Vec::new();
            let mut buf = [0u8; 256];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => all.extend_from_slice(&buf[..n]),
                }
                if all.windows(9).any(|w| w == b"Password ") {
                    seen.store(true, Ordering::SeqCst);
                }
            }
            all
        });
        let start = Instant::now();
        while !prompted.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(20));
        }
        // The prompt is printed before the terminal is put in raw mode; leave that a moment.
        std::thread::sleep(Duration::from_millis(300));
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(typed.as_bytes()).unwrap();
        drop(stdin);
        let status = child.wait().unwrap();
        let output = text(&reader.join().unwrap());
        (status.code().unwrap_or(-1), output)
    };
    let (code, out) = under_pty(
        &[
            "a",
            archive,
            src.path().to_str().unwrap(),
            "--password-prompt",
        ],
        "pty-secret\npty-secret\n",
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Password for pty.lpk: "), "{out}");
    assert!(out.contains("Confirm password: "), "{out}");
    assert!(!out.contains("pty-secret"), "echoed: {out}");
    let t = run(&["t", archive, "--password", "pty-secret"]);
    assert_eq!(t.status.code(), Some(0), "{}", text(&t.stderr));
    assert!(text(&t.stdout).starts_with("ok: "));
    // The fallback: no flag, a terminal, the archive needs the key.
    let (code, out) = under_pty(&["l", archive], "pty-secret\n");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Password for pty.lpk: "), "{out}");
    assert!(out.contains("a.txt"), "{out}");
    assert!(!out.contains("pty-secret"), "echoed: {out}");
    // A mismatched confirmation is a usage error and writes nothing.
    let other = work.path().join("other.lpk");
    let (code, out) = under_pty(
        &[
            "a",
            other.to_str().unwrap(),
            src.path().to_str().unwrap(),
            "--password-prompt",
        ],
        "one\ntwo\n",
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("the passwords do not match"), "{out}");
    assert!(!other.exists());
}
