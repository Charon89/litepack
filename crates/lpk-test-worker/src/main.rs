//! Test-only worker of the main workspace (E4-2): the worker loop of `lpk-proto` over the test
//! fixture reader, built with the `faults` feature so the host's tests can make it send
//! garbage, hang, close its pipe or page forever (`--fault=<name>`). The host's tests find it
//! beside their own executables (`target/<profile>/`). Never shipped: the shipped worker is
//! `workers/lpk-worker`, built without faults in its own workspace.
#![forbid(unsafe_code)]

/// Exit status when the bootstrap is missing or malformed.
const EXIT_NO_BOOTSTRAP: i32 = 3;

#[cfg(any(windows, target_os = "linux"))]
fn main() {
    let handles = match lpk_sandbox_sys::child::take_handles() {
        Ok(h) => h,
        Err(_) => std::process::exit(EXIT_NO_BOOTSTRAP),
    };
    let files = handles
        .into_iter()
        .map(|(role, h)| (role, std::fs::File::from(h)))
        .collect();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(lpk_proto::worker::run(
        files,
        &args,
        &lpk_proto::fixture::open,
    ));
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    std::process::exit(EXIT_NO_BOOTSTRAP);
}
