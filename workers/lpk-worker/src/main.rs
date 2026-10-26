//! `lpk-worker`: LitePack's sandboxed foreign-format worker (E4-2; design
//! `docs/notes/e4-design.md` §2). Started by the broker (`lpk_core::foreign`) through
//! `lpk-sandbox-sys` with the archive as a read-only handle and three pipes; it takes them from
//! the bootstrap page, answers the broker with `lpk-proto`'s worker loop and exits. It writes no
//! file and never uses `TEMP`.
//!
//! The readers are `lpk-foreign`'s (E4-3): ZIP, 7z, tar and the single compressed streams,
//! selected by the format the host detected (`Open { format }`); the test fixture of
//! `lpk_proto::fixture` is served too (the protocol tests use it). Built without the `faults` feature for release; a
//! `--fault=` argument is then refused.
//!
//! `lpk-worker --remove-profile` (unsandboxed, by the uninstaller) deletes the AppContainer
//! profile `LitePack.Worker` (Windows).
#![forbid(unsafe_code)]

/// Exit status when the bootstrap is missing or malformed.
const EXIT_NO_BOOTSTRAP: i32 = 3;

#[cfg(any(windows, target_os = "linux"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--remove-profile") {
        std::process::exit(remove_profile());
    }
    let handles = match lpk_sandbox_sys::child::take_handles() {
        Ok(h) => h,
        Err(_) => std::process::exit(EXIT_NO_BOOTSTRAP),
    };
    let files = handles
        .into_iter()
        .map(|(role, h)| (role, std::fs::File::from(h)))
        .collect();
    std::process::exit(lpk_proto::worker::run(files, &args, &lpk_foreign::open));
}

#[cfg(windows)]
fn remove_profile() -> i32 {
    match lpk_sandbox_sys::delete_appcontainer_profile(lpk_sandbox_sys::APPCONTAINER_NAME) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("lpk-worker: removing the AppContainer profile: {e}");
            1
        }
    }
}

#[cfg(target_os = "linux")]
fn remove_profile() -> i32 {
    0
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    std::process::exit(EXIT_NO_BOOTSTRAP);
}
