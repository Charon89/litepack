//! Linux backend: blocking `waitid` (no polling), watchdog thread for the timeout, `wait4` rusage.
//!
//! The leader is waited for with `WNOWAIT`, so it stays an unreaped zombie while the process group
//! is signalled; its pid (the group id) therefore cannot be reused before the final kill.

use crate::{Measurement, Spec};
use std::io;
use std::os::unix::process::CommandExt;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// `ru_maxrss` is in KiB on Linux.
const MAXRSS_UNIT: u64 = 1024;

fn tv(t: libc::timeval) -> Duration {
    Duration::new(t.tv_sec.max(0) as u64, (t.tv_usec.max(0) as u32) * 1000)
}

/// True if a live process other than the leader is still in process group `pgid`.
fn group_has_descendants(pgid: libc::pid_t) -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<libc::pid_t>().ok()) else {
            continue;
        };
        if pid == pgid {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // Format: `pid (comm) state ppid pgrp ...`; comm may contain spaces and parentheses.
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let mut f = stat[close + 1..].split_whitespace();
        let state = f.next();
        let _ppid = f.next();
        let pgrp = f.next().and_then(|v| v.parse::<libc::pid_t>().ok());
        if pgrp == Some(pgid) && state != Some("Z") {
            return true;
        }
    }
    false
}

fn kill_group(pgid: libc::pid_t) {
    // SAFETY: plain syscall; a negative pid targets the process group we created. The leader is
    // still un-reaped (zombie or alive) at every call site, so the group id cannot have been reused.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

/// Reap `pid`, retrying on EINTR, returning its wait status and rusage.
fn reap(pid: libc::pid_t) -> io::Result<(libc::c_int, libc::rusage)> {
    loop {
        let mut st: libc::c_int = 0;
        // SAFETY: an all-zero `rusage` is a valid value (plain integer fields).
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `st` and `ru` are valid for writes for the duration of the call; `pid` is our
        // own un-reaped child.
        let r = unsafe { libc::wait4(pid, &mut st, 0, &mut ru) };
        if r == pid {
            return Ok((st, ru));
        }
        let e = io::Error::last_os_error();
        if r < 0 && e.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(e);
    }
}

pub(crate) fn run(spec: &Spec) -> io::Result<Measurement> {
    let mut cmd = spec.command()?;
    // Own process group so one signal reaches the whole tree.
    cmd.process_group(0);
    let start = Instant::now();
    let child = cmd.spawn()?;
    let pid = child.id() as libc::pid_t;
    // `child` is never waited on through std; we reap it ourselves below.

    // The watchdog returns Some(had_descendants) if it fired and killed the group.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let watchdog = spec.timeout.map(|t| {
        std::thread::spawn(move || match stop_rx.recv_timeout(t) {
            Err(RecvTimeoutError::Timeout) => {
                let d = group_has_descendants(pid);
                kill_group(pid);
                Some(d)
            }
            _ => None,
        })
    });
    let stop_watchdog = |w: Option<std::thread::JoinHandle<Option<bool>>>| {
        let _ = stop_tx.send(());
        w.and_then(|h| h.join().ok().flatten())
    };

    loop {
        // SAFETY: an all-zero `siginfo_t` is a valid value to pass as an out-parameter.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is valid for writes; WNOWAIT leaves the child to be reaped by `wait4`.
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if r == 0 {
            break;
        }
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        stop_watchdog(watchdog);
        kill_group(pid);
        let _ = reap(pid);
        return Err(e);
    }
    let wall = start.elapsed();

    let fired = stop_watchdog(watchdog);
    let timed_out = fired.is_some();
    let descendants_killed = fired.unwrap_or_else(|| group_has_descendants(pid));
    // Leader is still an unreaped zombie here, so the group id is still ours.
    kill_group(pid);
    let (status, rusage) = reap(pid)?;

    let exit_code = if !timed_out && libc::WIFEXITED(status) {
        Some(libc::WEXITSTATUS(status))
    } else {
        None
    };
    Ok(Measurement {
        wall,
        user_cpu: tv(rusage.ru_utime),
        kernel_cpu: tv(rusage.ru_stime),
        peak_rss: (rusage.ru_maxrss.max(0) as u64) * MAXRSS_UNIT,
        peak_commit_process: None,
        peak_commit_job: None,
        exit_code,
        timed_out,
        descendants_killed,
    })
}
