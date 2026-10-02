//! The measuring loop of `lpk-bench run` (PLAN P0-3): for every class x tool x setting, compress
//! and extract `repeats` times through `lpk-procstat-sys`, verify every extraction against the
//! manifest and write one result file per combination.
//!
//! Protocol (see also `bench/tools.toml` and `docs/BASELINES.md`):
//! * Before every tool x setting, outside any timing, every input file of the class is read and
//!   its BLAKE3 compared with the manifest (this verifies the input and warms the file cache); a
//!   mismatch aborts the run.
//! * Tools run without a shell, creation in the parent of the class directory (the corpus root for
//!   a private corpus) with relative paths, extraction in the combination's scratch directory
//!   with plain names; output redirected, no console, the tool-configuration environment
//!   variables of [`STRIPPED_ENV`] removed. Files a later step reads are flushed to disk between
//!   steps, outside the timed intervals.
//! * Tar-stream tools run in two sequential steps through a temporary file; the published time is
//!   the sum of the steps, the published peak memory the larger of the two.
//! * Every repeat is extracted into an empty directory and verified by BLAKE3 of every file; a
//!   missing, extra or different file fails the combination. A failed combination is recorded with
//!   the reason and the run goes on.
//! * Temporary files live under `<tmp>/<run>/` and are removed after each repeat and at the end.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use lpk_procstat_sys as ps;

use super::catalogue::{Layout, Mode, Setting, Tool};
use super::discover::{self, Discovered, Status};
use super::host;
use super::result::{
    render, CorpusRef, Failure, Measure, Measurement, PeakMemoryKind, RunCombination, RunFile,
    Sample, SettingRef, StepTimes, TarInfo, ToolRef, ToolResult, ToolsFile, Verification,
    SCHEMA_VERSION,
};
use super::validate;
use super::{load_catalogue, select, RunArgs};
use crate::corpus::manifest::{ClassEntry, Manifest, ManifestFile};

/// Environment variables that change how a catalogued tool behaves, removed from every child so
/// that a user's configuration cannot change an archive or a time. Names only are recorded.
pub const STRIPPED_ENV: &[&str] = &[
    // xz
    "XZ_OPT",
    "XZ_DEFAULTS",
    // zstd
    "ZSTD_CLEVEL",
    "ZSTD_NBTHREADS",
    // gzip
    "GZIP",
    // WinRAR / rar
    "RAR",
    // GNU tar and bsdtar
    "TAR_OPTIONS",
    "TAR_READER_OPTIONS",
    "TAR_WRITER_OPTIONS",
    "TAPE",
];

const WALL_CPU_METHOD: &str = "wall: monotonic clock around the process (lpk-procstat-sys); \
cpu: user and kernel time of the whole process tree from OS accounting";

/// Everything the loop needs besides the catalogue.
#[derive(Debug, Clone)]
pub struct Config {
    pub corpus: PathBuf,
    pub classes: Vec<String>,
    pub repeats: u32,
    pub threads: u32,
    pub timeout: Duration,
    /// A combination whose first repeat (compress plus extract wall time) takes at least this long
    /// is not repeated.
    pub long_run: Duration,
    /// BLAKE3 of the catalogue file used, recorded in `run.json`.
    pub catalogue_blake3: String,
    pub results_root: PathBuf,
    pub tmp_root: PathBuf,
    pub allow_dirty: bool,
}

/// What a run did.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub results_dir: PathBuf,
    pub measured: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Problems `validate_dir` found in the written directory.
    pub problems: usize,
}

// ---------------------------------------------------------------------------------------------
// Paths

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Absolute and lexically normalised (no file system access, no symlink resolution).
pub fn absolute(p: &Path) -> Result<PathBuf> {
    Ok(normalize(&std::path::absolute(p).with_context(|| {
        format!("making {} absolute", p.display())
    })?))
}

/// `to` relative to the directory `from`, with `/` separators. Both must be absolute and on the
/// same volume.
pub fn relative(from: &Path, to: &Path) -> Result<String> {
    let f: Vec<Component<'_>> = from.components().collect();
    let t: Vec<Component<'_>> = to.components().collect();
    let same = |a: &Component<'_>, b: &Component<'_>| match (a, b) {
        (Component::Prefix(x), Component::Prefix(y)) => {
            x.as_os_str().to_string_lossy().to_lowercase()
                == y.as_os_str().to_string_lossy().to_lowercase()
        }
        _ => a == b,
    };
    let mut common = 0;
    while common < f.len() && common < t.len() && same(&f[common], &t[common]) {
        common += 1;
    }
    if common == 0 || matches!(f.first(), Some(Component::Prefix(_))) && common < 2 {
        bail!(
            "{} and {} are on different volumes: the temporary directory must be on the same \
             volume as the corpus",
            from.display(),
            to.display()
        );
    }
    let mut parts: Vec<String> = vec!["..".to_string(); f.len() - common];
    parts.extend(
        t[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    Ok(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

fn rm_rf(path: &Path) {
    for attempt in 0..5 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if attempt < 4 => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => {}
        }
    }
}

/// Removes a directory tree when dropped, also when the run unwinds or returns an error.
struct TmpGuard(PathBuf);

impl Drop for TmpGuard {
    fn drop(&mut self) {
        rm_rf(&self.0);
        // The run directory's parent (`bench/tmp`) stays; the run directory itself must go.
    }
}

// ---------------------------------------------------------------------------------------------
// Corpus

#[derive(Debug)]
pub struct Corpus {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub manifest_blake3: String,
    /// Set for a private corpus: where the scanned files live.
    pub private_root: Option<PathBuf>,
}

pub fn load_corpus(dir: &Path) -> Result<Corpus> {
    let mpath = dir.join("manifest.json");
    let bytes = std::fs::read(&mpath).with_context(|| {
        format!(
            "reading {} (build the corpus first: `lpk-bench corpus build --profile small`)",
            mpath.display()
        )
    })?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", mpath.display()))?;
    let manifest_blake3 = blake3::hash(&bytes).to_hex().to_string();
    let mut private_root = None;
    if let Ok(text) = std::fs::read_to_string(dir.join("build-info.json")) {
        let v: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", dir.join("build-info.json").display()))?;
        if v.get("private").and_then(serde_json::Value::as_bool) == Some(true) {
            let root = v
                .get("root")
                .and_then(serde_json::Value::as_str)
                .context("build-info.json says private but has no `root`")?;
            private_root = Some(PathBuf::from(root));
        }
    }
    Ok(Corpus {
        dir: dir.to_path_buf(),
        manifest,
        manifest_blake3,
        private_root,
    })
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(path)?)?;
    Ok(h.finalize().to_hex().to_string())
}

impl Corpus {
    fn is_private(&self) -> bool {
        self.private_root.is_some()
    }

    /// Where the files named in the manifest are found.
    fn files_root(&self) -> &Path {
        self.private_root.as_deref().unwrap_or(&self.dir)
    }

    /// Read every file of a class and compare its BLAKE3 with the manifest.
    fn check_inputs(&self, class: &str, entry: &ClassEntry) -> Result<()> {
        for f in &entry.files {
            let path = self.files_root().join(&f.path);
            let got = hash_file(&path).with_context(|| {
                format!(
                    "class `{class}`: input file `{}` cannot be read (corpus changed or damaged?)",
                    f.path
                )
            })?;
            if got != f.blake3 {
                bail!(
                    "class `{class}`: input file `{}` does not match the manifest \
                     (expected {}, found {got}); rebuild the corpus",
                    f.path,
                    f.blake3
                );
            }
        }
        Ok(())
    }
}

/// Manifest-relative path of a file as it appears in an extracted tree.
fn tree_path<'a>(corpus: &Corpus, class: &str, f: &'a ManifestFile) -> &'a str {
    if corpus.is_private() {
        &f.path
    } else {
        f.path
            .strip_prefix(class)
            .and_then(|r| r.strip_prefix('/'))
            .unwrap_or(&f.path)
    }
}

// ---------------------------------------------------------------------------------------------
// Verification

#[derive(Debug, Default)]
pub struct Findings {
    pub ok: u64,
    pub missing: Vec<String>,
    pub different: Vec<String>,
    pub extra: Vec<String>,
}

impl Findings {
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.different.is_empty() && self.extra.is_empty()
    }

    /// Short text naming the first offender of every kind.
    #[cfg(test)]
    pub fn reason(&self) -> String {
        self.reason_with(None)
    }

    /// With `private_index` (tree path -> manifest position) no file name appears: expected files
    /// are named by their position in the manifest and extra files (names a tool produced) only
    /// counted.
    pub fn reason_with(&self, private_index: Option<&BTreeMap<String, usize>>) -> String {
        let mut parts = Vec::new();
        for (what, list, expected) in [
            ("missing", &self.missing, true),
            ("different", &self.different, true),
            ("extra", &self.extra, false),
        ] {
            let Some(first) = list.first() else { continue };
            parts.push(match private_index {
                None => format!("{} {what} (first: {first})", list.len()),
                Some(index) if expected => match index.get(first) {
                    Some(i) => format!("{} {what} (first: manifest file #{i})", list.len()),
                    None => format!("{} {what}", list.len()),
                },
                Some(_) => format!("{} {what}", list.len()),
            });
        }
        format!("verification: {}", parts.join("; "))
    }
}

/// Compare the files under `base` with `expected` (relative path with `/` -> BLAKE3).
pub fn verify_tree(base: &Path, expected: &BTreeMap<String, String>) -> Findings {
    let mut found = Findings::default();
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(base)
                .map(|p| {
                    p.components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .unwrap_or_default();
            if !ft.is_file() {
                found.extra.push(rel);
                continue;
            }
            match expected.get(&rel) {
                None => found.extra.push(rel),
                Some(want) => {
                    seen.insert(rel.clone());
                    match hash_file(&path) {
                        Ok(got) if &got == want => found.ok += 1,
                        _ => found.different.push(rel),
                    }
                }
            }
        }
    }
    found.missing = expected
        .keys()
        .filter(|k| !seen.contains(*k))
        .cloned()
        .collect();
    found.missing.sort();
    found.different.sort();
    found.extra.sort();
    found
}

// ---------------------------------------------------------------------------------------------
// Templates

struct Vars<'a> {
    archive: &'a str,
    input: &'a str,
    outdir: &'a str,
    list: &'a str,
    settings: &'a [String],
    threads: &'a [String],
}

fn expand(template: &[String], v: &Vars<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for arg in template {
        match arg.as_str() {
            "{settings}" => out.extend(v.settings.iter().cloned()),
            "{threads}" => out.extend(v.threads.iter().cloned()),
            _ => out.push(
                arg.replace("{archive}", v.archive)
                    .replace("{input}", v.input)
                    .replace("{outdir}", v.outdir)
                    .replace("{list}", v.list)
                    .replace("{sep}", std::path::MAIN_SEPARATOR_STR),
            ),
        }
    }
    out
}

fn thread_args(tool: &Tool, threads: u32) -> Vec<String> {
    tool.threads
        .iter()
        .map(|t| t.replace("{n}", &threads.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Steps

/// A step or verification that failed.
#[derive(Debug)]
struct Fail {
    step: &'static str,
    reason: String,
    timed_out: bool,
    descendants_killed: bool,
    verification: Option<Verification>,
}

impl Fail {
    fn new(step: &'static str, reason: impl Into<String>) -> Fail {
        Fail {
            step,
            reason: reason.into(),
            timed_out: false,
            descendants_killed: false,
            verification: None,
        }
    }
}

struct TarTool {
    exe: PathBuf,
    tool: Tool,
    version: String,
}

struct Ctx<'a> {
    cfg: &'a Config,
    corpus: &'a Corpus,
    class: &'a str,
    entry: &'a ClassEntry,
    /// Working directory of creation: parent of the class directory, or the private root.
    cwd: PathBuf,
    /// Scratch directory of the current combination (absolute) and relative to `cwd`.
    work: PathBuf,
    work_rel: String,
    tar: Option<&'a TarTool>,
}

impl<'a> Ctx<'a> {
    fn rel(&self, name: &str) -> String {
        format!("{}/{name}", self.work_rel)
    }

    /// The same combination with the scratch directory as working directory: extraction runs there,
    /// with plain relative names.
    fn in_scratch(&self) -> Ctx<'a> {
        Ctx {
            cfg: self.cfg,
            corpus: self.corpus,
            class: self.class,
            entry: self.entry,
            cwd: self.work.clone(),
            work: self.work.clone(),
            work_rel: ".".to_string(),
            tar: self.tar,
        }
    }
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn step_times(m: &ps::Measurement) -> StepTimes {
    StepTimes {
        wall_seconds: secs(m.wall),
        user_cpu_seconds: secs(m.user_cpu),
        kernel_cpu_seconds: secs(m.kernel_cpu),
    }
}

fn single_measure(m: &ps::Measurement) -> Measure {
    Measure {
        wall_seconds: secs(m.wall),
        user_cpu_seconds: secs(m.user_cpu),
        kernel_cpu_seconds: secs(m.kernel_cpu),
        peak_memory_bytes: m.peak_rss,
        timed_out: m.timed_out,
        descendants_killed: m.descendants_killed,
        peak_job_memory_bytes: m.peak_commit_job,
        peak_process_commit_bytes: m.peak_commit_process,
        tar_step: None,
        tool_step: None,
    }
}

/// Tar step plus tool step: times add, the memory peaks are the larger of the two.
fn combined_measure(tar: &ps::Measurement, tool: &ps::Measurement) -> Measure {
    let (t, c) = (step_times(tar), step_times(tool));
    let both = |a: Option<u64>, b: Option<u64>| a.zip(b).map(|(x, y)| x.max(y));
    Measure {
        wall_seconds: t.wall_seconds + c.wall_seconds,
        user_cpu_seconds: t.user_cpu_seconds + c.user_cpu_seconds,
        kernel_cpu_seconds: t.kernel_cpu_seconds + c.kernel_cpu_seconds,
        peak_memory_bytes: tar.peak_rss.max(tool.peak_rss),
        timed_out: tar.timed_out || tool.timed_out,
        descendants_killed: tar.descendants_killed || tool.descendants_killed,
        peak_job_memory_bytes: both(tar.peak_commit_job, tool.peak_commit_job),
        peak_process_commit_bytes: both(tar.peak_commit_process, tool.peak_commit_process),
        tar_step: Some(t),
        tool_step: Some(c),
    }
}

fn print_stderr_tail(file: &Path) {
    if let Ok(bytes) = std::fs::read(file) {
        let tail = &bytes[bytes.len().saturating_sub(400)..];
        let text = String::from_utf8_lossy(tail);
        let text = text.trim();
        if !text.is_empty() {
            eprintln!("    tool stderr (tail): {text}");
        }
    }
}

/// Write a file's pages to disk (outside every timed interval) so that a later timed step does
/// not pay for flushing the previous step's output. Best effort; on Windows the handle needs
/// write access.
fn flush(path: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.sync_all();
    }
}

/// Run one program as a measured step; any abnormal end is a [`Fail`].
fn run_step(
    ctx: &Ctx<'_>,
    step: &'static str,
    what: &str,
    exe: &Path,
    args: &[String],
    stdin: ps::Input,
    stdout: ps::Output,
) -> Result<ps::Measurement, Fail> {
    let err_file = ctx.work.join("stderr.txt");
    let mut spec = ps::Spec::new(exe)
        .args(args)
        .cwd(&ctx.cwd)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(ps::Output::File(err_file.clone()))
        .timeout(ctx.cfg.timeout);
    for name in STRIPPED_ENV {
        spec = spec.env_remove(*name);
    }
    let m = match ps::run(&spec) {
        Ok(m) => m,
        Err(e) => {
            return Err(Fail::new(
                step,
                format!("{what}: could not run the program ({:?})", e.kind()),
            ))
        }
    };
    let reason = if m.timed_out {
        Some(format!(
            "{what}: timed out after {} s",
            ctx.cfg.timeout.as_secs()
        ))
    } else if m.descendants_killed {
        Some(format!(
            "{what}: started processes that were still running when it exited (killed)"
        ))
    } else {
        match m.exit_code {
            Some(0) => None,
            Some(c) => Some(format!("{what}: exit code {c}")),
            None => Some(format!("{what}: ended without an exit code")),
        }
    };
    match reason {
        None => Ok(m),
        Some(reason) => {
            print_stderr_tail(&err_file);
            Err(Fail {
                step,
                reason,
                timed_out: m.timed_out,
                descendants_killed: m.descendants_killed,
                verification: None,
            })
        }
    }
}

fn archive_size(step: &'static str, path: &Path) -> Result<u64, Fail> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() > 0 => Ok(m.len()),
        Ok(m) if m.is_file() => Err(Fail::new(step, "the archive is empty")),
        _ => Err(Fail::new(step, "the archive was not created")),
    }
}

/// One repeat's compress step. Returns the measurement and the archive size.
fn compress(
    ctx: &Ctx<'_>,
    tool: &Tool,
    exe: &Path,
    setting: &Setting,
    archive: &Path,
    archive_rel: &str,
) -> Result<(Measure, u64), Fail> {
    let threads = thread_args(tool, ctx.cfg.threads);
    let list_rel = ctx.rel("list.txt");
    let vars = |archive_rel: &'_ str| -> Vec<String> {
        let v = Vars {
            archive: archive_rel,
            input: ctx.class,
            outdir: "",
            list: &list_rel,
            settings: &setting.compress,
            threads: &threads,
        };
        let template = match (&tool.create_list, ctx.corpus.is_private()) {
            (Some(l), true) => l,
            _ => &tool.create,
        };
        expand(template, &v)
    };
    match tool.mode {
        Mode::Directory => {
            let args = vars(archive_rel);
            let m = run_step(
                ctx,
                "compress",
                "compress",
                exe,
                &args,
                ps::Input::Null,
                ps::Output::Discard,
            )?;
            let bytes = archive_size("compress", archive)?;
            flush(archive);
            Ok((single_measure(&m), bytes))
        }
        Mode::TarStream => {
            let Some(tar) = ctx.tar else {
                return Err(Fail::new(
                    "compress",
                    "the store tool's tar is not available",
                ));
            };
            let stream = ctx.work.join("stream.tar");
            let v = Vars {
                archive: &ctx.rel("stream.tar"),
                input: ctx.class,
                outdir: "",
                list: "",
                settings: &[],
                threads: &[],
            };
            let tar_args = expand(&tar.tool.create, &v);
            let tar_m = run_step(
                ctx,
                "compress",
                "tar step",
                &tar.exe,
                &tar_args,
                ps::Input::Null,
                ps::Output::Discard,
            )?;
            archive_size("compress", &stream)
                .map_err(|_| Fail::new("compress", "tar step: the tar stream was not created"))?;
            flush(&stream);
            let args = vars(archive_rel);
            let tool_m = run_step(
                ctx,
                "compress",
                "tool step",
                exe,
                &args,
                ps::Input::File(stream.clone()),
                ps::Output::File(archive.to_path_buf()),
            )?;
            let _ = std::fs::remove_file(&stream);
            let bytes = archive_size("compress", archive)?;
            flush(archive);
            Ok((combined_measure(&tar_m, &tool_m), bytes))
        }
    }
}

/// Name of the archive and of the extraction directory inside the scratch directory.
const OUTDIR: &str = "x";
const UNPACKED: &str = "unpacked.tar";

/// One repeat's extract step. `ctx` is the scratch-directory context ([`Ctx::in_scratch`]):
/// `archive_name` and the extraction directory are plain names in the working directory.
fn extract(
    ctx: &Ctx<'_>,
    tool: &Tool,
    exe: &Path,
    setting: &Setting,
    archive: &Path,
    archive_name: &str,
) -> Result<Measure, Fail> {
    let threads = thread_args(tool, ctx.cfg.threads);
    match tool.mode {
        Mode::Directory => {
            let v = Vars {
                archive: archive_name,
                input: ctx.class,
                outdir: OUTDIR,
                list: "",
                settings: &setting.extract,
                threads: &threads,
            };
            let m = run_step(
                ctx,
                "extract",
                "extract",
                exe,
                &expand(&tool.extract, &v),
                ps::Input::Null,
                ps::Output::Discard,
            )?;
            Ok(single_measure(&m))
        }
        Mode::TarStream => {
            let Some(tar) = ctx.tar else {
                return Err(Fail::new(
                    "extract",
                    "the store tool's tar is not available",
                ));
            };
            let stream = ctx.work.join(UNPACKED);
            let v = Vars {
                archive: archive_name,
                input: ctx.class,
                outdir: OUTDIR,
                list: "",
                settings: &setting.extract,
                threads: &threads,
            };
            let tool_m = run_step(
                ctx,
                "extract",
                "tool step",
                exe,
                &expand(&tool.extract, &v),
                ps::Input::File(archive.to_path_buf()),
                ps::Output::File(stream.clone()),
            )?;
            flush(&stream);
            let v = Vars {
                archive: UNPACKED,
                input: ctx.class,
                outdir: OUTDIR,
                list: "",
                settings: &[],
                threads: &[],
            };
            let tar_m = run_step(
                ctx,
                "extract",
                "tar step",
                &tar.exe,
                &expand(&tar.tool.extract, &v),
                ps::Input::Null,
                ps::Output::Discard,
            )?;
            let _ = std::fs::remove_file(&stream);
            Ok(combined_measure(&tar_m, &tool_m))
        }
    }
}

/// The recorded argument lists of a combination: the arguments of the first repeat. Creation
/// paths are relative to the working directory (the parent of the class directory); extraction
/// runs in the scratch directory with plain names.
#[derive(Debug, Default, Clone)]
struct ArgRecord {
    compress: Vec<String>,
    extract: Vec<String>,
}

/// One repeat: clean up, compress, extract, verify.
fn one_repeat(
    ctx: &Ctx<'_>,
    tool: &Tool,
    exe: &Path,
    setting: &Setting,
    expected: &BTreeMap<String, String>,
    record: &mut ArgRecord,
) -> Result<Sample, Fail> {
    rm_rf(&ctx.work);
    if ctx.work.exists() {
        return Err(Fail::new(
            "compress",
            "the scratch directory could not be cleaned",
        ));
    }
    let io =
        |e: std::io::Error| Fail::new("compress", format!("scratch directory: {:?}", e.kind()));
    std::fs::create_dir_all(&ctx.work).map_err(io)?;
    let outdir = ctx.work.join(OUTDIR);
    std::fs::create_dir_all(&outdir).map_err(io)?;
    if ctx.corpus.is_private() {
        let mut list = String::new();
        for f in &ctx.entry.files {
            list.push_str(&f.path);
            list.push('\n');
        }
        std::fs::write(ctx.work.join("list.txt"), list).map_err(io)?;
    }
    let archive_name = format!("a{}", tool.extension);
    let archive = ctx.work.join(&archive_name);
    let archive_rel = ctx.rel(&archive_name);

    if record.compress.is_empty() {
        let threads = thread_args(tool, ctx.cfg.threads);
        let list_rel = ctx.rel("list.txt");
        let v = Vars {
            archive: &archive_rel,
            input: ctx.class,
            outdir: "",
            list: &list_rel,
            settings: &setting.compress,
            threads: &threads,
        };
        let template = match (&tool.create_list, ctx.corpus.is_private()) {
            (Some(l), true) => l,
            _ => &tool.create,
        };
        record.compress = expand(template, &v);
        let v = Vars {
            archive: &archive_name,
            input: ctx.class,
            outdir: OUTDIR,
            list: "",
            settings: &setting.extract,
            threads: &threads,
        };
        record.extract = expand(&tool.extract, &v);
    }

    let (compress_m, archive_bytes) = compress(ctx, tool, exe, setting, &archive, &archive_rel)?;
    let extract_m = extract(
        &ctx.in_scratch(),
        tool,
        exe,
        setting,
        &archive,
        &archive_name,
    )?;

    let base =
        if ctx.corpus.is_private() || tool.layout == Layout::Flat || tool.mode == Mode::TarStream {
            outdir
        } else {
            outdir.join(ctx.class)
        };
    let found = verify_tree(&base, expected);
    let checked = expected.len() as u64;
    if !found.is_clean() {
        // A private corpus must not leak file names (its own or a tool's) into a result file:
        // refer to files by their position in the manifest.
        let index: Option<BTreeMap<String, usize>> = ctx.corpus.is_private().then(|| {
            ctx.entry
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| (tree_path(ctx.corpus, ctx.class, f).to_string(), i))
                .collect()
        });
        let mut f = Fail::new("verify", found.reason_with(index.as_ref()));
        f.verification = Some(Verification {
            verified: false,
            files_checked: checked,
            files_ok: found.ok,
        });
        return Err(f);
    }
    Ok(Sample {
        compress: compress_m,
        extract: extract_m,
        archive_bytes,
    })
}

// ---------------------------------------------------------------------------------------------
// Combinations and results

fn corpus_ref(corpus: &Corpus, entry: &ClassEntry) -> CorpusRef {
    CorpusRef {
        profile: corpus.manifest.profile.clone(),
        manifest_blake3: corpus.manifest_blake3.clone(),
        class_files: entry.files.len() as u64,
        class_bytes: entry.files.iter().map(|f| f.bytes).sum(),
    }
}

fn base_result(
    cfg: &Config,
    corpus: &Corpus,
    entry: &ClassEntry,
    class: &str,
    d: &Discovered,
    setting: &Setting,
) -> ToolResult {
    let version = match &d.status {
        Status::Found { version, .. } => Some(version.clone()),
        Status::Skipped { .. } => None,
    };
    ToolResult {
        schema_version: SCHEMA_VERSION,
        tool: ToolRef {
            id: d.tool.id.clone(),
            version,
            mode: d.tool.mode,
            ratio_depends_on_threads: d.tool.ratio_depends_on_threads,
        },
        setting: SettingRef {
            id: setting.id.clone(),
            compress_args: setting.compress.clone(),
            extract_args: setting.extract.clone(),
        },
        class: class.to_string(),
        corpus: corpus_ref(corpus, entry),
        threads: cfg.threads,
        private: corpus.is_private(),
        repeats_requested: None,
        repeats_short: None,
        measurement: None,
        repeats: None,
        median: None,
        verification: None,
        skipped: None,
        failed: None,
    }
}

fn measurement_info(tar: Option<&TarTool>, d: &Discovered) -> Measurement {
    Measurement {
        wall_cpu_method: WALL_CPU_METHOD.to_string(),
        peak_memory_kind: if cfg!(windows) {
            PeakMemoryKind::PeakWorkingSet
        } else {
            PeakMemoryKind::MaxRss
        },
        every_repeat_verified: true,
        env_stripped: STRIPPED_ENV.iter().map(|s| s.to_string()).collect(),
        tar: (d.tool.mode == Mode::TarStream)
            .then(|| {
                tar.map(|t| TarInfo {
                    tool: t.version.clone(),
                    format: if t.version.starts_with("bsdtar") {
                        "pax-restricted".to_string()
                    } else {
                        "gnu".to_string()
                    },
                    in_published_time: true,
                })
            })
            .flatten(),
    }
}

/// Run one tool x setting x class combination.
fn run_combination(
    ctx: &Ctx<'_>,
    d: &Discovered,
    exe: &Path,
    setting: &Setting,
    expected: &BTreeMap<String, String>,
) -> ToolResult {
    let mut result = base_result(ctx.cfg, ctx.corpus, ctx.entry, ctx.class, d, setting);
    result.measurement = Some(measurement_info(ctx.tar, d));
    result.repeats_requested = Some(ctx.cfg.repeats);
    let mut record = ArgRecord::default();
    let mut samples: Vec<Sample> = Vec::new();
    let mut failure: Option<(u32, Fail)> = None;
    let mut short: Option<String> = None;
    for n in 1..=ctx.cfg.repeats {
        match one_repeat(ctx, &d.tool, exe, setting, expected, &mut record) {
            Ok(s) => {
                let first_wall = s.compress.wall_seconds + s.extract.wall_seconds;
                samples.push(s);
                if n == 1 && n < ctx.cfg.repeats && first_wall >= ctx.cfg.long_run.as_secs_f64() {
                    short = Some(format!(
                        "the first repeat's compress plus extract wall time reached --long-run-s ({} s): not repeated",
                        ctx.cfg.long_run.as_secs()
                    ));
                    break;
                }
            }
            Err(f) => {
                failure = Some((n, f));
                break;
            }
        }
    }
    if !record.compress.is_empty() {
        result.setting.compress_args = record.compress;
        result.setting.extract_args = record.extract;
    }
    match failure {
        None => {
            result.median = Some(Sample::median_of(&samples));
            result.verification = Some(Verification {
                verified: true,
                files_checked: expected.len() as u64,
                files_ok: expected.len() as u64,
            });
            result.repeats = Some(samples);
            result.repeats_short = short;
        }
        Some((n, f)) => {
            result.repeats = (!samples.is_empty()).then_some(samples);
            result.verification = f.verification;
            result.failed = Some(Failure {
                reason: f.reason,
                step: f.step.to_string(),
                repeat: n,
                timed_out: f.timed_out,
                descendants_killed: f.descendants_killed,
            });
        }
    }
    result
}

fn summary_line(r: &ToolResult) -> String {
    let id = format!("{}/{}/{}", r.tool.id, r.setting.id, r.class);
    if let Some(why) = &r.skipped {
        return format!("{id}: skipped ({why})");
    }
    if let Some(f) = &r.failed {
        return format!("{id}: FAILED repeat {} {}: {}", f.repeat, f.step, f.reason);
    }
    match (&r.median, &r.verification) {
        (Some(m), Some(v)) => {
            format!(
            "{id}: archive {} B, compress {:.3} s, extract {:.3} s, peak {:.1} MiB, verified {}/{}{}",
            m.archive_bytes,
            m.compress.wall_seconds,
            m.extract.wall_seconds,
            m.compress.peak_memory_bytes.max(m.extract.peak_memory_bytes) as f64 / 1048576.0,
            v.files_ok,
            v.files_checked,
            if r.repeats_short.is_some() { " (measured once: long run)" } else { "" }
        )
        }
        _ => format!("{id}: no data"),
    }
}

fn write_result(dir: &Path, r: &ToolResult) -> Result<()> {
    let path = dir.join(r.file_name());
    std::fs::write(&path, render(r)).with_context(|| format!("writing {}", path.display()))
}

const SKIP_PRIVATE: &str =
    "private corpus: this tool cannot take a list of files (run it on a public corpus)";
const SKIP_NON_ASCII: &str = "private corpus on Windows: the system tar cannot read non-ASCII \
file names from a list, and this class has some";

/// Windows' classic path limit (MAX_PATH minus the terminating NUL).
const WINDOWS_PATH_LIMIT: usize = 259;

/// The warning printed before a run when a path may exceed the Windows limit that some tools
/// (WinRAR) cannot get around.
pub fn path_warning(longest_input: usize, scratch: usize, extraction: usize) -> Option<String> {
    (longest_input > WINDOWS_PATH_LIMIT
        || scratch > WINDOWS_PATH_LIMIT
        || extraction > WINDOWS_PATH_LIMIT)
        .then(|| {
            format!(
                "warning: a path of this run is longer than {WINDOWS_PATH_LIMIT} characters \
                 (longest input path {longest_input}, scratch archive path {scratch}, longest \
                 extraction path {extraction}); some tools (WinRAR) fail on such paths. Use a \
                 shorter --tmp or a shorter checkout path."
            )
        })
}

/// Run everything. `discovered` is the whole catalogue; `selected` the tool ids to run.
pub fn execute(cfg: &Config, discovered: &[Discovered], selected: &[String]) -> Result<Outcome> {
    host::check_build(env!("LPK_GIT_COMMIT"), cfg.allow_dirty).map_err(anyhow::Error::msg)?;
    if cfg.repeats == 0 {
        bail!("--repeats must be at least 1");
    }
    let corpus = load_corpus(&cfg.corpus)?;
    let classes: Vec<String> = if cfg.classes.is_empty() {
        corpus.manifest.classes.keys().cloned().collect()
    } else {
        for c in &cfg.classes {
            if !corpus.manifest.classes.contains_key(c) {
                let known: Vec<&str> = corpus.manifest.classes.keys().map(String::as_str).collect();
                bail!("unknown class `{c}` (the corpus has: {})", known.join(", "));
            }
        }
        cfg.classes.clone()
    };

    // The store tool's tar serves the tar-stream tools whatever was selected.
    let tar = discovered
        .iter()
        .find(|d| d.tool.id == "store")
        .and_then(|d| match &d.status {
            Status::Found { version, path } => Some(TarTool {
                exe: path.clone(),
                tool: d.tool.clone(),
                version: version.clone(),
            }),
            Status::Skipped { .. } => None,
        });

    let host_file = host::collect(cfg.allow_dirty);
    std::fs::create_dir_all(&cfg.results_root)
        .with_context(|| format!("creating {}", cfg.results_root.display()))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let name = host::free_results_dir_name(&cfg.results_root, now, &host_file.host);
    let results_dir = cfg.results_root.join(&name);
    std::fs::create_dir_all(&results_dir)
        .with_context(|| format!("creating {}", results_dir.display()))?;
    std::fs::write(results_dir.join("host.json"), render(&host_file))?;
    std::fs::write(
        results_dir.join("tools.json"),
        render(&ToolsFile::from_discovered(discovered)),
    )?;

    // Not named after the results directory: the recorded arguments contain this path and a host
    // name would end up in them.
    let tmp_run = absolute(&cfg.tmp_root)?.join(format!("run{}", std::process::id()));
    std::fs::create_dir_all(&tmp_run).with_context(|| format!("creating {}", tmp_run.display()))?;
    let _guard = TmpGuard(tmp_run.clone());

    let mut out = Outcome {
        results_dir: results_dir.clone(),
        ..Outcome::default()
    };
    let corpus_dir = absolute(&corpus.dir)?;
    let cwd_base = match &corpus.private_root {
        Some(root) => absolute(root)?,
        None => corpus_dir.clone(),
    };
    println!(
        "results: {} ({} thread(s), {} repeat(s))",
        results_dir.display(),
        cfg.threads,
        cfg.repeats
    );
    if cfg!(windows) {
        let longest = classes
            .iter()
            .filter_map(|c| corpus.manifest.classes.get(c))
            .flat_map(|e| e.files.iter())
            .map(|f| f.path.chars().count())
            .max()
            .unwrap_or(0);
        let input = cwd_base.to_string_lossy().chars().count() + 1 + longest;
        let scratch = tmp_run.to_string_lossy().chars().count() + "/c999/a.tar.zst".len();
        // run temp directory + a combination directory + the extraction directory + the longest
        // relative path (the manifest path is an upper bound: a public class's prefix is removed).
        let extraction = tmp_run.to_string_lossy().chars().count() + "/c999/x/".len() + longest;
        if let Some(w) = path_warning(input, scratch, extraction) {
            eprintln!("{w}");
        }
    }

    let mut planned: Vec<RunCombination> = Vec::new();
    let mut combo_no = 0usize;
    for class in &classes {
        let Some(entry) = corpus.manifest.classes.get(class) else {
            continue;
        };
        if entry.files.is_empty() {
            println!("{class}: no files, skipped");
            continue;
        }
        let expected: BTreeMap<String, String> = entry
            .files
            .iter()
            .map(|f| (tree_path(&corpus, class, f).to_string(), f.blake3.clone()))
            .collect();
        let non_ascii = entry.files.iter().any(|f| !f.path.is_ascii());
        for id in selected {
            let Some(d) = discovered.iter().find(|d| &d.tool.id == id) else {
                continue;
            };
            for setting in &d.tool.settings {
                let uses_system_tar = d.tool.id == "store" || d.tool.mode == Mode::TarStream;
                let result = match &d.status {
                    Status::Skipped { reason } => {
                        let mut r = base_result(cfg, &corpus, entry, class, d, setting);
                        r.skipped = Some(reason.clone());
                        r
                    }
                    Status::Found { path, .. } => {
                        if corpus.is_private() && cfg!(windows) && non_ascii && uses_system_tar {
                            let mut r = base_result(cfg, &corpus, entry, class, d, setting);
                            r.skipped = Some(SKIP_NON_ASCII.to_string());
                            r
                        } else if corpus.is_private()
                            && (d.tool.mode == Mode::TarStream || d.tool.create_list.is_none())
                        {
                            let mut r = base_result(cfg, &corpus, entry, class, d, setting);
                            r.skipped = Some(SKIP_PRIVATE.to_string());
                            r
                        } else if d.tool.mode == Mode::TarStream && tar.is_none() {
                            let mut r = base_result(cfg, &corpus, entry, class, d, setting);
                            r.skipped =
                                Some("needs the store tool's tar, which was not found".to_string());
                            r
                        } else {
                            // Every combination starts with its inputs verified and in the file
                            // cache, outside any timing.
                            println!(
                                "{class}: checking {} input file(s) against the manifest",
                                entry.files.len()
                            );
                            corpus.check_inputs(class, entry)?;
                            combo_no += 1;
                            let work = tmp_run.join(format!("c{combo_no}"));
                            let work_rel = relative(&cwd_base, &work)?;
                            let ctx = Ctx {
                                cfg,
                                corpus: &corpus,
                                class,
                                entry,
                                cwd: cwd_base.clone(),
                                work,
                                work_rel,
                                tar: tar.as_ref(),
                            };
                            let r = run_combination(&ctx, d, path, setting, &expected);
                            rm_rf(&ctx.work);
                            r
                        }
                    }
                };
                println!("{}", summary_line(&result));
                let outcome = if result.skipped.is_some() {
                    out.skipped += 1;
                    "skipped"
                } else if result.failed.is_some() {
                    out.failed += 1;
                    "failed"
                } else {
                    out.measured += 1;
                    "measured"
                };
                planned.push(RunCombination {
                    tool: d.tool.id.clone(),
                    setting: setting.id.clone(),
                    class: class.clone(),
                    outcome: outcome.to_string(),
                });
                write_result(&results_dir, &result)?;
            }
        }
    }

    // The scanners again, after the last combination and outside all timing: a snoozed scanner
    // can resume in the middle of a run.
    let av_end = host::antivirus_products();
    let antivirus_changed = !host::av_equivalent(
        (&host_file.antivirus_source, &host_file.antivirus),
        (av_end.0, &av_end.1),
    );
    if antivirus_changed {
        eprintln!(
            "warning: the antivirus state changed during the run: at the start {}; at the end {}",
            host::av_summary(&host_file.antivirus_source, &host_file.antivirus),
            host::av_summary(av_end.0, &av_end.1)
        );
    }

    // run.json is written last: a directory without it is an aborted run and does not validate.
    let run_file = RunFile {
        schema_version: SCHEMA_VERSION,
        complete: true,
        classes: classes.clone(),
        threads: cfg.threads,
        repeats_requested: cfg.repeats,
        long_run_s: cfg.long_run.as_secs(),
        catalogue_blake3: cfg.catalogue_blake3.clone(),
        antivirus_end_source: av_end.0.to_string(),
        antivirus_changed,
        antivirus_end: av_end.1,
        combinations: planned,
    };
    std::fs::write(results_dir.join("run.json"), render(&run_file))?;

    let report = validate::validate_dir(&results_dir)?;
    for n in &report.notes {
        println!("note: {n}");
    }
    for p in &report.problems {
        eprintln!("error: {p}");
    }
    out.problems = report.problems.len();
    println!(
        "{} measured, {} failed, {} skipped; {} validation problem(s)",
        out.measured, out.failed, out.skipped, out.problems
    );
    Ok(out)
}

/// `lpk-bench run --tools ... --profile ...`.
pub fn measure_command(args: &RunArgs) -> Result<ExitCode> {
    let (cat, local) = load_catalogue(&args.catalogue, &args.local)?;
    let ids = select(&cat, &args.tools)?;
    let discovered = discover::discover_all(&cat, &local, &discover::Env::current());
    let profile = args.profile.clone().unwrap_or_else(|| "small".to_string());
    let corpus = args
        .corpus
        .clone()
        .unwrap_or_else(|| PathBuf::from("bench/corpus").join(&profile));
    let cores = host::logical_cores();
    let catalogue_blake3 = blake3::hash(
        &std::fs::read(&args.catalogue)
            .with_context(|| format!("reading {}", args.catalogue.display()))?,
    )
    .to_hex()
    .to_string();
    let cfg = Config {
        corpus,
        classes: args.classes.clone(),
        repeats: args.repeats.unwrap_or(3),
        threads: args.threads.unwrap_or(cores).max(1),
        timeout: Duration::from_secs(args.timeout_s),
        long_run: Duration::from_secs(args.long_run_s),
        catalogue_blake3,
        results_root: args.results.clone(),
        tmp_root: args.tmp.clone(),
        allow_dirty: args.allow_dirty_build,
    };
    let out = execute(&cfg, &discovered, &ids)?;
    println!("results written to {}", out.results_dir.display());
    Ok(if out.failed == 0 && out.problems == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn the_path_warning_considers_input_archive_and_extraction_paths() {
        assert!(path_warning(100, 100, 100).is_none());
        assert!(path_warning(259, 259, 259).is_none());
        for (a, b, c) in [(260, 1, 1), (1, 260, 1), (1, 1, 260)] {
            let w = path_warning(a, b, c).expect("warning");
            assert!(w.contains("shorter --tmp"), "{w}");
        }
        assert!(path_warning(1, 1, 300)
            .expect("w")
            .contains("extraction path 300"));
    }

    #[test]
    fn normalize_removes_dot_and_dotdot() {
        assert_eq!(normalize(&p("/a/./b/../c")), p("/a/c"));
        assert_eq!(normalize(&p("a/../../b")), p("../b"));
    }

    #[cfg(unix)]
    #[test]
    fn relative_paths_climb_and_descend() {
        assert_eq!(
            relative(&p("/r/bench/corpus/small"), &p("/r/bench/tmp/x/c")).expect("relative"),
            "../../tmp/x/c"
        );
        assert_eq!(
            relative(&p("/r/a"), &p("/r/a/b/c")).expect("relative"),
            "b/c"
        );
        assert_eq!(relative(&p("/r/a"), &p("/r/a")).expect("relative"), ".");
        assert_eq!(relative(&p("/a"), &p("/b")).expect("relative"), "../b");
    }

    #[cfg(windows)]
    #[test]
    fn relative_paths_climb_and_descend() {
        assert_eq!(
            relative(&p(r"C:\r\bench\corpus\small"), &p(r"C:\r\bench\tmp\x\c")).expect("relative"),
            "../../tmp/x/c"
        );
        assert_eq!(
            relative(&p(r"c:\r\a"), &p(r"C:\r\a\b\c")).expect("relative"),
            "b/c"
        );
        assert!(relative(&p(r"C:\r"), &p(r"D:\r")).is_err());
    }

    #[test]
    fn expansion_handles_whole_argument_and_embedded_placeholders() {
        let t: Vec<String> = [
            "a",
            "{settings}",
            "{threads}",
            "-o{outdir}",
            "{input}/*",
            "{archive}",
            "@{list}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let v = Vars {
            archive: "w/a.7z",
            input: "cls",
            outdir: "w/x",
            list: "w/l.txt",
            settings: &["-mx5".to_string(), "-mqs".to_string()],
            threads: &[],
        };
        assert_eq!(
            expand(&t, &v),
            ["a", "-mx5", "-mqs", "-ow/x", "cls/*", "w/a.7z", "@w/l.txt"]
        );
    }

    #[test]
    fn verification_names_missing_different_and_extra_files() {
        let dir = tempfile::tempdir().expect("tmp");
        let w = |rel: &str, data: &[u8]| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, data).expect("write");
        };
        w("a.txt", b"same");
        w("d/b.txt", b"changed");
        w("d/extra.txt", b"x");
        let h = |d: &[u8]| blake3::hash(d).to_hex().to_string();
        let expected: BTreeMap<String, String> = [
            ("a.txt", h(b"same")),
            ("d/b.txt", h(b"original")),
            ("gone.txt", h(b"gone")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let f = verify_tree(dir.path(), &expected);
        assert_eq!(f.ok, 1);
        assert_eq!(f.missing, ["gone.txt"]);
        assert_eq!(f.different, ["d/b.txt"]);
        assert_eq!(f.extra, ["d/extra.txt"]);
        let r = f.reason();
        assert!(
            r.contains("gone.txt") && r.contains("d/b.txt") && r.contains("d/extra.txt"),
            "{r}"
        );
        assert!(!f.is_clean());
        // An empty directory is not a file.
        std::fs::create_dir_all(dir.path().join("empty")).expect("mkdir");
        std::fs::remove_file(dir.path().join("d/extra.txt")).expect("rm");
        let only: BTreeMap<String, String> =
            [("a.txt".to_string(), h(b"same"))].into_iter().collect();
        std::fs::remove_dir_all(dir.path().join("d")).expect("rm");
        assert!(verify_tree(dir.path(), &only).is_clean());
    }

    #[test]
    fn combined_measures_add_times_and_take_the_larger_peak() {
        let m = |wall: u64, rss: u64| ps::Measurement {
            wall: Duration::from_millis(wall),
            user_cpu: Duration::from_millis(wall),
            kernel_cpu: Duration::from_millis(1),
            peak_rss: rss,
            peak_commit_process: Some(rss),
            peak_commit_job: None,
            exit_code: Some(0),
            timed_out: false,
            descendants_killed: false,
        };
        let c = combined_measure(&m(100, 5), &m(300, 9));
        assert_eq!(c.peak_memory_bytes, 9);
        assert_eq!(c.peak_job_memory_bytes, None);
        assert_eq!(c.peak_process_commit_bytes, Some(9));
        assert_eq!(c.tar_step.map(|s| s.wall_seconds), Some(0.1));
        assert_eq!(c.wall_seconds, 0.1 + 0.3);
    }

    #[test]
    fn the_stripped_variables_cover_the_catalogued_tools() {
        for name in [
            "XZ_OPT",
            "XZ_DEFAULTS",
            "ZSTD_CLEVEL",
            "ZSTD_NBTHREADS",
            "RAR",
            "GZIP",
            "TAR_OPTIONS",
        ] {
            assert!(STRIPPED_ENV.contains(&name), "{name}");
        }
    }
}
