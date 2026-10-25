//! E4-2: `lpk x` and `lpk l` route a foreign archive (by its magic; only the test fixture in
//! E4-2) to the sandboxed worker. The debug build takes the test worker from
//! `LPK_TEST_WORKER` (a copy the AppContainer may read) and the profile from
//! `LPK_TEST_APPCONTAINER`.
#![cfg(any(windows, target_os = "linux"))]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use lpk_proto::fixture::{self, Member};

struct Staged {
    exe: PathBuf,
    profile: Option<String>,
    dir: tempfile::TempDir,
}

impl Drop for Staged {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(p) = &self.profile {
            let _ = lpk_sandbox_sys::delete_appcontainer_profile(p);
        }
    }
}

fn stage() -> Staged {
    static N: AtomicU32 = AtomicU32::new(0);
    let lpk = PathBuf::from(env!("CARGO_BIN_EXE_lpk"));
    let name = if cfg!(windows) {
        "lpk-test-worker.exe"
    } else {
        "lpk-test-worker"
    };
    let src = lpk.with_file_name(name);
    assert!(
        src.is_file(),
        "{} missing: build the workspace",
        src.display()
    );
    let dir = tempfile::tempdir().unwrap();
    let wdir = dir.path().join("worker");
    std::fs::create_dir(&wdir).unwrap();
    let exe = wdir.join(name);
    std::fs::copy(&src, &exe).unwrap();
    let n = N.fetch_add(1, Ordering::Relaxed);
    #[cfg(windows)]
    let profile = {
        const READ_EXECUTE: u32 = 0x0012_0089 | 0x0012_00A0;
        let name = format!("LitePack.Test.Cli.{}.{n}", std::process::id());
        lpk_sandbox_sys::create_appcontainer_profile(&name).unwrap();
        let sid = lpk_sandbox_sys::appcontainer_sid_string(&name).unwrap();
        lpk_sandbox_sys::grant_path_access(&wdir, &sid, READ_EXECUTE, true).unwrap();
        Some(name)
    };
    #[cfg(not(windows))]
    let profile = {
        let _ = n;
        None
    };
    Staged { exe, profile, dir }
}

impl Staged {
    fn lpk(&self, fault: Option<&str>, args: &[&str]) -> (i32, String, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_lpk"));
        c.args(args).env("LPK_TEST_WORKER", &self.exe);
        if let Some(p) = &self.profile {
            c.env("LPK_TEST_APPCONTAINER", p);
        }
        if let Some(f) = fault {
            c.env("LPK_TEST_WORKER_ARGS", format!("--fault={f}"));
        }
        let o = c.output().unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }
    fn fixture(&self, members: &[Member]) -> PathBuf {
        let p = self.dir.path().join("archive.fx");
        std::fs::write(&p, fixture::write(members, None)).unwrap();
        p
    }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn x_and_l_route_a_foreign_archive_to_the_worker() {
    let st = stage();
    let a = st.fixture(&[
        Member::dir("d"),
        Member::file("d/a.txt", b"hello"),
        Member::file("b.bin", &[1, 2, 3]),
    ]);
    let (code, out, err) = st.lpk(None, &["l", s(&a)]);
    assert_eq!(code, 0, "{err}");
    let paths: Vec<&str> = out
        .lines()
        .map(|l| l.rsplit('\t').next().unwrap())
        .collect();
    assert_eq!(paths, ["d", "d/a.txt", "b.bin"], "{out}");
    let dest = st.dir.path().join("out");
    let (code, out, err) = st.lpk(None, &["x", s(&a), s(&dest)]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "extracted 2 files, 1 directories");
    assert_eq!(std::fs::read(dest.join("d/a.txt")).unwrap(), b"hello");
    // An existing file is refused by the same policy as for .lpk.
    let (code, _, err) = st.lpk(None, &["--json", "x", s(&a), s(&dest)]);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn worker_failures_are_named_errors_with_exit_2() {
    let st = stage();
    let a = st.fixture(&[Member::file("a", b"x")]);
    let dest = st.dir.path().join("out");
    let (code, _, err) = st.lpk(Some("garbage"), &["--json", "x", s(&a), s(&dest)]);
    assert_eq!(code, 2, "{err}");
    let v: serde_json::Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(v["error"]["kind"], "Protocol", "{err}");
    let (code, _, err) = st.lpk(Some("panic"), &["--json", "l", s(&a)]);
    assert_eq!(code, 2, "{err}");
    let v: serde_json::Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(v["error"]["kind"], "Panicked", "{err}");
}

#[test]
fn the_limit_flags_parse_and_apply() {
    let st = stage();
    let a = st.fixture(&[
        Member::file("a", b"x"),
        Member::file("b", b"y"),
        Member::file("c", b"z"),
    ]);
    let (code, _, _) = st.lpk(None, &["l", "--max-entries", "0", s(&a)]);
    assert_eq!(code, 1, "0 is not a limit");
    let (code, _, err) = st.lpk(None, &["--json", "l", "--max-entries", "2", s(&a)]);
    assert_eq!(code, 2, "{err}");
    let v: serde_json::Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(v["error"]["kind"], "LimitExceeded", "{err}");
    let (code, _, err) = st.lpk(
        None,
        &["l", "--max-entries", "off", "--max-cpu", "off", s(&a)],
    );
    assert_eq!(code, 0, "{err}");
    let dest = st.dir.path().join("out");
    let (code, _, err) = st.lpk(
        None,
        &[
            "x",
            "--max-expansion",
            "1",
            "--max-cpu",
            "5",
            s(&a),
            s(&dest),
        ],
    );
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_keyfile_is_refused_on_a_foreign_archive() {
    let st = stage();
    let a = st.fixture(&[Member::file("a", b"x")]);
    let key = st.dir.path().join("key");
    std::fs::write(&key, b"k").unwrap();
    let (code, _, err) = st.lpk(None, &["--json", "l", "--keyfile", s(&key), s(&a)]);
    assert_eq!(code, 2, "{err}");
    let v: serde_json::Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(v["error"]["kind"], "UnneededKeyfile", "{err}");
}
