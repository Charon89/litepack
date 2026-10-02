//! Result-file types. The JSON they serialise to is described by `bench/results/schema.json`;
//! a test (`schema_and_types_agree`) fails when the two disagree.
//!
//! Layout: `bench/results/<date>-<host>/` holds `host.json`, `tools.json` and one
//! `<tool>-<setting>-<class>.json` per combination. No file may contain absolute paths, user
//! names or environment dumps.

use serde::{Deserialize, Serialize};

use super::discover::{Discovered, Status};

pub const SCHEMA_VERSION: u32 = 1;

/// Pretty JSON with a trailing newline (object keys come out sorted).
pub fn render<T: Serialize>(value: &T) -> String {
    let v = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    let mut s = serde_json::to_string_pretty(&v).unwrap_or_default();
    s.push('\n');
    s
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRef {
    pub id: String,
    /// Absent when the tool was not found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// The setting and the exact argument lists used. Templates keep their placeholders
/// (`{archive}`, `{input}`, `{outdir}`) so no machine-specific path is recorded; the thread
/// argument is filled in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingRef {
    pub id: String,
    pub compress_args: Vec<String>,
    pub extract_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusRef {
    pub profile: String,
    pub manifest_blake3: String,
    pub files: u64,
    pub input_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measure {
    pub wall_seconds: f64,
    pub user_cpu_seconds: f64,
    pub kernel_cpu_seconds: f64,
    pub peak_memory_bytes: u64,
}

/// One repeat (or the medians of all repeats): compress and extract measurements and the
/// archive size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub compress: Measure,
    pub extract: Measure,
    pub archive_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    pub verified: bool,
    pub files_checked: u64,
    pub files_ok: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResult {
    pub schema_version: u32,
    pub tool: ToolRef,
    pub setting: SettingRef,
    pub class: String,
    pub corpus: CorpusRef,
    pub threads: u32,
    /// `true` when the corpus is a private scan; absent otherwise.
    #[serde(default, skip_serializing_if = "is_false")]
    pub private: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeats: Option<Vec<Sample>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median: Option<Sample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Why the combination was skipped; then there are no measurements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl ToolResult {
    /// File name `<tool>-<setting>-<class>.json`.
    pub fn file_name(&self) -> String {
        format!("{}-{}-{}.json", self.tool.id, self.setting.id, self.class)
    }
}

fn median_f64(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => 0.0,
        n if n % 2 == 1 => v[n / 2],
        n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
    }
}

fn median_u64(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    match v.len() {
        0 => 0,
        n if n % 2 == 1 => v[n / 2],
        n => {
            let (a, b) = (v[n / 2 - 1], v[n / 2]);
            a / 2 + b / 2 + (a % 2 + b % 2) / 2
        }
    }
}

impl Measure {
    pub fn median_of(items: &[Measure]) -> Measure {
        let f = |g: fn(&Measure) -> f64| median_f64(items.iter().map(g).collect());
        Measure {
            wall_seconds: f(|m| m.wall_seconds),
            user_cpu_seconds: f(|m| m.user_cpu_seconds),
            kernel_cpu_seconds: f(|m| m.kernel_cpu_seconds),
            peak_memory_bytes: median_u64(items.iter().map(|m| m.peak_memory_bytes).collect()),
        }
    }
}

impl Sample {
    /// Median of each field over the repeats.
    pub fn median_of(items: &[Sample]) -> Sample {
        let compress: Vec<Measure> = items.iter().map(|s| s.compress).collect();
        let extract: Vec<Measure> = items.iter().map(|s| s.extract).collect();
        Sample {
            compress: Measure::median_of(&compress),
            extract: Measure::median_of(&extract),
            archive_bytes: median_u64(items.iter().map(|s| s.archive_bytes).collect()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolEntry {
    pub id: String,
    pub name: String,
    /// `found` or `skipped`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// `tools.json`: every catalogue tool, found or skipped, and the version used. The executable
/// path is deliberately not recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsFile {
    pub schema_version: u32,
    pub tools: Vec<ToolEntry>,
}

impl ToolsFile {
    pub fn from_discovered(found: &[Discovered]) -> ToolsFile {
        let tools = found
            .iter()
            .map(|d| match &d.status {
                Status::Found { version, .. } => ToolEntry {
                    id: d.tool.id.clone(),
                    name: d.tool.name.clone(),
                    status: "found".into(),
                    reason: None,
                    version: Some(version.clone()),
                },
                Status::Skipped { reason } => ToolEntry {
                    id: d.tool.id.clone(),
                    name: d.tool.name.clone(),
                    status: "skipped".into(),
                    reason: Some(reason.clone()),
                    version: None,
                },
            })
            .collect();
        ToolsFile {
            schema_version: SCHEMA_VERSION,
            tools,
        }
    }
}

/// `host.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFile {
    pub schema_version: u32,
    pub host: String,
    pub os: String,
    pub os_version: String,
    pub cpu_model: String,
    pub logical_cores: u32,
    pub ram_bytes: u64,
    pub lpk_bench_version: String,
    pub git_commit: String,
    pub rustc_version: String,
}

#[cfg(test)]
pub mod samples {
    //! Sample values shared by the tests of this module family.
    use super::*;

    pub fn measure(wall: f64) -> Measure {
        Measure {
            wall_seconds: wall,
            user_cpu_seconds: wall * 3.0,
            kernel_cpu_seconds: 0.25,
            peak_memory_bytes: 1 << 20,
        }
    }

    pub fn sample(wall: f64) -> Sample {
        Sample {
            compress: measure(wall),
            extract: measure(wall / 2.0),
            archive_bytes: 1000,
        }
    }

    pub fn measured() -> ToolResult {
        ToolResult {
            schema_version: SCHEMA_VERSION,
            tool: ToolRef {
                id: "7z".into(),
                version: Some("26.03".into()),
            },
            setting: SettingRef {
                id: "mx5".into(),
                compress_args: vec![
                    "a".into(),
                    "-mx5".into(),
                    "{archive}".into(),
                    "{input}/*".into(),
                ],
                extract_args: vec!["x".into(), "-o{outdir}".into(), "{archive}".into()],
            },
            class: "text".into(),
            corpus: CorpusRef {
                profile: "small".into(),
                manifest_blake3: "ab".repeat(32),
                files: 10,
                input_bytes: 5000,
            },
            threads: 4,
            private: true,
            repeats: Some(vec![sample(1.0), sample(1.5), sample(2.0)]),
            median: Some(sample(1.5)),
            verification: Some(Verification {
                verified: true,
                files_checked: 10,
                files_ok: 10,
            }),
            skipped: None,
        }
    }

    pub fn skipped() -> ToolResult {
        let mut r = measured();
        r.tool.id = "rar".into();
        r.tool.version = None;
        r.private = false;
        r.repeats = None;
        r.median = None;
        r.verification = None;
        r.skipped = Some("not installed".into());
        r
    }

    pub fn tools() -> ToolsFile {
        ToolsFile {
            schema_version: SCHEMA_VERSION,
            tools: vec![
                ToolEntry {
                    id: "7z".into(),
                    name: "7-Zip".into(),
                    status: "found".into(),
                    reason: None,
                    version: Some("26.03".into()),
                },
                ToolEntry {
                    id: "rar".into(),
                    name: "WinRAR (rar)".into(),
                    status: "skipped".into(),
                    reason: Some("not installed".into()),
                    version: None,
                },
            ],
        }
    }

    pub fn host() -> HostFile {
        HostFile {
            schema_version: SCHEMA_VERSION,
            host: "testbox".into(),
            os: "windows".into(),
            os_version: "Windows 11 Pro 10.0.26200".into(),
            cpu_model: "Test CPU".into(),
            logical_cores: 8,
            ram_bytes: 16 << 30,
            lpk_bench_version: "0.0.1".into(),
            git_commit: "0123456789ab".into(),
            rustc_version: "rustc 1.99.0".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;

    #[test]
    fn medians_use_the_middle_value() {
        let items = [sample(3.0), sample(1.0), sample(2.0)];
        let m = Sample::median_of(&items);
        assert_eq!(m.compress.wall_seconds, 2.0);
        assert_eq!(m.extract.wall_seconds, 1.0);
        let even = Sample::median_of(&[sample(1.0), sample(2.0)]);
        assert_eq!(even.compress.wall_seconds, 1.5);
        assert_eq!(median_u64(vec![u64::MAX, u64::MAX]), u64::MAX);
        assert_eq!(median_u64(vec![1, 2]), 1);
    }

    #[test]
    fn results_round_trip_through_json() {
        for r in [measured(), skipped()] {
            let back: ToolResult = serde_json::from_str(&render(&r)).expect("parse");
            assert_eq!(back, r);
        }
        assert_eq!(measured().file_name(), "7z-mx5-text.json");
        assert!(!render(&skipped()).contains("private"));
    }
}
