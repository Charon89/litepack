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

/// The committed schema, embedded so the binary and the file cannot disagree.
pub const SCHEMA_TEXT: &str = include_str!("../../../../bench/results/schema.json");

/// Which `$defs` entry a file is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Result,
    Tools,
    Host,
}

impl Kind {
    fn def(self) -> &'static str {
        match self {
            Kind::Result => "result",
            Kind::Tools => "tools",
            Kind::Host => "host",
        }
    }
}

pub struct Schemas {
    result: Validator,
    tools: Validator,
    host: Validator,
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
        })
    }

    fn validator(&self, kind: Kind) -> &Validator {
        match kind {
            Kind::Result => &self.result,
            Kind::Tools => &self.tools,
            Kind::Host => &self.host,
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

fn dir_name_ok(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() > 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'-'
        && b[11..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
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
            .problems
            .extend(r.problems.into_iter().map(|p| format!("{label}/{p}")));
    }
    Ok(total)
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
    if let Some(name) = abs.file_name().and_then(|n| n.to_str()) {
        if !dir_name_ok(name) {
            report.problems.push(format!(
                "{name}: directory name must be <YYYY-MM-DD>-<host> with host in [a-z0-9-]"
            ));
        }
    }

    let mut tools_found: Vec<String> = Vec::new();
    for (name, kind) in [("host.json", Kind::Host), ("tools.json", Kind::Tools)] {
        let path = dir.join(name);
        if !path.is_file() {
            report
                .problems
                .push(format!("{name}: (root): file is missing"));
            continue;
        }
        if let Some(v) = read_json(&path, name, &mut report.problems) {
            report.problems.extend(schemas.check_value(kind, name, &v));
            if kind == Kind::Tools {
                tools_found = v
                    .get("tools")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.get("id").and_then(Value::as_str).map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
    }

    let mut names: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            n.ends_with(".json")
                && n != "host.json"
                && n != "tools.json"
                && !n.starts_with("probe-")
        })
        .collect();
    names.sort();
    for name in names {
        report.results += 1;
        let Some(v) = read_json(&dir.join(&name), &name, &mut report.problems) else {
            continue;
        };
        let schema_problems = schemas.check_value(Kind::Result, &name, &v);
        let clean = schema_problems.is_empty();
        report.problems.extend(schema_problems);
        if clean {
            semantic_checks(&name, &v, &tools_found, &mut report.problems);
        }
    }
    Ok(report)
}

fn semantic_checks(name: &str, v: &Value, tools: &[String], out: &mut Vec<String>) {
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or("");
    let expected = format!(
        "{}-{}-{}.json",
        s("/tool/id"),
        s("/setting/id"),
        s("/class")
    );
    if name != expected {
        out.push(format!(
            "{name}: (file name): must be `{expected}` for the tool, setting and class inside"
        ));
    }
    if !tools.iter().any(|t| t == s("/tool/id")) {
        out.push(format!(
            "{name}: /tool/id: `{}` does not appear in tools.json",
            s("/tool/id")
        ));
    }
    if let Some(ver) = v.get("verification") {
        let n = |k: &str| ver.get(k).and_then(Value::as_u64).unwrap_or(0);
        let verified = ver
            .get("verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let all = n("files_ok") == n("files_checked")
            && v.pointer("/corpus/files").and_then(Value::as_u64) == Some(n("files_checked"));
        if n("files_ok") > n("files_checked") {
            out.push(format!(
                "{name}: /verification/files_ok: exceeds files_checked"
            ));
        } else if verified != all {
            out.push(format!(
                "{name}: /verification/verified: disagrees with the counts \
                 (verified means files_ok == files_checked == corpus.files)"
            ));
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
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("host.json"), render(&samples::host())).expect("host");
        std::fs::write(dir.join("tools.json"), render(&samples::tools())).expect("tools");
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
        let host = to_value(&crate::run::host::collect());
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
        let (m, s) = (
            to_value(&samples::measured()),
            to_value(&samples::skipped()),
        );
        assert_agree(&schema, "result", &[&m, &s]);
        assert_agree(&schema, "tool_ref", &[&m["tool"], &s["tool"]]);
        assert_agree(&schema, "setting_ref", &[&m["setting"]]);
        assert_agree(&schema, "corpus_ref", &[&m["corpus"]]);
        assert_agree(&schema, "sample", &[&m["median"], &m["repeats"][0]]);
        assert_agree(
            &schema,
            "measure",
            &[&m["median"]["compress"], &m["median"]["extract"]],
        );
        assert_agree(&schema, "verification", &[&m["verification"]]);
        let t = to_value(&samples::tools());
        assert_agree(&schema, "tools", &[&t]);
        assert_agree(&schema, "tool_entry", &[&t["tools"][0], &t["tools"][1]]);
        assert_agree(&schema, "host", &[&to_value(&samples::host())]);
        // Every def with properties is covered above (a new def must be added to this test).
        let covered = [
            "result",
            "tool_ref",
            "setting_ref",
            "corpus_ref",
            "sample",
            "measure",
            "verification",
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
