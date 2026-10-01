//! `corpus build`: registry + lock + downloader + extractor -> `<out>` with manifest.
//!
//! Extension point: [`build_source`] is the single dispatch on [`SourceSpec`]. A new kind adds a
//! match arm there that returns the files it produced. Kinds that need earlier output (derived
//! classes) read the files already produced through [`Ctx::produced`].

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::extract::{extract, Selection};
use super::fetch::{Artifact, DownloadError, Downloader, Expect, Fetcher, RetryPolicy};
use super::lock::{Lock, LockEntry};
use super::manifest::{now_rfc3339, BuildInfo, Manifest, ManifestFile, Skipped};
use super::registry::{file_name_for, ArchiveFormat, Profile, Registry, Source, SourceSpec};

/// Everything `corpus build` needs.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub profile: Profile,
    pub out: PathBuf,
    pub cache: PathBuf,
    /// Restrict to these classes (empty = all). The manifest then covers only those classes.
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
}

/// State shared by all sources of one build.
#[derive(Debug)]
pub struct Ctx<'a> {
    profile: Profile,
    update_lock: bool,
    downloader: Downloader<'a>,
    lock: &'a Lock,
    /// Lock entries recorded while building the current source (`--update-lock`).
    recorded: Vec<LockEntry>,
    /// `(class, file)` for every file produced so far, for kinds that derive from other files.
    pub produced: Vec<(String, ManifestFile)>,
}

impl Ctx<'_> {
    /// Get a verified artifact for `url`, enforcing the lock (or recording it with
    /// `--update-lock`).
    pub fn artifact(&mut self, source: &Source, url: &str) -> Result<Artifact, DownloadError> {
        let expect = if self.update_lock {
            Expect::Unpinned
        } else {
            match self.lock.get(self.profile, &source.id, url) {
                Some(pin) => Expect::Pinned(pin),
                None => return Err(missing_pin(self.profile, &source.id, url)),
            }
        };
        let a = self.downloader.obtain(&source.id, url, expect)?;
        if self.update_lock {
            self.recorded.push(LockEntry {
                blake3: a.blake3.clone(),
                bytes: a.bytes,
                source: source.id.clone(),
                url: url.to_string(),
            });
        }
        Ok(a)
    }
}

fn missing_pin(profile: Profile, source: &str, url: &str) -> DownloadError {
    DownloadError::Mismatch(format!(
        "source `{source}`: no entry for {url} in bench/corpus.lock (profile `{p}`). \
         Pin it with `lpk-bench corpus build --profile {p} --only <class> --update-lock` \
         and commit the lock.",
        p = profile.name()
    ))
}

/// Build the corpus. `fetcher` supplies bytes for URLs (HTTPS in production, memory in tests).
pub fn build(opts: &BuildOptions, fetcher: &dyn Fetcher) -> Result<BuildReport> {
    let registry = Registry::load(&opts.sources_path)?;
    let sources = registry.select(opts.profile, &opts.only)?;
    if sources.is_empty() {
        bail!("no sources apply to profile `{}`", opts.profile.name());
    }
    let mut lock = Lock::load(&opts.lock_path)?;

    // Fail before any network traffic if a pin is missing.
    if !opts.update_lock {
        for s in &sources {
            for url in s.spec.static_urls() {
                if lock.get(opts.profile, &s.id, url).is_none() {
                    return Err(missing_pin(opts.profile, &s.id, url).into());
                }
            }
        }
    }

    std::fs::create_dir_all(&opts.out)
        .with_context(|| format!("creating {}", opts.out.display()))?;
    let lock_snapshot = lock.clone();
    let mut ctx = Ctx {
        profile: opts.profile,
        update_lock: opts.update_lock,
        downloader: Downloader::new(fetcher, opts.cache.clone(), opts.retry, opts.profile),
        lock: &lock_snapshot,
        recorded: Vec::new(),
        produced: Vec::new(),
    };

    let mut skipped = Vec::new();
    let mut pins: Vec<(String, Vec<LockEntry>)> = Vec::new();
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

    if opts.update_lock {
        for (id, entries) in pins {
            lock.replace_source(opts.profile, &id, entries);
        }
        lock.save(&opts.lock_path)?;
        eprintln!("wrote {}", opts.lock_path.display());
    }

    let manifest = Manifest::new(opts.profile, std::mem::take(&mut ctx.produced));
    let manifest_text = manifest.render();
    let manifest_blake3 = blake3::hash(manifest_text.as_bytes()).to_hex().to_string();
    let manifest_path = opts.out.join("manifest.json");
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
    std::fs::write(opts.out.join("build-info.json"), info.render())?;

    Ok(BuildReport {
        manifest_path,
        manifest_blake3,
        files: manifest.classes.values().map(|c| c.files.len()).sum(),
        bytes_total: manifest.classes.values().map(|c| c.bytes_total).sum(),
        skipped,
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
            let sel = Selection::new(
                &spec.include,
                &spec.exclude,
                spec.max_files,
                spec.max_bytes,
                spec.strip_components,
            )?;
            let files = extract(format, &a.path, dir, &sel)?;
            Ok(files
                .into_iter()
                .map(|f| entry(&f.path, f.bytes, f.blake3))
                .collect())
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
            std::fs::read_to_string(self.root.join(out).join("manifest.json")).expect("manifest")
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
        assert!(env.root.join("out/beta/pack/z/last.txt").is_file());
        assert!(!env.root.join("out/beta/pack/skip").exists());
        let info: BuildInfo = serde_json::from_str(
            &std::fs::read_to_string(env.root.join("out/build-info.json")).expect("bi"),
        )
        .expect("json");
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
        assert!(msg.contains("pack") || msg.contains("blob"), "{msg}");
        assert_eq!(env.fetcher.call_count(), 0);
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
        assert!(
            msg.contains("`blob`") || msg.contains("`maybe`") || msg.contains("`pack`"),
            "{msg}"
        );
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
    fn only_restricts_classes_and_update_lock_keeps_other_pins() {
        let env = Env::new(false);
        build(&env.opts("out", true), &env.fetcher).expect("pin all");
        let full_lock = env.lock();
        let mut o = env.opts("out_only", true);
        o.only = vec!["alpha".into()];
        build(&o, &env.fetcher).expect("only alpha");
        let m: Manifest = serde_json::from_str(&env.manifest("out_only")).expect("json");
        assert_eq!(m.classes.keys().collect::<Vec<_>>(), ["alpha"]);
        assert_eq!(env.lock(), full_lock);
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
}
