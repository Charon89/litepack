//! The create wizard's command layer (E5-5; `docs/notes/e5-design.md` sections 2, 4 and 6).
//!
//! - **Plan**: [`App::create_plan`] turns the inputs (grants) into the wizard's defaults (the
//!   first input's folder and stem, Balanced, recovery [`PROPOSED_RECOVERY`], "Test when
//!   done") and the totals counted in Rust ([`lpk_api::measure`]).
//! - **Estimates**: [`App::calibrate`] is the only source of a time or size figure: it runs
//!   [`lpk_api::calibrate`] on this machine over at most [`SAMPLE_BYTES`] of the inputs and
//!   scales what it measured to the totals. No rate or ratio is written in the source (a test
//!   greps for one).
//! - **Password**: asked twice through the [`Prompter`] (the native dialog), held in wiped
//!   memory in the [`App`]; the page learns only that one is set.
//! - **Run**: [`App::create_run`] creates through the journaled [`crate::write::create_archive`],
//!   then (when asked) opens the archive and verifies it with [`lpk_api::Session::test`], and
//!   summarises the run. The recovery percent the wizard proposes is 5; the CLI's default,
//!   [`lpk_api::CreateSettings::default`], stays 0 (D-58).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use lpk_api::{
    CancelToken, Class, CreateSettings, Credentials, Monitor, OpenOptions, Seal, Session, Suite,
    Tier,
};
use serde::{Deserialize, Serialize};

use crate::app::{lock, App};
use crate::extract::Prompter;
use crate::grants::{Grant, GrantKind, Granted};
use crate::write::{archive_name, partial_path};
use crate::GuiError;

/// The recovery percent the wizard proposes (the CLI's default stays 0: D-58).
pub const PROPOSED_RECOVERY: u8 = 5;

/// The most recovery percent (the format's limit; the wizard's field).
pub const MAX_RECOVERY: u8 = 20;

/// The most input bytes a calibration runs over (the whole input when it is smaller).
pub const SAMPLE_BYTES: u64 = 16 << 20;

/// The suite of a password-protected archive (the CLI's default).
const SUITE: Suite = Suite::AesGcm;

/// A tier the wizard offers (`strong` is reserved).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TierChoice {
    /// zstd with a long window.
    Fast,
    /// LZMA or zstd per block (the wizard's default).
    Balanced,
    /// Every block stored.
    Store,
}

impl TierChoice {
    /// The engine's tier.
    pub fn tier(self) -> Tier {
        match self {
            TierChoice::Fast => Tier::Fast,
            TierChoice::Balanced => Tier::Balanced,
            TierChoice::Store => Tier::Store,
        }
    }
}

/// What the wizard starts with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePlan {
    /// The default folder: the first input's (none when no input has one).
    pub folder: Option<Granted>,
    /// The default name, without `.lpk`: the first input's stem (`archive` when none).
    pub name: String,
    /// Files in the inputs.
    pub files: u64,
    /// Their bytes.
    pub bytes: u64,
    /// The selected tier.
    pub tier: TierChoice,
    /// The proposed recovery percent.
    pub recovery: u8,
    /// The most recovery percent.
    pub max_recovery: u8,
    /// "Test when done".
    pub test: bool,
    /// "Encrypt file names" once a password is set.
    pub encrypt_names: bool,
}

/// A checked archive name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameCheck {
    /// The file name the archive gets (`.lpk` added when missing).
    pub file: String,
    /// `<file>` or its `.partial` already exists in the folder: the run would refuse.
    pub exists: bool,
}

/// What a tier would take on these inputs, scaled from a measured sample.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimate {
    /// The tier measured.
    pub tier: TierChoice,
    /// Bytes the calibration ran over.
    pub sample: u64,
    /// The inputs' bytes the figures are scaled to.
    pub total: u64,
    /// Estimated seconds for all the inputs (none when nothing was timed).
    pub seconds: Option<f64>,
    /// Estimated archive bytes (none when nothing was measured).
    pub bytes: Option<u64>,
}

/// What the page asks to create.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    /// The inputs (grants of kind `input`).
    pub inputs: Vec<Grant>,
    /// The destination folder (kind `folder`).
    pub folder: Grant,
    /// The archive name as typed.
    pub name: String,
    /// The tier.
    pub tier: TierChoice,
    /// Recovery percent, 0 to [`MAX_RECOVERY`].
    pub recovery: u8,
    /// Seal with the password the wizard set ([`App::set_password`]).
    pub encrypt: bool,
    /// With a password: encrypt the file names too (the archive does not list without it).
    pub encrypt_names: bool,
    /// Verify the archive when done.
    pub test: bool,
}

/// How the "Test when done" stage ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TestOutcome {
    /// Not asked for.
    Skipped,
    /// Every frame and chunk matched.
    Passed {
        /// Entries checked.
        entries: u64,
        /// Chunks checked.
        chunks: u64,
    },
    /// The archive did not open or verify.
    Failed {
        /// Why.
        detail: String,
    },
    /// Cancelled during the test: the archive was kept, untested.
    Cancelled,
}

/// What a create did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    /// The archive's file name.
    pub name: String,
    /// The archive's path, to show.
    pub path: String,
    /// A grant to open it (kind `archive`).
    pub archive: Granted,
    /// Entries written.
    pub entries: u64,
    /// Input bytes read.
    pub bytes_in: u64,
    /// The archive's bytes.
    pub bytes_out: u64,
    /// Seconds the create took.
    pub seconds: f64,
    /// The recovery percent used.
    pub recovery: u8,
    /// Sealed with a password.
    pub encrypted: bool,
    /// The file names are encrypted too.
    pub names_encrypted: bool,
    /// The tier.
    pub tier: TierChoice,
    /// The test stage.
    pub test: TestOutcome,
    /// The equivalent `lpk a` command line (no password).
    pub command: String,
}

/// The estimated archive size: `total` input bytes at the sample's out-to-in proportion, plus the
/// recovery records (`recovery` percent of the data).
pub fn scaled_size(total: u64, sample_out: u64, sample_in: u64, recovery: u8) -> u64 {
    let data = u128::from(total) * u128::from(sample_out) / u128::from(sample_in.max(1));
    (data * u128::from(100 + u32::from(recovery)) / 100) as u64
}

/// Quote `s` for a command line when it needs it, by the Windows rule: inside the quotes a run
/// of backslashes before a quote (an embedded one, or the closing one) is doubled, so that
/// a path ending in a backslash gets it doubled before the closing quote.
pub fn quoted(s: &str) -> String {
    if !(s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || "&()^;'\"".contains(c)))
    {
        return s.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// The `lpk a` command line that makes the same archive (the password is never part of it:
/// `--password-prompt` asks for it).
pub fn command_line(
    out: &Path,
    inputs: &[PathBuf],
    tier: TierChoice,
    recovery: u8,
    encrypted: bool,
    names_encrypted: bool,
) -> String {
    let mut c = vec![
        "lpk".to_string(),
        "a".to_string(),
        quoted(&out.display().to_string()),
    ];
    c.extend(inputs.iter().map(|p| quoted(&p.display().to_string())));
    c.push("--tier".into());
    c.push(tier.tier().name().into());
    c.push("--recovery".into());
    c.push(recovery.to_string());
    if encrypted {
        c.push("--password-prompt".into());
        if !names_encrypted {
            c.push("--listable".into());
        }
    }
    c.join(" ")
}

/// Counts the input bytes the engine reports, and passes the events on.
struct Seen {
    bytes_in: AtomicU64,
    inner: Option<Arc<dyn lpk_api::Progress>>,
}

impl lpk_api::Progress for Seen {
    fn event(&self, e: &lpk_api::Event) {
        if e.stage != lpk_api::Stage::Extract {
            self.bytes_in.fetch_max(e.bytes_in, Ordering::Relaxed);
        }
        if let Some(i) = &self.inner {
            i.event(e);
        }
    }
}

impl App {
    /// The wizard's defaults and the totals for `inputs`.
    pub fn create_plan(&self, inputs: &[Grant]) -> Result<CreatePlan, GuiError> {
        let sources = self.sources_of(inputs)?;
        let (files, bytes) = lpk_api::measure(&sources)?;
        let first = self.grants.resolve(&inputs[0], GrantKind::Input)?;
        let folder = first
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| self.grants.mint(GrantKind::Folder, p));
        let stem = if first.is_dir() {
            first.file_name()
        } else {
            first.file_stem()
        }
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| archive_name(n).is_ok());
        Ok(CreatePlan {
            folder,
            name: stem.unwrap_or_else(|| "archive".to_string()),
            files,
            bytes,
            tier: TierChoice::Balanced,
            recovery: PROPOSED_RECOVERY,
            max_recovery: MAX_RECOVERY,
            test: true,
            encrypt_names: true,
        })
    }

    /// Validate `name` and say whether it (or its partial) already exists in `folder`.
    pub fn create_name(&self, folder: &Grant, name: &str) -> Result<NameCheck, GuiError> {
        let file = archive_name(name)?;
        let out = self.grants.resolve(folder, GrantKind::Folder)?.join(&file);
        let exists = [out.clone(), partial_path(&out)]
            .iter()
            .any(|p| p.symlink_metadata().is_ok());
        Ok(NameCheck { file, exists })
    }

    /// Time and size of `tier` on `inputs`, from a calibration on this machine over at most
    /// [`SAMPLE_BYTES`], scaled to the inputs' totals (the size also by the `recovery` percent
    /// the wizard shows: recovery records are that share of the data). Ends `Cancelled` when `cancel` is set
    /// (a newer input set supersedes it).
    pub fn calibrate(
        &self,
        inputs: &[Grant],
        tier: TierChoice,
        recovery: u8,
        cancel: &CancelToken,
    ) -> Result<Estimate, GuiError> {
        let sources = self.sources_of(inputs)?;
        let (_, total) = lpk_api::measure(&sources)?;
        if cancel.is_cancelled() {
            return Err(lpk_api::Error::cancelled().into());
        }
        let settings = CreateSettings {
            tier: tier.tier(),
            ..CreateSettings::default()
        };
        let monitor = Monitor {
            progress: None,
            cancel: cancel.clone(),
        };
        let rate = lpk_api::calibrate(&sources, &settings, SAMPLE_BYTES, &monitor)?;
        if cancel.is_cancelled() {
            return Err(lpk_api::Error::cancelled().into());
        }
        let per_second = rate.bytes_per_second();
        Ok(Estimate {
            tier,
            sample: rate.bytes_in,
            total,
            seconds: (per_second > 0.0).then(|| total as f64 / per_second),
            bytes: (rate.bytes_in > 0)
                .then(|| scaled_size(total, rate.bytes_out, rate.bytes_in, recovery)),
        })
    }

    /// Ask for the new archive's password twice and keep it; true when one is set. A cancelled
    /// dialog leaves what was set before.
    pub fn set_password(&self, prompter: &dyn Prompter) -> bool {
        match prompter.new_password("") {
            Some(p) if !p.is_empty() => {
                *lock(&self.new_password) = Some(p);
                true
            }
            _ => self.has_password(),
        }
    }

    /// Whether the wizard has a password set.
    pub fn has_password(&self) -> bool {
        lock(&self.new_password).is_some()
    }

    /// Forget the password.
    pub fn clear_password(&self) {
        *lock(&self.new_password) = None;
    }

    /// Create the archive `req` asks for and, when asked, test it.
    pub fn create_run(
        &self,
        req: &CreateRequest,
        monitor: &Monitor,
    ) -> Result<CreateResult, GuiError> {
        self.create_run_hooked(req, monitor, None)
    }

    /// [`App::create_run`] with a test seam: `after_create` runs on the finished archive
    /// before the test stage.
    pub fn create_run_hooked(
        &self,
        req: &CreateRequest,
        monitor: &Monitor,
        after_create: Option<&dyn Fn(&Path)>,
    ) -> Result<CreateResult, GuiError> {
        if req.recovery > MAX_RECOVERY {
            return Err(GuiError::usage(format!(
                "recovery {}%: at most {MAX_RECOVERY}",
                req.recovery
            )));
        }
        let file = archive_name(&req.name)?;
        let folder = self.grants.resolve(&req.folder, GrantKind::Folder)?;
        let out = folder.join(&file);
        let inputs: Vec<PathBuf> = req
            .inputs
            .iter()
            .map(|g| self.grants.resolve(g, GrantKind::Input))
            .collect::<Result<_, _>>()?;
        let sources = self.sources_of(&req.inputs)?;
        let credentials = if req.encrypt {
            let pw = lock(&self.new_password).clone();
            let pw = pw.ok_or_else(|| GuiError::usage("no password is set"))?;
            Some(Credentials::password(pw.as_bytes().to_vec()))
        } else {
            None
        };
        let names_encrypted = credentials.is_some() && req.encrypt_names;
        let settings = CreateSettings {
            tier: req.tier.tier(),
            recovery: req.recovery,
            seal: credentials.clone().map(|credentials| Seal {
                suite: SUITE,
                listable: !req.encrypt_names,
                credentials,
            }),
            ..CreateSettings::default()
        };
        let seen = Arc::new(Seen {
            bytes_in: AtomicU64::new(0),
            inner: monitor.progress.clone(),
        });
        let watched = Monitor {
            progress: Some(seen.clone()),
            cancel: monitor.cancel.clone(),
        };
        let started = Instant::now();
        let summary = crate::write::create_archive(
            &sources,
            &out,
            &settings,
            &watched,
            self.journal.as_ref(),
        )?;
        let seconds = started.elapsed().as_secs_f64();
        self.record_destination(&folder);
        if credentials.is_some() {
            self.clear_password();
        }
        if let Some(hook) = after_create {
            hook(&out);
        }
        let test = if req.test {
            test_archive(&out, credentials.as_ref(), &watched)
        } else {
            TestOutcome::Skipped
        };
        Ok(CreateResult {
            name: file,
            path: out.display().to_string(),
            archive: self.grants.mint(GrantKind::Archive, &out),
            entries: summary.writer.entries,
            bytes_in: seen.bytes_in.load(Ordering::Relaxed),
            bytes_out: summary.writer.archive_len,
            seconds,
            recovery: req.recovery,
            encrypted: credentials.is_some(),
            names_encrypted,
            tier: req.tier,
            test,
            command: command_line(
                &out,
                &inputs,
                req.tier,
                req.recovery,
                credentials.is_some(),
                names_encrypted,
            ),
        })
    }
}

/// Open `out` (with `credentials`) and verify every frame and chunk.
fn test_archive(out: &Path, credentials: Option<&Credentials>, monitor: &Monitor) -> TestOutcome {
    let options = OpenOptions {
        credentials,
        ..OpenOptions::default()
    };
    let failed = |e: lpk_api::Error| TestOutcome::Failed {
        detail: e.to_string(),
    };
    let session = match Session::open(out, &options) {
        Ok(s) => s,
        Err(e) if e.class == Class::Cancelled => return TestOutcome::Cancelled,
        Err(e) => return failed(e),
    };
    let r = session.test(monitor);
    let _ = session.close();
    match r {
        Ok(s) => TestOutcome::Passed {
            entries: s.entries,
            chunks: s.chunks,
        },
        Err(e) if e.class == Class::Cancelled => TestOutcome::Cancelled,
        Err(e) => failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_follows_the_windows_rule() {
        assert_eq!(quoted("plain"), "plain");
        assert_eq!(quoted("a b"), "\"a b\"");
        assert_eq!(quoted("D:\\a b\\"), "\"D:\\a b\\\\\"");
        assert_eq!(quoted("a \"b\" c"), "\"a \\\"b\\\" c\"");
        assert_eq!(quoted("a b\\\"c"), "\"a b\\\\\\\"c\"");
        assert_eq!(quoted(""), "\"\"");
    }

    #[test]
    fn the_size_estimate_includes_the_recovery_records() {
        // Out is half of in; recovery adds its percent of the data.
        assert_eq!(scaled_size(1000, 50, 100, 0), 500);
        assert_eq!(scaled_size(1000, 50, 100, 5), 525);
        assert_eq!(scaled_size(1000, 50, 100, 20), 600);
        assert_eq!(scaled_size(1000, 0, 0, 5), 0);
    }
}
