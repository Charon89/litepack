//! The maintenance dialogs' command layer (E5-8; `docs/notes/e5-design.md` sections 2 and 6).
//!
//! - **Test** ([`App::test_run`], [`App::test_archive`]): verifies every frame and chunk with
//!   progress and Cancel. A keyless open of a sealed archive verifies the frame hashes and the
//!   recovery frames only (`chunks_checked` false): the result offers Unlock, which asks for the
//!   password through the [`Prompter`] and re-runs the full check; a failure offers repair.
//! - **Check and repair** ([`App::check_run`], [`App::repair_run`]): `check` counts the recovery
//!   frames and the damaged shards; a repair writes a copy named `<stem> (repaired).lpk`
//!   (`(repaired 2)` when taken), never over anything, beside the archive or in a folder grant,
//!   then tests the copy. The original is never written.
//! - **History** ([`App::history`], [`App::history_browse`], [`App::rollback_run`]): the trailer
//!   chain; Browse opens an earlier generation read-only; a rollback truncates in place after a
//!   native confirm ([`Confirm`]) that names the generations lost, with the session closed first
//!   (D-110 (2)) and the archive reopened after.
//! - **Update from folder** ([`App::append_plan`], [`App::append_run`]): the journaled
//!   [`crate::write::append_archive`]; an encrypted archive asks for its password before
//!   anything is written; a failed or cancelled append leaves the archive as it was.
//!
//! No command takes a path: archives, folders and the folder to append are grants.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use lpk_api::{
    CancelToken, Class, CreateSettings, Credentials, Error, Monitor, OpenOptions, Session,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::app::{App, Opened};
use crate::create::{TierChoice, MAX_RECOVERY, PROPOSED_RECOVERY};
use crate::extract::{open_unlocked_at, Prompter};
use crate::grants::{Grant, GrantKind, Granted};
use crate::{strings, GuiError};

/// Asks the user a Yes/No question (the native box on the main thread; a test's answer). The
/// rollback's confirmation: the page cannot answer it for the user.
pub trait Confirm: Send + Sync {
    /// Ask; true on Yes.
    fn confirm(&self, title: &str, text: &str) -> bool;
}

/// How a test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TestState {
    /// Everything checked matched.
    Passed,
    /// A frame, chunk or the archive itself failed.
    Failed,
    /// Cancelled (a password prompt, Cancel, the exit).
    Cancelled,
}

/// How a run that has no failure state of its own ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    /// The run finished.
    Done,
    /// Cancelled: nothing was kept (a repaired copy is removed, an append left no trace).
    Cancelled,
    /// The user declined the confirmation: nothing happened.
    Declined,
    /// The run failed after the session was closed: the archive was reopened if it could be
    /// (`error` says why).
    Failed,
}

/// What a test found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    /// The archive's file name.
    pub archive: String,
    /// `lpk`, or the foreign format's name (empty when the archive did not open).
    pub format: String,
    /// How it ended.
    pub state: TestState,
    /// Entries in the entry table.
    pub entries: u64,
    /// Chunks in the chunk table.
    pub chunks: u64,
    /// `ChunkData` blocks.
    pub blocks: u64,
    /// Every chunk was decoded and compared; false for a keyless open of a sealed archive
    /// (frame hashes and recovery frames only).
    pub chunks_checked: bool,
    /// The archive is sealed and was tested without its key: the page offers Unlock.
    pub offer_unlock: bool,
    /// A failed `.lpk`: the page offers "Check and repair...".
    pub offer_repair: bool,
    /// The failure: its message, the block and entry it names.
    pub error: Option<GuiError>,
}

impl TestResult {
    fn stopped(archive: &str, format: &str, state: TestState, error: Option<GuiError>) -> Self {
        TestResult {
            archive: archive.to_string(),
            format: format.to_string(),
            state,
            entries: 0,
            chunks: 0,
            blocks: 0,
            chunks_checked: false,
            offer_unlock: false,
            // Repair rebuilds damaged bytes: not for a missing prior, a limit or a sandbox.
            offer_repair: state == TestState::Failed
                && format == "lpk"
                && error.as_ref().is_some_and(|e| e.is(Class::Damaged)),
            error,
        }
    }
}

/// Test `session`, the run's whole body.
fn test_with(session: &Session, archive: &str, monitor: &Monitor) -> TestResult {
    let info = session.info();
    match session.test(monitor) {
        Ok(r) => TestResult {
            archive: archive.to_string(),
            format: info.format.clone(),
            state: TestState::Passed,
            entries: r.entries,
            chunks: r.chunks,
            blocks: r.blocks,
            chunks_checked: r.chunks_checked,
            offer_unlock: !r.chunks_checked && info.encrypted,
            offer_repair: false,
            error: None,
        },
        Err(e) if e.class == Class::Cancelled => {
            TestResult::stopped(archive, &info.format, TestState::Cancelled, None)
        }
        Err(e) => TestResult::stopped(archive, &info.format, TestState::Failed, Some(e.into())),
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    /// The archive's file name.
    pub archive: String,
    /// Finished, or cancelled.
    pub state: RunState,
    /// Recovery frames the index lists (0: the archive has no recovery records).
    pub frames: u64,
    /// Frames that failed their own checks: their coverage is unprotected.
    pub frames_unusable: u64,
    /// Data shards that differ from their frame's hash.
    pub shards_damaged: u64,
    /// The archive carries recovery records.
    pub has_recovery: bool,
    /// Damage was found: the page offers "Repair to a copy...".
    pub damaged: bool,
}

/// What a repair wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairResult {
    /// The archive's file name.
    pub archive: String,
    /// Finished, or cancelled (no copy is left).
    pub state: RunState,
    /// The copy's file name.
    pub copy: Option<String>,
    /// A grant for the copy (Open).
    pub grant: Option<Granted>,
    /// Recovery frames.
    pub frames: u64,
    /// Frames that failed their own checks.
    pub frames_unusable: u64,
    /// Damaged shards found.
    pub shards_damaged: u64,
    /// Shards rebuilt into the copy.
    pub shards_repaired: u64,
    /// A frame with more damage than it could rebuild (the copy is kept with every repair that
    /// was possible, D-58 (1)).
    pub unrepairable: Option<GuiError>,
    /// The copy, tested.
    pub test: Option<TestResult>,
}

/// One generation of the trailer chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Generation {
    /// The generation number (0 for the first write).
    pub generation: u64,
    /// The bytes the archive spans with this generation the latest.
    pub size: u64,
    /// The newest.
    pub latest: bool,
}

/// The generations of an archive, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    /// The archive's file name.
    pub archive: String,
    /// The generations.
    pub generations: Vec<Generation>,
    /// The generation the session shows.
    pub open: u64,
}

/// What a rollback did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackResult {
    /// The archive's file name.
    pub archive: String,
    /// Done, or declined at the confirmation.
    pub state: RunState,
    /// The generation the archive ends with now.
    pub generation: u64,
    /// The bytes the archive spans now.
    pub size: u64,
    /// The generations removed.
    pub lost: u64,
    /// The archive, reopened.
    pub opened: Option<Opened>,
    /// Why it could not be reopened (the rollback is done).
    pub reopen_error: Option<GuiError>,
    /// Why the rollback failed (state `failed`).
    pub error: Option<GuiError>,
}

/// What the Update from folder dialog starts with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendPlan {
    /// The archive's file name.
    pub archive: String,
    /// The archive is encrypted: the run asks for the password first.
    pub encrypted: bool,
    /// The latest generation now.
    pub generation: u64,
    /// The tier selected.
    pub tier: TierChoice,
    /// The proposed recovery percent.
    pub recovery: u8,
    /// The most recovery percent.
    pub max_recovery: u8,
    /// "Test when done".
    pub test: bool,
}

/// What the page asks to append.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendRequest {
    /// The folder (a grant of kind `input`, from the folder picker).
    pub folder: Grant,
    /// The tier.
    pub tier: TierChoice,
    /// Recovery percent, 0 to [`MAX_RECOVERY`].
    pub recovery: u8,
    /// Verify the archive when done.
    pub test: bool,
}

/// What an append did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendResult {
    /// The archive's file name.
    pub archive: String,
    /// Finished, or cancelled (the archive is as it was).
    pub state: RunState,
    /// The generation the archive ends with.
    pub generation: u64,
    /// Entries the new generation holds.
    pub entries: u64,
    /// Chunks the new generation added.
    pub chunks: u64,
    /// `ChunkData` blocks it wrote.
    pub blocks: u64,
    /// The archive's length now.
    pub archive_len: u64,
    /// Seconds the append took.
    pub seconds: f64,
    /// The archive, tested when asked.
    pub test: Option<TestResult>,
    /// The archive, reopened on its latest generation.
    pub opened: Option<Opened>,
    /// Why it could not be reopened.
    pub reopen_error: Option<GuiError>,
    /// Why the append failed (state `failed`; the archive is as it was).
    pub error: Option<GuiError>,
}

/// Hands a password already typed to the first ask, then defers to `then` (an archive reopened
/// after an append does not ask again).
struct Known<'a> {
    password: &'a Zeroizing<String>,
    used: AtomicBool,
    then: &'a dyn Prompter,
}

impl Prompter for Known<'_> {
    fn password(&self, archive: &str, wrong: bool) -> Option<Zeroizing<String>> {
        if !wrong && !self.used.swap(true, Ordering::SeqCst) {
            return Some(self.password.clone());
        }
        self.then.password(archive, wrong)
    }

    fn interactive(&self) -> bool {
        self.then.interactive()
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn refuse(class: Class, kind: &str, why: &str, path: &Path) -> GuiError {
    Error::new(class, kind, why, Some(path)).into()
}

/// `<stem> (repaired).lpk` beside or in `dir`, `(repaired 2)`, `(repaired 3)`... while taken.
pub fn repaired_name(dir: &Path, archive: &Path) -> PathBuf {
    let stem = archive
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_string());
    let mut n = 1u32;
    loop {
        let file = if n == 1 {
            format!("{stem} (repaired).lpk")
        } else {
            format!("{stem} (repaired {n}).lpk")
        };
        let out = dir.join(file);
        let mut partial = out.clone().into_os_string();
        partial.push(".partial");
        if (!out.exists() && !Path::new(&partial).exists()) || n == u32::MAX {
            return out;
        }
        n += 1;
    }
}

impl App {
    /// Test session `id`'s archive. With `unlock` the password is asked for (through
    /// `prompter`) and the session reopened with it first, so that a keyless result's Unlock
    /// re-runs the full check; the session stays unlocked.
    pub fn test_run(
        &self,
        id: u64,
        unlock: bool,
        prompter: &dyn Prompter,
        monitor: &Monitor,
    ) -> Result<TestResult, GuiError> {
        let slot = self.slot(id)?;
        if unlock {
            let session = slot.session();
            let info = session.info();
            let at = (info.generation != info.latest_generation).then_some(info.generation);
            match open_unlocked_at(session.path(), at, self.motw_off(), prompter, Some(false)) {
                Ok(fresh) => {
                    crate::extract::swap_unlocked(&slot.cell, &session, fresh);
                }
                Err(e) if e.class == Class::Cancelled => {
                    return Ok(TestResult::stopped(
                        &slot.name,
                        &info.format,
                        TestState::Cancelled,
                        None,
                    ))
                }
                Err(e) => return Err(e.into()),
            }
        }
        let session = slot.session();
        Ok(test_with(&session, &slot.name, monitor))
    }

    /// The verb `--test`: open the archive of `grant` (keyless when sealed; with `unlock` the
    /// password is asked for first), test it and close it. A failure, including one to open, is
    /// the archive's own result.
    pub fn test_archive(
        &self,
        grant: &Grant,
        unlock: bool,
        prompter: &dyn Prompter,
        monitor: &Monitor,
    ) -> Result<TestResult, GuiError> {
        let path = self.grants.resolve(grant, GrantKind::Archive)?;
        let name = file_name(&path);
        let opened = open_unlocked_at(
            &path,
            None,
            self.motw_off(),
            prompter,
            unlock.then_some(false),
        );
        let session = match opened {
            Ok(s) => s,
            Err(e) if e.class == Class::Cancelled => {
                return Ok(TestResult::stopped(&name, "", TestState::Cancelled, None))
            }
            Err(e) => {
                // No session opened, so nothing to repair: no format, no offer.
                return Ok(TestResult::stopped(
                    &name,
                    "",
                    TestState::Failed,
                    Some(e.into()),
                ));
            }
        };
        let r = test_with(&session, &name, monitor);
        let _ = session.close();
        Ok(r)
    }

    /// Check the recovery records of session `id`'s archive (`.lpk` only).
    pub fn check_run(&self, id: u64, monitor: &Monitor) -> Result<CheckResult, GuiError> {
        let slot = self.slot(id)?;
        let session = slot.session();
        let mut r = CheckResult {
            archive: slot.name.clone(),
            state: RunState::Done,
            frames: 0,
            frames_unusable: 0,
            shards_damaged: 0,
            has_recovery: false,
            damaged: false,
        };
        match session.check(monitor) {
            Ok(c) => {
                r.frames = c.frames;
                r.frames_unusable = c.frames_unusable;
                r.shards_damaged = c.shards_damaged;
                r.has_recovery = c.frames > 0;
                r.damaged = c.frames_unusable > 0 || c.shards_damaged > 0;
            }
            Err(e) if e.class == Class::Cancelled => r.state = RunState::Cancelled,
            Err(e) => return Err(e.into()),
        }
        Ok(r)
    }

    /// Write a repaired copy of session `id`'s archive into `folder` (a grant of kind `folder`;
    /// the archive's own folder when none) under [`repaired_name`], then test the copy. The
    /// archive is never written.
    pub fn repair_run(
        &self,
        id: u64,
        folder: Option<&Grant>,
        monitor: &Monitor,
    ) -> Result<RepairResult, GuiError> {
        let slot = self.slot(id)?;
        let session = slot.session();
        let path = session.path().to_path_buf();
        require_latest_lpk(&session, "repair")?;
        let dir = match folder {
            Some(g) => self.grants.resolve(g, GrantKind::Folder)?,
            None => match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => PathBuf::from("."),
            },
        };
        let out = repaired_name(&dir, &path);
        // Written as `<name>.partial` and published without replacing anything when complete:
        // a process dying mid-repair leaves a visibly named partial, never a short `.lpk`.
        let mut partial_name = out.clone().into_os_string();
        partial_name.push(".partial");
        let partial = PathBuf::from(partial_name);
        let mut r = RepairResult {
            archive: slot.name.clone(),
            state: RunState::Done,
            copy: None,
            grant: None,
            frames: 0,
            frames_unusable: 0,
            shards_damaged: 0,
            shards_repaired: 0,
            unrepairable: None,
            test: None,
        };
        let (report, unrepairable) = match session.repair(&partial, monitor) {
            Ok(x) => x,
            Err(e) if e.class == Class::Cancelled => {
                r.state = RunState::Cancelled;
                return Ok(r);
            }
            Err(e) => return Err(e.into()),
        };
        if let Err(e) = crate::write::publish(&partial, &out) {
            let _ = std::fs::remove_file(&partial);
            return Err(crate::io_error(e, &out));
        }
        r.frames = report.frames;
        r.frames_unusable = report.frames_unusable;
        r.shards_damaged = report.shards_damaged;
        r.shards_repaired = report.shards_repaired;
        r.unrepairable = unrepairable.map(GuiError::from);
        let copy = file_name(&out);
        r.grant = Some(self.grants.mint(GrantKind::Archive, &out));
        r.test = Some(match Session::open(&out, &OpenOptions::default()) {
            Ok(s) => {
                let t = test_with(&s, &copy, monitor);
                let _ = s.close();
                t
            }
            Err(e) if e.class == Class::Cancelled => {
                TestResult::stopped(&copy, "lpk", TestState::Cancelled, None)
            }
            Err(e) => TestResult::stopped(&copy, "lpk", TestState::Failed, Some(e.into())),
        });
        r.copy = Some(copy);
        Ok(r)
    }

    /// The generations of session `id`'s archive (`.lpk` only), newest first.
    pub fn history(&self, id: u64, cancel: &CancelToken) -> Result<History, GuiError> {
        let slot = self.slot(id)?;
        let session = slot.session();
        let info = session.info();
        if info.foreign {
            return Err(refuse(
                Class::Unsupported,
                "Unsupported",
                "history is for .lpk archives",
                session.path(),
            ));
        }
        // A fresh keyless session: the chain needs no key, and a shown view's own chain stops
        // at its generation.
        let fresh = Session::open(session.path(), &OpenOptions::default())?;
        let chain = fresh.history(cancel);
        let _ = fresh.close();
        let chain = chain?;
        let latest = chain.iter().map(|g| g.generation).max();
        Ok(History {
            archive: slot.name.clone(),
            generations: chain
                .iter()
                .map(|g| Generation {
                    generation: g.generation,
                    size: g.end,
                    latest: Some(g.generation) == latest,
                })
                .collect(),
            open: info.generation,
        })
    }

    /// Open `generation` of session `id`'s archive read-only as a new session (the page closes
    /// the old one). The latest generation opens as a normal session.
    pub fn history_browse(
        &self,
        id: u64,
        generation: u64,
        cancel: &CancelToken,
        prompter: &dyn Prompter,
    ) -> Result<Opened, GuiError> {
        let slot = self.slot(id)?;
        let session = slot.session();
        let info = session.info();
        if info.foreign {
            return Err(refuse(
                Class::Unsupported,
                "Unsupported",
                "history is for .lpk archives",
                session.path(),
            ));
        }
        if generation > info.latest_generation {
            return Err(GuiError::usage(format!(
                "generation {generation}: the archive ends at {}",
                info.latest_generation
            )));
        }
        let at = (generation != info.latest_generation).then_some(generation);
        self.open_path(session.path(), at, cancel, prompter)
    }

    /// Truncate session `id`'s archive to `generation` after `confirm` (a native box naming the
    /// generations lost; declined: nothing happens). The session is closed first and the
    /// archive reopened after.
    pub fn rollback_run(
        &self,
        id: u64,
        generation: u64,
        confirm: &dyn Confirm,
        prompter: &dyn Prompter,
        cancel: &CancelToken,
    ) -> Result<RollbackResult, GuiError> {
        let slot = self.slot(id)?;
        let session = slot.session();
        require_latest_lpk(&session, "roll back")?;
        let path = session.path().to_path_buf();
        let latest = session.info().latest_generation;
        let shown_length = session.info().length;
        if generation >= latest {
            return Err(GuiError::usage(format!(
                "generation {generation}: nothing is newer to remove (the archive ends at {latest})"
            )));
        }
        let lost = latest - generation;
        let (title, text) = strings::rollback_texts();
        let detail = strings::get("rollback.detail")
            .replace("{name}", &slot.name)
            .replace("{from}", &(generation + 1).to_string())
            .replace("{to}", &latest.to_string())
            .replace("{count}", &lost.to_string())
            .replace("{keep}", &generation.to_string());
        if !confirm.confirm(title, &format!("{detail}\n\n{text}")) {
            return Ok(RollbackResult {
                archive: slot.name.clone(),
                state: RunState::Declined,
                generation: latest,
                size: session.info().length,
                lost: 0,
                opened: None,
                reopen_error: None,
                error: None,
            });
        }
        // D-110 (2): the session's reads end before the file shrinks.
        self.close(id)?;
        // The file may have changed since the session opened (another process appended): read
        // its committed generation on the handle that truncates, and touch nothing unless it
        // is the one the confirmation named.
        let ran = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| lpk_api::Error::new(Class::Io, "Io", e.to_string(), Some(&path)))
            .and_then(|mut file| {
                let (now, end) = lpk_api::committed_end_of(&mut file, &path)?;
                // The number alone is not enough: the archive may have been rolled back and
                // appended again under the same number.
                if now != latest || end != shown_length {
                    return Err(lpk_api::Error::new(
                        Class::Usage,
                        "ArchiveChanged",
                        format!(
                            "the archive changed since it was opened (generation {latest}, now {now}); open it again"
                        ),
                        Some(&path),
                    ));
                }
                lpk_api::rollback_file(&mut file, &path, generation)
            });
        let (opened, reopen_error) = match self.open_path(&path, None, cancel, prompter) {
            Ok(o) => (Some(o), None),
            Err(e) => (None, Some(e)),
        };
        let (state, kept_generation, size, error) = match ran {
            Ok(k) => (RunState::Done, k.generation, k.end, None),
            Err(e) => (RunState::Failed, latest, 0, Some(GuiError::from(e))),
        };
        Ok(RollbackResult {
            archive: file_name(&path),
            state,
            generation: kept_generation,
            size,
            lost: if state == RunState::Done { lost } else { 0 },
            opened,
            reopen_error,
            error,
        })
    }

    /// What the Update from folder dialog starts with.
    pub fn append_plan(&self, id: u64) -> Result<AppendPlan, GuiError> {
        let slot = self.slot(id)?;
        let session = slot.session();
        require_latest_lpk(&session, "update")?;
        Ok(AppendPlan {
            archive: slot.name.clone(),
            encrypted: session.info().encrypted,
            generation: session.info().latest_generation,
            tier: TierChoice::Balanced,
            recovery: PROPOSED_RECOVERY,
            max_recovery: MAX_RECOVERY,
            test: true,
        })
    }

    /// Append the folder of `req` to session `id`'s archive as a new generation that mirrors the
    /// folder, through the journaled [`crate::write::append_archive`]. The session is closed
    /// first and the archive reopened after; a failed or cancelled append leaves it as it was.
    pub fn append_run(
        &self,
        id: u64,
        req: &AppendRequest,
        prompter: &dyn Prompter,
        monitor: &Monitor,
    ) -> Result<AppendResult, GuiError> {
        let slot = self.slot(id)?;
        let session = slot.session();
        require_latest_lpk(&session, "update")?;
        if req.recovery > MAX_RECOVERY {
            return Err(GuiError::usage(format!(
                "recovery {}%: at most {MAX_RECOVERY}",
                req.recovery
            )));
        }
        let path = session.path().to_path_buf();
        let dir = self.grants.resolve(&req.folder, GrantKind::Input)?;
        if !dir.is_dir() {
            return Err(GuiError::usage("the update source is not a folder"));
        }
        let mut result = AppendResult {
            archive: slot.name.clone(),
            state: RunState::Done,
            generation: session.info().latest_generation,
            entries: 0,
            chunks: 0,
            blocks: 0,
            archive_len: session.info().length,
            seconds: 0.0,
            test: None,
            opened: None,
            reopen_error: None,
            error: None,
        };
        // The password, asked for before anything is written (Cancel: nothing happened).
        let password = if session.info().encrypted {
            match self.ask_password(&path, prompter) {
                Ok(p) => Some(p),
                Err(e) if e.class == Class::Cancelled => {
                    result.state = RunState::Cancelled;
                    return Ok(result);
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            None
        };
        let credentials = password
            .as_ref()
            .map(|p| Credentials::password(p.as_bytes().to_vec()));
        let settings = CreateSettings {
            tier: req.tier.tier(),
            recovery: req.recovery,
            ..CreateSettings::default()
        };
        // D-110 (2): the session's reads end before the file grows.
        self.close(id)?;
        let started = Instant::now();
        let ran = crate::write::append_archive(
            &path,
            &dir,
            &settings,
            credentials.as_ref(),
            monitor,
            self.journal.as_ref(),
        );
        result.seconds = started.elapsed().as_secs_f64();
        match ran {
            Ok(s) => {
                result.entries = s.writer.entries;
                result.chunks = s.writer.new_chunks;
                result.blocks = s.writer.blocks;
                result.archive_len = s.writer.archive_len;
                result.generation = s.writer.generation;
            }
            Err(e) if e.is(Class::Cancelled) => result.state = RunState::Cancelled,
            Err(e) => {
                result.state = RunState::Failed;
                result.error = Some(e);
            }
        }
        if result.state == RunState::Done && req.test {
            let options = OpenOptions {
                credentials: credentials.as_ref(),
                motw_off: self.motw_off(),
                ..OpenOptions::default()
            };
            result.test = Some(match Session::open(&path, &options) {
                Ok(s) => {
                    let t = test_with(&s, &result.archive, monitor);
                    let _ = s.close();
                    t
                }
                Err(e) if e.class == Class::Cancelled => {
                    TestResult::stopped(&result.archive, "lpk", TestState::Cancelled, None)
                }
                Err(e) => {
                    TestResult::stopped(&result.archive, "lpk", TestState::Failed, Some(e.into()))
                }
            });
        }
        // Reopen on the latest generation: the typed password serves the reopen too. A cancel
        // that ended the run must not end the reopen: it has its own token.
        let fresh = CancelToken::new();
        let known;
        let prompt: &dyn Prompter = match &password {
            Some(p) => {
                known = Known {
                    password: p,
                    used: AtomicBool::new(false),
                    then: prompter,
                };
                &known
            }
            None => prompter,
        };
        match self.open_path(&path, None, &fresh, prompt) {
            Ok(o) => result.opened = Some(o),
            Err(e) => result.reopen_error = Some(e),
        }
        Ok(result)
    }

    /// Ask for the password of the archive at `path` until it is right (Cancel: `Cancelled`).
    fn ask_password(
        &self,
        path: &Path,
        prompter: &dyn Prompter,
    ) -> Result<Zeroizing<String>, Error> {
        if !prompter.interactive() {
            return Err(Error::new(
                Class::PasswordRequired,
                "PasswordRequired",
                "the archive is encrypted and there is no one to ask for the password",
                Some(path),
            ));
        }
        let name = file_name(path);
        let mut wrong = false;
        loop {
            let pw = prompter
                .password(&name, wrong)
                .ok_or_else(Error::cancelled)?;
            let creds = Credentials::password(pw.as_bytes().to_vec());
            let options = OpenOptions {
                credentials: Some(&creds),
                motw_off: self.motw_off(),
                ..OpenOptions::default()
            };
            match Session::open(path, &options) {
                Ok(s) => {
                    let _ = s.close();
                    return Ok(pw);
                }
                Err(e) if matches!(e.class, Class::PasswordRequired | Class::WrongPassword) => {
                    wrong = true;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// The maintenance actions that write need the latest generation of an `.lpk`.
fn require_latest_lpk(session: &Session, what: &str) -> Result<(), GuiError> {
    let info = session.info();
    if info.foreign {
        return Err(refuse(
            Class::Unsupported,
            "Unsupported",
            &format!("{what} is for .lpk archives"),
            session.path(),
        ));
    }
    if info.generation != info.latest_generation {
        return Err(refuse(
            Class::Usage,
            "ReadOnlyView",
            &format!(
                "{what}: generation {} is a read-only view; the archive ends at {}",
                info.generation, info.latest_generation
            ),
            session.path(),
        ));
    }
    Ok(())
}
