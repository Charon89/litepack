//! Run one external program to completion and measure it honestly.
//!
//! Reports wall-clock time, user and kernel CPU time summed over the program and every process it
//! spawned, peak memory, the exit code, and whether a timeout killed the process tree.
//!
//! * Windows: the child is created suspended, assigned to a Job Object (kill-on-close) and only
//!   then resumed, so nothing it starts can escape accounting. The wall clock starts right before
//!   the resume. `peak_rss` is the main process's `PeakWorkingSetSize` (descendants are not
//!   included); `peak_commit_process` / `peak_commit_job` are the job's `PeakProcessMemoryUsed`
//!   (largest single process) and `PeakJobMemoryUsed` (whole job at once), both *committed* memory.
//! * Linux: the child leads its own process group; the exit is awaited with a blocking `waitid`
//!   (a watchdog thread enforces the timeout) and the child is then reaped with `wait4`, whose
//!   rusage covers all waited-for descendants. `peak_rss` is `ru_maxrss`: the largest resident set
//!   of any single process in the tree (not a sum). The commit peaks are `None`.
//!
//! Only Windows and Linux are supported; other targets fail to compile.
//!
//! No shell is ever involved; arguments are passed as a list.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(all(unix, not(target_os = "linux")))]
compile_error!(
    "lpk-procstat-sys supports only Windows and Linux (ru_maxrss units differ elsewhere)"
);

#[cfg(target_os = "linux")]
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

/// Where the child's standard input comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Input {
    /// Nothing: reads see end of file at once.
    #[default]
    Null,
    /// Read this existing file from its start.
    File(PathBuf),
}

impl Input {
    pub(crate) fn to_stdio(&self) -> io::Result<Stdio> {
        Ok(match self {
            Input::Null => Stdio::null(),
            Input::File(p) => Stdio::from(File::open(p)?),
        })
    }
}

/// A program to run and how to run it.
#[derive(Debug, Clone)]
pub struct Spec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Variables to set. Applied after `env_clear` / `env_remove`.
    pub env: Vec<(OsString, OsString)>,
    /// Variables to remove from the inherited environment.
    pub env_remove: Vec<OsString>,
    /// Start from an empty environment instead of the caller's.
    pub env_clear: bool,
    pub stdin: Input,
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
            env_remove: Vec::new(),
            env_clear: false,
            stdin: Input::Null,
            stdout: Output::Inherit,
            stderr: Output::Inherit,
            timeout: None,
        }
    }
    pub fn stdin(mut self, input: Input) -> Self {
        self.stdin = input;
        self
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
    pub fn env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.env_remove.push(key.into());
        self
    }
    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
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
        if self.env_clear {
            c.env_clear();
        }
        for k in &self.env_remove {
            c.env_remove(k);
        }
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c.stdin(self.stdin.to_stdio()?);
        c.stdout(self.stdout.to_stdio()?);
        c.stderr(self.stderr.to_stdio()?);
        Ok(c)
    }
}

/// What one run measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    /// Wall-clock time. Windows: from resuming the suspended child (process creation excluded).
    /// Linux: from just before fork/exec (process creation included).
    pub wall: Duration,
    /// User-mode CPU time of the program and all its descendants.
    pub user_cpu: Duration,
    /// Kernel-mode CPU time of the program and all its descendants.
    pub kernel_cpu: Duration,
    /// Peak resident memory in bytes: the figure to publish. Linux: `ru_maxrss`, the largest
    /// resident set of any process in the tree. Windows: peak working set of the main process only.
    pub peak_rss: u64,
    /// Windows only: peak committed memory of the largest single process in the job, in bytes.
    pub peak_commit_process: Option<u64>,
    /// Windows only: peak committed memory of the whole job at one time, in bytes.
    pub peak_commit_job: Option<u64>,
    /// Exit code of the program; `None` if it was killed (always `None` when `timed_out`).
    pub exit_code: Option<i32>,
    /// The timeout expired and the process tree was killed.
    pub timed_out: bool,
    /// Descendants of the program were still running when it exited (or the timeout fired) and
    /// were killed; their CPU time may be incomplete (Windows) or missing (Linux). Decided at the
    /// moment the program exits, with no grace period. One known false positive: on Windows with
    /// an inherited stdout or stderr and a caller without a console, Windows gives the child a
    /// console host, which is then reported as a descendant. The same happens when the program itself
    /// starts console programs without detaching them (the first child runs without a console).
    pub descendants_killed: bool,
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
