//! `lpk-bench run --compare <dirA> <dirB> [--max-diff-pct N]`: how far two runs of the same
//! corpus on the same machine differ in time. This is the arithmetic behind the PLAN P0-3
//! acceptance clause "a second run differs by < 3% in time".
//!
//! For every tool x setting measured in both directories: the sum over classes of the median
//! compress wall time and of the median extract wall time in each directory, and the percentage
//! difference `|B - A| / A`. The comparison fails when a difference exceeds the threshold, and
//! when the directories do not measure the same thing: a different corpus, different tool
//! versions, a different thread count, different classes for a combination, or a combination
//! measured in only one of them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};

use super::result::{HostFile, RunFile, ToolResult, ToolsFile};

pub const DEFAULT_MAX_DIFF_PCT: f64 = 3.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub tool: String,
    pub setting: String,
    pub classes: usize,
    pub compress_a: f64,
    pub compress_b: f64,
    pub compress_pct: f64,
    pub extract_a: f64,
    pub extract_b: f64,
    pub extract_pct: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub rows: Vec<Row>,
    pub problems: Vec<String>,
}

#[derive(Default)]
struct Loaded {
    results: Vec<ToolResult>,
    versions: BTreeMap<String, Option<String>>,
    unreadable: Vec<String>,
    run: Option<RunFile>,
    host: Option<String>,
}

fn load(dir: &Path) -> Result<Loaded> {
    let mut out = Loaded::default();
    let run_path = dir.join("run.json");
    let text = std::fs::read_to_string(&run_path).with_context(|| {
        format!(
            "reading {} (a directory without run.json is an aborted run)",
            run_path.display()
        )
    })?;
    out.run = Some(
        serde_json::from_str(&text).with_context(|| format!("parsing {}", run_path.display()))?,
    );
    let host_path = dir.join("host.json");
    let text = std::fs::read_to_string(&host_path)
        .with_context(|| format!("reading {}", host_path.display()))?;
    let host: HostFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", host_path.display()))?;
    out.host = Some(host.host);
    let tools_path = dir.join("tools.json");
    let text = std::fs::read_to_string(&tools_path)
        .with_context(|| format!("reading {}", tools_path.display()))?;
    let tools: ToolsFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", tools_path.display()))?;
    for t in tools.tools {
        out.versions.insert(t.id, t.version);
    }
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json") && !n.starts_with("probe-"))
        .filter(|n| n != "host.json" && n != "tools.json" && n != "run.json")
        .collect();
    names.sort();
    for name in names {
        let text = std::fs::read_to_string(dir.join(&name))
            .with_context(|| format!("reading {name} in {}", dir.display()))?;
        match serde_json::from_str::<ToolResult>(&text) {
            Ok(r) => out.results.push(r),
            Err(e) => out.unreadable.push(format!("{name}: {e}")),
        }
    }
    Ok(out)
}

/// Per class: median compress and extract wall seconds; `failed` combinations listed apart.
#[derive(Default)]
struct Combos {
    measured: BTreeMap<(String, String), BTreeMap<String, (f64, f64)>>,
    failed: BTreeSet<(String, String)>,
}

fn combos(l: &Loaded) -> Combos {
    let mut c = Combos::default();
    for r in &l.results {
        let key = (r.tool.id.clone(), r.setting.id.clone());
        if r.failed.is_some() {
            c.failed.insert(key);
        } else if let (None, Some(m)) = (&r.skipped, &r.median) {
            c.measured.entry(key).or_default().insert(
                r.class.clone(),
                (m.compress.wall_seconds, m.extract.wall_seconds),
            );
        }
    }
    c
}

fn pct(a: f64, b: f64) -> f64 {
    if a == 0.0 {
        if b == 0.0 {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        (b - a).abs() / a * 100.0
    }
}

fn corpus_set(l: &Loaded) -> BTreeSet<(String, String, bool)> {
    l.results
        .iter()
        .map(|r| {
            (
                r.corpus.profile.clone(),
                r.corpus.manifest_blake3.clone(),
                r.private,
            )
        })
        .collect()
}

fn thread_set(l: &Loaded) -> BTreeSet<u32> {
    l.results
        .iter()
        .filter(|r| r.median.is_some())
        .map(|r| r.threads)
        .collect()
}

pub fn compare(a: &Path, b: &Path, max_diff_pct: f64) -> Result<Report> {
    let (la, lb) = (load(a)?, load(b)?);
    let mut rep = Report::default();
    for (dir, l) in [(a, &la), (b, &lb)] {
        for u in &l.unreadable {
            rep.problems.push(format!("{}: {u}", dir.display()));
        }
    }
    let (ca, cb) = (corpus_set(&la), corpus_set(&lb));
    if ca != cb {
        rep.problems.push(
            "the two directories were measured on different corpora (profile, manifest hash or \
             private flag differ)"
                .to_string(),
        );
    }
    if la.host != lb.host {
        rep.problems.push(format!(
            "different hosts: {:?} against {:?}",
            la.host.as_deref().unwrap_or("?"),
            lb.host.as_deref().unwrap_or("?")
        ));
    }
    if let (Some(ra), Some(rb)) = (&la.run, &lb.run) {
        if ra.repeats_requested != rb.repeats_requested {
            rep.problems.push(format!(
                "different requested repeats: {} against {}",
                ra.repeats_requested, rb.repeats_requested
            ));
        }
        if ra.long_run_s != rb.long_run_s {
            rep.problems.push(format!(
                "different --long-run-s: {} against {}",
                ra.long_run_s, rb.long_run_s
            ));
        }
        if ra.catalogue_blake3 != rb.catalogue_blake3 {
            rep.problems
                .push("the two runs used different catalogue files (bench/tools.toml)".to_string());
        }
    }
    let (ta, tb) = (thread_set(&la), thread_set(&lb));
    if ta != tb {
        rep.problems
            .push(format!("different thread counts: {ta:?} against {tb:?}"));
    }
    for (id, va) in &la.versions {
        if let Some(vb) = lb.versions.get(id) {
            if va.is_some() && vb.is_some() && va != vb {
                rep.problems.push(format!(
                    "tool `{id}` has different versions: {} against {}",
                    va.as_deref().unwrap_or("?"),
                    vb.as_deref().unwrap_or("?")
                ));
            }
        }
    }

    let (ma, mb) = (combos(&la), combos(&lb));
    let keys: BTreeSet<&(String, String)> = ma.measured.keys().chain(mb.measured.keys()).collect();
    for key in keys {
        let label = format!("{}/{}", key.0, key.1);
        match (ma.measured.get(key), mb.measured.get(key)) {
            (Some(x), Some(y)) => {
                if x.keys().ne(y.keys()) {
                    rep.problems.push(format!(
                        "{label}: the two directories measured different classes"
                    ));
                    continue;
                }
                let sum = |m: &BTreeMap<String, (f64, f64)>, f: fn(&(f64, f64)) -> f64| {
                    m.values().map(f).sum::<f64>()
                };
                let row = Row {
                    tool: key.0.clone(),
                    setting: key.1.clone(),
                    classes: x.len(),
                    compress_a: sum(x, |v| v.0),
                    compress_b: sum(y, |v| v.0),
                    compress_pct: 0.0,
                    extract_a: sum(x, |v| v.1),
                    extract_b: sum(y, |v| v.1),
                    extract_pct: 0.0,
                };
                let row = Row {
                    compress_pct: pct(row.compress_a, row.compress_b),
                    extract_pct: pct(row.extract_a, row.extract_b),
                    ..row
                };
                for (what, p) in [("compress", row.compress_pct), ("extract", row.extract_pct)] {
                    if p > max_diff_pct {
                        rep.problems.push(format!(
                            "{label}: {what} time differs by {p:.2}% (limit {max_diff_pct}%)"
                        ));
                    }
                }
                rep.rows.push(row);
            }
            (Some(_), None) | (None, Some(_)) => {
                let (here, there, failed) = if ma.measured.contains_key(key) {
                    ("A", "B", &mb.failed)
                } else {
                    ("B", "A", &ma.failed)
                };
                rep.problems.push(format!(
                    "{label}: measured only in {here}; in {there} it {}",
                    if failed.contains(key) {
                        "failed"
                    } else {
                        "was not measured"
                    }
                ));
            }
            (None, None) => {}
        }
    }
    for key in ma.failed.intersection(&mb.failed) {
        rep.problems.push(format!(
            "{}/{}: failed in both directories (it cannot be compared)",
            key.0, key.1
        ));
    }
    if rep.rows.is_empty() {
        rep.problems
            .push("no tool x setting was measured in both directories".to_string());
    }
    Ok(rep)
}

/// The table printed by `--compare`.
pub fn table(rep: &Report, max_diff_pct: f64) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "{:<22} {:>3} {:>12} {:>12} {:>8} {:>12} {:>12} {:>8}\n",
        "tool/setting",
        "cls",
        "compress A s",
        "compress B s",
        "diff %",
        "extract A s",
        "extract B s",
        "diff %"
    ));
    for r in &rep.rows {
        let flag = if r.compress_pct > max_diff_pct || r.extract_pct > max_diff_pct {
            "  <-- over the limit"
        } else {
            ""
        };
        s.push_str(&format!(
            "{:<22} {:>3} {:>12.3} {:>12.3} {:>8.2} {:>12.3} {:>12.3} {:>8.2}{flag}\n",
            format!("{}/{}", r.tool, r.setting),
            r.classes,
            r.compress_a,
            r.compress_b,
            r.compress_pct,
            r.extract_a,
            r.extract_b,
            r.extract_pct
        ));
    }
    s
}

pub fn compare_command(a: &Path, b: &Path, max_diff_pct: f64) -> Result<ExitCode> {
    let rep = compare(a, b, max_diff_pct)?;
    print!("{}", table(&rep, max_diff_pct));
    for p in &rep.problems {
        eprintln!("error: {p}");
    }
    if rep.problems.is_empty() {
        println!(
            "{} combination(s) compared; every difference is within {max_diff_pct}%",
            rep.rows.len()
        );
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{} problem(s)", rep.problems.len());
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::result::{render, samples, Sample};

    /// Scale every wall time of a sample result by `f` and recompute its median.
    fn scaled(mut r: ToolResult, f: f64) -> ToolResult {
        let mut reps = r.repeats.clone().expect("repeats");
        for s in &mut reps {
            s.compress.wall_seconds *= f;
            s.extract.wall_seconds *= f;
        }
        r.median = Some(Sample::median_of(&reps));
        r.repeats = Some(reps);
        r
    }

    fn write(
        root: &Path,
        name: &str,
        results: &[ToolResult],
        tools: &ToolsFile,
    ) -> std::path::PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("host.json"), render(&samples::host())).expect("host");
        std::fs::write(dir.join("tools.json"), render(tools)).expect("tools");
        std::fs::write(dir.join("run.json"), render(&samples::run_file(results))).expect("run");
        for r in results {
            std::fs::write(dir.join(r.file_name()), render(r)).expect("result");
        }
        dir
    }

    fn two_classes(f: f64) -> Vec<ToolResult> {
        let a = scaled(samples::measured(), f);
        let mut b = scaled(samples::measured(), f * 2.0);
        b.class = "logs".into();
        vec![a, b]
    }

    #[test]
    fn identical_runs_compare_clean_and_sums_are_over_classes() {
        let tmp = tempfile::tempdir().expect("tmp");
        let a = write(tmp.path(), "a", &two_classes(1.0), &samples::tools());
        let b = write(tmp.path(), "b", &two_classes(1.0), &samples::tools());
        let rep = compare(&a, &b, 3.0).expect("compare");
        assert!(rep.problems.is_empty(), "{:?}", rep.problems);
        assert_eq!(rep.rows.len(), 1);
        let row = &rep.rows[0];
        assert_eq!(row.classes, 2);
        // Median compress of the sample is 1.5 s; classes scale by 1 and 2.
        assert!((row.compress_a - 4.5).abs() < 1e-9, "{row:?}");
        assert_eq!(row.compress_pct, 0.0);
        assert!(table(&rep, 3.0).contains("7z/mx5"));
    }

    #[test]
    fn a_difference_within_the_limit_passes_and_beyond_it_fails() {
        let tmp = tempfile::tempdir().expect("tmp");
        let a = write(tmp.path(), "a", &two_classes(1.0), &samples::tools());
        let within = write(tmp.path(), "w", &two_classes(1.02), &samples::tools());
        let beyond = write(tmp.path(), "x", &two_classes(1.05), &samples::tools());
        let ok = compare(&a, &within, 3.0).expect("compare");
        assert!(ok.problems.is_empty(), "{:?}", ok.problems);
        assert!((ok.rows[0].compress_pct - 2.0).abs() < 1e-6);
        let bad = compare(&a, &beyond, 3.0).expect("compare");
        assert!(
            bad.problems
                .iter()
                .any(|p| p.contains("compress time differs by 5.00%")),
            "{:?}",
            bad.problems
        );
        assert!(bad
            .problems
            .iter()
            .any(|p| p.contains("extract time differs")));
        assert!(table(&bad, 3.0).contains("over the limit"));
        // Faster counts too, and the threshold is a parameter.
        let faster = compare(&beyond, &a, 3.0).expect("compare");
        assert!(!faster.problems.is_empty());
        assert!(compare(&a, &beyond, 6.0)
            .expect("compare")
            .problems
            .is_empty());
    }

    #[test]
    fn different_corpus_threads_or_tool_versions_are_problems() {
        let tmp = tempfile::tempdir().expect("tmp");
        let a = write(tmp.path(), "a", &two_classes(1.0), &samples::tools());

        let mut other_corpus = two_classes(1.0);
        other_corpus[0].corpus.manifest_blake3 = "cd".repeat(32);
        let b = write(tmp.path(), "b", &other_corpus, &samples::tools());
        let rep = compare(&a, &b, 3.0).expect("compare");
        assert!(
            rep.problems.iter().any(|p| p.contains("different corpora")),
            "{:?}",
            rep.problems
        );

        let mut threads = two_classes(1.0);
        for r in &mut threads {
            r.threads = 8;
        }
        let c = write(tmp.path(), "c", &threads, &samples::tools());
        let rep = compare(&a, &c, 3.0).expect("compare");
        assert!(
            rep.problems.iter().any(|p| p.contains("thread counts")),
            "{:?}",
            rep.problems
        );

        let mut tools = samples::tools();
        tools.tools[0].version = Some("26.04".into());
        let d = write(tmp.path(), "d", &two_classes(1.0), &tools);
        let rep = compare(&a, &d, 3.0).expect("compare");
        assert!(
            rep.problems
                .iter()
                .any(|p| p.contains("`7z` has different versions")),
            "{:?}",
            rep.problems
        );
    }

    #[test]
    fn different_host_repeats_long_run_or_catalogue_are_problems_and_double_failures_are_listed() {
        let tmp = tempfile::tempdir().expect("tmp");
        let results = two_classes(1.0);
        let a = write(tmp.path(), "a", &results, &samples::tools());
        let b = write(tmp.path(), "b", &results, &samples::tools());
        let mut run = samples::run_file(&results);
        run.repeats_requested = 5;
        run.long_run_s = 7;
        run.catalogue_blake3 = "00".repeat(32);
        std::fs::write(b.join("run.json"), render(&run)).expect("w");
        let mut host = samples::host();
        host.host = "otherbox".into();
        std::fs::write(b.join("host.json"), render(&host)).expect("w");
        let rep = compare(&a, &b, 3.0).expect("compare");
        for what in [
            "different hosts",
            "requested repeats",
            "--long-run-s",
            "catalogue files",
        ] {
            assert!(
                rep.problems.iter().any(|p| p.contains(what)),
                "{what}: {:?}",
                rep.problems
            );
        }
        let f1 = write(tmp.path(), "f1", &[samples::failed()], &samples::tools());
        let f2 = write(tmp.path(), "f2", &[samples::failed()], &samples::tools());
        let rep = compare(&f1, &f2, 3.0).expect("compare");
        assert!(
            rep.problems
                .iter()
                .any(|p| p.contains("failed in both directories")),
            "{:?}",
            rep.problems
        );
        // No run.json: an error, not a comparison.
        std::fs::remove_file(f2.join("run.json")).expect("rm");
        assert!(compare(&f1, &f2, 3.0).is_err());
    }

    #[test]
    fn a_failed_or_missing_combination_in_one_directory_is_a_problem() {
        let tmp = tempfile::tempdir().expect("tmp");
        let a = write(tmp.path(), "a", &[samples::measured()], &samples::tools());
        let failed = write(tmp.path(), "f", &[samples::failed()], &samples::tools());
        let rep = compare(&a, &failed, 3.0).expect("compare");
        assert!(
            rep.problems
                .iter()
                .any(|p| p.contains("measured only in A") && p.contains("failed")),
            "{:?}",
            rep.problems
        );
        assert!(rep.problems.iter().any(|p| p.contains("no tool x setting")));
        let fewer = write(tmp.path(), "g", &two_classes(1.0), &samples::tools());
        let rep = compare(&a, &fewer, 3.0).expect("compare");
        assert!(
            rep.problems.iter().any(|p| p.contains("different classes")),
            "{:?}",
            rep.problems
        );
    }
}
