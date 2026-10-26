//! E4-5, crash containment (Windows): a worker "parser" that aborts, dereferences null,
//! allocates past the job's commit limit, spins past the CPU-time limit, spawns a process,
//! opens the network and opens a file outside its allowance, each under both sandbox modes
//! (LPAC, the default, and the opt-in restricted token). Each test asserts the broker's named
//! cause, that the host (this test process) is still running, and that the next extraction in
//! the same session — a good fixture through a fresh worker, same profile and mode — succeeds.
//!
//! The cases are `lpk-test-worker`'s `--crash=<case>` (this package's binary), except the null
//! dereference, which needs `unsafe` and is `lpk-sandbox-sys`'s test worker's
//! `crash-at-request` (found beside this test's executable, built by the workspace build): it
//! sends the broker's own `Hello` (encoded here and passed on its command line), waits for the
//! first request and faults.
//!
//! The network case connects to a loopback listener of this test and to TEST-NET-1
//! (192.0.2.1, never routed). Under LPAC both are refused and the listener sees nothing; under
//! the restricted-token opt-in the loopback connection gets through: that mode's documented
//! residual (D-97), pinned here — the extraction then succeeds and the listener receives the
//! worker's bytes. No firewall rule or machine setting is touched.
//!
//! Each test creates its own AppContainer profile (`LitePack.Test.Crash.<pid>.<n>`) and deletes
//! it at its end, also on panic; only a killed test process leaves one behind.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lpk_core::foreign::{self, ForeignSummary, Limits, Options, Sandbox};
use lpk_core::{CoreError, Monitor, Selection, WorkerError};
use lpk_proto::fixture::{self, Member};
use lpk_proto::Response;

/// What a test allows beyond its limit: the AppContainer launch, the broker's polling and the
/// kill, on a machine loaded by the rest of the workspace's tests (an abort once took 33 s
/// end to end while two workspace test runs overlapped).
const SLACK: Duration = Duration::from_secs(30);
/// The most the job's periodic user-time check may lag behind the limit (see `spin`).
const SPIN_LAG: Duration = Duration::from_secs(10);
/// `std::process::abort()` on Windows is a fail-fast (`__fastfail`), whose exit status is
/// STATUS_STACK_BUFFER_OVERRUN.
const STATUS_FAIL_FAST: u32 = 0xC000_0409;
/// An unhandled access violation (the job's die-on-unhandled-exception makes it the exit status).
const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
const FILE_GENERIC_READ: u32 = 0x0012_0089;
const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;

/// The two workers copied into a directory the test profile's package SID may read and
/// execute (as the installer grants it on the worker images), and the profile; both removed at
/// the end.
struct Staged {
    proto_worker: PathBuf,
    raw_worker: PathBuf,
    profile: String,
    _dir: tempfile::TempDir,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = lpk_sandbox_sys::delete_appcontainer_profile(&self.profile);
    }
}

/// `lpk-sandbox-sys`'s test worker, built beside this test's executable (`target/<profile>/`).
fn sandbox_test_worker() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).unwrap();
    let w = dir.join("lpk-sandbox-testworker.exe");
    assert!(
        w.is_file(),
        "{} missing: build the workspace (cargo build --workspace, or cargo nextest run --workspace)",
        w.display()
    );
    w
}

fn stage() -> Staged {
    static N: AtomicU32 = AtomicU32::new(0);
    let profile = format!(
        "LitePack.Test.Crash.{}.{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    lpk_sandbox_sys::create_appcontainer_profile(&profile).unwrap();
    let dir = tempfile::tempdir().unwrap();
    // From here the profile is deleted by `Staged`'s drop, also if a step below panics.
    let s = Staged {
        proto_worker: dir.path().join("lpk-test-worker.exe"),
        raw_worker: dir.path().join("lpk-sandbox-testworker.exe"),
        profile,
        _dir: dir,
    };
    let sid = lpk_sandbox_sys::appcontainer_sid_string(&s.profile).unwrap();
    let parent = s.proto_worker.parent().unwrap();
    lpk_sandbox_sys::grant_path_access(
        parent,
        &sid,
        FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        true,
    )
    .unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_lpk-test-worker"), &s.proto_worker).unwrap();
    std::fs::copy(sandbox_test_worker(), &s.raw_worker).unwrap();
    s
}

impl Staged {
    fn options(&self, mode: Sandbox, worker: &Path, args: Vec<String>, limits: Limits) -> Options {
        Options {
            worker: Some(worker.to_path_buf()),
            sandbox: mode,
            appcontainer: Some(self.profile.clone()),
            limits,
            worker_args: args,
        }
    }
}

fn members() -> Vec<Member> {
    let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    vec![
        Member::dir("d"),
        Member::file("d/a.txt", b"hello"),
        Member::file("d/big.bin", &big),
    ]
}

/// The limits of every case: short watchdogs; the cases that test a limit set it.
fn limits() -> Limits {
    Limits {
        no_progress: Duration::from_secs(30),
        ..Limits::default()
    }
}

/// One extraction of a good fixture through `worker` with `args`; the result and its time.
fn extract(
    s: &Staged,
    mode: Sandbox,
    worker: &Path,
    args: Vec<String>,
    limits: Limits,
) -> (
    Result<ForeignSummary, CoreError>,
    Duration,
    tempfile::TempDir,
) {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("archive.fx");
    std::fs::write(&archive, fixture::write(&members(), None)).unwrap();
    let out = work.path().join("out");
    let t = Instant::now();
    let r = foreign::extract_file(
        &archive,
        &out,
        None,
        &lpk_core::DefaultPolicy,
        &Selection::all(),
        &s.options(mode, worker, args, limits),
        &Monitor::default(),
    );
    (r, t.elapsed(), work)
}

/// The host is alive (this code runs) and its next extraction in the same session, through a
/// fresh worker of the same profile and mode, succeeds with every byte.
fn host_continues(s: &Staged, mode: Sandbox) {
    let (r, _, work) = extract(s, mode, &s.proto_worker, Vec::new(), Limits::default());
    let sum = r.unwrap_or_else(|e| panic!("the next extraction failed: {e}"));
    assert_eq!((sum.files, sum.directories), (2, 1));
    let out = work.path().join("out");
    for m in members() {
        if m.data.is_empty() {
            continue;
        }
        let p = out.join(std::str::from_utf8(&m.name).unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), m.data, "{}", p.display());
    }
}

/// Run `--crash=<case>` of `lpk-test-worker`; the error and the time.
fn crash(s: &Staged, mode: Sandbox, case: &str, limits: Limits) -> (CoreError, Duration) {
    let args = vec![format!("--crash={case}")];
    let (r, took, work) = extract(s, mode, &s.proto_worker, args, limits);
    let e = match r {
        Ok(sum) => panic!("the {case} case extracted: {sum:?}"),
        Err(e) => e,
    };
    // The named cause, for the test log (CI evidence).
    eprintln!("{case} under {mode:?}: {e} ({took:?})");
    // Nothing of the failed extraction is left half-written.
    assert!(!work.path().join("out/d/big.bin").exists());
    (e, took)
}

fn assert_denied(e: &CoreError, resource: &str) {
    match e {
        CoreError::Worker(WorkerError::Denied { message }) => {
            assert!(message.starts_with(resource), "{message}")
        }
        other => panic!("expected Denied({resource}), got {other:?}: {other}"),
    }
}

// --- abort ----------------------------------------------------------------------------------

fn abort(mode: Sandbox) {
    let s = stage();
    let (e, took) = crash(&s, mode, "abort", limits());
    assert!(
        matches!(
            e,
            CoreError::Worker(WorkerError::Crashed {
                code: STATUS_FAIL_FAST,
                ..
            })
        ),
        "{e:?}: {e}"
    );
    assert!(took < SLACK, "{took:?}");
    host_continues(&s, mode);
}

#[test]
fn abort_lpac() {
    abort(Sandbox::Lpac);
}

#[test]
fn abort_restricted() {
    abort(Sandbox::Restricted);
}

// --- null dereference ----------------------------------------------------------------------

fn null_dereference(mode: Sandbox) {
    let s = stage();
    let mut hello = Vec::new();
    Response::Hello {
        proto: lpk_proto::PROTO,
        build: lpk_proto::BUILD.to_string(),
    }
    .write_to(&mut hello)
    .unwrap();
    let hex: String = hello.iter().map(|b| format!("{b:02x}")).collect();
    let args = vec!["crash-at-request".to_string(), hex];
    let (r, took, work) = extract(&s, mode, &s.raw_worker, args, limits());
    let e = r.unwrap_err();
    eprintln!("null dereference under {mode:?}: {e} ({took:?})");
    assert!(
        matches!(
            e,
            CoreError::Worker(WorkerError::Crashed {
                code: STATUS_ACCESS_VIOLATION,
                ..
            })
        ),
        "{e:?}: {e}"
    );
    assert!(took < SLACK, "{took:?}");
    assert!(!work.path().join("out").exists());
    host_continues(&s, mode);
}

#[test]
fn null_dereference_lpac() {
    null_dereference(Sandbox::Lpac);
}

#[test]
fn null_dereference_restricted() {
    null_dereference(Sandbox::Restricted);
}

// --- allocation past the commit limit -------------------------------------------------------

fn allocation(mode: Sandbox) {
    let s = stage();
    let memory = 256u64 << 20;
    let lim = Limits {
        memory: Some(memory),
        ..limits()
    };
    let (e, took) = crash(&s, mode, "alloc", lim);
    match e {
        CoreError::Worker(WorkerError::MemoryLimit { limit }) => assert_eq!(limit, memory),
        other => panic!("expected MemoryLimit, got {other:?}: {other}"),
    }
    assert!(took < SLACK, "{took:?}");
    host_continues(&s, mode);
}

#[test]
fn allocation_past_the_limit_lpac() {
    allocation(Sandbox::Lpac);
}

#[test]
fn allocation_past_the_limit_restricted() {
    allocation(Sandbox::Restricted);
}

// --- spinning past the CPU-time limit -------------------------------------------------------

fn spin(mode: Sandbox) {
    let s = stage();
    let cpu = Duration::from_secs(2);
    let lim = Limits {
        max_cpu: Some(cpu),
        ..limits()
    };
    let (e, took) = crash(&s, mode, "spin", lim);
    match e {
        CoreError::Worker(WorkerError::CpuLimit { seconds }) => assert_eq!(seconds, 2),
        other => panic!("expected CpuLimit, got {other:?}: {other}"),
    }
    // The job enforces user-time limits periodically, not at once: the kill landed 4.6-8.8 s
    // wall after the start for this 2 s limit on Windows 11 26200 (E4-1 measured a lag of about
    // 3 s), hence the wider bound.
    assert!(took >= cpu, "{took:?}");
    assert!(took < cpu + SPIN_LAG + SLACK, "{took:?}");
    host_continues(&s, mode);
}

#[test]
fn spin_past_the_cpu_limit_lpac() {
    spin(Sandbox::Lpac);
}

#[test]
fn spin_past_the_cpu_limit_restricted() {
    spin(Sandbox::Restricted);
}

// --- spawning a process ---------------------------------------------------------------------

fn spawn(mode: Sandbox) {
    let s = stage();
    let (e, took) = crash(&s, mode, "spawn", limits());
    assert_denied(&e, "process:");
    assert!(took < SLACK, "{took:?}");
    host_continues(&s, mode);
}

#[test]
fn spawn_a_process_lpac() {
    spawn(Sandbox::Lpac);
}

#[test]
fn spawn_a_process_restricted() {
    spawn(Sandbox::Restricted);
}

// --- opening the network --------------------------------------------------------------------

/// A loopback listener; `accepted` reports what one connection sent within `wait`, if any came.
struct Listener(TcpListener);

impl Listener {
    fn new() -> Listener {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.set_nonblocking(true).unwrap();
        Listener(l)
    }
    fn port(&self) -> u16 {
        self.0.local_addr().unwrap().port()
    }
    fn accepted(&self, wait: Duration) -> Option<Vec<u8>> {
        let end = Instant::now() + wait;
        loop {
            match self.0.accept() {
                Ok((mut c, _)) => {
                    c.set_nonblocking(false).unwrap();
                    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut got = Vec::new();
                    let _ = c.read_to_end(&mut got);
                    return Some(got);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() > end {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("accept: {e}"),
            }
        }
    }
}

#[test]
fn open_the_network_lpac() {
    let s = stage();
    let l = Listener::new();
    let (e, took) = crash(
        &s,
        Sandbox::Lpac,
        &format!("network:{}", l.port()),
        limits(),
    );
    assert_denied(&e, "network:");
    assert!(took < SLACK, "{took:?}");
    assert_eq!(
        l.accepted(Duration::ZERO),
        None,
        "the LPAC worker reached the host"
    );
    host_continues(&s, Sandbox::Lpac);
}

/// The opt-in's residual (D-97 (4), `docs/notes/e4-design.md` §1): the restricted token cannot
/// close the network. Pinned: the worker's connection reaches this test's listener and the host
/// cannot tell (the extraction succeeds). If this ever fails because the connection is refused,
/// the residual is gone and the security notes should say so.
#[test]
fn open_the_network_restricted_is_the_residual() {
    let s = stage();
    let l = Listener::new();
    let args = vec![format!("--crash=network:{}", l.port())];
    let (r, took, _work) = extract(&s, Sandbox::Restricted, &s.proto_worker, args, limits());
    let sum = r.unwrap_or_else(|e| panic!("expected the residual (a quiet success), got {e}"));
    assert_eq!(sum.files, 2);
    assert!(took < SLACK, "{took:?}");
    assert_eq!(
        l.accepted(Duration::from_secs(5)).as_deref(),
        Some(&b"lpk-test-worker"[..]),
        "the restricted worker's connection did not arrive"
    );
    host_continues(&s, Sandbox::Restricted);
}

// --- opening a file outside the allowance ---------------------------------------------------

/// A file of the user's (TEMP's owner-only ACL, no Users or package grant): outside the
/// worker's allowance in both modes. (A file readable by BUILTIN\Users is the opt-in's residual,
/// pinned by `lpk-sandbox-sys`'s `users_readable_file_lpac_denied_restricted_readable`.)
fn file_outside(mode: Sandbox) {
    let s = stage();
    let secret_dir = tempfile::tempdir().unwrap();
    let secret = secret_dir.path().join("secret.txt");
    std::fs::write(&secret, b"secret").unwrap();
    let case = format!("read:{}", secret.to_str().unwrap());
    let (e, took) = crash(&s, mode, &case, limits());
    assert_denied(&e, "file:");
    assert!(took < SLACK, "{took:?}");
    host_continues(&s, mode);
}

#[test]
fn open_a_file_outside_the_allowance_lpac() {
    file_outside(Sandbox::Lpac);
}

#[test]
fn open_a_file_outside_the_allowance_restricted() {
    file_outside(Sandbox::Restricted);
}
