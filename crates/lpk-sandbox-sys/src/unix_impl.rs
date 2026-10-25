//! Linux launch (decision (d)): a plain child under `setrlimit`. **Not a sandbox**: the worker can
//! read whatever the user can and reach the network. Used by CI only.

use crate::bootstrap;
use crate::Launch;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

/// Open files the worker may hold beyond its passed fds.
const NOFILE_SLACK: u64 = 32;

fn secs_up(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

/// A running worker. Dropping it kills the worker (as closing the job does on Windows).
#[derive(Debug)]
pub struct Worker {
    child: Mutex<Child>,
    pid: u32,
    cpu_ceiling: Duration,
    keeper: Option<(mpsc::Sender<()>, std::thread::JoinHandle<()>)>,
}

impl Worker {
    pub(crate) fn spawn(l: &Launch<'_>) -> io::Result<Worker> {
        let n = l.handles.len();
        let mut srcs = [-1 as RawFd; bootstrap::MAX_HANDLES];
        let mut arg = String::from(bootstrap::FDS_PREFIX);
        for (i, (role, fd, _)) in l.handles.iter().enumerate() {
            srcs[i] = fd.as_raw_fd();
            if i > 0 {
                arg.push(',');
            }
            arg.push_str(&format!("{role}:{}", 3 + i));
        }
        let mut cmd = Command::new(&l.exe);
        cmd.arg(arg)
            .args(&l.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let cwd = l
            .current_dir
            .clone()
            .or_else(|| l.exe.parent().map(|p| p.to_path_buf()));
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        let mem = l.limits.memory;
        let cpu_soft = secs_up(l.limits.cpu_time);
        let cpu_hard = secs_up(l.limits.cpu_ceiling);
        let nofile = 3 + n as u64 + NOFILE_SLACK;
        // SAFETY: the closure runs between fork and exec and calls only async-signal-safe
        // functions (prctl, fcntl, dup2, setrlimit) on stack data; it allocates nothing.
        unsafe {
            cmd.pre_exec(move || {
                let check = |r: libc::c_int| {
                    if r < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                };
                check(libc::prctl(
                    libc::PR_SET_PDEATHSIG,
                    libc::SIGKILL as libc::c_ulong,
                ))?;
                // Move every passed fd above the targets first, then onto 3, 4, ...
                let mut tmp = [-1 as RawFd; bootstrap::MAX_HANDLES];
                for i in 0..n {
                    tmp[i] = libc::fcntl(srcs[i], libc::F_DUPFD_CLOEXEC, (3 + n) as libc::c_int);
                    check(tmp[i])?;
                }
                for (i, &t) in tmp.iter().enumerate().take(n) {
                    check(libc::dup2(t, (3 + i) as libc::c_int))?;
                }
                let set = |res, soft: u64, hard: u64| {
                    let r = libc::rlimit {
                        rlim_cur: soft as libc::rlim_t,
                        rlim_max: hard as libc::rlim_t,
                    };
                    check(libc::setrlimit(res, &r))
                };
                set(libc::RLIMIT_AS, mem, mem)?;
                set(libc::RLIMIT_CPU, cpu_soft, cpu_hard)?;
                set(libc::RLIMIT_CORE, 0, 0)?;
                set(libc::RLIMIT_FSIZE, 0, 0)?;
                set(libc::RLIMIT_NOFILE, nofile, nofile)?;
                Ok(())
            });
        }
        // PR_SET_PDEATHSIG fires when the *thread* that forked the worker exits, not the
        // process. So the worker is forked from a keeper thread that lives exactly as long as
        // the `Worker`: a caller spawning from a short-lived thread does not kill its worker,
        // and the worker still dies if the whole host dies.
        let (child_tx, child_rx) = mpsc::channel::<io::Result<Child>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let keeper = std::thread::Builder::new()
            .name("lpk-worker-keeper".into())
            .spawn(move || {
                let r = cmd.spawn();
                let ok = r.is_ok();
                let _ = child_tx.send(r);
                if ok {
                    // Returns when the Worker drops its sender.
                    let _ = stop_rx.recv();
                }
            })?;
        let child = child_rx
            .recv()
            .map_err(|_| io::Error::other("worker keeper thread died"))??;
        Ok(Worker {
            pid: child.id(),
            child: Mutex::new(child),
            cpu_ceiling: l.limits.cpu_ceiling,
            keeper: Some((stop_tx, keeper)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Child> {
        self.child.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The worker's process id.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Wait for the worker to exit; `None` timeout waits forever. Returns its exit code (128 +
    /// signal number when a signal ended it), or `None` when the timeout passed first.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<Option<u32>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if let Some(st) = self.lock().try_wait()? {
                let code = match (st.code(), st.signal()) {
                    (Some(c), _) => c as u32,
                    (None, Some(s)) => 128 + s as u32,
                    _ => u32::MAX,
                };
                return Ok(Some(code));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kill the worker.
    pub fn kill(&self) -> io::Result<()> {
        self.lock().kill()
    }

    /// Raise (or lower) the soft `RLIMIT_CPU` of the running worker; at most the ceiling.
    pub fn set_cpu_limit(&self, cpu: Duration) -> io::Result<()> {
        if cpu > self.cpu_ceiling {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cpu limit above the ceiling",
            ));
        }
        let r = libc::rlimit {
            rlim_cur: secs_up(cpu) as libc::rlim_t,
            rlim_max: secs_up(self.cpu_ceiling) as libc::rlim_t,
        };
        // SAFETY: `r` is a valid rlimit; a null old-limit pointer is allowed.
        let rc = unsafe {
            libc::prlimit(
                self.pid as libc::pid_t,
                libc::RLIMIT_CPU,
                &r,
                std::ptr::null_mut(),
            )
        };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        {
            let mut c = self.lock();
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some((stop, keeper)) = self.keeper.take() {
            drop(stop);
            let _ = keeper.join();
        }
    }
}
