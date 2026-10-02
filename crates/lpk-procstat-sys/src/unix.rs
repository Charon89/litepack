//! Linux/Unix backend: `wait4` rusage, process group kill on timeout.

use crate::{Measurement, Spec};
use std::io;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

fn tv(t: libc::timeval) -> Duration {
    Duration::new(t.tv_sec.max(0) as u64, (t.tv_usec.max(0) as u32) * 1000)
}

pub(crate) fn run(spec: &Spec) -> io::Result<Measurement> {
    let mut cmd = spec.command()?;
    // Own process group so a timeout can kill the whole tree with one signal.
    cmd.process_group(0);
    let start = Instant::now();
    let child = cmd.spawn()?;
    let pid = child.id() as libc::pid_t;
    // `child` is never waited on through std; we reap it ourselves with wait4 below.
    let mut timed_out = false;
    let status;
    let rusage;
    loop {
        let mut st: libc::c_int = 0;
        // SAFETY: an all-zero `rusage` is a valid value (plain integer fields).
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        let flags = if spec.timeout.is_some() {
            libc::WNOHANG
        } else {
            0
        };
        // SAFETY: `st` and `ru` are valid for writes for the duration of the call; `pid` is our
        // own un-reaped child.
        let r = unsafe { libc::wait4(pid, &mut st, flags, &mut ru) };
        if r == pid {
            status = st;
            rusage = ru;
            break;
        }
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        // r == 0: still running (only with WNOHANG).
        if let Some(t) = spec.timeout {
            if !timed_out && start.elapsed() >= t {
                timed_out = true;
                // SAFETY: signalling the process group we created; a negative pid targets it.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let wall = start.elapsed();
    let exit_code = if libc::WIFEXITED(status) {
        Some(libc::WEXITSTATUS(status))
    } else {
        None
    };
    if !timed_out {
        // Best effort: nothing from our group should outlive a normal run.
        // SAFETY: signalling our own process group; ESRCH (already empty) is fine.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    Ok(Measurement {
        wall,
        user_cpu: tv(rusage.ru_utime),
        kernel_cpu: tv(rusage.ru_stime),
        // Linux reports ru_maxrss in KiB.
        peak_process_memory: (rusage.ru_maxrss.max(0) as u64) * 1024,
        peak_job_memory: None,
        exit_code,
        timed_out,
    })
}
