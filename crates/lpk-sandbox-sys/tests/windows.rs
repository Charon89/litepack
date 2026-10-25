//! E4-1's proofs on Windows, in both modes. Each test copies the test worker into its own temp
//! directory and grants the AppContainer SID read and execute there (as the installer will grant it
//! on the worker images); the directory and everything in it, ACEs included, go with the TempDir.
//! No AppContainer profile is created (the SID is derived), so the tests leave no system state.
#![cfg(windows)]

use lpk_sandbox_sys::{
    appcontainer_sid_string, grant_path_access, roles, wait_handle, Access, Launch, Limits, Mode,
    APPCONTAINER_NAME,
};
use std::io::{BufRead, BufReader, Read};
use std::os::windows::io::AsHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::time::Duration;

const FILE_GENERIC_READ: u32 = 0x0012_0089;
const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;
const ERROR_ACCESS_DENIED: &str = "5";
const BUILTIN_USERS: &str = "S-1-5-32-545";
const DETACHED_PROCESS: u32 = 0x0000_0008;

struct Staged {
    _dir: tempfile::TempDir,
    exe: PathBuf,
}

fn stage() -> Staged {
    let dir = tempfile::tempdir().expect("tempdir");
    let sid = appcontainer_sid_string(APPCONTAINER_NAME).expect("derive sid");
    grant_path_access(
        dir.path(),
        &sid,
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        true,
    )
    .expect("grant package sid");
    let exe = dir.path().join("lpk-sandbox-testworker.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_lpk-sandbox-testworker"), &exe).expect("copy worker");
    Staged { _dir: dir, exe }
}

fn limits() -> Limits {
    Limits {
        memory: 256 << 20,
        cpu_time: Duration::from_secs(30),
        cpu_ceiling: Duration::from_secs(60),
    }
}

/// Run the worker to completion; returns (exit code, response text).
fn run(s: &Staged, mode: Mode, lim: Limits, args: &[&str]) -> (u32, String) {
    let (mut r, w) = std::io::pipe().expect("pipe");
    let worker = Launch::new(&s.exe)
        .mode(mode)
        .limits(lim)
        .args(args)
        .handle(roles::RESPONSES, w.as_handle(), Access::Same)
        .spawn()
        .expect("spawn");
    drop(w);
    let mut text = String::new();
    r.read_to_string(&mut text).expect("read responses");
    let code = worker
        .wait(Some(Duration::from_secs(60)))
        .expect("wait")
        .expect("worker exited");
    (code, text)
}

fn token_line(text: &str) -> &str {
    text.lines()
        .find(|l| l.starts_with("token "))
        .unwrap_or_else(|| panic!("no token line in {text:?}"))
}

#[test]
#[ignore = "E4-1: not yet"]
fn token_shape_lpac() {
    let s = stage();
    let (code, text) = run(&s, Mode::Lpac, limits(), &["report"]);
    assert_eq!(code, 0, "{text}");
    let sid = appcontainer_sid_string(APPCONTAINER_NAME).expect("sid");
    let t = token_line(&text);
    assert!(t.contains("appcontainer=1 lpac=1 "), "{t}");
    assert!(t.contains("integrity=0x1000 "), "{t}");
    assert!(t.ends_with(&format!("package={sid}")), "{t}");
}

#[test]
fn token_shape_restricted() {
    let s = stage();
    let (code, text) = run(&s, Mode::RestrictedToken, limits(), &["report"]);
    assert_eq!(code, 0, "{text}");
    let t = token_line(&text);
    assert!(t.contains("appcontainer=0 lpac=0 "), "{t}");
    assert!(t.contains("integrity=0x1000 "), "{t}");
    assert!(t.contains("restricted=1 restricted_sids=3 "), "{t}");
    // DISABLE_MAX_PRIVILEGE leaves SeChangeNotifyPrivilege only.
    assert!(t.contains("privileges=1 "), "{t}");
    assert!(t.ends_with("package=-"), "{t}");
}

fn memory_limit_kills(mode: Mode) {
    let s = stage();
    let lim = Limits {
        memory: 64 << 20,
        ..limits()
    };
    let (code, text) = run(&s, mode, lim, &["alloc", "512"]);
    assert!(text.contains("allocating 512"), "{text}");
    assert!(!text.contains("alloc ok"), "{text}");
    assert_ne!(code, 0, "{text}");
}

#[test]
#[ignore = "E4-1: not yet"]
fn memory_limit_kills_lpac() {
    memory_limit_kills(Mode::Lpac);
}

#[test]
fn memory_limit_kills_restricted() {
    memory_limit_kills(Mode::RestrictedToken);
}

fn dies_with_job(mode: Mode) {
    let s = stage();
    let (r, w) = std::io::pipe().expect("pipe");
    let worker = Launch::new(&s.exe)
        .mode(mode)
        .limits(limits())
        .args(["sleep", "60000"])
        .handle(roles::RESPONSES, w.as_handle(), Access::Same)
        .spawn()
        .expect("spawn");
    drop(w);
    let mut line = String::new();
    BufReader::new(r).read_line(&mut line).expect("read");
    assert_eq!(line.trim(), "sleeping");
    let process = worker.try_clone_process().expect("clone");
    assert!(!wait_handle(process.as_handle(), Some(Duration::ZERO)).expect("wait"));
    drop(worker); // closes the job: kill-on-close
    assert!(
        wait_handle(process.as_handle(), Some(Duration::from_secs(10))).expect("wait"),
        "worker outlived its job"
    );
}

#[test]
#[ignore = "E4-1: not yet"]
fn dies_with_job_lpac() {
    dies_with_job(Mode::Lpac);
}

#[test]
fn dies_with_job_restricted() {
    dies_with_job(Mode::RestrictedToken);
}

fn cannot_spawn(mode: Mode) {
    let s = stage();
    let (code, text) = run(&s, mode, limits(), &["spawn"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("spawn err "), "{text}");
}

#[test]
#[ignore = "E4-1: not yet"]
fn cannot_spawn_lpac() {
    cannot_spawn(Mode::Lpac);
}

#[test]
fn cannot_spawn_restricted() {
    cannot_spawn(Mode::RestrictedToken);
}

fn only_passed_handles(mode: Mode) {
    let s = stage();
    let archive_path = s.exe.with_file_name("archive.bin");
    std::fs::write(&archive_path, b"archive").expect("write archive");
    let archive = std::fs::File::open(&archive_path).expect("open archive");
    let (req_r, _req_w) = std::io::pipe().expect("pipe");
    let (mut resp_r, resp_w) = std::io::pipe().expect("pipe");
    let (_err_r, err_w) = std::io::pipe().expect("pipe");
    let worker = Launch::new(&s.exe)
        .mode(mode)
        .limits(limits())
        .args(["report"])
        .handle(roles::REQUESTS, req_r.as_handle(), Access::Same)
        .handle(roles::RESPONSES, resp_w.as_handle(), Access::Same)
        .handle(roles::STDERR, err_w.as_handle(), Access::Same)
        .handle(roles::ARCHIVE, archive.as_handle(), Access::ReadOnly)
        .spawn()
        .expect("spawn");
    drop((req_r, resp_w, err_w));
    let mut text = String::new();
    resp_r.read_to_string(&mut text).expect("read");
    assert_eq!(
        worker.wait(Some(Duration::from_secs(30))).expect("wait"),
        Some(0),
        "{text}"
    );

    let passed: Vec<(u32, u64)> = text
        .lines()
        .filter_map(|l| l.strip_prefix("passed "))
        .map(|p| {
            let (r, v) = p.split_once('=').expect("passed line");
            (r.parse().expect("role"), v.parse().expect("value"))
        })
        .collect();
    let roles_seen: Vec<u32> = passed.iter().map(|p| p.0).collect();
    assert_eq!(
        roles_seen,
        [
            roles::REQUESTS,
            roles::RESPONSES,
            roles::STDERR,
            roles::ARCHIVE
        ]
    );
    // (value, type index, access)
    let table: Vec<(u64, u32, u32)> = text
        .lines()
        .filter_map(|l| l.strip_prefix("handle "))
        .map(|h| {
            let f: Vec<&str> = h.split(' ').collect();
            (
                f[0].parse().expect("value"),
                f[1].parse().expect("type"),
                u32::from_str_radix(f[2].trim_start_matches("0x"), 16).expect("access"),
            )
        })
        .collect();
    let find = |v: u64| {
        *table
            .iter()
            .find(|e| e.0 == v)
            .unwrap_or_else(|| panic!("{v} not in table: {text}"))
    };
    let archive_entry = find(passed[3].1);
    assert_eq!(
        archive_entry.2, FILE_GENERIC_READ,
        "archive handle is read-only: {text}"
    );
    // Every File-type handle (files, pipes, devices) is a passed one, except the current
    // directory (opened by the loader with SYNCHRONIZE | FILE_TRAVERSE).
    let file_type = archive_entry.1;
    let extra: Vec<_> = table
        .iter()
        .filter(|e| e.1 == file_type && !passed.iter().any(|p| p.1 == e.0) && e.2 != 0x0010_0020)
        .collect();
    assert!(
        extra.is_empty(),
        "unexpected File handles {extra:?} in\n{text}"
    );
}

#[test]
#[ignore = "E4-1: not yet"]
fn only_passed_handles_lpac() {
    only_passed_handles(Mode::Lpac);
}

#[test]
#[ignore = "E4-1: not yet"]
fn only_passed_handles_restricted() {
    only_passed_handles(Mode::RestrictedToken);
}

fn console_less_host(mode: &str) {
    let s = stage();
    let out = s.exe.with_file_name(format!("host-{mode}.txt"));
    let status = std::process::Command::new(&s.exe)
        .args(["host", mode])
        .arg(&out)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED_PROCESS)
        .status()
        .expect("run host");
    let text = std::fs::read_to_string(&out).unwrap_or_default();
    assert!(status.success(), "{status} {text}");
    assert_eq!(text.trim(), "first=sleeping jobcount=1");
}

#[test]
#[ignore = "E4-1: not yet"]
fn console_less_host_lpac() {
    console_less_host("lpac");
}

#[test]
fn console_less_host_restricted() {
    console_less_host("restricted");
}

/// A file readable by BUILTIN\Users (plus its inherited owner ACEs): denied under LPAC, readable
/// under the opt-in restricted token — the opt-in's residual, pinned. A file without that ACE is
/// denied in both modes.
#[test]
#[ignore = "E4-1: not yet"]
fn users_readable_file_lpac_denied_restricted_readable() {
    let s = stage();
    let data = tempfile::tempdir().expect("tempdir");
    let users_file = data.path().join("users-readable.txt");
    std::fs::write(&users_file, b"residual").expect("write");
    grant_path_access(&users_file, BUILTIN_USERS, FILE_GENERIC_READ, false).expect("grant users");
    let private_file = data.path().join("private.txt");
    std::fs::write(&private_file, b"private").expect("write");
    let users = users_file.to_str().expect("utf8 path");
    let private = private_file.to_str().expect("utf8 path");

    let (_, text) = run(&s, Mode::Lpac, limits(), &["read", users]);
    assert_eq!(text.trim(), format!("read err {ERROR_ACCESS_DENIED}"));
    let (_, text) = run(&s, Mode::Lpac, limits(), &["read", private]);
    assert_eq!(text.trim(), format!("read err {ERROR_ACCESS_DENIED}"));
    let (_, text) = run(&s, Mode::RestrictedToken, limits(), &["read", users]);
    assert_eq!(text.trim(), "read ok 8", "the opt-in's residual changed");
    let (_, text) = run(&s, Mode::RestrictedToken, limits(), &["read", private]);
    assert_eq!(text.trim(), format!("read err {ERROR_ACCESS_DENIED}"));
}

/// Past its user-time limit the worker is killed; raising the limit while it runs saves it.
#[test]
#[ignore = "E4-1: not yet"]
fn cpu_limit_kills_and_can_be_raised_while_running() {
    let s = stage();
    let lim = Limits {
        cpu_time: Duration::from_millis(500),
        ..limits()
    };
    let (code, text) = run(&s, Mode::Lpac, lim, &["spin", "3000"]);
    assert!(
        text.contains("spinning") && !text.contains("spin done"),
        "{text}"
    );
    assert_ne!(code, 0);

    let (r, w) = std::io::pipe().expect("pipe");
    let worker = Launch::new(&s.exe)
        .limits(lim)
        .args(["spin", "3000"])
        .handle(roles::RESPONSES, w.as_handle(), Access::Same)
        .spawn()
        .expect("spawn");
    drop(w);
    let mut r = BufReader::new(r);
    let mut line = String::new();
    r.read_line(&mut line).expect("read");
    assert_eq!(line.trim(), "spinning");
    assert!(
        worker.set_cpu_limit(Duration::from_secs(61)).is_err(),
        "above the ceiling"
    );
    worker
        .set_cpu_limit(Duration::from_secs(30))
        .expect("raise");
    let mut rest = String::new();
    r.read_to_string(&mut rest).expect("read");
    assert_eq!(rest.trim(), "spin done");
    assert_eq!(
        worker.wait(Some(Duration::from_secs(60))).expect("wait"),
        Some(0)
    );
}

/// The worker refuses to start without a valid bootstrap page.
#[test]
fn worker_refuses_without_bootstrap() {
    let s = stage();
    for args in [
        vec!["report"],
        vec![
            "--lpk-bootstrap=0x100000000,0x200000000,0x300000000,0x400000000",
            "report",
        ],
        vec!["--lpk-bootstrap=garbage", "report"],
    ] {
        let st = std::process::Command::new(&s.exe)
            .args(&args)
            .status()
            .expect("run");
        assert_eq!(st.code(), Some(3), "{args:?}");
    }
}
