//! E5-6: `Session::read_entry` on a foreign archive through the sandboxed test worker
//! (`lpk-test-worker`, built beside the test executable by a workspace build; LPAC on
//! Windows with a profile per test, deleted at its end; unsandboxed on Linux).
#![cfg(any(windows, target_os = "linux"))]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use lpk_api::{CancelToken, Class, Credentials, OpenOptions, Session};
use lpk_core::foreign::Options;
use lpk_proto::fixture::{self, Member};
use lpk_proto::RecordKind;

struct Staged {
    exe: PathBuf,
    profile: Option<String>,
    _dir: tempfile::TempDir,
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
    let n = N.fetch_add(1, Ordering::Relaxed);
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).unwrap();
    let name = if cfg!(windows) {
        "lpk-test-worker.exe"
    } else {
        "lpk-test-worker"
    };
    let src = dir.join(name);
    assert!(
        src.is_file(),
        "{} missing: build the workspace",
        src.display()
    );
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join(name);
    std::fs::copy(&src, &exe).unwrap();
    #[cfg(windows)]
    let profile = {
        const READ_EXECUTE: u32 = 0x0012_0089 | 0x0012_00A0;
        let p = format!("LitePack.Test.ApiRead.{}.{n}", std::process::id());
        lpk_sandbox_sys::create_appcontainer_profile(&p).unwrap();
        let sid = lpk_sandbox_sys::appcontainer_sid_string(&p).unwrap();
        lpk_sandbox_sys::grant_path_access(tmp.path(), &sid, READ_EXECUTE, true).unwrap();
        Some(p)
    };
    #[cfg(not(windows))]
    let profile = {
        let _ = n;
        None
    };
    Staged {
        exe,
        profile,
        _dir: tmp,
    }
}

impl Staged {
    fn options(&self, fault: Option<&str>) -> Options {
        Options {
            worker: Some(self.exe.clone()),
            appcontainer: self.profile.clone(),
            worker_args: fault
                .map(|f| vec![format!("--fault={f}")])
                .unwrap_or_default(),
            ..Options::default()
        }
    }
}

fn members() -> Vec<Member> {
    vec![
        Member::dir("d"),
        Member::file("d/a.txt", b"hello"),
        Member::link(RecordKind::HardLink, "d/copy.txt", "d/a.txt"),
    ]
}

fn write(dir: &Path, members: &[Member], pw: Option<&[u8]>) -> PathBuf {
    let p = dir.join("archive.fx");
    std::fs::write(&p, fixture::write(members, pw)).unwrap();
    p
}

#[test]
fn a_foreign_member_is_read_and_the_refusals_have_their_classes() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write(work.path(), &members(), None);
    let o = OpenOptions {
        foreign: s.options(None),
        ..OpenOptions::default()
    };
    let session = Session::open(&archive, &o).unwrap();
    let c = CancelToken::new();
    assert_eq!(session.read_entry(1, 100, &c).unwrap(), b"hello");
    // A directory and a hard link: Usage. A bad id: NotFound.
    for id in [0, 2] {
        let e = session.read_entry(id, 100, &c).unwrap_err();
        assert_eq!(e.class, Class::Usage, "{id}: {e}");
    }
    assert_eq!(
        session.read_entry(9, 100, &c).unwrap_err().class,
        Class::NotFound
    );
    // Over the cap: TooLarge, naming the cap and the member, with no worker started (the
    // worker that hangs on any member read would otherwise time out).
    let hang = OpenOptions {
        foreign: s.options(Some("hang-read-member")),
        ..OpenOptions::default()
    };
    let session = Session::open(&archive, &hang).unwrap();
    let e = session.read_entry(1, 2, &c).unwrap_err();
    assert_eq!(e.class, Class::TooLarge, "{e}");
    assert_eq!(e.kind, "Refused");
    assert_eq!(e.path.as_deref(), Some("d/a.txt"));
}

#[test]
fn an_encrypted_foreign_member_reads_with_the_credentials_given_at_open() {
    let s = stage();
    let work = tempfile::tempdir().unwrap();
    let archive = write(work.path(), &[Member::file("a", b"secret")], Some(b"pw"));
    let creds = Credentials::password(b"pw".to_vec());
    let o = OpenOptions {
        credentials: Some(&creds),
        foreign: s.options(None),
        ..OpenOptions::default()
    };
    let session = Session::open(&archive, &o).unwrap();
    assert_eq!(
        session.read_entry(0, 100, &CancelToken::new()).unwrap(),
        b"secret"
    );
}
