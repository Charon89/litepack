//! `corpus build`: registry + lock + downloader + extractor -> `<out>` with manifest.
//!
//! Extension point: [`build_source`] is the single dispatch on [`SourceSpec`]. A new kind adds a
//! match arm there that returns the files it produced:
//! * static kinds (`file`, `archive`) call [`Ctx::artifact`] for their fixed URLs;
//! * list kinds (URLs resolved from an API) follow the contract in [`ListedFile`]: resolve only
//!   under `--update-lock`, otherwise take the list from [`Ctx::listed_pins`], and write files
//!   with [`Ctx::fetch_listed`];
//! * derived kinds read other classes' files below [`Ctx::out`] (and [`Ctx::produced`] for the
//!   files made earlier in this run).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};

use super::extract::{check_listing, extract, sanitize_path, Selection};
use super::fetch::{
    repin_hint, Artifact, DownloadError, Downloader, Expect, Fetcher, Politeness, RetryPolicy,
};
use super::lock::{Lock, LockEntry};
use super::manifest::{now_rfc3339, BuildInfo, Manifest, ManifestFile, Skipped};
use super::registry::{
    file_name_for, gz_output_name, ArchiveFormat, FilesSpec, Profile, Registry, Source, SourceSpec,
};

/// Marker written into every output directory this tool creates. A non-empty directory
/// without it is never touched.
pub const MARKER: &str = ".lpk-corpus";

/// Files the tool writes at the top of `<out>` itself (never removed by clean-up).
const TOP_LEVEL_FILES: [&str; 5] = [
    MARKER,
    "manifest.json",
    "manifest.partial.json",
    "build-info.json",
    "build-info.partial.json",
];

/// Everything `corpus build` needs.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub profile: Profile,
    pub out: PathBuf,
    pub cache: PathBuf,
    /// Restrict to these classes (empty = all). A restricted build is *partial*: it writes
    /// `manifest.partial.json` instead of `manifest.json` and removes nothing.
    pub only: Vec<String>,
    pub update_lock: bool,
    pub sources_path: PathBuf,
    pub lock_path: PathBuf,
    pub retry: RetryPolicy,
}

/// Result of a successful build.
#[derive(Debug, Clone)]
pub struct BuildReport {
    pub manifest_path: PathBuf,
    pub manifest_blake3: String,
    pub files: usize,
    pub bytes_total: u64,
    pub skipped: Vec<Skipped>,
    /// Paths (relative to `<out>`) removed because the manifest does not list them.
    pub removed: Vec<String>,
    /// Lock pins of built sources that this build did not use (normal builds only).
    pub unused_pins: Vec<String>,
    /// True for `--only` builds.
    pub partial: bool,
}

/// One file of a list-type source. This is the contract for such sources: the lock *is* the
/// list. Under `--update-lock` a resolver calls its API, builds `ListedFile`s and fetches them
/// with [`Ctx::fetch_listed`], which records each one in the lock. A normal build must make no
/// API call: it takes the same list from [`Ctx::listed_pins`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedFile {
    pub url: String,
    /// Output path relative to `<out>/<class>/<source>/`, validated like archive entries.
    pub path: String,
    /// Per-file licence; the manifest uses it instead of the source-level string.
    pub licence: Option<String>,
    /// Author / credit line, kept in the lock.
    pub attribution: Option<String>,
    /// Anything else a resolver wants to keep with the pin.
    pub extra: BTreeMap<String, String>,
}

/// State shared by all sources of one build.
#[derive(Debug)]
pub struct Ctx<'a> {
    profile: Profile,
    update_lock: bool,
    out: PathBuf,
    downloader: Downloader<'a>,
    lock: &'a Lock,
    /// Lock entries recorded while building the current source (`--update-lock`).
    recorded: Vec<LockEntry>,
    /// `(source, url)` of every pin consulted in a normal build.
    used: BTreeSet<(String, String)>,
    /// `(class, file)` for every file produced so far in this run.
    pub produced: Vec<(String, ManifestFile)>,
}

impl Ctx<'_> {
    /// The output root (`<out>`), e.g. for derivation steps reading other classes' files.
    #[allow(dead_code)] // for derived kinds (later sub-task)
    pub fn out(&self) -> &Path {
        &self.out
    }

    /// True under `--update-lock`: the only mode in which a resolver may call an API.
    pub fn update_lock(&self) -> bool {
        self.update_lock
    }

    /// Get a verified artifact for `url`, enforcing the lock (or recording it with
    /// `--update-lock`). For static kinds.
    pub fn artifact(&mut self, source: &Source, url: &str) -> Result<Artifact, DownloadError> {
        self.artifact_inner(source, url, None)
    }

    fn artifact_inner(
        &mut self,
        source: &Source,
        url: &str,
        meta: Option<&ListedFile>,
    ) -> Result<Artifact, DownloadError> {
        let expect = if self.update_lock {
            Expect::Unpinned
        } else {
            match self.lock.get(self.profile, &source.id, url) {
                Some(pin) => {
                    self.used.insert((source.id.clone(), url.to_string()));
                    Expect::Pinned(pin)
                }
                None => return Err(missing_pin(self.profile, &source.id, url)),
            }
        };
        let a = self.downloader.obtain(&source.id, url, expect)?;
        if self.update_lock {
            let mut entry = LockEntry::artifact(&source.id, url, a.bytes, a.blake3.clone());
            if let Some(m) = meta {
                entry.path = Some(m.path.clone());
                entry.licence = m.licence.clone();
                entry.attribution = m.attribution.clone();
                entry.extra = m.extra.clone();
            }
            self.recorded.push(entry);
        }
        Ok(a)
    }

    /// The file list of a list-type source in a normal build: its lock entries, sorted by URL.
    /// An empty list is an error (the source was never pinned).
    pub fn listed_pins(&self, source: &Source) -> Result<Vec<ListedFile>> {
        let pins = self.lock.entries(self.profile, &source.id);
        if pins.is_empty() {
            return Err(missing_pin(self.profile, &source.id, "(list)").into());
        }
        let listing: Vec<ListedFile> = pins
            .into_iter()
            .map(|e| {
                Ok(ListedFile {
                    url: e.url.clone(),
                    path: e.path.clone().with_context(|| {
                        format!("source `{}`: lock entry {} has no `path`", source.id, e.url)
                    })?,
                    licence: e.licence.clone(),
                    attribution: e.attribution.clone(),
                    extra: e.extra.clone(),
                })
            })
            .collect::<Result<_>>()?;
        self.check_listing(source, &listing)?;
        Ok(listing)
    }

    /// Validate a whole listing before anything is fetched (safe, portable paths; no
    /// case-insensitive duplicates or file/directory clashes, identically on every OS). Resolvers
    /// must call this under `--update-lock`; `listed_pins` does it for normal builds.
    pub fn check_listing(&self, source: &Source, listing: &[ListedFile]) -> Result<()> {
        let paths: Vec<String> = listing.iter().map(|f| f.path.clone()).collect();
        check_listing(&paths).with_context(|| format!("source `{}`: listing", source.id))
    }

    /// Fetch one listed file (verified against its pin, or recorded under `--update-lock`) and
    /// place it at `dir/<item.path>`. The manifest entry carries the per-file licence when set.
    pub fn fetch_listed(
        &mut self,
        source: &Source,
        dir: &Path,
        item: &ListedFile,
    ) -> Result<ManifestFile> {
        let comps = sanitize_path(&item.path)?;
        ensure!(!comps.is_empty(), "source `{}`: empty file path", source.id);
        let a = self.artifact_inner(source, &item.url, Some(item))?;
        let mut dest = dir.to_path_buf();
        for c in &comps {
            dest.push(c);
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        ensure!(
            !dest.exists(),
            "source `{}`: two listed files map to `{}`",
            source.id,
            item.path
        );
        std::fs::copy(&a.path, &dest)
            .with_context(|| format!("copying {} into the corpus", a.path.display()))?;
        Ok(ManifestFile {
            blake3: a.blake3,
            bytes: a.bytes,
            licence: item
                .licence
                .clone()
                .unwrap_or_else(|| source.licence.clone()),
            path: format!("{}/{}/{}", source.class, source.id, comps.join("/")),
            source: source.id.clone(),
        })
    }

    /// Pins of `source_id` that were not consulted.
    fn unused(&self, source_id: &str) -> Vec<String> {
        self.lock
            .entries(self.profile, source_id)
            .into_iter()
            .filter(|e| !self.used.contains(&(e.source.clone(), e.url.clone())))
            .map(|e| format!("{}: {}", e.source, e.url))
            .collect()
    }
}

fn missing_pin(profile: Profile, source: &str, url: &str) -> DownloadError {
    DownloadError::Mismatch(format!(
        "source `{source}`: no entry for {url} in bench/corpus.lock (profile `{}`). {}",
        profile.name(),
        repin_hint(profile)
    ))
}

/// Before any network traffic: every static URL needs a pin, and a list source needs a
/// non-empty pinned listing (otherwise it would fail after earlier sources have downloaded).
fn preflight_pins(sources: &[&Source], lock: &Lock, profile: Profile) -> Result<()> {
    for s in sources {
        if s.spec.is_list() && lock.entries(profile, &s.id).is_empty() {
            return Err(missing_pin(profile, &s.id, "(list)").into());
        }
        for url in s.spec.static_urls() {
            if lock.get(profile, &s.id, url).is_none() {
                return Err(missing_pin(profile, &s.id, url).into());
            }
        }
    }
    Ok(())
}

/// The listing a `files` source declares, sorted by URL (the order of its pins).
fn spec_listing(spec: &FilesSpec) -> Result<Vec<ListedFile>> {
    let mut v = spec
        .files
        .iter()
        .map(|i| {
            Ok(ListedFile {
                url: i.url.clone(),
                path: file_name_for(&i.url, i.path.as_deref())?,
                licence: i.licence.clone(),
                attribution: i.attribution.clone(),
                extra: BTreeMap::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    v.sort_by(|a, b| a.url.cmp(&b.url));
    Ok(v)
}

/// Refuse to build derived sources whose input classes are neither part of this run nor already
/// in `<out>`. Only relevant with `--only` (a full run builds everything in registry order).
fn check_derived_inputs(sources: &[&Source], out: &Path) -> Result<()> {
    for (i, s) in sources.iter().enumerate() {
        let missing: Vec<&str> = s
            .inputs
            .iter()
            .map(String::as_str)
            .filter(|class| !sources[..i].iter().any(|b| b.class == *class))
            .filter(|class| !dir_has_files(&out.join(class)))
            .collect();
        if !missing.is_empty() {
            bail!(
                "source `{}` (class `{}`) is derived from class(es) {} which are neither built \
                 earlier in this run nor present in {}; add them to --only or build them first",
                s.id,
                s.class,
                missing.join(", "),
                out.display()
            );
        }
    }
    Ok(())
}

fn dir_has_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

/// Make `<out>` ready: refuse foreign non-empty directories, write the marker, make sure the
/// cache is not inside it, and delete stale manifests so a failed build leaves none behind.
fn prepare_out(out: &Path, cache: &Path) -> Result<()> {
    if out.exists() {
        ensure!(
            out.is_dir(),
            "{} exists and is not a directory",
            out.display()
        );
        if !out.join(MARKER).exists() && dir_has_files(out) {
            bail!(
                "{} is not empty and was not created by lpk-bench (no {MARKER} marker); \
                 refusing to touch it. Use a new or empty directory.",
                out.display()
            );
        }
    }
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    std::fs::create_dir_all(cache).with_context(|| format!("creating {}", cache.display()))?;
    let (out_c, cache_c) = (out.canonicalize()?, cache.canonicalize()?);
    ensure!(
        !cache_c.starts_with(&out_c),
        "the cache directory {} must not be inside the output directory {} (clean-up would \
         delete it)",
        cache.display(),
        out.display()
    );
    let marker = out.join(MARKER);
    if !marker.exists() {
        std::fs::write(
            &marker,
            "lpk-bench corpus output. A full build deletes files here that manifest.json does not list.\n",
        )?;
    }
    for name in TOP_LEVEL_FILES.iter().filter(|n| **n != MARKER) {
        match std::fs::remove_file(out.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing stale {name}")),
        }
    }
    Ok(())
}

/// Remove everything below `out` that is not in `listed` (relative `/` paths), then empty
/// directories. Returns the removed file paths.
fn clean_unlisted(out: &Path, listed: &BTreeSet<String>) -> Result<Vec<String>> {
    fn walk(
        dir: &Path,
        rel: &str,
        listed: &BTreeSet<String>,
        removed: &mut Vec<String>,
    ) -> Result<()> {
        let mut names: Vec<_> = std::fs::read_dir(dir)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<_>>()?;
        names.sort();
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if rel.is_empty() && TOP_LEVEL_FILES.contains(&name.as_str()) {
                continue;
            }
            let path = dir.join(&name);
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                walk(&path, &child, listed, removed)?;
                if dir_is_empty(&path) {
                    std::fs::remove_dir(&path)?;
                }
            } else if !listed.contains(&child) {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing unlisted {}", path.display()))?;
                removed.push(child);
            }
        }
        Ok(())
    }
    let mut removed = Vec::new();
    walk(out, "", listed, &mut removed)?;
    Ok(removed)
}

fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none())
}

/// Build the corpus. `fetcher` supplies bytes for URLs (HTTPS in production, memory in tests).
pub fn build(opts: &BuildOptions, fetcher: &dyn Fetcher) -> Result<BuildReport> {
    let registry = Registry::load(&opts.sources_path)?;
    let sources = registry.select(opts.profile, &opts.only)?;
    if sources.is_empty() {
        bail!("no sources apply to profile `{}`", opts.profile.name());
    }
    let partial = !opts.only.is_empty();
    let mut lock = Lock::load(&opts.lock_path)?;

    // Fail before touching anything if the request cannot be satisfied.
    if partial {
        check_derived_inputs(&sources, &opts.out)?;
    }
    if !opts.update_lock {
        preflight_pins(&sources, &lock, opts.profile)?;
    }

    prepare_out(&opts.out, &opts.cache)?;
    let lock_snapshot = lock.clone();
    let mut ctx = Ctx {
        profile: opts.profile,
        update_lock: opts.update_lock,
        out: opts.out.clone(),
        downloader: Downloader::new(fetcher, opts.cache.clone(), opts.retry, opts.profile)
            .with_politeness(Politeness::from_specs(&registry.hosts)),
        lock: &lock_snapshot,
        recorded: Vec::new(),
        used: BTreeSet::new(),
        produced: Vec::new(),
    };

    let mut skipped = Vec::new();
    let mut pins: Vec<(String, Vec<LockEntry>)> = Vec::new();
    let mut built_ids: Vec<&str> = Vec::new();
    for source in &sources {
        eprintln!("[{}] {} ({})", source.class, source.id, source.origin);
        let dir = opts.out.join(&source.class).join(&source.id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clearing {}", dir.display()))?;
        }
        std::fs::create_dir_all(&dir)?;
        ctx.recorded.clear();
        match build_source(&mut ctx, source, &dir) {
            Ok(files) => {
                eprintln!("  {} files", files.len());
                ctx.produced
                    .extend(files.into_iter().map(|f| (source.class.clone(), f)));
                pins.push((source.id.clone(), std::mem::take(&mut ctx.recorded)));
                built_ids.push(&source.id);
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                let tolerated = source.optional
                    && matches!(
                        e.downcast_ref::<DownloadError>(),
                        Some(DownloadError::Fetch(_))
                    );
                if !tolerated {
                    return Err(e.context(format!("building source `{}`", source.id)));
                }
                eprintln!("  skipped (optional): {e:#}");
                skipped.push(Skipped {
                    reason: format!("{e:#}"),
                    source: source.id.clone(),
                });
            }
        }
    }

    let mut unused_pins = Vec::new();
    if opts.update_lock {
        for (id, entries) in pins {
            lock.replace_source(opts.profile, &id, entries);
        }
        lock.save(&opts.lock_path)?;
        eprintln!("wrote {}", opts.lock_path.display());
    } else {
        for id in &built_ids {
            unused_pins.extend(ctx.unused(id));
        }
    }

    let manifest = Manifest::new(opts.profile, std::mem::take(&mut ctx.produced));
    let removed = if partial {
        Vec::new()
    } else {
        let listed: BTreeSet<String> = manifest
            .classes
            .values()
            .flat_map(|c| c.files.iter().map(|f| f.path.clone()))
            .collect();
        clean_unlisted(&opts.out, &listed)?
    };
    let (manifest_name, info_name) = if partial {
        ("manifest.partial.json", "build-info.partial.json")
    } else {
        ("manifest.json", "build-info.json")
    };
    let manifest_text = manifest.render();
    let manifest_blake3 = blake3::hash(manifest_text.as_bytes()).to_hex().to_string();
    let manifest_path = opts.out.join(manifest_name);
    std::fs::write(&manifest_path, &manifest_text)
        .with_context(|| format!("writing {}", manifest_path.display()))?;
    let info = BuildInfo {
        built_at: now_rfc3339(),
        host_arch: std::env::consts::ARCH.to_string(),
        host_os: std::env::consts::OS.to_string(),
        lpk_bench_version: env!("CARGO_PKG_VERSION").to_string(),
        manifest_blake3: manifest_blake3.clone(),
        profile: opts.profile.name().to_string(),
        skipped: skipped.clone(),
    };
    std::fs::write(opts.out.join(info_name), info.render())?;

    Ok(BuildReport {
        manifest_path,
        manifest_blake3,
        files: manifest.classes.values().map(|c| c.files.len()).sum(),
        bytes_total: manifest.classes.values().map(|c| c.bytes_total).sum(),
        skipped,
        removed,
        unused_pins,
        partial,
    })
}

/// Materialise one source into `dir` (`<out>/<class>/<id>`, empty on entry).
pub fn build_source(ctx: &mut Ctx<'_>, source: &Source, dir: &Path) -> Result<Vec<ManifestFile>> {
    let entry = |rel: &str, bytes: u64, blake3: String| ManifestFile {
        blake3,
        bytes,
        licence: source.licence.clone(),
        path: format!("{}/{}/{rel}", source.class, source.id),
        source: source.id.clone(),
    };
    match &source.spec {
        SourceSpec::File(spec) => {
            let a = ctx.artifact(source, &spec.url)?;
            let name = file_name_for(&spec.url, spec.filename.as_deref())?;
            std::fs::copy(&a.path, dir.join(&name))
                .with_context(|| format!("copying {} into the corpus", a.path.display()))?;
            Ok(vec![entry(&name, a.bytes, a.blake3)])
        }
        SourceSpec::Archive(spec) => {
            let a = ctx.artifact(source, &spec.url)?;
            let format = spec
                .format
                .or_else(|| ArchiveFormat::guess(&spec.url))
                .context("archive format unknown")?;
            let mut sel = Selection::new(
                &spec.include,
                &spec.exclude,
                spec.max_files,
                spec.max_bytes,
                spec.strip_components,
            )?;
            sel.truncate = spec.truncate_files;
            if format == ArchiveFormat::Gz {
                sel.single_name = Some(gz_output_name(&spec.url)?);
            }
            let files = extract(format, &a.path, dir, &sel)?;
            Ok(files
                .into_iter()
                .map(|f| entry(&f.path, f.bytes, f.blake3))
                .collect())
        }
        SourceSpec::Files(spec) => {
            let declared = spec_listing(spec)?;
            let items = if ctx.update_lock() {
                declared
            } else {
                let pinned = ctx.listed_pins(source)?;
                ensure!(
                    pinned == declared,
                    "source `{}`: the registry's file list differs from the pins in bench/corpus.lock. {}",
                    source.id,
                    repin_hint(ctx.profile)
                );
                pinned
            };
            ctx.check_listing(source, &items)?;
            let mut made = Vec::new();
            for item in &items {
                made.push(ctx.fetch_listed(source, dir, item)?);
            }
            Ok(made)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fetch::fake::{fast_retry, FakeFetcher};
    use super::*;
    use std::io::{Cursor, Write};

    const ZIP_URL: &str = "https://example.org/pack.zip";
    const FILE_URL: &str = "https://example.org/data/blob.bin";
    const OPT_URL: &str = "https://example.org/optional.bin";

    fn registry_text(optional_flag: bool) -> String {
        format!(
            r#"
[[source]]
id = "pack"
class = "beta"
licence = "MIT"
origin = "test zip"
profiles = ["small"]
kind = "archive"
url = "{ZIP_URL}"
exclude = ["skip/**"]

[[source]]
id = "blob"
class = "alpha"
licence = "CC0-1.0"
origin = "test file"
profiles = ["small", "full"]
kind = "file"
url = "{FILE_URL}"

[[source]]
id = "maybe"
class = "gamma"
licence = "MIT"
origin = "flaky"
profiles = ["small"]
optional = {optional_flag}
kind = "file"
url = "{OPT_URL}"
"#
        )
    }

    fn make_zip() -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        for (n, d) in [
            ("z/last.txt", &b"last"[..]),
            ("a.txt", b"first!"),
            ("skip/x", b"no"),
        ] {
            w.start_file(n, o).expect("start");
            w.write_all(d).expect("write");
        }
        w.finish().expect("finish").into_inner()
    }

    struct Env {
        _dir: tempfile::TempDir,
        root: PathBuf,
        fetcher: FakeFetcher,
    }

    impl Env {
        fn new(optional_flag: bool) -> Env {
            let dir = tempfile::tempdir().expect("tmp");
            let root = dir.path().to_path_buf();
            std::fs::write(root.join("sources.toml"), registry_text(optional_flag)).expect("w");
            let fetcher = FakeFetcher::with(ZIP_URL, make_zip());
            fetcher
                .files
                .borrow_mut()
                .insert(FILE_URL.into(), b"blob-bytes".to_vec());
            fetcher
                .files
                .borrow_mut()
                .insert(OPT_URL.into(), b"opt".to_vec());
            Env {
                _dir: dir,
                root,
                fetcher,
            }
        }

        fn opts(&self, out: &str, update_lock: bool) -> BuildOptions {
            BuildOptions {
                profile: Profile::Small,
                out: self.root.join(out),
                cache: self.root.join("cache"),
                only: Vec::new(),
                update_lock,
                sources_path: self.root.join("sources.toml"),
                lock_path: self.root.join("corpus.lock"),
                retry: fast_retry(),
            }
        }

        fn manifest(&self, out: &str) -> String {
            self.read(out, "manifest.json")
        }

        fn read(&self, out: &str, name: &str) -> String {
            std::fs::read_to_string(self.root.join(out).join(name)).expect(name)
        }

        fn lock(&self) -> String {
            std::fs::read_to_string(self.root.join("corpus.lock")).expect("lock")
        }
    }

    #[test]
    fn manifest_content_is_sorted_slashed_and_totalled() {
        let env = Env::new(false);
        let report = build(&env.opts("out", true), &env.fetcher).expect("build");
        let m: Manifest = serde_json::from_str(&env.manifest("out")).expect("json");
        assert_eq!(m.profile, "small");
        assert_eq!(
            m.classes.keys().collect::<Vec<_>>(),
            ["alpha", "beta", "gamma"]
        );
        let beta = &m.classes["beta"];
        let paths: Vec<_> = beta.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["beta/pack/a.txt", "beta/pack/z/last.txt"]);
        assert_eq!(beta.bytes_total, 6 + 4);
        assert_eq!(beta.files[0].licence, "MIT");
        assert_eq!(beta.files[0].source, "pack");
        assert_eq!(
            beta.files[0].blake3,
            blake3::hash(b"first!").to_hex().to_string()
        );
        assert_eq!(m.classes["alpha"].files[0].path, "alpha/blob/blob.bin");
        assert!(!env.manifest("out").contains('\\'));
        assert_eq!(report.files, 4);
        assert_eq!(report.bytes_total, 10 + 10 + 3);
        assert!(!report.partial && report.removed.is_empty());
        assert!(env.root.join("out/beta/pack/z/last.txt").is_file());
        assert!(!env.root.join("out/beta/pack/skip").exists());
        assert!(env.root.join("out").join(MARKER).is_file());
        let info: BuildInfo =
            serde_json::from_str(&env.read("out", "build-info.json")).expect("json");
        assert_eq!(info.manifest_blake3, report.manifest_blake3);
        assert_eq!(info.lpk_bench_version, env!("CARGO_PKG_VERSION"));
        assert!(info.built_at.ends_with('Z'));
    }

    #[test]
    fn three_builds_give_byte_identical_manifests_and_lock() {
        let env = Env::new(false);
        build(&env.opts("out1", true), &env.fetcher).expect("update");
        let lock1 = env.lock();
        build(&env.opts("out2", false), &env.fetcher).expect("verify");
        assert_eq!(env.manifest("out1"), env.manifest("out2"));
        std::fs::remove_dir_all(env.root.join("out2")).expect("rm");
        build(&env.opts("out3", false), &env.fetcher).expect("from cache");
        assert_eq!(env.manifest("out1"), env.manifest("out3"));
        build(&env.opts("out4", true), &env.fetcher).expect("update again");
        assert_eq!(
            lock1,
            env.lock(),
            "regenerating the lock must be byte-identical"
        );
    }

    #[test]
    fn normal_build_uses_cache_after_out_is_deleted() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("update");
        let calls = env.fetcher.call_count();
        std::fs::remove_dir_all(env.root.join("out")).expect("rm");
        build(&env.opts("out", false), &env.fetcher).expect("rebuild");
        assert_eq!(
            env.fetcher.call_count(),
            calls,
            "no network when the cache verifies"
        );
    }

    #[test]
    fn missing_lock_entry_is_a_hard_error_before_any_download() {
        let env = Env::new(false);
        let err = build(&env.opts("out", false), &env.fetcher).expect_err("no lock");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--update-lock") && msg.contains("corpus.lock"),
            "{msg}"
        );
        assert!(
            msg.contains("manifest.partial.json"),
            "hint must explain --only: {msg}"
        );
        assert!(msg.contains("pack") || msg.contains("blob"), "{msg}");
        assert_eq!(env.fetcher.call_count(), 0);
        assert!(
            !env.root.join("out").exists(),
            "nothing may be created on a pre-flight failure"
        );
    }

    #[test]
    fn lock_mismatch_is_a_hard_error_naming_the_source() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("update");
        // Upstream "changes": bytes differ and the (old) cache no longer matches a tampered pin.
        let lock = env.lock();
        let first_hash = lock
            .split("\"blake3\": \"")
            .nth(1)
            .and_then(|s| s.get(..64))
            .expect("hash");
        std::fs::write(
            env.root.join("corpus.lock"),
            lock.replacen(first_hash, &"0".repeat(64), 1),
        )
        .expect("tamper");
        let err = build(&env.opts("out", false), &env.fetcher).expect_err("mismatch");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("does not match") && msg.contains("--update-lock"),
            "{msg}"
        );
        assert!(msg.contains("`blob`"), "{msg}");
    }

    #[test]
    fn failed_build_leaves_no_stale_manifest() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("update");
        assert!(env.root.join("out/manifest.json").is_file());
        // Break the build after the pre-flight: the cache is gone and upstream changed.
        std::fs::remove_dir_all(env.root.join("cache")).expect("rm cache");
        env.fetcher
            .files
            .borrow_mut()
            .insert(FILE_URL.into(), b"changed!".to_vec());
        assert!(build(&env.opts("out", false), &env.fetcher).is_err());
        for name in ["manifest.json", "build-info.json"] {
            assert!(
                !env.root.join("out").join(name).exists(),
                "{name} must not survive a failure"
            );
        }
    }

    #[test]
    fn upstream_change_is_caught_and_repinned_with_update_lock() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("update");
        std::fs::remove_dir_all(env.root.join("cache")).expect("rm cache");
        env.fetcher
            .files
            .borrow_mut()
            .insert(FILE_URL.into(), b"changed!".to_vec());
        let err = build(&env.opts("out", false), &env.fetcher).expect_err("changed upstream");
        assert!(format!("{err:#}").contains("`blob`"));
        build(&env.opts("out", true), &env.fetcher).expect("repin");
        build(&env.opts("out", false), &env.fetcher).expect("now passes");
    }

    #[test]
    fn optional_source_failure_is_recorded_as_skipped() {
        let env = Env::new(true);
        build(&env.opts("out", true), &env.fetcher).expect("pin all");
        std::fs::remove_dir_all(env.root.join("cache")).expect("rm cache");
        env.fetcher.files.borrow_mut().remove(OPT_URL);
        let report = build(&env.opts("out", false), &env.fetcher).expect("optional may fail");
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].source, "maybe");
        assert!(!env.manifest("out").contains("gamma"));

        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("pin all");
        std::fs::remove_dir_all(env.root.join("cache")).expect("rm cache");
        env.fetcher.files.borrow_mut().remove(OPT_URL);
        assert!(build(&env.opts("out", false), &env.fetcher).is_err());
    }

    #[test]
    fn only_writes_a_partial_manifest_and_cleans_nothing() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("pin all");
        let full_lock = env.lock();
        let full_manifest = env.manifest("out");
        std::fs::write(env.root.join("out/stray.txt"), b"x").expect("stray");

        let mut o = env.opts("out", true);
        o.only = vec!["alpha".into()];
        let report = build(&o, &env.fetcher).expect("only alpha");
        assert!(report.partial && report.removed.is_empty());
        assert!(
            env.root.join("out/stray.txt").exists(),
            "--only must not clean up"
        );
        assert!(
            env.root.join("out/beta/pack/a.txt").exists(),
            "other classes stay"
        );
        assert!(
            !env.root.join("out/manifest.json").exists(),
            "never a partial manifest.json"
        );
        let m: Manifest =
            serde_json::from_str(&env.read("out", "manifest.partial.json")).expect("json");
        assert_eq!(m.classes.keys().collect::<Vec<_>>(), ["alpha"]);
        assert!(env.root.join("out/build-info.partial.json").is_file());
        assert_eq!(env.lock(), full_lock, "other sources' pins are kept");

        // A following full build restores manifest.json (identical) and drops the partial files.
        build(&env.opts("out", false), &env.fetcher).expect("full");
        assert_eq!(env.manifest("out"), full_manifest);
        assert!(!env.root.join("out/manifest.partial.json").exists());
        assert!(!env.root.join("out/stray.txt").exists());
    }

    #[test]
    fn full_build_removes_unlisted_files_and_empty_dirs_and_reports_them() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("build");
        let out = env.root.join("out");
        std::fs::create_dir_all(out.join("oldclass/oldsrc/deep")).expect("mk");
        std::fs::write(out.join("oldclass/oldsrc/deep/f.bin"), b"1").expect("w");
        std::fs::write(out.join("beta/pack/extra.txt"), b"2").expect("w");
        let report = build(&env.opts("out", false), &env.fetcher).expect("rebuild");
        assert_eq!(report.removed, ["oldclass/oldsrc/deep/f.bin"]);
        assert!(!out.join("oldclass").exists(), "emptied directories go too");
        assert!(!out.join("beta/pack/extra.txt").exists());
        assert!(out.join(MARKER).is_file() && out.join("manifest.json").is_file());
        assert!(out.join("beta/pack/a.txt").is_file());
    }

    #[test]
    fn foreign_non_empty_directory_is_refused_untouched() {
        let env = Env::new(false);
        let foreign = env.root.join("precious");
        std::fs::create_dir_all(&foreign).expect("mk");
        std::fs::write(foreign.join("keep.txt"), b"mine").expect("w");
        let err = build(&env.opts("precious", true), &env.fetcher).expect_err("foreign dir");
        assert!(
            format!("{err:#}").contains("not created by lpk-bench"),
            "{err:#}"
        );
        assert_eq!(
            std::fs::read(foreign.join("keep.txt")).expect("read"),
            b"mine"
        );
        assert!(!foreign.join(MARKER).exists());
        // An empty existing directory is fine and gets the marker.
        let empty = env.root.join("empty");
        std::fs::create_dir_all(&empty).expect("mk");
        build(&env.opts("empty", true), &env.fetcher).expect("empty dir ok");
        assert!(empty.join(MARKER).is_file());
    }

    #[test]
    fn cache_inside_out_is_refused() {
        let env = Env::new(false);
        let mut o = env.opts("out", true);
        o.cache = o.out.join("cache");
        let err = build(&o, &env.fetcher).expect_err("cache in out");
        assert!(format!("{err:#}").contains("must not be inside"), "{err:#}");
    }

    #[test]
    fn unused_pins_are_reported_by_normal_builds() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("pin");
        let mut lock = Lock::load(&env.root.join("corpus.lock")).expect("lock");
        let mut pins: Vec<LockEntry> = lock
            .entries(Profile::Small, "blob")
            .into_iter()
            .cloned()
            .collect();
        pins.push(LockEntry::artifact(
            "blob",
            "https://example.org/gone.bin",
            1,
            "ab".repeat(32),
        ));
        lock.replace_source(Profile::Small, "blob", pins);
        lock.save(&env.root.join("corpus.lock")).expect("save");
        let report = build(&env.opts("out", false), &env.fetcher).expect("build");
        assert_eq!(report.unused_pins, ["blob: https://example.org/gone.bin"]);
    }

    #[test]
    fn only_with_a_derived_class_needs_its_inputs() {
        let env = Env::new(false);
        let mut text = registry_text(false);
        text.push_str(&format!(
            "\n[[source]]\nid = \"derived\"\nclass = \"delta\"\nlicence = \"MIT\"\n\
             origin = \"derived from alpha\"\nprofiles = [\"small\"]\ninputs = [\"alpha\"]\n\
             kind = \"file\"\nurl = \"{OPT_URL}\"\nfilename = \"d.bin\"\n"
        ));
        std::fs::write(env.root.join("sources.toml"), text).expect("w");

        let mut o = env.opts("out", true);
        o.only = vec!["delta".into()];
        let err = build(&o, &env.fetcher).expect_err("inputs missing");
        let msg = format!("{err:#}");
        assert!(msg.contains("alpha") && msg.contains("--only"), "{msg}");
        assert!(
            !env.root.join("out").exists(),
            "must fail before touching <out>"
        );

        // Selecting the input class in the same run is fine.
        o.only = vec!["alpha".into(), "delta".into()];
        build(&o, &env.fetcher).expect("inputs selected");

        // Inputs already present in <out> (from an earlier run) are fine too.
        let mut o = env.opts("out2", true);
        o.only = vec!["alpha".into()];
        build(&o, &env.fetcher).expect("alpha");
        o.only = vec!["delta".into()];
        build(&o, &env.fetcher).expect("alpha present in out");
    }

    #[test]
    fn profile_filter_applies() {
        let env = Env::new(false);
        let mut o = env.opts("out", true);
        o.profile = Profile::Full;
        build(&o, &env.fetcher).expect("full");
        let m: Manifest = serde_json::from_str(&env.manifest("out")).expect("json");
        assert_eq!(m.profile, "full");
        assert_eq!(m.classes.keys().collect::<Vec<_>>(), ["alpha"]);
    }

    // ---- list-type source contract (lock is the listing; API only under --update-lock) ----

    fn list_source() -> Source {
        Source {
            id: "photos".into(),
            class: "photo".into(),
            licence: "CC0-1.0".into(),
            origin: "test list".into(),
            profiles: vec![Profile::Small],
            optional: false,
            inputs: vec![],
            spec: SourceSpec::File(super::super::registry::FileSpec {
                url: "https://unused.example/".into(),
                filename: None,
            }),
        }
    }

    fn test_ctx<'a>(
        fetcher: &'a FakeFetcher,
        root: &Path,
        lock: &'a Lock,
        update_lock: bool,
    ) -> Ctx<'a> {
        Ctx {
            profile: Profile::Small,
            update_lock,
            out: root.join("out"),
            downloader: Downloader::new(fetcher, root.join("cache"), fast_retry(), Profile::Small),
            lock,
            recorded: Vec::new(),
            used: BTreeSet::new(),
            produced: Vec::new(),
        }
    }

    fn item(n: u8, licence: Option<&str>) -> ListedFile {
        ListedFile {
            url: format!("https://example.org/p{n}.jpg"),
            path: format!("sub/p{n}.jpg"),
            licence: licence.map(String::from),
            attribution: licence.map(|_| format!("Author {n}")),
            extra: BTreeMap::from([("page".to_string(), format!("File:P{n}"))]),
        }
    }

    #[test]
    fn list_sources_record_metadata_then_build_from_pins_without_a_resolver() {
        let dir = tempfile::tempdir().expect("tmp");
        let fetcher = FakeFetcher::default();
        for n in 1..=3u8 {
            fetcher.files.borrow_mut().insert(
                format!("https://example.org/p{n}.jpg"),
                vec![n; 10 + n as usize],
            );
        }
        let source = list_source();
        let out = dir.path().join("out");

        // --update-lock: the "resolver" lists items; fetch_listed records them.
        let empty = Lock::default();
        let mut ctx = test_ctx(&fetcher, dir.path(), &empty, true);
        assert!(ctx.update_lock());
        let dir1 = out.join("photo/photos");
        let listed = [
            item(1, Some("CC-BY-4.0")),
            item(2, None),
            item(3, Some("CC-BY-SA-4.0")),
        ];
        let mut made = Vec::new();
        for it in &listed {
            made.push(ctx.fetch_listed(&source, &dir1, it).expect("fetch"));
        }
        assert_eq!(made[0].licence, "CC-BY-4.0", "per-file licence wins");
        assert_eq!(made[1].licence, "CC0-1.0", "source licence is the fallback");
        assert_eq!(made[0].path, "photo/photos/sub/p1.jpg");
        assert!(dir1.join("sub/p1.jpg").is_file());
        let mut lock = Lock::default();
        lock.replace_source(Profile::Small, "photos", std::mem::take(&mut ctx.recorded));
        let rendered = lock.render();
        assert!(rendered.contains("\"attribution\": \"Author 1\""));
        assert!(rendered.contains("\"path\": \"sub/p2.jpg\""));

        // Normal build: the list comes from the lock alone; only pinned items are fetched.
        let lock = Lock::parse(&rendered).expect("parse");
        let mut ctx = test_ctx(&fetcher, dir.path(), &lock, false);
        assert!(!ctx.update_lock());
        let pins = ctx.listed_pins(&source).expect("pins");
        assert_eq!(
            pins,
            listed.to_vec(),
            "pins round-trip every field, sorted by URL"
        );
        let dir2 = out.join("again/photos");
        for p in pins.iter().take(2) {
            ctx.fetch_listed(&source, &dir2, p).expect("verified fetch");
        }
        assert_eq!(ctx.unused("photos"), ["photos: https://example.org/p3.jpg"]);

        // A source that was never pinned is an error, not an empty build.
        let other = Source {
            id: "never".into(),
            ..list_source()
        };
        let err = ctx.listed_pins(&other).expect_err("unpinned");
        assert!(format!("{err:#}").contains("--update-lock"));
    }

    #[test]
    fn listed_paths_are_validated_and_must_be_unique() {
        let dir = tempfile::tempdir().expect("tmp");
        let fetcher = FakeFetcher::with("https://example.org/p1.jpg", vec![1; 4]);
        let lock = Lock::default();
        let mut ctx = Ctx {
            profile: Profile::Small,
            update_lock: true,
            out: dir.path().join("out"),
            downloader: Downloader::new(
                &fetcher,
                dir.path().join("cache"),
                fast_retry(),
                Profile::Small,
            ),
            lock: &lock,
            recorded: Vec::new(),
            used: BTreeSet::new(),
            produced: Vec::new(),
        };
        let source = list_source();
        let target = dir.path().join("t");
        for bad in ["../escape.jpg", "/abs.jpg", "C:/x.jpg", "NUL", "./"] {
            let it = ListedFile {
                path: bad.into(),
                ..item(1, None)
            };
            assert!(ctx.fetch_listed(&source, &target, &it).is_err(), "{bad}");
        }
        assert_eq!(ctx.out(), dir.path().join("out"));
        ctx.fetch_listed(&source, &target, &item(1, None))
            .expect("first");
        assert!(
            ctx.fetch_listed(&source, &target, &item(1, None)).is_err(),
            "same path twice"
        );
    }

    // ---- `files` kind, early list check ----

    const A_URL: &str = "https://example.org/files/a.bin";
    const B_URL: &str = "https://example.org/files/b.bin";

    fn files_registry(extra: &str) -> String {
        format!(
            r#"
[[source]]
id = "set"
class = "stuff"
licence = "MIT"
origin = "listed"
profiles = ["small"]
kind = "files"
files = [
  {{ url = "{A_URL}", path = "dir/a.bin" }},
  {{ url = "{B_URL}", licence = "CC0-1.0", attribution = "Someone" }},{extra}
]
"#
        )
    }

    fn files_env(extra: &str) -> Env {
        let env = Env::new(false);
        std::fs::write(env.root.join("sources.toml"), files_registry(extra)).expect("w");
        env.fetcher
            .files
            .borrow_mut()
            .insert(A_URL.into(), b"AAA".to_vec());
        env.fetcher
            .files
            .borrow_mut()
            .insert(B_URL.into(), b"BBBB".to_vec());
        env
    }

    #[test]
    fn files_kind_pins_each_file_and_rebuilds_identically() {
        let env = files_env("");
        build(&env.opts("out", true), &env.fetcher).expect("pin");
        let lock = env.lock();
        assert!(
            lock.contains("\"path\": \"dir/a.bin\"")
                && lock.contains("\"attribution\": \"Someone\"")
        );
        let m1 = env.manifest("out");
        let m: Manifest = serde_json::from_str(&m1).expect("json");
        let files = &m.classes["stuff"].files;
        assert_eq!(
            files[0].path, "stuff/set/b.bin",
            "default name is the URL's last segment"
        );
        assert_eq!(files[0].licence, "CC0-1.0");
        assert_eq!(files[1].path, "stuff/set/dir/a.bin");
        assert_eq!(files[1].licence, "MIT");
        std::fs::remove_dir_all(env.root.join("out")).expect("rm");
        build(&env.opts("out2", false), &env.fetcher).expect("from lock");
        assert_eq!(m1, env.manifest("out2"));
    }

    #[test]
    fn files_kind_registry_and_lock_must_agree() {
        let env = files_env("");
        build(&env.opts("out", true), &env.fetcher).expect("pin");
        // A new URL in the registry has no pin: caught before any network traffic.
        let c = "https://example.org/files/c.bin";
        env.fetcher
            .files
            .borrow_mut()
            .insert(c.into(), b"C".to_vec());
        std::fs::write(
            env.root.join("sources.toml"),
            files_registry(&format!("\n  {{ url = \"{c}\" }},")),
        )
        .expect("w");
        let calls = env.fetcher.call_count();
        let err = build(&env.opts("out2", false), &env.fetcher).expect_err("unpinned url");
        assert!(format!("{err:#}").contains("--update-lock"));
        assert_eq!(env.fetcher.call_count(), calls);
        assert!(!env.root.join("out2").exists());
        // A renamed output path differs from the pinned one.
        std::fs::write(
            env.root.join("sources.toml"),
            files_registry("").replace("dir/a.bin", "dir/renamed.bin"),
        )
        .expect("w");
        let err = build(&env.opts("out2", false), &env.fetcher).expect_err("path differs");
        assert!(
            format!("{err:#}").contains("differs from the pins"),
            "{err:#}"
        );
    }

    #[test]
    fn colliding_listed_paths_are_rejected_when_the_registry_loads() {
        let env = files_env("");
        let bad = files_registry("").replace("dir/a.bin", "Dup.bin").replace(
            "{ url = \"https://example.org/files/b.bin\"",
            "{ path = \"dup.BIN\", url = \"https://example.org/files/b.bin\"",
        );
        std::fs::write(env.root.join("sources.toml"), bad).expect("w");
        let err = build(&env.opts("out", true), &env.fetcher).expect_err("collision");
        assert!(format!("{err:#}").contains("duplicate"), "{err:#}");
        assert_eq!(env.fetcher.call_count(), 0, "nothing fetched");
    }

    #[test]
    fn list_source_without_pins_fails_before_earlier_sources_download() {
        let env = files_env("");
        let text = format!(
            "{}\n{}",
            registry_text(false),
            files_registry("").replace("\"set\"", "\"set2\"")
        );
        std::fs::write(env.root.join("sources.toml"), text).expect("w");
        let sources = Registry::load(&env.root.join("sources.toml")).expect("registry");
        let picked = sources.select(Profile::Small, &[]).expect("select");
        let mut lock = Lock::default();
        // Pin everything except the list source's entries.
        for s in picked.iter().filter(|s| !s.spec.is_list()) {
            for url in s.spec.static_urls() {
                lock.replace_source(
                    Profile::Small,
                    &s.id,
                    vec![LockEntry::artifact(&s.id, url, 1, "ab".repeat(32))],
                );
            }
        }
        let err = preflight_pins(&picked, &lock, Profile::Small).expect_err("list unpinned");
        let msg = format!("{err:#}");
        assert!(msg.contains("set2") && msg.contains("(list)"), "{msg}");
    }
}
