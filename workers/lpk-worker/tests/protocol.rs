//! `lpk-worker` speaks the protocol: started through `lpk-sandbox-sys` (LPAC on Windows with a
//! profile per test; unsandboxed on Linux), driven with `lpk-proto`'s frames over a fixture.
#![cfg(any(windows, target_os = "linux"))]
#![allow(clippy::unwrap_used)]

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lpk_proto::fixture::{self, Member};
use lpk_proto::{Request, Response};
use lpk_sandbox_sys::{roles, Access, Launch, Limits};

struct Staged {
    exe: PathBuf,
    profile: String,
    dir: tempfile::TempDir,
}

impl Drop for Staged {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = lpk_sandbox_sys::delete_appcontainer_profile(&self.profile);
    }
}

fn stage() -> Staged {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = tempfile::tempdir().unwrap();
    let src = PathBuf::from(env!("CARGO_BIN_EXE_lpk-worker"));
    let exe = dir.path().join(src.file_name().unwrap());
    std::fs::copy(&src, &exe).unwrap();
    let profile = format!(
        "LitePack.Test.Worker.{}.{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    #[cfg(windows)]
    {
        const READ_EXECUTE: u32 = 0x0012_0089 | 0x0012_00A0;
        lpk_sandbox_sys::create_appcontainer_profile(&profile).unwrap();
        let sid = lpk_sandbox_sys::appcontainer_sid_string(&profile).unwrap();
        lpk_sandbox_sys::grant_path_access(dir.path(), &sid, READ_EXECUTE, true).unwrap();
    }
    Staged { exe, profile, dir }
}

#[cfg(windows)]
fn b<T: std::os::windows::io::AsHandle>(t: &T) -> lpk_sandbox_sys::Borrowed<'_> {
    t.as_handle()
}
#[cfg(unix)]
fn b<T: std::os::fd::AsFd>(t: &T) -> lpk_sandbox_sys::Borrowed<'_> {
    t.as_fd()
}

/// Send `reqs` (then close the request pipe), collect every response and the exit status.
fn session(s: &Staged, fx: &[u8], args: &[&str], reqs: &[Request]) -> (u32, Vec<Response>, String) {
    let archive = s.dir.path().join("a.fx");
    std::fs::write(&archive, fx).unwrap();
    let file = std::fs::File::open(&archive).unwrap();
    let (req_r, mut req_w) = std::io::pipe().unwrap();
    let (mut resp_r, resp_w) = std::io::pipe().unwrap();
    let (mut err_r, err_w) = std::io::pipe().unwrap();
    let worker = Launch::new(&s.exe)
        .appcontainer_name(&s.profile)
        .limits(Limits {
            memory: 256 << 20,
            cpu_time: Duration::from_secs(30),
            cpu_ceiling: Duration::from_secs(60),
        })
        .args(args.iter().copied())
        .handle(roles::REQUESTS, b(&req_r), Access::Read)
        .handle(roles::RESPONSES, b(&resp_w), Access::Write)
        .handle(roles::STDERR, b(&err_w), Access::Write)
        .handle(roles::ARCHIVE, b(&file), Access::Read)
        .spawn()
        .unwrap();
    drop((req_r, resp_w, err_w, file));
    for r in reqs {
        r.write_to(&mut req_w).unwrap();
    }
    drop(req_w);
    let mut out = Vec::new();
    let mut buf = Vec::new();
    while let Ok(Some(r)) = Response::read_from(&mut resp_r, &mut buf) {
        out.push(r);
    }
    let mut diag = String::new();
    let _ = err_r.read_to_string(&mut diag);
    let code = worker.wait(Some(Duration::from_secs(30))).unwrap().unwrap();
    (code, out, diag)
}

#[test]
fn the_worker_round_trips_a_session() {
    let s = stage();
    let fx = fixture::write(
        &[
            Member::dir("d"),
            Member::file("d/a", b"hello"),
            Member::file("d/b", &vec![3u8; 300_000]),
        ],
        None,
    );
    let (code, rs, diag) = session(
        &s,
        &fx,
        &[],
        &[
            Request::Open {
                format: fixture::FORMAT.into(),
                name: Vec::new(),
                password: None,
            },
            Request::List { from: 0, max: 10 },
            Request::ReadMember {
                indices: vec![1, 2],
            },
            Request::Close,
        ],
    );
    assert_eq!(code, 0, "{diag}");
    assert!(
        matches!(&rs[0], Response::Hello { proto, build } if *proto == lpk_proto::PROTO && build == lpk_proto::BUILD)
    );
    assert!(matches!(&rs[1], Response::Opened { format } if format == fixture::FORMAT));
    assert!(matches!(&rs[2], Response::Entries { records, more: false } if records.len() == 3));
    assert_eq!(rs[3], Response::MemberStart { index: 1 });
    let mut data: Vec<u8> = Vec::new();
    let mut ends = 0;
    for r in &rs[4..] {
        match r {
            Response::Data(d) => data.extend_from_slice(d),
            Response::MemberEnd { .. } => ends += 1,
            Response::MemberStart { index: 2 } => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(ends, 2);
    assert_eq!(data.len(), 5 + 300_000);
    assert_eq!(&data[..5], b"hello");
}

/// A shipped worker (no `faults` feature) refuses to be told to misbehave.
#[cfg(not(feature = "faults"))]
#[test]
fn a_release_worker_refuses_faults() {
    let s = stage();
    let fx = fixture::write(&[Member::file("a", b"x")], None);
    let (code, rs, diag) = session(&s, &fx, &["--fault=hang"], &[Request::Close]);
    assert_eq!(code, 5, "{diag}");
    assert!(rs.is_empty());
    assert!(diag.contains("without faults"), "{diag}");
}

/// With the feature (tests only), the fault argument is taken.
#[cfg(feature = "faults")]
#[test]
fn a_test_build_takes_faults() {
    let s = stage();
    let fx = fixture::write(&[Member::file("a", b"x")], None);
    let (code, rs, _) = session(
        &s,
        &fx,
        &["--fault=garbage"],
        &[
            Request::Open {
                format: fixture::FORMAT.into(),
                name: Vec::new(),
                password: None,
            },
            Request::Close,
        ],
    );
    assert_eq!(code, 0);
    assert!(rs.len() <= 1, "garbage should not decode past Hello");
}
