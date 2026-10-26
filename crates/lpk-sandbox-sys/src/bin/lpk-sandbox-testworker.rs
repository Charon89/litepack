//! Test-only worker for `lpk-sandbox-sys`'s tests: takes its handles from the bootstrap (exit 3
//! when absent or malformed) and reports over its response pipe what it can and cannot do.
//!
//! Commands (after the bootstrap argument): `report`, `read <path>`, `spawn`, `alloc <MiB>`,
//! `spin <cpu-ms>`, `sleep <ms>`, `env`, `open <pid> <tid>` (Windows), `reg <hkcu|hklm> <path>`
//! (Windows), `crash-at-request <hex>` (E4-5: writes the hex-decoded bytes to the response pipe,
//! the broker's `Hello`, waits for the first request frame's header on the request pipe, then
//! dereferences null). Unsandboxed helper mode: `host <lpac|restricted> <out-file> [appcontainer-name]`
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
    let mut req: Option<File> = None;
    let mut keep = Vec::new();
    for (role, h) in handles {
        if role == roles::RESPONSES {
            resp = Some(File::from(h));
        } else if role == roles::REQUESTS {
            req = Some(File::from(h));
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
        Some("env") => {
            for (k, _) in std::env::vars_os() {
                // Names only, except the folders the AppContainer launch rewrites.
                let k = k.to_string_lossy().into_owned();
                match std::env::var(&k) {
                    Ok(v) if matches!(k.as_str(), "LOCALAPPDATA" | "TEMP" | "TMP") => {
                        let _ = writeln!(out, "env {k}={v}");
                    }
                    _ => {
                        let _ = writeln!(out, "env {k}");
                    }
                }
            }
        }
        #[cfg(windows)]
        Some("open") => {
            let id = |i: usize| cmd.get(i).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let pid = id(1);
            for access in PROCESS_PROBES {
                let r = lpk_sandbox_sys::probe_open_process(pid, access);
                let _ = writeln!(out, "openp {access:#x} {}", outcome(r));
            }
            // Every thread id given: the victim's initial thread and the ones it made itself.
            for tid in cmd.iter().skip(2).filter_map(|s| s.parse::<u32>().ok()) {
                for access in THREAD_PROBES {
                    let r = lpk_sandbox_sys::probe_open_thread(tid, access);
                    let _ = writeln!(out, "opent {tid} {access:#x} {}", outcome(r));
                }
            }
        }
        #[cfg(windows)]
        Some("threadsleep") => {
            // A thread of the worker's own making (created with the worker's default DACL).
            let ms: u64 = cmd.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let (tx, rx) = std::sync::mpsc::channel();
            let t = std::thread::spawn(move || {
                let _ = tx.send(child::current_thread_id());
                std::thread::sleep(Duration::from_millis(ms));
            });
            let tid = rx.recv().unwrap_or(0);
            let _ = writeln!(out, "thread {tid}");
            let _ = writeln!(out, "sleeping");
            let _ = out.flush();
            let _ = t.join();
        }
        #[cfg(windows)]
        Some("threads") => {
            // Several threads of the worker's own; with `raise`, one ends in an unhandled SEH
            // exception, which the job's die-on-unhandled-exception turns into the exit status.
            let n: u32 = cmd.get(1).and_then(|s| s.parse().ok()).unwrap_or(4);
            let raise = cmd.get(2).copied() == Some("raise");
            let workers: Vec<_> = (0..n)
                .map(|i| {
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(50));
                        if raise && i == 0 {
                            // SAFETY: raising a non-continuable, non-C++ exception with no
                            // arguments; Rust frames do not catch it, so it is unhandled.
                            unsafe {
                                windows::Win32::System::Diagnostics::Debug::RaiseException(
                                    TEST_EXCEPTION,
                                    1,
                                    None,
                                )
                            };
                        }
                        let mut x = 0u64;
                        for k in 0..1_000_000u64 {
                            x = x.wrapping_add(k * u64::from(i));
                        }
                        std::hint::black_box(x)
                    })
                })
                .collect();
            let ok = workers.into_iter().filter_map(|t| t.join().ok()).count();
            let _ = writeln!(out, "threads done {ok}");
        }
        #[cfg(windows)]
        Some("reg") => {
            let hklm = cmd.get(1).copied() == Some("hklm");
            let r = lpk_sandbox_sys::probe_registry_read(hklm, cmd.get(2).copied().unwrap_or(""));
            let _ = writeln!(out, "reg {}", outcome(r));
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
        Some("crash-at-request") => {
            let hello = cmd.get(1).and_then(|h| unhex(h)).unwrap_or_default();
            if out.write_all(&hello).and_then(|_| out.flush()).is_err() {
                std::process::exit(EXIT_NO_RESPONSES);
            }
            // The header of the first request frame (length and kind: 6 bytes), then crash.
            let mut header = [0u8; 6];
            let got = req
                .as_mut()
                .map(|r| std::io::Read::read_exact(r, &mut header).is_ok())
                .unwrap_or(false);
            if got {
                null_dereference();
            }
        }
        other => {
            let _ = writeln!(out, "unknown command {other:?}");
        }
    }
    drop(keep);
}

/// Bytes from hex digits; `None` when malformed.
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A read through a null pointer to a structure, at its field 16 bytes in: the classic null
/// dereference of a parser. (Address 16 rather than 0 keeps Rust's debug precondition check of
/// `read_volatile`, which aborts on a null pointer, out of the way; the first 64 KiB of the
/// address space are never mapped on Windows, nor the first page on Linux.)
fn null_dereference() -> ! {
    let field = std::ptr::without_provenance::<u64>(16);
    // SAFETY: deliberately unsound, in this test-only binary: the address lies in the null
    // page, which is never mapped, so the read faults (an access violation on Windows, ended by
    // the job's die-on-unhandled-exception; SIGSEGV on Linux) and never returns a value.
    let v = unsafe { std::ptr::read_volatile(field) };
    std::hint::black_box(v);
    std::process::abort()
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

/// The exception code `threads <n> raise` ends with.
#[cfg(windows)]
const TEST_EXCEPTION: u32 = 0xE000_1234;
/// PROCESS_VM_READ, PROCESS_VM_WRITE | PROCESS_VM_OPERATION, PROCESS_DUP_HANDLE,
/// PROCESS_TERMINATE, PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZE.
#[cfg(windows)]
const PROCESS_PROBES: [u32; 6] = [0x10, 0x28, 0x40, 0x1, 0x1000, 0x10_0000];
/// THREAD_GET_CONTEXT, THREAD_SET_CONTEXT, THREAD_SUSPEND_RESUME,
/// THREAD_QUERY_LIMITED_INFORMATION, SYNCHRONIZE.
#[cfg(windows)]
const THREAD_PROBES: [u32; 5] = [0x8, 0x10, 0x2, 0x800, 0x10_0000];

#[cfg(windows)]
fn outcome(r: std::io::Result<()>) -> String {
    match r {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("err {}", e.raw_os_error().unwrap_or(-1)),
    }
}

/// Every handle of the table taken when `main` began: value, type index, access, type name and,
/// for handles not passed by the broker, the object name (passed pipes are never queried).
#[cfg(windows)]
fn report(out: &mut File, passed: &[(u32, u64)]) {
    match EARLY.get().expect("snapshot taken in main") {
        Ok(hs) => {
            for h in hs {
                let ours = passed.iter().any(|p| p.1 == h.value);
                let ty = child::handle_type(h.value).unwrap_or_else(|| "?".into());
                let name = if ours {
                    None
                } else {
                    child::handle_path(h.value)
                };
                let _ = writeln!(
                    out,
                    "handle {} {} {:#x} {} {}",
                    h.value,
                    h.type_index,
                    h.access,
                    ty.replace(' ', "_"),
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
        let mut launch = Launch::new(&exe);
        if let Some(name) = args.get(2) {
            launch = launch.appcontainer_name(name);
        }
        let worker = launch
            .mode(mode)
            .args(["sleep", "5000"])
            .handle(roles::RESPONSES, w.as_handle(), Access::Write)
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
