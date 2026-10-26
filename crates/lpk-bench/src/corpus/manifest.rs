//! `manifest.json` (deterministic) and `build-info.json` (timestamps and host details).

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::registry::Profile;

/// One file in the corpus. Fields are alphabetical so output keys are sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    pub blake3: String,
    pub bytes: u64,
    pub licence: String,
    /// Relative to the corpus output directory, `/` separators.
    pub path: String,
    /// Registry source id.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassEntry {
    pub bytes_total: u64,
    pub files: Vec<ManifestFile>,
}

/// `manifest.json`: no timestamps, so identical inputs give identical bytes on every OS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub classes: BTreeMap<String, ClassEntry>,
    pub profile: String,
}

impl Manifest {
    /// Group `(class, file)` pairs into a manifest; files are sorted bytewise by path.
    pub fn new(profile: Profile, files: Vec<(String, ManifestFile)>) -> Manifest {
        Manifest::with_profile_name(profile.name(), files)
    }

    /// Like [`Manifest::new`] for a profile name that is not a public [`Profile`]
    /// (`"private"` for `corpus scan`).
    pub fn with_profile_name(profile: &str, files: Vec<(String, ManifestFile)>) -> Manifest {
        let mut classes: BTreeMap<String, ClassEntry> = BTreeMap::new();
        for (class, file) in files {
            let entry = classes.entry(class).or_insert_with(|| ClassEntry {
                bytes_total: 0,
                files: Vec::new(),
            });
            entry.bytes_total += file.bytes;
            entry.files.push(file);
        }
        for entry in classes.values_mut() {
            entry
                .files
                .sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        }
        Manifest {
            classes,
            profile: profile.to_string(),
        }
    }

    /// Sorted keys, two-space indent, trailing newline, LF endings.
    pub fn render(&self) -> String {
        render_json(self)
    }

    /// The class digest of `class` (see [`ClassEntry::digest`]), or `None` when the manifest has
    /// no such class.
    pub fn class_blake3(&self, class: &str) -> Option<String> {
        self.classes.get(class).map(ClassEntry::digest)
    }
}

impl ClassEntry {
    /// The class digest (E8-10): BLAKE3 over the class's files in the manifest's order (bytewise
    /// by path), each serialised as `path NUL bytes NUL blake3 NUL` -- the relative path with `/`
    /// separators as in the manifest, the size in decimal ASCII without leading zeros, and the
    /// content hash as 64 lower-case hex digits. A path never holds NUL, so the encoding is
    /// unambiguous. Licence and source id are not part of it: they do not change the bytes a tool
    /// compresses. Two manifests whose class holds the same files hash the same, whatever the
    /// other classes hold.
    pub fn digest(&self) -> String {
        let mut h = blake3::Hasher::new();
        for f in &self.files {
            h.update(f.path.as_bytes());
            h.update(b"\0");
            h.update(f.bytes.to_string().as_bytes());
            h.update(b"\0");
            h.update(f.blake3.as_bytes());
            h.update(b"\0");
        }
        h.finalize().to_hex().to_string()
    }
}

/// `build-info.json`: everything that legitimately differs between two builds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub built_at: String,
    pub host_arch: String,
    pub host_os: String,
    pub lpk_bench_version: String,
    pub manifest_blake3: String,
    pub profile: String,
    pub skipped: Vec<Skipped>,
    /// Expected versus found files per built source (public builds only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounting: Vec<SourceAccount>,
    /// Files and bytes per class, and in total (public builds only).
    #[serde(default, skip_serializing_if = "Summary::is_empty")]
    pub summary: Summary,
    /// Versions of external programs a build used (e.g. `git`), so a difference between two
    /// builds can be traced to the tool.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, String>,
    /// Listed files that were gone or no longer matched their pin: left out of the manifest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<Unavailable>,
}

/// A file of a list source that a normal build could not obtain as pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unavailable {
    pub reason: String,
    pub source: String,
    pub url: String,
}

/// D-14 accounting for one source: how many files extraction produced (`expected`) and how many
/// still existed with the right size when the manifest was written (`found`). Missing files
/// (for example quarantined by antivirus) are listed and left out of the manifest; the build
/// does not fail. Exclude them by name in the registry to make every machine build the same
/// corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAccount {
    pub class: String,
    pub expected: u64,
    pub found: u64,
    /// Files that were gone when the manifest was written.
    pub missing: Vec<String>,
    /// Files whose content on disk no longer hashes to what extraction wrote (cleaned or
    /// tampered with in place). Left out of the manifest like missing ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub altered: Vec<String>,
    pub source: String,
}

/// Files and bytes of one class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassTotals {
    /// The class digest ([`ClassEntry::digest`]); absent in build-info files written before E8-10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
    pub bytes: u64,
    pub files: u64,
}

/// End-of-build summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub classes: BTreeMap<String, ClassTotals>,
    pub total_bytes: u64,
    pub total_files: u64,
}

impl Summary {
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    pub fn of(manifest: &Manifest) -> Summary {
        let classes: BTreeMap<String, ClassTotals> = manifest
            .classes
            .iter()
            .map(|(k, c)| {
                (
                    k.clone(),
                    ClassTotals {
                        blake3: Some(c.digest()),
                        bytes: c.bytes_total,
                        files: c.files.len() as u64,
                    },
                )
            })
            .collect();
        Summary {
            total_bytes: classes.values().map(|c| c.bytes).sum(),
            total_files: classes.values().map(|c| c.files).sum(),
            classes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub reason: String,
    pub source: String,
}

impl BuildInfo {
    pub fn render(&self) -> String {
        render_json(self)
    }
}

/// Pretty JSON through `serde_json::Value`, whose maps are sorted, plus a trailing newline.
fn render_json<T: Serialize>(value: &T) -> String {
    let v = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    let mut s = serde_json::to_string_pretty(&v).unwrap_or_default();
    s.push('\n');
    s
}

/// Current time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    rfc3339_utc(secs)
}

/// Format Unix seconds as UTC RFC 3339 (civil-from-days algorithm).
pub fn rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, bytes: u64, source: &str) -> ManifestFile {
        ManifestFile {
            blake3: "00".repeat(32),
            bytes,
            licence: "MIT".into(),
            path: path.into(),
            source: source.into(),
        }
    }

    #[test]
    fn manifest_is_sorted_totalled_and_input_order_independent() {
        let a = Manifest::new(
            Profile::Small,
            vec![
                ("zeta".into(), f("zeta/s/b", 5, "s")),
                ("alpha".into(), f("alpha/s/z", 2, "s")),
                ("alpha".into(), f("alpha/s/a", 3, "s")),
            ],
        );
        let b = Manifest::new(
            Profile::Small,
            vec![
                ("alpha".into(), f("alpha/s/a", 3, "s")),
                ("zeta".into(), f("zeta/s/b", 5, "s")),
                ("alpha".into(), f("alpha/s/z", 2, "s")),
            ],
        );
        assert_eq!(a.render(), b.render());
        assert_eq!(a.classes["alpha"].bytes_total, 5);
        assert_eq!(a.classes["alpha"].files[0].path, "alpha/s/a");
        let text = a.render();
        assert!(text.ends_with("}\n") && !text.contains('\r'));
        assert!(text.find("\"classes\"") < text.find("\"profile\""));
        assert!(text.find("\"alpha\"") < text.find("\"zeta\""));
        // Per-file keys sorted: blake3, bytes, licence, path, source.
        let k = |s: &str| text.find(&format!("\"{s}\"")).expect("key");
        assert!(k("blake3") < k("bytes") && k("bytes") < k("licence"));
        assert!(k("licence") < k("path") && k("path") < k("source"));
        assert!(!text.contains("built_at"));
    }

    #[test]
    fn class_digest_is_documented_and_ignores_other_classes() {
        let a = Manifest::new(
            Profile::Small,
            vec![
                ("alpha".into(), f("alpha/s/b", 5, "s")),
                ("alpha".into(), f("alpha/s/a", 3, "s")),
                ("zeta".into(), f("zeta/s/b", 5, "s")),
            ],
        );
        let b = Manifest::new(
            Profile::Small,
            vec![
                ("alpha".into(), f("alpha/s/a", 3, "other-source")),
                ("alpha".into(), f("alpha/s/b", 5, "s")),
                ("zeta".into(), f("zeta/s/b", 6, "s")),
            ],
        );
        // The documented serialisation, by hand.
        let zero = "00".repeat(32);
        let text = format!("alpha/s/a\x003\x00{zero}\x00alpha/s/b\x005\x00{zero}\x00");
        let want = blake3::hash(text.as_bytes()).to_hex().to_string();
        assert_eq!(a.class_blake3("alpha").as_deref(), Some(want.as_str()));
        // Another class changed (and a source id): alpha's digest does not move, zeta's does.
        assert_ne!(a.render(), b.render());
        assert_eq!(a.class_blake3("alpha"), b.class_blake3("alpha"));
        assert_ne!(a.class_blake3("zeta"), b.class_blake3("zeta"));
        assert_eq!(a.class_blake3("missing"), None);
        let s = Summary::of(&a);
        assert_eq!(s.classes["alpha"].blake3.as_deref(), Some(want.as_str()));
    }

    #[test]
    fn rfc3339() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    }
}
