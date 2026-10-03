//! Component probes (PLAN P0-4): `lpk-bench probe <name|all>`.
//!
//! ```text
//! lpk-bench probe <jpeg|deflate|dedup|text|weights|entropy-gate|all> [--profile small|full]
//!     [--corpus DIR] [--results ROOT | --into DIR] [--tmp DIR] [--threads N]
//!     [--allow-dirty-build] [--allow-debug-build]
//! ```
//!
//! Run probes from a release build: `cargo run --release -p lpk-bench -- probe ...`.
//!
//! Each probe writes `probe-<name>.json` and `probe-<name>.md` into a results directory (plus
//! `host.json` when the directory is new). The JSON is an [`Envelope`] shared by every probe with
//! the probe's own typed `data` inside; the Markdown table is a pure function of the parsed JSON
//! ([`render_file`]), so the report (PLAN P0-5) and `run --validate` produce exactly the text a
//! probe wrote. Raw quantities (bytes, seconds, counts) are stored; percentages and MB/s
//! (10^6 bytes per second) appear only when rendering.
//!
//! Shared helpers for the probes live here: class file access with input verification
//! ([`Ctx::read_file`]), an order-preserving parallel map ([`par_map`]) and a timing helper
//! ([`timed`]). Timing rule: a timed section runs alone (nothing else of the probe runs in
//! parallel), on data already in memory, and the JSON records how many threads the library used.

#![allow(dead_code)] // shared helpers for probes that land in later tasks

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::corpus::manifest::{rfc3339_utc, ManifestFile};
use crate::run::exec::{load_corpus, Corpus};
use crate::run::host;
use crate::run::result::{render, HostFile};

pub mod codec;
pub mod dedup;
pub mod deflate;
pub mod entropy_gate;
pub mod jpeg;
pub mod tarball;
pub mod text;
pub mod tool;
pub mod weights;

/// Version of the envelope and of the rules in this module.
pub const FORMAT_VERSION: u32 = 1;

/// Every probe, in the order `probe all` runs them.
pub const NAMES: [&str; 6] = [
    "jpeg",
    "deflate",
    "dedup",
    "text",
    "weights",
    "entropy-gate",
];

#[derive(Debug, Args)]
pub struct ProbeArgs {
    /// Probe to run, or `all`
    #[arg(value_parser = ["jpeg", "deflate", "dedup", "text", "weights", "entropy-gate", "all"])]
    pub name: String,
    /// Corpus profile, `small` or `full`: selects bench/corpus/<profile> (default: small)
    #[arg(long, value_parser = ["small", "full"])]
    pub profile: Option<String>,
    /// Corpus directory holding manifest.json (default: bench/corpus/<profile>)
    #[arg(long, value_name = "DIR")]
    pub corpus: Option<PathBuf>,
    /// Root under which a new `<date>-<host>[-<n>]` results directory is created
    #[arg(
        long,
        value_name = "ROOT",
        default_value = "bench/results",
        conflicts_with = "into"
    )]
    pub results: PathBuf,
    /// Add to this existing results directory instead (it must contain host.json and match this
    /// host, build and corpus)
    #[arg(long, value_name = "DIR")]
    pub into: Option<PathBuf>,
    /// Where scratch files go (default: bench/tmp, on the same volume as the corpus)
    #[arg(long, value_name = "DIR", default_value = "bench/tmp")]
    pub tmp: PathBuf,
    /// Run from a build without optimisation (recorded in every probe file; speeds from such a
    /// build are meaningless: use `cargo run --release -p lpk-bench -- probe ...`)
    #[arg(long)]
    pub allow_debug_build: bool,
    /// Threads for work where only sizes matter (default: the machine's logical cores); timed
    /// sections never run in parallel
    #[arg(long)]
    pub threads: Option<u32>,
    /// Write results from a dirty or unknown build (recorded in host.json)
    #[arg(long)]
    pub allow_dirty_build: bool,
}

// ---------------------------------------------------------------------------------------------
// The envelope

/// Which corpus a probe ran on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusId {
    pub profile: String,
    pub manifest_blake3: String,
    /// A private corpus: per-file records carry an index and no name or path.
    pub private: bool,
}

/// The file `probe-<name>.json`: the same for every probe, with the probe's own `data`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<D> {
    /// Probe name, as in the file name.
    pub probe: String,
    pub format_version: u32,
    pub corpus: CorpusId,
    /// Build stamp of lpk-bench (git commit, `-dirty` suffix when uncommitted).
    pub build: String,
    pub build_profile: BuildProfile,
    pub host: String,
    /// UTC, RFC 3339.
    pub date: String,
    /// Threads used for the parts where only sizes matter.
    pub threads: u32,
    /// Threads the compression libraries used inside timed sections.
    pub library_threads: u32,
    /// Name -> version of every compression library the probe used (linked C libraries too).
    pub libraries: BTreeMap<String, String>,
    /// Wall time of the whole probe.
    pub elapsed_seconds: f64,
    /// Parts that were skipped, and why.
    pub notes: Vec<String>,
    pub data: D,
}

/// How the binary that ran a probe was built. Probes time code of this crate, so a build
/// without optimisation gives meaningless speeds; `probe` refuses to run from one unless
/// `--allow-debug-build` is given, and the flag is recorded here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildProfile {
    /// Cargo profile name (`release`, `debug`).
    pub profile: String,
    pub opt_level: String,
    pub debug_assertions: bool,
    /// The run used `--allow-debug-build`.
    pub allow_debug_build: bool,
}

impl BuildProfile {
    pub fn current(allow_debug_build: bool) -> BuildProfile {
        BuildProfile {
            profile: env!("LPK_PROFILE").to_string(),
            opt_level: env!("LPK_OPT_LEVEL").to_string(),
            debug_assertions: cfg!(debug_assertions),
            allow_debug_build,
        }
    }

    /// An optimised build without debug assertions.
    pub fn is_release(&self) -> bool {
        self.profile == "release" && self.opt_level != "0" && !self.debug_assertions
    }
}

/// What a probe's `run` returns. Build it with [`Output::new`] and register the compression
/// libraries with [`Output::with_zstd`] / [`Output::with_xz`], so version strings come from one
/// place.
#[derive(Debug)]
pub struct Output<D> {
    pub data: D,
    pub libraries: BTreeMap<String, String>,
    pub notes: Vec<String>,
    /// Threads the compression libraries used inside timed sections (1: the calling thread).
    pub library_threads: u32,
}

impl<D> Output<D> {
    pub fn new(data: D, library_threads: u32) -> Self {
        Output {
            data,
            libraries: BTreeMap::new(),
            notes: Vec::new(),
            library_threads: library_threads.max(1),
        }
    }

    /// Record that libzstd was used.
    pub fn with_zstd(mut self) -> Self {
        self.libraries
            .insert("libzstd".to_string(), codec::zstd_version());
        self
    }

    /// Record that liblzma (xz) was used.
    pub fn with_xz(mut self) -> Self {
        self.libraries
            .insert("liblzma".to_string(), codec::xz_version());
        self
    }

    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.notes.push(text.into());
        self
    }
}

/// A set of files that share a folder, see [`Ctx::groups`].
#[derive(Debug)]
pub struct Group<'a> {
    pub label: String,
    pub files: Vec<&'a ManifestFile>,
}

/// What a probe sees of the corpus and of this run.
#[derive(Debug)]
pub struct Ctx<'a> {
    pub corpus: &'a Corpus,
    /// Threads for work where only sizes matter.
    pub threads: u32,
    /// This run's scratch directory (absolute; removed when the run ends, also on failure).
    pub scratch: PathBuf,
    /// `bench/tools.local.toml`, where programs not on `PATH` are named.
    pub local_tools: PathBuf,
    /// Timeout of one external program run.
    pub tool_timeout: Duration,
}

impl Ctx<'_> {
    pub fn private(&self) -> bool {
        self.corpus.private_root.is_some()
    }

    /// The files of a class in manifest order (sorted by path), or `None` when the corpus has no
    /// such class.
    pub fn class_files(&self, class: &str) -> Option<&[ManifestFile]> {
        self.corpus
            .manifest
            .classes
            .get(class)
            .map(|c| c.files.as_slice())
    }

    /// The manifest path of a file, or `None` for a private corpus (names stay out of results).
    pub fn label(&self, f: &ManifestFile) -> Option<String> {
        (!self.private()).then(|| f.path.clone())
    }

    /// A file's path inside its class, `/`-separated (the manifest path without the class
    /// folder for a public corpus; private manifests have no class folder).
    pub fn rel_path(&self, class: &str, f: &ManifestFile) -> String {
        f.path
            .strip_prefix(class)
            .and_then(|r| r.strip_prefix('/'))
            .unwrap_or(&f.path)
            .to_string()
    }

    /// The files of a class grouped by their folder inside the class, `depth` folder levels deep
    /// (a file with fewer levels is grouped by its own folder; depth 0: one group). Groups are
    /// sorted by folder path in natural order (runs of digits compare as numbers: `v2` before
    /// `v10`). Public corpora: the label is the folder path (`""` for the class root); private
    /// corpora: the label is `group-<n>`, `n` counting groups in that sorted order, so no folder
    /// name is published (the numbering is not stable across scans of a changing tree).
    pub fn groups(&self, class: &str, depth: usize) -> Vec<Group<'_>> {
        let mut found: Vec<(String, Vec<&ManifestFile>)> = Vec::new();
        for f in self.class_files(class).unwrap_or(&[]) {
            let rel = self.rel_path(class, f);
            let mut parts: Vec<&str> = rel.split('/').collect();
            parts.pop();
            parts.truncate(depth);
            let key = parts.join("/");
            match found.iter().position(|(k, _)| *k == key) {
                Some(i) => found[i].1.push(f),
                None => found.push((key, vec![f])),
            }
        }
        found.sort_by(|a, b| natural_cmp(&a.0, &b.0));
        found
            .into_iter()
            .enumerate()
            .map(|(n, (key, files))| Group {
                label: if self.private() {
                    format!("group-{n}")
                } else {
                    key
                },
                files,
            })
            .collect()
    }

    /// Read a class file in blocks of `block` bytes, passing each to `sink`, hashing as it goes;
    /// after the last block the size and BLAKE3 are compared with the manifest and a mismatch is
    /// an error naming the file. For files too large to hold in memory.
    pub fn read_blocks(
        &self,
        class: &str,
        f: &ManifestFile,
        block: usize,
        mut sink: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        let path = self.corpus.files_root().join(&f.path);
        let mut file = std::fs::File::open(&path).with_context(|| {
            format!(
                "class `{class}`: input file `{}` cannot be read (corpus changed or damaged?)",
                f.path
            )
        })?;
        let (total, got) = hash_blocks(&mut file, block, &mut sink)
            .with_context(|| format!("class `{class}`: reading `{}`", f.path))?;
        if total != f.bytes || got != f.blake3 {
            bail!(
                "class `{class}`: input file `{}` does not match the manifest \
                 (expected {} bytes and BLAKE3 {}, found {total} bytes and {got}); rebuild the corpus",
                f.path,
                f.bytes,
                f.blake3
            );
        }
        Ok(())
    }

    /// A scratch directory of this run (created empty, removed with the run), named `name`.
    pub fn scratch_dir(&self, name: &str) -> Result<PathBuf> {
        let dir = self.scratch.join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(dir)
    }

    /// Where an external program is: `bench/tools.local.toml` entry, then `PATH`. `Err` is the
    /// skip reason (`not installed`). Names: `zstd`, `bsc`, `kanzi`, `hdiffz`, `hpatchz`.
    pub fn find_tool(&self, name: &str) -> std::result::Result<PathBuf, String> {
        tool::find_tool(name, &self.local_tools)
    }

    /// Read a class file and verify it against the manifest (size and BLAKE3) before it is used;
    /// a mismatch is an error naming the file.
    pub fn read_file(&self, class: &str, f: &ManifestFile) -> Result<Vec<u8>> {
        let path = self.corpus.files_root().join(&f.path);
        let bytes = std::fs::read(&path).with_context(|| {
            format!(
                "class `{class}`: input file `{}` cannot be read (corpus changed or damaged?)",
                f.path
            )
        })?;
        verify_bytes(class, f, &bytes)?;
        Ok(bytes)
    }
}

/// Deliver `reader` to `sink` in blocks of exactly `block` bytes (only the last may be shorter,
/// however short the individual reads are) and return the byte count and BLAKE3 of the stream.
pub fn hash_blocks<R: std::io::Read>(
    reader: &mut R,
    block: usize,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<(u64, String)> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; block.max(1)];
    let mut total = 0u64;
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            match reader.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        if filled == 0 {
            break;
        }
        hasher.update(&buf[..filled]);
        total += filled as u64;
        sink(&buf[..filled])?;
        if filled < buf.len() {
            break;
        }
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
}

/// Order of folder names with runs of digits compared as numbers (`v2` before `v10`); ties and
/// equal numbers fall back to the plain text order.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match out.last_mut() {
                Some((d, run)) if *d == digit => run.push(c),
                _ => out.push((digit, c.to_string())),
            }
        }
        out
    }
    let (pa, pb) = (parts(a), parts(b));
    for (x, y) in pa.iter().zip(pb.iter()) {
        let ord = if x.0 && y.0 {
            let (tx, ty) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
            tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty))
        } else {
            x.1.cmp(&y.1)
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    pa.len().cmp(&pb.len()).then_with(|| a.cmp(b))
}

/// Compare file contents with their manifest entry.
pub fn verify_bytes(class: &str, f: &ManifestFile, bytes: &[u8]) -> Result<()> {
    let got = blake3::hash(bytes).to_hex().to_string();
    if bytes.len() as u64 != f.bytes || got != f.blake3 {
        bail!(
            "class `{class}`: input file `{}` does not match the manifest \
             (expected {} bytes and BLAKE3 {}, found {} bytes and {got}); rebuild the corpus",
            f.path,
            f.bytes,
            f.blake3,
            bytes.len()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Helpers for the probes

/// Map `f(index, item)` over `items` on up to `threads` scoped threads; the result keeps the
/// order of `items`. For work where only sizes matter, never for timed sections.
pub fn par_map<T, R, F>(items: &[T], threads: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, &T) -> R + Sync,
{
    let workers = threads.clamp(1, items.len().max(1));
    if workers == 1 {
        return items.iter().enumerate().map(|(i, t)| f(i, t)).collect();
    }
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                let mut mine = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    mine.push((i, f(i, item)));
                }
                if let Ok(mut all) = done.lock() {
                    all.extend(mine);
                }
            });
        }
    });
    let mut all = done.into_inner().unwrap_or_default();
    all.sort_by_key(|(i, _)| *i);
    all.into_iter().map(|(_, r)| r).collect()
}

/// Run `f` and return its value with the wall time in seconds (monotonic clock). The caller must
/// make sure nothing else of the probe runs meanwhile and that the data is already in memory.
pub fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed().as_secs_f64())
}

/// `part` as a percentage of `whole` with two decimals, or `n/a` when `whole` is zero.
pub fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        "n/a".to_string()
    } else {
        format!("{:.2}%", 100.0 * part as f64 / whole as f64)
    }
}

/// Throughput in MB/s (10^6 bytes per second), or `n/a` when no time was measured.
pub fn mbps(bytes: u64, seconds: f64) -> String {
    if seconds > 0.0 && seconds.is_finite() {
        format!("{:.1}", bytes as f64 / 1e6 / seconds)
    } else {
        "n/a".to_string()
    }
}

/// A Markdown table; `|` in a cell is escaped.
pub fn md_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let cell = |s: &str| s.replace('|', "\\|");
    let mut out = String::new();
    out.push_str(&format!(
        "| {} |\n",
        headers
            .iter()
            .map(|h| cell(h))
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    out.push_str(&format!("|{}|\n", vec!["---"; headers.len()].join("|")));
    for r in rows {
        out.push_str(&format!(
            "| {} |\n",
            r.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | ")
        ));
    }
    out
}

/// The heading and the facts every probe's Markdown starts with, all taken from the envelope.
pub fn md_header<D>(e: &Envelope<D>) -> String {
    let mut s = format!("# Probe `{}`\n\n", e.probe);
    s.push_str(&format!(
        "- corpus: `{}` (manifest BLAKE3 `{}`){}\n",
        e.corpus.profile,
        e.corpus.manifest_blake3,
        if e.corpus.private {
            ", private: true"
        } else {
            ""
        }
    ));
    let bp = &e.build_profile;
    s.push_str(&format!(
        "- build `{}` ({} profile, opt-level {}{}{}) on `{}`, {}, {} thread(s) for size-only work, \
         {} library thread(s) in timed sections, {:.1} s elapsed\n",
        e.build,
        bp.profile,
        bp.opt_level,
        if bp.debug_assertions {
            ", debug assertions"
        } else {
            ""
        },
        if bp.allow_debug_build {
            ", --allow-debug-build"
        } else {
            ""
        },
        e.host,
        e.date,
        e.threads,
        e.library_threads,
        e.elapsed_seconds
    ));
    let libs: Vec<String> = e
        .libraries
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect();
    s.push_str(&format!("- libraries: {}\n", libs.join(", ")));
    for n in &e.notes {
        s.push_str(&format!("- note: {n}\n"));
    }
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------------------------
// Per-probe dispatch (the only place that lists the probes)

/// Run one probe and return the envelope as JSON text.
fn run_one(name: &str, ctx: &Ctx<'_>, info: &RunInfo) -> Result<String> {
    let started = Instant::now();
    macro_rules! go {
        ($m:ident) => {{
            let out = $m::run(ctx)?;
            to_text(name, ctx, info, started, out)
        }};
    }
    match name {
        "jpeg" => go!(jpeg),
        "deflate" => go!(deflate),
        "dedup" => go!(dedup),
        "text" => go!(text),
        "weights" => go!(weights),
        "entropy-gate" => go!(entropy_gate),
        other => bail!("unknown probe `{other}`"),
    }
}

/// The envelope facts that do not depend on the probe.
#[derive(Debug)]
struct RunInfo {
    build: String,
    build_profile: BuildProfile,
    host: String,
    date: String,
}

fn to_text<D: Serialize>(
    name: &str,
    ctx: &Ctx<'_>,
    info: &RunInfo,
    started: Instant,
    out: Output<D>,
) -> Result<String> {
    let env = Envelope {
        probe: name.to_string(),
        format_version: FORMAT_VERSION,
        corpus: CorpusId {
            profile: ctx.corpus.manifest.profile.clone(),
            manifest_blake3: ctx.corpus.manifest_blake3.clone(),
            private: ctx.private(),
        },
        build: info.build.clone(),
        build_profile: info.build_profile.clone(),
        host: info.host.clone(),
        date: info.date.clone(),
        threads: ctx.threads,
        library_threads: out.library_threads,
        libraries: out.libraries,
        elapsed_seconds: started.elapsed().as_secs_f64(),
        notes: out.notes,
        data: out.data,
    };
    Ok(render(&env))
}

/// The Markdown for a probe's JSON text: parse it with the probe's types and render. Pure.
pub fn render_file(name: &str, json: &str) -> Result<String> {
    macro_rules! go {
        ($m:ident) => {{
            let env: Envelope<$m::Data> = serde_json::from_str(json)?;
            Ok($m::render(&env))
        }};
    }
    match name {
        "jpeg" => go!(jpeg),
        "deflate" => go!(deflate),
        "dedup" => go!(dedup),
        "text" => go!(text),
        "weights" => go!(weights),
        "entropy-gate" => go!(entropy_gate),
        other => bail!("unknown probe `{other}`"),
    }
}

// ---------------------------------------------------------------------------------------------
// Validation (called by `run --validate`)

/// What a probe file says about the run, for the agreement checks of the validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub probe: String,
    pub host: String,
    pub build: String,
    pub profile: String,
    pub manifest_blake3: String,
}

/// The problems found in one `probe-<name>.json` (each prefixed with its file name) and what the
/// file claims about host, build and corpus (when it could be read).
#[derive(Debug, Default)]
pub struct FileCheck {
    pub problems: Vec<String>,
    pub meta: Option<Meta>,
}

fn read_text(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path).map(|t| t.replace("\r\n", "\n"))
}

/// Check `<dir>/<json_name>`: typed parse (unknown fields rejected), the envelope rules, the
/// probe's own consistency rules, and `probe-<name>.md` equal to the table rendered from the
/// JSON.
pub fn check_file(dir: &Path, json_name: &str) -> FileCheck {
    let mut out = FileCheck::default();
    let text = match read_text(&dir.join(json_name)) {
        Ok(t) => t,
        Err(e) => {
            out.problems
                .push(format!("{json_name}: (root): cannot read: {e}"));
            return out;
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            out.problems
                .push(format!("{json_name}: (root): not valid JSON: {e}"));
            return out;
        }
    };
    crate::run::validate::scan_absolute_paths(&value, "", json_name, &mut out.problems);
    let data_value = value.get("data").cloned().unwrap_or(Value::Null);
    let head: Envelope<Value> = match serde_json::from_value(value) {
        Ok(h) => h,
        Err(e) => {
            out.problems.push(format!(
                "{json_name}: (root): does not fit the probe envelope: {e}"
            ));
            return out;
        }
    };
    out.meta = Some(Meta {
        probe: head.probe.clone(),
        host: head.host.clone(),
        build: head.build.clone(),
        profile: head.corpus.profile.clone(),
        manifest_blake3: head.corpus.manifest_blake3.clone(),
    });
    if head.corpus.private {
        private_names(&data_value, "/data", json_name, &mut out.problems);
    }
    let stem = json_name
        .strip_prefix("probe-")
        .and_then(|s| s.strip_suffix(".json"))
        .unwrap_or("");
    if head.probe != stem {
        out.problems.push(format!(
            "{json_name}: /probe: `{}` does not match the file name",
            head.probe
        ));
    }
    let md_name = format!("probe-{stem}.md");
    let md = read_text(&dir.join(&md_name)).ok();
    macro_rules! typed {
        ($m:ident) => {
            check_typed::<$m::Data>(
                json_name,
                &md_name,
                &text,
                md.as_deref(),
                $m::render,
                $m::check,
                &mut out.problems,
            )
        };
    }
    match stem {
        "jpeg" => typed!(jpeg),
        "deflate" => typed!(deflate),
        "dedup" => typed!(dedup),
        "text" => typed!(text),
        "weights" => typed!(weights),
        "entropy-gate" => typed!(entropy_gate),
        _ => out.problems.push(format!(
            "{json_name}: (file name): `{stem}` is not a known probe"
        )),
    }
    out
}

/// Keys that carry a file or folder name: not allowed anywhere inside `data` of a private corpus.
const NAME_KEYS: [&str; 3] = ["path", "name", "folder"];

fn private_names(v: &Value, at: &str, file: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, item) in map {
                if NAME_KEYS.contains(&k.as_str()) {
                    out.push(format!(
                        "{file}: {at}/{k}: a private corpus carries no file or folder names"
                    ));
                }
                private_names(item, &format!("{at}/{k}"), file, out);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                private_names(item, &format!("{at}/{i}"), file, out);
            }
        }
        _ => {}
    }
}

type RenderFn<D> = fn(&Envelope<D>) -> String;
type CheckFn<D> = fn(&Envelope<D>) -> Vec<String>;

fn check_typed<D: DeserializeOwned>(
    json_name: &str,
    md_name: &str,
    text: &str,
    md: Option<&str>,
    render_fn: RenderFn<D>,
    check_fn: CheckFn<D>,
    problems: &mut Vec<String>,
) {
    let env: Envelope<D> = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => {
            problems.push(format!(
                "{json_name}: (root): does not fit the probe types: {e}"
            ));
            return;
        }
    };
    for p in check_common(&env) {
        problems.push(format!("{json_name}: {p}"));
    }
    for p in check_fn(&env) {
        problems.push(format!("{json_name}: {p}"));
    }
    match md {
        None => problems.push(format!(
            "{md_name}: (file): missing (the table is written next to the JSON)"
        )),
        Some(m) if m != render_fn(&env) => problems.push(format!(
            "{md_name}: (file): does not equal the table rendered from {json_name}"
        )),
        Some(_) => {}
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` with a real calendar date and a real time of day.
pub fn valid_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let shape = b.len() == 20
        && s.is_ascii()
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z';
    if !shape {
        return false;
    }
    let field = |from: usize, to: usize| -> Option<u32> {
        let t = &s[from..to];
        if t.bytes().all(|c| c.is_ascii_digit()) {
            t.parse().ok()
        } else {
            None
        }
    };
    let (Some(h), Some(mi), Some(sec)) = (field(11, 13), field(14, 16), field(17, 19)) else {
        return false;
    };
    h < 24 && mi < 60 && sec < 60 && crate::run::validate::valid_date(&s[..10])
}

/// The rules every envelope must satisfy.
pub fn check_common<D>(e: &Envelope<D>) -> Vec<String> {
    let mut p = Vec::new();
    if e.format_version != FORMAT_VERSION {
        p.push(format!(
            "/format_version: {} (this build reads {FORMAT_VERSION})",
            e.format_version
        ));
    }
    if e.threads == 0 {
        p.push("/threads: must be at least 1".to_string());
    }
    if !(e.elapsed_seconds.is_finite() && e.elapsed_seconds >= 0.0) {
        p.push("/elapsed_seconds: must be a non-negative number".to_string());
    }
    if e.libraries.is_empty() {
        p.push("/libraries: a probe names every library it used".to_string());
    }
    let hash_ok = e.corpus.manifest_blake3.len() == 64
        && e.corpus
            .manifest_blake3
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hash_ok {
        p.push("/corpus/manifest_blake3: must be 64 lower-case hex digits".to_string());
    }
    if !valid_timestamp(&e.date) {
        p.push("/date: must be a real UTC time, YYYY-MM-DDTHH:MM:SSZ".to_string());
    }
    if e.library_threads == 0 {
        p.push("/library_threads: must be at least 1".to_string());
    }
    if !e.build_profile.is_release() && !e.build_profile.allow_debug_build {
        p.push(
            "/build_profile: not an optimised release build and --allow-debug-build is not \
             recorded: the timings mean nothing"
                .to_string(),
        );
    }
    if e.host != host::sanitize_host(&e.host) {
        p.push("/host: must be lower-case [a-z0-9-]".to_string());
    }
    p
}

// ---------------------------------------------------------------------------------------------
// The command

/// Everything `execute` needs.
#[derive(Debug, Clone)]
pub struct Config {
    pub probes: Vec<String>,
    pub corpus: PathBuf,
    /// Add to this existing results directory (it must hold `host.json`); `None`: a new
    /// directory under `results_root`.
    pub into: Option<PathBuf>,
    pub results_root: PathBuf,
    /// Scratch root; a per-run directory is created under it and removed at the end.
    pub tmp_root: PathBuf,
    pub threads: u32,
    pub allow_dirty: bool,
    pub allow_debug_build: bool,
    /// `bench/tools.local.toml`
    pub local_tools: PathBuf,
    pub tool_timeout: Duration,
}

/// What a run did.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub results_dir: PathBuf,
    pub written: Vec<String>,
    /// Probe name and the error that stopped it.
    pub failed: Vec<(String, String)>,
    /// Problems `validate_dir` found in the directory afterwards.
    pub problems: usize,
}

fn same_machine(a: &HostFile, b: &HostFile) -> bool {
    a.host == b.host
        && a.os == b.os
        && a.cpu_model == b.cpu_model
        && a.logical_cores == b.logical_cores
}

/// The (file, profile, manifest hash) of every result or probe file already in `dir`.
fn existing_corpora(dir: &Path) -> Result<Vec<(String, String, String)>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json")
            || ["host.json", "tools.json", "run.json"].contains(&name.as_str())
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let get = |p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_string);
        if let (Some(profile), Some(hash)) =
            (get("/corpus/profile"), get("/corpus/manifest_blake3"))
        {
            found.push((name, profile, hash));
        }
    }
    found.sort();
    Ok(found)
}

/// Decide where results go and make sure the directory has a `host.json`. A new directory (under
/// the results root) gets one, written by the same code as the baseline runner. An existing one
/// (`--into`) must hold a `host.json` of the same machine and build and only files of the same
/// corpus; its `host.json` is left alone.
fn prepare_dir(cfg: &Config, corpus: &Corpus, now: u64) -> Result<PathBuf> {
    let current = host::collect(cfg.allow_dirty);
    let Some(dir) = &cfg.into else {
        std::fs::create_dir_all(&cfg.results_root)
            .with_context(|| format!("creating {}", cfg.results_root.display()))?;
        let dir = cfg.results_root.join(host::free_results_dir_name(
            &cfg.results_root,
            now,
            &current.host,
        ));
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(dir.join("host.json"), render(&current))
            .with_context(|| format!("writing {}", dir.join("host.json").display()))?;
        return Ok(dir);
    };
    let host_path = dir.join("host.json");
    let text = std::fs::read_to_string(&host_path).with_context(|| {
        format!(
            "{} has no host.json: --into needs an existing results directory",
            dir.display()
        )
    })?;
    let existing: HostFile = serde_json::from_str(&text)
        .with_context(|| format!("{} does not parse", host_path.display()))?;
    if !same_machine(&existing, &current) {
        bail!(
            "{} was made on a different machine (host.json says `{}`, this is `{}`): refusing to \
             mix results",
            dir.display(),
            existing.host,
            current.host
        );
    }
    if existing.git_commit != current.git_commit {
        bail!(
            "{} was made by build `{}`, this is build `{}`: one build per results directory",
            dir.display(),
            existing.git_commit,
            current.git_commit
        );
    }
    for (file, profile, hash) in existing_corpora(dir)? {
        if profile != corpus.manifest.profile || hash != corpus.manifest_blake3 {
            bail!(
                "{} holds {file} from another corpus (profile `{profile}`, manifest {hash}); \
                 this run uses `{}` ({}): refusing to mix results",
                dir.display(),
                corpus.manifest.profile,
                corpus.manifest_blake3
            );
        }
    }
    Ok(dir.clone())
}

/// Removes the run's scratch directory when dropped, whatever happened.
#[derive(Debug)]
struct ScratchGuard(PathBuf);

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The refusal of a build without optimisation, unless the flag allows it.
pub fn check_profile(allow_debug_build: bool) -> Result<()> {
    let p = BuildProfile::current(allow_debug_build);
    if p.is_release() || allow_debug_build {
        Ok(())
    } else {
        bail!(
            "this binary is not an optimised release build ({} profile, opt-level {}{}): probe \
             timings from it mean nothing. Run `cargo run --release -p lpk-bench -- probe ...`, or \
             pass --allow-debug-build to record the run as such",
            p.profile,
            p.opt_level,
            if p.debug_assertions {
                ", debug assertions"
            } else {
                ""
            }
        )
    }
}

/// Run the selected probes, write their files, and validate the directory. A failing probe is
/// reported and the next one runs. Any earlier result of a probe is removed before it runs, so a
/// failure leaves no stale file behind.
pub fn execute(cfg: &Config) -> Result<Outcome> {
    host::check_build(env!("LPK_GIT_COMMIT"), cfg.allow_dirty).map_err(anyhow::Error::msg)?;
    check_profile(cfg.allow_debug_build)?;
    let corpus = load_corpus(&cfg.corpus)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let dir = prepare_dir(cfg, &corpus, now)?;
    let scratch =
        crate::run::exec::absolute(&cfg.tmp_root)?.join(format!("probe{}", std::process::id()));
    std::fs::create_dir_all(&scratch).with_context(|| format!("creating {}", scratch.display()))?;
    let _guard = ScratchGuard(scratch.clone());
    let info = RunInfo {
        build: env!("LPK_GIT_COMMIT").to_string(),
        build_profile: BuildProfile::current(cfg.allow_debug_build),
        host: host::sanitize_host(&sysinfo::System::host_name().unwrap_or_default()),
        date: String::new(),
    };
    let ctx = Ctx {
        corpus: &corpus,
        threads: cfg.threads.max(1),
        scratch,
        local_tools: cfg.local_tools.clone(),
        tool_timeout: cfg.tool_timeout,
    };
    let mut out = Outcome {
        results_dir: dir.clone(),
        ..Outcome::default()
    };
    println!("results: {}", dir.display());
    for name in &cfg.probes {
        for ext in ["json", "md"] {
            let _ = std::fs::remove_file(dir.join(format!("probe-{name}.{ext}")));
        }
        let info = RunInfo {
            date: rfc3339_utc(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
            ),
            build: info.build.clone(),
            build_profile: info.build_profile.clone(),
            host: info.host.clone(),
        };
        println!("probe {name}: running");
        let step = run_one(name, &ctx, &info).and_then(|json| {
            let md = render_file(name, &json).context("rendering the table from the JSON")?;
            std::fs::write(dir.join(format!("probe-{name}.json")), &json)?;
            std::fs::write(dir.join(format!("probe-{name}.md")), &md)?;
            Ok(md)
        });
        // Whatever the probe left in its scratch space goes before the next one.
        if let Ok(rd) = std::fs::read_dir(&ctx.scratch) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                let _ = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
            }
        }
        match step {
            Ok(md) => {
                println!("{md}");
                out.written.push(name.clone());
            }
            Err(e) => {
                eprintln!("error: probe {name}: {e:#}");
                out.failed.push((name.clone(), format!("{e:#}")));
            }
        }
    }
    if cfg.into.is_none() && out.written.is_empty() {
        // A directory this run created and wrote no probe file into: nothing to keep.
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(out);
    }
    let report = crate::run::validate::validate_dir(&dir)?;
    for p in &report.problems {
        eprintln!("error: {p}");
    }
    out.problems = report.problems.len();
    Ok(out)
}

pub fn command(args: &ProbeArgs) -> ExitCode {
    let profile = args.profile.clone().unwrap_or_else(|| "small".to_string());
    let probes: Vec<String> = if args.name == "all" {
        NAMES.iter().map(|s| s.to_string()).collect()
    } else {
        vec![args.name.clone()]
    };
    let cfg = Config {
        probes,
        corpus: args
            .corpus
            .clone()
            .unwrap_or_else(|| PathBuf::from("bench/corpus").join(&profile)),
        into: args.into.clone(),
        results_root: args.results.clone(),
        tmp_root: args.tmp.clone(),
        threads: args.threads.unwrap_or_else(host::logical_cores).max(1),
        allow_dirty: args.allow_dirty_build,
        allow_debug_build: args.allow_debug_build,
        local_tools: PathBuf::from(crate::run::DEFAULT_LOCAL),
        tool_timeout: Duration::from_secs(3600),
    };
    match execute(&cfg) {
        Ok(o) => {
            if !o.written.is_empty() {
                println!("results written to {}", o.results_dir.display());
            }
            if o.failed.is_empty() && o.problems == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
