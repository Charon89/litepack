//! Test-only worker of the main workspace (E4-2): the worker loop of `lpk-proto` over the test
//! fixture reader, built with the `faults` feature so the host's tests can make it send
//! garbage, hang, close its pipe or page forever (`--fault=<name>`). The host's tests find it
//! beside their own executables (`target/<profile>/`). Never shipped: the shipped worker is
//! `workers/lpk-worker`, built without faults in its own workspace.
//!
//! **Crash containment (E4-5).** `--crash=<case>` makes the "parser" (the opener, run at
//! `Open` after it has read the archive) misbehave the way a hostile or broken one would:
//! * `abort`: `std::process::abort()`;
//! * `alloc`: commit memory in 1 MiB pieces, touched, until the job's commit limit refuses one
//!   (Rust's allocation failure then aborts the process);
//! * `spin`: burn user-mode CPU forever;
//! * `spawn`: start `cmd.exe` (`true` elsewhere);
//! * `network:<port>`: connect by TCP to `127.0.0.1:<port>` (the test host's listener) and to
//!   `192.0.2.1:9` (TEST-NET-1, never routed: RFC 5737), two seconds each;
//! * `read:<path>`: read a file outside the worker's allowance.
//!
//! What the sandbox refuses (spawn, network, read) is reported as an `Error` frame with code
//! `Denied` and a message starting with the resource (`process:`, `network:`, `file:`); what
//! it allows is reported as `Corrupt` ("... was allowed"), except the network under the
//! restricted-token opt-in, whose reach is that mode's documented residual: the worker then
//! serves the archive as if nothing happened (a hostile parser would stay quiet), and the test
//! host observes the connection on its listener. The null dereference needs `unsafe` and lives
//! in `lpk-sandbox-sys`'s test worker (`crash-at-request`).
#![forbid(unsafe_code)]

/// Exit status when the bootstrap is missing or malformed.
const EXIT_NO_BOOTSTRAP: i32 = 3;
/// Exit status when a `--crash=` argument names no case.
const EXIT_BAD_CRASH: i32 = 5;

/// The argument prefix that selects a crash case.
const CRASH_ARG: &str = "--crash=";

/// A crash-containment case (E4-5).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Crash {
    Abort,
    Alloc,
    Spin,
    Spawn,
    Network(u16),
    Read(String),
}

impl Crash {
    fn parse(s: &str) -> Option<Crash> {
        Some(match s {
            "abort" => Crash::Abort,
            "alloc" => Crash::Alloc,
            "spin" => Crash::Spin,
            "spawn" => Crash::Spawn,
            _ => match s.split_once(':')? {
                ("network", port) => Crash::Network(port.parse().ok()?),
                ("read", path) => Crash::Read(path.to_string()),
                _ => return None,
            },
        })
    }

    /// Misbehave. `Ok(())` lets the parser go on (only the network reach of the opt-in mode).
    fn run(&self) -> Result<(), lpk_proto::ReaderError> {
        use lpk_proto::{ErrorCode, ReaderError};
        let denied = |m: String| Err(ReaderError::new(ErrorCode::Denied, m));
        let allowed = |m: String| Err(ReaderError::corrupt(format!("{m} was allowed")));
        match self {
            Crash::Abort => std::process::abort(),
            Crash::Alloc => {
                let mut held: Vec<Vec<u8>> = Vec::new();
                loop {
                    // An infallible allocation: the job's refusal ends in Rust's abort.
                    held.push(vec![1u8; 1 << 20]);
                    std::hint::black_box(&held);
                }
            }
            Crash::Spin => {
                let mut x = 0u64;
                loop {
                    for i in 0..1_000_000u64 {
                        x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
                    }
                    std::hint::black_box(x);
                }
            }
            Crash::Spawn => {
                let r = if cfg!(windows) {
                    std::process::Command::new("cmd.exe")
                        .args(["/c", "exit", "0"])
                        .status()
                } else {
                    std::process::Command::new("true").status()
                };
                match r {
                    Ok(s) => allowed(format!("process: spawn ({s})")),
                    Err(e) => denied(format!("process: spawn refused: {}", os_error(&e))),
                }
            }
            Crash::Network(port) => {
                let port = *port;
                // Under LPAC Winsock itself cannot start (`WSAStartup` answers
                // WSASYSCALLFAILURE, 10107, measured on Windows 11 26200), and std panics on
                // that: caught here and reported as the refusal it is.
                let attempt = std::panic::catch_unwind(move || {
                    let t = std::time::Duration::from_secs(2);
                    let mut errors = Vec::new();
                    for addr in [
                        std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                        std::net::SocketAddr::from(([192, 0, 2, 1], 9)),
                    ] {
                        match std::net::TcpStream::connect_timeout(&addr, t) {
                            Ok(s) => {
                                // Reached: the opt-in mode's residual. Say hello, go on quietly.
                                use std::io::Write;
                                let mut s = s;
                                let _ = s.write_all(b"lpk-test-worker");
                                return Ok(());
                            }
                            Err(e) => errors.push(format!("{addr} {}", os_error(&e))),
                        }
                    }
                    Err(errors.join("; "))
                });
                match attempt {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(errors)) => denied(format!("network: connect refused: {errors}")),
                    Err(p) => {
                        let text = p
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_default();
                        denied(format!("network: Winsock could not start: {text}"))
                    }
                }
            }
            Crash::Read(path) => match std::fs::read(path) {
                Ok(b) => allowed(format!("file: read of {} bytes", b.len())),
                Err(e) => denied(format!("file: read refused: {}", os_error(&e))),
            },
        }
    }
}

/// An I/O error as its kind and OS code (stable across locales, unlike the text).
fn os_error(e: &std::io::Error) -> String {
    format!(
        "{:?} (os error {})",
        e.kind(),
        e.raw_os_error().unwrap_or(-1)
    )
}

/// The `--crash=` case among `args` (and the arguments left for the worker loop).
fn split_args(args: Vec<String>) -> Result<(Option<Crash>, Vec<String>), String> {
    let mut crash = None;
    let mut rest = Vec::new();
    for a in args {
        if let Some(name) = a.strip_prefix(CRASH_ARG) {
            crash = Some(Crash::parse(name).ok_or_else(|| format!("unknown crash case {name}"))?);
        } else {
            rest.push(a);
        }
    }
    Ok((crash, rest))
}

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
    let (crash, args) = match split_args(std::env::args().skip(1).collect()) {
        Ok(v) => v,
        Err(_) => std::process::exit(EXIT_BAD_CRASH),
    };
    let opener = move |file: std::fs::File, args: &lpk_proto::reader::OpenArgs<'_>| {
        let reader = lpk_proto::fixture::open(file, args)?;
        if let Some(c) = &crash {
            c.run()?;
        }
        Ok(reader)
    };
    std::process::exit(lpk_proto::worker::run(files, &args, &opener));
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    let _ = split_args(Vec::new());
    std::process::exit(EXIT_NO_BOOTSTRAP);
}
