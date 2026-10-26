//! E4-4 through the host: `lpk_core::foreign` routes the libarchive formats to the real
//! `lpk-worker-libarchive`, under LPAC on Windows (a test AppContainer profile
//! `LitePack.Test.La.<pid>.<n>`, deleted at the end) and unsandboxed on Linux (decision (d)).
//! The restricted token is refused for these formats before any worker starts.
//!
//! The listing and extraction checks need libarchive; while `vendor/libarchive-3.8.9/` is
//! absent they are skipped with a message (the named-gap and refusal checks run regardless,
//! since the RAR pre-check runs in the worker before libarchive).
#![cfg(any(windows, target_os = "linux"))]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use lpk_core::foreign::{self, Options, Sandbox};
use lpk_core::policy::DefaultPolicy;
use lpk_core::{CoreError, ForeignError, Monitor, Selection, WorkerError};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// The worker staged where the sandbox may run it, and the options naming it.
struct Staged {
    opts: Options,
    #[cfg(windows)]
    profile: String,
    _dir: tempfile::TempDir,
}

#[cfg(windows)]
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = lpk_sandbox_sys::delete_appcontainer_profile(&self.profile);
    }
}

fn stage() -> Staged {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join(foreign::WORKER_LIBARCHIVE_NAME);
    let mut opts = Options::default();
    #[cfg(windows)]
    let profile = {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        const FILE_GENERIC_READ: u32 = 0x0012_0089;
        const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;
        let profile = format!(
            "LitePack.Test.La.{}.{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        lpk_sandbox_sys::create_appcontainer_profile(&profile).unwrap();
        let sid = lpk_sandbox_sys::appcontainer_sid_string(&profile).unwrap();
        lpk_sandbox_sys::grant_path_access(
            dir.path(),
            &sid,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            true,
        )
        .unwrap();
        opts.appcontainer = Some(profile.clone());
        profile
    };
    std::fs::copy(env!("CARGO_BIN_EXE_lpk-worker-libarchive"), &exe).unwrap();
    opts.worker_libarchive = Some(exe);
    opts.sandbox = Sandbox::Lpac;
    Staged {
        opts,
        #[cfg(windows)]
        profile,
        _dir: dir,
    }
}

#[test]
fn the_restricted_token_is_refused_for_libarchive_formats() {
    let opts = Options {
        sandbox: Sandbox::Restricted,
        worker_libarchive: Some(PathBuf::from("no-such-worker")),
        ..Options::default()
    };
    for name in [
        "rar5.rar",
        "cab-mszip.cab",
        "iso-joliet.iso",
        "fixture.cpio",
    ] {
        let e = foreign::list_file(&fixture(name), None, &opts, &Monitor::default()).unwrap_err();
        match e {
            CoreError::Worker(WorkerError::Sandbox { message }) => {
                assert_eq!(message, foreign::RESTRICTED_REFUSED, "{name}")
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[test]
fn rar_gaps_are_named_errors_through_the_sandboxed_worker() {
    let s = stage();
    for (name, words) in [
        ("gap-rar4-solid.rar", "solid RAR 4"),
        ("gap-rar5-encrypted-headers.rar", "encrypted RAR 5 headers"),
        ("gap-rar20.rar", "RAR 1.5/2.x"),
    ] {
        let e = foreign::list_file(&fixture(name), None, &s.opts, &Monitor::default()).unwrap_err();
        match e {
            CoreError::Foreign(ForeignError::Unsupported { message }) => {
                assert!(message.contains(words), "{name}: {message}")
            }
            other => panic!("{name}: {other:?}"),
        }
    }
    // The host is still fine: the next archive through a fresh worker answers too.
    let e = foreign::list_file(
        &fixture("gap-rar4-encrypted.rar"),
        None,
        &s.opts,
        &Monitor::default(),
    )
    .unwrap_err();
    assert!(
        matches!(e, CoreError::Foreign(ForeignError::Unsupported { .. })),
        "{e:?}"
    );
}

#[test]
fn fixtures_extract_with_blake3_equality_through_the_host() {
    if !lpk_libarchive_sys::BUILT {
        eprintln!(
            "SKIPPED fixtures_extract_with_blake3_equality_through_the_host: libarchive is not \
             built (vendor/libarchive-3.8.9 absent)"
        );
        return;
    }
    let manifest = common::manifest();
    let s = stage();
    for (archive, expected) in common::CASES {
        let out = tempfile::tempdir().unwrap();
        let summary = foreign::extract_file(
            &fixture(archive),
            out.path(),
            None,
            &DefaultPolicy,
            &Selection::all(),
            &s.opts,
            &Monitor::default(),
        )
        .unwrap_or_else(|e| panic!("{archive}: {e}"));
        assert!(summary.files > 0, "{archive}");
        for rel in *expected {
            let got = std::fs::read(out.path().join(rel))
                .unwrap_or_else(|e| panic!("{archive}: {rel}: {e}"));
            assert_eq!(
                blake3::hash(&got).to_hex().to_string(),
                manifest[*rel],
                "{archive}: {rel}"
            );
        }
    }
}

mod common;
