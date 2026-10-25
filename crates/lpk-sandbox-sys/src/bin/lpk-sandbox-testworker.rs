//! Test-only worker for `lpk-sandbox-sys`'s tests: takes its handles from the bootstrap (exit 3
//! when absent or malformed) and reports over its response pipe what it can and cannot do.
//!
//! Commands (after the bootstrap argument): `report`, `read <path>`, `spawn`, `alloc <MiB>`,
//! `spin <cpu-ms>`, `sleep <ms>`. Unsandboxed helper mode: `host <lpac|restricted> <out-file>`
//! launches a sandboxed copy of itself the way a console-less host would and writes the number of
//! processes in its job to `out-file` (Windows only).

use lpk_sandbox_sys::{bootstrap, child, roles};
use std::fs::File;
use std::io::Write;
use std::time::Duration;

/// Exit status when the bootstrap is missing or malformed.
const EXIT_NO_BOOTSTRAP: i32 = 3;
/// Exit status when no response handle was passed.
const EXIT_NO_RESPONSES: i32 = 4;

/// The handle table as it was when `main` began (Windows), before the worker opened anything.
#[cfg(windows)]
static EARLY: std::sync::OnceLock<std::io::Result<Vec<child::HandleEntry>>> =
    std::sync::OnceLock::new();

fn main() {
    #[cfg(windows)]
    let _ = EARLY.set(child::own_handles());
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("host") {
        std::process::exit(host(&args[2..]));
    }
    let handles = match child::take_handles() {
        Ok(h) => h,
        Err(_) => std::process::exit(EXIT_NO_BOOTSTRAP),
    };
    let passed: Vec<(u32, u64)> = handles.iter().map(|(r, h)| (*r, raw_value(h))).collect();
    let mut resp: Option<File> = None;
    let mut keep = Vec::new();
    for (role, h) in handles {
        if role == roles::RESPONSES {
            resp = Some(File::from(h));
        } else {
            keep.push(h);
        }
    }
    let Some(mut resp) = resp else {
        std::process::exit(EXIT_NO_RESPONSES);
    };
    let cmd: Vec<&str> = args[1..]
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with(bootstrap::ARG_PREFIX) && !a.starts_with(bootstrap::FDS_PREFIX))
        .collect();
    let out = &mut resp;
    match cmd.first().copied() {
        Some("report") => {
            for (role, v) in &passed {
                let _ = writeln!(out, "passed {role}={v}");
            }
            report(out, &passed);
        }
        Some("read") => {
            let path = cmd.get(1).copied().unwrap_or("");
            match std::fs::read(path) {
                Ok(b) => {
                    let _ = writeln!(out, "read ok {}", b.len());
                }
                Err(e) => {
                    let _ = writeln!(out, "read err {}", e.raw_os_error().unwrap_or(-1));
                }
            }
        }
        Some("spawn") => {
            let r = if cfg!(windows) {
                std::process::Command::new("cmd.exe")
                    .args(["/c", "exit", "0"])
                    .status()
            } else {
                std::process::Command::new("true").status()
            };
            match r {
                Ok(s) => {
                    let _ = writeln!(out, "spawn ok {s}");
                }
                Err(e) => {
                    let _ = writeln!(out, "spawn err {}", e.raw_os_error().unwrap_or(-1));
                }
            }
        }
        Some("alloc") => {
            let mib: usize = cmd.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let _ = writeln!(out, "allocating {mib}");
            let _ = out.flush();
            let mut v: Vec<u8> = Vec::new();
            v.resize(mib << 20, 1);
            let _ = writeln!(
                out,
                "alloc ok {}",
                v.iter().step_by(4096).map(|&b| b as usize).sum::<usize>()
            );
        }
        Some("spin") => {
            let ms: u64 = cmd.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let _ = writeln!(out, "spinning");
            let _ = out.flush();
            let target = Duration::from_millis(ms);
            let mut x = 0u64;
            while child::own_user_cpu() < target {
                for i in 0..100_000u64 {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
                }
                std::hint::black_box(x);
            }
            let _ = writeln!(out, "spin done");
        }
        Some("sleep") => {
            let ms: u64 = cmd.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let _ = writeln!(out, "sleeping");
            let _ = out.flush();
            std::thread::sleep(Duration::from_millis(ms));
            let _ = writeln!(out, "woke");
        }
        other => {
            let _ = writeln!(out, "unknown command {other:?}");
        }
    }
    drop(keep);
}

#[cfg(windows)]
fn raw_value(h: &child::Owned) -> u64 {
    use std::os::windows::io::AsRawHandle;
    h.as_raw_handle() as usize as u64
}

#[cfg(unix)]
fn raw_value(h: &child::Owned) -> u64 {
    use std::os::fd::AsRawFd;
    h.as_raw_fd() as u64
}

#[cfg(windows)]
fn report(out: &mut File, passed: &[(u32, u64)]) {
    match child::token_report() {
        Ok(t) => {
            let _ = writeln!(
                out,
                "token appcontainer={} lpac={} integrity={:#x} restricted={} restricted_sids={} privileges={} package={}",
                t.appcontainer as u8,
                t.lpac as u8,
                t.integrity,
                t.restricted as u8,
                t.restricted_sids,
                t.privileges,
                t.package_sid.as_deref().unwrap_or("-"),
            );
        }
        Err(e) => {
            let _ = writeln!(out, "token err {e}");
        }
    }
    match EARLY.get().expect("snapshot taken in main") {
        Ok(hs) => {
            for h in hs {
                // Pipes have no path; only ask for the others (passed pipes are never queried).
                let name = if passed.iter().any(|p| p.1 == h.value) {
                    None
                } else {
                    child::handle_path(h.value)
                };
                let _ = writeln!(
                    out,
                    "handle {} {} {:#x} {}",
                    h.value,
                    h.type_index,
                    h.access,
                    name.as_deref().unwrap_or("-")
                );
            }
        }
        Err(e) => {
            let _ = writeln!(out, "handles err {e}");
        }
    }
}

#[cfg(unix)]
fn report(out: &mut File, _passed: &[(u32, u64)]) {
    let limits = std::fs::read_to_string("/proc/self/limits").unwrap_or_default();
    for l in limits.lines() {
        let _ = writeln!(out, "limit {l}");
    }
    let mut fds: Vec<String> = std::fs::read_dir("/proc/self/fd")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    fds.sort();
    let _ = writeln!(out, "fds {}", fds.join(","));
}

#[cfg(windows)]
fn host(args: &[String]) -> i32 {
    use lpk_sandbox_sys::{Access, Launch, Mode};
    use std::io::{BufRead, BufReader};
    use std::os::windows::io::AsHandle;
    let mode = match args.first().map(String::as_str) {
        Some("lpac") => Mode::Lpac,
        Some("restricted") => Mode::RestrictedToken,
        _ => return 2,
    };
    let Some(out_path) = args.get(1) else {
        return 2;
    };
    let run = || -> std::io::Result<String> {
        let exe = std::env::current_exe()?;
        let (r, w) = std::io::pipe()?;
        let worker = Launch::new(&exe)
            .mode(mode)
            .args(["sleep", "5000"])
            .handle(roles::RESPONSES, w.as_handle(), Access::Same)
            .spawn()?;
        drop(w);
        let mut line = String::new();
        BufReader::new(r).read_line(&mut line)?;
        let ids = worker.job().process_ids()?;
        worker.kill()?;
        Ok(format!("first={} jobcount={}\n", line.trim(), ids.len()))
    };
    let text = run().unwrap_or_else(|e| format!("error {e}\n"));
    match std::fs::write(out_path, text) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

#[cfg(unix)]
fn host(_args: &[String]) -> i32 {
    2
}
