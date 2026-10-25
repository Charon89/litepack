//! E4-1's proofs on Windows, in both modes. Each test copies the test worker into its own temp
//! directory and grants the AppContainer SID read and execute there (as the installer will grant it
//! on the worker images); the directory and everything in it, ACEs included, go with the TempDir.
//! Each test creates its own AppContainer profile (`LitePack.Test.<pid>.<n>`) and deletes it at
//! its end, so the tests leave no system state unless the test process is killed.
#![cfg(windows)]

use lpk_sandbox_sys::{
    appcontainer_sid_string, create_appcontainer_profile, delete_appcontainer_profile,
    grant_path_access, roles, wait_handle, Access, Launch, Limits, Mode,
};
use std::io::{BufRead, BufReader, Read};
use std::os::windows::io::AsHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const FILE_GENERIC_READ: u32 = 0x0012_0089;
const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;
const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
/// File-type handles the worker's own DLLs open while loading: bcryptprimitives.dll (linked by
/// Rust's std) opens the CNG device.
const KNOWN_LOADER_FILE_HANDLES: &[&str] = &[r"\Device\CNG"];
const ERROR_ACCESS_DENIED: &str = "5";
const BUILTIN_USERS: &str = "S-1-5-32-545";
const ALL_APPLICATION_PACKAGES: &str = "S-1-15-2-1";
const DETACHED_PROCESS: u32 = 0x0000_0008;
const STATUS_QUOTA_EXCEEDED: u32 = 0xC000_0044;

/// An AppContainer profile created for one test and deleted when the test ends (also on panic).
/// A profile is needed: a SID derived without one fails `CreateProcessW` with
/// ERROR_FILE_NOT_FOUND (measured on Windows 11 26200). Names start `LitePack.Test.` so a profile
/// leaked by a killed test run is easy to find.
struct Profile(String);

impl Profile {
    fn new() -> Profile {
        static N: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "LitePack.Test.{}.{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        create_appcontainer_profile(&name).expect("create test profile");
        Profile(name)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = delete_appcontainer_profile(&self.0);
    }
}

struct Staged {
    exe: PathBuf,
    // Field order: the profile goes after the worker copy's directory.
    _dir: tempfile::TempDir,
    profile: Profile,
}

impl Staged {
    fn launch(&self) -> Launch<'_> {
        Launch::new(&self.exe).appcontainer_name(&self.profile.0)
    }
}

fn stage() -> Staged {
    let profile = Profile::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let sid = appcontainer_sid_string(&profile.0).expect("derive sid");
    grant_path_access(
        dir.path(),
        &sid,
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        true,
    )
    .expect("grant package sid");
    let exe = dir.path().join("lpk-sandbox-testworker.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_lpk-sandbox-testworker"), &exe).expect("copy worker");
    Staged {
        exe,
        _dir: dir,
        profile,
    }
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
    let worker = s
        .launch()
        .mode(mode)
        .limits(lim)
        .args(args)
        .handle(roles::RESPONSES, w.as_handle(), Access::Write)
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

/// A worker that sleeps until the test ends (its pipe end is returned to keep it open).
fn sleeper(s: &Staged, mode: Mode) -> (lpk_sandbox_sys::Worker, BufReader<std::io::PipeReader>) {
    let (r, w) = std::io::pipe().expect("pipe");
    let worker = s
        .launch()
        .mode(mode)
        .limits(limits())
        .args(["sleep", "60000"])
        .handle(roles::RESPONSES, w.as_handle(), Access::Write)
        .spawn()
        .expect("spawn");
    drop(w);
    let mut r = BufReader::new(r);
    let mut line = String::new();
    r.read_line(&mut line).expect("read");
    assert_eq!(line.trim(), "sleeping");
    (worker, r)
}

const SE_GROUP_USE_FOR_DENY_ONLY: u32 = 0x10;
const SE_GROUP_INTEGRITY: u32 = 0x20;

/// The LPAC worker's token, read by the host: an AppContainer with the profile's package SID, no
/// capability, Low integrity, no restricting SID.
#[test]
fn token_shape_lpac() {
    let s = stage();
    let (worker, _r) = sleeper(&s, Mode::Lpac);
    let t = worker.token_view().expect("token");
    let sid = appcontainer_sid_string(&s.profile.0).expect("sid");
    assert!(t.appcontainer, "{t:?}");
    assert_eq!(t.package_sid.as_deref(), Some(sid.as_str()), "{t:?}");
    assert!(t.capabilities.is_empty(), "{t:?}");
    assert_eq!(t.integrity, 0x1000, "{t:?}");
    assert!(t.restricting.is_empty(), "{t:?}");
}

/// The restricted worker's token, read by the host: every group deny-only except Everyone, Users
/// and the integrity label (an elevated host's Administrators included), restricting SIDs exactly
/// {Everyone, Users, the launch's unique SID}, Low integrity, SeChangeNotifyPrivilege only.
#[test]
fn token_shape_restricted() {
    let s = stage();
    let (worker, _r) = sleeper(&s, Mode::RestrictedToken);
    let t = worker.token_view().expect("token");
    assert!(!t.appcontainer, "{t:?}");
    assert_eq!(t.integrity, 0x1000, "{t:?}");
    assert_eq!(t.privileges, 1, "{t:?}");
    let unique = worker.restricting_sid().expect("unique sid").to_string();
    assert!(unique.starts_with("S-1-5-21-"), "{unique}");
    let mut restricting = t.restricting.clone();
    restricting.sort();
    let mut want = vec!["S-1-1-0".to_string(), BUILTIN_USERS.to_string(), unique];
    want.sort();
    assert_eq!(restricting, want);
    for (sid, attrs) in &t.groups {
        let exempt = sid == "S-1-1-0" || sid == BUILTIN_USERS || attrs & SE_GROUP_INTEGRITY != 0;
        if !exempt {
            assert!(
                attrs & SE_GROUP_USE_FOR_DENY_ONLY != 0,
                "{sid} enabled: {t:?}"
            );
        }
        if sid == "S-1-5-32-544" {
            assert!(
                attrs & SE_GROUP_USE_FOR_DENY_ONLY != 0,
                "Administrators enabled"
            );
        }
    }
    // A second launch gets a different unique SID.
    let (other, _r2) = sleeper(&s, Mode::RestrictedToken);
    assert_ne!(other.restricting_sid(), worker.restricting_sid());
}

/// A running worker cannot open another running worker's process or thread, whatever the access
/// asked for (same mode, so the same package SID or the same user and integrity); the broker can.
fn workers_cannot_open_each_other(mode: Mode) {
    let s = stage();
    let (victim, _vr) = sleeper(&s, mode);
    let pid = victim.id().to_string();
    let tid = victim.thread_id().to_string();
    let (code, text) = run(&s, mode, limits(), &["open", &pid, &tid]);
    assert_eq!(code, 0, "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 11, "{text}");
    for l in &lines {
        assert!(
            l.ends_with(&format!("err {ERROR_ACCESS_DENIED}")),
            "a worker opened another: {l}\n{text}"
        );
    }
    // The broker still can.
    lpk_sandbox_sys::probe_open_process(victim.id(), 0x10 | 0x1000).expect("broker opens process");
    lpk_sandbox_sys::probe_open_thread(victim.thread_id(), 0x8 | 0x800)
        .expect("broker opens thread");
}

#[test]
fn workers_cannot_open_each_other_lpac() {
    workers_cannot_open_each_other(Mode::Lpac);
}

#[test]
fn workers_cannot_open_each_other_restricted() {
    workers_cannot_open_each_other(Mode::RestrictedToken);
}

/// The worker's environment is `SystemRoot` alone: a variable set in the host never reaches it.
fn environment_is_minimal(mode: Mode) {
    // nextest runs each test in its own process, so this does not leak into other tests.
    std::env::set_var("LPK_SANDBOX_SENTINEL_SECRET", "do-not-leak");
    let s = stage();
    let (code, text) = run(&s, mode, limits(), &["env"]);
    assert_eq!(code, 0, "{text}");
    let names: Vec<&str> = text
        .lines()
        .map(|l| l.split('=').next().unwrap_or(l))
        .collect();
    if mode == Mode::Lpac {
        // The launch adds TEMP and TMP and points all three at the package's own folders.
        assert_eq!(
            names,
            ["env SystemRoot", "env LOCALAPPDATA", "env TEMP", "env TMP"],
            "{text}"
        );
        let pkg = format!(r"\packages\{}", s.profile.0.to_lowercase());
        for l in text.lines().skip(1) {
            assert!(
                l.to_lowercase().contains(&pkg),
                "{l} is outside the package folder"
            );
        }
    } else {
        assert_eq!(names, ["env SystemRoot"], "{text}");
    }
}

#[test]
fn environment_is_minimal_lpac() {
    environment_is_minimal(Mode::Lpac);
}

#[test]
fn environment_is_minimal_restricted() {
    environment_is_minimal(Mode::RestrictedToken);
}

/// The registry: HKCU (whose default ACL grants RESTRICTED read) is closed to both modes now
/// that the restricted worker's third restricting SID is unique; HKLM\SOFTWARE stays readable to
/// the opt-in (Users may read it) — its residual, pinned — and closed to LPAC.
#[test]
fn registry_reach_lpac_and_restricted() {
    let s = stage();
    let reg = |mode, hive: &str, path: &str| run(&s, mode, limits(), &["reg", hive, path]).1;
    let denied = format!("reg err {ERROR_ACCESS_DENIED}");
    assert_eq!(reg(Mode::Lpac, "hkcu", "Software").trim(), denied);
    assert_eq!(
        reg(Mode::RestrictedToken, "hkcu", "Software").trim(),
        denied
    );
    assert_eq!(
        reg(Mode::RestrictedToken, "hkcu", "Environment").trim(),
        denied
    );
    let nt = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    assert_eq!(
        reg(Mode::RestrictedToken, "hklm", nt).trim(),
        "reg ok",
        "residual changed"
    );
    assert_eq!(reg(Mode::Lpac, "hklm", nt).trim(), denied);
}

/// An LPAC launch without its profile fails with the named error, not a bare "file not found".
#[test]
fn missing_profile_is_a_named_error() {
    let s = stage();
    let (_r, w) = std::io::pipe().expect("pipe");
    let err = Launch::new(&s.exe)
        .appcontainer_name("LitePack.Test.NoSuchProfile")
        .args(["report"])
        .handle(roles::RESPONSES, w.as_handle(), Access::Write)
        .spawn()
        .expect_err("no profile");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    let inner = err
        .get_ref()
        .and_then(|e| e.downcast_ref::<lpk_sandbox_sys::ProfileMissing>())
        .expect("ProfileMissing");
    assert_eq!(inner.0, "LitePack.Test.NoSuchProfile");
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
    let worker = s
        .launch()
        .mode(mode)
        .limits(limits())
        .args(["sleep", "60000"])
        .handle(roles::RESPONSES, w.as_handle(), Access::Write)
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
fn cannot_spawn_lpac() {
    cannot_spawn(Mode::Lpac);
}

#[test]
fn cannot_spawn_restricted() {
    cannot_spawn(Mode::RestrictedToken);
}

/// A handle the host made inheritable before the launch (as a careless host might): it must
/// not reach the worker, since nothing is inherited.
fn inheritable_sentinel(path: &std::path::Path) -> std::fs::File {
    use windows::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
    std::fs::write(path, b"sentinel").expect("write sentinel");
    let f = std::fs::File::open(path).expect("open sentinel");
    // SAFETY: `f` owns a valid file handle for the duration of the call.
    unsafe {
        SetHandleInformation(
            HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&f)),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAG_INHERIT,
        )
    }
    .expect("make the sentinel inheritable");
    f
}

fn only_passed_handles(mode: Mode) {
    let s = stage();
    let archive_path = s.exe.with_file_name("archive.bin");
    std::fs::write(&archive_path, b"archive").expect("write archive");
    let archive = std::fs::File::open(&archive_path).expect("open archive");
    let _sentinel = inheritable_sentinel(&s.exe.with_file_name("inheritable-sentinel.txt"));
    let (req_r, _req_w) = std::io::pipe().expect("pipe");
    let (mut resp_r, resp_w) = std::io::pipe().expect("pipe");
    let (_err_r, err_w) = std::io::pipe().expect("pipe");
    let worker = s
        .launch()
        .mode(mode)
        .limits(limits())
        .args(["report"])
        .handle(roles::REQUESTS, req_r.as_handle(), Access::Read)
        .handle(roles::RESPONSES, resp_w.as_handle(), Access::Write)
        .handle(roles::STDERR, err_w.as_handle(), Access::Write)
        .handle(roles::ARCHIVE, archive.as_handle(), Access::Read)
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
    // (value, access, type name, object name or "-")
    let table: Vec<(u64, u32, &str, &str)> = text
        .lines()
        .filter_map(|l| l.strip_prefix("handle "))
        .map(|h| {
            let f: Vec<&str> = h.splitn(5, ' ').collect();
            (
                f[0].parse().expect("value"),
                u32::from_str_radix(f[2].trim_start_matches("0x"), 16).expect("access"),
                f[3],
                f[4],
            )
        })
        .collect();
    let find = |v: u64| {
        *table
            .iter()
            .find(|e| e.0 == v)
            .unwrap_or_else(|| panic!("{v} not in table: {text}"))
    };
    // Each passed handle carries exactly the access asked for, never the host's.
    for (i, want) in [
        FILE_GENERIC_READ,
        FILE_GENERIC_WRITE,
        FILE_GENERIC_WRITE,
        FILE_GENERIC_READ,
    ]
    .into_iter()
    .enumerate()
    {
        let e = find(passed[i].1);
        assert_eq!((e.1, e.2), (want, "File"), "role {}: {text}", passed[i].0);
    }
    // The table is the one the worker had when `main` began, every type included. No handle to
    // the host's objects: no process, thread or token handle, nothing naming the inheritable
    // sentinel, and no File-type handle (files, pipes, devices) but the passed ones, apart from
    // KNOWN_LOADER_FILE_HANDLES (opened by the worker's own DLLs while loading) and a
    // current-directory handle if the loader opened one (SYNCHRONIZE | FILE_TRAVERSE).
    let not_passed = || table.iter().filter(|e| !passed.iter().any(|p| p.1 == e.0));
    let foreign: Vec<_> = not_passed()
        .filter(|e| {
            matches!(e.2, "Process" | "Thread" | "Token")
                || e.3.ends_with("inheritable-sentinel.txt")
        })
        .collect();
    assert!(
        foreign.is_empty(),
        "host objects in the worker {foreign:?}:\n{text}"
    );
    let extra: Vec<_> = not_passed()
        .filter(|e| e.2 == "File")
        .filter(|e| !KNOWN_LOADER_FILE_HANDLES.contains(&e.3) && e.1 != 0x0010_0020)
        .collect();
    assert!(
        extra.is_empty(),
        "unexpected File handles {extra:?} in\n{text}"
    );
    assert!(
        table.iter().all(|e| e.2 != "?"),
        "a handle type could not be named:\n{text}"
    );
}

#[test]
fn only_passed_handles_lpac() {
    only_passed_handles(Mode::Lpac);
}

#[test]
fn only_passed_handles_restricted() {
    only_passed_handles(Mode::RestrictedToken);
}

fn console_less_host(mode: &str) {
    let s = stage();
    let out = s.exe.with_file_name(format!("host-{mode}.txt"));
    let status = std::process::Command::new(&s.exe)
        .args(["host", mode])
        .arg(&out)
        .arg(&s.profile.0)
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

/// The LPAC shape, by behaviour (`TokenIsLessPrivilegedAppContainer` answers
/// ERROR_INVALID_PARAMETER on Windows 11 26200): a file granted to ALL APPLICATION PACKAGES is
/// denied (the opt-out works), one granted to the worker's package SID is readable (the path is
/// reachable).
#[test]
fn lpac_ignores_all_application_packages() {
    let s = stage();
    let data = tempfile::tempdir().expect("tempdir");
    let aap_file = data.path().join("all-app-packages.txt");
    std::fs::write(&aap_file, b"aap").expect("write");
    grant_path_access(
        &aap_file,
        ALL_APPLICATION_PACKAGES,
        FILE_GENERIC_READ,
        false,
    )
    .expect("grant aap");
    let pkg_file = data.path().join("package.txt");
    std::fs::write(&pkg_file, b"package").expect("write");
    let sid = appcontainer_sid_string(&s.profile.0).expect("sid");
    // Traversal into `data` comes from the bypass-traverse privilege; reading needs the ACE.
    grant_path_access(&pkg_file, &sid, FILE_GENERIC_READ, false).expect("grant package");

    let (_, text) = run(
        &s,
        Mode::Lpac,
        limits(),
        &["read", aap_file.to_str().expect("utf8")],
    );
    assert_eq!(text.trim(), format!("read err {ERROR_ACCESS_DENIED}"));
    let (_, text) = run(
        &s,
        Mode::Lpac,
        limits(),
        &["read", pkg_file.to_str().expect("utf8")],
    );
    assert_eq!(text.trim(), "read ok 7");
}

/// Past its user-time limit the worker is killed (STATUS_QUOTA_EXCEEDED); raising the limit on
/// the running worker (8 s spun against a 500 ms start limit raised to 30 s) saves it.
#[test]
fn cpu_limit_kills_and_can_be_raised_while_running() {
    let s = stage();
    let lim = Limits {
        cpu_time: Duration::from_millis(500),
        ..limits()
    };
    let t0 = std::time::Instant::now();
    let (code, text) = run(&s, Mode::Lpac, lim, &["spin", "30000"]);
    // The job enforces user-time limits periodically: the kill lands about 3 s after the limit
    // (measured 3.3-3.7 s wall for a 500 ms limit), so the spin must outlast that lag.
    let elapsed = t0.elapsed();
    assert!(
        text.contains("spinning") && !text.contains("spin done"),
        "{text}"
    );
    assert_eq!(code, STATUS_QUOTA_EXCEEDED, "{text}");
    assert!(
        elapsed < Duration::from_secs(20),
        "killed only after {elapsed:?}"
    );

    let (r, w) = std::io::pipe().expect("pipe");
    let worker = s
        .launch()
        .limits(lim)
        .args(["spin", "8000"])
        .handle(roles::RESPONSES, w.as_handle(), Access::Write)
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
