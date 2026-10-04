//! Result-file types. The JSON they serialise to is described by `bench/results/schema.json`
//! (every property there has a description with its unit); a test (`schema_and_types_agree`)
//! fails when the two disagree.
//!
//! Layout: `bench/results/<date>-<host>[-<n>]/` holds `host.json`, `tools.json` and one
//! `<tool>-<setting>-<class>.json` per combination. No file may contain absolute paths, user
//! names or environment dumps.

use serde::{Deserialize, Serialize};

use super::catalogue::Mode;
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
    pub mode: Mode,
    /// The thread count changes the archive size, not only the speed.
    pub ratio_depends_on_threads: bool,
}

/// The setting and the exact argument lists executed: thread argument filled in, paths shown
/// relative to the working directory (the parent of the class directory), so no machine-specific
/// path is recorded.
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
    /// Files in this class (not in the whole manifest).
    pub class_files: u64,
    /// Bytes in this class.
    pub class_bytes: u64,
}

/// Time of one step of a tar-stream pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepTimes {
    pub wall_seconds: f64,
    pub user_cpu_seconds: f64,
    pub kernel_cpu_seconds: f64,
}

/// One measured run. For tar-stream tools the times are tar step plus tool step and the steps
/// are recorded separately too; the memory peak is the larger of the two.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measure {
    pub wall_seconds: f64,
    pub user_cpu_seconds: f64,
    pub kernel_cpu_seconds: f64,
    pub peak_memory_bytes: u64,
    /// The timeout expired and the process tree was killed (either step, for tar-stream tools).
    pub timed_out: bool,
    /// Descendants of the program were still running when it exited and were killed (either step).
    pub descendants_killed: bool,
    /// Windows only: peak committed memory of the whole job object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_job_memory_bytes: Option<u64>,
    /// Windows only: peak committed memory of the largest process in the job object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_process_commit_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tar_step: Option<StepTimes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_step: Option<StepTimes>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeakMemoryKind {
    /// Windows: peak working set of the tool's main process.
    PeakWorkingSet,
    /// Linux: `ru_maxrss`.
    MaxRss,
}

/// The tar that made the stream for a tar-stream tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TarInfo {
    /// Flavour and version, for example `bsdtar 3.7.7`.
    pub tool: String,
    /// Tar format written (`pax`, `ustar`, ...).
    pub format: String,
    /// Always true: published time = tar step + tool step.
    pub in_published_time: bool,
}

/// How the numbers were taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    pub wall_cpu_method: String,
    pub peak_memory_kind: PeakMemoryKind,
    /// Always true: every repeat was extracted and verified.
    pub every_repeat_verified: bool,
    /// Names of the tool-configuration environment variables removed from the child's
    /// environment (names only, never values).
    pub env_stripped: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tar: Option<TarInfo>,
}

/// Why a combination failed. A failed result has no median; the repeats completed before the
/// failure may be recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    /// What went wrong, without paths: exit code, missing or empty archive, timeout, or the
    /// verification finding (missing, extra or different file, by manifest-relative name).
    pub reason: String,
    /// `compress`, `extract` or `verify`.
    pub step: String,
    /// The repeat (counting from 1) that failed.
    pub repeat: u32,
    pub timed_out: bool,
    pub descendants_killed: bool,
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
    /// Repeats asked for (`--repeats`); present on every result that ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeats_requested: Option<u32>,
    /// Why fewer repeats than requested were run (a long combination is measured once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeats_short: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<Measurement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeats: Option<Vec<Sample>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub median: Option<Sample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Why the combination was skipped; then there are no measurements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// The combination ran and failed; then there is no median.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<Failure>,
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

/// Median of an optional field: present only when every item has it.
fn median_opt_u64(items: &[Option<u64>]) -> Option<u64> {
    let all: Option<Vec<u64>> = items.iter().copied().collect();
    all.filter(|v| !v.is_empty()).map(median_u64)
}

impl StepTimes {
    fn median_of(items: &[StepTimes]) -> StepTimes {
        let f = |g: fn(&StepTimes) -> f64| median_f64(items.iter().map(g).collect());
        StepTimes {
            wall_seconds: f(|m| m.wall_seconds),
            user_cpu_seconds: f(|m| m.user_cpu_seconds),
            kernel_cpu_seconds: f(|m| m.kernel_cpu_seconds),
        }
    }
}

fn median_steps(items: &[Option<StepTimes>]) -> Option<StepTimes> {
    let all: Option<Vec<StepTimes>> = items.iter().copied().collect();
    all.filter(|v| !v.is_empty())
        .map(|v| StepTimes::median_of(&v))
}

impl Measure {
    /// Median of each field; optional fields only when every item has them.
    pub fn median_of(items: &[Measure]) -> Measure {
        let f = |g: fn(&Measure) -> f64| median_f64(items.iter().map(g).collect());
        let opt = |g: fn(&Measure) -> Option<u64>| {
            median_opt_u64(&items.iter().map(g).collect::<Vec<_>>())
        };
        Measure {
            wall_seconds: f(|m| m.wall_seconds),
            user_cpu_seconds: f(|m| m.user_cpu_seconds),
            kernel_cpu_seconds: f(|m| m.kernel_cpu_seconds),
            peak_memory_bytes: median_u64(items.iter().map(|m| m.peak_memory_bytes).collect()),
            timed_out: items.iter().any(|m| m.timed_out),
            descendants_killed: items.iter().any(|m| m.descendants_killed),
            peak_job_memory_bytes: opt(|m| m.peak_job_memory_bytes),
            peak_process_commit_bytes: opt(|m| m.peak_process_commit_bytes),
            tar_step: median_steps(&items.iter().map(|m| m.tar_step).collect::<Vec<_>>()),
            tool_step: median_steps(&items.iter().map(|m| m.tool_step).collect::<Vec<_>>()),
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
    /// False when the catalogue's command lines were never run by us.
    pub catalogue_verified: bool,
    pub manual: bool,
    /// `bench/tools.local.toml` overrode this tool on this machine.
    pub local_override: bool,
    /// The catalogue flags the tool as deduplicating across files (D-43); absent in results
    /// written before that.
    #[serde(default)]
    pub dedup: bool,
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
            .map(|d| {
                let (status, reason, version) = match &d.status {
                    Status::Found { version, .. } => ("found", None, Some(version.clone())),
                    Status::Skipped { reason } => ("skipped", Some(reason.clone()), None),
                };
                ToolEntry {
                    id: d.tool.id.clone(),
                    name: d.tool.name.clone(),
                    status: status.into(),
                    reason,
                    version,
                    catalogue_verified: d.tool.verified,
                    manual: d.tool.manual,
                    local_override: d.local_override,
                    dedup: d.tool.dedup,
                }
            })
            .collect();
        ToolsFile {
            schema_version: SCHEMA_VERSION,
            tools,
        }
    }
}

/// One antivirus product registered with Windows Security Center.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvProduct {
    pub name: String,
    /// The raw `productState`, as hex text (`0x60100`).
    pub product_state: String,
    /// Decoded from bits 12-15: `off`, `on`, `snoozed`, `expired` or `unknown`.
    pub scanner: String,
}

/// One planned combination and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCombination {
    pub tool: String,
    pub setting: String,
    pub class: String,
    /// `measured`, `failed` or `skipped`.
    pub outcome: String,
}

/// `run.json`: written last; its absence marks an aborted run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFile {
    pub schema_version: u32,
    /// Always true: the run reached its end.
    pub complete: bool,
    pub classes: Vec<String>,
    pub threads: u32,
    pub repeats_requested: u32,
    pub long_run_s: u64,
    /// Pause per 1000 files after deleting extracted files, in milliseconds (0: none). Absent in
    /// runs made before the pause existed, which had none.
    #[serde(default)]
    pub settle_ms_per_1000_files: u64,
    /// BLAKE3 of the catalogue file the run used.
    pub catalogue_blake3: String,
    /// Where `antivirus_end` came from (same values as the host file's `antivirus_source`).
    pub antivirus_end_source: String,
    /// The antivirus products queried again after the last combination, outside all timing.
    pub antivirus_end: Vec<AvProduct>,
    /// True when the names or decoded scanner states differ from the host file's start-of-run
    /// values (a snoozed scanner can resume in the middle of a run).
    pub antivirus_changed: bool,
    pub combinations: Vec<RunCombination>,
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
    /// 12 hex digits, with `-dirty` when the tree had uncommitted changes, or `unknown`.
    pub git_commit: String,
    pub rustc_version: String,
    /// Windows Defender real-time protection when the run started: `on`, `off` or `unknown`
    /// (best effort; always `unknown` outside Windows).
    pub defender_realtime: String,
    /// Where `antivirus` came from: `queried` (Windows Security Center answered; the list may be
    /// empty), `query-failed`, or `not-applicable` (not Windows, or no Security Center).
    pub antivirus_source: String,
    /// Antivirus products registered with Windows Security Center, when `antivirus_source` is
    /// `queried`.
    pub antivirus: Vec<AvProduct>,
    /// The run used the flag that allows results from a dirty or unknown build.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dirty_build_allowed: bool,
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
            timed_out: false,
            descendants_killed: false,
            peak_job_memory_bytes: Some(2 << 20),
            peak_process_commit_bytes: Some(1 << 21),
            tar_step: None,
            tool_step: None,
        }
    }

    /// A tar-stream measure: totals are the sum of the two steps.
    pub fn tar_measure(wall: f64) -> Measure {
        let tar = StepTimes {
            wall_seconds: wall / 4.0,
            user_cpu_seconds: 0.5,
            kernel_cpu_seconds: 0.25,
        };
        let tool = StepTimes {
            wall_seconds: wall,
            user_cpu_seconds: wall * 3.0,
            kernel_cpu_seconds: 0.5,
        };
        Measure {
            wall_seconds: tar.wall_seconds + tool.wall_seconds,
            user_cpu_seconds: tar.user_cpu_seconds + tool.user_cpu_seconds,
            kernel_cpu_seconds: tar.kernel_cpu_seconds + tool.kernel_cpu_seconds,
            peak_memory_bytes: 1 << 20,
            timed_out: false,
            descendants_killed: false,
            peak_job_memory_bytes: None,
            peak_process_commit_bytes: None,
            tar_step: Some(tar),
            tool_step: Some(tool),
        }
    }

    pub fn sample(wall: f64) -> Sample {
        Sample {
            compress: measure(wall),
            extract: measure(wall / 2.0),
            archive_bytes: 1000,
        }
    }

    pub fn tar_sample(wall: f64) -> Sample {
        Sample {
            compress: tar_measure(wall),
            extract: tar_measure(wall / 2.0),
            archive_bytes: 900,
        }
    }

    pub fn measurement() -> Measurement {
        Measurement {
            wall_cpu_method: "wall: monotonic clock around the process; cpu: OS process accounting"
                .into(),
            peak_memory_kind: PeakMemoryKind::PeakWorkingSet,
            every_repeat_verified: true,
            env_stripped: vec!["XZ_OPT".into(), "ZSTD_CLEVEL".into()],
            tar: None,
        }
    }

    pub fn measured() -> ToolResult {
        let repeats = vec![sample(1.0), sample(1.5), sample(2.0)];
        ToolResult {
            schema_version: SCHEMA_VERSION,
            tool: ToolRef {
                id: "7z".into(),
                version: Some("26.03".into()),
                mode: Mode::Directory,
                ratio_depends_on_threads: true,
            },
            setting: SettingRef {
                id: "mx5".into(),
                compress_args: vec![
                    "a".into(),
                    "-mx5".into(),
                    "-mmt=4".into(),
                    "out/a.7z".into(),
                    "text".into(),
                ],
                extract_args: vec!["x".into(), "-oout/x".into(), "out/a.7z".into()],
            },
            class: "text".into(),
            corpus: CorpusRef {
                profile: "small".into(),
                manifest_blake3: "ab".repeat(32),
                class_files: 10,
                class_bytes: 5000,
            },
            threads: 4,
            private: true,
            repeats_requested: Some(3),
            repeats_short: None,
            measurement: Some(measurement()),
            median: Some(Sample::median_of(&repeats)),
            repeats: Some(repeats),
            verification: Some(Verification {
                verified: true,
                files_checked: 10,
                files_ok: 10,
            }),
            skipped: None,
            failed: None,
        }
    }

    /// A combination that failed in the second repeat's extract step: one completed repeat, no
    /// median.
    pub fn failed() -> ToolResult {
        let mut r = measured();
        r.private = false;
        r.median = None;
        r.repeats = Some(vec![sample(1.0)]);
        r.verification = None;
        r.failed = Some(Failure {
            reason: "extract: exit code 2".into(),
            step: "extract".into(),
            repeat: 2,
            timed_out: false,
            descendants_killed: false,
        });
        r
    }

    /// A tar-stream result (zstd), with its tar recorded.
    pub fn tar_stream() -> ToolResult {
        let mut r = measured();
        r.tool.id = "zstd".into();
        r.tool.version = Some("1.5.7".into());
        r.tool.mode = Mode::TarStream;
        r.setting.id = "3".into();
        r.private = false;
        let repeats = vec![tar_sample(1.0), tar_sample(1.5), tar_sample(2.0)];
        r.median = Some(Sample::median_of(&repeats));
        r.repeats = Some(repeats);
        let mut m = measurement();
        m.tar = Some(TarInfo {
            tool: "bsdtar 3.7.7".into(),
            format: "pax".into(),
            in_published_time: true,
        });
        r.measurement = Some(m);
        r
    }

    pub fn skipped() -> ToolResult {
        let mut r = measured();
        r.tool.id = "rar".into();
        r.tool.version = None;
        r.repeats_requested = None;
        r.private = false;
        r.measurement = None;
        r.repeats = None;
        r.median = None;
        r.verification = None;
        r.skipped = Some("not installed".into());
        r
    }

    pub fn tools() -> ToolsFile {
        let entry = |id: &str, name: &str, found: Option<&str>| ToolEntry {
            id: id.into(),
            name: name.into(),
            status: if found.is_some() { "found" } else { "skipped" }.into(),
            reason: found.is_none().then(|| "not installed".to_string()),
            version: found.map(String::from),
            catalogue_verified: true,
            manual: false,
            local_override: false,
            dedup: matches!(id, "zpaqfranz" | "tsaur"),
        };
        ToolsFile {
            schema_version: SCHEMA_VERSION,
            tools: vec![
                entry("7z", "7-Zip", Some("26.03")),
                entry("zstd", "Zstandard", Some("1.5.7")),
                entry("rar", "WinRAR (rar)", None),
            ],
        }
    }

    /// The `run.json` that matches `results` (outcomes read from the results themselves).
    pub fn run_file(results: &[ToolResult]) -> RunFile {
        let mut classes: Vec<String> = Vec::new();
        for r in results {
            if !classes.contains(&r.class) {
                classes.push(r.class.clone());
            }
        }
        RunFile {
            schema_version: SCHEMA_VERSION,
            complete: true,
            classes,
            threads: results.first().map_or(4, |r| r.threads),
            repeats_requested: 3,
            long_run_s: 120,
            settle_ms_per_1000_files: 500,
            catalogue_blake3: "ef".repeat(32),
            antivirus_end_source: "queried".into(),
            antivirus_end: host().antivirus,
            antivirus_changed: false,
            combinations: results
                .iter()
                .map(|r| RunCombination {
                    tool: r.tool.id.clone(),
                    setting: r.setting.id.clone(),
                    class: r.class.clone(),
                    outcome: if r.skipped.is_some() {
                        "skipped"
                    } else if r.failed.is_some() {
                        "failed"
                    } else {
                        "measured"
                    }
                    .into(),
                })
                .collect(),
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
            defender_realtime: "unknown".into(),
            antivirus_source: "queried".into(),
            antivirus: vec![AvProduct {
                name: "Windows Defender".into(),
                product_state: "0x60100".into(),
                scanner: "off".into(),
            }],
            dirty_build_allowed: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;

    #[test]
    fn tools_json_records_the_dedup_flag_and_old_files_default_to_false() {
        let cat =
            crate::run::catalogue::Catalogue::parse(include_str!("../../../../bench/tools.toml"))
                .expect("catalogue");
        let found: Vec<Discovered> = ["zpaqfranz", "zstd"]
            .iter()
            .map(|id| Discovered {
                tool: cat.get(id).expect("tool").clone(),
                status: Status::Skipped {
                    reason: "not installed".into(),
                },
                local_override: false,
            })
            .collect();
        let file = ToolsFile::from_discovered(&found);
        assert!(file.tools[0].dedup && !file.tools[1].dedup);
        let text = render(&file);
        assert!(text.contains("\"dedup\": true") && text.contains("\"dedup\": false"));
        let back: ToolsFile = serde_json::from_str(&text).expect("round trip");
        assert_eq!(back, file);
        let old = text
            .replace(",\n      \"dedup\": true", "")
            .replace(",\n      \"dedup\": false", "");
        assert!(!old.contains("dedup"));
        let parsed: ToolsFile = serde_json::from_str(&old).expect("old file");
        assert!(parsed.tools.iter().all(|t| !t.dedup));
    }

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
    fn optional_fields_have_a_median_only_when_every_repeat_has_them() {
        let mut items = [sample(1.0), sample(2.0), sample(3.0)];
        assert_eq!(
            Sample::median_of(&items).compress.peak_job_memory_bytes,
            Some(2 << 20)
        );
        items[1].compress.peak_job_memory_bytes = None;
        assert_eq!(
            Sample::median_of(&items).compress.peak_job_memory_bytes,
            None
        );
        let t = Sample::median_of(&[tar_sample(1.0), tar_sample(2.0), tar_sample(3.0)]);
        assert_eq!(t.compress.tool_step.map(|s| s.wall_seconds), Some(2.0));
    }

    #[test]
    fn results_round_trip_through_json() {
        for r in [measured(), skipped(), tar_stream(), failed()] {
            let back: ToolResult = serde_json::from_str(&render(&r)).expect("parse");
            assert_eq!(back, r);
        }
        assert_eq!(measured().file_name(), "7z-mx5-text.json");
        assert!(!render(&skipped()).contains("private"));
        assert!(render(&measured()).contains("\"mode\": \"directory\""));
        assert!(render(&tar_stream()).contains("\"mode\": \"tar-stream\""));
    }
}
