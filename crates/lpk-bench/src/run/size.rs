//! `lpk-bench run --compare-sizes <GOLDEN> <NEW> [--max-growth-pct 1]`: the size-regression
//! gate (PLAN E8-2). Archive bytes are deterministic (D-60), so unlike time this comparison is
//! meaningful on shared CI runners.
//!
//! `GOLDEN` is a results directory or a pointer file (`bench/ci-golden.toml`) whose `results`
//! key names one, relative to the working directory (the repository root). For every row
//! (tool x setting x class) of the golden directory the new directory must have the same row
//! with a median archive no more than `--max-growth-pct` percent larger. Every row's change is
//! printed. The gate fails on growth beyond the limit, on a row missing on either side, on a row
//! that failed or was skipped, and when the two directories measured different corpora (the
//! manifest BLAKE3 differs: a changed fixture set needs new goldens). A shrink beyond the limit
//! is not a failure: it prints a notice that the goldens may be refreshed.
//!
//! # Refreshing the goldens
//!
//! Run the harness on the `ci` profile from a clean release build (the CI job's commands:
//! `lpk-bench corpus build --profile ci`, then
//! `lpk-bench run --tools lpk --settings fast,balanced,store --profile ci --repeats 1 --threads 2`),
//! commit the new `bench/results/<dir>` and point `bench/ci-golden.toml` at it in the same
//! commit, whose message cites the directory. Sizes are never typed by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::result::ToolResult;

pub const DEFAULT_MAX_GROWTH_PCT: f64 = 1.0;

/// `bench/ci-golden.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    /// Results directory of the golden run, relative to the working directory.
    results: PathBuf,
}

/// The golden directory: `p` itself, or the directory a pointer file names.
pub fn resolve_golden(p: &Path) -> Result<PathBuf> {
    if p.is_file() {
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let ptr: Pointer =
            toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        Ok(ptr.results)
    } else {
        Ok(p.to_path_buf())
    }
}

/// One row's state in a directory.
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Bytes(u64),
    Unusable(String),
}

#[derive(Debug, Default)]
struct Rows {
    cells: BTreeMap<(String, String, String), Cell>,
    manifests: BTreeSet<String>,
    threads: BTreeSet<u32>,
}

fn load(dir: &Path) -> Result<Rows> {
    if !dir.join("run.json").is_file() {
        bail!(
            "{} has no run.json (not a results directory, or an aborted run)",
            dir.display()
        );
    }
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json") && !n.starts_with("probe-"))
        .filter(|n| n != "host.json" && n != "tools.json" && n != "run.json")
        .collect();
    names.sort();
    let mut rows = Rows::default();
    for name in names {
        let text = std::fs::read_to_string(dir.join(&name))
            .with_context(|| format!("reading {name} in {}", dir.display()))?;
        let r: ToolResult = serde_json::from_str(&text)
            .with_context(|| format!("parsing {name} in {}", dir.display()))?;
        let key = (r.tool.id.clone(), r.setting.id.clone(), r.class.clone());
        let cell = if let Some(f) = &r.failed {
            Cell::Unusable(format!("failed ({}: {})", f.step, f.reason))
        } else if let Some(s) = &r.skipped {
            Cell::Unusable(format!("skipped ({s})"))
        } else if let Some(m) = &r.median {
            Cell::Bytes(m.archive_bytes)
        } else {
            Cell::Unusable("no median".into())
        };
        rows.manifests.insert(r.corpus.manifest_blake3.clone());
        rows.threads.insert(r.threads);
        rows.cells.insert(key, cell);
    }
    Ok(rows)
}

/// One compared row.
#[derive(Debug, Clone, PartialEq)]
pub struct SizeRow {
    pub tool: String,
    pub setting: String,
    pub class: String,
    pub golden: u64,
    pub new: u64,
    /// `(new - golden) / golden`, in percent.
    pub pct: f64,
}

#[derive(Debug, Default)]
pub struct SizeReport {
    pub rows: Vec<SizeRow>,
    pub problems: Vec<String>,
    pub notes: Vec<String>,
}

fn growth_pct(golden: u64, new: u64) -> f64 {
    if golden == 0 {
        if new == 0 {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        (new as f64 - golden as f64) / golden as f64 * 100.0
    }
}

/// Compare the rows of `golden` and `new` (both results directories).
pub fn compare_sizes(golden: &Path, new: &Path, max_growth_pct: f64) -> Result<SizeReport> {
    let g = load(golden)?;
    let n = load(new)?;
    let mut rep = SizeReport::default();
    if g.cells.is_empty() {
        rep.problems
            .push(format!("{} holds no result rows", golden.display()));
    }
    if g.manifests != n.manifests {
        rep.problems.push(format!(
            "the two directories measured different corpora (manifest BLAKE3 {:?} against {:?}): \
             a changed fixture set needs new goldens",
            g.manifests, n.manifests
        ));
    }
    if g.threads != n.threads {
        rep.notes.push(format!(
            "thread counts differ ({:?} against {:?})",
            g.threads, n.threads
        ));
    }
    let keys: BTreeSet<&(String, String, String)> = g.cells.keys().chain(n.cells.keys()).collect();
    for key in keys {
        let name = format!("{}/{}/{}", key.0, key.1, key.2);
        match (g.cells.get(key), n.cells.get(key)) {
            (Some(Cell::Bytes(a)), Some(Cell::Bytes(b))) => {
                let pct = growth_pct(*a, *b);
                if pct > max_growth_pct {
                    rep.problems.push(format!(
                        "{name}: {a} -> {b} bytes ({pct:+.3}%), over the {max_growth_pct}% limit"
                    ));
                } else if pct < -max_growth_pct {
                    rep.notes.push(format!(
                        "{name}: {a} -> {b} bytes ({pct:+.3}%): smaller by more than \
                         {max_growth_pct}%, the goldens may be refreshed"
                    ));
                }
                rep.rows.push(SizeRow {
                    tool: key.0.clone(),
                    setting: key.1.clone(),
                    class: key.2.clone(),
                    golden: *a,
                    new: *b,
                    pct,
                });
            }
            (None, Some(_)) => rep
                .problems
                .push(format!("{name}: not in the golden directory")),
            (Some(_), None) => rep
                .problems
                .push(format!("{name}: missing from the new directory")),
            (Some(Cell::Unusable(why)), _) => rep
                .problems
                .push(format!("{name}: unusable in the golden directory: {why}")),
            (_, Some(Cell::Unusable(why))) => rep
                .problems
                .push(format!("{name}: unusable in the new directory: {why}")),
            (None, None) => {}
        }
    }
    Ok(rep)
}

/// The table printed by `--compare-sizes`.
pub fn table(rep: &SizeReport, max_growth_pct: f64) -> String {
    let mut s = format!(
        "{:<34} {:>14} {:>14} {:>9}\n",
        "tool/setting/class", "golden bytes", "new bytes", "change %"
    );
    for r in &rep.rows {
        let flag = if r.pct > max_growth_pct {
            "  <-- grew over the limit"
        } else if r.pct < -max_growth_pct {
            "  (smaller: refresh?)"
        } else {
            ""
        };
        s.push_str(&format!(
            "{:<34} {:>14} {:>14} {:>+9.3}{flag}\n",
            format!("{}/{}/{}", r.tool, r.setting, r.class),
            r.golden,
            r.new,
            r.pct
        ));
    }
    s
}

/// Refuse a growth limit that is not a finite number of at least 0 (NaN would pass every row, a
/// negative limit fail every row).
pub fn check_limit(max_growth_pct: f64) -> Result<()> {
    if !max_growth_pct.is_finite() || max_growth_pct < 0.0 {
        bail!("--max-growth-pct must be a finite number of at least 0, not {max_growth_pct}");
    }
    Ok(())
}

/// The gate: both directories validated (`run --validate`'s checks; any problem fails the gate,
/// so a hand-edited golden cannot pass), then [`compare_sizes`].
pub fn gate(golden: &Path, new: &Path, max_growth_pct: f64) -> Result<SizeReport> {
    check_limit(max_growth_pct)?;
    let mut problems = Vec::new();
    for (label, dir) in [("golden", golden), ("new", new)] {
        let v = super::validate::validate_dir(dir)?;
        problems.extend(
            v.problems
                .into_iter()
                .map(|p| format!("{label} directory does not validate: {p}")),
        );
    }
    let mut rep = compare_sizes(golden, new, max_growth_pct)?;
    problems.append(&mut rep.problems);
    rep.problems = problems;
    Ok(rep)
}

pub fn compare_sizes_command(golden: &Path, new: &Path, max_growth_pct: f64) -> Result<ExitCode> {
    check_limit(max_growth_pct)?;
    let dir = resolve_golden(golden)?;
    println!("golden: {}; new: {}", dir.display(), new.display());
    let rep = gate(&dir, new, max_growth_pct)?;
    print!("{}", table(&rep, max_growth_pct));
    for n in &rep.notes {
        println!("notice: {n}");
    }
    for p in &rep.problems {
        eprintln!("error: {p}");
    }
    if rep.problems.is_empty() {
        println!(
            "{} row(s) compared; no archive grew by more than {max_growth_pct}%",
            rep.rows.len()
        );
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{} problem(s): the size gate fails", rep.problems.len());
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::result::{render, samples};

    fn row(setting: &str, class: &str, bytes: u64) -> ToolResult {
        let mut r = samples::measured();
        r.tool.id = "lpk".into();
        r.setting.id = setting.into();
        r.class = class.into();
        if let Some(m) = r.median.as_mut() {
            m.archive_bytes = bytes;
        }
        r
    }

    fn write(root: &Path, name: &str, results: &[ToolResult]) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("host.json"), render(&samples::host())).expect("host");
        std::fs::write(dir.join("tools.json"), render(&samples::tools())).expect("tools");
        std::fs::write(dir.join("run.json"), render(&samples::run_file(results))).expect("run");
        for r in results {
            std::fs::write(dir.join(r.file_name()), render(r)).expect("result");
        }
        dir
    }

    fn golden() -> Vec<ToolResult> {
        vec![
            row("fast", "text", 10_000),
            row("balanced", "text", 8_000),
            row("store", "text", 50_000),
        ]
    }

    #[test]
    fn equal_sizes_pass_and_every_row_is_printed() {
        let t = tempfile::tempdir().expect("tmp");
        let a = write(t.path(), "a", &golden());
        let b = write(t.path(), "b", &golden());
        let rep = compare_sizes(&a, &b, 1.0).expect("compare");
        assert!(rep.problems.is_empty(), "{:?}", rep.problems);
        assert_eq!(rep.rows.len(), 3);
        let text = table(&rep, 1.0);
        for s in ["lpk/fast/text", "lpk/balanced/text", "lpk/store/text"] {
            assert!(text.contains(s), "{text}");
        }
    }

    #[test]
    fn growth_within_the_limit_passes_and_beyond_it_fails() {
        let t = tempfile::tempdir().expect("tmp");
        let a = write(t.path(), "a", &golden());
        let mut within = golden();
        within[0] = row("fast", "text", 10_100); // +1.0%: at the limit, passes
        let b = write(t.path(), "b", &within);
        let rep = compare_sizes(&a, &b, 1.0).expect("compare");
        assert!(rep.problems.is_empty(), "{:?}", rep.problems);

        let mut over = golden();
        over[1] = row("balanced", "text", 8_081); // +1.0125%
        let c = write(t.path(), "c", &over);
        let rep = compare_sizes(&a, &c, 1.0).expect("compare");
        assert_eq!(rep.problems.len(), 1, "{:?}", rep.problems);
        assert!(rep.problems[0].contains("lpk/balanced/text"));
        assert!(table(&rep, 1.0).contains("grew over the limit"));
    }

    #[test]
    fn a_shrink_beyond_the_limit_is_a_notice_not_a_failure() {
        let t = tempfile::tempdir().expect("tmp");
        let a = write(t.path(), "a", &golden());
        let mut smaller = golden();
        smaller[1] = row("balanced", "text", 7_000);
        let b = write(t.path(), "b", &smaller);
        let rep = compare_sizes(&a, &b, 1.0).expect("compare");
        assert!(rep.problems.is_empty(), "{:?}", rep.problems);
        assert_eq!(rep.notes.len(), 1);
        assert!(rep.notes[0].contains("may be refreshed"));
    }

    #[test]
    fn rows_missing_on_either_side_or_failed_fail() {
        let t = tempfile::tempdir().expect("tmp");
        let a = write(t.path(), "a", &golden());
        let b = write(t.path(), "b", &golden()[..2]);
        let rep = compare_sizes(&a, &b, 1.0).expect("compare");
        assert!(rep
            .problems
            .iter()
            .any(|p| p.contains("lpk/store/text") && p.contains("missing from the new")));

        let mut extra = golden();
        extra.push(row("fast", "logs", 1));
        let c = write(t.path(), "c", &extra);
        let rep = compare_sizes(&a, &c, 1.0).expect("compare");
        assert!(rep
            .problems
            .iter()
            .any(|p| p.contains("lpk/fast/logs") && p.contains("not in the golden")));

        let mut failed = golden();
        failed[0] = samples::failed();
        failed[0].tool.id = "lpk".into();
        failed[0].setting.id = "fast".into();
        failed[0].class = "text".into();
        let d = write(t.path(), "d", &failed);
        let rep = compare_sizes(&a, &d, 1.0).expect("compare");
        assert!(rep
            .problems
            .iter()
            .any(|p| p.contains("lpk/fast/text") && p.contains("unusable in the new")));
    }

    #[test]
    fn a_different_corpus_fails_and_the_pointer_file_is_followed() {
        let t = tempfile::tempdir().expect("tmp");
        let a = write(t.path(), "a", &golden());
        let mut other = golden();
        for r in &mut other {
            r.corpus.manifest_blake3 = "cd".repeat(32);
        }
        let b = write(t.path(), "b", &other);
        let rep = compare_sizes(&a, &b, 1.0).expect("compare");
        assert!(rep.problems.iter().any(|p| p.contains("different corpora")));

        let ptr = t.path().join("golden.toml");
        std::fs::write(
            &ptr,
            format!(
                "# comment\nresults = {:?}\n",
                a.to_string_lossy().replace('\\', "/")
            ),
        )
        .expect("ptr");
        assert_eq!(
            resolve_golden(&ptr).expect("resolve"),
            PathBuf::from(a.to_string_lossy().replace('\\', "/"))
        );
        assert_eq!(resolve_golden(&a).expect("dir"), a);
        std::fs::write(&ptr, "dir = \"x\"\n").expect("bad");
        assert!(resolve_golden(&ptr).is_err());
    }

    #[test]
    fn the_committed_pointer_names_a_results_directory_of_the_ci_profile() {
        // Tests run in the crate directory; the pointer is relative to the repository root.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = resolve_golden(&root.join("bench/ci-golden.toml")).expect("pointer");
        let dir = root.join(dir);
        let rows = load(&dir).expect("golden rows");
        assert!(!rows.cells.is_empty());
        let settings: BTreeSet<&str> = rows.cells.keys().map(|k| k.1.as_str()).collect();
        assert_eq!(settings, BTreeSet::from(["balanced", "fast", "store"]));
        assert!(rows.cells.values().all(|c| matches!(c, Cell::Bytes(_))));
        let rep = gate(&dir, &dir, DEFAULT_MAX_GROWTH_PCT).expect("self");
        assert!(rep.problems.is_empty(), "{:?}", rep.problems);
        // The goldens belong to the fixture set the generator writes today: a generator change
        // without refreshed goldens fails here, not only on CI.
        let blake3: BTreeSet<&str> = rows.manifests.iter().map(String::as_str).collect();
        assert_eq!(
            blake3,
            BTreeSet::from([crate::corpus::fixtures::tests::MANIFEST_BLAKE3]),
            "the goldens were measured on another ci fixture set: refresh them"
        );
        for name in std::fs::read_dir(&dir).expect("dir").filter_map(|e| e.ok()) {
            let n = name.file_name().to_string_lossy().into_owned();
            if n.starts_with("lpk-") {
                let r: ToolResult =
                    serde_json::from_str(&std::fs::read_to_string(name.path()).expect("read"))
                        .expect("parse");
                assert_eq!(r.corpus.profile, "ci", "{n}");
            }
        }
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("mk");
        for e in std::fs::read_dir(from)
            .expect("read")
            .filter_map(|e| e.ok())
        {
            std::fs::copy(e.path(), to.join(e.file_name())).expect("copy");
        }
    }

    #[test]
    fn a_hand_edited_golden_fails_the_gate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let golden = root.join(resolve_golden(&root.join("bench/ci-golden.toml")).expect("ptr"));
        let t = tempfile::tempdir().expect("tmp");
        let name = golden.file_name().expect("name");
        let tampered = t.path().join("a").join(name);
        copy_dir(&golden, &tampered);
        let file = tampered.join("lpk-balanced-text-prose.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("json");
        v["median"]["archive_bytes"] = serde_json::json!(1_000_000_000u64);
        std::fs::write(&file, serde_json::to_string_pretty(&v).expect("ser")).expect("write");
        // Compared alone, the raised golden is only a "smaller" notice...
        let plain = compare_sizes(&tampered, &golden, 1.0).expect("compare");
        assert!(plain.problems.is_empty(), "{:?}", plain.problems);
        // ...the gate validates both directories first and fails.
        let rep = gate(&tampered, &golden, 1.0).expect("gate");
        assert!(
            rep.problems
                .iter()
                .any(|p| p.starts_with("golden directory does not validate")
                    && p.contains("archive_bytes")),
            "{:?}",
            rep.problems
        );
    }

    #[test]
    fn the_growth_limit_must_be_finite_and_not_negative() {
        for bad in [f64::NAN, f64::INFINITY, -0.5] {
            assert!(check_limit(bad).is_err(), "{bad}");
        }
        for ok in [0.0, 1.0, 12.5] {
            check_limit(ok).expect("ok");
        }
        let t = tempfile::tempdir().expect("tmp");
        let a = write(t.path(), "a", &golden());
        assert!(gate(&a, &a, f64::NAN).is_err());
    }
}
