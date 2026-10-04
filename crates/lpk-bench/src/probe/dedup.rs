//! `probe dedup` (PLAN P0-4): how much content-defined chunking and whole-file deduplication
//! save on every class and on the corpus as a whole, how the unique bytes of `backup-versions`
//! grow from version to version, and what a binary delta between consecutive versions costs
//! next to compressing the new version alone.
//!
//! Chunking: FastCDC (the crate's 2020 variant, normalization level 1) with a minimum of 4 KiB,
//! an average of 64 KiB and a maximum of 512 KiB, run on each file separately (a chunk never
//! spans two files); every chunk is identified by its BLAKE3 hash. A class row counts duplicates
//! inside the class, the corpus row across all classes. Whole-file duplicates are files whose
//! manifest BLAKE3 (verified against the bytes as the file is read) was seen before; their
//! bytes are those of every occurrence after the first.
//!
//! Timing: chunking is timed alone, single-threaded, on the file's bytes in memory, once for
//! FastCDC alone (boundaries only) and once for FastCDC plus a BLAKE3 hash of every chunk.
//!
//! Deltas: the version folders of `backup-versions` (top-level folders of the class, in manifest
//! order) are tarred in process (`tarball`), and for each consecutive pair the external `zstd`
//! (`--patch-from`, level 19, `--long=N` with N the smallest window log from 27 up that covers
//! the larger tar, one thread) and `hdiffz`/`hpatchz` (zstd level 19 inside, one thread) make a
//! patch that is then applied and compared byte by byte. A tool that is not installed, or that
//! fails, is recorded with the reason. Compared against: zstd level 19 of the new version's tar
//! alone (in process, library defaults) and zstd level 19 of the version's new unique chunks
//! (those not present in an earlier version) concatenated in file order.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use fastcdc::v2020::FastCDC;
use lpk_procstat_sys as ps;
use serde::{Deserialize, Serialize};

use super::codec::{zstd_size, ZstdSettings};
use super::tarball::tar_bytes_relative;
use super::tool::{flush_file, run_tool, tool_version, ToolSpec};
use super::{mbps, md_header, md_table, pct, timed, Ctx, Envelope, Output};

pub const NAME: &str = "dedup";

/// The class whose folders are versions.
pub const VERSIONS_CLASS: &str = "backup-versions";
/// The larger versioned class (D-43: three Godot releases); handled exactly like
/// [`VERSIONS_CLASS`], its results in `Data::versions_large`.
pub const VERSIONS_LARGE_CLASS: &str = "backup-versions-large";
/// The row label of the whole corpus (not a class name: classes are folder names).
pub const CORPUS_ROW: &str = "(corpus)";

pub const MIN_CHUNK: usize = 4 * 1024;
pub const AVG_CHUNK: usize = 64 * 1024;
pub const MAX_CHUNK: usize = 512 * 1024;
/// Version of the `fastcdc` crate this probe is built with (checked against `Cargo.lock` by a
/// test).
const FASTCDC_VERSION: &str = "5.0.0";

/// Chunker settings, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunking {
    pub algorithm: String,
    pub min_bytes: u64,
    pub avg_bytes: u64,
    pub max_bytes: u64,
    pub chunk_hash: String,
}

impl Chunking {
    fn current() -> Chunking {
        Chunking {
            algorithm: "fastcdc v2020, normalization level 1, per file".to_string(),
            min_bytes: MIN_CHUNK as u64,
            avg_bytes: AVG_CHUNK as u64,
            max_bytes: MAX_CHUNK as u64,
            chunk_hash: "blake3".to_string(),
        }
    }
}

/// Deduplication statistics of one class or of the whole corpus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    /// Class name, or [`CORPUS_ROW`].
    pub class: String,
    pub files: u64,
    pub bytes: u64,
    /// Files whose content equals that of an earlier file of the same scope.
    pub dup_files: u64,
    /// Bytes of those files.
    pub dup_bytes: u64,
    pub chunks: u64,
    pub unique_chunks: u64,
    /// Bytes of the distinct chunks (each counted once).
    pub unique_chunk_bytes: u64,
    /// Seconds of FastCDC alone, summed over the files.
    pub cdc_seconds: f64,
    /// Seconds of FastCDC plus BLAKE3 of every chunk, summed over the files.
    pub cdc_hash_seconds: f64,
}

/// What one tool did on one pair of consecutive versions. Exactly one of `skipped`, `failed`
/// and (`patch_bytes`, `create_seconds`, `apply_seconds`) is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRun {
    pub tool: String,
    /// The options used, for the table.
    pub settings: String,
    pub skipped: Option<String>,
    pub failed: Option<String>,
    pub patch_bytes: Option<u64>,
    pub create_seconds: Option<f64>,
    pub apply_seconds: Option<f64>,
    /// The patch was applied and the result equals the new version's tar byte for byte.
    pub verified: bool,
}

/// Delta of a version against the one before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delta {
    pub old_tar_bytes: u64,
    pub new_tar_bytes: u64,
    /// Zstd level 19 (library defaults, one thread) of the new version's tar alone.
    pub new_alone_zstd19_bytes: u64,
    /// Zstd level 19 of the same tar with the window log below and long-distance matching, as the
    /// patch tools use it (one thread).
    pub new_alone_zstd19_long_bytes: u64,
    /// The `--long` window log used for the patch and for the line above.
    pub window_log: u32,
    /// `2^window_log` is at least the larger tar; `false` when a tar exceeds what the command
    /// line's largest window (log 31) can cover.
    pub window_covers_input: bool,
    pub tools: Vec<ToolRun>,
}

/// One version folder of `backup-versions`, in manifest order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    /// The folder name (`group-<n>` for a private corpus).
    pub label: String,
    pub files: u64,
    pub bytes: u64,
    /// Distinct chunks of all versions up to and including this one.
    pub cumulative_unique_chunks: u64,
    pub cumulative_unique_chunk_bytes: u64,
    /// Bytes of the chunks first seen in this version.
    pub new_unique_chunk_bytes: u64,
    /// Zstd level 19 (library defaults) of those chunks concatenated in file order.
    pub new_unique_chunks_zstd19_bytes: u64,
    /// `None` for the first version, and for files at the class root (no folder to tar).
    pub delta: Option<Delta>,
}

/// The probe's own part of the result file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub chunking: Chunking,
    pub classes: Vec<Row>,
    pub corpus: Row,
    pub versions: Vec<Version>,
    /// The same for class `backup-versions-large` (absent in older result files).
    #[serde(default)]
    pub versions_large: Vec<Version>,
}

// ---------------------------------------------------------------------------------------------
// Chunk statistics

struct Piece {
    hash: [u8; 32],
    offset: usize,
    len: usize,
}

/// Chunk a file's bytes twice, timing each pass: boundaries alone, then boundaries plus BLAKE3.
fn chunk(data: &[u8]) -> (Vec<Piece>, f64, f64) {
    let (cut, cdc) = timed(|| {
        FastCDC::new(data, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK)
            .map(|c| c.length)
            .sum::<usize>()
    });
    std::hint::black_box(cut);
    let (pieces, both) = timed(|| {
        FastCDC::new(data, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK)
            .map(|c| Piece {
                hash: *blake3::hash(&data[c.offset..c.offset + c.length]).as_bytes(),
                offset: c.offset,
                len: c.length,
            })
            .collect::<Vec<_>>()
    });
    (pieces, cdc, both)
}

/// Counters of one scope (a class or the corpus).
#[derive(Default)]
struct Scope {
    files: u64,
    bytes: u64,
    dup_files: u64,
    dup_bytes: u64,
    chunks: u64,
    unique_chunk_bytes: u64,
    cdc_seconds: f64,
    cdc_hash_seconds: f64,
    seen_files: HashSet<String>,
    seen_chunks: HashSet<[u8; 32]>,
}

impl Scope {
    fn add(&mut self, file_blake3: &str, len: u64, pieces: &[Piece], cdc: f64, both: f64) {
        self.files += 1;
        self.bytes += len;
        if !self.seen_files.insert(file_blake3.to_string()) {
            self.dup_files += 1;
            self.dup_bytes += len;
        }
        for p in pieces {
            self.chunks += 1;
            if self.seen_chunks.insert(p.hash) {
                self.unique_chunk_bytes += p.len as u64;
            }
        }
        self.cdc_seconds += cdc;
        self.cdc_hash_seconds += both;
    }

    fn row(&self, class: &str) -> Row {
        Row {
            class: class.to_string(),
            files: self.files,
            bytes: self.bytes,
            dup_files: self.dup_files,
            dup_bytes: self.dup_bytes,
            chunks: self.chunks,
            unique_chunks: self.seen_chunks.len() as u64,
            unique_chunk_bytes: self.unique_chunk_bytes,
            cdc_seconds: self.cdc_seconds,
            cdc_hash_seconds: self.cdc_hash_seconds,
        }
    }
}

/// Accumulates the versions of one class in order.
#[derive(Default)]
struct VersionsAcc {
    seen: HashSet<[u8; 32]>,
    unique_bytes: u64,
    done: Vec<Version>,
    // The open version.
    label: String,
    folder: Option<String>,
    files: u64,
    bytes: u64,
    new_bytes: u64,
    new_data: Vec<u8>,
    open: bool,
}

impl VersionsAcc {
    fn start(&mut self, label: &str, folder: Option<String>) {
        self.label = label.to_string();
        self.folder = folder;
        self.files = 0;
        self.bytes = 0;
        self.new_bytes = 0;
        self.new_data.clear();
        self.open = true;
    }

    fn add(&mut self, data: &[u8], pieces: &[Piece]) {
        self.files += 1;
        self.bytes += data.len() as u64;
        for p in pieces {
            if self.seen.insert(p.hash) {
                self.new_bytes += p.len as u64;
                self.new_data
                    .extend_from_slice(&data[p.offset..p.offset + p.len]);
            }
        }
    }

    fn finish(&mut self) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        self.open = false;
        self.unique_bytes += self.new_bytes;
        let zstd = zstd_size(&self.new_data, &ZstdSettings::level19())?;
        self.done.push(Version {
            label: self.label.clone(),
            files: self.files,
            bytes: self.bytes,
            cumulative_unique_chunks: self.seen.len() as u64,
            cumulative_unique_chunk_bytes: self.unique_bytes,
            new_unique_chunk_bytes: self.new_bytes,
            new_unique_chunks_zstd19_bytes: zstd,
            delta: None,
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Delta tools

/// The first path component of a class-relative path when there is a folder above the file name.
fn version_folder(rel: &str) -> Option<String> {
    let mut parts = rel.split('/');
    let first = parts.next()?;
    parts.next().map(|_| first.to_string())
}

/// The window log for `zstd --long=N`: the smallest value from 27 up (31 at most, the largest the
/// command line accepts) whose window covers the larger of the two inputs.
pub fn long_window_log(old: u64, new: u64) -> u32 {
    let need = old.max(new).max(1);
    let mut n = 27;
    while n < 31 && (1u64 << n) < need {
        n += 1;
    }
    n
}

/// Whether a window of `2^window_log` bytes covers the larger of the two inputs.
pub fn window_covers(window_log: u32, old: u64, new: u64) -> bool {
    window_log < 64 && (1u64 << window_log) >= old.max(new)
}

// The natural folder order lives in the framework now (`Ctx::groups` sorts with it).
pub use super::natural_cmp;

/// The `X.Y.Z` token of a `zstd --version` line, for example `1.5.7`.
pub fn zstd_cli_version(line: &str) -> Option<String> {
    line.split(|c: char| c.is_whitespace() || c == ',')
        .map(|t| t.trim_start_matches('v'))
        .find(|t| {
            let parts: Vec<&str> = t.split('.').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_string)
}

pub fn zstd_create_args(old: &str, new: &str, patch: &str, window_log: u32) -> Vec<String> {
    [
        "-19".to_string(),
        format!("--long={window_log}"),
        "-T1".to_string(),
        format!("--patch-from={old}"),
        new.to_string(),
        "-o".to_string(),
        patch.to_string(),
        "-f".to_string(),
        "-q".to_string(),
    ]
    .to_vec()
}

pub fn zstd_apply_args(old: &str, patch: &str, out: &str, window_log: u32) -> Vec<String> {
    [
        "-d".to_string(),
        format!("--long={window_log}"),
        format!("--patch-from={old}"),
        patch.to_string(),
        "-o".to_string(),
        out.to_string(),
        "-f".to_string(),
        "-q".to_string(),
    ]
    .to_vec()
}

pub fn hdiffz_create_args(old: &str, new: &str, diff: &str) -> Vec<String> {
    [
        "-f".to_string(),
        "-p-1".to_string(),
        "-c-zstd-19".to_string(),
        old.to_string(),
        new.to_string(),
        diff.to_string(),
    ]
    .to_vec()
}

pub fn hpatchz_apply_args(old: &str, diff: &str, out: &str) -> Vec<String> {
    [
        "-f".to_string(),
        old.to_string(),
        diff.to_string(),
        out.to_string(),
    ]
    .to_vec()
}

/// Where the external tools are, or why they are not used.
#[derive(Debug, Clone)]
pub struct Tools {
    pub zstd: Result<PathBuf, String>,
    /// `hdiffz` and `hpatchz`; the reason names the one that is missing.
    pub hdiff: Result<(PathBuf, PathBuf), String>,
}

impl Tools {
    fn discover(ctx: &Ctx<'_>) -> Tools {
        let hdiff = match (ctx.find_tool("hdiffz"), ctx.find_tool("hpatchz")) {
            (Ok(a), Ok(b)) => Ok((a, b)),
            (Err(e), _) => Err(format!("hdiffz: {e}")),
            (_, Err(e)) => Err(format!("hpatchz: {e}")),
        };
        Tools {
            zstd: ctx.find_tool("zstd"),
            hdiff,
        }
    }
}

fn files_equal(a: &Path, b: &Path) -> Result<bool> {
    use std::io::Read;
    let mut fa = std::fs::File::open(a)?;
    let mut fb = std::fs::File::open(b)?;
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(true);
        }
        fb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

/// Create a patch, apply it, compare. `create` and `apply` are (program, arguments). Files are
/// named relative to `work`, which is the working directory of both programs.
#[allow(clippy::too_many_arguments)]
fn patch_round(
    tool: &str,
    settings: String,
    work: &Path,
    timeout: Duration,
    create: (&Path, Vec<String>),
    apply: (&Path, Vec<String>),
    patch_file: &str,
    out_file: &str,
) -> ToolRun {
    let base = ToolRun {
        tool: tool.to_string(),
        settings,
        skipped: None,
        failed: None,
        patch_bytes: None,
        create_seconds: None,
        apply_seconds: None,
        verified: false,
    };
    let fail = |mut r: ToolRun, why: String| {
        r.failed = Some(why);
        r
    };
    let err = work.join("stderr.txt");
    let step = |exe: &Path, args: &[String], what: String| {
        run_tool(
            &ToolSpec {
                exe,
                args,
                cwd: work,
                stdin: ps::Input::Null,
                stdout: ps::Output::Discard,
                stderr_file: &err,
                timeout,
            },
            &what,
        )
    };
    // No stale output may stand in for this step's; the inputs are on disk before the timed step.
    let _ = std::fs::remove_file(work.join(out_file));
    let _ = std::fs::remove_file(work.join(patch_file));
    flush_file(&work.join("old.tar"));
    flush_file(&work.join("new.tar"));
    let m = match step(create.0, &create.1, format!("{tool} create")) {
        Ok(m) => m,
        Err(f) => return fail(base, f.reason),
    };
    let patch_len = match std::fs::metadata(work.join(patch_file)) {
        Ok(md) => md.len(),
        Err(_) => return fail(base, format!("{tool} create: no patch file was written")),
    };
    let create_seconds = m.wall.as_secs_f64();
    flush_file(&work.join(patch_file));
    let m = match step(apply.0, &apply.1, format!("{tool} apply")) {
        Ok(m) => m,
        Err(f) => return fail(base, f.reason),
    };
    let apply_seconds = m.wall.as_secs_f64();
    match files_equal(&work.join("new.tar"), &work.join(out_file)) {
        Ok(true) => ToolRun {
            patch_bytes: Some(patch_len),
            create_seconds: Some(create_seconds),
            apply_seconds: Some(apply_seconds),
            verified: true,
            ..base
        },
        Ok(false) => fail(
            base,
            format!("{tool}: the patched file differs from the new version"),
        ),
        Err(_) => fail(
            base,
            format!("{tool}: the patched file could not be compared"),
        ),
    }
}

/// The delta of `new` against `old` (tar streams), with every installed tool. Files are written to
/// `work` (an existing, empty scratch directory).
pub fn delta(
    tools: &Tools,
    work: &Path,
    timeout: Duration,
    old: &[u8],
    new: &[u8],
) -> Result<Delta> {
    std::fs::write(work.join("old.tar"), old)?;
    std::fs::write(work.join("new.tar"), new)?;
    let alone = zstd_size(new, &ZstdSettings::level19())?;
    let wl = long_window_log(old.len() as u64, new.len() as u64);
    let covers = window_covers(wl, old.len() as u64, new.len() as u64);
    let alone_long = zstd_size(
        new,
        &ZstdSettings {
            level: 19,
            window_log: Some(wl),
            long_distance_matching: true,
            threads: 1,
        },
    )?;
    let mut runs = Vec::new();
    let skipped = |tool: &str, settings: &str, why: &str| ToolRun {
        tool: tool.to_string(),
        settings: settings.to_string(),
        skipped: Some(why.to_string()),
        failed: None,
        patch_bytes: None,
        create_seconds: None,
        apply_seconds: None,
        verified: false,
    };
    match &tools.zstd {
        Ok(exe) => {
            runs.push(patch_round(
                "zstd --patch-from",
                format!("level 19, --long={wl}, 1 thread"),
                work,
                timeout,
                (exe, zstd_create_args("old.tar", "new.tar", "patch.zst", wl)),
                (exe, zstd_apply_args("old.tar", "patch.zst", "out.tar", wl)),
                "patch.zst",
                "out.tar",
            ));
        }
        Err(why) => runs.push(skipped(
            "zstd --patch-from",
            "level 19, --long, 1 thread",
            why,
        )),
    }
    match &tools.hdiff {
        Ok((hdiffz, hpatchz)) => runs.push(patch_round(
            "hdiffz",
            "zstd level 19 inside, 1 thread".to_string(),
            work,
            timeout,
            (
                hdiffz,
                hdiffz_create_args("old.tar", "new.tar", "patch.diff"),
            ),
            (
                hpatchz,
                hpatchz_apply_args("old.tar", "patch.diff", "out.hdiff.tar"),
            ),
            "patch.diff",
            "out.hdiff.tar",
        )),
        Err(why) => runs.push(skipped("hdiffz", "zstd level 19 inside, 1 thread", why)),
    }
    Ok(Delta {
        old_tar_bytes: old.len() as u64,
        new_tar_bytes: new.len() as u64,
        new_alone_zstd19_bytes: alone,
        new_alone_zstd19_long_bytes: alone_long,
        window_log: wl,
        window_covers_input: covers,
        tools: runs,
    })
}

// ---------------------------------------------------------------------------------------------
// The probe

/// Fill in the deltas between consecutive versions of `class` (tars built in process).
fn deltas_of(
    ctx: &Ctx<'_>,
    class: &str,
    versions: &mut [Version],
    folders: &[Option<String>],
    tools: &Tools,
    notes: &mut Vec<String>,
) -> Result<()> {
    let mut prev_tar: Option<Vec<u8>> = None;
    for (i, version) in versions.iter_mut().enumerate() {
        let Some(folder) = folders.get(i).cloned().flatten() else {
            notes.push(format!(
                "{class} version {}: files at the class root, no folder to tar; no delta",
                i + 1
            ));
            prev_tar = None;
            continue;
        };
        let tar = tar_bytes_relative(ctx, class, &folder)?;
        if let Some(old) = &prev_tar {
            let work = ctx.scratch_dir("delta")?;
            version.delta = Some(
                delta(tools, &work, ctx.tool_timeout, old, &tar)
                    .with_context(|| format!("delta of {class} version {}", i + 1))?,
            );
        }
        prev_tar = Some(tar);
    }
    Ok(())
}

pub fn run(ctx: &Ctx<'_>) -> Result<Output<Data>> {
    let mut classes = Vec::new();
    let mut corpus = Scope::default();
    let mut versions: Vec<Version> = Vec::new();
    let mut folders: Vec<Option<String>> = Vec::new();
    let mut versions_large: Vec<Version> = Vec::new();
    let mut folders_large: Vec<Option<String>> = Vec::new();
    let mut notes = Vec::new();
    for (class, entry) in &ctx.corpus.manifest.classes {
        let mut scope = Scope::default();
        let is_versions = class == VERSIONS_CLASS || class == VERSIONS_LARGE_CLASS;
        let large = class == VERSIONS_LARGE_CLASS;
        let mut acc = VersionsAcc::default();
        let mut one = |f: &crate::corpus::manifest::ManifestFile,
                       acc: Option<&mut VersionsAcc>|
         -> Result<()> {
            let data = ctx.read_file(class, f)?;
            let (pieces, cdc, both) = chunk(&data);
            let covered: usize = pieces.iter().map(|p| p.len).sum();
            if covered != data.len() {
                bail!("class `{class}`: the chunks of a file do not cover its bytes");
            }
            scope.add(&f.blake3, f.bytes, &pieces, cdc, both);
            corpus.add(&f.blake3, f.bytes, &pieces, cdc, both);
            if let Some(a) = acc {
                a.add(&data, &pieces);
            }
            Ok(())
        };
        if is_versions {
            // Natural order of the folder names; a private corpus is then labelled by position.
            let mut groups: Vec<(Option<String>, super::Group<'_>)> = ctx
                .groups(class, 1)
                .into_iter()
                .map(|g| {
                    let folder = g
                        .files
                        .first()
                        .and_then(|f| version_folder(&ctx.rel_path(class, f)));
                    (folder, g)
                })
                .collect();
            groups.sort_by(|a, b| {
                natural_cmp(a.0.as_deref().unwrap_or(""), b.0.as_deref().unwrap_or(""))
            });
            for (n, (folder, g)) in groups.into_iter().enumerate() {
                let label = if ctx.private() {
                    format!("group-{n}")
                } else {
                    g.label.clone()
                };
                acc.start(&label, folder);
                for f in &g.files {
                    one(f, Some(&mut acc))?;
                }
                if large {
                    folders_large.push(acc.folder.clone());
                } else {
                    folders.push(acc.folder.clone());
                }
                acc.finish()?;
            }
            if large {
                versions_large = std::mem::take(&mut acc.done);
            } else {
                versions = std::mem::take(&mut acc.done);
            }
        } else {
            for f in &entry.files {
                one(f, None)?;
            }
        }
        classes.push(scope.row(class));
    }
    if ctx.class_files(VERSIONS_CLASS).is_none() {
        notes.push(format!(
            "the corpus has no class `{VERSIONS_CLASS}`: no version growth and no deltas"
        ));
    }

    // Deltas between consecutive versions.
    let tools = Tools::discover(ctx);
    let mut libs: Vec<(String, String)> = Vec::new();
    if let Ok(exe) = &tools.zstd {
        let work = ctx.scratch_dir("version-probe")?;
        if let Some(v) = tool_version(exe, &["--version"], &work) {
            libs.push((
                "zstd (command line)".to_string(),
                zstd_cli_version(&v).unwrap_or(v),
            ));
        }
    }
    if let Err(why) = &tools.zstd {
        notes.push(format!("zstd --patch-from: skipped: {why}"));
    }
    if let Err(why) = &tools.hdiff {
        notes.push(format!("hdiffz/hpatchz: skipped: {why}"));
    }
    deltas_of(
        ctx,
        VERSIONS_CLASS,
        &mut versions,
        &folders,
        &tools,
        &mut notes,
    )?;
    deltas_of(
        ctx,
        VERSIONS_LARGE_CLASS,
        &mut versions_large,
        &folders_large,
        &tools,
        &mut notes,
    )?;

    let mut out = Output::new(
        Data {
            chunking: Chunking::current(),
            classes,
            corpus: corpus.row(CORPUS_ROW),
            versions,
            versions_large,
        },
        1,
    )
    .with_zstd();
    out.libraries
        .insert("fastcdc".to_string(), FASTCDC_VERSION.to_string());
    for (k, v) in libs {
        out.libraries.insert(k, v);
    }
    out.notes = notes;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Table and rules

fn row_cells(r: &Row) -> Vec<String> {
    vec![
        r.class.clone(),
        r.files.to_string(),
        r.bytes.to_string(),
        r.dup_files.to_string(),
        r.dup_bytes.to_string(),
        pct(r.dup_bytes, r.bytes),
        r.chunks.to_string(),
        r.unique_chunk_bytes.to_string(),
        pct(r.bytes.saturating_sub(r.unique_chunk_bytes), r.bytes),
        mbps(r.bytes, r.cdc_seconds),
        mbps(r.bytes, r.cdc_hash_seconds),
    ]
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    let d = &e.data;
    let mut s = md_header(e);
    s.push_str(&format!(
        "Chunking: {}, minimum {} B, average {} B, maximum {} B, chunks identified by {}. A class \
         row dedups inside the class, the corpus row across all classes. Duplicate files are \
         files equal to an earlier one (their bytes: every copy after the first). Saved = bytes \
         not in a first-seen chunk, as a share of all bytes. Speeds are MB/s of file bytes \
         (10^6 bytes per second): FastCDC alone, and FastCDC plus BLAKE3 of every chunk, each \
         timed alone on one thread with the file in memory.\n\n",
        d.chunking.algorithm,
        d.chunking.min_bytes,
        d.chunking.avg_bytes,
        d.chunking.max_bytes,
        d.chunking.chunk_hash
    ));
    let mut rows: Vec<Vec<String>> = d.classes.iter().map(row_cells).collect();
    rows.push(row_cells(&d.corpus));
    s.push_str(&md_table(
        &[
            "class",
            "files",
            "bytes",
            "dup files",
            "dup bytes",
            "dup share",
            "chunks",
            "unique chunk bytes",
            "saved by chunk dedup",
            "FastCDC MB/s",
            "FastCDC+BLAKE3 MB/s",
        ],
        &rows,
    ));
    s.push('\n');

    if d.versions.is_empty() && d.versions_large.is_empty() {
        s.push_str(&format!(
            "No versions: the corpus has no folders in class `{VERSIONS_CLASS}`.\n"
        ));
        return s;
    }
    render_versions(&mut s, VERSIONS_CLASS, &d.versions);
    if !d.versions.is_empty() && !d.versions_large.is_empty() {
        s.push('\n');
    }
    render_versions(&mut s, VERSIONS_LARGE_CLASS, &d.versions_large);
    s
}

/// The growth table and the delta table of one versioned class (nothing when it has no version).
fn render_versions(s: &mut String, class: &str, versions: &[Version]) {
    if versions.is_empty() {
        return;
    }
    s.push_str(&format!(
        "Versions of `{class}` (top-level folders, in natural order: runs of digits \
         compare as numbers, so v2 comes before v10): unique chunk bytes after adding each version in turn, and zstd level 19 (library defaults) of the \
         chunks that are new in that version.\n\n"
    ));
    let rows: Vec<Vec<String>> = versions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            vec![
                format!("{} ({})", i + 1, v.label),
                v.files.to_string(),
                v.bytes.to_string(),
                v.cumulative_unique_chunks.to_string(),
                v.cumulative_unique_chunk_bytes.to_string(),
                v.new_unique_chunk_bytes.to_string(),
                v.new_unique_chunks_zstd19_bytes.to_string(),
            ]
        })
        .collect();
    s.push_str(&md_table(
        &[
            "version",
            "files",
            "bytes",
            "cumulative unique chunks",
            "cumulative unique chunk bytes",
            "new unique chunk bytes",
            "new unique chunks, zstd 19 bytes",
        ],
        &rows,
    ));
    s.push('\n');

    s.push_str(
        "Delta of each version against the one before, on the in-process deterministic tar of the \
         version folder (entries named relative to the folder, so identical content gives \
         identical bytes). Compared against zstd level 19 of the new tar alone, with library \
         defaults and again with the patch's window log and long-distance matching, and against \
         zstd level 19 of the new unique chunks (previous table). Every patch is applied and the \
         result compared with the new tar byte for byte; seconds are the wall time of the \
         program run, with the program's own start-up.\n\n",
    );
    let mut rows = Vec::new();
    for (i, v) in versions.iter().enumerate() {
        let Some(dl) = &v.delta else { continue };
        for t in &dl.tools {
            let mut status = match (&t.skipped, &t.failed) {
                (Some(why), _) => format!("skipped: {why}"),
                (_, Some(why)) => format!("failed: {why}"),
                _ if t.verified => "verified".to_string(),
                _ => "not verified".to_string(),
            };
            if !dl.window_covers_input {
                status.push_str("; the window did not cover the input");
            }
            rows.push(vec![
                format!("{} vs {}", i + 1, i),
                t.tool.clone(),
                t.settings.clone(),
                dl.old_tar_bytes.to_string(),
                dl.new_tar_bytes.to_string(),
                dl.new_alone_zstd19_bytes.to_string(),
                dl.new_alone_zstd19_long_bytes.to_string(),
                v.new_unique_chunks_zstd19_bytes.to_string(),
                t.patch_bytes.map_or("n/a".to_string(), |b| b.to_string()),
                t.patch_bytes.map_or("n/a".to_string(), |b| {
                    pct(b, dl.new_alone_zstd19_long_bytes)
                }),
                t.create_seconds
                    .map_or("n/a".to_string(), |x| format!("{x:.3}")),
                t.apply_seconds
                    .map_or("n/a".to_string(), |x| format!("{x:.3}")),
                status,
            ]);
        }
    }
    s.push_str(&md_table(
        &[
            "pair",
            "tool",
            "settings",
            "old tar bytes",
            "new tar bytes",
            "new alone, zstd 19 bytes",
            "new alone, zstd 19 long bytes",
            "new unique chunks, zstd 19 bytes",
            "patch bytes",
            "patch vs new alone (long)",
            "create s",
            "apply s",
            "status",
        ],
        &rows,
    ));
}

fn row_rules(r: &Row, at: &str, p: &mut Vec<String>) {
    if r.dup_files > r.files {
        p.push(format!("{at}/dup_files: more duplicate files than files"));
    }
    if r.dup_bytes > r.bytes {
        p.push(format!("{at}/dup_bytes: more duplicate bytes than bytes"));
    }
    if r.unique_chunks > r.chunks {
        p.push(format!(
            "{at}/unique_chunks: more unique chunks than chunks"
        ));
    }
    if r.unique_chunk_bytes > r.bytes {
        p.push(format!(
            "{at}/unique_chunk_bytes: unique chunk bytes exceed the total bytes"
        ));
    }
    if r.chunks == 0 && r.bytes > 0 {
        p.push(format!("{at}/chunks: bytes without chunks"));
    }
    if r.unique_chunks > 0 && r.unique_chunk_bytes == 0 {
        p.push(format!("{at}/unique_chunk_bytes: chunks without bytes"));
    }
    for (k, x) in [
        ("cdc_seconds", r.cdc_seconds),
        ("cdc_hash_seconds", r.cdc_hash_seconds),
    ] {
        if !(x.is_finite() && x >= 0.0) {
            p.push(format!("{at}/{k}: must be a non-negative number"));
        }
    }
}

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(e: &Envelope<Data>) -> Vec<String> {
    let d = &e.data;
    let mut p = Vec::new();
    if d.chunking != Chunking::current() {
        p.push("/data/chunking: not the settings of this probe".to_string());
    }
    if e.library_threads != 1 {
        p.push("/library_threads: chunking is timed on one thread".to_string());
    }
    for (i, r) in d.classes.iter().enumerate() {
        row_rules(r, &format!("/data/classes/{i}"), &mut p);
        if r.class == CORPUS_ROW {
            p.push(format!("/data/classes/{i}/class: reserved row label"));
        }
        if i > 0 && d.classes[i - 1].class >= r.class {
            p.push(format!(
                "/data/classes/{i}/class: classes must be unique and sorted"
            ));
        }
    }
    row_rules(&d.corpus, "/data/corpus", &mut p);
    let c = &d.corpus;
    if c.class != CORPUS_ROW {
        p.push(format!("/data/corpus/class: must be `{CORPUS_ROW}`"));
    }
    let sum = |f: fn(&Row) -> u64| d.classes.iter().map(f).sum::<u64>();
    let max = |f: fn(&Row) -> u64| d.classes.iter().map(f).max().unwrap_or(0);
    for (k, got, total) in [
        ("files", c.files, sum(|r| r.files)),
        ("bytes", c.bytes, sum(|r| r.bytes)),
        ("chunks", c.chunks, sum(|r| r.chunks)),
    ] {
        if got != total {
            p.push(format!(
                "/data/corpus/{k}: {got} is not the sum over the classes ({total})"
            ));
        }
    }
    // Duplicates in a class are duplicates in the corpus; sharing across classes can only add.
    for (k, got, floor) in [
        ("dup_files", c.dup_files, sum(|r| r.dup_files)),
        ("dup_bytes", c.dup_bytes, sum(|r| r.dup_bytes)),
    ] {
        if got < floor {
            p.push(format!(
                "/data/corpus/{k}: {got} is below the sum over the classes ({floor})"
            ));
        }
    }
    for (k, got, lo, hi) in [
        (
            "unique_chunks",
            c.unique_chunks,
            max(|r| r.unique_chunks),
            sum(|r| r.unique_chunks),
        ),
        (
            "unique_chunk_bytes",
            c.unique_chunk_bytes,
            max(|r| r.unique_chunk_bytes),
            sum(|r| r.unique_chunk_bytes),
        ),
    ] {
        if got < lo || got > hi {
            p.push(format!(
                "/data/corpus/{k}: {got} must lie between the largest class ({lo}) and the sum \
                 over the classes ({hi})"
            ));
        }
    }
    let secs = |f: fn(&Row) -> f64| d.classes.iter().map(f).sum::<f64>();
    for (k, got, total) in [
        ("cdc_seconds", c.cdc_seconds, secs(|r| r.cdc_seconds)),
        (
            "cdc_hash_seconds",
            c.cdc_hash_seconds,
            secs(|r| r.cdc_hash_seconds),
        ),
    ] {
        if (got - total).abs() > 1e-6 * total.max(1.0) {
            p.push(format!("/data/corpus/{k}: not the sum over the classes"));
        }
    }

    version_rules(e, VERSIONS_CLASS, "versions", &d.versions, &mut p);
    version_rules(
        e,
        VERSIONS_LARGE_CLASS,
        "versions_large",
        &d.versions_large,
        &mut p,
    );
    p
}

/// The rules of one versioned class's growth and delta records.
fn version_rules(
    e: &Envelope<Data>,
    class: &str,
    field: &str,
    versions: &[Version],
    p: &mut Vec<String>,
) {
    let d = &e.data;
    let class_row = d.classes.iter().find(|r| r.class == class);
    let mut prev_cum = 0u64;
    let mut prev_chunks = 0u64;
    for (i, v) in versions.iter().enumerate() {
        let at = format!("/data/{field}/{i}");
        if v.cumulative_unique_chunk_bytes != prev_cum + v.new_unique_chunk_bytes {
            p.push(format!(
                "{at}/cumulative_unique_chunk_bytes: not the previous cumulative plus the new bytes"
            ));
        }
        if v.cumulative_unique_chunks < prev_chunks {
            p.push(format!(
                "{at}/cumulative_unique_chunks: decreases from the previous version"
            ));
        }
        prev_cum = v.cumulative_unique_chunk_bytes;
        prev_chunks = v.cumulative_unique_chunks;
        if v.new_unique_chunk_bytes > v.bytes {
            p.push(format!(
                "{at}/new_unique_chunk_bytes: more new bytes than the version has"
            ));
        }
        if e.corpus.private && v.label != format!("group-{i}") {
            p.push(format!(
                "{at}/label: a private corpus labels versions `group-<n>`, found `{}`",
                v.label
            ));
        }
        if v.new_unique_chunk_bytes > 0 && v.new_unique_chunks_zstd19_bytes == 0 {
            p.push(format!(
                "{at}/new_unique_chunks_zstd19_bytes: zero for a version with new bytes"
            ));
        }
        match (&v.delta, i) {
            (Some(_), 0) => p.push(format!("{at}/delta: the first version has no predecessor")),
            (Some(dl), _) => {
                delta_rules(dl, &format!("{at}/delta"), p);
                if let Some(prev) = i.checked_sub(1).and_then(|j| versions[j].delta.as_ref()) {
                    if dl.old_tar_bytes != prev.new_tar_bytes {
                        p.push(format!(
                            "{at}/delta/old_tar_bytes: differs from the previous version's \
                             new_tar_bytes"
                        ));
                    }
                }
            }
            (None, _) => {}
        }
    }
    if versions.is_empty() && class_row.is_some_and(|r| r.files > 0) {
        p.push(format!(
            "/data/{field}: empty although class `{class}` has files"
        ));
    }
    match (class_row, versions.is_empty()) {
        (Some(r), false) => {
            if prev_chunks != r.unique_chunks {
                p.push(format!(
                    "/data/{field}: the last cumulative unique chunks differ from class \
                     `{class}`"
                ));
            }
            let files: u64 = versions.iter().map(|v| v.files).sum();
            let bytes: u64 = versions.iter().map(|v| v.bytes).sum();
            if files != r.files || bytes != r.bytes {
                p.push(format!(
                    "/data/{field}: files and bytes do not add up to class `{class}`"
                ));
            }
            if prev_cum != r.unique_chunk_bytes {
                p.push(format!(
                    "/data/{field}: the last cumulative unique chunk bytes differ from class \
                     `{class}`"
                ));
            }
        }
        (None, false) => p.push(format!(
            "/data/{field}: versions without a class `{class}` row"
        )),
        _ => {}
    }
}

/// The tools every delta lists, in this order.
const TOOL_NAMES: [&str; 2] = ["zstd --patch-from", "hdiffz"];

fn delta_rules(dl: &Delta, at: &str, p: &mut Vec<String>) {
    let names: Vec<&str> = dl.tools.iter().map(|t| t.tool.as_str()).collect();
    if names != TOOL_NAMES {
        p.push(format!(
            "{at}/tools: must list exactly {TOOL_NAMES:?}, found {names:?}"
        ));
    }
    if dl.window_log != long_window_log(dl.old_tar_bytes, dl.new_tar_bytes) {
        p.push(format!(
            "{at}/window_log: not the log the tar sizes call for"
        ));
    }
    if dl.window_covers_input != window_covers(dl.window_log, dl.old_tar_bytes, dl.new_tar_bytes) {
        p.push(format!(
            "{at}/window_covers_input: does not match the window log and the tar sizes"
        ));
    }
    if dl.new_tar_bytes > 0
        && (dl.new_alone_zstd19_bytes == 0 || dl.new_alone_zstd19_long_bytes == 0)
    {
        p.push(format!(
            "{at}/new_alone_zstd19_bytes: zero for a non-empty tar"
        ));
    }
    for (j, t) in dl.tools.iter().enumerate() {
        let at = format!("{at}/tools/{j}");
        let results = [
            t.patch_bytes.is_some(),
            t.create_seconds.is_some(),
            t.apply_seconds.is_some(),
        ];
        let any = results.iter().any(|b| *b);
        if results.iter().any(|b| *b) && !results.iter().all(|b| *b) {
            p.push(format!(
                "{at}: patch_bytes, create_seconds and apply_seconds go together"
            ));
        }
        let set = [t.skipped.is_some(), t.failed.is_some(), any]
            .iter()
            .filter(|b| **b)
            .count();
        if set != 1 {
            p.push(format!(
                "{at}: exactly one of skipped, failed and a result must be given"
            ));
        }
        if t.verified != any {
            p.push(format!(
                "{at}/verified: must be true exactly when a result is given"
            ));
        }
        for (k, x) in [
            ("create_seconds", t.create_seconds),
            ("apply_seconds", t.apply_seconds),
        ] {
            if x.is_some_and(|x| !(x.is_finite() && x >= 0.0)) {
                p.push(format!("{at}/{k}: must be a non-negative number"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::tests::with_ctx;
    use super::*;
    use crate::corpus::manifest::{Manifest, ManifestFile};

    /// Deterministic incompressible-looking bytes.
    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut v = Vec::with_capacity(len);
        while v.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.truncate(len);
        v
    }

    fn mf(path: &str, data: &[u8]) -> ManifestFile {
        ManifestFile {
            blake3: blake3::hash(data).to_hex().to_string(),
            bytes: data.len() as u64,
            licence: "CC0-1.0".to_string(),
            path: path.to_string(),
            source: "test".to_string(),
        }
    }

    /// Write `(path, bytes)` files (path starts with the class) and a manifest.
    fn build(tmp: &Path, files: &[(&str, Vec<u8>)]) -> PathBuf {
        let dir = tmp.join("corpus");
        let mut entries = Vec::new();
        for (path, data) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(&p, data).expect("write");
            let class = path.split('/').next().expect("class").to_string();
            entries.push((class, mf(path, data)));
        }
        let m = Manifest::with_profile_name("small", entries);
        std::fs::write(dir.join("manifest.json"), m.render()).expect("manifest");
        dir
    }

    fn no_tools() -> Tools {
        Tools {
            zstd: Err("not installed".to_string()),
            hdiff: Err("hdiffz: not installed".to_string()),
        }
    }

    fn run_on(files: &[(&str, Vec<u8>)]) -> Data {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = build(tmp.path(), files);
        let mut got = None;
        with_ctx(&dir, tmp.path(), |ctx| {
            got = Some(run(ctx).expect("run").data);
        });
        got.expect("data")
    }

    #[test]
    fn exact_copies_and_shared_chunks_are_counted() {
        let a = noise(1, 1_000_000);
        // Same bytes, different name: a whole-file duplicate with every chunk shared.
        let b = a.clone();
        // A prefix inserted: no whole-file duplicate, but most chunks are shared.
        let mut c = noise(2, 1000);
        c.extend_from_slice(&a);
        let other = noise(3, 400_000);
        let d = run_on(&[
            ("one/a.bin", a.clone()),
            ("one/b.bin", b),
            ("one/c.bin", c.clone()),
            ("two/a-again.bin", a.clone()),
            ("two/other.bin", other.clone()),
            ("two/empty.bin", Vec::new()),
        ]);
        let one = &d.classes[0];
        assert_eq!(one.class, "one");
        assert_eq!((one.files, one.dup_files), (3, 1));
        assert_eq!(one.dup_bytes, a.len() as u64);
        assert_eq!(one.bytes, (a.len() * 2 + c.len()) as u64);
        let chunks_a = chunk(&a).0.len() as u64;
        let chunks_c = chunk(&c).0.len() as u64;
        assert_eq!(one.chunks, chunks_a * 2 + chunks_c);
        // c reuses the chunks of a after the first cut points, so far fewer than all of c's are new.
        assert!(one.unique_chunks >= chunks_a);
        assert!(one.unique_chunks < chunks_a + chunks_c / 2);
        assert!(one.unique_chunk_bytes < (a.len() + c.len() / 2) as u64);
        let two = &d.classes[1];
        assert_eq!((two.files, two.dup_files, two.dup_bytes), (3, 0, 0));
        assert_eq!(two.chunks, chunks_a + chunk(&other).0.len() as u64);
        assert_eq!(two.unique_chunks, two.chunks);
        assert_eq!(two.unique_chunk_bytes, (a.len() + other.len()) as u64);
        // Across the corpus the copy in `two` is a duplicate of one in `one`.
        let all = &d.corpus;
        assert_eq!(all.class, CORPUS_ROW);
        assert_eq!(all.files, 6);
        assert_eq!(all.dup_files, 2);
        assert_eq!(all.dup_bytes, (a.len() * 2) as u64);
        assert!(all.unique_chunk_bytes < one.unique_chunk_bytes + two.unique_chunk_bytes);
        assert!(all.unique_chunk_bytes >= one.unique_chunk_bytes);
        assert_eq!(all.bytes, one.bytes + two.bytes);
        assert!(d.versions.is_empty());
    }

    #[test]
    fn chunk_sizes_respect_the_limits_and_cover_the_file() {
        let data = noise(9, 3_000_000);
        let (pieces, cdc, both) = chunk(&data);
        assert!(cdc >= 0.0 && both >= 0.0);
        assert_eq!(pieces.iter().map(|p| p.len).sum::<usize>(), data.len());
        for (i, p) in pieces.iter().enumerate() {
            assert!(p.len <= MAX_CHUNK);
            if i + 1 < pieces.len() {
                assert!(p.len >= MIN_CHUNK);
            }
            assert_eq!(
                p.hash,
                *blake3::hash(&data[p.offset..p.offset + p.len]).as_bytes()
            );
        }
        assert!(chunk(&[]).0.is_empty());
    }

    #[test]
    fn versions_grow_by_their_new_chunks_only() {
        let base = noise(5, 600_000);
        let mut v2 = base.clone();
        v2.extend_from_slice(&noise(6, 200_000));
        let mut v3 = v2.clone();
        v3.extend_from_slice(&noise(7, 100_000));
        let d = run_on(&[
            ("backup-versions/v1/data.bin", base.clone()),
            ("backup-versions/v2/data.bin", v2.clone()),
            ("backup-versions/v3/data.bin", v3.clone()),
            ("backup-versions/v3/extra.txt", b"tiny".to_vec()),
        ]);
        assert_eq!(d.versions.len(), 3);
        let v = &d.versions;
        assert_eq!(v[0].label, "v1");
        assert_eq!(v[0].new_unique_chunk_bytes, base.len() as u64);
        assert_eq!(v[0].cumulative_unique_chunk_bytes, base.len() as u64);
        // Later versions add only what is not in an earlier one.
        assert!(v[1].new_unique_chunk_bytes < 300_000);
        assert!(v[2].new_unique_chunk_bytes < 200_000);
        assert_eq!(
            v[2].cumulative_unique_chunk_bytes,
            v.iter().map(|x| x.new_unique_chunk_bytes).sum::<u64>()
        );
        assert_eq!(v[2].files, 2);
        assert_eq!(
            v[2].cumulative_unique_chunk_bytes,
            d.classes[0].unique_chunk_bytes
        );
        // No external tool is configured in the test context: deltas record the skip or run.
        assert!(v[0].delta.is_none());
        assert!(v[1].delta.is_some() && v[2].delta.is_some());
    }

    #[test]
    fn missing_tools_are_recorded_as_skipped_with_the_reason() {
        let tmp = tempfile::tempdir().expect("tmp");
        let old = noise(1, 50_000);
        let mut new = old.clone();
        new[100] ^= 1;
        let dl = delta(&no_tools(), tmp.path(), Duration::from_secs(5), &old, &new).expect("delta");
        assert_eq!(dl.tools.len(), 2);
        assert_eq!(dl.tools[0].skipped.as_deref(), Some("not installed"));
        assert_eq!(
            dl.tools[1].skipped.as_deref(),
            Some("hdiffz: not installed")
        );
        assert!(dl.tools.iter().all(|t| !t.verified && t.failed.is_none()));
        assert_eq!(dl.old_tar_bytes, 50_000);
        assert!(dl.new_alone_zstd19_bytes > 40_000);
    }

    #[test]
    fn a_tool_that_fails_is_recorded_as_failed_without_a_path() {
        let tmp = tempfile::tempdir().expect("tmp");
        let me = std::env::current_exe().expect("exe");
        let tools = Tools {
            zstd: Ok(me),
            hdiff: Err("hdiffz: not installed".to_string()),
        };
        let dl = delta(
            &tools,
            tmp.path(),
            Duration::from_secs(60),
            b"old data",
            b"new data",
        )
        .expect("delta");
        let t = &dl.tools[0];
        assert!(t.failed.as_deref().is_some_and(|f| f.contains("exit code")));
        assert!(!t.verified && t.patch_bytes.is_none());
    }

    #[test]
    fn the_real_zstd_patch_is_applied_and_verified_when_installed() {
        let local = PathBuf::from("none.toml");
        let Ok(zstd) = super::super::tool::find_tool("zstd", &local) else {
            eprintln!("zstd not installed: the real-tool test is skipped");
            return;
        };
        let tmp = tempfile::tempdir().expect("tmp");
        let old = noise(11, 300_000);
        let mut new = old.clone();
        new[1000..1010].copy_from_slice(b"0123456789");
        new.extend_from_slice(&noise(12, 5000));
        let tools = Tools {
            zstd: Ok(zstd),
            hdiff: Err("hdiffz: not installed".to_string()),
        };
        let dl = delta(&tools, tmp.path(), Duration::from_secs(120), &old, &new).expect("delta");
        let t = &dl.tools[0];
        assert!(t.verified, "{t:?}");
        assert!(t.patch_bytes.is_some_and(|b| b > 0 && b < new.len() as u64));
        assert!(t.create_seconds.is_some() && t.apply_seconds.is_some());
    }

    #[test]
    fn a_stale_patch_is_not_taken_for_the_output_of_a_tool_that_wrote_nothing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let me = std::env::current_exe().expect("exe");
        for f in ["old.tar", "new.tar", "p.bin", "o.bin"] {
            std::fs::write(tmp.path().join(f), b"stale").expect("write");
        }
        let args = vec!["--list".to_string()];
        let r = patch_round(
            "fake",
            String::new(),
            tmp.path(),
            Duration::from_secs(60),
            (&me, args.clone()),
            (&me, args),
            "p.bin",
            "o.bin",
        );
        assert!(!r.verified && r.patch_bytes.is_none(), "{r:?}");
        assert!(r
            .failed
            .as_deref()
            .is_some_and(|f| f.contains("no patch file")));
    }

    #[test]
    fn argument_construction() {
        assert_eq!(
            zstd_create_args("o", "n", "p", 27),
            [
                "-19",
                "--long=27",
                "-T1",
                "--patch-from=o",
                "n",
                "-o",
                "p",
                "-f",
                "-q"
            ]
        );
        assert_eq!(
            zstd_apply_args("o", "p", "r", 29),
            [
                "-d",
                "--long=29",
                "--patch-from=o",
                "p",
                "-o",
                "r",
                "-f",
                "-q"
            ]
        );
        assert_eq!(
            hdiffz_create_args("o", "n", "d"),
            ["-f", "-p-1", "-c-zstd-19", "o", "n", "d"]
        );
        assert_eq!(hpatchz_apply_args("o", "d", "r"), ["-f", "o", "d", "r"]);
        assert_eq!(long_window_log(0, 0), 27);
        assert_eq!(long_window_log(1 << 27, 5), 27);
        assert_eq!(long_window_log((1 << 27) + 1, 5), 28);
        assert_eq!(long_window_log(1 << 40, 5), 31);
        assert_eq!(version_folder("v1/a/b.txt").as_deref(), Some("v1"));
        assert_eq!(version_folder("a.txt"), None);
    }

    #[test]
    fn the_fastcdc_version_matches_the_lock_file() {
        let lock = include_str!("../../../../Cargo.lock");
        let want = format!("name = \"fastcdc\"\nversion = \"{FASTCDC_VERSION}\"");
        assert!(lock.replace("\r\n", "\n").contains(&want));
    }

    fn envelope(data: Data) -> Envelope<Data> {
        use super::super::{BuildProfile, CorpusId};
        Envelope {
            probe: NAME.to_string(),
            format_version: super::super::FORMAT_VERSION,
            corpus: CorpusId {
                profile: "small".to_string(),
                manifest_blake3: "0".repeat(64),
                private: false,
            },
            build: "test".to_string(),
            build_profile: BuildProfile::current(true),
            host: "test".to_string(),
            date: "2026-01-01T00:00:00Z".to_string(),
            threads: 1,
            library_threads: 1,
            libraries: [("fastcdc".to_string(), "5.0.0".to_string())].into(),
            elapsed_seconds: 1.0,
            notes: Vec::new(),
            data,
        }
    }

    #[test]
    fn the_rules_accept_a_real_result_and_reject_inconsistent_ones() {
        let base = noise(5, 300_000);
        let mut v2 = base.clone();
        v2.extend_from_slice(&noise(6, 100_000));
        let d = run_on(&[
            ("backup-versions/v1/data.bin", base.clone()),
            ("backup-versions/v2/data.bin", v2),
            ("other/x.bin", base),
        ]);
        let e = envelope(d.clone());
        assert_eq!(check(&e), Vec::<String>::new());
        let text = render(&e);
        assert!(text.contains("| backup-versions |") && text.contains("| (corpus) |"));
        assert!(text.contains("skipped") || text.contains("verified"));
        // The table is a pure function of the parsed JSON.
        let json = serde_json::to_string(&e).expect("json");
        let back: Envelope<Data> = serde_json::from_str(&json).expect("parse");
        assert_eq!(render(&back), text);

        let broken = |f: &dyn Fn(&mut Data)| {
            let mut x = d.clone();
            f(&mut x);
            check(&envelope(x))
        };
        assert!(!broken(&|x| x.classes[0].unique_chunk_bytes = x.classes[0].bytes + 1).is_empty());
        assert!(!broken(&|x| x.classes[0].dup_files = x.classes[0].files + 1).is_empty());
        assert!(!broken(&|x| x.corpus.files += 1).is_empty());
        assert!(!broken(&|x| x.corpus.unique_chunk_bytes = 0).is_empty());
        assert!(!broken(&|x| x.versions[0].cumulative_unique_chunk_bytes += 1).is_empty());
        assert!(!broken(&|x| x.versions[0].delta = x.versions[1].delta.clone()).is_empty());
        assert!(!broken(&|x| {
            if let Some(dl) = x.versions[1].delta.as_mut() {
                dl.tools[0].failed = Some("x".to_string());
                dl.tools[0].skipped = Some("y".to_string());
            }
        })
        .is_empty());
        let has = |f: &dyn Fn(&mut Data), what: &str| {
            let mut x = d.clone();
            f(&mut x);
            let found = check(&envelope(x));
            assert!(found.iter().any(|m| m.contains(what)), "{what}: {found:?}");
        };
        has(&|x| x.versions.clear(), "empty although");
        has(
            &|x| x.versions[1].cumulative_unique_chunks += 1,
            "last cumulative unique chunks",
        );
        has(
            &|x| {
                if let Some(dl) = x.versions[1].delta.as_mut() {
                    dl.tools.pop();
                }
            },
            "must list exactly",
        );
        has(
            &|x| x.versions[1].new_unique_chunks_zstd19_bytes = 0,
            "zero for a version with new bytes",
        );
        has(
            &|x| {
                let mut third = x.versions[1].clone();
                third.delta.as_mut().expect("delta").old_tar_bytes += 1;
                x.versions.push(third);
            },
            "differs from the previous version's",
        );
        has(
            &|x| {
                if let Some(dl) = x.versions[1].delta.as_mut() {
                    dl.window_covers_input = !dl.window_covers_input;
                }
            },
            "window_covers_input",
        );
    }

    #[test]
    fn long_relative_names_give_identical_tars_for_identical_versions() {
        // A relative name over 100 bytes takes the GNU long-name path of the tar writer.
        let long = format!("sub/{}.bin", "d".repeat(130));
        let a = noise(11, 30_000);
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = build(
            tmp.path(),
            &[
                (&format!("backup-versions/v1/{long}"), a.clone()),
                (&format!("backup-versions/v2/{long}"), a),
            ],
        );
        with_ctx(&dir, tmp.path(), |ctx| {
            let t1 = tar_bytes_relative(ctx, VERSIONS_CLASS, "v1").expect("v1");
            let t2 = tar_bytes_relative(ctx, VERSIONS_CLASS, "v2").expect("v2");
            assert_eq!(t1, t2);
            let mut ar = tar::Archive::new(&t1[..]);
            let names: Vec<String> = ar
                .entries()
                .expect("entries")
                .map(|e| {
                    e.expect("entry")
                        .path()
                        .expect("path")
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            assert_eq!(names, std::slice::from_ref(&long));
        });
    }

    #[test]
    fn public_versions_come_in_natural_order() {
        let d = run_on(&[
            ("backup-versions/v10/a.bin", noise(21, 5_000)),
            ("backup-versions/v2/a.bin", noise(22, 5_000)),
            ("backup-versions/v1/a.bin", noise(23, 5_000)),
        ]);
        let labels: Vec<&str> = d.versions.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(labels, ["v1", "v2", "v10"]);
    }

    #[test]
    fn identical_version_folders_give_identical_tars() {
        let a = noise(1, 70_000);
        let b = noise(2, 5_000);
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = build(
            tmp.path(),
            &[
                ("backup-versions/v1/a.bin", a.clone()),
                ("backup-versions/v1/sub/b.bin", b.clone()),
                ("backup-versions/v2/a.bin", a),
                ("backup-versions/v2/sub/b.bin", b),
            ],
        );
        with_ctx(&dir, tmp.path(), |ctx| {
            let t1 = tar_bytes_relative(ctx, VERSIONS_CLASS, "v1").expect("v1");
            let t2 = tar_bytes_relative(ctx, VERSIONS_CLASS, "v2").expect("v2");
            assert_eq!(t1, t2);
            assert!(!t1.is_empty());
        });
    }

    #[test]
    fn a_private_corpus_labels_versions_by_position_and_hides_folder_names() {
        let tmp = tempfile::tempdir().expect("tmp");
        let public = build(
            tmp.path(),
            &[
                ("backup-versions/secretfolder10/a.bin", noise(1, 30_000)),
                ("backup-versions/secretfolder2/a.bin", noise(2, 30_000)),
            ],
        );
        let private = tmp.path().join("private");
        std::fs::create_dir_all(&private).expect("mkdir");
        std::fs::copy(public.join("manifest.json"), private.join("manifest.json")).expect("copy");
        let info = serde_json::json!({"private": true, "root": public.to_string_lossy()});
        std::fs::write(private.join("build-info.json"), info.to_string()).expect("info");
        let mut data = None;
        with_ctx(&private, tmp.path(), |ctx| {
            assert!(ctx.private());
            data = Some(run(ctx).expect("run").data);
        });
        let data = data.expect("data");
        let labels: Vec<&str> = data.versions.iter().map(|v| v.label.as_str()).collect();
        assert_eq!(labels, ["group-0", "group-1"]);
        let mut e = envelope(data);
        e.corpus.private = true;
        let json = serde_json::to_string(&e).expect("json");
        assert!(!json.contains("secretfolder"));
        assert_eq!(check(&e), Vec::<String>::new());
        e.data.versions[1].label = "secretfolder10".to_string();
        assert!(check(&e).iter().any(|m| m.contains("group-<n>")));
    }

    #[test]
    fn natural_order_window_and_version_token() {
        let mut v = vec!["v10", "v2", "v1", "v02", "a9", "v1b"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a9", "v1", "v1b", "v02", "v2", "v10"]);
        assert_eq!(
            zstd_cli_version("*** Zstandard CLI (64-bit) v1.5.7, by Yann Collet ***").as_deref(),
            Some("1.5.7")
        );
        assert_eq!(zstd_cli_version("no version here"), None);
        assert!(window_covers(27, 1 << 27, 5));
        assert!(!window_covers(27, (1 << 27) + 1, 5));
        let big = 3u64 << 30;
        assert_eq!(long_window_log(big, 5), 31);
        assert!(!window_covers(31, big, 5));
    }

    #[test]
    fn the_real_hdiffz_patch_is_applied_and_verified_when_installed() {
        let local = PathBuf::from("none.toml");
        let found = (
            super::super::tool::find_tool("hdiffz", &local),
            super::super::tool::find_tool("hpatchz", &local),
        );
        let (Ok(hdiffz), Ok(hpatchz)) = found else {
            eprintln!("hdiffz/hpatchz not installed: the real-tool test is skipped");
            return;
        };
        let tmp = tempfile::tempdir().expect("tmp");
        let old = noise(21, 300_000);
        let mut new = old.clone();
        new[2000..2010].copy_from_slice(b"0123456789");
        new.extend_from_slice(&noise(22, 5000));
        let tools = Tools {
            zstd: Err("not installed".to_string()),
            hdiff: Ok((hdiffz, hpatchz)),
        };
        let dl = delta(&tools, tmp.path(), Duration::from_secs(120), &old, &new).expect("delta");
        let t = &dl.tools[1];
        assert!(t.verified, "{t:?}");
        assert!(t.patch_bytes.is_some_and(|b| b > 0));
    }

    #[test]
    fn version_labels_come_from_the_groups_and_no_name_keys_exist() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = build(
            tmp.path(),
            &[
                ("backup-versions/secret-one/a.bin", noise(1, 20_000)),
                ("backup-versions/secret-two/a.bin", noise(2, 20_000)),
            ],
        );
        with_ctx(&dir, tmp.path(), |ctx| {
            let g = ctx.groups(VERSIONS_CLASS, 1);
            assert_eq!(g.len(), 2);
            assert_eq!(g[0].label, "secret-one");
        });
        // The data type has no key that carries a folder or file name except `label`, which the
        // probe fills from `Ctx::groups`: group-<n> for private corpora.
        let json = serde_json::to_value(run_on(&[("c/x", vec![1])])).expect("json");
        let text = json.to_string();
        for k in ["\"path\"", "\"name\"", "\"folder\""] {
            assert!(!text.contains(k), "{k}");
        }
    }
}
