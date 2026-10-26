//! E4-3 end to end: the main workspace's debug `lpk` (from the shared `target/`) extracts and
//! lists foreign archives through this workspace's `lpk-worker` (staged, given to `lpk` as
//! `LPK_TEST_WORKER`; on Windows in an AppContainer profile made per test).
//!
//! `lpk` is built by the main workspace, not here: when it is missing the tests say so on
//! stderr and pass, unless `LPK_REQUIRE_E2E=1` (the `workers` CI job builds `lpk` first and sets
//! it), which makes a missing `lpk` a failure. The corpus test is ignored; it reads
//! `$LPK_CORPUS/archives-nested` (the `small` profile).
#![cfg(any(windows, target_os = "linux"))]
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

struct Staged {
    lpk: PathBuf,
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

/// The debug `lpk` beside this test's worker, or `None` (with a note) when it is not built.
fn lpk() -> Option<PathBuf> {
    let worker = PathBuf::from(env!("CARGO_BIN_EXE_lpk-worker"));
    let lpk = worker.with_file_name(if cfg!(windows) { "lpk.exe" } else { "lpk" });
    if lpk.is_file() {
        return Some(lpk);
    }
    let msg = format!(
        "{} missing: build the main workspace (`cargo build -p lpk-cli` at the root) to run the end-to-end tests",
        lpk.display()
    );
    assert!(
        std::env::var_os("LPK_REQUIRE_E2E").is_none(),
        "{msg} (LPK_REQUIRE_E2E is set)"
    );
    eprintln!("SKIPPED: {msg}");
    None
}

fn stage() -> Option<Staged> {
    static N: AtomicU32 = AtomicU32::new(0);
    let lpk = lpk()?;
    let dir = tempfile::tempdir().unwrap();
    let wdir = dir.path().join("worker");
    std::fs::create_dir(&wdir).unwrap();
    let src = PathBuf::from(env!("CARGO_BIN_EXE_lpk-worker"));
    let exe = wdir.join(src.file_name().unwrap());
    std::fs::copy(&src, &exe).unwrap();
    let n = N.fetch_add(1, Ordering::Relaxed);
    #[cfg(windows)]
    let profile = {
        const READ_EXECUTE: u32 = 0x0012_0089 | 0x0012_00A0;
        let name = format!("LitePack.Test.E2e.{}.{n}", std::process::id());
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
    Some(Staged {
        lpk,
        exe,
        profile,
        dir,
    })
}

impl Staged {
    fn run(&self, args: &[&std::ffi::OsStr]) -> (i32, String, String) {
        let mut c = Command::new(&self.lpk);
        c.args(args).env("LPK_TEST_WORKER", &self.exe);
        if let Some(p) = &self.profile {
            c.env("LPK_TEST_APPCONTAINER", p);
        }
        let o = c.output().unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lpk-foreign/tests/fixtures")
        .join(name)
}

/// The fixtures' content (`lpk-foreign/tests/fixtures/README.md`).
fn content() -> BTreeMap<&'static str, Vec<u8>> {
    let mut x: u32 = 12345;
    let data: Vec<u8> = (0..65536)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345) & 0x7fff_ffff;
            (x >> 16) as u8
        })
        .collect();
    let text: Vec<u8> = (0..5000)
        .flat_map(|i| format!("line {i} of the LitePack fixture text\n").into_bytes())
        .collect();
    BTreeMap::from([
        ("hello.txt", b"hello world\n".to_vec()),
        ("dir/data.bin", data),
        ("dir/empty.txt", Vec::new()),
        ("dir/text.txt", text),
    ])
}

fn os(s: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(s)
}

#[test]
fn lpk_x_extracts_a_zip_through_the_worker() {
    let Some(s) = stage() else { return };
    for name in ["7zip-deflate.zip", "7zip-bzip2.zip", "7zip-lzma2-solid.7z"] {
        let out = s.dir.path().join(format!("out-{name}"));
        let (code, stdout, stderr) = s.run(&[os("x"), fixture(name).as_os_str(), out.as_os_str()]);
        assert_eq!(code, 0, "{name}: {stdout}\n{stderr}");
        for (member, want) in content() {
            let got = std::fs::read(out.join(member)).unwrap();
            assert_eq!(blake3::hash(&got), blake3::hash(&want), "{name}: {member}");
        }
    }
}

#[test]
fn lpk_x_takes_the_password_of_an_encrypted_zip() {
    let Some(s) = stage() else { return };
    let out = s.dir.path().join("out-aes");
    let (code, _, stderr) = s.run(&[
        os("x"),
        fixture("7zip-aes256.zip").as_os_str(),
        out.as_os_str(),
    ]);
    assert_ne!(code, 0, "no password: refused");
    assert!(!out.join("hello.txt").exists(), "{stderr}");
}

#[test]
fn lpk_l_lists_a_7z_through_the_worker() {
    let Some(s) = stage() else { return };
    let (code, stdout, stderr) = s.run(&[os("l"), fixture("7zip-lzma2-solid.7z").as_os_str()]);
    assert_eq!(code, 0, "{stderr}");
    for member in content().keys() {
        assert!(stdout.contains(member), "{member} not listed:\n{stdout}");
    }
}

/// `small`'s `archives-nested` class (ZIP, JAR, EPUB): every archive lists and extracts
/// through the worker, and every listed name is extracted.
#[test]
#[ignore = "needs LPK_CORPUS pointing at a built `small` corpus"]
fn the_archives_nested_class_lists_and_extracts() {
    let Some(s) = stage() else { return };
    let corpus = PathBuf::from(std::env::var_os("LPK_CORPUS").expect("LPK_CORPUS"));
    let mut archives = Vec::new();
    let mut stack = vec![corpus.join("archives-nested")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                archives.push(p);
            }
        }
    }
    archives.sort();
    assert!(!archives.is_empty());
    let mut members = 0usize;
    for (i, a) in archives.iter().enumerate() {
        let (code, listing, stderr) = s.run(&[os("l"), a.as_os_str()]);
        assert_eq!(code, 0, "{}: {stderr}", a.display());
        let out = s.dir.path().join(format!("corpus-{i}"));
        let (code, _, stderr) = s.run(&[os("x"), a.as_os_str(), out.as_os_str()]);
        assert_eq!(code, 0, "{}: {stderr}", a.display());
        // `lpk l` lines: kind, size, time, name (tab-separated).
        for line in listing.lines().filter(|l| !l.is_empty()) {
            let f: Vec<&str> = line.splitn(4, '\t').collect();
            assert_eq!(f.len(), 4, "{line}");
            let p = out.join(f[3]);
            match f[0] {
                "File" => {
                    let len = std::fs::metadata(&p)
                        .unwrap_or_else(|e| panic!("{}: {}: {e}", a.display(), f[3]))
                        .len();
                    assert_eq!(len.to_string(), f[1], "{}: {}", a.display(), f[3]);
                }
                _ => assert!(p.exists(), "{}: {} not extracted", a.display(), f[3]),
            }
            members += 1;
        }
    }
    eprintln!(
        "archives-nested: {} archives, {members} members listed and extracted",
        archives.len()
    );
}
