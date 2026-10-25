//! Start a foreign-format worker in a sandbox (E4-1; design: `docs/notes/e4-design.md` §1).
//!
//! **Windows.** Two launch modes:
//! * [`Mode::Lpac`], the default: an AppContainer process with no capability and the
//!   ALL APPLICATION PACKAGES opt-out (a "less privileged AppContainer"). Its identity is the SID
//!   `DeriveAppContainerSidFromAppContainerName("LitePack.Worker")`; no profile is created.
//! * [`Mode::RestrictedToken`], an explicit opt-in (warn with [`RESTRICTED_WARNING`]): the caller's
//!   token with every privilege but `SeChangeNotifyPrivilege` removed, every group deny-only,
//!   restricting SIDs {Everyone, Users, RESTRICTED}, Low integrity. Its residual: whatever those
//!   SIDs may read (files, registry, named objects) and the network.
//!
//! In both modes the child is created suspended, `DETACHED_PROCESS` (no console, so no
//! `conhost.exe` joins the job), straight into a job ([`job::Job::sandbox`]: one process, memory and
//! per-process user-time limits, every UI restriction, kill-on-close, die-on-unhandled-exception),
//! with child-process creation blocked and the Win32k, extension-point, dynamic-code, remote/low
//! image-load and strict-handle mitigations on. Nothing is inherited: the handles the caller
//! passes are duplicated into the suspended child and their values written to a page committed
//! at the first free of four addresses named on its command line ([`bootstrap`]); the child takes
//! them with [`child::take_handles`] and refuses to start without a valid page.
//!
//! **Linux** (decision (d)): a plain child under `setrlimit` (`RLIMIT_AS`, `RLIMIT_CPU`,
//! `RLIMIT_CORE` 0, `RLIMIT_FSIZE` 0, `RLIMIT_NOFILE`) with `PR_SET_PDEATHSIG`, the passed fds
//! moved to 3, 4, ... and named on the command line. **It is not sandboxed**; CI only.

#![deny(clippy::undocumented_unsafe_blocks)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(not(any(windows, target_os = "linux")))]
compile_error!("lpk-sandbox-sys supports only Windows and Linux");

pub mod bootstrap;
pub mod child;
#[cfg(windows)]
pub mod job;
#[cfg(target_os = "linux")]
mod unix_impl;
#[cfg(windows)]
mod windows_impl;

#[cfg(target_os = "linux")]
pub use unix_impl::Worker;
#[cfg(windows)]
pub use windows_impl::{
    appcontainer_sid_string, create_appcontainer_profile, delete_appcontainer_profile,
    grant_path_access, wait_handle, Worker,
};

/// The warning shown on every use of the restricted-token mode.
pub const RESTRICTED_WARNING: &str = "warning: the restricted-token sandbox is weaker than the \
default: the worker can read whatever Everyone and Users may read and can reach the network";

/// The AppContainer name of the LPAC worker (its SID is derived from it; no profile).
pub const APPCONTAINER_NAME: &str = "LitePack.Worker";

/// How the worker is sandboxed (Windows; ignored on Linux, which does not sandbox).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// Less privileged AppContainer, no capability. The default.
    #[default]
    Lpac,
    /// Restricted token at Low integrity. Opt-in only; see [`RESTRICTED_WARNING`].
    RestrictedToken,
}

/// Resource limits of the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Committed memory (Windows: per process and for the job) / address space (Linux), bytes.
    pub memory: u64,
    /// User-mode CPU time before the worker is killed; raised with `Worker::set_cpu_limit`.
    pub cpu_time: Duration,
    /// The most `cpu_time` may be raised to (Linux: the hard `RLIMIT_CPU`).
    pub cpu_ceiling: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            memory: 1 << 30,
            cpu_time: Duration::from_secs(60),
            cpu_ceiling: Duration::from_secs(3600),
        }
    }
}

/// The access a passed handle gets in the worker (Windows; on Linux fds keep their open mode).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Access {
    /// The same access as the caller's handle.
    #[default]
    Same,
    /// `FILE_GENERIC_READ` only (an archive handle).
    ReadOnly,
}

/// Well-known roles of passed handles (the worker finds its handles by role).
pub mod roles {
    /// Requests, broker to worker (the worker's read end).
    pub const REQUESTS: u32 = 1;
    /// Responses, worker to broker (the worker's write end).
    pub const RESPONSES: u32 = 2;
    /// Diagnostics, worker to broker (the worker's write end).
    pub const STDERR: u32 = 3;
    /// The first archive handle; further volumes take the next roles.
    pub const ARCHIVE: u32 = 16;
}

/// A handle (Windows) or fd (Linux) the caller lends for duplication into the worker.
#[cfg(windows)]
pub type Borrowed<'a> = std::os::windows::io::BorrowedHandle<'a>;
/// A handle (Windows) or fd (Linux) the caller lends for duplication into the worker.
#[cfg(unix)]
pub type Borrowed<'a> = std::os::fd::BorrowedFd<'a>;

/// What to start and how.
#[derive(Debug)]
pub struct Launch<'a> {
    pub(crate) exe: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) mode: Mode,
    pub(crate) limits: Limits,
    pub(crate) appcontainer_name: String,
    pub(crate) current_dir: Option<PathBuf>,
    pub(crate) handles: Vec<(u32, Borrowed<'a>, Access)>,
}

impl<'a> Launch<'a> {
    /// Start `exe` (an absolute path) in the default mode with default limits and no handle.
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Launch {
            exe: exe.into(),
            args: Vec::new(),
            mode: Mode::default(),
            limits: Limits::default(),
            appcontainer_name: APPCONTAINER_NAME.to_string(),
            current_dir: None,
            handles: Vec::new(),
        }
    }
    /// Append arguments (after the bootstrap argument).
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
    /// Choose the mode.
    pub fn mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }
    /// Set the limits.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
    /// Use another AppContainer name (tests); the default is [`APPCONTAINER_NAME`].
    pub fn appcontainer_name(mut self, name: impl Into<String>) -> Self {
        self.appcontainer_name = name.into();
        self
    }
    /// The worker's current directory (default: the directory of `exe`).
    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }
    /// Lend a handle; the worker receives a duplicate under `role`.
    pub fn handle(mut self, role: u32, handle: Borrowed<'a>, access: Access) -> Self {
        self.handles.push((role, handle, access));
        self
    }
    /// Start the worker.
    pub fn spawn(&self) -> std::io::Result<Worker> {
        if self.handles.len() > bootstrap::MAX_HANDLES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "too many handles for the bootstrap block",
            ));
        }
        if self.limits.cpu_time > self.limits.cpu_ceiling {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cpu_time above cpu_ceiling",
            ));
        }
        Worker::spawn(self)
    }
}
