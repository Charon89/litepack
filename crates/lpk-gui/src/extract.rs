//! The extract wizard's command layer (E5-4; `docs/notes/e5-design.md` sections 2, 4 and 6).
//!
//! - **Destinations** are derived in Rust from a grant ([`Dest`]): Smart and New folder beside
//!   the archive (D-54's smart rule, [`lpk_api::Destination`]), This folder (the archive's
//!   own), Choose... and the recents (a folder grant). No command takes a path.
//! - **Refuse-first** (D-54 (4)): [`App::extract_plan`] counts what would be written, what
//!   already exists and the free space before any write; bytes over the free space are
//!   refused naming both figures ([`check_space`]), and existing targets are the page's one
//!   question (Skip, Keep both, Replace, Cancel): without an answer the engine refuses them.
//! - **Passwords** go through [`Prompter`] (the native dialog on the main thread, or a test's
//!   answers): the password goes straight into the engine's credentials, never to the page. A
//!   wrong one asks again; Cancel ends the operation; a session unlocked once stays unlocked.
//! - **The selection** is the picks inside the current view (scope and filter), the set the
//!   status bar counts ([`crate::browse::Browser::picked_in_view`]).
//! - **Verbs**: [`App::extract_archive`] opens, extracts beside the archive and closes, for
//!   `--extract-here` (Smart) and `--extract-to` (New folder).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lpk_api::{
    Class, Credentials, Destination, Error, ExtractOptions, Extracted, Monitor, OpenOptions,
    Overwrite, Preflight, Selection, Session,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::app::App;
use crate::grants::{Grant, GrantKind};
use crate::{GuiError, RefusalFigures};

/// The [`RefusalFigures::field`] of a free-space refusal (`needed` bytes, `allowed` free).
pub const DISK_FIELD: &str = "disk";

/// Where the page asks to extract.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Dest {
    /// The smart rule beside the archive.
    Smart,
    /// A fresh `<name>` folder beside the archive.
    NewFolder,
    /// The archive's own folder.
    Here,
    /// A folder the user chose (a [`GrantKind::Folder`] grant: Choose... or a recent one).
    Folder {
        /// The grant.
        grant: Grant,
    },
}

/// What of the archive to extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Everything.
    All,
    /// The picks inside the current view.
    Selected,
}

/// The page's answer to "some files already exist".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Conflict {
    /// No question was asked (nothing exists); an existing target refuses the run.
    Refuse,
    /// Leave the existing files.
    Skip,
    /// Write beside them under a new name.
    Rename,
    /// Replace them.
    Replace,
}

impl Conflict {
    fn overwrite(self) -> Overwrite {
        match self {
            Conflict::Refuse => Overwrite::Refuse,
            Conflict::Skip => Overwrite::Skip,
            Conflict::Rename => Overwrite::Rename,
            Conflict::Replace => Overwrite::Replace,
        }
    }
}

/// What the verbs do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `--extract-here`: the smart rule.
    Here,
    /// `--extract-to`: a new folder.
    To,
}

/// A request to extract from an open session.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// Where.
    pub dest: Dest,
    /// What.
    pub scope: Scope,
    /// The answer to the conflict question.
    pub conflict: Conflict,
    /// "Allow X": the memory (bytes) this run alone may use after an envelope refusal.
    pub memory: Option<u64>,
}

/// What the dialog shows before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// The folder's name the entries go into (never its path).
    pub folder: String,
    /// Entries selected.
    pub entries: u64,
    /// File entries.
    pub files: u64,
    /// Their bytes.
    pub bytes: u64,
    /// Targets that exist: above 0 the dialog asks once.
    pub existing: u64,
    /// Free bytes of the volume, if known.
    pub free: Option<u64>,
    /// Picks the filter hides, left out.
    pub left_out: u64,
}

/// How the extracted data was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Check {
    /// An `.lpk`: every file matched its BLAKE3 hash (true by construction when no entry failed).
    Blake3,
    /// ZIP and 7z: each member matched a checksum the archive stores.
    Members,
    /// ZIP and 7z: `matched` members matched their checksum, `none` had none stored to check.
    Partial {
        /// Members checked.
        matched: u64,
        /// Members extracted without a stored checksum.
        none: u64,
    },
    /// tar and the rest: nothing to verify against.
    Unverifiable,
}

/// How the run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// The extraction finished.
    Done,
    /// Cancelled once the engine's run had started: any files already extracted were kept, the
    /// unfinished one was removed (D-87 (2)).
    CancelledAfterStart,
    /// Cancelled before the engine's run started (a password prompt, the preflight, Cancel at
    /// once): nothing was written.
    CancelledBeforeStart,
}

/// A symlink not created because of its target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefusedLink {
    /// The entry.
    pub path: String,
    /// Why.
    pub reason: String,
}

/// What an extraction did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResult {
    /// The archive's file name.
    pub archive: String,
    /// The folder's name the entries went into.
    pub folder: String,
    /// Files written.
    pub files: u64,
    /// Folders created.
    pub directories: u64,
    /// Bytes written.
    pub bytes: u64,
    /// Entries left out because they existed.
    pub skipped: u64,
    /// Entries written under a new name.
    pub renamed: u64,
    /// Symlink entries skipped (the default).
    pub skipped_symlinks: Vec<String>,
    /// Symlink entries refused for their target.
    pub refused_symlinks: Vec<RefusedLink>,
    /// Symlink entries not created for want of the privilege.
    pub unprivileged_symlinks: Vec<String>,
    /// The Mark of the Web could not be written on some file.
    pub motw_unmarked: bool,
    /// How the data was checked; none unless a file was written (a cancelled run claims nothing).
    pub check: Option<Check>,
    /// How the run ended.
    pub outcome: Outcome,
    /// Picks the filter hid, left out.
    pub left_out: u64,
}

/// Asks the user for a password (the native dialog; a test's answers). `None`: cancelled.
pub trait Prompter: Send + Sync {
    /// Ask for the password of `archive`; `wrong` when the last one was refused.
    fn password(&self, archive: &str, wrong: bool) -> Option<Zeroizing<String>>;

    /// Whether a user can answer (false: a missing password stays the error it is).
    fn interactive(&self) -> bool {
        true
    }
}

/// Never answers: an archive needing a password stays refused.
#[derive(Debug, Clone, Copy)]
pub struct NoPrompt;

impl Prompter for NoPrompt {
    fn password(&self, _: &str, _: bool) -> Option<Zeroizing<String>> {
        None
    }

    fn interactive(&self) -> bool {
        false
    }
}

fn needs_password(e: &Error) -> bool {
    matches!(e.class, Class::PasswordRequired | Class::WrongPassword)
}

/// Open `path`, asking for the password as often as it takes (`ask_first`: ask before the
/// first try, with `Some(wrong)` saying whether the last one was refused). Cancel is
/// `Cancelled`, with nothing written.
pub(crate) fn open_unlocked(
    path: &Path,
    motw_off: bool,
    prompter: &dyn Prompter,
    ask_first: Option<bool>,
) -> Result<Session, Error> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut creds: Option<Credentials> = None;
    let mut ask = ask_first;
    loop {
        if let Some(wrong) = ask.take() {
            let pw = prompter
                .password(&name, wrong)
                .ok_or_else(Error::cancelled)?;
            creds = Some(Credentials::password(pw.as_bytes().to_vec()));
        }
        let options = OpenOptions {
            credentials: creds.as_ref(),
            motw_off,
            ..OpenOptions::default()
        };
        match Session::open(path, &options) {
            Ok(s) => return Ok(s),
            Err(e) if needs_password(&e) && prompter.interactive() => ask = Some(creds.is_some()),
            Err(e) => return Err(e),
        }
    }
}

/// Refuse-first: the bytes to write against the free space (`free` overrides what the
/// preflight measured; tests inject it). Both figures are named.
pub fn check_space(pf: &Preflight, free: Option<u64>) -> Result<(), GuiError> {
    match free.or(pf.free) {
        Some(free) if pf.bytes > free => {
            let mut e: GuiError = Error::new(
                Class::DiskFull,
                "DiskFull",
                format!(
                    "needs {} bytes; {free} bytes are free on the destination",
                    pf.bytes
                ),
                None,
            )
            .into();
            e.refusal = Some(Box::new(RefusalFigures {
                field: DISK_FIELD.into(),
                needed: pf.bytes,
                allowed: free,
            }));
            Err(e)
        }
        _ => Ok(()),
    }
}

/// The `Destination` of `dest` for the archive at `archive`.
pub fn destination(app: &App, archive: &Path, dest: &Dest) -> Result<Destination, GuiError> {
    let beside = match archive.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok(match dest {
        Dest::Smart => Destination::Smart { beside },
        Dest::NewFolder => Destination::NewFolder { beside },
        Dest::Here => Destination::Dir(beside),
        Dest::Folder { grant } => Destination::Dir(app.grants.resolve(grant, GrantKind::Folder)?),
    })
}

fn last_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// The check line of a finished extraction: only when a file was written (never "every file
/// matched" over none); ZIP and 7z by what the members' readers reported (`unchecked`).
pub fn check_of(outcome: &Extracted) -> Option<Check> {
    match outcome {
        Extracted::Lpk(s) => (s.files > 0).then_some(Check::Blake3),
        Extracted::Foreign(s) if s.files == 0 => None,
        Extracted::Foreign(s) => {
            let f = s.format.to_ascii_lowercase();
            Some(if !(f.contains("zip") || f.contains("7z")) {
                Check::Unverifiable
            } else if s.unchecked == 0 {
                Check::Members
            } else {
                Check::Partial {
                    matched: s.checked,
                    none: s.unchecked,
                }
            })
        }
    }
}

/// Watches the engine's events for what an extraction wrote, and passes them on.
struct Seen {
    files: std::sync::atomic::AtomicU64,
    bytes: std::sync::atomic::AtomicU64,
    inner: Option<Arc<dyn lpk_api::Progress>>,
}

impl lpk_api::Progress for Seen {
    fn event(&self, e: &lpk_api::Event) {
        use std::sync::atomic::Ordering::Relaxed;
        if e.stage == lpk_api::Stage::Extract {
            self.files.fetch_max(e.files_done, Relaxed);
            self.bytes.fetch_max(e.bytes_out, Relaxed);
        }
        if let Some(i) = &self.inner {
            i.event(e);
        }
    }
}

fn cancelled_result(job: &Job<'_>, folder: String, files: u64, bytes: u64) -> ExtractResult {
    ExtractResult {
        archive: job.archive.to_string(),
        folder,
        files,
        directories: 0,
        bytes,
        skipped: 0,
        renamed: 0,
        skipped_symlinks: Vec::new(),
        refused_symlinks: Vec::new(),
        unprivileged_symlinks: Vec::new(),
        motw_unmarked: false,
        check: None,
        outcome: Outcome::CancelledAfterStart,
        left_out: job.left_out,
    }
}

/// A run ended before it began (a cancelled password prompt or preflight): nothing written.
pub(crate) fn cancelled_before(archive: &str) -> ExtractResult {
    ExtractResult {
        archive: archive.to_string(),
        folder: String::new(),
        files: 0,
        directories: 0,
        bytes: 0,
        skipped: 0,
        renamed: 0,
        skipped_symlinks: Vec::new(),
        refused_symlinks: Vec::new(),
        unprivileged_symlinks: Vec::new(),
        motw_unmarked: false,
        check: None,
        outcome: Outcome::CancelledBeforeStart,
        left_out: 0,
    }
}

/// Wait for a native dialog's answer on `rx` without holding the exit: polls the operation's
/// token, and gives `None` (cancelled) when it is cancelled or the sender is gone.
pub fn wait_for_answer<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    token: &lpk_api::CancelToken,
) -> Option<T> {
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(v) => return Some(v),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if token.is_cancelled() {
                    return None;
                }
            }
        }
    }
}

/// What one extraction needs besides the session.
pub(crate) struct Job<'a> {
    pub selection: &'a Selection,
    pub dest: &'a Destination,
    pub overwrite: Overwrite,
    pub memory: Option<u64>,
    pub free: Option<u64>,
    pub motw_off: bool,
    pub left_out: u64,
    pub archive: &'a str,
}

fn attempt(s: &Session, job: &Job<'_>, monitor: &Monitor) -> Result<ExtractResult, GuiError> {
    let pf = s.preflight(job.selection, job.dest, &monitor.cancel)?;
    check_space(&pf, job.free)?;
    let options = ExtractOptions {
        overwrite: job.overwrite,
        memory: job.memory,
        ..ExtractOptions::default()
    };
    // The folder the preflight chose, so that both steps agree.
    let seen = Arc::new(Seen {
        files: std::sync::atomic::AtomicU64::new(0),
        bytes: std::sync::atomic::AtomicU64::new(0),
        inner: monitor.progress.clone(),
    });
    let watched = Monitor {
        progress: Some(seen.clone()),
        cancel: monitor.cancel.clone(),
    };
    let dir = pf.dir.clone();
    // Control flow decides the outcome: a cancel up to here is "before the run" (an `Err` the
    // caller turns into `CancelledBeforeStart`); one from the engine's run is "after start",
    // true also after zero-byte files or before the first progress event (D-87 (2): finished
    // files are kept).
    if monitor.cancel.is_cancelled() {
        return Err(Error::cancelled().into());
    }
    let report = match s.extract(job.selection, &Destination::Dir(pf.dir), &options, &watched) {
        Ok(r) => r,
        Err(e) if e.class == Class::Cancelled => {
            use std::sync::atomic::Ordering::Relaxed;
            let (files, bytes) = (seen.files.load(Relaxed), seen.bytes.load(Relaxed));
            return Ok(cancelled_result(job, last_name(&dir), files, bytes));
        }
        Err(e) => return Err(e.into()),
    };
    let (files, directories, bytes, skipped, renamed) = match &report.outcome {
        Extracted::Lpk(x) => (x.files, x.directories, x.bytes, x.skipped, x.renamed),
        Extracted::Foreign(x) => (x.files, x.directories, x.bytes, x.skipped, x.renamed),
    };
    Ok(ExtractResult {
        archive: job.archive.to_string(),
        folder: last_name(&report.dir),
        files,
        directories,
        bytes,
        skipped,
        renamed,
        skipped_symlinks: report.skipped_symlinks.clone(),
        refused_symlinks: report
            .refused_symlink_targets
            .iter()
            .map(|(p, r)| RefusedLink {
                path: p.clone(),
                reason: r.to_string(),
            })
            .collect(),
        unprivileged_symlinks: report.unprivileged_symlinks.clone(),
        motw_unmarked: report.motw_unmarked,
        check: check_of(&report.outcome),
        outcome: Outcome::Done,
        left_out: job.left_out,
    })
}

/// Extract with `cell`'s session; a password the content needs is asked for (the session is
/// reopened with it and stays so), and a wrong one asks again. A cancel before the first write
/// (the prompt, the preflight) is an `Ok` result that says nothing was written; one during it
/// says the finished files were kept.
pub(crate) fn run(
    cell: &Mutex<Arc<Session>>,
    job: &Job<'_>,
    prompter: &dyn Prompter,
    monitor: &Monitor,
) -> Result<ExtractResult, GuiError> {
    match run_inner(cell, job, prompter, monitor) {
        Err(e) if e.is(Class::Cancelled) => Ok(cancelled_before(job.archive)),
        other => other,
    }
}

fn run_inner(
    cell: &Mutex<Arc<Session>>,
    job: &Job<'_>,
    prompter: &dyn Prompter,
    monitor: &Monitor,
) -> Result<ExtractResult, GuiError> {
    loop {
        let session = cell.lock().unwrap_or_else(|p| p.into_inner()).clone();
        match attempt(&session, job, monitor) {
            Err(e) if e.is(Class::PasswordRequired) || e.is(Class::WrongPassword) => {
                let wrong = e.is(Class::WrongPassword);
                let fresh = open_unlocked(session.path(), job.motw_off, prompter, Some(wrong))?;
                let old = std::mem::replace(
                    &mut *cell.lock().unwrap_or_else(|p| p.into_inner()),
                    Arc::new(fresh),
                );
                let _ = old.close();
            }
            other => return other,
        }
    }
}

impl App {
    /// What extracting `scope` of session `id` to `dest` would do, before any write; refused
    /// when it would not fit.
    pub fn extract_plan(
        &self,
        id: u64,
        dest: &Dest,
        scope: Scope,
        cancel: &lpk_api::CancelToken,
    ) -> Result<Plan, GuiError> {
        let slot = self.slot(id)?;
        let session = slot.session();
        let (selection, left_out) = self.selection(&slot, scope)?;
        let to = destination(self, session.path(), dest)?;
        let pf = session.preflight(&selection, &to, cancel)?;
        check_space(&pf, self.free_override())?;
        Ok(Plan {
            folder: last_name(&pf.dir),
            entries: pf.entries,
            files: pf.files,
            bytes: pf.bytes,
            existing: pf.existing,
            free: self.free_override().or(pf.free),
            left_out,
        })
    }

    /// Extract `req` from session `id`.
    pub fn extract_run(
        &self,
        id: u64,
        req: &Request,
        prompter: &dyn Prompter,
        monitor: &Monitor,
    ) -> Result<ExtractResult, GuiError> {
        let slot = self.slot(id)?;
        let (selection, left_out) = self.selection(&slot, req.scope)?;
        let to = destination(self, slot.session().path(), &req.dest)?;
        let job = Job {
            selection: &selection,
            dest: &to,
            overwrite: req.conflict.overwrite(),
            memory: req.memory.or(self.memory_default()),
            free: self.free_override(),
            motw_off: self.motw_off(),
            left_out,
            archive: &slot.name,
        };
        run(&slot.cell, &job, prompter, monitor)
    }

    /// The verbs: open the archive of `grant` (asking for its password if its names are
    /// encrypted), extract all of it beside itself, Smart or into a new folder, and close it.
    pub fn extract_archive(
        &self,
        grant: &Grant,
        verb: Verb,
        memory: Option<u64>,
        prompter: &dyn Prompter,
        monitor: &Monitor,
    ) -> Result<ExtractResult, GuiError> {
        let path = self.grants.resolve(grant, GrantKind::Archive)?;
        let name = last_name(&path);
        let session = match open_unlocked(&path, self.motw_off(), prompter, None) {
            Ok(s) => s,
            Err(e) if e.class == Class::Cancelled => return Ok(cancelled_before(&name)),
            Err(e) => return Err(e.into()),
        };
        let cell = Mutex::new(Arc::new(session));
        let dest = match verb {
            Verb::Here => Dest::Smart,
            Verb::To => Dest::NewFolder,
        };
        let to = destination(self, &path, &dest)?;
        let job = Job {
            selection: &Selection::All,
            dest: &to,
            overwrite: Overwrite::Refuse,
            memory: memory.or(self.memory_default()),
            free: self.free_override(),
            motw_off: self.motw_off(),
            left_out: 0,
            archive: &name,
        };
        let r = run(&cell, &job, prompter, monitor);
        let _ = cell.lock().unwrap_or_else(|p| p.into_inner()).close();
        r
    }
}
