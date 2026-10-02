//! Run one external program to completion and measure it honestly.
//!
//! Reports wall-clock time, user and kernel CPU time summed over the program and every process it
//! spawned, peak memory, the exit code, and whether a timeout killed the process tree.
//!
//! * Windows: the child is created suspended, assigned to a Job Object (kill-on-close) and only
//!   then resumed, so nothing it starts can escape accounting. Memory is the job's
//!   `PeakProcessMemoryUsed` (peak *committed* memory of the largest single process) and
//!   `PeakJobMemoryUsed` (peak committed memory of the whole job at once).
//! * Linux: the child leads its own process group and is reaped with `wait4`; its rusage covers all
//!   waited-for descendants. `ru_maxrss` is the largest resident set of any single process in the
//!   tree (not a sum). `peak_job_memory` is `None`.
//!
//! No shell is ever involved; arguments are passed as a list.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows_impl;

/// Where a standard stream of the child goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Output {
    /// Share the caller's stream.
    #[default]
    Inherit,
    /// Throw the output away.
    Discard,
    /// Create or truncate this file and write there.
    File(PathBuf),
}

impl Output {
    pub(crate) fn to_stdio(&self) -> io::Result<Stdio> {
        Ok(match self {
            Output::Inherit => Stdio::inherit(),
            Output::Discard => Stdio::null(),
            Output::File(p) => Stdio::from(File::create(p)?),
        })
    }
}

/// A program to run and how to run it.
#[derive(Debug, Clone)]
pub struct Spec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub stdout: Output,
    pub stderr: Output,
    /// When exceeded the whole process tree is killed and `timed_out` is set.
    pub timeout: Option<Duration>,
}

impl Spec {
    pub fn new(program: impl Into<OsString>) -> Self {
        Spec {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            stdout: Output::Inherit,
            stderr: Output::Inherit,
            timeout: None,
        }
    }
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
    pub fn cwd(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
    pub fn stdout(mut self, out: Output) -> Self {
        self.stdout = out;
        self
    }
    pub fn stderr(mut self, out: Output) -> Self {
        self.stderr = out;
        self
    }
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }

    pub(crate) fn command(&self) -> io::Result<std::process::Command> {
        let mut c = std::process::Command::new(&self.program);
        c.args(&self.args);
        if let Some(d) = &self.cwd {
            c.current_dir(d);
        }
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c.stdin(Stdio::null());
        c.stdout(self.stdout.to_stdio()?);
        c.stderr(self.stderr.to_stdio()?);
        Ok(c)
    }
}

/// What one run measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    pub wall: Duration,
    /// User-mode CPU time of the program and all its descendants.
    pub user_cpu: Duration,
    /// Kernel-mode CPU time of the program and all its descendants.
    pub kernel_cpu: Duration,
    /// Peak memory of the largest single process, in bytes (Windows: committed; Linux: resident).
    pub peak_process_memory: u64,
    /// Windows only: peak committed memory of the whole job at one time, in bytes.
    pub peak_job_memory: Option<u64>,
    /// Exit code of the program; `None` if it was killed (timeout or signal).
    pub exit_code: Option<i32>,
    /// The timeout expired and the process tree was killed.
    pub timed_out: bool,
}

impl Measurement {
    pub fn total_cpu(&self) -> Duration {
        self.user_cpu + self.kernel_cpu
    }
}

/// Run `spec` to completion (or timeout). A program that cannot be started is an `Err`.
pub fn run(spec: &Spec) -> io::Result<Measurement> {
    #[cfg(windows)]
    {
        windows_impl::run(spec)
    }
    #[cfg(unix)]
    {
        unix::run(spec)
    }
}
