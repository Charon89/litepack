//! `bench/corpus.lock`: pinned URL, size and BLAKE3 for every downloaded artifact, per profile.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::registry::Profile;

/// Pin for one downloaded artifact. Field order is alphabetical on purpose (stable output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    pub blake3: String,
    pub bytes: u64,
    pub source: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LockFile {
    profiles: BTreeMap<String, Vec<LockEntry>>,
    version: u32,
}

const LOCK_VERSION: u32 = 1;

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
        if file.version != LOCK_VERSION {
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
        LockEntry {
            blake3: "ab".repeat(32),
            bytes: 3,
            source: source.into(),
            url: url.into(),
        }
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
        assert!(text.find("\"full\"") < text.find("\"small\""));
        assert!(text.find("https://x/1") < text.find("https://x/2"));
        assert_eq!(Lock::parse(&text).expect("roundtrip"), a);
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
    }
}
