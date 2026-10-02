//! The source registry: parsing and validation of `bench/corpus-sources.toml`.
//!
//! See the module docs of [`crate::corpus`] for the file format and for how to add a kind.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use super::extract::check_portable_component;

/// Corpus profile. `small` is for iteration and CI, `full` is for the Phase 0 report.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Small,
    Full,
}

impl Profile {
    /// Lower-case name used in paths, the lock and the manifest.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Small => "small",
            Profile::Full => "full",
        }
    }
}

/// Archive container formats the extractor understands. Add new formats here and in
/// `extract::extract`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ArchiveFormat {
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "tar.gz")]
    TarGz,
    /// 7z, also a self-extracting `.7z.exe` (the archive is located by its signature).
    #[serde(rename = "7z")]
    SevenZ,
    /// One gzip stream, decompressed to a single file.
    #[serde(rename = "gz")]
    Gz,
}

impl ArchiveFormat {
    /// Guess the format from a URL or file name.
    pub fn guess(url: &str) -> Option<ArchiveFormat> {
        let lower = url
            .split(['?', '#'])
            .next()
            .unwrap_or(url)
            .to_ascii_lowercase();
        if lower.ends_with(".zip") {
            Some(ArchiveFormat::Zip)
        } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Some(ArchiveFormat::TarGz)
        } else if lower.ends_with(".7z") || lower.ends_with(".7z.exe") {
            Some(ArchiveFormat::SevenZ)
        } else if lower.ends_with(".gz") {
            Some(ArchiveFormat::Gz)
        } else {
            None
        }
    }
}

/// Kind-specific part of a source. Internally tagged by `kind`; unknown fields are rejected.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SourceSpec {
    /// One URL is stored as one file.
    File(FileSpec),
    /// One URL is an archive that is extracted (with filters and a deterministic cap).
    Archive(ArchiveSpec),
    /// Several URLs with output names, each pinned separately (static list of files).
    Files(FilesSpec),
}

/// Keys of `kind = "files"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesSpec {
    pub files: Vec<FileItem>,
}

/// One entry of a `files` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileItem {
    pub url: String,
    /// Output path below `<out>/<class>/<source>/`; defaults to the last URL segment.
    pub path: Option<String>,
    /// Per-file licence (overrides the source's in the manifest).
    pub licence: Option<String>,
    /// Author / credit line, kept in the lock.
    pub attribution: Option<String>,
}

/// Keys of `kind = "file"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    pub url: String,
    /// Output file name; defaults to the last path segment of the URL.
    pub filename: Option<String>,
}

/// Keys of `kind = "archive"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSpec {
    pub url: String,
    /// Defaults to a guess from the URL extension.
    pub format: Option<ArchiveFormat>,
    /// Glob patterns an entry must match (empty = everything). Matched against the path
    /// after `strip_components`, with `/` separators; `*` does not cross `/`, `**` does.
    #[serde(default)]
    pub include: Vec<String>,
    /// Glob patterns that remove entries again.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Keep at most this many files (sorted-path order).
    pub max_files: Option<usize>,
    /// Keep files while the running total of declared sizes stays within this many bytes.
    pub max_bytes: Option<u64>,
    /// Drop this many leading path components; entries with too few components are skipped.
    #[serde(default)]
    pub strip_components: usize,
    /// Keep only the first N bytes of every larger file, cut at the last line break.
    pub truncate_files: Option<u64>,
    /// `tar.gz` only: symlink and hardlink entries are skipped (their names are still
    /// validated) instead of failing the extraction. For source trees that contain links.
    #[serde(default)]
    pub skip_links: bool,
}

impl SourceSpec {
    /// Every URL this source downloads. Used for the pre-flight lock check, so kinds whose URLs
    /// are only known after resolving (API lists) must return what is statically known.
    pub fn static_urls(&self) -> Vec<&str> {
        match self {
            SourceSpec::File(f) => vec![f.url.as_str()],
            SourceSpec::Archive(a) => vec![a.url.as_str()],
            SourceSpec::Files(f) => f.files.iter().map(|i| i.url.as_str()).collect(),
        }
    }

    /// True for kinds whose file list lives in the lock (pins carry the paths).
    pub fn is_list(&self) -> bool {
        matches!(self, SourceSpec::Files(_))
    }
}

/// One registry entry.
#[derive(Debug, Clone)]
pub struct Source {
    pub id: String,
    pub class: String,
    /// SPDX identifier or short statement.
    pub licence: String,
    /// Human description of where the data comes from.
    pub origin: String,
    pub profiles: Vec<Profile>,
    /// If true, a failed download is recorded under `skipped` instead of failing the build.
    /// Lock mismatches are always fatal.
    pub optional: bool,
    /// Classes this source is derived from (derivation kinds). They must have a source earlier in
    /// the registry. `--only` requires them to be built in the same run or already present.
    pub inputs: Vec<String>,
    pub spec: SourceSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Common {
    id: String,
    class: String,
    licence: String,
    origin: String,
    profiles: Vec<Profile>,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    inputs: Vec<String>,
}

/// Politeness settings for one host (`[[host]]` table). Every request of the downloader to the
/// host first waits for `min_interval_ms` since the previous request to it ended.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostSpec {
    pub name: String,
    #[serde(default)]
    pub min_interval_ms: u64,
    /// Throughput cap in megabit per second.
    pub max_mbit_per_s: Option<u64>,
}

/// All sources, in file order (the build runs them in this order).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub sources: Vec<Source>,
    pub hosts: Vec<HostSpec>,
}

const COMMON_KEYS: [&str; 7] = [
    "id", "class", "licence", "origin", "profiles", "optional", "inputs",
];

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
        && !s.starts_with('.')
}

impl Registry {
    /// Load and validate a registry file.
    pub fn load(path: &Path) -> Result<Registry> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading source registry {}", path.display()))?;
        Registry::parse(&text).with_context(|| format!("in source registry {}", path.display()))
    }

    /// Parse and validate registry text.
    pub fn parse(text: &str) -> Result<Registry> {
        let root: toml::Table = text.parse().context("invalid TOML")?;
        for key in root.keys() {
            if key != "source" && key != "host" {
                bail!("unknown top-level key `{key}` (only `[[source]]` and `[[host]]` tables are allowed)");
            }
        }
        let hosts: Vec<HostSpec> = match root.get("host") {
            Some(h) => h.clone().try_into().context("invalid [[host]] table")?,
            None => Vec::new(),
        };
        let mut sources = Vec::new();
        if let Some(list) = root.get("source") {
            let list = list
                .as_array()
                .context("`source` must be an array of tables")?;
            for (n, item) in list.iter().enumerate() {
                let mut table = item
                    .as_table()
                    .with_context(|| format!("source #{} is not a table", n + 1))?
                    .clone();
                let mut common = toml::Table::new();
                for key in COMMON_KEYS {
                    if let Some(v) = table.remove(key) {
                        common.insert(key.to_string(), v);
                    }
                }
                let common: Common = toml::Value::Table(common)
                    .try_into()
                    .with_context(|| format!("source #{}", n + 1))?;
                let spec: SourceSpec = toml::Value::Table(table)
                    .try_into()
                    .with_context(|| format!("source `{}`", common.id))?;
                sources.push(Source {
                    id: common.id,
                    class: common.class,
                    licence: common.licence,
                    origin: common.origin,
                    profiles: common.profiles,
                    optional: common.optional,
                    inputs: common.inputs,
                    spec,
                });
            }
        }
        let registry = Registry { sources, hosts };
        registry.validate()?;
        Ok(registry)
    }

    fn validate(&self) -> Result<()> {
        let mut host_names = BTreeSet::new();
        for h in &self.hosts {
            if h.name.is_empty() || h.name != h.name.to_ascii_lowercase() || h.name.contains('/') {
                bail!("host name `{}` must be a lower-case host name", h.name);
            }
            if !host_names.insert(h.name.as_str()) {
                bail!("duplicate [[host]] `{}`", h.name);
            }
            if h.max_mbit_per_s == Some(0) {
                bail!("host `{}`: max_mbit_per_s must be positive", h.name);
            }
        }
        let mut ids = BTreeSet::new();
        let mut classes_before: BTreeSet<&str> = BTreeSet::new();
        for s in &self.sources {
            if !valid_ident(&s.id) {
                bail!(
                    "source id `{}` must match [a-z0-9._-]+ and not start with `.`",
                    s.id
                );
            }
            if !valid_ident(&s.class) {
                bail!(
                    "source `{}`: class `{}` must match [a-z0-9._-]+",
                    s.id,
                    s.class
                );
            }
            if !ids.insert(s.id.as_str()) {
                bail!("duplicate source id `{}`", s.id);
            }
            if s.profiles.is_empty() {
                bail!("source `{}` applies to no profile", s.id);
            }
            if s.licence.trim().is_empty() || s.origin.trim().is_empty() {
                bail!("source `{}` needs a non-empty licence and origin", s.id);
            }
            for input in &s.inputs {
                if input == &s.class || !classes_before.contains(input.as_str()) {
                    bail!(
                        "source `{}`: input class `{input}` must have a source earlier in the \
                         registry and differ from the source's own class",
                        s.id
                    );
                }
            }
            classes_before.insert(s.class.as_str());
            for url in s.spec.static_urls() {
                if !url.starts_with("https://") {
                    bail!("source `{}`: URL must be https:// (got `{url}`)", s.id);
                }
            }
            match &s.spec {
                SourceSpec::File(f) => {
                    let name = file_name_for(&f.url, f.filename.as_deref())
                        .with_context(|| format!("source `{}`", s.id))?;
                    if name.contains('/') || name == "." || name == ".." {
                        bail!("source `{}`: bad filename `{name}`", s.id);
                    }
                    check_portable_component(&name)
                        .with_context(|| format!("source `{}`: filename", s.id))?;
                }
                SourceSpec::Archive(a) => {
                    if a.format.is_none() && ArchiveFormat::guess(&a.url).is_none() {
                        bail!(
                            "source `{}`: cannot infer archive format, set `format`",
                            s.id
                        );
                    }
                    if a.truncate_files == Some(0) {
                        bail!("source `{}`: truncate_files must be positive", s.id);
                    }
                    if a.format.or_else(|| ArchiveFormat::guess(&a.url)) == Some(ArchiveFormat::Gz)
                    {
                        let name =
                            gz_output_name(&a.url).with_context(|| format!("source `{}`", s.id))?;
                        check_portable_component(&name)
                            .with_context(|| format!("source `{}`: gz output name", s.id))?;
                    }
                    compile_globs(&a.include)
                        .with_context(|| format!("source `{}` include", s.id))?;
                    compile_globs(&a.exclude)
                        .with_context(|| format!("source `{}` exclude", s.id))?;
                }
                SourceSpec::Files(f) => {
                    if f.files.is_empty() {
                        bail!("source `{}`: `files` is empty", s.id);
                    }
                    let mut urls = BTreeSet::new();
                    let mut paths = Vec::new();
                    for item in &f.files {
                        if !urls.insert(item.url.as_str()) {
                            bail!("source `{}`: URL listed twice: {}", s.id, item.url);
                        }
                        paths.push(
                            file_name_for(&item.url, item.path.as_deref())
                                .with_context(|| format!("source `{}`", s.id))?,
                        );
                    }
                    super::extract::check_listing(&paths)
                        .with_context(|| format!("source `{}`", s.id))?;
                }
            }
        }
        Ok(())
    }

    /// Sources that apply to `profile`, optionally restricted to the given classes.
    pub fn select(&self, profile: Profile, only: &[String]) -> Result<Vec<&Source>> {
        for class in only {
            if !self.sources.iter().any(|s| &s.class == class) {
                let known: BTreeSet<&str> = self.sources.iter().map(|s| s.class.as_str()).collect();
                bail!(
                    "unknown class `{class}` in --only; classes in the registry: {}",
                    known.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
        }
        let picked: Vec<&Source> = self
            .sources
            .iter()
            .filter(|s| s.profiles.contains(&profile))
            .filter(|s| only.is_empty() || only.contains(&s.class))
            .collect();
        for class in only {
            if !picked.iter().any(|s| &s.class == class) {
                bail!(
                    "class `{class}` has no source in profile `{}`",
                    profile.name()
                );
            }
        }
        Ok(picked)
    }
}

/// Output file name for a `file` source.
pub fn file_name_for(url: &str, filename: Option<&str>) -> Result<String> {
    if let Some(name) = filename {
        return Ok(name.to_string());
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path.rsplit('/').next() {
        Some(last) if !last.is_empty() && !path.ends_with("://") => Ok(last.to_string()),
        _ => bail!("cannot derive a file name from `{url}`, set `filename`"),
    }
}

/// Output file name of a `gz` source: the last URL segment without `.gz`.
pub fn gz_output_name(url: &str) -> Result<String> {
    let name = file_name_for(url, None)?;
    match name.strip_suffix(".gz") {
        Some(stem) if !stem.is_empty() => Ok(stem.to_string()),
        _ => bail!("gz URL `{url}` does not end in a name plus `.gz`"),
    }
}

/// Compile glob patterns; `*` and `?` do not cross `/`.
pub fn compile_globs(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let glob = GlobBuilder::new(p)
            .literal_separator(true)
            .build()
            .with_context(|| format!("bad glob `{p}`"))?;
        builder.add(glob);
    }
    builder.build().context("building glob set")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[[source]]
id = "a"
class = "c1"
licence = "MIT"
origin = "somewhere"
profiles = ["small", "full"]
kind = "file"
url = "https://example.org/x/data.bin"

[[source]]
id = "b"
class = "c2"
licence = "CC0-1.0"
origin = "elsewhere"
profiles = ["full"]
kind = "archive"
url = "https://example.org/b.tar.gz"
include = ["**/*.txt"]
max_files = 3
"#;

    #[test]
    fn parses_both_kinds_and_selects_by_profile() {
        let r = Registry::parse(GOOD).expect("parse");
        assert_eq!(r.sources.len(), 2);
        assert_eq!(r.select(Profile::Small, &[]).expect("sel").len(), 1);
        assert_eq!(r.select(Profile::Full, &[]).expect("sel").len(), 2);
        let only = vec!["c2".to_string()];
        assert_eq!(r.select(Profile::Full, &only).expect("sel").len(), 1);
        assert!(r.select(Profile::Small, &only).is_err());
        assert!(r.select(Profile::Full, &["nope".to_string()]).is_err());
    }

    #[test]
    fn rejects_unknown_fields_kinds_and_duplicates() {
        let typo = GOOD.replace("max_files", "max_fils");
        assert!(Registry::parse(&typo).is_err());
        let kind = GOOD.replace("kind = \"file\"", "kind = \"teleport\"");
        assert!(Registry::parse(&kind).is_err());
        let dup = GOOD.replace("id = \"b\"", "id = \"a\"");
        assert!(Registry::parse(&dup).is_err());
        let http = GOOD.replace("https://example.org/b", "http://example.org/b");
        assert!(Registry::parse(&http).is_err());
        let extra = format!("{GOOD}\n[other]\nx = 1\n");
        assert!(Registry::parse(&extra).is_err());
    }

    #[test]
    fn file_names() {
        assert_eq!(
            file_name_for("https://h/a/b.zip?x=1", None).expect("n"),
            "b.zip"
        );
        assert_eq!(file_name_for("https://h/a/", Some("z")).expect("n"), "z");
        assert!(file_name_for("https://h/a/", None).is_err());
    }

    #[test]
    fn inputs_must_name_earlier_classes_and_filenames_must_be_portable() {
        let derived = |input: &str| {
            GOOD.replace(
                "origin = \"elsewhere\"",
                &format!("origin = \"elsewhere\"\ninputs = [\"{input}\"]"),
            )
        };
        assert!(Registry::parse(&derived("c1")).is_ok());
        assert!(Registry::parse(&derived("c2")).is_err(), "own class");
        assert!(Registry::parse(&derived("missing")).is_err());
        // A source may not depend on a class that only appears later in the file.
        let later = GOOD.replace(
            "origin = \"somewhere\"",
            "origin = \"somewhere\"\ninputs = [\"c2\"]",
        );
        assert!(Registry::parse(&later).is_err());
        let nul = GOOD.replace("kind = \"file\"", "kind = \"file\"\nfilename = \"NUL\"");
        assert!(Registry::parse(&nul).is_err());
    }

    #[test]
    fn committed_registry_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/corpus-sources.toml");
        let r = Registry::load(&path).expect("committed registry parses");
        assert!(!r.sources.is_empty());
    }
}
