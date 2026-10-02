//! Validation of a results directory against `bench/results/schema.json`.
//!
//! Beyond the schema: no string anywhere may contain an absolute path, a result file's name must
//! equal `<tool>-<setting>-<class>.json` of its contents, its tool must appear in `tools.json`,
//! and `verified` must agree with the counts. Every problem is reported as
//! `<file>: <field>: <message>`.

use std::path::Path;

use anyhow::{Context, Result};
use jsonschema::Validator;
use serde_json::Value;

use super::catalogue::Mode;
use super::result::{RunFile, Sample, ToolResult};

/// The committed schema, embedded so the binary and the file cannot disagree.
pub const SCHEMA_TEXT: &str = include_str!("../../../../bench/results/schema.json");

/// Which `$defs` entry a file is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Result,
    Tools,
    Host,
    Run,
}

impl Kind {
    fn def(self) -> &'static str {
        match self {
            Kind::Result => "result",
            Kind::Tools => "tools",
            Kind::Host => "host",
            Kind::Run => "run",
        }
    }
}

pub struct Schemas {
    result: Validator,
    tools: Validator,
    host: Validator,
    run: Validator,
}

impl std::fmt::Debug for Schemas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Schemas")
    }
}

pub fn schema_value() -> Result<Value> {
    serde_json::from_str(SCHEMA_TEXT).context("bench/results/schema.json is not valid JSON")
}

fn build(schema: &Value, kind: Kind) -> Result<Validator> {
    let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
    let wrapper = serde_json::json!({
        "$schema": schema.get("$schema").cloned().unwrap_or(Value::Null),
        "$defs": defs,
        "$ref": format!("#/$defs/{}", kind.def()),
    });
    jsonschema::validator_for(&wrapper)
        .map_err(|e| anyhow::anyhow!("schema.json `{}` does not compile: {e}", kind.def()))
}

impl Schemas {
    pub fn load() -> Result<Schemas> {
        let schema = schema_value()?;
        Ok(Schemas {
            result: build(&schema, Kind::Result)?,
            tools: build(&schema, Kind::Tools)?,
            host: build(&schema, Kind::Host)?,
            run: build(&schema, Kind::Run)?,
        })
    }

    fn validator(&self, kind: Kind) -> &Validator {
        match kind {
            Kind::Result => &self.result,
            Kind::Tools => &self.tools,
            Kind::Host => &self.host,
            Kind::Run => &self.run,
        }
    }

    /// Schema and absolute-path problems of one parsed file, each prefixed with `file`.
    pub fn check_value(&self, kind: Kind, file: &str, value: &Value) -> Vec<String> {
        let mut problems: Vec<String> = self
            .validator(kind)
            .iter_errors(value)
            .map(|e| {
                let at = e.instance_path().to_string();
                let at = if at.is_empty() {
                    "(root)".to_string()
                } else {
                    at
                };
                format!("{file}: {at}: {e}")
            })
            .collect();
        problems.sort();
        scan_absolute_paths(value, "", file, &mut problems);
        problems
    }
}

fn absolute_path_regex() -> Option<regex::Regex> {
    // A path start (drive, UNC, or `/dir/`) at the start of a string, after a separator, or right
    // after a short flag such as `-o`.
    regex::Regex::new(r#"(?:^|[\s=,;"'(\[]|-[A-Za-z]{1,2})(?:[A-Za-z]:[\\/]|\\\\|/[^\s/*{}]+/)"#)
        .ok()
}

fn scan_absolute_paths(value: &Value, pointer: &str, file: &str, out: &mut Vec<String>) {
    fn walk(re: &regex::Regex, v: &Value, pointer: &str, file: &str, out: &mut Vec<String>) {
        match v {
            Value::String(s) => {
                let trimmed = s.trim_start();
                let rooted =
                    trimmed.starts_with('/') && trimmed.len() > 1 && !trimmed.starts_with("/*");
                if re.is_match(s) || rooted {
                    let at = if pointer.is_empty() {
                        "(root)"
                    } else {
                        pointer
                    };
                    out.push(format!("{file}: {at}: absolute path in string `{s}`"));
                }
            }
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    walk(re, item, &format!("{pointer}/{i}"), file, out);
                }
            }
            Value::Object(map) => {
                for (k, item) in map {
                    walk(re, item, &format!("{pointer}/{k}"), file, out);
                }
            }
            _ => {}
        }
    }
    if let Some(re) = absolute_path_regex() {
        walk(&re, value, pointer, file, out);
    }
}

fn valid_date(date: &str) -> bool {
    let parts: Vec<&str> = date.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return false;
    };
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<u32>(), m.parse::<u32>(), d.parse::<u32>()) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (2024..=2200).contains(&y) && (1..=days).contains(&d)
}

/// Split `<YYYY-MM-DD>-<rest>` where the date is a real calendar date and `rest` is `[a-z0-9-]+`.
fn split_dir_name(name: &str) -> Option<(&str, &str)> {
    let (date, rest) = (name.get(..10)?, name.get(11..)?);
    let ok = name.as_bytes().get(10) == Some(&b'-')
        && date.len() == 10
        && valid_date(date)
        && !rest.is_empty()
        && rest
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
    ok.then_some((date, rest))
}

fn dir_name_ok(name: &str) -> bool {
    split_dir_name(name).is_some()
}

/// Does the directory name's `rest` equal `host` or `host-<n>` with n >= 2 (a repeat run on the
/// same UTC day)?
fn host_matches(rest: &str, host: &str) -> bool {
    match rest.strip_prefix(host) {
        Some("") => true,
        Some(suffix) => suffix
            .strip_prefix('-')
            .is_some_and(|n| !n.starts_with('0') && n.parse::<u32>().is_ok_and(|n| n >= 2)),
        None => false,
    }
}

/// Numbers compare with a tiny relative tolerance, everything else exactly. Differences are
/// appended as JSON-pointer-like paths.
fn diff_values(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let same = match (x.as_f64(), y.as_f64()) {
                (Some(fx), Some(fy)) if x.is_f64() || y.is_f64() => {
                    let (x, y) = (fx, fy);
                    (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0)
                }
                _ => x == y,
            };
            if !same {
                out.push(path.to_string());
            }
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => diff_values(&format!("{path}/{k}"), p, q, out),
                    _ => out.push(format!("{path}/{k}")),
                }
            }
        }
        (x, y) if x == y => {}
        _ => out.push(path.to_string()),
    }
}

fn read_json(path: &Path, label: &str, problems: &mut Vec<String>) -> Option<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                problems.push(format!("{label}: (root): not valid JSON: {e}"));
                None
            }
        },
        Err(e) => {
            problems.push(format!("{label}: (root): cannot read: {e}"));
            None
        }
    }
}

/// Outcome of validating a directory.
#[derive(Debug, Default)]
pub struct Report {
    /// Number of per-combination result files checked.
    pub results: usize,
    pub problems: Vec<String>,
    /// Things that are accepted but worth saying, such as probe files taken on trust.
    pub notes: Vec<String>,
}

/// Check `<dir>` (a `<date>-<host>` results directory): `host.json`, `tools.json` and every
/// result file. `probe-*.json` files belong to the probes (PLAN P0-4) and are skipped.
///
/// A parent such as `bench/results` (not itself a `<date>-<host>` directory and without
/// `host.json`/`tools.json`) is also accepted: each subdirectory is checked, files such as
/// `README.md` and `schema.json` are ignored, and problems are prefixed with the subdirectory.
pub fn validate_dir(dir: &Path) -> Result<Report> {
    let schemas = Schemas::load()?;
    let abs = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let own_name_ok = abs
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(dir_name_ok);
    if !dir.is_dir()
        || own_name_ok
        || dir.join("host.json").exists()
        || dir.join("tools.json").exists()
    {
        return validate_one(&schemas, dir);
    }
    let mut subdirs: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    let mut total = Report::default();
    if subdirs.is_empty() {
        total
            .problems
            .push(format!("{}: no results directories found", dir.display()));
    }
    for sub in subdirs {
        let label = sub
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let r = validate_one(&schemas, &sub)?;
        total.results += r.results;
        total
            .notes
            .extend(r.notes.into_iter().map(|n| format!("{label}/{n}")));
        total
            .problems
            .extend(r.problems.into_iter().map(|p| format!("{label}/{p}")));
    }
    Ok(total)
}

/// What `tools.json` says about one tool.
struct ToolState {
    found: bool,
    version: Option<String>,
}

fn validate_one(schemas: &Schemas, dir: &Path) -> Result<Report> {
    let mut report = Report::default();
    if !dir.is_dir() {
        report
            .problems
            .push(format!("{}: not a directory", dir.display()));
        return Ok(report);
    }
    let abs = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let dir_name = abs
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let parsed_name = split_dir_name(&dir_name);
    if parsed_name.is_none() {
        report.problems.push(format!(
            "{dir_name}: directory name must be <YYYY-MM-DD>-<host>[-<n>] with a real date \
             and host in [a-z0-9-]"
        ));
    }

    let mut tools: std::collections::BTreeMap<String, ToolState> = Default::default();
    for (name, kind) in [("host.json", Kind::Host), ("tools.json", Kind::Tools)] {
        let path = dir.join(name);
        if !path.is_file() {
            report
                .problems
                .push(format!("{name}: (root): file is missing"));
            continue;
        }
        let Some(v) = read_json(&path, name, &mut report.problems) else {
            continue;
        };
        let schema_problems = schemas.check_value(kind, name, &v);
        let clean = schema_problems.is_empty();
        report.problems.extend(schema_problems);
        if !clean {
            continue;
        }
        match kind {
            Kind::Host => {
                let commit = v.get("git_commit").and_then(Value::as_str).unwrap_or("");
                let allowed = v
                    .get("dirty_build_allowed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !crate::run::host::build_is_clean(commit) && !allowed {
                    report.problems.push(format!(
                        "{name}: /git_commit: `{commit}` is not a clean commit and \
                         /dirty_build_allowed is not set"
                    ));
                }
                let host = v.get("host").and_then(Value::as_str).unwrap_or("");
                if let Some((_, rest)) = parsed_name {
                    if !host_matches(rest, host) {
                        report.problems.push(format!(
                            "{name}: /host: `{host}` does not match the directory name \
                             `{dir_name}` (expected <date>-{host} or <date>-{host}-<n>, n >= 2)"
                        ));
                    }
                }
            }
            Kind::Tools => {
                for t in v
                    .get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let id = t
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    tools.insert(
                        id,
                        ToolState {
                            found: t.get("status").and_then(Value::as_str) == Some("found"),
                            version: t.get("version").and_then(Value::as_str).map(String::from),
                        },
                    );
                }
            }
            Kind::Result | Kind::Run => {}
        }
    }

    // run.json is written last: without it the run was aborted and nothing can be trusted to be
    // complete.
    let mut run: Option<RunFile> = None;
    let run_path = dir.join("run.json");
    if !run_path.is_file() {
        report.problems.push(
            "run.json: (root): file is missing (an aborted run does not validate)".to_string(),
        );
    } else if let Some(v) = read_json(&run_path, "run.json", &mut report.problems) {
        let schema_problems = schemas.check_value(Kind::Run, "run.json", &v);
        let clean = schema_problems.is_empty();
        report.problems.extend(schema_problems);
        if clean {
            match serde_json::from_value::<RunFile>(v) {
                Ok(r) => run = Some(r),
                Err(e) => report
                    .problems
                    .push(format!("run.json: (root): does not fit the run types: {e}")),
            }
        }
    }
    let mut seen: std::collections::BTreeMap<(String, String, String), &'static str> =
        Default::default();

    let mut names: Vec<String> = Vec::new();
    let mut probes = 0usize;
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            report.problems.push(format!(
                "{name}: unexpected directory in a results directory"
            ));
        } else if name == "host.json" || name == "tools.json" || name == "run.json" {
        } else if name.starts_with("probe-") && name.ends_with(".json") {
            probes += 1;
        } else if name.ends_with(".json") {
            names.push(name);
        } else {
            report.problems.push(format!(
                "{name}: unexpected file (a results directory holds host.json, tools.json, \
                 result files and probe-*.json)"
            ));
        }
    }
    if probes > 0 {
        report.notes.push(format!(
            "{probes} probe-*.json file(s) accepted by name only; their contents are not checked \
             (probe formats arrive with PLAN P0-4)"
        ));
    }
    names.sort();
    let mut corpus_seen: Option<(String, String, String)> = None;
    for name in names {
        report.results += 1;
        let Some(v) = read_json(&dir.join(&name), &name, &mut report.problems) else {
            continue;
        };
        let schema_problems = schemas.check_value(Kind::Result, &name, &v);
        let clean = schema_problems.is_empty();
        report.problems.extend(schema_problems);
        if !clean {
            continue;
        }
        match serde_json::from_value::<ToolResult>(v) {
            Ok(r) => {
                semantic_checks(&name, &r, &tools, &mut report.problems);
                let outcome = if r.skipped.is_some() {
                    "skipped"
                } else if r.failed.is_some() {
                    "failed"
                } else {
                    "measured"
                };
                seen.insert(
                    (r.tool.id.clone(), r.setting.id.clone(), r.class.clone()),
                    outcome,
                );
                if let Some(run) = &run {
                    if r.threads != run.threads {
                        report.problems.push(format!(
                            "{name}: /threads: {} but run.json says {}",
                            r.threads, run.threads
                        ));
                    }
                    if r.skipped.is_none() && r.repeats_requested != Some(run.repeats_requested) {
                        report.problems.push(format!(
                            "{name}: /repeats_requested: {:?} but run.json says {}",
                            r.repeats_requested, run.repeats_requested
                        ));
                    }
                }
                let here = (
                    r.corpus.profile.clone(),
                    r.corpus.manifest_blake3.clone(),
                    name.clone(),
                );
                match &corpus_seen {
                    None => corpus_seen = Some(here),
                    Some((profile, hash, first)) => {
                        if *profile != here.0 {
                            report.problems.push(format!(
                                "{name}: /corpus/profile: `{}` but {first} has `{profile}` \
                                 (one profile per results directory)",
                                here.0
                            ));
                        }
                        if *hash != here.1 {
                            report.problems.push(format!(
                                "{name}: /corpus/manifest_blake3: differs from {first} \
                                 (one manifest per results directory)"
                            ));
                        }
                    }
                }
            }
            Err(e) => report.problems.push(format!(
                "{name}: (root): does not fit the result types: {e}"
            )),
        }
    }
    if let Some(run) = &run {
        let mut listed: std::collections::BTreeSet<(String, String, String)> = Default::default();
        for c in &run.combinations {
            let key = (c.tool.clone(), c.setting.clone(), c.class.clone());
            let label = format!("{}/{}/{}", c.tool, c.setting, c.class);
            if !listed.insert(key.clone()) {
                report.problems.push(format!(
                    "run.json: /combinations: `{label}` is listed twice"
                ));
            }
            match seen.get(&key) {
                None => report.problems.push(format!(
                    "run.json: /combinations: `{label}` has no result file"
                )),
                Some(outcome) if *outcome != c.outcome => report.problems.push(format!(
                    "run.json: /combinations: `{label}` is listed as {} but its file says {outcome}",
                    c.outcome
                )),
                Some(_) => {}
            }
            if !run.classes.contains(&c.class) {
                report.problems.push(format!(
                    "run.json: /combinations: `{label}` is of a class not in /classes"
                ));
            }
        }
        for (tool, setting, class) in seen.keys() {
            if !listed.contains(&(tool.clone(), setting.clone(), class.clone())) {
                report.problems.push(format!(
                    "run.json: /combinations: result file for `{tool}/{setting}/{class}` is not listed"
                ));
            }
        }
    }
    Ok(report)
}

fn semantic_checks(
    name: &str,
    r: &ToolResult,
    tools: &std::collections::BTreeMap<String, ToolState>,
    out: &mut Vec<String>,
) {
    let expected = r.file_name();
    if name != expected {
        out.push(format!(
            "{name}: (file name): must be `{expected}` for the tool, setting and class inside"
        ));
    }
    let ran = r.skipped.is_none();
    match tools.get(&r.tool.id) {
        None => out.push(format!(
            "{name}: /tool/id: `{}` does not appear in tools.json",
            r.tool.id
        )),
        Some(state) if ran => {
            if !state.found {
                out.push(format!(
                    "{name}: /tool/id: `{}` is marked skipped in tools.json but has measurements",
                    r.tool.id
                ));
            } else if state.version != r.tool.version {
                out.push(format!(
                    "{name}: /tool/version: {:?} differs from tools.json ({:?})",
                    r.tool.version, state.version
                ));
            }
        }
        Some(_) => {}
    }
    if !ran {
        return;
    }
    let empty = Vec::new();
    let repeats = r.repeats.as_ref().unwrap_or(&empty);
    // Repeat counts: all requested repeats ran, unless the combination failed (then the failing
    // repeat is the one after the completed ones) or was marked as measured once for being long.
    match r.repeats_requested {
        None => out.push(format!(
            "{name}: /repeats_requested: missing from a result that ran"
        )),
        Some(req) => {
            let run = repeats.len() as u64;
            if let Some(f) = &r.failed {
                if u64::from(f.repeat) != run + 1 || run >= u64::from(req) {
                    out.push(format!(
                        "{name}: /failed/repeat: {} is not the repeat after the {run} recorded \
                         (of {req} requested)",
                        f.repeat
                    ));
                }
                if r.repeats_short.is_some() {
                    out.push(format!(
                        "{name}: /repeats_short: present on a failed result"
                    ));
                }
            } else if r.repeats_short.is_some() {
                if run >= u64::from(req) {
                    out.push(format!(
                        "{name}: /repeats_short: present although all {req} requested repeats ran"
                    ));
                }
            } else if run != u64::from(req) {
                out.push(format!(
                    "{name}: /repeats: {run} recorded but {req} requested and no /repeats_short \
                     explains the difference"
                ));
            }
        }
    }
    // A repeat that timed out or lost descendants cannot be part of a good result.
    let flagged = repeats.iter().any(|s| {
        [&s.compress, &s.extract]
            .iter()
            .any(|m| m.timed_out || m.descendants_killed)
    });
    let verified_true = r.verification.as_ref().is_some_and(|v| v.verified);
    if flagged && r.failed.is_none() {
        out.push(format!(
            "{name}: /repeats: a repeat timed out or lost descendants but /failed is absent"
        ));
    }
    if (flagged || r.failed.is_some()) && verified_true {
        out.push(format!(
            "{name}: /verification/verified: true for a result with a failed, timed-out or \
             descendant-killing repeat"
        ));
    }
    if (flagged || r.failed.is_some()) && r.median.is_some() {
        out.push(format!(
            "{name}: /median: present for a result with a failed, timed-out or \
             descendant-killing repeat"
        ));
    }
    if let Some(ver) = &r.verification {
        let all = ver.files_ok == ver.files_checked && ver.files_checked == r.corpus.class_files;
        if ver.files_ok > ver.files_checked {
            out.push(format!(
                "{name}: /verification/files_ok: exceeds files_checked"
            ));
        } else if r.failed.is_none() && ver.verified != all {
            out.push(format!(
                "{name}: /verification/verified: disagrees with the counts \
                 (verified means files_ok == files_checked == corpus.class_files)"
            ));
        }
    }
    if let Some(median) = &r.median {
        let expected_median = Sample::median_of(repeats);
        let mut diffs = Vec::new();
        diff_values(
            "/median",
            &serde_json::to_value(median).unwrap_or(Value::Null),
            &serde_json::to_value(expected_median).unwrap_or(Value::Null),
            &mut diffs,
        );
        for d in diffs {
            out.push(format!(
                "{name}: {d}: is not the median of the repeats (recomputed from /repeats)"
            ));
        }
    }
    // Tar-stream tools: the tar must be recorded and the steps must add up in every repeat.
    let tar_stream = r.tool.mode == Mode::TarStream;
    let has_tar = r.measurement.as_ref().is_some_and(|m| m.tar.is_some());
    if tar_stream != has_tar {
        out.push(format!(
            "{name}: /measurement/tar: {} for a {} tool",
            if has_tar { "present" } else { "missing" },
            if tar_stream {
                "tar-stream"
            } else {
                "directory"
            }
        ));
    }
    for (i, s) in repeats.iter().enumerate() {
        for (what, m) in [("compress", &s.compress), ("extract", &s.extract)] {
            let at = format!("{name}: /repeats/{i}/{what}");
            match (tar_stream, m.tar_step, m.tool_step) {
                (true, Some(t), Some(c)) => {
                    let mut d = Vec::new();
                    let total = |a: f64, b: f64| serde_json::json!(a + b);
                    for (k, sum, have) in [
                        (
                            "wall_seconds",
                            total(t.wall_seconds, c.wall_seconds),
                            m.wall_seconds,
                        ),
                        (
                            "user_cpu_seconds",
                            total(t.user_cpu_seconds, c.user_cpu_seconds),
                            m.user_cpu_seconds,
                        ),
                        (
                            "kernel_cpu_seconds",
                            total(t.kernel_cpu_seconds, c.kernel_cpu_seconds),
                            m.kernel_cpu_seconds,
                        ),
                    ] {
                        diff_values(k, &sum, &serde_json::json!(have), &mut d);
                    }
                    for k in d {
                        out.push(format!("{at}/{k}: is not tar_step + tool_step"));
                    }
                }
                (true, _, _) => out.push(format!(
                    "{at}: tar_step and tool_step are both required for a tar-stream tool"
                )),
                (false, None, None) => {}
                (false, _, _) => out.push(format!(
                    "{at}: tar_step/tool_step belong to tar-stream tools only"
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::result::samples;
    use crate::run::result::{render, ToolResult};
    use serde_json::json;
    use std::collections::BTreeSet;

    fn to_value<T: serde::Serialize>(t: &T) -> Value {
        serde_json::to_value(t).expect("value")
    }

    fn write_dir(root: &Path, results: &[ToolResult]) -> std::path::PathBuf {
        let dir = root.join("2026-10-01-testbox");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("host.json"), render(&samples::host())).expect("host");
        std::fs::write(dir.join("tools.json"), render(&samples::tools())).expect("tools");
        std::fs::write(dir.join("run.json"), render(&samples::run_file(results))).expect("run");
        for r in results {
            std::fs::write(dir.join(r.file_name()), render(r)).expect("result");
        }
        dir
    }

    fn problems(dir: &Path) -> Vec<String> {
        validate_dir(dir).expect("validate").problems
    }

    #[test]
    fn generated_samples_validate() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = write_dir(tmp.path(), &[samples::measured(), samples::skipped()]);
        let report = validate_dir(&dir).expect("validate");
        assert_eq!(report.results, 2);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
    }

    #[test]
    fn this_machines_host_file_validates() {
        let schemas = Schemas::load().expect("schemas");
        let host = to_value(&crate::run::host::collect(false));
        let p = schemas.check_value(Kind::Host, "host.json", &host);
        assert!(p.is_empty(), "{p:?}");
    }

    #[test]
    fn the_embedded_schema_compiles() {
        assert!(Schemas::load().is_ok());
    }

    /// Property names of a `$defs` entry, and its `required` list.
    fn schema_keys(schema: &Value, def: &str) -> (BTreeSet<String>, BTreeSet<String>) {
        let d = &schema["$defs"][def];
        let props = d["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("schema def `{def}` has no properties"))
            .keys()
            .cloned()
            .collect();
        let required = d["required"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        (props, required)
    }

    /// The serialised values of one type, in all their shapes, must use exactly the schema's
    /// properties (union) and always include the required ones.
    fn assert_agree(schema: &Value, def: &str, values: &[&Value]) {
        let (props, required) = schema_keys(schema, def);
        let mut union = BTreeSet::new();
        for v in values {
            let keys: BTreeSet<String> = v.as_object().expect("object").keys().cloned().collect();
            assert!(
                keys.is_subset(&props),
                "{def}: types emit {keys:?}, schema allows {props:?}"
            );
            assert!(
                required.is_subset(&keys),
                "{def}: required {required:?} missing from {keys:?}"
            );
            union.extend(keys);
        }
        assert_eq!(
            union, props,
            "{def}: schema lists properties the types never emit"
        );
    }

    #[test]
    fn schema_and_types_agree() {
        let schema = schema_value().expect("schema");
        let mut short = samples::measured();
        short.repeats_short = Some("measured once: long run".into());
        short.repeats = short.repeats.map(|r| r[..1].to_vec());
        let (m, s, z, f, sh) = (
            to_value(&samples::measured()),
            to_value(&samples::skipped()),
            to_value(&samples::tar_stream()),
            to_value(&samples::failed()),
            to_value(&short),
        );
        assert_agree(&schema, "result", &[&m, &s, &z, &f, &sh]);
        let run = to_value(&samples::run_file(&[samples::measured()]));
        assert_agree(&schema, "run", &[&run]);
        assert_agree(&schema, "run_combination", &[&run["combinations"][0]]);
        assert_agree(&schema, "failure", &[&f["failed"]]);
        assert_agree(&schema, "tool_ref", &[&m["tool"], &s["tool"]]);
        assert_agree(&schema, "setting_ref", &[&m["setting"]]);
        assert_agree(&schema, "corpus_ref", &[&m["corpus"]]);
        assert_agree(&schema, "sample", &[&m["median"], &m["repeats"][0]]);
        // `measure` has Windows-only and tar-stream-only optional fields: union over both shapes.
        assert_agree(
            &schema,
            "measure",
            &[&m["median"]["compress"], &z["median"]["compress"]],
        );
        assert_agree(
            &schema,
            "step_times",
            &[&z["median"]["compress"]["tar_step"]],
        );
        assert_agree(
            &schema,
            "measurement",
            &[&m["measurement"], &z["measurement"]],
        );
        assert_agree(&schema, "tar_info", &[&z["measurement"]["tar"]]);
        assert_agree(&schema, "verification", &[&m["verification"]]);
        let t = to_value(&samples::tools());
        assert_agree(&schema, "tools", &[&t]);
        assert_agree(&schema, "tool_entry", &[&t["tools"][0], &t["tools"][2]]);
        let host = to_value(&samples::host());
        assert_agree(&schema, "host", &[&host]);
        assert_agree(&schema, "av_product", &[&host["antivirus"][0]]);
        // Every def with properties is covered above (a new def must be added to this test).
        let covered = [
            "result",
            "tool_ref",
            "setting_ref",
            "corpus_ref",
            "sample",
            "measure",
            "step_times",
            "measurement",
            "tar_info",
            "verification",
            "failure",
            "av_product",
            "run",
            "run_combination",
            "tools",
            "tool_entry",
            "host",
        ];
        let with_props: BTreeSet<String> = schema["$defs"]
            .as_object()
            .expect("defs")
            .iter()
            .filter(|(_, d)| d.get("properties").is_some())
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(with_props, covered.iter().map(|s| s.to_string()).collect());
    }

    #[test]
    fn missing_field_is_named_with_its_file() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = write_dir(tmp.path(), &[samples::measured()]);
        let mut v = to_value(&samples::measured());
        v.as_object_mut().expect("obj").remove("threads");
        std::fs::write(dir.join("7z-mx5-text.json"), v.to_string()).expect("write");
        let p = problems(&dir);
        assert!(
            p.iter()
                .any(|m| m.starts_with("7z-mx5-text.json:") && m.contains("threads")),
            "{p:?}"
        );
        // A host.json field.
        let mut h = to_value(&samples::host());
        h.as_object_mut().expect("obj").remove("cpu_model");
        std::fs::write(dir.join("host.json"), h.to_string()).expect("write");
        assert!(problems(&dir)
            .iter()
            .any(|m| m.starts_with("host.json:") && m.contains("cpu_model")));
    }

    #[test]
    fn wrong_type_is_reported_with_the_field_path() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = write_dir(tmp.path(), &[samples::measured()]);
        let mut v = to_value(&samples::measured());
        v["median"]["compress"]["wall_seconds"] = json!("fast");
        std::fs::write(dir.join("7z-mx5-text.json"), v.to_string()).expect("write");
        let p = problems(&dir);
        assert!(
            p.iter()
                .any(|m| m.starts_with("7z-mx5-text.json: /median/compress/wall_seconds:")),
            "{p:?}"
        );
    }

    #[test]
    fn absolute_paths_are_rejected_everywhere() {
        let tmp = tempfile::tempdir().expect("tmp");
        for (path, bad) in [
            ("/setting/compress_args/1", "C:\\Users\\me\\x.7z"),
            ("/setting/compress_args/1", "-o/home/me/out"),
            ("/setting/extract_args/0", "/usr/bin/7zz"),
            ("/tool/version", "D:/tools/7z 26.03"),
            ("/tool/version", "\\\\server\\share\\7z"),
        ] {
            let dir = write_dir(tmp.path(), &[samples::measured()]);
            let mut v = to_value(&samples::measured());
            *v.pointer_mut(path).expect("pointer") = json!(bad);
            std::fs::write(dir.join("7z-mx5-text.json"), v.to_string()).expect("write");
            let p = problems(&dir);
            assert!(
                p.iter().any(|m| m.starts_with("7z-mx5-text.json:")
                    && m.contains(path)
                    && m.contains("absolute path")),
                "{bad}: {p:?}"
            );
        }
        // The placeholders and flags of the real catalogue are fine.
        let dir = write_dir(tmp.path(), &[samples::measured()]);
        assert!(problems(&dir).is_empty());
    }

    #[test]
    fn inconsistent_results_are_rejected() {
        let tmp = tempfile::tempdir().expect("tmp");

        // Skipped result that still carries measurements.
        let mut both = samples::measured();
        both.skipped = Some("not installed".into());
        let dir = write_dir(tmp.path(), &[both]);
        assert!(!problems(&dir).is_empty());

        // Measured result without verification.
        let mut no_ver = samples::measured();
        no_ver.verification = None;
        let dir = write_dir(tmp.path(), &[no_ver]);
        assert!(!problems(&dir).is_empty());

        // verified = true with a failed file.
        let mut lying = samples::measured();
        lying.verification = Some(crate::run::result::Verification {
            verified: true,
            files_checked: 10,
            files_ok: 9,
        });
        let dir = write_dir(tmp.path(), &[lying]);
        assert!(problems(&dir)
            .iter()
            .any(|m| m.contains("/verification/verified")));

        // File name that does not match the contents.
        let dir = write_dir(tmp.path(), &[]);
        std::fs::write(dir.join("wrong-name.json"), render(&samples::measured())).expect("write");
        assert!(problems(&dir)
            .iter()
            .any(|m| m.starts_with("wrong-name.json:")));

        // Tool absent from tools.json.
        let mut stranger = samples::measured();
        stranger.tool.id = "ghost".into();
        let dir = write_dir(tmp.path(), &[stranger]);
        assert!(problems(&dir).iter().any(|m| m.contains("tools.json")));
    }

    #[test]
    fn failed_results_validate_and_cannot_carry_a_median_or_verified() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = write_dir(tmp.path(), &[samples::failed()]);
        assert!(problems(&dir).is_empty(), "{:?}", problems(&dir));

        // A failed result with a median.
        let mut with_median = samples::failed();
        with_median.median = Some(crate::run::result::Sample::median_of(&[samples::sample(
            1.0,
        )]));
        let dir = write_dir(tmp.path(), &[with_median]);
        assert!(!problems(&dir).is_empty());

        // A failed result claiming verified.
        let mut verified = samples::failed();
        verified.verification = samples::measured().verification;
        let dir = write_dir(tmp.path(), &[verified]);
        assert!(problems(&dir)
            .iter()
            .any(|m| m.contains("/verification/verified")));

        // A failed result with a failed verification may differ from the counts (an extra file).
        let mut extra = samples::failed();
        extra.verification = Some(crate::run::result::Verification {
            verified: false,
            files_checked: 10,
            files_ok: 10,
        });
        let dir = write_dir(tmp.path(), &[extra]);
        assert!(problems(&dir).is_empty(), "{:?}", problems(&dir));

        // A timed-out repeat without `failed`, with a median and verified: all three reported.
        let mut timed = samples::measured();
        let mut repeats = timed.repeats.clone().expect("repeats");
        repeats[0].compress.timed_out = true;
        timed.median = Some(crate::run::result::Sample::median_of(&repeats));
        timed.repeats = Some(repeats);
        let dir = write_dir(tmp.path(), &[timed]);
        let p = problems(&dir);
        for what in ["/repeats", "/verification/verified", "/median"] {
            assert!(p.iter().any(|m| m.contains(what)), "{what}: {p:?}");
        }

        // `failed` and `skipped` together, and a failed result with no measurement.
        let mut both = samples::failed();
        both.skipped = Some("not installed".into());
        let dir = write_dir(tmp.path(), &[both]);
        assert!(!problems(&dir).is_empty());
        let mut no_m = samples::failed();
        no_m.measurement = None;
        let dir = write_dir(tmp.path(), &[no_m]);
        assert!(!problems(&dir).is_empty());
    }

    #[test]
    fn an_aborted_or_trimmed_directory_does_not_validate() {
        let tmp = tempfile::tempdir().expect("tmp");
        let two = [samples::measured(), samples::skipped()];

        // Aborted: no run.json.
        let dir = write_dir(tmp.path(), &two);
        std::fs::remove_file(dir.join("run.json")).expect("rm");
        assert!(problems(&dir)
            .iter()
            .any(|m| m.starts_with("run.json:") && m.contains("missing")));

        // Trimmed: a listed combination lost its file.
        let dir = write_dir(tmp.path(), &two);
        std::fs::remove_file(dir.join(two[1].file_name())).expect("rm");
        assert!(problems(&dir)
            .iter()
            .any(|m| m.contains("has no result file")));

        // An unlisted result file.
        let dir = write_dir(tmp.path(), &two[..1]);
        std::fs::write(dir.join(two[1].file_name()), render(&two[1])).expect("w");
        assert!(problems(&dir).iter().any(|m| m.contains("is not listed")));

        // Outcome disagreement, thread and repeat-count disagreement.
        let dir = write_dir(tmp.path(), &two);
        let mut run = samples::run_file(&two);
        run.combinations[0].outcome = "failed".into();
        run.threads = 9;
        run.repeats_requested = 5;
        std::fs::write(dir.join("run.json"), render(&run)).expect("w");
        let p = problems(&dir);
        for what in ["is listed as failed", "run.json says 9", "run.json says 5"] {
            assert!(p.iter().any(|m| m.contains(what)), "{what}: {p:?}");
        }

        // complete: false is not accepted.
        let dir = write_dir(tmp.path(), &two);
        let mut v = to_value(&samples::run_file(&two));
        v["complete"] = json!(false);
        std::fs::write(dir.join("run.json"), v.to_string()).expect("w");
        assert!(!problems(&dir).is_empty());
    }

    #[test]
    fn fewer_repeats_than_requested_need_the_long_run_marker() {
        let tmp = tempfile::tempdir().expect("tmp");
        let one = |mut r: ToolResult| {
            let reps = r.repeats.take().expect("repeats")[..1].to_vec();
            r.median = Some(Sample::median_of(&reps));
            r.repeats = Some(reps);
            r
        };
        // One repeat of three, unexplained.
        let dir = write_dir(tmp.path(), &[one(samples::measured())]);
        assert!(problems(&dir)
            .iter()
            .any(|m| m.contains("/repeats:") && m.contains("repeats_short")));
        // Explained.
        let mut ok = one(samples::measured());
        ok.repeats_short = Some("measured once: long run".into());
        let dir = write_dir(tmp.path(), &[ok]);
        assert!(problems(&dir).is_empty(), "{:?}", problems(&dir));
        // A marker although everything ran.
        let mut lie = samples::measured();
        lie.repeats_short = Some("measured once: long run".into());
        let dir = write_dir(tmp.path(), &[lie]);
        assert!(problems(&dir).iter().any(|m| m.contains("/repeats_short")));
        // failed.repeat must follow the recorded repeats.
        let mut bad = samples::failed();
        bad.failed.as_mut().expect("failed").repeat = 3;
        let dir = write_dir(tmp.path(), &[bad]);
        assert!(problems(&dir).iter().any(|m| m.contains("/failed/repeat")));
    }

    #[test]
    fn a_dirty_or_unknown_build_needs_the_flag_in_host_json() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = write_dir(tmp.path(), &[samples::measured()]);
        for (commit, allowed, ok) in [
            ("0123456789ab-dirty", false, false),
            ("unknown", false, false),
            ("0123456789ab-dirty", true, true),
            ("0123456789ab", false, true),
        ] {
            let mut h = samples::host();
            h.git_commit = commit.into();
            h.dirty_build_allowed = allowed;
            std::fs::write(dir.join("host.json"), render(&h)).expect("host");
            let p = problems(&dir);
            assert_eq!(p.is_empty(), ok, "{commit} {allowed}: {p:?}");
            if !ok {
                assert!(p.iter().any(|m| m.starts_with("host.json: /git_commit")));
            }
        }
    }

    #[test]
    fn every_schema_property_has_a_description() {
        fn walk(v: &Value, at: &str, missing: &mut Vec<String>) {
            if let Some(props) = v.get("properties").and_then(Value::as_object) {
                for (k, p) in props {
                    if p.get("description")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        missing.push(format!("{at}/{k}"));
                    }
                }
            }
        }
        let schema = schema_value().expect("schema");
        let mut missing = Vec::new();
        for (name, def) in schema["$defs"].as_object().expect("defs") {
            walk(def, name, &mut missing);
        }
        assert!(
            missing.is_empty(),
            "properties without a description: {missing:?}"
        );
    }

    fn write_value(dir: &Path, name: &str, v: &Value) {
        std::fs::write(dir.join(name), v.to_string()).expect("write");
    }

    #[test]
    fn a_fabricated_median_is_caught() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = write_dir(tmp.path(), &[samples::measured()]);
        let mut v = to_value(&samples::measured());
        v["median"]["compress"]["wall_seconds"] = json!(0.01);
        v["median"]["archive_bytes"] = json!(1);
        write_value(&dir, "7z-mx5-text.json", &v);
        let p = problems(&dir);
        assert!(
            p.iter()
                .any(|m| m.contains("/median/compress/wall_seconds: is not the median")),
            "{p:?}"
        );
        assert!(p
            .iter()
            .any(|m| m.contains("/median/archive_bytes: is not the median")));
    }

    #[test]
    fn measured_results_need_a_found_tool_with_the_same_version() {
        let tmp = tempfile::tempdir().expect("tmp");
        // Tool marked skipped in tools.json (rar) but with measurements.
        let mut rar = samples::measured();
        rar.tool.id = "rar".into();
        let dir = write_dir(tmp.path(), &[rar]);
        assert!(problems(&dir)
            .iter()
            .any(|m| m.contains("marked skipped in tools.json")));
        // Different version.
        let mut other = samples::measured();
        other.tool.version = Some("25.01".into());
        let dir = write_dir(tmp.path(), &[other]);
        assert!(problems(&dir).iter().any(|m| m.contains("/tool/version")));
    }

    #[test]
    fn host_and_date_must_match_the_directory_name() {
        let tmp = tempfile::tempdir().expect("tmp");
        // host.json says a different host.
        let dir = write_dir(tmp.path(), &[]);
        let mut h = to_value(&samples::host());
        h["host"] = json!("otherbox");
        write_value(&dir, "host.json", &h);
        assert!(problems(&dir)
            .iter()
            .any(|m| m.starts_with("host.json: /host:")));
        // A repeat-run suffix is accepted, a suffix of 1 or a different word is not.
        for (name, ok) in [
            ("2026-10-01-testbox-2", true),
            ("2026-10-01-testbox-17", true),
            ("2026-10-01-testbox-1", false),
            ("2026-10-01-testbox-02", false),
            ("2026-10-01-testbox-x", false),
            ("2026-13-01-testbox", false),
            ("2026-02-30-testbox", false),
            ("1999-10-01-testbox", false),
        ] {
            let d = tmp.path().join(name);
            std::fs::create_dir_all(&d).expect("dir");
            std::fs::write(d.join("host.json"), render(&samples::host())).expect("host");
            std::fs::write(d.join("tools.json"), render(&samples::tools())).expect("tools");
            std::fs::write(d.join("run.json"), render(&samples::run_file(&[]))).expect("run");
            assert_eq!(problems(&d).is_empty(), ok, "{name}: {:?}", problems(&d));
        }
    }

    #[test]
    fn unexpected_files_are_reported_and_probes_are_accepted_by_name() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = write_dir(tmp.path(), &[samples::measured()]);
        std::fs::write(dir.join("probe-jpeg.json"), "not even json").expect("probe");
        std::fs::write(dir.join("notes.txt"), "x").expect("txt");
        std::fs::create_dir(dir.join("scratch")).expect("dir");
        let report = validate_dir(&dir).expect("validate");
        assert!(report
            .problems
            .iter()
            .any(|m| m.starts_with("notes.txt: unexpected file")));
        assert!(report
            .problems
            .iter()
            .any(|m| m.starts_with("scratch: unexpected directory")));
        assert!(!report.problems.iter().any(|m| m.contains("probe-jpeg")));
        assert!(report
            .notes
            .iter()
            .any(|n| n.contains("1 probe-*.json") && n.contains("not checked")));
    }

    #[test]
    fn one_profile_and_manifest_per_results_directory() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut a = samples::measured();
        a.class = "audio".into();
        a.corpus.manifest_blake3 = "cd".repeat(32);
        let mut b = samples::measured();
        b.class = "docs".into();
        b.corpus.profile = "full".into();
        let dir = write_dir(tmp.path(), &[samples::measured(), a, b]);
        let p = problems(&dir);
        assert!(
            p.iter()
                .any(|m| m.contains("/corpus/manifest_blake3: differs")),
            "{p:?}"
        );
        assert!(p.iter().any(|m| m.contains("/corpus/profile:")), "{p:?}");
    }

    #[test]
    fn tar_stream_results_record_their_tar_and_steps_that_add_up() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = write_dir(tmp.path(), &[samples::tar_stream()]);
        let p = problems(&dir);
        assert!(p.is_empty(), "{p:?}");

        // Tar not recorded.
        let mut no_tar = samples::tar_stream();
        no_tar.measurement.as_mut().expect("m").tar = None;
        let dir = write_dir(tmp.path(), &[no_tar]);
        assert!(problems(&dir)
            .iter()
            .any(|m| m.contains("/measurement/tar: missing")));

        // Steps that do not add up to the totals.
        let mut v = to_value(&samples::tar_stream());
        v["repeats"][0]["compress"]["wall_seconds"] = json!(99.0);
        write_value(&dir, "zstd-3-text.json", &v);
        // (the dir above holds a different file name; rewrite the matching one)
        let dir = write_dir(tmp.path(), &[]);
        write_value(&dir, "zstd-3-text.json", &v);
        let p = problems(&dir);
        assert!(
            p.iter().any(
                |m| m.contains("/repeats/0/compress/wall_seconds: is not tar_step + tool_step")
            ),
            "{p:?}"
        );

        // A directory-mode tool with a tar entry, or with steps.
        let mut dm = samples::measured();
        dm.measurement.as_mut().expect("m").tar = samples::tar_stream().measurement.expect("m").tar;
        let dir = write_dir(tmp.path(), &[dm]);
        assert!(problems(&dir)
            .iter()
            .any(|m| m.contains("/measurement/tar: present")));
    }

    #[test]
    fn a_parent_directory_checks_each_results_subdirectory() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join("README.md"), "x").expect("readme");
        std::fs::write(tmp.path().join("schema.json"), "{}").expect("schema");
        let good = write_dir(tmp.path(), &[samples::measured()]);
        let report = validate_dir(tmp.path()).expect("validate");
        assert_eq!(
            (report.results, report.problems.len()),
            (1, 0),
            "{:?}",
            report.problems
        );
        // Break one file: the problem names the subdirectory.
        std::fs::write(good.join("host.json"), "{}").expect("host");
        let p = problems(tmp.path());
        assert!(
            p.iter()
                .any(|m| m.starts_with("2026-10-01-testbox/host.json:")),
            "{p:?}"
        );
        // An empty parent is a problem, not a silent pass.
        let empty = tempfile::tempdir().expect("tmp");
        assert!(problems(empty.path())[0].contains("no results directories"));
    }

    #[test]
    fn directory_level_problems() {
        let tmp = tempfile::tempdir().expect("tmp");
        let bad_name = tmp.path().join("results");
        std::fs::create_dir_all(&bad_name).expect("dir");
        std::fs::write(bad_name.join("host.json"), "{}").expect("host");
        let p = problems(&bad_name);
        assert!(p.iter().any(|m| m.contains("directory name")));
        assert!(p.iter().any(|m| m.starts_with("host.json:")));
        assert!(p.iter().any(|m| m.starts_with("tools.json:")));
        assert!(problems(&tmp.path().join("missing"))[0].contains("not a directory"));
        assert!(dir_name_ok("2026-10-01-my-pc") && !dir_name_ok("2026-10-01-My_PC"));
    }
}
