//! `lpk-worker-libarchive`: LitePack's sandboxed libarchive worker (E4-4; design
//! `docs/notes/e4-design.md` §1 and §3). Started by the broker (`lpk_core::foreign`) through
//! `lpk-sandbox-sys` in an LPAC AppContainer only (the host refuses the restricted token for it)
//! with the archive's volumes as read-only handles (roles `ARCHIVE`, `ARCHIVE + 1`, …) and three
//! pipes; it answers the broker with `lpk-proto`'s worker loop and exits. It writes no file.
#![forbid(unsafe_code)]

/// Exit status when the bootstrap is missing or malformed.
const EXIT_NO_BOOTSTRAP: i32 = 3;

#[cfg(any(windows, target_os = "linux"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let handles = match lpk_sandbox_sys::child::take_handles() {
        Ok(h) => h,
        Err(_) => std::process::exit(EXIT_NO_BOOTSTRAP),
    };
    let (handles, volumes) = lpk_worker_libarchive::split_volumes(
        handles
            .into_iter()
            .map(|(role, h)| (role, std::fs::File::from(h)))
            .collect(),
    );
    let opener = move |first: std::fs::File, args: &lpk_proto::OpenArgs<'_>| {
        lpk_worker_libarchive::open(first, &volumes, args)
    };
    std::process::exit(lpk_proto::worker::run(handles, &args, &opener));
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    std::process::exit(EXIT_NO_BOOTSTRAP);
}
