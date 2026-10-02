//! End-to-end tests of `lpk-bench run` (PLAN P0-3): the real binary drives a fake archiver
//! (`lpk-fake-archiver`) and the tar that ships with Windows and Linux over a tiny corpus built
//! in a temporary directory. No external archiver is needed.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const FAKE: &str = env!("CARGO_BIN_EXE_lpk-fake-archiver");

const CATALOGUE: &str = r#"
[[tool]]
id = "store"
name = "tar (store)"
exe = { windows = ["tar"], linux = ["tar"] }
hints = { windows = ["%SystemRoot%/System32/tar.exe"] }
hints_first = true
version = { args = ["--version"], pattern = '(bsdtar \d+(?:\.\d+)+|GNU tar\)? \d+(?:\.\d+)+)' }
extension = ".tar"
mode = "directory"
create = ["-cf", "{archive}", "-C", "{input}", "."]
create_list = ["-cf", "{archive}", "-T", "{list}"]
extract = ["-xf", "{archive}", "-C", "{outdir}"]
licence = "test"
[[tool.setting]]
id = "store"

[[tool]]
id = "fake"
name = "fake archiver"
version = { args = ["version"], pattern = 'fake-archiver (\d+\.\d+)' }
extension = ".fk"
mode = "directory"
create = ["create", "{settings}", "{archive}", "{input}"]
extract = ["extract", "{settings}", "{archive}", "{outdir}"]
licence = "test"
[[tool.setting]]
id = "ok"
[[tool.setting]]
id = "noarchive"
compress = ["--mode=noarchive"]
[[tool.setting]]
id = "empty"
compress = ["--mode=empty"]
[[tool.setting]]
id = "exit1"
compress = ["--mode=exit1"]
[[tool.setting]]
id = "sleep"
compress = ["--mode=sleep"]
[[tool.setting]]
id = "miss"
extract = ["--mode=miss"]
[[tool.setting]]
id = "alter"
extract = ["--mode=alter"]
[[tool.setting]]
id = "extra"
extract = ["--mode=extra"]
[[tool.setting]]
id = "exit3"
extract = ["--mode=exit3"]
[[tool.setting]]
id = "orphan"
compress = ["--mode=orphan"]

[[tool]]
id = "fake-env"
name = "fake archiver, environment check"
version = { args = ["version"], pattern = 'fake-archiver (\d+\.\d+)' }
extension = ".fk"
mode = "directory"
create = ["create", "{settings}", "{archive}", "{input}"]
extract = ["extract", "{archive}", "{outdir}"]
licence = "test"
[[tool.setting]]
id = "envcheck"
compress = ["--mode=envcheck"]

[[tool]]
id = "fake-nested"
name = "fake archiver, nested layout"
version = { args = ["version"], pattern = 'fake-archiver (\d+\.\d+)' }
extension = ".fk"
mode = "directory"
layout = "nested"
create = ["create", "{archive}", "{input}"]
extract = ["extract", "--nested", "{archive}", "{outdir}"]
licence = "test"
[[tool.setting]]
id = "ok"

[[tool]]
id = "fake-list"
name = "fake archiver taking a list"
version = { args = ["version"], pattern = 'fake-archiver (\d+\.\d+)' }
extension = ".fk"
mode = "directory"
create = ["create", "{archive}", "{input}"]
create_list = ["create", "{archive}", "@{list}"]
extract = ["extract", "{settings}", "{archive}", "{outdir}"]
licence = "test"
[[tool.setting]]
id = "ok"
[[tool.setting]]
id = "miss"
extract = ["--mode=miss"]

[[tool]]
id = "fake-stream"
name = "fake stream compressor"
version = { args = ["version"], pattern = 'fake-archiver (\d+\.\d+)' }
extension = ".tar.fk"
mode = "tar-stream"
create = ["stream", "compress", "{settings}", "{threads}"]
extract = ["stream", "decompress", "{settings}"]
threads = ["--threads={n}"]
licence = "test"
[[tool.setting]]
id = "ok"
[[tool.setting]]
id = "exit1"
compress = ["--mode=exit1"]
"#;

struct Env {
    tmp: tempfile::TempDir,
    catalogue: PathBuf,
    local: PathBuf,
    corpus: PathBuf,
    results: PathBuf,
    work: PathBuf,
}

type Files<'a> = &'a [(&'a str, Vec<u8>)];

fn bytes(seed: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// Write the files, `manifest.json` and `build-info.json`. Public corpus: files under
/// `<corpus>/`; private: under `private_root`, which build-info.json names.
fn write_corpus(corpus: &Path, classes: &[(&str, Files<'_>)], private_root: Option<&Path>) {
    std::fs::create_dir_all(corpus).unwrap();
    let root = private_root.unwrap_or(corpus);
    let mut class_entries = serde_json::Map::new();
    for (class, files) in classes {
        let mut list = Vec::new();
        let mut total = 0u64;
        for (path, data) in files.iter() {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, data).unwrap();
            total += data.len() as u64;
            list.push(serde_json::json!({
                "blake3": blake3::hash(data).to_hex().to_string(),
                "bytes": data.len(),
                "licence": "test",
                "path": path,
                "source": "test",
            }));
        }
        class_entries.insert(
            class.to_string(),
            serde_json::json!({ "bytes_total": total, "files": list }),
        );
    }
    let manifest = serde_json::json!({
        "classes": class_entries,
        "profile": if private_root.is_some() { "private" } else { "small" },
    });
    std::fs::write(
        corpus.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let info = match private_root {
        Some(r) => serde_json::json!({ "private": true, "root": r.to_string_lossy() }),
        None => serde_json::json!({}),
    };
    std::fs::write(corpus.join("build-info.json"), info.to_string()).unwrap();
}

type OwnedFiles = Vec<(&'static str, Vec<u8>)>;

fn public_classes() -> Vec<(&'static str, OwnedFiles)> {
    vec![
        (
            "text",
            vec![
                ("text/a.txt", b"hello world\n".repeat(200)),
                ("text/sub/b.txt", bytes(3, 5000)),
                ("text/sub/deep/c.bin", bytes(9, 70_000)),
                ("text/empty.txt", Vec::new()),
            ],
        ),
        (
            "bin",
            vec![
                ("bin/x.bin", bytes(1, 40_000)),
                ("bin/y.bin", bytes(2, 123)),
            ],
        ),
    ]
}

fn env_with(tools_local: &[&str]) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let catalogue = tmp.path().join("tools.toml");
    std::fs::write(&catalogue, CATALOGUE).unwrap();
    let local = tmp.path().join("tools.local.toml");
    let mut text = String::new();
    for id in tools_local {
        text.push_str(&format!("[[tool]]\nid = \"{id}\"\npath = '{FAKE}'\n"));
    }
    std::fs::write(&local, text).unwrap();
    let corpus = tmp.path().join("corpus");
    let classes = public_classes();
    let refs: Vec<(&str, Files<'_>)> = classes.iter().map(|(c, f)| (*c, f.as_slice())).collect();
    write_corpus(&corpus, &refs, None);
    Env {
        results: tmp.path().join("results"),
        work: tmp.path().join("work"),
        tmp,
        catalogue,
        local,
        corpus,
    }
}

fn env() -> Env {
    env_with(&[
        "fake",
        "fake-env",
        "fake-nested",
        "fake-list",
        "fake-stream",
    ])
}

fn command(e: &Env) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lpk-bench"));
    c.arg("run")
        .arg("--catalogue")
        .arg(&e.catalogue)
        .arg("--local")
        .arg(&e.local)
        .arg("--results")
        .arg(&e.results)
        .arg("--tmp")
        .arg(&e.work);
    c
}

fn measure(e: &Env, extra: &[&str]) -> Output {
    command(e)
        .arg("--corpus")
        .arg(&e.corpus)
        .arg("--allow-dirty-build")
        .args(extra)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The only (or last) results directory.
fn results_dir(e: &Env) -> PathBuf {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&e.results)
        .unwrap()
        .map(|d| d.unwrap().path())
        .collect();
    dirs.sort();
    dirs.pop().expect("a results directory")
}

fn result(dir: &Path, name: &str) -> Value {
    let path = dir.join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{name}")))
        .unwrap()
}

fn assert_tmp_clean(e: &Env) {
    let left: Vec<_> = match std::fs::read_dir(&e.work) {
        Ok(rd) => rd.map(|d| d.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    };
    assert!(left.is_empty(), "temporary files left behind: {left:?}");
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    if v.len() % 2 == 1 {
        v[v.len() / 2]
    } else {
        (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0
    }
}

#[test]
fn a_full_pass_validates_verifies_every_repeat_and_cleans_up() {
    let e = env();
    let out = measure(
        &e,
        &[
            "--tools",
            "store,fake-nested,fake-stream",
            "--repeats",
            "3",
            "--threads",
            "2",
        ],
    );
    // The catalogue's `fake-stream/exit1` setting fails on purpose, so the exit code is non-zero.
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("6 measured, 2 failed, 0 skipped; 0 validation problem(s)"),
        "{}",
        text(&out)
    );
    let dir = results_dir(&e);

    // The validator accepts the directory when run on its own, too.
    let v = Command::new(env!("CARGO_BIN_EXE_lpk-bench"))
        .arg("run")
        .arg("--validate")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(v.status.success(), "{}", text(&v));

    let mut seen = 0;
    for tool_setting in [
        "store-store",
        "fake-nested-ok",
        "fake-stream-ok",
        "fake-stream-exit1",
    ] {
        for class in ["text", "bin"] {
            let r = result(&dir, &format!("{tool_setting}-{class}.json"));
            if tool_setting == "fake-stream-exit1" {
                assert!(
                    r["failed"]["reason"]
                        .as_str()
                        .unwrap()
                        .contains("exit code 1"),
                    "{r}"
                );
                continue;
            }
            seen += 1;
            assert_eq!(r["threads"], 2);
            assert!(
                r.get("failed").is_none() && r.get("skipped").is_none(),
                "{r}"
            );
            let repeats = r["repeats"].as_array().unwrap();
            assert_eq!(repeats.len(), 3);
            let want_files = if class == "text" { 4 } else { 2 };
            assert_eq!(r["verification"]["verified"], true);
            assert_eq!(r["verification"]["files_checked"], want_files);
            assert_eq!(r["verification"]["files_ok"], want_files);
            assert_eq!(r["measurement"]["every_repeat_verified"], true);
            assert!(r["measurement"]["env_stripped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n == "XZ_OPT"));
            for (k, f) in [
                ("compress", "wall_seconds"),
                ("extract", "wall_seconds"),
                ("compress", "user_cpu_seconds"),
            ] {
                let want = median(repeats.iter().map(|s| s[k][f].as_f64().unwrap()).collect());
                let got = r["median"][k][f].as_f64().unwrap();
                assert!(
                    (want - got).abs() < 1e-12,
                    "{tool_setting} {class} {k}.{f}: {want} vs {got}"
                );
            }
            assert!(r["median"]["archive_bytes"].as_u64().unwrap() > 0);
            for s in repeats {
                assert_eq!(s["compress"]["timed_out"], false);
                assert_eq!(s["extract"]["descendants_killed"], false);
            }
            let stream = tool_setting.starts_with("fake-stream");
            assert_eq!(r["measurement"].get("tar").is_some(), stream, "{r}");
            for s in repeats {
                for k in ["compress", "extract"] {
                    let (tar, tool) = (&s[k]["tar_step"], &s[k]["tool_step"]);
                    if stream {
                        let sum = tar["wall_seconds"].as_f64().unwrap()
                            + tool["wall_seconds"].as_f64().unwrap();
                        assert_eq!(s[k]["wall_seconds"].as_f64().unwrap(), sum);
                    } else {
                        assert!(tar.is_null() && tool.is_null());
                    }
                }
            }
        }
    }
    assert_eq!(seen, 6);
    // The recorded arguments are relative to the working directory.
    let r = result(&dir, "fake-nested-ok-text.json");
    for a in r["setting"]["compress_args"].as_array().unwrap() {
        let a = a.as_str().unwrap();
        assert!(!Path::new(a).is_absolute(), "{a}");
    }
    assert_eq!(r["setting"]["compress_args"][2], "text");
    assert_tmp_clean(&e);
}

#[test]
fn failures_are_recorded_with_their_reason_and_do_not_stop_the_run() {
    let e = env();
    let out = measure(
        &e,
        &[
            "--tools",
            "store,fake",
            "--repeats",
            "2",
            "--timeout-s",
            "2",
            "--classes",
            "text",
        ],
    );
    assert!(
        !out.status.success(),
        "a failed combination must make the run exit non-zero"
    );
    assert!(
        text(&out).contains("0 validation problem(s)"),
        "{}",
        text(&out)
    );
    let dir = results_dir(&e);
    let get = |setting: &str| result(&dir, &format!("fake-{setting}-text.json"));

    // The healthy combinations ran, before and after the failing ones.
    for name in ["store-store-text.json", "fake-ok-text.json"] {
        let r = result(&dir, name);
        assert!(r.get("failed").is_none(), "{name}: {r}");
        assert_eq!(r["verification"]["verified"], true);
    }
    let reason = |s: &str| {
        let r = get(s);
        assert!(
            r.get("median").is_none(),
            "{s}: a failed combination carries no median"
        );
        (
            r["failed"]["reason"].as_str().unwrap().to_string(),
            r["failed"]["step"].as_str().unwrap().to_string(),
            r["failed"]["repeat"].as_u64().unwrap(),
            r["failed"]["timed_out"].as_bool().unwrap(),
        )
    };
    let (why, step, repeat, _) = reason("noarchive");
    assert!(
        why.contains("not created") && step == "compress" && repeat == 1,
        "{why}"
    );
    let (why, ..) = reason("empty");
    assert!(why.contains("empty"), "{why}");
    let (why, ..) = reason("exit1");
    assert!(why.contains("exit code 1"), "{why}");
    let (why, step, _, timed_out) = reason("sleep");
    assert!(
        why.contains("timed out") && timed_out && step == "compress",
        "{why}"
    );
    let (why, step, ..) = reason("exit3");
    assert!(why.contains("exit code 3") && step == "extract", "{why}");
    let (why, step, ..) = reason("miss");
    assert!(
        why.contains("missing") && why.contains("a.txt") && step == "verify",
        "{why}"
    );
    let r = get("miss");
    assert_eq!(r["verification"]["verified"], false);
    assert_eq!(r["verification"]["files_ok"], 3);
    let (why, ..) = reason("alter");
    assert!(why.contains("different") && why.contains("a.txt"), "{why}");
    let (why, ..) = reason("extra");
    assert!(
        why.contains("extra") && why.contains("__extra.txt"),
        "{why}"
    );
    // A tool that leaves a child running is a failure, with the flag set.
    let r = get("orphan");
    assert!(r.get("median").is_none());
    assert_eq!(r["failed"]["descendants_killed"], true, "{r}");
    assert!(
        r["failed"]["reason"]
            .as_str()
            .unwrap()
            .contains("still running"),
        "{r}"
    );
    assert_tmp_clean(&e);
}

#[test]
fn a_stream_step_failure_is_recorded() {
    let e = env();
    let out = measure(
        &e,
        &[
            "--tools",
            "store,fake-stream",
            "--repeats",
            "1",
            "--classes",
            "bin",
        ],
    );
    assert!(!out.status.success());
    let dir = results_dir(&e);
    let r = result(&dir, "fake-stream-exit1-bin.json");
    assert!(
        r["failed"]["reason"]
            .as_str()
            .unwrap()
            .contains("tool step: exit code 1"),
        "{r}"
    );
    assert!(result(&dir, "fake-stream-ok-bin.json")
        .get("failed")
        .is_none());
}

#[test]
fn tool_configuration_variables_do_not_reach_the_child() {
    // The control: the fake does see such a variable when it is not stripped.
    let direct = Command::new(FAKE)
        .args(["create", "--mode=envcheck", "unused.fk", "unused"])
        .env("XZ_OPT", "-9")
        .current_dir(tempfile::tempdir().unwrap().path())
        .status();
    if let Ok(s) = direct {
        assert_eq!(s.code(), Some(17));
    }
    let e = env();
    let mut cmd = command(&e);
    let out = cmd
        .arg("--corpus")
        .arg(&e.corpus)
        .arg("--allow-dirty-build")
        .args([
            "--tools",
            "store,fake-env",
            "--repeats",
            "1",
            "--classes",
            "bin",
        ])
        .env("XZ_OPT", "-9")
        .env("ZSTD_CLEVEL", "19")
        .env("TAR_OPTIONS", "--no-such-option")
        .env("RAR", "-m0")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let dir = results_dir(&e);
    assert!(result(&dir, "fake-env-envcheck-bin.json")
        .get("failed")
        .is_none());
    assert!(result(&dir, "store-store-bin.json").get("failed").is_none());
    // The variables' values are nowhere in the results.
    let all: String = std::fs::read_dir(&dir)
        .unwrap()
        .map(|d| std::fs::read_to_string(d.unwrap().path()).unwrap())
        .collect();
    assert!(!all.contains("no-such-option"));
}

#[test]
fn a_private_corpus_runs_list_capable_tools_and_skips_the_others() {
    let e = env();
    let root = e.tmp.path().join("private-root");
    let corpus = e.tmp.path().join("private-corpus");
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("one/a.txt", b"private a".to_vec()),
        ("one/two/b.bin", bytes(4, 3000)),
        ("c.txt", b"c".to_vec()),
    ];
    write_corpus(&corpus, &[("mixed", files.as_slice())], Some(&root));
    let out = command(&e)
        .arg("--corpus")
        .arg(&corpus)
        .arg("--allow-dirty-build")
        .args([
            "--tools",
            "store,fake,fake-list,fake-stream",
            "--repeats",
            "2",
        ])
        .output()
        .unwrap();
    // fake-list/miss fails on purpose.
    assert!(!out.status.success(), "{}", text(&out));
    let dir = results_dir(&e);
    for name in ["store-store-mixed.json", "fake-list-ok-mixed.json"] {
        let r = result(&dir, name);
        assert_eq!(r["private"], true, "{name}");
        assert_eq!(r["corpus"]["profile"], "private");
        assert_eq!(r["verification"]["verified"], true, "{r}");
        assert_eq!(r["verification"]["files_ok"], 3);
    }
    for name in ["fake-ok-mixed.json", "fake-stream-ok-mixed.json"] {
        let r = result(&dir, name);
        assert_eq!(r["private"], true);
        assert!(
            r["skipped"].as_str().unwrap().contains("private corpus"),
            "{r}"
        );
        assert!(r.get("median").is_none());
    }
    assert_tmp_clean(&e);
}

#[test]
fn an_input_that_does_not_match_the_manifest_aborts_naming_the_file() {
    let e = env();
    std::fs::write(e.corpus.join("text/a.txt"), b"tampered").unwrap();
    let out = measure(&e, &["--tools", "store", "--repeats", "1"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("text/a.txt") && err.contains("does not match the manifest"),
        "{err}"
    );
    assert_tmp_clean(&e);
}

#[test]
fn an_unknown_class_or_tool_is_refused() {
    let e = env();
    let out = measure(&e, &["--tools", "store", "--classes", "nope"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown class `nope`"));
    let out = measure(&e, &["--tools", "nope"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown tool"));
}

#[test]
fn compare_accepts_a_run_against_itself_and_rejects_a_different_corpus() {
    let e = env();
    let args = [
        "--tools",
        "store,fake-nested",
        "--repeats",
        "1",
        "--classes",
        "bin",
    ];
    assert!(measure(&e, &args).status.success());
    assert!(measure(&e, &args).status.success());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&e.results)
        .unwrap()
        .map(|d| d.unwrap().path())
        .collect();
    dirs.sort();
    assert_eq!(
        dirs.len(),
        2,
        "a second run on the same day gets its own directory: {dirs:?}"
    );
    let bin = env!("CARGO_BIN_EXE_lpk-bench");
    let same = Command::new(bin)
        .args(["run", "--compare"])
        .arg(&dirs[0])
        .arg(&dirs[0])
        .args(["--max-diff-pct", "0"])
        .output()
        .unwrap();
    assert!(same.status.success(), "{}", text(&same));
    assert!(text(&same).contains("store/store"));
    // Two real runs differ by some amount; a generous limit passes, an impossible one cannot.
    let loose = Command::new(bin)
        .args(["run", "--compare"])
        .arg(&dirs[0])
        .arg(&dirs[1])
        .args(["--max-diff-pct", "100000"])
        .output()
        .unwrap();
    assert!(loose.status.success(), "{}", text(&loose));

    // Another corpus: compare refuses.
    let other = e.tmp.path().join("other-corpus");
    let mut classes = public_classes();
    classes[1].1[1].1 = b"different bytes".to_vec();
    let refs: Vec<(&str, Files<'_>)> = classes.iter().map(|(c, f)| (*c, f.as_slice())).collect();
    write_corpus(&other, &refs, None);
    let out = command(&e)
        .arg("--corpus")
        .arg(&other)
        .arg("--allow-dirty-build")
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    dirs = std::fs::read_dir(&e.results)
        .unwrap()
        .map(|d| d.unwrap().path())
        .collect();
    dirs.sort();
    let mismatch = Command::new(bin)
        .args(["run", "--compare"])
        .arg(&dirs[0])
        .arg(&dirs[2])
        .args(["--max-diff-pct", "100000"])
        .output()
        .unwrap();
    assert!(!mismatch.status.success());
    assert!(
        String::from_utf8_lossy(&mismatch.stderr).contains("different corpora"),
        "{}",
        text(&mismatch)
    );
}

#[test]
fn a_dirty_or_unknown_build_is_refused_without_the_flag() {
    let e = env();
    // With the flag the run works and host.json records which build it was.
    let ok = measure(
        &e,
        &["--tools", "store", "--repeats", "1", "--classes", "bin"],
    );
    assert!(ok.status.success(), "{}", text(&ok));
    let host = result(&results_dir(&e), "host.json");
    assert_eq!(host["dirty_build_allowed"], true);
    let commit = host["git_commit"].as_str().unwrap().to_string();
    let before = std::fs::read_dir(&e.results).unwrap().count();
    let out = command(&e)
        .arg("--corpus")
        .arg(&e.corpus)
        .args(["--tools", "store", "--repeats", "1", "--classes", "bin"])
        .output()
        .unwrap();
    if commit.ends_with("-dirty") || commit == "unknown" {
        assert!(!out.status.success(), "{}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not a clean commit"),
            "{}",
            text(&out)
        );
        assert_eq!(
            std::fs::read_dir(&e.results).unwrap().count(),
            before,
            "no results from a refused run"
        );
    } else {
        // A clean checkout: nothing to refuse (the unit test of `check_build` covers the gate).
        assert!(out.status.success(), "{}", text(&out));
    }
}

/// A private corpus with Cyrillic, CJK and accented names and spaces, plus an ASCII class.
fn write_private_non_ascii(e: &Env) -> PathBuf {
    let root = e.tmp.path().join("private-root-u");
    let corpus = e.tmp.path().join("private-corpus-u");
    let uni: Vec<(&str, Vec<u8>)> = vec![
        ("дом/файл с пробелом.txt", b"cyrillic".to_vec()),
        ("日本語/ファイル.bin", bytes(5, 2000)),
        ("café/geheim-ünï.txt", b"secret".to_vec()),
    ];
    let plain: Vec<(&str, Vec<u8>)> = vec![("plain/a b.txt", b"plain".to_vec())];
    write_corpus(
        &corpus,
        &[("unicode", uni.as_slice()), ("ascii", plain.as_slice())],
        Some(&root),
    );
    corpus
}

#[test]
fn non_ascii_private_names_round_trip_through_the_list_path_and_the_system_tar_rule_holds() {
    let e = env();
    let corpus = write_private_non_ascii(&e);
    let out = command(&e)
        .arg("--corpus")
        .arg(&corpus)
        .arg("--allow-dirty-build")
        .args(["--tools", "store,fake-list,fake-stream", "--repeats", "2"])
        .output()
        .unwrap();
    // fake-list/miss fails on purpose; everything else must be consistent.
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("0 validation problem(s)"),
        "{}",
        text(&out)
    );
    let dir = results_dir(&e);
    for class in ["unicode", "ascii"] {
        let r = result(&dir, &format!("fake-list-ok-{class}.json"));
        assert_eq!(r["verification"]["verified"], true, "{class}: {r}");
        let s = result(&dir, &format!("store-store-{class}.json"));
        if cfg!(windows) && class == "unicode" {
            assert!(s["skipped"].as_str().unwrap().contains("non-ASCII"), "{s}");
            let z = result(&dir, "fake-stream-ok-unicode.json");
            assert!(z["skipped"].as_str().unwrap().contains("non-ASCII"), "{z}");
        } else {
            assert_eq!(s["verification"]["verified"], true, "{class}: {s}");
        }
    }
    assert_tmp_clean(&e);
}

#[test]
fn a_private_failure_names_no_file_in_any_result() {
    let e = env();
    let corpus = write_private_non_ascii(&e);
    let out = command(&e)
        .arg("--corpus")
        .arg(&corpus)
        .arg("--allow-dirty-build")
        .args([
            "--tools",
            "fake-list",
            "--repeats",
            "1",
            "--classes",
            "unicode",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let dir = results_dir(&e);
    let r = result(&dir, "fake-list-miss-unicode.json");
    let why = r["failed"]["reason"].as_str().unwrap();
    assert!(
        why.contains("missing") && why.contains("manifest file #"),
        "{why}"
    );
    let all: String = std::fs::read_dir(&dir)
        .unwrap()
        .map(|d| std::fs::read_to_string(d.unwrap().path()).unwrap())
        .collect();
    for secret in ["geheim", "ünï", "дом", "日本語", "пробелом", "unicode/"] {
        assert!(
            !all.contains(secret) || secret == "unicode/",
            "`{secret}` leaked into the results"
        );
    }
}

#[test]
fn a_long_combination_is_measured_once_and_says_so() {
    let e = env();
    let out = measure(
        &e,
        &[
            "--tools",
            "store,fake-nested",
            "--repeats",
            "3",
            "--long-run-s",
            "0",
            "--classes",
            "bin",
        ],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("0 validation problem(s)"),
        "{}",
        text(&out)
    );
    let dir = results_dir(&e);
    for name in ["store-store-bin.json", "fake-nested-ok-bin.json"] {
        let r = result(&dir, name);
        assert_eq!(r["repeats_requested"], 3);
        assert_eq!(r["repeats"].as_array().unwrap().len(), 1);
        assert!(
            r["repeats_short"]
                .as_str()
                .unwrap()
                .contains("--long-run-s"),
            "{r}"
        );
        assert_eq!(r["verification"]["verified"], true);
    }
    let run = result(&dir, "run.json");
    assert_eq!(run["long_run_s"], 0);
    assert_eq!(run["complete"], true);
}

#[test]
fn an_aborted_run_does_not_validate_and_a_trimmed_one_does_not_either() {
    let e = env();
    // Aborted: an input does not match the manifest, the run stops before run.json.
    std::fs::write(e.corpus.join("text/a.txt"), b"tampered").unwrap();
    let out = measure(&e, &["--tools", "store", "--repeats", "1"]);
    assert!(!out.status.success());
    let dir = results_dir(&e);
    assert!(!dir.join("run.json").exists());
    let v = Command::new(env!("CARGO_BIN_EXE_lpk-bench"))
        .args(["run", "--validate"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(!v.status.success());
    assert!(
        String::from_utf8_lossy(&v.stderr).contains("run.json"),
        "{}",
        text(&v)
    );

    // Trimmed: a result file removed from a complete run.
    let e = env();
    let out = measure(&e, &["--tools", "store", "--repeats", "1"]);
    assert!(out.status.success(), "{}", text(&out));
    let dir = results_dir(&e);
    std::fs::remove_file(dir.join("store-store-bin.json")).unwrap();
    let v = Command::new(env!("CARGO_BIN_EXE_lpk-bench"))
        .args(["run", "--validate"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(!v.status.success());
    assert!(
        String::from_utf8_lossy(&v.stderr).contains("has no result file"),
        "{}",
        text(&v)
    );
}

#[test]
fn compare_refuses_runs_with_different_repeats() {
    let e = env();
    let base = ["--tools", "store", "--classes", "bin"];
    assert!(measure(&e, &[&base[..], &["--repeats", "1"]].concat())
        .status
        .success());
    assert!(measure(&e, &[&base[..], &["--repeats", "2"]].concat())
        .status
        .success());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&e.results)
        .unwrap()
        .map(|d| d.unwrap().path())
        .collect();
    dirs.sort();
    let out = Command::new(env!("CARGO_BIN_EXE_lpk-bench"))
        .args(["run", "--compare"])
        .arg(&dirs[0])
        .arg(&dirs[1])
        .args(["--max-diff-pct", "100000"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("requested repeats"),
        "{}",
        text(&out)
    );
}
