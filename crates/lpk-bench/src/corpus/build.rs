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
use super::lock::{Lock, LockEntry, RunLock};
use super::manifest::{
    now_rfc3339, BuildInfo, Manifest, ManifestFile, Skipped, SourceAccount, Summary, Unavailable,
};
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
    /// The `git` program for git sources (`None`: `git` from `PATH`). Injectable for tests.
    pub git_program: Option<String>,
    /// Continue when listed files of a list source are gone (404/410) or no longer match their
    /// pin, leaving them out and recording them as unavailable. Default: fail.
    pub allow_unavailable: bool,
    /// The `ffmpeg` program for video encodes (`None`: `ffmpeg` from `PATH`). Injectable for tests.
    pub ffmpeg_program: Option<String>,
    /// `--repin`: source ids (or `all`) whose pins are forgotten and fetched again. Needs
    /// `update_lock`.
    pub repin: Vec<String>,
    /// `--list-only`: resolve and save the listings, download nothing (see [`list_only`]).
    pub list_only: bool,
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
    /// Files that extraction produced but that were gone when the manifest was written.
    pub missing: Vec<String>,
    /// Files whose bytes on disk no longer matched the extraction-time hash.
    pub altered: Vec<String>,
    /// Files and bytes per class and in total.
    pub summary: Summary,
    /// Listed files that could not be fetched or no longer match their pin; left out.
    pub unavailable: Vec<Unavailable>,
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
    /// Name or path of the `git` program.
    git_program: String,
    /// State of the derived kinds (see [`super::derive`]).
    pub(super) derive: super::derive::State,
    /// External tool versions seen by this run (`build-info.json`, key `tools`).
    tools: BTreeMap<String, String>,
    /// Listed files that were gone or no longer matched their pin (normal builds).
    unavailable: Vec<Unavailable>,
    /// `--allow-unavailable`: continue without listed files that are gone or changed.
    allow_unavailable: bool,
    /// `--update-lock`: the lock as pinned so far. It starts as the file's content, grows as
    /// artifacts are pinned and is saved to `lock_path` at checkpoints (see [`Ctx::flush`]).
    work: Lock,
    /// Where the working lock is saved; `Some` only under `--update-lock`.
    lock_path: Option<PathBuf>,
    /// Pins recorded since the working lock was last saved.
    unsaved: usize,
    /// Artifacts of the current source taken from the cache without a request.
    reused: usize,
    /// When the working lock was last saved.
    last_save: std::time::Instant,
    /// Sources named by `--repin`: their cached files are not trusted, everything is fetched.
    repinning: BTreeSet<String>,
}

/// Pins recorded between two saves of the lock while a list source is being pinned.
const CHECKPOINT_EVERY: usize = 16;
/// Also save when this long has passed since the last save (slow hosts, large files).
const CHECKPOINT_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
impl<'a> Ctx<'a> {
    /// The version recorded for external tool `name` by this context, if any.
    pub fn take_tool(&self, name: &str) -> Option<String> {
        self.tools.get(name).cloned()
    }

    /// A context over `fetcher` with an empty lock (leaked: tests only), for resolver tests.
    pub fn for_tests(fetcher: &'a dyn Fetcher, root: &Path, update_lock: bool) -> Ctx<'a> {
        let lock: &'static Lock = Box::leak(Box::default());
        Ctx {
            profile: Profile::Small,
            update_lock,
            out: root.join("out"),
            downloader: Downloader::new(
                fetcher,
                root.join("cache"),
                super::fetch::fake::fast_retry(),
                Profile::Small,
            ),
            lock,
            recorded: Vec::new(),
            used: BTreeSet::new(),
            produced: Vec::new(),
            git_program: "git".into(),
            derive: Default::default(),
            tools: BTreeMap::new(),
            unavailable: Vec::new(),
            allow_unavailable: false,
            work: Lock::default(),
            lock_path: None,
            unsaved: 0,
            reused: 0,
            last_save: std::time::Instant::now(),
            repinning: BTreeSet::new(),
        }
    }
}

impl Ctx<'_> {
    /// Name or path of the `git` program git sources run.
    pub fn git_program(&self) -> &str {
        &self.git_program
    }

    /// The download cache directory (scratch space for resolvers that need some).
    pub fn cache_dir(&self) -> &Path {
        self.downloader.cache_dir()
    }

    /// Remember the version of an external program for `build-info.json`.
    pub fn note_tool(&mut self, name: &str, version: &str) {
        self.tools.insert(name.to_string(), version.to_string());
    }

    /// GET an API response (never cached). Only a resolver running under `--update-lock` may
    /// call this; a normal build must make no API call.
    pub fn api_get(&self, source: &Source, url: &str, limit: u64) -> Result<Vec<u8>> {
        ensure!(
            self.update_lock,
            "source `{}`: API calls are only allowed with --update-lock",
            source.id
        );
        Ok(self.downloader.get_bytes(&source.id, url, limit)?)
    }

    /// Like [`Ctx::api_get`], also returning the response's `Retry-After`.
    pub fn api_get_response(
        &self,
        source: &Source,
        url: &str,
        limit: u64,
    ) -> Result<(Vec<u8>, Option<std::time::Duration>)> {
        ensure!(
            self.update_lock,
            "source `{}`: API calls are only allowed with --update-lock",
            source.id
        );
        Ok(self.downloader.get_response(&source.id, url, limit)?)
    }

    /// The downloader, for pacing helpers (`settle`, `backoff`).
    pub fn downloader(&self) -> &Downloader<'_> {
        &self.downloader
    }

    /// Pin (`--update-lock`) or verify (normal build) the commit of a git source. A normal
    /// build requires the lock to carry exactly the registry's commit.
    pub fn pin_commit(&mut self, source: &Source, url: &str, commit: &str) -> Result<()> {
        if self.update_lock {
            self.recorded
                .push(LockEntry::commit_pin(&source.id, url, commit));
            return Ok(());
        }
        let Some(pin) = self.lock.get(self.profile, &source.id, url) else {
            return Err(missing_pin(self.profile, &source.id, url).into());
        };
        self.used.insert((source.id.clone(), url.to_string()));
        ensure!(
            pin.commit.as_deref() == Some(commit),
            "source `{}`: the registry names commit {commit} but bench/corpus.lock pins {:?}. {}",
            source.id,
            pin.commit,
            repin_hint(self.profile)
        );
        Ok(())
    }

    /// The output root (`<out>`), e.g. for derivation steps reading other classes' files.
    pub fn out(&self) -> &Path {
        &self.out
    }

    /// What the derived kinds need to know about this run.
    pub fn derive_state(&self) -> &super::derive::State {
        &self.derive
    }

    /// Record that part of a derived source's output was left out (`skipped` in
    /// `build-info.json`).
    pub fn note_skip(&mut self, source: &str, reason: String) {
        self.derive.skipped.push((source.to_string(), reason));
    }

    /// Name or path of the `ffmpeg` program.
    pub fn ffmpeg_program(&self) -> &str {
        self.derive.ffmpeg.as_deref().unwrap_or("ffmpeg")
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
        // Under `--update-lock` an artifact that is already pinned (with a hash) is verified
        // against that pin: the cache is reused when it matches, otherwise the file is fetched
        // and must match. Changing a pin takes `--repin`.
        let existing: Option<LockEntry> = if self.update_lock {
            self.work
                .get(self.profile, &source.id, url)
                .filter(|p| p.bytes.is_some() && p.blake3.is_some())
                .cloned()
        } else {
            None
        };
        // A cached file whose SHA-1 equals the one the API lists for it is the listed file: take
        // it instead of downloading it again (this is what makes a re-resolved listing cheap).
        if self.update_lock && existing.is_none() && !self.repinning.contains(&source.id) {
            if let Some(want) = meta.and_then(|m| m.extra.get("sha1")) {
                let cached = self.downloader.cache_path(&source.id, url);
                let same = cached.is_file()
                    && sha1_file(&cached).is_ok_and(|got| want.eq_ignore_ascii_case(&got));
                if same {
                    let (bytes, blake3) = super::fetch::hash_file(&cached)
                        .map_err(|e| DownloadError::Io(format!("source `{}`: {e}", source.id)))?;
                    let a = Artifact {
                        path: cached,
                        bytes,
                        blake3,
                        cached: true,
                    };
                    self.reused += 1;
                    self.record_new(source, url, meta, &a)?;
                    return Ok(a);
                }
            }
        }
        let expect = if self.update_lock {
            match &existing {
                Some(pin) => Expect::Pinned(pin),
                None => Expect::Unpinned,
            }
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
        if let Some(pin) = existing {
            if a.cached {
                self.reused += 1;
            }
            self.recorded.push(pin);
            self.note_recorded()
                .map_err(|e| DownloadError::Io(format!("source `{}`: {e:#}", source.id)))?;
            return Ok(a);
        }
        if self.update_lock {
            if let Some(want) = meta.and_then(|m| m.extra.get("sha1")) {
                let got = sha1_file(&a.path)
                    .map_err(|e| DownloadError::Io(format!("source `{}`: {e}", source.id)))?;
                if !want.eq_ignore_ascii_case(&got) {
                    return Err(DownloadError::Mismatch(format!(
                        "source `{}`: {url} has SHA-1 {got} but the API listed {want}; the file \
                         changed after it was listed (a pin run lists the source again by \
                         itself and keeps the other pins)",
                        source.id
                    )));
                }
            }
        }
        if self.update_lock {
            self.record_new(source, url, meta, &a)?;
        }
        Ok(a)
    }

    /// Record the pin of a freshly verified artifact (`--update-lock`).
    fn record_new(
        &mut self,
        source: &Source,
        url: &str,
        meta: Option<&ListedFile>,
        a: &Artifact,
    ) -> Result<(), DownloadError> {
        let mut entry = LockEntry::artifact(&source.id, url, a.bytes, a.blake3.clone());
        if let Some(m) = meta {
            entry.path = Some(m.path.clone());
            entry.licence = m.licence.clone();
            entry.attribution = m.attribution.clone();
            entry.extra = m.extra.clone();
        }
        self.recorded.push(entry);
        self.note_recorded()
            .map_err(|e| DownloadError::Io(format!("source `{}`: {e:#}", source.id)))
    }

    /// Count a recorded pin and save the working lock every [`CHECKPOINT_EVERY`] pins or
    /// [`CHECKPOINT_AFTER`], whichever comes first.
    fn note_recorded(&mut self) -> Result<()> {
        self.unsaved += 1;
        if self.unsaved >= CHECKPOINT_EVERY || self.last_save.elapsed() >= CHECKPOINT_AFTER {
            self.flush()?;
        }
        Ok(())
    }

    /// `--update-lock`: merge the pins recorded for the current source into the working lock
    /// (other entries, including the unhashed listing of a list source, stay) and save it
    /// atomically. A no-op in normal builds and when nothing is unsaved.
    pub fn flush(&mut self) -> Result<()> {
        let Some(path) = self.lock_path.clone() else {
            return Ok(());
        };
        if self.unsaved == 0 {
            return Ok(());
        }
        for e in &self.recorded {
            self.work.upsert(self.profile, e.clone());
        }
        self.work.save(&path)?;
        self.unsaved = 0;
        self.last_save = std::time::Instant::now();
        Ok(())
    }

    /// `--update-lock`: the source is complete; its pins replace everything recorded for it and
    /// the lock is saved. Reports how many artifacts came from the cache.
    fn finish_source(&mut self, source_id: &str) -> Result<()> {
        let entries = std::mem::take(&mut self.recorded);
        if self.reused > 0 {
            eprintln!(
                "  {} of {} pinned file(s) reused from the cache, no request",
                self.reused,
                entries.len()
            );
        }
        self.reused = 0;
        let Some(path) = self.lock_path.clone() else {
            return Ok(());
        };
        self.work.replace_source(self.profile, source_id, entries);
        self.work.save(&path)?;
        self.unsaved = 0;
        Ok(())
    }

    /// `--repin`: forget the pins of `ids` and save, so that a plain `--update-lock` after an
    /// interruption continues the new pin instead of resurrecting the old one.
    fn forget_pins(&mut self, ids: &[&str]) -> Result<()> {
        for id in ids {
            self.work.replace_source(self.profile, id, Vec::new());
            self.repinning.insert((*id).to_string());
        }
        if let Some(path) = self.lock_path.clone() {
            self.work.save(&path)?;
        }
        Ok(())
    }

    /// `--update-lock`: the listing saved in the working lock for a list source (from an earlier,
    /// possibly interrupted run), if there is one. Sorted by URL.
    fn saved_listing(&self, source: &Source) -> Result<Option<Vec<ListedFile>>> {
        let pins = self.work.entries(self.profile, &source.id);
        if pins.is_empty() {
            return Ok(None);
        }
        Ok(Some(listing_from(source, &pins)?))
    }

    /// `--update-lock`: save a freshly resolved listing in the lock before any download. The
    /// entries carry the API's metadata but no hash yet; a normal build refuses them.
    ///
    /// A hashed pin of the same URL with the same API sha1 is kept (its hash is carried over),
    /// so resolving a source again re-downloads only what changed.
    fn save_listing(&mut self, source: &Source, items: &[ListedFile]) -> Result<()> {
        let Some(path) = self.lock_path.clone() else {
            return Ok(());
        };
        let entries = items
            .iter()
            .map(|i| {
                let kept = self
                    .work
                    .get(self.profile, &source.id, &i.url)
                    .filter(|o| o.blake3.is_some() && o.bytes.is_some())
                    .filter(|o| o.extra.get("sha1") == i.extra.get("sha1"));
                LockEntry {
                    attribution: i.attribution.clone(),
                    blake3: kept.and_then(|o| o.blake3.clone()),
                    bytes: kept.and_then(|o| o.bytes),
                    commit: None,
                    extra: i.extra.clone(),
                    licence: i.licence.clone(),
                    path: Some(i.path.clone()),
                    source: source.id.clone(),
                    url: i.url.clone(),
                }
            })
            .collect();
        self.work.replace_source(self.profile, &source.id, entries);
        self.work.save(&path)
    }

    /// `--update-lock`, list kinds: the listing to pin. A listing saved in the lock is used as it
    /// is, without an API call, when it has `count` files and was made for the same list
    /// specification (`fingerprint`; a pin without one is accepted); otherwise the source is
    /// resolved and the listing saved. Returns the listing and whether it was resolved now.
    fn listing_for_update(
        &mut self,
        source: &Source,
        fingerprint: &str,
        count: usize,
        resolve: &dyn Fn(&mut Ctx<'_>) -> Result<Vec<ListedFile>>,
    ) -> Result<(Vec<ListedFile>, bool)> {
        let saved = self.saved_listing(source)?;
        if let Some(saved) = &saved {
            if let Some(other) = saved
                .iter()
                .filter_map(|i| i.extra.get("spec"))
                .find(|s| *s != fingerprint)
            {
                bail!(
                    "source `{id}`: its registry entry changed since the listing in \
                     bench/corpus.lock was saved (list specification {other}, now {fingerprint}). \
                     Run with `--update-lock --repin {id}` to list and download it again",
                    id = source.id
                );
            }
            if saved.len() == count {
                eprintln!(
                    "  continuing from the listing saved in the lock ({} files), no API call",
                    saved.len()
                );
                self.check_listing(source, saved)?;
                return Ok((saved.clone(), false));
            }
            eprintln!(
                "  the saved listing has {} files but the registry wants {count}; resolving again",
                saved.len()
            );
        }
        Ok((self.relist(source, fingerprint, resolve)?, true))
    }

    /// Resolve a list source through its API and save the listing (keeping matching hashed pins).
    fn relist(
        &mut self,
        source: &Source,
        fingerprint: &str,
        resolve: &dyn Fn(&mut Ctx<'_>) -> Result<Vec<ListedFile>>,
    ) -> Result<Vec<ListedFile>> {
        let mut items = resolve(self)?;
        for i in &mut items {
            i.extra.insert("spec".to_string(), fingerprint.to_string());
        }
        self.check_listing(source, &items)?;
        // What this run pinned so far belongs to the old listing: merge it, then let the new
        // listing keep what still matches.
        for e in std::mem::take(&mut self.recorded) {
            self.work.upsert(self.profile, e);
        }
        self.unsaved = 0;
        self.save_listing(source, &items)?;
        Ok(items)
    }

    /// The file list of a list-type source in a normal build: its lock entries, sorted by URL.
    /// An empty list is an error (the source was never pinned), an unfinished one too.
    pub fn listed_pins(&self, source: &Source) -> Result<Vec<ListedFile>> {
        let pins = self.lock.entries(self.profile, &source.id);
        if pins.is_empty() {
            return Err(missing_pin(self.profile, &source.id, "(list)").into());
        }
        check_finished(self.profile, source, &pins)?;
        let listing = listing_from(source, &pins)?;
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

/// SHA-1 of a file as lower-case hex (used only to check the API's `sha1` when pinning).
fn sha1_file(path: &Path) -> std::io::Result<String> {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Fetch every item of a list source. Under `--update-lock` any failure is fatal. In a normal
/// build, for the API-resolved kinds only (`tolerant`), two conditions make a file
/// "unavailable": the server says it is gone (404/410) or the bytes no longer match the pin.
/// By default the build then fails, listing each such file and the ways forward;
/// `--allow-unavailable` leaves them out and records them. Timeouts, 5xx, connection failures
/// and refused `Retry-After` waits are always fatal, and so is every failure of a static source.
fn fetch_items(
    ctx: &mut Ctx<'_>,
    source: &Source,
    dir: &Path,
    items: &[ListedFile],
    tolerant: bool,
) -> Result<Vec<ManifestFile>> {
    let mut made = Vec::with_capacity(items.len());
    let mut missing: Vec<Unavailable> = Vec::new();
    for item in items {
        match ctx.fetch_listed(source, dir, item) {
            Ok(f) => made.push(f),
            Err(e) => {
                let gone_or_changed = matches!(
                    e.downcast_ref::<DownloadError>(),
                    Some(DownloadError::Gone(_) | DownloadError::Mismatch(_))
                );
                if ctx.update_lock || !tolerant || !gone_or_changed {
                    return Err(e);
                }
                missing.push(Unavailable {
                    reason: format!("{e:#}"),
                    source: source.id.clone(),
                    url: item.url.clone(),
                });
            }
        }
    }
    if missing.is_empty() {
        return Ok(made);
    }
    if !ctx.allow_unavailable {
        let list: Vec<String> = missing
            .iter()
            .map(|u| format!("  {}: {}", u.url, u.reason))
            .collect();
        bail!(
            "source `{}`: {} listed file(s) are gone upstream or no longer match their pin:\n{}\n\
             Either re-pin: `lpk-bench corpus build --profile <p> --update-lock` fetches what the \
             cache lacks and, for a file that is gone or changed, lists the source again and \
             replaces just those pins (`--repin <source-id>` starts over and re-downloads \
             everything); or build without them with `--allow-unavailable` (they are left out \
             and recorded under `unavailable` in build-info.json)",
            source.id,
            missing.len(),
            list.join("\n")
        );
    }
    for u in &missing {
        eprintln!("  unavailable: {}: {}", u.url, u.reason);
    }
    ctx.unavailable.extend(missing);
    Ok(made)
}

fn missing_pin(profile: Profile, source: &str, url: &str) -> DownloadError {
    DownloadError::Mismatch(format!(
        "source `{source}`: no entry for {url} in bench/corpus.lock (profile `{}`). {}",
        profile.name(),
        repin_hint(profile)
    ))
}

/// The listing a list source's pins describe (every pin needs its `path`).
fn listing_from(source: &Source, pins: &[&LockEntry]) -> Result<Vec<ListedFile>> {
    pins.iter()
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
        .collect()
}

/// A normal build refuses a listing in which some pin has no hash: its pin run was interrupted.
fn check_finished(profile: Profile, source: &Source, pins: &[&LockEntry]) -> Result<()> {
    let open = pins
        .iter()
        .filter(|e| e.blake3.is_none() && e.commit.is_none())
        .count();
    ensure!(
        open == 0,
        "source `{id}`: the pin run for profile `{p}` was not finished: {open} of {n} listed \
         file(s) in bench/corpus.lock have no hash yet. Run `lpk-bench corpus build --profile {p} \
         --update-lock --only {class}` again; it continues from the saved listing, makes no API \
         call and downloads only what is missing.",
        id = source.id,
        p = profile.name(),
        n = pins.len(),
        class = source.class
    );
    Ok(())
}

/// Before any network traffic: every static URL needs a pin, and a list source needs a
/// non-empty, finished pinned listing (otherwise it would fail after earlier sources have
/// downloaded).
fn preflight_pins(sources: &[&Source], lock: &Lock, profile: Profile) -> Result<()> {
    for s in sources {
        if s.spec.is_list() {
            let pins = lock.entries(profile, &s.id);
            if pins.is_empty() {
                return Err(missing_pin(profile, &s.id, "(list)").into());
            }
            check_finished(profile, s, &pins)?;
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

/// Refuse to build derived sources whose input classes have no selected source earlier in this
/// run. With `--only` an input class that is not selected at all may instead already be in
/// `<out>`. A derived source that is itself `optional` passes: it is skipped, with a reason, when
/// its turn comes (see [`check_inputs_built`]).
fn check_derived_inputs(
    sources: &[&Source],
    out: &Path,
    partial: bool,
    profile: Profile,
) -> Result<()> {
    for (i, s) in sources.iter().enumerate() {
        if s.optional {
            continue;
        }
        let missing: Vec<&str> = s
            .inputs
            .iter()
            .map(String::as_str)
            .filter(|class| !sources[..i].iter().any(|b| b.class == *class))
            .filter(|class| {
                !(partial
                    && !sources.iter().any(|b| b.class == *class)
                    && dir_has_files(&out.join(class)))
            })
            .collect();
        if !missing.is_empty() {
            if partial {
                bail!(
                    "source `{}` (class `{}`) is derived from class(es) {} which are neither \
                     built earlier in this run nor present in {}; add them to --only or build \
                     them first",
                    s.id,
                    s.class,
                    missing.join(", "),
                    out.display()
                );
            }
            bail!(
                "source `{}` (class `{}`) is derived from class(es) {} which have no source \
                 before it in profile `{}`",
                s.id,
                s.class,
                missing.join(", "),
                profile.name()
            );
        }
    }
    Ok(())
}

/// Before a derived source runs: each input class must have produced something (a source of the
/// class built in this run, or, for a class this run does not select, files already in `<out>`).
/// Fails with a [`Skip`](super::derive::Skip) error, which an `optional` source tolerates.
fn check_inputs_built(ctx: &Ctx<'_>, source: &Source) -> Result<()> {
    let st = ctx.derive_state();
    for class in &source.inputs {
        let ok = if st.run_classes.contains(class) {
            st.built.iter().any(|(c, _)| c == class)
        } else {
            dir_has_files(&ctx.out().join(class))
        };
        if !ok {
            return Err(super::derive::Skip(format!(
                "input class `{class}` has no files: every source of it was skipped or none \
                 applies to this build"
            ))
            .into());
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

/// The source ids `--repin` names (`all` = every selected source). Needs `--update-lock`; an id
/// that is not among the selected sources is an error.
fn resolve_repin<'a>(
    repin: &[String],
    sources: &[&'a Source],
    update_lock: bool,
) -> Result<Vec<&'a str>> {
    if repin.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(update_lock, "--repin needs --update-lock");
    if repin.iter().any(|r| r == "all") {
        return Ok(sources.iter().map(|s| s.id.as_str()).collect());
    }
    let mut ids = Vec::new();
    for r in repin {
        match sources.iter().find(|s| &s.id == r) {
            Some(s) => ids.push(s.id.as_str()),
            None => bail!(
                "--repin names `{r}`, which is not a source of this build (sources: {})",
                sources
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    Ok(ids)
}

/// The shared state of one run. `snapshot` is the lock as read (normal builds verify against
/// it); under `--update-lock` the working lock starts as `lock`.
fn new_ctx<'a>(
    opts: &BuildOptions,
    registry: &Registry,
    fetcher: &'a dyn Fetcher,
    snapshot: &'a Lock,
    lock: &Lock,
) -> Ctx<'a> {
    Ctx {
        profile: opts.profile,
        update_lock: opts.update_lock,
        out: opts.out.clone(),
        downloader: Downloader::new(fetcher, opts.cache.clone(), opts.retry, opts.profile)
            .with_politeness(Politeness::from_specs(&registry.hosts)),
        lock: snapshot,
        recorded: Vec::new(),
        used: BTreeSet::new(),
        produced: Vec::new(),
        allow_unavailable: opts.allow_unavailable,
        git_program: opts
            .git_program
            .clone()
            .unwrap_or_else(|| "git".to_string()),
        derive: Default::default(),
        tools: BTreeMap::new(),
        unavailable: Vec::new(),
        work: if opts.update_lock {
            lock.clone()
        } else {
            Lock::default()
        },
        lock_path: opts.update_lock.then(|| opts.lock_path.clone()),
        unsaved: 0,
        reused: 0,
        last_save: std::time::Instant::now(),
        repinning: BTreeSet::new(),
    }
}

/// What `--list-only` found for one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingSummary {
    pub source: String,
    /// Files in the listing.
    pub files: usize,
    /// Sum of the API's sizes (`None` when the API does not report sizes, or the saved listing
    /// predates them).
    pub bytes: Option<u64>,
}

/// `--list-only` (with `--update-lock`): resolve and save the listing of every API-listed source
/// of the selection (a listing already saved is reused unless `--repin` names the source),
/// download nothing, and report files and API sizes per source.
pub fn list_only(opts: &BuildOptions, fetcher: &dyn Fetcher) -> Result<Vec<ListingSummary>> {
    ensure!(opts.update_lock, "--list-only needs --update-lock");
    let registry = Registry::load(&opts.sources_path)?;
    let sources = registry.select(opts.profile, &opts.only)?;
    let _run_lock = RunLock::acquire(&opts.lock_path)?;
    let lock = Lock::load(&opts.lock_path)?;
    let repin_ids = resolve_repin(&opts.repin, &sources, true)?;
    std::fs::create_dir_all(&opts.cache)
        .with_context(|| format!("creating {}", opts.cache.display()))?;
    let snapshot = lock.clone();
    let mut ctx = new_ctx(opts, &registry, fetcher, &snapshot, &lock);
    if !repin_ids.is_empty() {
        ctx.forget_pins(&repin_ids)?;
    }
    let mut out = Vec::new();
    for source in &sources {
        let listed = match &source.spec {
            SourceSpec::CommonsPhotos(spec) => {
                eprintln!("[{}] {}", source.class, source.id);
                let fp = list_fingerprint(&source.spec);
                Some(ctx.listing_for_update(source, &fp, spec.count, &|c| {
                    super::commons::resolve(c, source, spec)
                })?)
            }
            SourceSpec::ArxivPapers(spec) => {
                eprintln!("[{}] {}", source.class, source.id);
                let fp = list_fingerprint(&source.spec);
                Some(ctx.listing_for_update(source, &fp, spec.count, &|c| {
                    super::arxiv::resolve(c, source, spec)
                })?)
            }
            _ => None,
        };
        let Some((items, _)) = listed else {
            continue;
        };
        let sizes: Vec<Option<u64>> = items
            .iter()
            .map(|i| i.extra.get("size").and_then(|s| s.parse().ok()))
            .collect();
        out.push(ListingSummary {
            source: source.id.clone(),
            files: items.len(),
            bytes: sizes
                .iter()
                .all(Option::is_some)
                .then(|| sizes.iter().flatten().sum()),
        });
    }
    Ok(out)
}

/// Version of the selection rules of the API-listed kinds (what a resolver accepts). Bump it when
/// `commons.rs` or `arxiv.rs` change which files they select, so saved listings made under the
/// old rules are refused.
const LIST_RULES_VERSION: u32 = 1;

/// Fingerprint of everything in a list source's registry entry that affects which files it
/// lists, plus [`LIST_RULES_VERSION`]. Stored as `extra.spec` of every pin the pin run writes;
/// a pin without it (made before fingerprints existed) is accepted as it is.
fn list_fingerprint(spec: &SourceSpec) -> String {
    let text = match spec {
        SourceSpec::CommonsPhotos(c) => format!(
            "v{LIST_RULES_VERSION}|commons|{}|{}|{}|{}",
            c.category, c.count, c.min_bytes, c.max_bytes
        ),
        SourceSpec::ArxivPapers(a) => format!(
            "v{LIST_RULES_VERSION}|arxiv|{}|{}|{:?}|{}",
            a.from, a.until, a.set, a.count
        ),
        _ => String::new(),
    };
    blake3::hash(text.as_bytes()).to_hex().as_str()[..16].to_string()
}

/// Build the corpus. `fetcher` supplies bytes for URLs (HTTPS in production, memory in tests).
pub fn build(opts: &BuildOptions, fetcher: &dyn Fetcher) -> Result<BuildReport> {
    let registry = Registry::load(&opts.sources_path)?;
    let sources = registry.select(opts.profile, &opts.only)?;
    if sources.is_empty() {
        bail!("no sources apply to profile `{}`", opts.profile.name());
    }
    let partial = !opts.only.is_empty();
    ensure!(
        !opts.list_only,
        "--list-only is not a build; use `list_only`"
    );
    // A pin run is the only writer of the lock for its whole duration.
    let _run_lock = opts
        .update_lock
        .then(|| RunLock::acquire(&opts.lock_path))
        .transpose()?;
    let lock = Lock::load(&opts.lock_path)?;
    let repin_ids = resolve_repin(&opts.repin, &sources, opts.update_lock)?;

    // Fail before touching anything if the request cannot be satisfied.
    check_derived_inputs(&sources, &opts.out, partial, opts.profile)?;
    if !opts.update_lock {
        preflight_pins(&sources, &lock, opts.profile)?;
    }

    prepare_out(&opts.out, &opts.cache)?;
    let lock_snapshot = lock.clone();
    let mut ctx = new_ctx(opts, &registry, fetcher, &lock_snapshot, &lock);
    if !repin_ids.is_empty() {
        eprintln!("re-pinning from scratch: {}", repin_ids.join(", "));
        ctx.forget_pins(&repin_ids)?;
    }
    ctx.derive = super::derive::State {
        built: Vec::new(),
        run_classes: sources.iter().map(|s| s.class.clone()).collect(),
        derived_ids: registry
            .sources
            .iter()
            .filter(|s| !s.inputs.is_empty())
            .map(|s| s.id.clone())
            .collect(),
        ffmpeg: opts.ffmpeg_program.clone(),
        skipped: Vec::new(),
        class_sources: registry
            .sources
            .iter()
            .filter(|s| s.profiles.contains(&opts.profile))
            .fold(
                BTreeMap::new(),
                |mut m: BTreeMap<String, BTreeSet<String>>, s| {
                    m.entry(s.class.clone()).or_default().insert(s.id.clone());
                    m
                },
            ),
    };

    let mut skipped = Vec::new();
    let mut built_ids: Vec<&str> = Vec::new();
    let mut expected: Vec<(String, String, u64)> = Vec::new();
    for source in &sources {
        eprintln!("[{}] {} ({})", source.class, source.id, source.origin);
        let dir = opts.out.join(&source.class).join(&source.id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clearing {}", dir.display()))?;
        }
        std::fs::create_dir_all(&dir)?;
        ctx.recorded.clear();
        ctx.reused = 0;
        let built =
            check_inputs_built(&ctx, source).and_then(|()| build_source(&mut ctx, source, &dir));
        match built {
            Ok(files) => {
                eprintln!("  {} files", files.len());
                ctx.derive
                    .built
                    .push((source.class.clone(), source.id.clone()));
                expected.push((source.id.clone(), source.class.clone(), files.len() as u64));
                ctx.produced
                    .extend(files.into_iter().map(|f| (source.class.clone(), f)));
                ctx.finish_source(&source.id)?;
                built_ids.push(&source.id);
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                // Keep the progress of an interrupted pin run.
                if let Err(fe) = ctx.flush() {
                    eprintln!("warning: could not save the lock: {fe:#}");
                }
                let tolerated = source.optional
                    && (matches!(
                        e.downcast_ref::<DownloadError>(),
                        Some(DownloadError::Fetch(_) | DownloadError::Gone(_))
                    ) || e.downcast_ref::<super::derive::Skip>().is_some());
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

    skipped.extend(
        std::mem::take(&mut ctx.derive.skipped)
            .into_iter()
            .map(|(source, reason)| Skipped { reason, source }),
    );
    let mut unused_pins = Vec::new();
    if opts.update_lock {
        eprintln!("lock {} is up to date", opts.lock_path.display());
    } else {
        for id in &built_ids {
            unused_pins.extend(ctx.unused(id));
        }
    }

    let (found, accounting) = account_for(&opts.out, std::mem::take(&mut ctx.produced), &expected);
    let missing: Vec<String> = accounting
        .iter()
        .flat_map(|a| a.missing.iter().cloned())
        .collect();
    let altered: Vec<String> = accounting
        .iter()
        .flat_map(|a| a.altered.iter().cloned())
        .collect();
    for m in &missing {
        eprintln!("missing after extraction (not in the manifest): {m}");
    }
    for m in &altered {
        eprintln!("altered after extraction (not in the manifest, will be removed): {m}");
    }
    let manifest = Manifest::new(opts.profile, found);
    let summary = Summary::of(&manifest);
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
        accounting,
        summary: summary.clone(),
        tools: std::mem::take(&mut ctx.tools),
        unavailable: ctx.unavailable.clone(),
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
        missing,
        altered,
        summary,
        unavailable: std::mem::take(&mut ctx.unavailable),
    })
}

/// Expected-versus-found accounting (D-14): keep the produced files that still exist below
/// `out` with the recorded size *and* hash (the bytes on disk are re-hashed, so antivirus that
/// cleans a file in place without changing its length is caught), and report per source what is
/// gone or altered. Altered files stay on disk but out of the manifest, so a full build's
/// clean-up removes them. `expected` lists `(source, class, files produced)` for every built
/// source, in build order.
fn account_for(
    out: &Path,
    produced: Vec<(String, ManifestFile)>,
    expected: &[(String, String, u64)],
) -> (Vec<(String, ManifestFile)>, Vec<SourceAccount>) {
    let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut altered: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut found = Vec::with_capacity(produced.len());
    for (class, file) in produced {
        let mut path = out.to_path_buf();
        path.extend(file.path.split('/'));
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() == file.bytes => match super::fetch::hash_file(&path) {
                Ok((_, h)) if h == file.blake3 => found.push((class, file)),
                _ => altered
                    .entry(file.source.clone())
                    .or_default()
                    .push(file.path),
            },
            Ok(m) if m.is_file() => altered
                .entry(file.source.clone())
                .or_default()
                .push(file.path),
            _ => missing
                .entry(file.source.clone())
                .or_default()
                .push(file.path),
        }
    }
    let accounts = expected
        .iter()
        .map(|(source, class, n)| {
            let mut gone = missing.remove(source).unwrap_or_default();
            let mut changed = altered.remove(source).unwrap_or_default();
            gone.sort();
            changed.sort();
            SourceAccount {
                class: class.clone(),
                expected: *n,
                found: n.saturating_sub((gone.len() + changed.len()) as u64),
                missing: gone,
                altered: changed,
                source: source.clone(),
            }
        })
        .collect();
    (found, accounts)
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
            sel.skip_links = spec.skip_links;
            if let Some(max) = spec.max_extracted_bytes {
                sel.ceiling = max;
            }
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
            fetch_items(ctx, source, dir, &items, false)
        }
        SourceSpec::CommonsPhotos(spec) => {
            let fp = list_fingerprint(&source.spec);
            build_listed(ctx, source, dir, &fp, spec.count, &|c| {
                super::commons::resolve(c, source, spec)
            })
        }
        SourceSpec::ArxivPapers(spec) => {
            let fp = list_fingerprint(&source.spec);
            build_listed(ctx, source, dir, &fp, spec.count, &|c| {
                super::arxiv::resolve(c, source, spec)
            })
        }
        SourceSpec::GitRepo(spec) => super::gitsrc::build(ctx, source, spec, dir),
        _ => super::derive::build(ctx, source, dir),
    }
}

/// A list kind: under `--update-lock` the resolver calls the API and returns the listing;
/// otherwise the listing is the source's lock entries and no API is called. Either way each file
/// goes through [`Ctx::fetch_listed`].
///
/// Under `--update-lock` a listing already saved in the lock (from an earlier, possibly
/// interrupted run) is used as it is, without an API call, as long as it has `count` files; a
/// freshly resolved listing is saved (unhashed) before the first download. `--repin` forgets the
/// saved listing first. A saved listing made for another list specification (`fingerprint`) is an
/// error naming `--repin`.
///
/// If a listed file turns out to be gone (404/410) or no longer matches the API's sha1 (or its
/// pin), the saved listing is stale: the source is resolved again in the same run (at most
/// [`MAX_RESOLVES`] times), every hashed pin whose URL and sha1 reappear is kept, a cached file
/// with the listed sha1 is taken without a download, and only the rest is fetched.
fn build_listed(
    ctx: &mut Ctx<'_>,
    source: &Source,
    dir: &Path,
    fingerprint: &str,
    count: usize,
    resolve: &dyn Fn(&mut Ctx<'_>) -> Result<Vec<ListedFile>>,
) -> Result<Vec<ManifestFile>> {
    if !ctx.update_lock() {
        let items = ctx.listed_pins(source)?;
        return fetch_items(ctx, source, dir, &items, true);
    }
    let (mut items, fresh) = ctx.listing_for_update(source, fingerprint, count, resolve)?;
    let mut resolves = u32::from(fresh);
    loop {
        let err = match fetch_items(ctx, source, dir, &items, true) {
            Ok(files) => return Ok(files),
            Err(e) => e,
        };
        let stale = matches!(
            err.downcast_ref::<DownloadError>(),
            Some(DownloadError::Gone(_) | DownloadError::Mismatch(_))
        );
        if !stale {
            return Err(err);
        }
        if resolves >= MAX_RESOLVES {
            return Err(err.context(format!(
                "source `{}`: the listing went stale and was resolved {resolves} time(s) in \
                 this run; run `--update-lock` again to continue, or `--update-lock --repin {}` \
                 to start over",
                source.id, source.id
            )));
        }
        eprintln!("  {err:#}\n  the saved listing is stale; listing the source again");
        // Files copied for the old listing must not clash with the new one.
        std::fs::remove_dir_all(dir).with_context(|| format!("clearing {}", dir.display()))?;
        std::fs::create_dir_all(dir)?;
        items = ctx.relist(source, fingerprint, resolve)?;
        resolves += 1;
    }
}

/// Times a pin run resolves one source (the first listing included) before giving up.
const MAX_RESOLVES: u32 = 3;

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
                git_program: None,
                allow_unavailable: false,
                ffmpeg_program: None,
                repin: Vec::new(),
                list_only: false,
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
        // A plain --update-lock keeps what is pinned: it does not adopt the changed bytes.
        let err = build(&env.opts("out", true), &env.fetcher).expect_err("pin is kept");
        assert!(format!("{err:#}").contains("--repin"), "{err:#}");
        let mut o = env.opts("out", true);
        o.repin = vec!["blob".into()];
        build(&o, &env.fetcher).expect("repin");
        build(&env.opts("out", false), &env.fetcher).expect("now passes");
    }

    const N_FILES: usize = 40;

    fn many_files_env() -> Env {
        let env = Env::new(false);
        let mut toml = String::from(
            "[[source]]\nid = \"many\"\nclass = \"lots\"\nlicence = \"MIT\"\norigin = \"t\"\n\
             profiles = [\"small\"]\nkind = \"files\"\nfiles = [\n",
        );
        for i in 0..N_FILES {
            toml.push_str(&format!(
                "  {{ url = \"https://example.org/m/f{i:02}.bin\" }},\n"
            ));
            env.fetcher.files.borrow_mut().insert(
                format!("https://example.org/m/f{i:02}.bin"),
                vec![i as u8; 7],
            );
        }
        toml.push_str("]\n");
        std::fs::write(env.root.join("sources.toml"), toml).expect("w");
        env
    }

    #[test]
    fn an_interrupted_static_pin_resumes_and_downloads_only_the_rest() {
        let reference = many_files_env();
        build(&reference.opts("out", true), &reference.fetcher).expect("uninterrupted");
        let want = reference.lock();

        let env = many_files_env();
        let k = 29;
        let broken = format!("https://example.org/m/f{k:02}.bin");
        let saved = env
            .fetcher
            .files
            .borrow_mut()
            .remove(&broken)
            .expect("file");
        assert!(build(&env.opts("out", true), &env.fetcher).is_err());
        assert_eq!(env.fetcher.call_count(), k + 1);
        // The progress is in the lock, and a normal build refuses the incomplete pin.
        assert_eq!(env.lock().matches("\"blake3\"").count(), k);
        assert!(build(&env.opts("n", false), &env.fetcher).is_err());

        env.fetcher.files.borrow_mut().insert(broken, saved);
        env.fetcher.calls.borrow_mut().clear();
        build(&env.opts("out", true), &env.fetcher).expect("resume");
        assert_eq!(
            env.fetcher.call_count(),
            N_FILES - k,
            "only the rest is requested"
        );
        assert_eq!(env.lock(), want, "byte-identical to the uninterrupted lock");

        // --repin starts over: everything is requested again.
        env.fetcher.calls.borrow_mut().clear();
        let mut o = env.opts("out", true);
        o.repin = vec!["many".into()];
        build(&o, &env.fetcher).expect("repin");
        assert_eq!(env.fetcher.call_count(), N_FILES);
        assert_eq!(env.lock(), want);
        // A normal build is unchanged by all this.
        env.fetcher.calls.borrow_mut().clear();
        build(&env.opts("n", false), &env.fetcher).expect("normal");
        assert_eq!(
            env.fetcher.call_count(),
            0,
            "served from the verified cache"
        );
    }

    #[test]
    fn the_lock_is_saved_after_every_source() {
        let env = Env::new(false);
        env.fetcher.files.borrow_mut().remove(OPT_URL);
        // `maybe` (the last source) fails fatally; the sources before it are already pinned.
        assert!(build(&env.opts("out", true), &env.fetcher).is_err());
        let lock = env.lock();
        assert!(
            lock.contains("pack.zip") && lock.contains("blob.bin"),
            "{lock}"
        );
        assert!(!lock.contains("optional.bin"));
        assert!(!env.root.join("corpus.lock.tmp").exists());
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
            git_program: "git".into(),
            derive: Default::default(),
            tools: BTreeMap::new(),
            unavailable: Vec::new(),
            allow_unavailable: false,
            work: Lock::default(),
            lock_path: None,
            unsaved: 0,
            reused: 0,
            last_save: std::time::Instant::now(),
            repinning: BTreeSet::new(),
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
            git_program: "git".into(),
            derive: Default::default(),
            tools: BTreeMap::new(),
            unavailable: Vec::new(),
            allow_unavailable: false,
            work: Lock::default(),
            lock_path: None,
            unsaved: 0,
            reused: 0,
            last_save: std::time::Instant::now(),
            repinning: BTreeSet::new(),
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

    #[test]
    fn missing_files_are_accounted_not_fatal_and_summary_is_written() {
        let env = Env::new(false);
        let report = build(&env.opts("out", true), &env.fetcher).expect("build");
        assert!(report.missing.is_empty());
        let info: BuildInfo =
            serde_json::from_str(&env.read("out", "build-info.json")).expect("json");
        assert_eq!(info.summary.total_files, 4);
        assert_eq!(info.summary.total_bytes, 23);
        assert_eq!(info.summary.classes["beta"].files, 2);
        assert!(info
            .accounting
            .iter()
            .all(|a| a.expected == a.found && a.missing.is_empty()));
        assert_eq!(report.summary, info.summary);

        // A file that vanished after extraction (antivirus) is listed, not an error.
        let m: Manifest = serde_json::from_str(&env.manifest("out")).expect("json");
        let produced: Vec<(String, ManifestFile)> = m
            .classes
            .iter()
            .flat_map(|(c, e)| e.files.iter().map(move |f| (c.clone(), f.clone())))
            .collect();
        std::fs::remove_file(env.root.join("out/beta/pack/a.txt")).expect("rm");
        std::fs::write(env.root.join("out/beta/pack/z/last.txt"), b"x").expect("resize");
        let expected = [
            ("pack".to_string(), "beta".to_string(), 2),
            ("blob".to_string(), "alpha".to_string(), 1),
        ];
        let (found, accounts) = account_for(&env.root.join("out"), produced, &expected);
        let pack = &accounts[0];
        assert_eq!((pack.expected, pack.found), (2, 0));
        assert_eq!(pack.missing, ["beta/pack/a.txt"]);
        assert_eq!(
            pack.altered,
            ["beta/pack/z/last.txt"],
            "different size is altered"
        );
        assert_eq!((accounts[1].expected, accounts[1].found), (1, 1));
        assert!(found.iter().all(|(_, f)| f.source != "pack"));
    }

    #[test]
    fn same_length_in_place_changes_are_altered_and_dropped_from_the_manifest() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("build");
        let full = env.manifest("out");
        // Same length, different bytes, as an antivirus clean-up could leave behind.
        std::fs::write(env.root.join("out/beta/pack/a.txt"), b"FIRST!").expect("tamper");
        let report = build(&env.opts("out2", false), &env.fetcher).expect("rebuild is clean");
        assert_eq!(env.manifest("out2"), full, "a fresh build is unaffected");
        assert!(report.altered.is_empty());

        let m: Manifest = serde_json::from_str(&full).expect("json");
        let produced: Vec<(String, ManifestFile)> = m
            .classes
            .iter()
            .flat_map(|(c, e)| e.files.iter().map(move |f| (c.clone(), f.clone())))
            .collect();
        let expected = [
            ("pack".to_string(), "beta".to_string(), 2),
            ("blob".to_string(), "alpha".to_string(), 1),
        ];
        let (found, accounts) = account_for(&env.root.join("out"), produced, &expected);
        assert_eq!(accounts[0].altered, ["beta/pack/a.txt"]);
        assert!(accounts[0].missing.is_empty());
        assert_eq!((accounts[0].expected, accounts[0].found), (2, 1));
        assert!(found.iter().all(|(_, f)| f.path != "beta/pack/a.txt"));
        // Unlisted files are removed by a full build's clean-up.
        let listed: BTreeSet<String> = found.iter().map(|(_, f)| f.path.clone()).collect();
        let removed = clean_unlisted(&env.root.join("out"), &listed).expect("clean");
        assert!(removed.contains(&"beta/pack/a.txt".to_string()));
        assert!(!env.root.join("out/beta/pack/a.txt").exists());
    }
}
