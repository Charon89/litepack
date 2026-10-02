//! `bench/corpus.lock`: the pinned listing of everything a build downloads, per profile.
//!
//! The lock *is* the file list of list-type sources: a normal build never calls a listing API,
//! it builds exactly the entries pinned here. Only `--update-lock` resolves and records.
//!
//! Format version 2 (version 1, which only had `blake3`/`bytes`/`source`/`url`, is still read
//! and is rewritten as version 2 by the next `--update-lock`).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::registry::Profile;

/// Pin for one artifact. Field order is alphabetical on purpose (stable output); optional
/// fields are omitted when unset.
///
/// Identity is `(source, url)`. Download pins carry `bytes` and `blake3`; a git pin carries
/// `commit` instead (the resolver for it verifies the commit hash itself). `path`, `licence`
/// and `attribution` describe the produced file for list-type sources; `extra` is for
/// anything else a resolver needs to re-verify (keys sorted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockEntry {
    /// Author / credit line (CC-BY style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Git commit id for clone-type sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
    /// Per-file licence; overrides the source-level licence in the manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
    /// Output path relative to `<out>/<class>/<source>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub source: String,
    pub url: String,
}

impl LockEntry {
    /// A plain download pin.
    pub fn artifact(source: &str, url: &str, bytes: u64, blake3: String) -> LockEntry {
        LockEntry {
            attribution: None,
            blake3: Some(blake3),
            bytes: Some(bytes),
            commit: None,
            extra: BTreeMap::new(),
            licence: None,
            path: None,
            source: source.to_string(),
            url: url.to_string(),
        }
    }
}

impl LockEntry {
    /// A git pin: the commit of `url` (the repository).
    pub fn commit_pin(source: &str, url: &str, commit: &str) -> LockEntry {
        LockEntry {
            attribution: None,
            blake3: None,
            bytes: None,
            commit: Some(commit.to_string()),
            extra: BTreeMap::new(),
            licence: None,
            path: None,
            source: source.to_string(),
            url: url.to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LockFile {
    profiles: BTreeMap<String, Vec<LockEntry>>,
    version: u32,
}

/// Version written by this tool.
pub const LOCK_VERSION: u32 = 2;

/// In-memory lock. Entries are unique per `(profile, source, url)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lock {
    entries: BTreeMap<String, BTreeMap<(String, String), LockEntry>>,
}

impl Lock {
    /// Read a lock file; a missing file is an empty lock.
    pub fn load(path: &Path) -> Result<Lock> {
        match std::fs::read_to_string(path) {
            Ok(text) => Lock::parse(&text).with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Lock> {
        let file: LockFile = serde_json::from_str(text).context("invalid corpus.lock")?;
        if !(1..=LOCK_VERSION).contains(&file.version) {
            bail!("unsupported corpus.lock version {}", file.version);
        }
        let mut lock = Lock::default();
        for (profile, list) in file.profiles {
            for e in list {
                lock.entries
                    .entry(profile.clone())
                    .or_default()
                    .insert((e.source.clone(), e.url.clone()), e);
            }
        }
        Ok(lock)
    }

    pub fn get(&self, profile: Profile, source: &str, url: &str) -> Option<&LockEntry> {
        self.entries
            .get(profile.name())?
            .get(&(source.to_string(), url.to_string()))
    }

    /// Every pin of `source` in `profile`, sorted by URL. For list-type sources this is the
    /// file list.
    pub fn entries(&self, profile: Profile, source: &str) -> Vec<&LockEntry> {
        self.entries
            .get(profile.name())
            .map(|m| m.values().filter(|e| e.source == source).collect())
            .unwrap_or_default()
    }

    /// Replace everything recorded for `source` in `profile` with `entries`.
    pub fn replace_source(&mut self, profile: Profile, source: &str, entries: Vec<LockEntry>) {
        let map = self.entries.entry(profile.name().to_string()).or_default();
        map.retain(|(s, _), _| s != source);
        for e in entries {
            map.insert((e.source.clone(), e.url.clone()), e);
        }
        if map.is_empty() {
            self.entries.remove(profile.name());
        }
    }

    /// Stable serialisation: sorted keys, two-space indent, trailing newline, LF endings.
    pub fn render(&self) -> String {
        let file = LockFile {
            profiles: self
                .entries
                .iter()
                .map(|(p, m)| (p.clone(), m.values().cloned().collect()))
                .collect(),
            version: LOCK_VERSION,
        };
        // Serialising plain structs with string keys cannot fail.
        let mut s = serde_json::to_string_pretty(&file).unwrap_or_default();
        s.push('\n');
        s
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.render()).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(source: &str, url: &str) -> LockEntry {
        LockEntry::artifact(source, url, 3, "ab".repeat(32))
    }

    #[test]
    fn render_is_sorted_and_order_independent() {
        let mut a = Lock::default();
        a.replace_source(
            Profile::Small,
            "z",
            vec![entry("z", "https://x/2"), entry("z", "https://x/1")],
        );
        a.replace_source(Profile::Full, "a", vec![entry("a", "https://x/0")]);
        let mut b = Lock::default();
        b.replace_source(Profile::Full, "a", vec![entry("a", "https://x/0")]);
        b.replace_source(
            Profile::Small,
            "z",
            vec![entry("z", "https://x/1"), entry("z", "https://x/2")],
        );
        assert_eq!(a.render(), b.render());
        let text = a.render();
        assert!(text.ends_with("}\n"));
        assert!(text.contains("\"version\": 2"));
        assert!(text.find("\"full\"") < text.find("\"small\""));
        assert!(text.find("https://x/1") < text.find("https://x/2"));
        assert_eq!(Lock::parse(&text).expect("roundtrip"), a);
    }

    #[test]
    fn optional_fields_roundtrip_in_sorted_key_order_and_are_omitted_when_unset() {
        let mut e = entry("s", "https://x/photo.jpg");
        e.attribution = Some("Jane Doe".into());
        e.licence = Some("CC-BY-4.0".into());
        e.path = Some("a/photo.jpg".into());
        e.extra.insert("revision".into(), "42".into());
        e.extra.insert("api".into(), "commons".into());
        let git = LockEntry {
            blake3: None,
            bytes: None,
            commit: Some("0123456789abcdef".into()),
            ..entry("g", "https://x/repo.git")
        };
        let mut l = Lock::default();
        l.replace_source(Profile::Small, "s", vec![e.clone()]);
        l.replace_source(Profile::Small, "g", vec![git.clone()]);
        let text = l.render();
        let k = |s: &str| text.find(&format!("\"{s}\"")).expect(s);
        assert!(k("attribution") < k("blake3") && k("blake3") < k("bytes"));
        assert!(k("bytes") < k("extra") && k("extra") < k("licence"));
        assert!(k("licence") < k("path") && k("api") < k("revision"));
        let back = Lock::parse(&text).expect("roundtrip");
        assert_eq!(back.get(Profile::Small, "s", &e.url), Some(&e));
        assert_eq!(back.get(Profile::Small, "g", &git.url), Some(&git));
        // The plain pin has no optional keys at all.
        let plain = Lock::default();
        let mut plain = plain;
        plain.replace_source(Profile::Small, "p", vec![entry("p", "https://x/p")]);
        let t = plain.render();
        assert!(!t.contains("attribution") && !t.contains("commit") && !t.contains("extra"));
    }

    #[test]
    fn entries_lists_a_sources_pins_sorted_by_url() {
        let mut l = Lock::default();
        l.replace_source(
            Profile::Small,
            "s",
            vec![entry("s", "https://x/b"), entry("s", "https://x/a")],
        );
        l.replace_source(Profile::Small, "t", vec![entry("t", "https://x/c")]);
        let urls: Vec<_> = l
            .entries(Profile::Small, "s")
            .iter()
            .map(|e| e.url.clone())
            .collect();
        assert_eq!(urls, ["https://x/a", "https://x/b"]);
        assert!(l.entries(Profile::Full, "s").is_empty());
    }

    #[test]
    fn version_1_locks_are_read_and_rewritten_as_2() {
        let v1 = r#"{"profiles":{"small":[{"blake3":"aa","bytes":5,"source":"s","url":"https://x"}]},"version":1}"#;
        let l = Lock::parse(v1).expect("v1 readable");
        assert_eq!(
            l.get(Profile::Small, "s", "https://x")
                .and_then(|e| e.bytes),
            Some(5)
        );
        assert!(l.render().contains("\"version\": 2"));
    }

    #[test]
    fn replace_source_drops_stale_urls() {
        let mut l = Lock::default();
        l.replace_source(Profile::Small, "s", vec![entry("s", "https://old")]);
        l.replace_source(Profile::Small, "s", vec![entry("s", "https://new")]);
        assert!(l.get(Profile::Small, "s", "https://old").is_none());
        assert!(l.get(Profile::Small, "s", "https://new").is_some());
        assert!(l.get(Profile::Full, "s", "https://new").is_none());
    }

    #[test]
    fn missing_file_is_empty_and_bad_version_rejected() {
        let dir = tempfile::tempdir().expect("tmp");
        assert_eq!(
            Lock::load(&dir.path().join("nope.lock")).expect("load"),
            Lock::default()
        );
        assert!(Lock::parse("{\"profiles\":{},\"version\":99}").is_err());
        assert!(Lock::parse("{\"profiles\":{},\"version\":0}").is_err());
    }
}
