//! The broker: foreign archives read by a sandboxed worker (E4-2; design
//! `docs/notes/e4-design.md` §2, D-97 for the launch).
//!
//! **One worker per archive.** [`extract_file`] and [`list_file`] start `lpk-worker` from the
//! host's own directory (or [`Options::worker`]) through `lpk-sandbox-sys` — an LPAC
//! AppContainer by default, the restricted token only when asked — with the archive as a
//! read-only handle and three pipes (requests, responses, diagnostics); the worker never sees
//! the destination. The conversation is `lpk-proto`'s: `Hello` (within [`HELLO_TIMEOUT`],
//! equal to the host's protocol and build), `Open` (the password only; `NeedPassword` is
//! [`FormatError::PasswordRequired`], a wrong one [`FormatError::WrongKey`]), `List` pages,
//! `ReadMember` streamed, `Cancel`, `Close`. The broker owns every file write.
//!
//! **Limits, never from the worker's reports.** Progress is what the host observes: the bytes
//! the job read (credited up to the archive's length) plus the `Data` bytes received (each
//! member's at most its declared size). Without progress for [`Limits::no_progress`] the worker
//! is killed ([`WorkerError::Timeout`]). Its user-time limit starts at [`CPU_STEP`] and is
//! raised at each progress to the user time then plus [`CPU_STEP`], at most
//! [`Limits::max_cpu`] ([`WorkerError::CpuLimit`]); its memory is the extraction budget (D-67,
//! `--memory`). Entries are at most the archive's length over the format's smallest header
//! and at most [`Limits::max_entries`]; the listing held by the host is charged
//! [`entry_charge`] per entry (the measured structures, the name held [`NAME_COPIES`] times)
//! against the same budget; a page
//! saying `more` past a cap is refused. Bytes written are at most [`Limits::max_expansion`]
//! times the archive's length, refused from the declared total before any write; free space
//! is checked against the declared total before writing and every [`FREE_SPACE_STEP`].
//!
//! **Before any write**, every record is checked (a UTF-8 name, a packed range inside the
//! archive, no overlap between blocks, no duplicate name, a hard link to a file that is
//! extracted) and becomes an [`Entry`] that passes E3-2's policy pass unchanged (shared with
//! `.lpk` extraction: [`crate::extract`]), then the format's path rule
//! ([`lpk_format::validate_path`]). Files are created create-new as their data arrives,
//! never pre-sized; a member whose bytes differ from its declared size is
//! [`ForeignError::SizeMismatch`] (the excess is never written); on any failure or
//! cancellation the file being written is removed (D-54: finished files and directories stay).
//! Hard links become copies of the extracted member they name.
//!
//! **Cancellation** (E3-3): the token is polled between frames; a cancelled run sends `Cancel`,
//! waits up to [`Limits::cancel_grace`] for `Cancelled`, kills the worker, cleans up and
//! returns [`CoreError::Cancelled`].
//!
//! **Diagnostics.** The worker's diagnostics pipe is drained by a thread; the first and last
//! [`STDERR_KEEP`] bytes are kept and shown escaped in [`WorkerError::Crashed`] and
//! [`WorkerError::Pipe`], never parsed. Every string from the worker is escaped before it is
//! shown. No worker output can panic the host (the `worker_proto` fuzz target).
//!
//! **Formats.** E4-2 has no real reader: detection recognises only the test fixture of
//! `lpk_proto::fixture` ([`Format::TestFixture`]); E4-3 adds ZIP, 7z and tar.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lpk_format::{Credentials, Entry, EntryFlags, EntryKind, FormatError};
use lpk_proto::frame::{FLAG_ENCRYPTED, NO_BLOCK};
use lpk_proto::{ErrorCode, ProtoError, Record, RecordKind, Request, Response};

use crate::error::{ForeignError, WorkerError};
use crate::extract::{make_dirs, make_links, mtime, policy_pass, Placed};
use crate::policy::{self, ExtractPolicy};
use crate::progress::{Monitor, Reporter, Stage};
use crate::select::Selection;
use crate::CoreError;

/// The worker's first frame must arrive within this.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// The default time without observed progress before the worker is killed.
pub const NO_PROGRESS: Duration = Duration::from_secs(60);
/// After `Cancel`, how long the worker has to answer `Cancelled` before it is killed.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);
/// The worker's user time may run this far past its user time at the last observed progress.
pub const CPU_STEP: Duration = Duration::from_secs(60);
/// The default ceiling of the worker's user time (`--max-cpu`).
pub const DEFAULT_MAX_CPU: Duration = Duration::from_secs(3600);
/// The ceiling `--max-cpu off` stands for (30 days).
pub const CPU_OFF: Duration = Duration::from_secs(30 * 24 * 3600);
/// The default most entries (`--max-entries`).
pub const DEFAULT_MAX_ENTRIES: u64 = 10_000_000;
/// The default expansion limit: bytes written at most this times the archive's length
/// (`--max-expansion`; the zlib format tops out at 1032:1).
pub const DEFAULT_MAX_EXPANSION: u64 = 10_000;
/// Free space is checked again after each this many bytes written.
pub const FREE_SPACE_STEP: u64 = 64 << 20;
/// The bytes of the worker's diagnostics kept at the start and at the end.
pub const STDERR_KEEP: usize = 32 << 10;
/// The records asked for per `List` page.
pub const PAGE: u32 = 4096;
/// What one listed entry is charged against the memory budget beyond its name and target
/// bytes: every structure that holds an entry during an extraction at once — the record, the
/// `Entry` (twice: the checked list and the selection's), the policy pass's placement and
/// the file target, the record position, declared size, hard-link target and encryption mark,
/// the hash tables by name (duplicates, records, conflicting names, case folding, hard
/// links: each slot counted twice for the tables' load factor), plus 16 bytes of allocator
/// overhead for each of the entry's eight heap blocks. Measured against a counting allocator
/// (`tests/foreign_charge.rs`).
pub const ENTRY_CHARGE: u64 = (std::mem::size_of::<Record>()
    + 2 * std::mem::size_of::<Entry>()
    + std::mem::size_of::<crate::extract::Placed>()
    + 3 * 32 // a file target: two strings and its sizes
    + 8 + 16 + 24 + 1 // position, declared size, hard-link target, encryption mark
    + 2 * (16 + 16 + 56 + 24 + 24 + 24) // the tables' slots
    + 8 * 16) as u64;
/// How many times an entry's name (and link target) is held at once during an extraction:
/// the entry, the table by name, the placement's path, the target's name and path, the case
/// folding's two tables, and one transient copy.
pub const NAME_COPIES: u64 = 8;

/// The listing charge of one entry with a name of `name` bytes and a link target of `target`
/// bytes. (The output directory's path, which each placement also holds, is not known when
/// the listing is charged; `NAME_COPIES` leaves a margin for it.)
pub fn entry_charge(name: usize, target: usize) -> u64 {
    ENTRY_CHARGE.saturating_add(NAME_COPIES.saturating_mul((name + target) as u64))
}
/// The most member indices per `ReadMember` (8 KiB of indices: well below 64 KiB and below
/// common pipe buffers).
pub const MEMBER_BATCH: usize = 1024;
/// How often the broker looks at the worker while it waits for a frame.
const TICK: Duration = Duration::from_millis(50);

mod detect;
pub use detect::{
    detect, detect_file, volume_set, Format, Ranges, WorkerKind, HEAD, MAX_VOLUMES, PE_SCAN,
};

/// How the worker is sandboxed (Windows; Linux runs it unsandboxed, decision (d)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sandbox {
    /// LPAC AppContainer, no capability: the default.
    #[default]
    Lpac,
    /// Restricted token at Low integrity: an opt-in, weaker (D-97 (4)).
    Restricted,
}

/// The broker's limits.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// The worker's `Hello` must arrive within this.
    pub hello: Duration,
    /// The worker is killed after this long without observed progress.
    pub no_progress: Duration,
    /// After `Cancel`, the worker's time to answer.
    pub cancel_grace: Duration,
    /// The most entries (`None`: no cap beyond the format's structural one).
    pub max_entries: Option<u64>,
    /// Bytes written at most this times the archive's length (`None`: no cap).
    pub max_expansion: Option<u64>,
    /// The ceiling of the worker's user time (`None`: [`CPU_OFF`]).
    pub max_cpu: Option<Duration>,
    /// The memory budget (D-67): the worker's commit limit and the listing charge; `None` is
    /// the extraction default ([`crate::memory::EXTRACT_CEILING`] resolved).
    pub memory: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            hello: HELLO_TIMEOUT,
            no_progress: NO_PROGRESS,
            cancel_grace: CANCEL_GRACE,
            max_entries: Some(DEFAULT_MAX_ENTRIES),
            max_expansion: Some(DEFAULT_MAX_EXPANSION),
            max_cpu: Some(DEFAULT_MAX_CPU),
            memory: None,
        }
    }
}

impl Limits {
    fn budget(&self) -> u64 {
        crate::memory::Budget::resolve(self.memory, crate::memory::EXTRACT_CEILING).bytes
    }
}

/// Where the worker is and how it runs.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The worker executable; `None` is `lpk-worker` beside the running executable.
    pub worker: Option<PathBuf>,
    /// The libarchive worker executable (E4-4); `None` is `lpk-worker-libarchive` beside the
    /// running executable.
    pub worker_libarchive: Option<PathBuf>,
    /// The sandbox mode.
    pub sandbox: Sandbox,
    /// The AppContainer name; `None` is `lpk_sandbox_sys::APPCONTAINER_NAME`.
    pub appcontainer: Option<String>,
    /// The limits.
    pub limits: Limits,
    /// Test hook: extra arguments for the worker (a test worker's `--fault=<name>`; a shipped
    /// worker refuses them). Empty outside tests.
    #[doc(hidden)]
    pub worker_args: Vec<String>,
}

/// The worker's file name.
pub const WORKER_NAME: &str = if cfg!(windows) {
    "lpk-worker.exe"
} else {
    "lpk-worker"
};

/// The libarchive worker's file name (E4-4).
pub const WORKER_LIBARCHIVE_NAME: &str = if cfg!(windows) {
    "lpk-worker-libarchive.exe"
} else {
    "lpk-worker-libarchive"
};

/// The message of the [`WorkerError::Sandbox`] refusing the restricted token for a format the
/// libarchive worker reads (design section 1: LPAC only, never the restricted token for
/// libarchive).
pub const RESTRICTED_REFUSED: &str = "the restricted-token sandbox is never used for the libarchive worker (C code); this format needs the default LPAC sandbox";

impl Options {
    fn worker_exe(&self, kind: WorkerKind) -> Result<PathBuf, CoreError> {
        let (given, name) = match kind {
            WorkerKind::Rust => (&self.worker, WORKER_NAME),
            WorkerKind::Libarchive => (&self.worker_libarchive, WORKER_LIBARCHIVE_NAME),
        };
        if let Some(w) = given {
            return Ok(w.clone());
        }
        let exe = std::env::current_exe().map_err(|e| WorkerError::Spawn {
            message: format!("cannot locate the host executable: {e}"),
        })?;
        Ok(exe.with_file_name(name))
    }
}

/// What a foreign extraction did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForeignSummary {
    /// The format, as the worker named it (escaped).
    pub format: String,
    /// Entries listed.
    pub entries: u64,
    /// Files written (hard-link copies included).
    pub files: u64,
    /// Directory entries created.
    pub directories: u64,
    /// Symlinks created.
    pub symlinks: u64,
    /// File bytes written.
    pub bytes: u64,
    /// Entries skipped because of what existed at their target.
    pub skipped: u64,
    /// Entries written under a new name.
    pub renamed: u64,
    /// Symlink entries let pass and not created.
    pub symlinks_skipped: u64,
    /// Warnings of the launch (an AppContainer temporary folder that could not be emptied).
    pub warnings: Vec<String>,
}

/// A foreign archive's validated listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The format, as the worker named it (escaped).
    pub format: String,
    /// The entries, in archive order (a hard link listed as an empty file).
    pub entries: Vec<Entry>,
    /// Parallel to `entries`: the member is encrypted.
    pub encrypted: Vec<bool>,
    /// Warnings of the launch.
    pub warnings: Vec<String>,
}

/// Escape control characters (and lossily decode) bytes from the worker for display.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for c in String::from_utf8_lossy(bytes).chars() {
        if c.is_control() {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The worker as the broker sees it.

/// What the OS shows of the worker.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Usage {
    read_bytes: u64,
    user_time: Duration,
    peak_memory: u64,
}

pub(crate) enum RecvError {
    /// The response pipe ended.
    Closed,
    /// It carried something that is not a frame.
    Proto(ProtoError),
}

/// The worker: a sandboxed process, or (tests, fuzzing) a script of its output.
/// An encoded request frame (it may hold a password: wiped on drop).
pub(crate) type Frame = lpk_proto::Zeroizing<Vec<u8>>;

/// A request as one frame; a request the protocol refuses (an over-long format name) is a
/// protocol error, never an empty or truncated frame.
fn encode(r: &Request) -> Result<Frame, CoreError> {
    let mut f = lpk_proto::Zeroizing::new(Vec::new());
    r.write_to(&mut *f)
        .map_err(|e| protocol(format!("a request cannot be encoded: {e}")))?;
    Ok(f)
}

pub(crate) trait Peer {
    /// Hand a frame to the writer without blocking: `Ok(Some(frame))` back when the writer is
    /// still busy with earlier frames (the worker is not reading).
    fn try_send(&mut self, frame: Frame) -> Result<Option<Frame>, RecvError>;
    /// The next frame if one comes within `tick`.
    fn recv(&mut self, tick: Duration) -> Result<Option<Response>, RecvError>;
    fn usage(&self) -> Option<Usage>;
    fn set_cpu_limit(&self, d: Duration);
    /// The exit status if the worker exits within `wait`.
    fn exit_code(&self, wait: Duration) -> Option<u32>;
    fn kill(&self);
    fn stderr(&self) -> String;
}

/// Recorded worker output, one frame at a time; requests are dropped. The pipe ends with the
/// bytes.
struct ScriptPeer<'a> {
    input: &'a [u8],
    buf: Vec<u8>,
}

impl Peer for ScriptPeer<'_> {
    fn try_send(&mut self, _frame: Frame) -> Result<Option<Frame>, RecvError> {
        Ok(None)
    }
    fn recv(&mut self, _tick: Duration) -> Result<Option<Response>, RecvError> {
        match Response::read_from(&mut self.input, &mut self.buf) {
            Ok(Some(r)) => Ok(Some(r)),
            Ok(None) => Err(RecvError::Closed),
            Err(e) => Err(RecvError::Proto(e)),
        }
    }
    fn usage(&self) -> Option<Usage> {
        None
    }
    fn set_cpu_limit(&self, _d: Duration) {}
    fn exit_code(&self, _wait: Duration) -> Option<u32> {
        Some(0)
    }
    fn kill(&self) {}
    fn stderr(&self) -> String {
        String::new()
    }
}

#[cfg(any(windows, target_os = "linux"))]
mod process {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{mpsc, Arc, Mutex};

    /// The first and last [`STDERR_KEEP`] bytes of the worker's diagnostics.
    #[derive(Default)]
    pub(super) struct Diag {
        head: Vec<u8>,
        tail: VecDeque<u8>,
        dropped: bool,
    }

    impl Diag {
        pub(super) fn push(&mut self, mut b: &[u8]) {
            let room = STDERR_KEEP.saturating_sub(self.head.len());
            let n = room.min(b.len());
            self.head.extend_from_slice(&b[..n]);
            b = &b[n..];
            for &x in b {
                if self.tail.len() == STDERR_KEEP {
                    self.tail.pop_front();
                    self.dropped = true;
                }
                self.tail.push_back(x);
            }
        }
        pub(super) fn text(&self) -> String {
            let mut s = escape(&self.head);
            if self.dropped {
                s.push_str(" [...] ");
            }
            let tail: Vec<u8> = self.tail.iter().copied().collect();
            s.push_str(&escape(&tail));
            s.trim().to_string()
        }
    }

    pub(super) struct ProcessPeer {
        worker: lpk_sandbox_sys::Worker,
        requests: mpsc::SyncSender<Frame>,
        rx: mpsc::Receiver<Result<Response, RecvError>>,
        diag: Arc<Mutex<Diag>>,
    }

    #[cfg(windows)]
    fn borrow<T: std::os::windows::io::AsHandle>(t: &T) -> lpk_sandbox_sys::Borrowed<'_> {
        t.as_handle()
    }
    #[cfg(unix)]
    fn borrow<T: std::os::fd::AsFd>(t: &T) -> lpk_sandbox_sys::Borrowed<'_> {
        t.as_fd()
    }

    fn spawn_err(e: std::io::Error) -> CoreError {
        if let Some(p) = e
            .get_ref()
            .and_then(|i| i.downcast_ref::<lpk_sandbox_sys::ProfileMissing>())
        {
            return WorkerError::Sandbox {
                message: p.to_string(),
            }
            .into();
        }
        match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput => WorkerError::Spawn {
                message: e.to_string(),
            },
            _ => WorkerError::Sandbox {
                message: e.to_string(),
            },
        }
        .into()
    }

    /// The AppContainer's temporary folder, `%LOCALAPPDATA%\Packages\<name>\AC\Temp`, the
    /// one writable place every LPAC worker shares (D-97 (4)).
    pub(super) fn appcontainer_temp(name: &str) -> Option<PathBuf> {
        let base = std::env::var_os("LOCALAPPDATA")?;
        let mut p = PathBuf::from(base);
        p.push("Packages");
        p.push(name);
        p.push("AC");
        p.push("Temp");
        Some(p)
    }

    /// A reparse point (a junction, a symlink, any other) on Windows; a symlink elsewhere.
    #[cfg(windows)]
    fn is_reparse(m: &std::fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || m.file_type().is_symlink()
        }
        #[cfg(not(windows))]
        {
            m.file_type().is_symlink()
        }
    }

    /// Empty the AppContainer's temporary folder of `name` (D-97 (4)), as the full user on a
    /// folder the sandbox may write: refused when `Packages\<name>`, `AC` or `Temp` is a
    /// reparse point or not a directory (attributes read without following:
    /// `symlink_metadata`); inside, every reparse point is removed as a link, never entered,
    /// and directories are emptied one level at a time. `Err` is a warning for the caller.
    /// Residual: a sandboxed worker running at the same time could swap a directory for a
    /// junction between the check and a removal; workers run one per archive and the folder
    /// is emptied before the launch, when no worker of this host runs.
    #[cfg(windows)]
    pub(super) fn clear_appcontainer_temp(name: &str) -> Result<(), String> {
        let Some(temp) = appcontainer_temp(name) else {
            return Ok(());
        };
        let mut chain = Vec::new();
        let mut p = temp.clone();
        for _ in 0..3 {
            chain.push(p.clone());
            if !p.pop() {
                break;
            }
        }
        chain.reverse();
        for dir in &chain {
            match std::fs::symlink_metadata(dir) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(format!("{}: {e}", dir.display())),
                Ok(m) if is_reparse(&m) => {
                    return Err(format!(
                        "{} is a reparse point; temporary folder not emptied",
                        dir.display()
                    ))
                }
                Ok(m) if !m.is_dir() => {
                    return Err(format!("{} is not a directory; not emptied", dir.display()))
                }
                Ok(_) => {}
            }
        }
        clear_tree(&temp).map_err(|e| format!("emptying {}: {e}", temp.display()))
    }

    /// Remove everything in `dir` without following a reparse point: a reparse point is
    /// removed as a link, a real directory with `remove_dir_all`.
    #[cfg(windows)]
    pub(super) fn clear_tree(dir: &Path) -> std::io::Result<()> {
        let mut first_err = None;
        for ent in std::fs::read_dir(dir)?.flatten() {
            let p = ent.path();
            let r = match std::fs::symlink_metadata(&p) {
                // The link itself: a directory link (junction) goes with remove_dir, a file link
                // with remove_file; neither follows it.
                Ok(m) if is_reparse(&m) => {
                    std::fs::remove_dir(&p).or_else(|_| std::fs::remove_file(&p))
                }
                // A real directory: std's remove_dir_all, which removes relative to the parent
                // handle and does not follow links inside (since the CVE-2022-21658 fix).
                Ok(m) if m.is_dir() => std::fs::remove_dir_all(&p),
                Ok(_) => std::fs::remove_file(&p),
                Err(e) => Err(e),
            };
            if let Err(e) = r {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    pub(super) fn spawn(
        exe: &Path,
        archives: &[File],
        opts: &Options,
        memory: u64,
    ) -> Result<(ProcessPeer, Vec<String>), CoreError> {
        use lpk_sandbox_sys::{roles, Access, Launch, Mode};
        #[allow(unused_mut)]
        let mut warnings = Vec::new();
        let pipe = |what: &str| {
            std::io::pipe().map_err(|e| {
                CoreError::from(WorkerError::Spawn {
                    message: format!("{what} pipe: {e}"),
                })
            })
        };
        let (req_r, req_w) = pipe("request")?;
        let (resp_r, resp_w) = pipe("response")?;
        let (err_r, err_w) = pipe("diagnostics")?;
        let name = opts
            .appcontainer
            .clone()
            .unwrap_or_else(|| lpk_sandbox_sys::APPCONTAINER_NAME.to_string());
        let mode = match opts.sandbox {
            Sandbox::Lpac => Mode::Lpac,
            Sandbox::Restricted => Mode::RestrictedToken,
        };
        #[cfg(windows)]
        if mode == Mode::Lpac {
            warnings.extend(prepare_lpac(exe, &name)?);
        }
        let cpu_ceiling = opts.limits.max_cpu.unwrap_or(CPU_OFF);
        let limits = lpk_sandbox_sys::Limits {
            memory,
            cpu_time: CPU_STEP.min(cpu_ceiling),
            cpu_ceiling,
        };
        let mut launch = Launch::new(exe)
            .mode(mode)
            .appcontainer_name(&name)
            .limits(limits)
            .args(&opts.worker_args)
            .handle(roles::REQUESTS, borrow(&req_r), Access::Read)
            .handle(roles::RESPONSES, borrow(&resp_w), Access::Write)
            .handle(roles::STDERR, borrow(&err_w), Access::Write);
        // The volumes of a set take the roles after the first archive handle, in order.
        for (i, a) in (0u32..).zip(archives) {
            launch = launch.handle(roles::ARCHIVE + i, borrow(a), Access::Read);
        }
        let worker = launch.spawn().map_err(spawn_err)?;
        drop((req_r, resp_w, err_w));
        // Responses: decoded by a thread into a small channel, so the broker can wait with a
        // timeout; at most a few frames (each at most 1 MiB) are held, and a full channel
        // blocks the thread and then the worker (pull only).
        let (tx, rx) = mpsc::sync_channel::<Result<Response, RecvError>>(4);
        let mut resp_r = resp_r;
        std::thread::Builder::new()
            .name("lpk-broker-responses".into())
            .spawn(move || {
                let mut buf = Vec::new();
                loop {
                    let r = match Response::read_from(&mut resp_r, &mut buf) {
                        Ok(Some(r)) => Ok(r),
                        Ok(None) => Err(RecvError::Closed),
                        Err(e) => Err(RecvError::Proto(e)),
                    };
                    let last = r.is_err();
                    if tx.send(r).is_err() || last {
                        return;
                    }
                }
            })
            .map_err(|e| WorkerError::Spawn {
                message: format!("response thread: {e}"),
            })?;
        let diag = Arc::new(Mutex::new(Diag::default()));
        let d = Arc::clone(&diag);
        let mut err_r = err_r;
        std::thread::Builder::new()
            .name("lpk-broker-diagnostics".into())
            .spawn(move || {
                let mut b = [0u8; 4096];
                while let Ok(n) = err_r.read(&mut b) {
                    if n == 0 {
                        return;
                    }
                    if let Ok(mut g) = d.lock() {
                        g.push(&b[..n]);
                    }
                }
            })
            .map_err(|e| WorkerError::Spawn {
                message: format!("diagnostics thread: {e}"),
            })?;
        // Requests: written by a thread from a channel of one frame, so a worker that stops
        // reading blocks that thread, never the broker (which keeps polling its watchdog and
        // the cancel token).
        let (req_tx, req_rx) = mpsc::sync_channel::<Frame>(1);
        let mut req_w = req_w;
        std::thread::Builder::new()
            .name("lpk-broker-requests".into())
            .spawn(move || {
                while let Ok(f) = req_rx.recv() {
                    if req_w.write_all(&f).and_then(|()| req_w.flush()).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| WorkerError::Spawn {
                message: format!("request thread: {e}"),
            })?;
        Ok((
            ProcessPeer {
                worker,
                requests: req_tx,
                rx,
                diag,
            },
            warnings,
        ))
    }

    /// The LPAC launch's prerequisites, as the installer will provide them (E7): the
    /// AppContainer profile (created once, reused; idempotent) and read and execute on the
    /// worker image for the package SID (a failure to grant is left to the launch to report).
    /// The AppContainer's temporary folder is emptied first (D-97 (4): the worker never uses
    /// it; a file a compromised worker left there is gone before the next one starts).
    #[cfg(windows)]
    /// What persists: the profile (per-user folders and registry) and the read/execute ACE
    /// for its package SID on the worker image; E7's uninstaller and `lpk-worker
    /// --remove-profile` remove the profile. A cleanup that cannot run is a warning.
    fn prepare_lpac(exe: &Path, name: &str) -> Result<Vec<String>, CoreError> {
        const READ_EXECUTE: u32 = 0x0012_0089 | 0x0012_00A0;
        lpk_sandbox_sys::create_appcontainer_profile(name).map_err(|e| WorkerError::Sandbox {
            message: format!("AppContainer profile {name}: {e}"),
        })?;
        let mut warnings = Vec::new();
        if let Err(w) = clear_appcontainer_temp(name) {
            warnings.push(w);
        }
        if let Ok(sid) = lpk_sandbox_sys::appcontainer_sid_string(name) {
            let _ = lpk_sandbox_sys::grant_path_access(exe, &sid, READ_EXECUTE, false);
        }
        Ok(warnings)
    }

    impl Peer for ProcessPeer {
        fn try_send(&mut self, frame: Frame) -> Result<Option<Frame>, RecvError> {
            match self.requests.try_send(frame) {
                Ok(()) => Ok(None),
                Err(mpsc::TrySendError::Full(f)) => Ok(Some(f)),
                Err(mpsc::TrySendError::Disconnected(_)) => Err(RecvError::Closed),
            }
        }
        fn recv(&mut self, tick: Duration) -> Result<Option<Response>, RecvError> {
            match self.rx.recv_timeout(tick) {
                Ok(Ok(r)) => Ok(Some(r)),
                Ok(Err(e)) => Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(RecvError::Closed),
            }
        }
        fn usage(&self) -> Option<Usage> {
            self.worker.usage().ok().map(|u| Usage {
                read_bytes: u.read_bytes,
                user_time: u.user_time,
                peak_memory: u.peak_memory,
            })
        }
        fn set_cpu_limit(&self, d: Duration) {
            let _ = self.worker.set_cpu_limit(d);
        }
        fn exit_code(&self, wait: Duration) -> Option<u32> {
            self.worker.wait(Some(wait)).ok().flatten()
        }
        fn kill(&self) {
            let _ = self.worker.kill();
        }
        fn stderr(&self) -> String {
            // The diagnostics thread may still be draining the last bytes.
            std::thread::sleep(Duration::from_millis(20));
            self.diag.lock().map(|d| d.text()).unwrap_or_default()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn diagnostics_keep_the_first_and_last_bytes_escaped() {
            let mut d = Diag::default();
            d.push(b"start\x1b[31m\n");
            d.push(&vec![b'x'; STDERR_KEEP * 3]);
            d.push(b"end\0");
            let t = d.text();
            assert!(t.starts_with("start\\u{1b}[31m\\n"), "{}", &t[..40]);
            assert!(t.ends_with("end\\0"), "{}", &t[t.len() - 20..]);
            assert!(t.contains("[...]"));
            assert!(t.len() < STDERR_KEEP * 2 + 100);
        }
    }
}

/// The AppContainer temporary folder the broker empties before each LPAC launch, for `name`
/// (the default name when `None`); `None` when `LOCALAPPDATA` is not set.
#[cfg(any(windows, target_os = "linux"))]
pub fn appcontainer_temp(name: Option<&str>) -> Option<PathBuf> {
    process::appcontainer_temp(name.unwrap_or(lpk_sandbox_sys::APPCONTAINER_NAME))
}

// ---------------------------------------------------------------------------------------------
// The conversation.

struct Conv<'m, P: Peer> {
    peer: P,
    limits: Limits,
    archive_len: u64,
    monitor: &'m Monitor,
    memory: u64,
    /// `Data` bytes received.
    data: u64,
    /// Members finished (`MemberEnd`), each credited as progress (at most the members asked
    /// for, which the stream checks index by index).
    ends: u64,
    /// The progress figure last seen, and when it last grew.
    seen: u64,
    last: Instant,
    cpu_limit: Duration,
    cpu_max: Duration,
    usage: Usage,
    killed: bool,
    /// Warnings of the launch.
    warnings: Vec<String>,
    /// The format the host detected (sent in `Open`; decides the record checks).
    format: Format,
    /// A password was sent in `Open`.
    has_password: bool,
}

impl<'m, P: Peer> Conv<'m, P> {
    fn new(peer: P, limits: Limits, archive_len: u64, monitor: &'m Monitor) -> Self {
        let cpu_max = limits.max_cpu.unwrap_or(CPU_OFF);
        let memory = limits.budget();
        Conv {
            peer,
            limits,
            archive_len,
            monitor,
            memory,
            data: 0,
            ends: 0,
            seen: 0,
            last: Instant::now(),
            cpu_limit: CPU_STEP.min(cpu_max),
            cpu_max,
            usage: Usage::default(),
            killed: false,
            warnings: Vec::new(),
            format: Format::TestFixture,
            has_password: false,
        }
    }

    fn kill(&mut self) {
        self.peer.kill();
        self.killed = true;
    }

    /// Queue a request; while the writer is busy (the worker is not reading), keep polling
    /// the cancel token and the watchdog.
    fn send(&mut self, r: &Request) -> Result<(), CoreError> {
        let mut frame = encode(r)?;
        loop {
            match self.peer.try_send(frame) {
                Ok(None) => return Ok(()),
                Ok(Some(back)) => frame = back,
                Err(_) => return Err(self.died()),
            }
            self.monitor.cancel.check()?;
            std::thread::sleep(TICK);
            self.watch("the worker to read a request")?;
        }
    }

    /// Observe the worker: raise its CPU limit on progress, kill it after `no_progress`
    /// without.
    fn watch(&mut self, waiting_for: &'static str) -> Result<(), CoreError> {
        if let Some(u) = self.peer.usage() {
            self.usage = u;
        }
        let progress = self
            .usage
            .read_bytes
            .min(self.archive_len.saturating_mul(self.format.read_passes()))
            .saturating_add(self.data)
            .saturating_add(self.ends);
        if progress > self.seen {
            self.seen = progress;
            self.last = Instant::now();
            let want = (self.usage.user_time + CPU_STEP).min(self.cpu_max);
            if want > self.cpu_limit {
                self.peer.set_cpu_limit(want);
                self.cpu_limit = want;
            }
        } else if self.last.elapsed() > self.limits.no_progress {
            let seconds = self.last.elapsed().as_secs_f64();
            self.kill();
            return Err(WorkerError::Timeout {
                seconds,
                waiting_for,
            }
            .into());
        }
        Ok(())
    }

    /// The worker's pipe closed: why.
    fn died(&mut self) -> CoreError {
        let code = self.peer.exit_code(Duration::from_millis(500));
        if let Some(u) = self.peer.usage() {
            self.usage = u;
        }
        let stderr = self.peer.stderr();
        let Some(code) = code else {
            self.kill();
            return WorkerError::Pipe { stderr }.into();
        };
        let margin = (self.memory / 16).max(1 << 20);
        // The job's user-time kill ends the process with STATUS_QUOTA_EXCEEDED (E4-1, E4-5):
        // named by the status too, in case the job's accounting could not be read.
        const STATUS_QUOTA_EXCEEDED: u32 = 0xC000_0044;
        if self.usage.user_time + Duration::from_secs(1) >= self.cpu_limit
            || (cfg!(unix) && (code == 128 + 24 || code == 128 + 9))
            || (cfg!(windows) && code == STATUS_QUOTA_EXCEEDED)
        {
            WorkerError::CpuLimit {
                seconds: self.cpu_limit.as_secs(),
            }
            .into()
        } else if self.usage.peak_memory > 0
            && self.usage.peak_memory.saturating_add(margin) >= self.memory
        {
            WorkerError::MemoryLimit { limit: self.memory }.into()
        } else if code == 0 {
            WorkerError::Pipe { stderr }.into()
        } else {
            WorkerError::Crashed { code, stderr }.into()
        }
    }

    /// The next frame; `deadline` bounds the wait regardless of progress (the `Hello`).
    fn next_by(
        &mut self,
        waiting_for: &'static str,
        deadline: Option<Instant>,
    ) -> Result<Response, CoreError> {
        loop {
            self.monitor.cancel.check()?;
            match self.peer.recv(TICK) {
                Ok(Some(Response::Fatal { message })) => {
                    self.kill();
                    return Err(WorkerError::Panicked {
                        message: escape(message.as_bytes()),
                    }
                    .into());
                }
                Ok(Some(r)) => {
                    // A frame is not progress by itself: a worker sending frames that move
                    // nothing forward is stopped as one that sends nothing.
                    self.watch(waiting_for)?;
                    return Ok(r);
                }
                Ok(None) => {}
                Err(RecvError::Closed) => return Err(self.died()),
                Err(RecvError::Proto(e)) => {
                    self.kill();
                    return Err(protocol(e.to_string()));
                }
            }
            if let Some(d) = deadline {
                if Instant::now() >= d {
                    let seconds = self.limits.hello.as_secs_f64();
                    self.kill();
                    return Err(WorkerError::Timeout {
                        seconds,
                        waiting_for,
                    }
                    .into());
                }
            }
            self.watch(waiting_for)?;
        }
    }

    fn next(&mut self, waiting_for: &'static str) -> Result<Response, CoreError> {
        self.next_by(waiting_for, None)
    }

    fn handshake(&mut self) -> Result<(), CoreError> {
        let deadline = Instant::now() + self.limits.hello;
        match self.next_by("its Hello", Some(deadline))? {
            Response::Hello { proto, build }
                if proto == lpk_proto::PROTO && build == lpk_proto::BUILD =>
            {
                Ok(())
            }
            Response::Hello { build, .. } => Err(WorkerError::BuildMismatch {
                host: lpk_proto::BUILD.to_string(),
                worker: escape(build.as_bytes()),
            }
            .into()),
            other => Err(unexpected("Hello", &other)),
        }
    }

    fn open(
        &mut self,
        format: Format,
        name: &[u8],
        credentials: Option<&Credentials>,
    ) -> Result<String, CoreError> {
        let password = credentials.map(|c| lpk_proto::Zeroizing::new(c.password.clone()));
        self.format = format;
        self.has_password = password.is_some();
        self.send(&Request::Open {
            format: format.name().to_string(),
            name: name.to_vec(),
            password,
        })?;
        match self.next("the archive to open")? {
            Response::Opened { format } => Ok(escape(format.as_bytes())),
            Response::NeedPassword => Err(FormatError::PasswordRequired.into()),
            Response::Error { code, message } => Err(reader_error(code, &message)),
            other => Err(unexpected("Opened", &other)),
        }
    }

    /// Every page of the listing, under the entries cap and the listing charge.
    fn list(&mut self) -> Result<Vec<Record>, CoreError> {
        let format = self.format;
        // Compressed headers: the archive's length times the expansion limit, over the
        // smallest header (multiplied first, so a small compressed archive still has room).
        let structural = if format.compressed_headers() {
            self.archive_len
                .saturating_mul(self.limits.max_expansion.unwrap_or(1))
                .div_ceil(format.min_header().max(1))
        } else {
            self.archive_len / format.min_header().max(1)
        };
        let (cap, flag) = match self.limits.max_entries {
            Some(m) if m < structural => (m, "--max-entries"),
            _ => (
                structural,
                "the archive's length over the format's smallest header",
            ),
        };
        let mut records: Vec<Record> = Vec::new();
        let mut charged = 0u64;
        loop {
            self.send(&Request::List {
                from: records.len() as u64,
                max: PAGE,
            })?;
            match self.next("a listing page")? {
                Response::Entries {
                    records: page,
                    more,
                } => {
                    if page.len() > PAGE as usize {
                        self.kill();
                        return Err(protocol(format!(
                            "a page of {} records when at most {PAGE} were asked for",
                            page.len()
                        )));
                    }
                    if page.is_empty() && more {
                        self.kill();
                        return Err(protocol("an empty page saying more follow".into()));
                    }
                    let total = records.len() as u64 + page.len() as u64;
                    if total > cap || (more && total >= cap) {
                        self.kill();
                        return Err(ForeignError::LimitExceeded {
                            limit: "entries",
                            value: total + u64::from(more),
                            max: cap,
                            flag,
                        }
                        .into());
                    }
                    for r in &page {
                        charged =
                            charged.saturating_add(entry_charge(r.name.len(), r.target.len()));
                    }
                    if charged > self.memory {
                        self.kill();
                        return Err(ForeignError::LimitExceeded {
                            limit: "listing memory",
                            value: charged,
                            max: self.memory,
                            flag: "--memory",
                        }
                        .into());
                    }
                    records.extend(page);
                    if !more {
                        return Ok(records);
                    }
                }
                Response::Error { code, message } => return Err(reader_error(code, &message)),
                other => return Err(unexpected("Entries", &other)),
            }
        }
    }

    /// `Cancel`, a grace period for `Cancelled`, then the kill.
    fn cancel(&mut self) {
        let sent = encode(&Request::Cancel).map(|f| self.peer.try_send(f));
        if self.killed || !matches!(sent, Ok(Ok(None))) {
            self.kill();
            return;
        }
        let end = Instant::now() + self.limits.cancel_grace;
        while Instant::now() < end {
            match self.peer.recv(TICK) {
                Ok(Some(Response::Cancelled)) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        self.kill();
    }

    fn close(&mut self) {
        if !self.killed {
            if let Ok(f) = encode(&Request::Close) {
                let _ = self.peer.try_send(f);
            }
            let _ = self.peer.exit_code(Duration::from_millis(500));
        }
        self.kill();
    }
}

fn protocol(message: String) -> CoreError {
    WorkerError::Protocol { message }.into()
}

fn unexpected(wanted: &str, got: &Response) -> CoreError {
    let name = match got {
        Response::Hello { .. } => "Hello",
        Response::Opened { .. } => "Opened",
        Response::NeedPassword => "NeedPassword",
        Response::Entries { .. } => "Entries",
        Response::MemberStart { .. } => "MemberStart",
        Response::Data(_) => "Data",
        Response::MemberEnd { .. } => "MemberEnd",
        Response::Cancelled => "Cancelled",
        Response::Error { .. } => "Error",
        Response::Fatal { .. } => "Fatal",
    };
    protocol(format!("{name} where {wanted} was expected"))
}

/// The named error for a reader's `Error` frame.
fn reader_error(code: u16, message: &str) -> CoreError {
    let message = escape(message.as_bytes());
    match ErrorCode::from_u16(code) {
        Some(ErrorCode::NeedPassword) => FormatError::PasswordRequired.into(),
        Some(ErrorCode::WrongPassword) => FormatError::WrongKey.into(),
        Some(ErrorCode::Corrupt) | Some(ErrorCode::NoSuchMember) | Some(ErrorCode::Io) => {
            ForeignError::Corrupt { message }.into()
        }
        Some(ErrorCode::Unsupported) => ForeignError::Unsupported { message }.into(),
        Some(ErrorCode::Denied) => WorkerError::Denied { message }.into(),
        Some(ErrorCode::Protocol) | Some(ErrorCode::Stopped) => protocol(message),
        None => protocol(format!("unknown error code {code}: {message}")),
    }
}

// ---------------------------------------------------------------------------------------------
// The listing, checked.

/// The checked listing: entries for the policy pass, and per entry its record position and
/// (for a hard link) the name it links to.
struct Checked {
    entries: Vec<Entry>,
    index: Vec<u64>,
    hard_target: Vec<Option<String>>,
    encrypted: Vec<bool>,
}

fn check_records(
    records: Vec<Record>,
    archive_len: u64,
    format: Format,
) -> Result<Checked, CoreError> {
    let corrupt = |m: String| CoreError::from(ForeignError::Corrupt { message: m });
    // Packed ranges: required where the format has them (D-99 (4)), inside the archive, and
    // no overlap between blocks.
    let mut ranges: Vec<(u64, u64, u64, usize)> = Vec::new(); // (start, end, block, record)
    let mut by_block: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (i, r) in records.iter().enumerate() {
        if r.packed_len == 0 {
            let required = match format.ranges() {
                Ranges::Advisory => false,
                Ranges::Every => true,
                Ranges::NonEmptyFiles => r.kind == RecordKind::File && r.size > 0,
            };
            if required {
                return Err(corrupt(format!(
                    "{}: no packed range, which a {} archive's record must have",
                    escape(&r.name),
                    format.name()
                )));
            }
            continue;
        }
        let end = r
            .packed_offset
            .checked_add(r.packed_len)
            .filter(|&e| e <= archive_len)
            .ok_or_else(|| {
                corrupt(format!(
                    "{}: packed range outside the archive",
                    escape(&r.name)
                ))
            })?;
        let block = if r.block == NO_BLOCK {
            u64::MAX - i as u64
        } else {
            r.block
        };
        match by_block.get(&block) {
            Some(&k) => {
                let e = &mut ranges[k];
                e.0 = e.0.min(r.packed_offset);
                e.1 = e.1.max(end);
            }
            None => {
                by_block.insert(block, ranges.len());
                ranges.push((r.packed_offset, end, block, i));
            }
        }
    }
    ranges.sort_unstable();
    for w in ranges.windows(2) {
        if w[1].0 < w[0].1 {
            return Err(ForeignError::OverlappingEntries {
                first: escape(&records[w[0].3].name),
                second: escape(&records[w[1].3].name),
            }
            .into());
        }
    }
    let mut out = Checked {
        entries: Vec::with_capacity(records.len()),
        index: Vec::with_capacity(records.len()),
        hard_target: Vec::with_capacity(records.len()),
        encrypted: Vec::with_capacity(records.len()),
    };
    for (i, r) in records.into_iter().enumerate() {
        let path = String::from_utf8(r.name)
            .map_err(|e| corrupt(format!("{}: name is not UTF-8", escape(e.as_bytes()))))?;
        let (kind, size, symlink_target, hard) = match r.kind {
            RecordKind::File => (EntryKind::File, r.size, None, None),
            RecordKind::Directory if r.size == 0 => (EntryKind::Directory, 0, None, None),
            RecordKind::Directory => {
                return Err(corrupt(format!(
                    "{}: a directory with a size",
                    escape(path.as_bytes())
                )))
            }
            RecordKind::Symlink if !r.target.is_empty() => (
                EntryKind::Symlink,
                r.target.len() as u64,
                Some(r.target),
                None,
            ),
            RecordKind::Symlink => {
                return Err(corrupt(format!(
                    "{}: a link without a target",
                    escape(path.as_bytes())
                )))
            }
            RecordKind::HardLink => {
                let t = String::from_utf8(r.target).map_err(|_| {
                    corrupt(format!(
                        "{}: hard link target is not UTF-8",
                        escape(path.as_bytes())
                    ))
                })?;
                (EntryKind::File, 0, None, Some(t))
            }
        };
        out.entries.push(Entry {
            kind,
            flags: EntryFlags::EMPTY,
            path,
            mtime_ns: r.mtime_ns,
            size,
            symlink_target,
            chunks: Vec::new(),
        });
        out.index.push(i as u64);
        out.hard_target.push(hard);
        out.encrypted.push(r.flags & FLAG_ENCRYPTED != 0);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Writing.

/// Where member bytes go: files under the destination, or (fuzzing, tests) nowhere.
trait Out {
    fn create(&mut self, path: &Path, replace: bool) -> Result<(), CoreError>;
    fn write(&mut self, data: &[u8]) -> Result<(), CoreError>;
    fn finish(&mut self, mtime_ns: i64) -> Result<(), CoreError>;
    /// Remove the file being written, if any (D-54).
    fn abort(&mut self);
    /// Copy a finished file (a hard link's source) to a new file.
    fn copy(
        &mut self,
        from: &Path,
        to: &Path,
        replace: bool,
        mtime_ns: i64,
    ) -> Result<u64, CoreError>;
    fn free_space(&self) -> Option<u64>;
}

struct FsOut {
    dir: PathBuf,
    cur: Option<(File, PathBuf)>,
}

impl Out for FsOut {
    fn create(&mut self, path: &Path, replace: bool) -> Result<(), CoreError> {
        if replace {
            policy::remove_existing(path).map_err(|e| CoreError::io(path, e))?;
        }
        let f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| CoreError::io(path, e))?;
        self.cur = Some((f, path.to_path_buf()));
        Ok(())
    }
    fn write(&mut self, data: &[u8]) -> Result<(), CoreError> {
        match self.cur.as_mut() {
            Some((f, p)) => f.write_all(data).map_err(|e| CoreError::io(p.as_path(), e)),
            None => Err(CoreError::Extract("data with no file open")),
        }
    }
    fn finish(&mut self, mtime_ns: i64) -> Result<(), CoreError> {
        if let Some((f, p)) = self.cur.as_ref() {
            if let Some(m) = mtime(mtime_ns) {
                f.set_modified(m)
                    .map_err(|e| CoreError::io(p.as_path(), e))?;
            }
        }
        self.cur = None;
        Ok(())
    }
    fn abort(&mut self) {
        if let Some((f, p)) = self.cur.take() {
            drop(f);
            let _ = std::fs::remove_file(p);
        }
    }
    fn copy(
        &mut self,
        from: &Path,
        to: &Path,
        replace: bool,
        mtime_ns: i64,
    ) -> Result<u64, CoreError> {
        let mut src = File::open(from).map_err(|e| CoreError::io(from, e))?;
        self.create(to, replace)?;
        let n = match self.cur.as_mut() {
            Some((f, _)) => std::io::copy(&mut src, f).map_err(|e| CoreError::io(to, e))?,
            None => 0,
        };
        self.finish(mtime_ns)?;
        Ok(n)
    }
    fn free_space(&self) -> Option<u64> {
        free_space(&self.dir)
    }
}

#[cfg(any(windows, target_os = "linux"))]
fn free_space(p: &Path) -> Option<u64> {
    let mut p = p;
    loop {
        if p.exists() {
            return lpk_procstat_sys::free_space(p);
        }
        p = p.parent()?;
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn free_space(_p: &Path) -> Option<u64> {
    None
}

/// Writes nothing; counts.
#[derive(Default)]
struct NullOut {
    written: u64,
}

impl Out for NullOut {
    fn create(&mut self, _path: &Path, _replace: bool) -> Result<(), CoreError> {
        Ok(())
    }
    fn write(&mut self, data: &[u8]) -> Result<(), CoreError> {
        self.written += data.len() as u64;
        Ok(())
    }
    fn finish(&mut self, _mtime_ns: i64) -> Result<(), CoreError> {
        Ok(())
    }
    fn abort(&mut self) {}
    fn copy(&mut self, _: &Path, _: &Path, _: bool, _: i64) -> Result<u64, CoreError> {
        Ok(0)
    }
    fn free_space(&self) -> Option<u64> {
        None
    }
}

/// One file to receive from the worker.
struct Target {
    index: u64,
    name: String,
    path: PathBuf,
    size: u64,
    mtime_ns: i64,
    replace: bool,
}

/// One hard link to copy after the files.
struct HardCopy {
    path: PathBuf,
    source: PathBuf,
    mtime_ns: i64,
    replace: bool,
}

/// The run after the listing: checks, the policy pass, the limits, directories and links,
/// then the members streamed into `out`, then the hard-link copies.
#[allow(clippy::too_many_arguments)]
fn run_extract<P: Peer>(
    conv: &mut Conv<'_, P>,
    records: Vec<Record>,
    dir: &Path,
    write_fs: bool,
    out: &mut dyn Out,
    policy: &dyn ExtractPolicy,
    select: &Selection,
    summary: &mut ForeignSummary,
) -> Result<(), CoreError> {
    summary.entries = records.len() as u64;
    let declared: std::collections::HashMap<u64, u64> = records
        .iter()
        .enumerate()
        .map(|(i, r)| (i as u64, r.size))
        .collect();
    let checked = check_records(records, conv.archive_len, conv.format)?;
    // Duplicate names, refused before the policy pass (a second member would meet the first's
    // file at create-new).
    let mut seen = std::collections::HashSet::new();
    for e in &checked.entries {
        if !seen.insert(e.path.as_str()) {
            return Err(CoreError::DuplicatePath {
                path: escape(e.path.as_bytes()),
            });
        }
    }
    let by_name: std::collections::HashMap<String, (u64, Option<String>)> = checked
        .entries
        .iter()
        .zip(checked.index.iter().zip(checked.hard_target.iter()))
        .map(|(e, (&i, h))| (e.path.clone(), (i, h.clone())))
        .collect();
    let checked_encrypted = checked.encrypted.clone();
    let mut pass = policy_pass(checked.entries, dir, policy, select)?;
    let mkdirs = pass.mkdirs(dir);
    for e in &pass.entries {
        lpk_format::validate_path(&e.path).map_err(|reason| FormatError::UnsafePath {
            path: escape(e.path.as_bytes()),
            reason,
        })?;
    }
    // Files to receive, in listing order; hard links to copy.
    let mut targets: Vec<Target> = Vec::new();
    let mut hard: Vec<(PathBuf, String, i64, bool)> = Vec::new();
    let mut links = Vec::new();
    for (e, p) in pass.entries.iter().zip(std::mem::take(&mut pass.placed)) {
        match p {
            Placed::Dir(_) | Placed::Skipped => {}
            Placed::Link(l) => links.push(l),
            Placed::File { path, replace } => {
                let (index, h) = by_name
                    .get(&e.path)
                    .cloned()
                    .ok_or(CoreError::Extract("an entry lost its record"))?;
                match h {
                    Some(t) => hard.push((path, t, e.mtime_ns, replace)),
                    None => targets.push(Target {
                        index,
                        name: e.path.clone(),
                        size: declared.get(&index).copied().unwrap_or(0),
                        path,
                        mtime_ns: e.mtime_ns,
                        replace,
                    }),
                }
            }
        }
    }
    targets.sort_by_key(|t| t.index);
    // An encrypted member to receive and no password given: asked for before anything is
    // written, so the retry with a password (`with_credentials`) starts from a clean slate.
    if !conv.has_password {
        let encrypted: std::collections::HashSet<u64> = checked_encrypted
            .iter()
            .enumerate()
            .filter(|(_, &e)| e)
            .map(|(i, _)| i as u64)
            .collect();
        if targets.iter().any(|t| encrypted.contains(&t.index)) {
            conv.kill();
            return Err(FormatError::PasswordRequired.into());
        }
    }
    // Hard links resolve by name to a file this run receives (one map lookup each, the
    // cancel token polled as they go).
    let by_target: std::collections::HashMap<&str, (&Path, u64)> = targets
        .iter()
        .map(|t| (t.name.as_str(), (t.path.as_path(), t.size)))
        .collect();
    let mut copies: Vec<HardCopy> = Vec::with_capacity(hard.len());
    let mut total = targets.iter().fold(0u64, |a, t| a.saturating_add(t.size));
    for (i, (path, t, mtime_ns, replace)) in hard.into_iter().enumerate() {
        if i % 4096 == 0 {
            conv.monitor.cancel.check()?;
        }
        let (source, size) =
            by_target
                .get(t.as_str())
                .copied()
                .ok_or_else(|| ForeignError::Unsupported {
                    message: format!(
                        "hard link {} to {}, a member that is not extracted",
                        escape(path.to_string_lossy().as_bytes()),
                        escape(t.as_bytes())
                    ),
                })?;
        // The output cap and free space count each copy (from the declared sizes).
        total = total.saturating_add(size);
        copies.push(HardCopy {
            path,
            source: source.to_path_buf(),
            mtime_ns,
            replace,
        });
    }
    drop(by_target);
    let hard = copies;
    if let Some(f) = conv.limits.max_expansion {
        let max = f.saturating_mul(conv.archive_len.max(1));
        if total > max {
            conv.kill();
            return Err(ForeignError::LimitExceeded {
                limit: "declared output",
                value: total,
                max,
                flag: "--max-expansion",
            }
            .into());
        }
    }
    if let Some(free) = out.free_space() {
        if free < total {
            conv.kill();
            return Err(ForeignError::NoSpace {
                needed: total,
                available: free,
            }
            .into());
        }
    }
    conv.monitor.cancel.check()?;
    summary.directories = pass.directories;
    summary.skipped = pass.skipped;
    summary.renamed = pass.renamed;
    summary.symlinks_skipped = pass.symlinks_skipped;
    if write_fs {
        make_dirs(&mkdirs)?;
        make_links(&links)?;
    }
    summary.symlinks = links.len() as u64;
    let rep = conv.monitor.reporter();
    rep.totals(Some(targets.len() as u64 + hard.len() as u64), None);
    stream(conv, &targets, out, &rep, total, summary)?;
    for h in &hard {
        conv.monitor.cancel.check()?;
        let n = out.copy(&h.source, &h.path, h.replace, h.mtime_ns)?;
        summary.files += 1;
        summary.bytes += n;
    }
    rep.done_if_started(Stage::Extract);
    Ok(())
}

/// Receive every target's bytes, in batches of `ReadMember`.
fn stream<P: Peer>(
    conv: &mut Conv<'_, P>,
    targets: &[Target],
    out: &mut dyn Out,
    rep: &Reporter,
    total: u64,
    summary: &mut ForeignSummary,
) -> Result<(), CoreError> {
    let mut written = 0u64;
    let mut next_space_check = FREE_SPACE_STEP;
    for batch in targets.chunks(MEMBER_BATCH) {
        conv.send(&Request::ReadMember {
            indices: batch.iter().map(|t| t.index).collect(),
        })?;
        for t in batch {
            match conv.next("a member's start")? {
                Response::MemberStart { index } if index == t.index => {}
                Response::Error { code, message } => return Err(reader_error(code, &message)),
                other => return Err(unexpected("MemberStart", &other)),
            }
            out.create(&t.path, t.replace)?;
            let mut received = 0u64;
            loop {
                match conv.next("member data")? {
                    Response::Data(d) if d.is_empty() => {
                        conv.kill();
                        return Err(protocol("an empty Data frame".into()));
                    }
                    Response::Data(d) => {
                        let n = d.len() as u64;
                        if received.saturating_add(n) > t.size {
                            conv.kill();
                            return Err(ForeignError::SizeMismatch {
                                path: escape(t.name.as_bytes()),
                                declared: t.size,
                                received: received.saturating_add(n),
                            }
                            .into());
                        }
                        out.write(&d)?;
                        received += n;
                        written += n;
                        conv.data += n;
                        summary.bytes += n;
                        if written >= next_space_check {
                            next_space_check = written.saturating_add(FREE_SPACE_STEP);
                            if let Some(free) = out.free_space() {
                                let left = total.saturating_sub(written);
                                if free < left {
                                    conv.kill();
                                    return Err(ForeignError::NoSpace {
                                        needed: left,
                                        available: free,
                                    }
                                    .into());
                                }
                            }
                        }
                        rep.set(
                            Stage::Extract,
                            summary.files,
                            conv.usage.read_bytes.min(conv.archive_len),
                            written,
                            0,
                        );
                    }
                    Response::MemberEnd { index, .. } if index == t.index => {
                        if received != t.size {
                            conv.kill();
                            return Err(ForeignError::SizeMismatch {
                                path: escape(t.name.as_bytes()),
                                declared: t.size,
                                received,
                            }
                            .into());
                        }
                        out.finish(t.mtime_ns)?;
                        conv.ends += 1;
                        summary.files += 1;
                        break;
                    }
                    Response::Error { code, message } => return Err(reader_error(code, &message)),
                    other => return Err(unexpected("Data or MemberEnd", &other)),
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Entry points.

/// Open the archive, start its worker, handshake and open.
#[cfg(any(windows, target_os = "linux"))]
fn start<'m>(
    archive: &Path,
    opts: &Options,
    credentials: Option<&Credentials>,
    monitor: &'m Monitor,
) -> Result<(Conv<'m, process::ProcessPeer>, Format, String), CoreError> {
    if credentials.is_some_and(|c| c.keyfile.is_some()) {
        return Err(ForeignError::Unsupported {
            message: "a foreign archive takes no keyfile; --keyfile is not needed".into(),
        }
        .into());
    }
    let format = detect_file(archive)
        .map_err(|e| CoreError::io(archive, e))?
        .ok_or(ForeignError::UnknownFormat)?;
    let kind = format.worker();
    // Design section 1: the libarchive worker (C code) runs under LPAC only.
    if kind == WorkerKind::Libarchive && opts.sandbox == Sandbox::Restricted {
        return Err(WorkerError::Sandbox {
            message: RESTRICTED_REFUSED.to_string(),
        }
        .into());
    }
    let paths = match volume_set(archive, format).map_err(|e| CoreError::io(archive, e))? {
        Ok(p) => p,
        Err(n) => {
            return Err(ForeignError::Unsupported {
                message: format!(
                "a volume set of {n} or more volumes; at most {MAX_VOLUMES} are read (named limit)"
            ),
            }
            .into())
        }
    };
    let mut files = Vec::with_capacity(paths.len());
    let mut len = 0u64;
    for p in &paths {
        let f = File::open(p).map_err(|e| CoreError::io(p, e))?;
        len = len.saturating_add(f.metadata().map_err(|e| CoreError::io(p, e))?.len());
        files.push(f);
    }
    // The worker gets the file name only (single streams name their member from it), never
    // the directory.
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned().into_bytes())
        .unwrap_or_default();
    let exe = opts.worker_exe(kind)?;
    let memory = opts.limits.budget();
    let (peer, warnings) = process::spawn(&exe, &files, opts, memory)?;
    drop(files);
    let mut conv = Conv::new(peer, opts.limits.clone(), len, monitor);
    conv.warnings = warnings;
    let r = conv
        .handshake()
        .and_then(|()| conv.open(format, &name, credentials));
    match r {
        Ok(name) => Ok((conv, format, name)),
        Err(e) => {
            conv.close();
            Err(e)
        }
    }
}

/// Extract the foreign archive at `archive` into `dir` through a sandboxed worker (module
/// documentation): the listing checked and passed through `policy` and `select` before
/// anything is written, every file written by the host.
#[cfg(any(windows, target_os = "linux"))]
pub fn extract_file(
    archive: &Path,
    dir: &Path,
    credentials: Option<&Credentials>,
    policy: &dyn ExtractPolicy,
    select: &Selection,
    opts: &Options,
    monitor: &Monitor,
) -> Result<ForeignSummary, CoreError> {
    let (mut conv, _format, name) = start(archive, opts, credentials, monitor)?;
    let mut summary = ForeignSummary {
        format: name,
        warnings: conv.warnings.clone(),
        ..ForeignSummary::default()
    };
    let mut out = FsOut {
        dir: dir.to_path_buf(),
        cur: None,
    };
    let r = conv.list().and_then(|records| {
        run_extract(
            &mut conv,
            records,
            dir,
            true,
            &mut out,
            policy,
            select,
            &mut summary,
        )
    });
    finish_run(&mut conv, &mut out, r).map(|()| summary)
}

/// Cleanup after a run: the partial file removed on failure, the worker told to cancel (on
/// a cancellation) or closed, then killed.
fn finish_run<P: Peer>(
    conv: &mut Conv<'_, P>,
    out: &mut dyn Out,
    r: Result<(), CoreError>,
) -> Result<(), CoreError> {
    match r {
        Ok(()) => {
            conv.close();
            Ok(())
        }
        Err(e) => {
            if matches!(e, CoreError::Cancelled) {
                conv.cancel();
            } else {
                conv.kill();
            }
            out.abort();
            Err(e)
        }
    }
}

/// List the foreign archive at `archive` through a sandboxed worker: the records checked
/// (names, ranges, sizes of non-files) but not passed through an extraction policy.
#[cfg(any(windows, target_os = "linux"))]
pub fn list_file(
    archive: &Path,
    credentials: Option<&Credentials>,
    opts: &Options,
    monitor: &Monitor,
) -> Result<Listing, CoreError> {
    let (mut conv, format, name) = start(archive, opts, credentials, monitor)?;
    let len = conv.archive_len;
    let checked = conv
        .list()
        .and_then(|records| check_records(records, len, format));
    let mut out = NullOut::default();
    match checked {
        Ok(c) => {
            finish_run(&mut conv, &mut out, Ok(()))?;
            Ok(Listing {
                warnings: std::mem::take(&mut conv.warnings),
                format: name,
                entries: c.entries,
                encrypted: c.encrypted,
            })
        }
        Err(e) => Err(finish_run(&mut conv, &mut out, Err(e))
            .err()
            .unwrap_or(CoreError::Extract("a failed listing reported no error"))),
    }
}

/// Run the broker's side over recorded worker output (fuzzing and the malicious-worker tests):
/// the decoder, the limits, the listing checks, the policy pass and the writer — into `dir`
/// when given (files really created), else into a sink that only counts. The requests are
/// dropped; the end of `output` is the worker closing its pipe. Returns the summary and the
/// bytes the writer received.
#[doc(hidden)]
pub fn replay(
    output: &[u8],
    archive_len: u64,
    dir: Option<&Path>,
    policy: &dyn ExtractPolicy,
    limits: &Limits,
) -> (Result<ForeignSummary, CoreError>, u64) {
    let monitor = Monitor::default();
    let peer = ScriptPeer {
        input: output,
        buf: Vec::new(),
    };
    let mut conv = Conv::new(peer, limits.clone(), archive_len, &monitor);
    let mut summary = ForeignSummary::default();
    // A path that cannot exist (a NUL), so the policy pass never looks at the disk.
    let nowhere = PathBuf::from("lpk-replay\0nowhere");
    let run = |out: &mut dyn Out, conv: &mut Conv<'_, ScriptPeer<'_>>, s: &mut ForeignSummary| {
        conv.handshake()?;
        s.format = conv.open(Format::TestFixture, b"", None)?;
        let records = conv.list()?;
        let d = dir.unwrap_or(&nowhere);
        run_extract(
            conv,
            records,
            d,
            dir.is_some(),
            out,
            policy,
            &Selection::all(),
            s,
        )
    };
    match dir {
        Some(d) => {
            let mut out = FsOut {
                dir: d.to_path_buf(),
                cur: None,
            };
            let r = run(&mut out, &mut conv, &mut summary);
            let r = finish_run(&mut conv, &mut out, r);
            let bytes = summary.bytes;
            (r.map(|()| summary), bytes)
        }
        None => {
            let mut out = NullOut::default();
            let r = run(&mut out, &mut conv, &mut summary);
            let r = finish_run(&mut conv, &mut out, r);
            (r.map(|()| summary), out.written)
        }
    }
}

#[cfg(test)]
mod tests;
