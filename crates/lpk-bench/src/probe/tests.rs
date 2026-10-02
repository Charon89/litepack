//! Tests of the probe framework with a tiny corpus built in a temporary directory.

use std::path::{Path, PathBuf};

use super::weights::{build_safetensors, sample_floats};
use super::*;
use crate::corpus::manifest::Manifest;
use crate::run::validate::validate_dir;

fn manifest_file(path: &str, data: &[u8]) -> ManifestFile {
    ManifestFile {
        blake3: blake3::hash(data).to_hex().to_string(),
        bytes: data.len() as u64,
        licence: "CC0-1.0".to_string(),
        path: path.to_string(),
        source: "test".to_string(),
    }
}

/// A corpus with one `model-weights` class: a safetensors file and a text file. With `private`,
/// the files live under `<tmp>/data` and the corpus directory only holds the manifest.
fn tiny_corpus(tmp: &Path, private: bool) -> PathBuf {
    let weights = build_safetensors(&[
        ("a", "F32", vec![64, 16], sample_floats(1024, 4)),
        ("b", "BF16", vec![300], sample_floats(300, 2)),
        ("ids", "I64", vec![2], vec![1; 16]),
    ]);
    let text = b"not a model".to_vec();
    let files_dir = if private {
        tmp.join("data")
    } else {
        tmp.join("corpus")
    };
    let class_dir = files_dir.join("model-weights");
    std::fs::create_dir_all(&class_dir).expect("mkdir");
    std::fs::write(class_dir.join("secret-name.safetensors"), &weights).expect("write");
    std::fs::write(class_dir.join("readme.txt"), &text).expect("write");
    let manifest = Manifest::with_profile_name(
        if private { "private" } else { "small" },
        vec![
            (
                "model-weights".to_string(),
                manifest_file("model-weights/secret-name.safetensors", &weights),
            ),
            (
                "model-weights".to_string(),
                manifest_file("model-weights/readme.txt", &text),
            ),
        ],
    );
    let corpus_dir = tmp.join("corpus");
    std::fs::create_dir_all(&corpus_dir).expect("mkdir");
    std::fs::write(corpus_dir.join("manifest.json"), manifest.render()).expect("manifest");
    if private {
        let info = serde_json::json!({"private": true, "root": files_dir.to_string_lossy()});
        std::fs::write(corpus_dir.join("build-info.json"), info.to_string()).expect("info");
    }
    corpus_dir
}

fn config(corpus: PathBuf, root: PathBuf, probes: &[&str]) -> Config {
    let tmp_root = root.join("tmp");
    Config {
        probes: probes.iter().map(|s| s.to_string()).collect(),
        corpus,
        into: None,
        results_root: root,
        tmp_root,
        threads: 2,
        allow_dirty: true,
        // The tests run in a debug build.
        allow_debug_build: true,
        local_tools: PathBuf::from("no-such-local-tools.toml"),
        tool_timeout: Duration::from_secs(60),
    }
}

/// A corpus with a class `docs`: `a.txt`, `sub/b.txt`, `sub/deep/c.txt`.
pub(crate) fn tiny_class_corpus(tmp: &Path) -> PathBuf {
    let dir = tmp.join("classcorpus");
    let mut files = Vec::new();
    for (rel, data) in [
        ("a.txt", "alpha"),
        ("sub/b.txt", "bravo bravo"),
        ("sub/deep/c.txt", "charlie"),
    ] {
        let p = dir.join("docs").join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(&p, data).expect("write");
        files.push((
            "docs".to_string(),
            manifest_file(&format!("docs/{rel}"), data.as_bytes()),
        ));
    }
    let m = Manifest::with_profile_name("small", files);
    std::fs::write(dir.join("manifest.json"), m.render()).expect("manifest");
    dir
}

/// Run `f` with a context on the corpus in `corpus_dir` and a scratch directory under `tmp`.
pub(crate) fn with_ctx(corpus_dir: &Path, tmp: &Path, f: impl FnOnce(&Ctx<'_>)) {
    let corpus = load_corpus(corpus_dir).expect("corpus");
    let scratch = tmp.join("scratch");
    std::fs::create_dir_all(&scratch).expect("scratch");
    let ctx = Ctx {
        corpus: &corpus,
        threads: 2,
        scratch,
        local_tools: PathBuf::from("none.toml"),
        tool_timeout: Duration::from_secs(60),
    };
    f(&ctx);
}

fn problems(dir: &Path) -> Vec<String> {
    validate_dir(dir).expect("validate").problems
}

fn edit_json(path: &Path, f: impl FnOnce(&mut Value)) {
    let mut v: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
    f(&mut v);
    std::fs::write(path, serde_json::to_string_pretty(&v).expect("render")).expect("write");
}

/// Run `probe weights` on the tiny corpus into a fresh results directory.
fn weights_run(tmp: &Path) -> (PathBuf, Outcome) {
    let corpus = tiny_corpus(tmp, false);
    let cfg = config(corpus, tmp.join("results"), &["weights"]);
    let out = execute(&cfg).expect("execute");
    (out.results_dir.clone(), out)
}

#[test]
fn weights_end_to_end_writes_json_and_table_and_the_directory_validates() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, out) = weights_run(tmp.path());
    assert!(out.failed.is_empty(), "{:?}", out.failed);
    assert_eq!(out.problems, 0, "{:?}", problems(&dir));
    assert!(dir.join("host.json").is_file());
    assert!(
        !dir.join("tools.json").exists() && !dir.join("run.json").exists(),
        "a probe-only directory needs neither"
    );
    let json = std::fs::read_to_string(dir.join("probe-weights.json")).expect("json");
    let md = std::fs::read_to_string(dir.join("probe-weights.md")).expect("md");
    assert_eq!(md, render_file("weights", &json).expect("render"));
    let env: Envelope<weights::Data> = serde_json::from_str(&json).expect("typed");
    assert_eq!(env.probe, "weights");
    assert_eq!(env.format_version, FORMAT_VERSION);
    assert!(env.libraries.contains_key("libzstd"));
    assert_eq!(env.data.files.len(), 1);
    assert_eq!(env.data.not_parsed.len(), 1);
    assert_eq!(env.data.not_parsed[0].index, 0, "readme.txt sorts first");
    assert_eq!(env.data.files[0].index, 1);
    let names: Vec<&str> = env.data.files[0]
        .dtypes
        .iter()
        .map(|d| d.dtype.as_str())
        .collect();
    assert_eq!(names, ["BF16", "F32"]);
    assert!(md.contains("## Sizes by dtype, all files") && md.contains("## Not parsed"));
    // The output carries no absolute path or user name.
    assert!(!json.contains(tmp.path().to_string_lossy().as_ref()));
}

#[test]
fn rendering_is_pure_and_a_changed_table_is_a_problem() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    let json = std::fs::read_to_string(dir.join("probe-weights.json")).expect("json");
    assert_eq!(
        render_file("weights", &json).expect("a"),
        render_file("weights", &json).expect("b")
    );
    let md_path = dir.join("probe-weights.md");
    let md = std::fs::read_to_string(&md_path).expect("md");
    std::fs::write(&md_path, md.replacen("gain", "GAIN", 1)).expect("write");
    assert!(problems(&dir)
        .iter()
        .any(|m| m.starts_with("probe-weights.md:") && m.contains("does not equal the table")));
    std::fs::remove_file(&md_path).expect("rm");
    assert!(problems(&dir)
        .iter()
        .any(|m| m.starts_with("probe-weights.md:") && m.contains("missing")));
}

#[test]
fn unknown_fields_and_broken_rules_are_reported() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    let path = dir.join("probe-weights.json");
    let original = std::fs::read_to_string(&path).expect("read");

    edit_json(&path, |v| v["surprise"] = Value::from(1));
    assert!(problems(&dir)
        .iter()
        .any(|m| m.starts_with("probe-weights.json:") && m.contains("unknown field")));

    std::fs::write(&path, &original).expect("restore");
    edit_json(&path, |v| {
        v["data"]["files"][0]["dtypes"][1]["byte_planes"]["compressed_bytes"] = Value::from(1)
    });
    assert!(problems(&dir)
        .iter()
        .any(|m| m.contains("/data/files/0/dtypes/1/byte_planes/compressed_bytes")));

    std::fs::write(&path, &original).expect("restore");
    edit_json(&path, |v| {
        v["data"]["files"][0]["tensors"] = Value::from(99)
    });
    assert!(problems(&dir)
        .iter()
        .any(|m| m.contains("/data/files/0/tensors")));

    std::fs::write(&path, &original).expect("restore");
    edit_json(&path, |v| v["probe"] = Value::from("jpeg"));
    assert!(problems(&dir)
        .iter()
        .any(|m| m.contains("does not match the file name")));

    std::fs::write(&path, &original).expect("restore");
    assert!(problems(&dir).is_empty());
}

#[test]
fn host_and_corpus_mismatches_are_reported() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    let json = dir.join("probe-weights.json");
    let original = std::fs::read_to_string(&json).expect("read");

    edit_json(&json, |v| v["host"] = Value::from("another-box"));
    assert!(problems(&dir)
        .iter()
        .any(|m| m.starts_with("probe-weights.json: /host:") && m.contains("host.json says")));
    std::fs::write(&json, &original).expect("restore");

    // A second probe file made on another manifest.
    let other = dir.join("probe-jpeg.json");
    std::fs::write(&other, &original).expect("copy");
    edit_json(&other, |v| {
        v["probe"] = Value::from("jpeg");
        v["data"] = serde_json::json!({});
        v["corpus"]["manifest_blake3"] = Value::from("0".repeat(64));
    });
    assert!(problems(&dir)
        .iter()
        .any(|m| m.contains("/corpus/manifest_blake3: differs from probe-jpeg.json")));
}

#[test]
fn a_directory_with_baseline_files_still_needs_run_and_tools_json() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    std::fs::write(dir.join("store-default-docs.json"), "{}").expect("write");
    let p = problems(&dir);
    assert!(
        p.iter()
            .any(|m| m.starts_with("tools.json: (root): file is missing")),
        "{p:?}"
    );
    assert!(
        p.iter()
            .any(|m| m.starts_with("run.json: (root): file is missing")),
        "{p:?}"
    );
    // And a host.json alone, without probe or result files, is still an aborted run.
    let empty = tmp.path().join("results").join("2026-10-02-empty");
    std::fs::create_dir_all(&empty).expect("mkdir");
    std::fs::copy(dir.join("host.json"), empty.join("host.json")).expect("copy");
    assert!(problems(&empty)
        .iter()
        .any(|m| m.starts_with("run.json: (root): file is missing")));
}

#[test]
fn a_private_corpus_leaves_names_and_paths_out_of_the_result() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_corpus(tmp.path(), true);
    let cfg = config(corpus, tmp.path().join("results"), &["weights"]);
    let out = execute(&cfg).expect("execute");
    assert_eq!(out.problems, 0, "{:?}", problems(&out.results_dir));
    let json = std::fs::read_to_string(out.results_dir.join("probe-weights.json")).expect("json");
    let md = std::fs::read_to_string(out.results_dir.join("probe-weights.md")).expect("md");
    for text in [&json, &md] {
        assert!(
            !text.contains("secret-name") && !text.contains("readme"),
            "{text}"
        );
        assert!(!text.contains("model-weights/"), "{text}");
    }
    let env: Envelope<weights::Data> = serde_json::from_str(&json).expect("typed");
    assert!(env.corpus.private);
    assert!(json.contains("\"private\": true"));
    assert!(env.data.files.iter().all(|f| f.path.is_none()));
    assert!(env.data.not_parsed.iter().all(|f| f.path.is_none()));
    // A path put back is a problem.
    let path = out.results_dir.join("probe-weights.json");
    edit_json(&path, |v| {
        v["data"]["files"][0]["path"] = Value::from("a/b.safetensors")
    });
    assert!(problems(&out.results_dir)
        .iter()
        .any(|m| m.contains("a private corpus carries no file names")));
}

#[test]
fn probe_all_continues_after_a_failing_probe() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_corpus(tmp.path(), false);
    let cfg = config(corpus, tmp.path().join("results"), &NAMES);
    let out = execute(&cfg).expect("execute");
    // Independent of which probes are still stubs: every name either wrote its file or
    // failed as a stub, and the run went on after the first failure.
    let failed: Vec<&str> = out.failed.iter().map(|(n, _)| n.as_str()).collect();
    let mut covered: Vec<&str> = out.written.iter().map(String::as_str).collect();
    covered.extend(failed.iter().copied());
    covered.sort_unstable();
    let mut all: Vec<&str> = NAMES.to_vec();
    all.sort_unstable();
    assert_eq!(covered, all, "every probe either wrote its file or failed");
    assert!(out.written.iter().any(|n| n == "weights"), "weights ran");
    for (name, reason) in &out.failed {
        assert!(reason.contains("not implemented yet"), "{name}: {reason}");
        assert!(!out.results_dir.join(format!("probe-{name}.json")).exists());
    }
    assert_eq!(out.problems, 0, "the written files validate");
    assert!(out.results_dir.join("probe-weights.json").is_file());
}

#[test]
fn an_existing_results_directory_is_extended_only_for_the_same_corpus_and_host() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    let host_before = std::fs::read_to_string(dir.join("host.json")).expect("host");
    let corpus = tmp.path().join("corpus");
    let mut cfg = config(corpus.clone(), tmp.path().join("results"), &["weights"]);
    cfg.into = Some(dir.clone());
    let again = execute(&cfg).expect("same corpus is accepted");
    assert_eq!(again.results_dir, dir);
    assert_eq!(
        std::fs::read_to_string(dir.join("host.json")).expect("host"),
        host_before,
        "host.json is not rewritten"
    );

    // Another corpus (one more file, so another manifest hash) is refused.
    let extra = corpus.join("model-weights/extra.txt");
    std::fs::write(&extra, b"extra").expect("write");
    let mut m: Manifest =
        serde_json::from_str(&std::fs::read_to_string(corpus.join("manifest.json")).expect("m"))
            .expect("parse");
    let entry = m.classes.get_mut("model-weights").expect("class");
    entry
        .files
        .push(manifest_file("model-weights/extra.txt", b"extra"));
    std::fs::write(corpus.join("manifest.json"), m.render()).expect("write");
    let err = execute(&cfg).expect_err("refused").to_string();
    assert!(err.contains("another corpus"), "{err}");

    // A different machine is refused too.
    edit_json(&dir.join("host.json"), |v| {
        v["cpu_model"] = Value::from("Other CPU")
    });
    let err = execute(&cfg).expect_err("refused").to_string();
    assert!(err.contains("different machine"), "{err}");

    // A non-empty directory without host.json is not a results directory.
    let stray = tmp.path().join("stray");
    std::fs::create_dir_all(&stray).expect("mkdir");
    std::fs::write(stray.join("x.txt"), "x").expect("write");
    cfg.into = Some(stray);
    assert!(execute(&cfg).is_err());
}

#[test]
fn a_changed_input_file_aborts_naming_the_file() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_corpus(tmp.path(), false);
    std::fs::write(corpus.join("model-weights/readme.txt"), b"tampered!!!").expect("write");
    let cfg = config(corpus, tmp.path().join("results"), &["weights"]);
    let out = execute(&cfg).expect("execute");
    assert_eq!(out.failed.len(), 1);
    assert!(
        out.failed[0].1.contains("model-weights/readme.txt"),
        "{:?}",
        out.failed
    );
    assert!(out.failed[0].1.contains("does not match the manifest"));
}

#[test]
fn par_map_keeps_the_order_for_any_thread_count() {
    let items: Vec<u64> = (0..200).collect();
    for threads in [0usize, 1, 2, 7, 64] {
        let out = par_map(&items, threads, |i, v| {
            assert_eq!(i as u64, *v);
            v * 3
        });
        assert_eq!(
            out,
            items.iter().map(|v| v * 3).collect::<Vec<_>>(),
            "{threads}"
        );
    }
    let empty: Vec<u8> = Vec::new();
    assert!(par_map(&empty, 4, |_, v| *v).is_empty());
}

#[test]
fn the_timer_returns_the_value_and_a_non_negative_time() {
    let (v, s) = timed(|| 21 * 2);
    assert_eq!(v, 42);
    assert!(s >= 0.0 && s.is_finite());
    assert_eq!(pct(1, 4), "25.00%");
    assert_eq!(pct(1, 0), "n/a");
    assert_eq!(mbps(2_000_000, 1.0), "2.0");
    assert_eq!(mbps(1, 0.0), "n/a");
}

#[test]
fn the_envelope_rules_catch_bad_headers() {
    let mut env = Envelope {
        probe: "weights".to_string(),
        format_version: FORMAT_VERSION,
        corpus: CorpusId {
            profile: "small".to_string(),
            manifest_blake3: "a".repeat(64),
            private: false,
        },
        build: "abc123".to_string(),
        build_profile: BuildProfile {
            profile: "release".to_string(),
            opt_level: "3".to_string(),
            debug_assertions: false,
            allow_debug_build: false,
        },
        host: "box".to_string(),
        date: "2026-10-02T10:00:00Z".to_string(),
        threads: 4,
        library_threads: 1,
        libraries: [("libzstd".to_string(), "1".to_string())]
            .into_iter()
            .collect(),
        elapsed_seconds: 1.0,
        notes: Vec::new(),
        data: Value::Null,
    };
    assert!(check_common(&env).is_empty());
    env.threads = 0;
    env.libraries.clear();
    env.corpus.manifest_blake3 = "xyz".to_string();
    env.date = "yesterday".to_string();
    env.host = "Box!".to_string();
    env.elapsed_seconds = -1.0;
    env.format_version = 9;
    env.library_threads = 0;
    assert_eq!(check_common(&env).len(), 8, "{:?}", check_common(&env));
}

#[test]
fn timestamps_must_be_real() {
    assert!(valid_timestamp("2026-10-02T10:00:00Z"));
    assert!(valid_timestamp("2028-02-29T23:59:59Z"));
    for bad in [
        "2026-99-99T99:99:99Z",
        "2026-02-30T10:00:00Z",
        "2026-10-02T24:00:00Z",
        "2026-10-02T10:60:00Z",
        "2026-10-02T10:00:60Z",
        "2026-10-02 10:00:00Z",
        "2026-10-02T10:00:00",
        "20x6-10-02T10:00:00Z",
        "",
    ] {
        assert!(!valid_timestamp(bad), "{bad}");
    }
}

#[test]
fn an_unoptimised_build_is_refused_unless_allowed_and_the_validator_checks_the_record() {
    if cfg!(debug_assertions) {
        assert!(
            check_profile(false).is_err(),
            "the test build is a debug build"
        );
        let msg = check_profile(false).expect_err("refused").to_string();
        assert!(
            msg.contains("--allow-debug-build") && msg.contains("--release"),
            "{msg}"
        );
        let tmp = tempfile::tempdir().expect("tmp");
        let corpus = tiny_corpus(tmp.path(), false);
        let mut cfg = config(corpus, tmp.path().join("results"), &["weights"]);
        cfg.allow_debug_build = false;
        assert!(execute(&cfg).is_err());
    }
    assert!(check_profile(true).is_ok());
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    let json: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("probe-weights.json")).expect("json"),
    )
    .expect("parse");
    assert_eq!(
        json["build_profile"]["allow_debug_build"],
        Value::Bool(true)
    );
    assert!(json["build_profile"]["profile"].is_string());
    assert!(json["library_threads"].is_u64());
    // An unoptimised profile with the flag not recorded is a problem.
    let path = dir.join("probe-weights.json");
    edit_json(&path, |v| {
        v["build_profile"] = serde_json::json!({
            "profile": "debug", "opt_level": "0", "debug_assertions": true,
            "allow_debug_build": false
        });
    });
    assert!(problems(&dir)
        .iter()
        .any(|m| m.contains("/build_profile: not an optimised release build")));
}

#[test]
fn one_build_per_directory_when_adding_and_when_validating() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    let mut cfg = config(
        tmp.path().join("corpus"),
        tmp.path().join("results"),
        &["weights"],
    );
    cfg.into = Some(dir.clone());
    edit_json(&dir.join("host.json"), |v| {
        v["git_commit"] = Value::from("0123456789ab")
    });
    let err = execute(&cfg).expect_err("refused").to_string();
    assert!(err.contains("one build per results directory"), "{err}");
    assert!(problems(&dir)
        .iter()
        .any(|m| m.starts_with("probe-weights.json: /build:") && m.contains("one build per")));
}

#[test]
fn into_needs_an_existing_directory_with_host_json_and_results_is_a_root() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_corpus(tmp.path(), false);
    let mut cfg = config(corpus, tmp.path().join("results"), &["weights"]);
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).expect("mkdir");
    cfg.into = Some(empty);
    assert!(execute(&cfg)
        .expect_err("no host.json")
        .to_string()
        .contains("host.json"));
    cfg.into = Some(tmp.path().join("missing"));
    assert!(execute(&cfg).is_err());
    cfg.into = None;
    let out = execute(&cfg).expect("fresh");
    assert_eq!(
        out.results_dir.parent(),
        Some(tmp.path().join("results").as_path())
    );
    let second = execute(&cfg).expect("a second fresh directory");
    assert_ne!(second.results_dir, out.results_dir);
    assert!(second.results_dir.to_string_lossy().ends_with("-2"));
}

#[test]
fn a_failing_rerun_removes_the_earlier_files_and_the_scratch_is_gone() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    assert!(dir.join("probe-weights.json").is_file());
    // Same manifest, damaged input: the probe fails.
    std::fs::write(
        tmp.path().join("corpus/model-weights/readme.txt"),
        b"damaged!!!",
    )
    .expect("write");
    let mut cfg = config(
        tmp.path().join("corpus"),
        tmp.path().join("results"),
        &["weights"],
    );
    cfg.into = Some(dir.clone());
    let out = execute(&cfg).expect("execute");
    assert_eq!(out.failed.len(), 1);
    assert!(!dir.join("probe-weights.json").exists() && !dir.join("probe-weights.md").exists());
    // Nothing stale is left: the directory is just a host.json, which is no result at all.
    assert!(
        problems(&dir).iter().all(|m| !m.contains("probe-weights")),
        "{:?}",
        problems(&dir)
    );
    let left: Vec<_> = std::fs::read_dir(&cfg.tmp_root)
        .map(|r| r.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "scratch directories left behind: {left:?}");
}

#[test]
fn a_private_result_with_name_fields_in_data_is_rejected() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_corpus(tmp.path(), true);
    let cfg = config(corpus, tmp.path().join("results"), &["weights"]);
    let out = execute(&cfg).expect("execute");
    let path = out.results_dir.join("probe-weights.json");
    for key in ["name", "folder", "path"] {
        let original = std::fs::read_to_string(&path).expect("read");
        edit_json(&path, |v| {
            v["data"]["not_parsed"][0][key] = Value::from("x");
        });
        assert!(
            problems(&out.results_dir)
                .iter()
                .any(|m| m.contains(&format!(
                    "/data/not_parsed/0/{key}: a private corpus carries no file or folder names"
                ))),
            "{key}"
        );
        std::fs::write(&path, original).expect("restore");
    }
}

#[test]
fn groups_label_by_folder_and_index_for_private_corpora() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_class_corpus(tmp.path());
    with_ctx(&corpus, tmp.path(), |ctx| {
        let g = ctx.groups("docs", 1);
        let labels: Vec<&str> = g.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["", "sub"]);
        assert_eq!(
            g[1].files.len(),
            2,
            "deeper files fall into their depth-1 folder"
        );
        let g2 = ctx.groups("docs", 2);
        let labels: Vec<&str> = g2.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["", "sub", "sub/deep"]);
        assert_eq!(ctx.groups("docs", 0).len(), 1);
        assert!(ctx.groups("nope", 1).is_empty());
    });
    // Private: index labels, no folder names.
    let private = tmp.path().join("private");
    std::fs::create_dir_all(&private).expect("mkdir");
    std::fs::copy(corpus.join("manifest.json"), private.join("manifest.json")).expect("copy");
    let info = serde_json::json!({"private": true, "root": corpus.to_string_lossy()});
    std::fs::write(private.join("build-info.json"), info.to_string()).expect("info");
    with_ctx(&private, tmp.path(), |ctx| {
        let labels: Vec<String> = ctx.groups("docs", 2).into_iter().map(|g| g.label).collect();
        assert_eq!(labels, ["group-0", "group-1", "group-2"]);
    });
}

#[test]
fn blocks_are_streamed_and_a_changed_file_fails_at_the_end() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_class_corpus(tmp.path());
    with_ctx(&corpus, tmp.path(), |ctx| {
        let f = &ctx.class_files("docs").expect("class")[1];
        let mut got = Vec::new();
        let mut blocks = 0;
        ctx.read_blocks("docs", f, 4, |b| {
            got.extend_from_slice(b);
            blocks += 1;
            Ok(())
        })
        .expect("streamed");
        assert_eq!(got, b"bravo bravo");
        assert_eq!(blocks, 3);
        std::fs::write(corpus.join("docs/sub/b.txt"), b"bravo brav0").expect("tamper");
        let err = ctx
            .read_blocks("docs", f, 4, |_| Ok(()))
            .expect_err("mismatch");
        assert!(err.to_string().contains("docs/sub/b.txt"), "{err}");
        let err = ctx.read_file("docs", f).expect_err("mismatch");
        assert!(err.to_string().contains("does not match the manifest"));
    });
}

/// A reader that returns at most `step` bytes per call.
struct Trickle<'a> {
    data: &'a [u8],
    step: usize,
}

impl std::io::Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.step.min(buf.len()).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

#[test]
fn blocks_are_full_size_even_when_reads_are_short() {
    let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    for (block, step) in [
        (64usize, 1usize),
        (64, 7),
        (100, 100),
        (1, 1),
        (4096, 3),
        (7, 64),
    ] {
        let mut sizes = Vec::new();
        let mut got = Vec::new();
        let (total, hash) = hash_blocks(&mut Trickle { data: &data, step }, block, &mut |b| {
            sizes.push(b.len());
            got.extend_from_slice(b);
            Ok(())
        })
        .expect("blocks");
        assert_eq!(got, data);
        assert_eq!(total, 1000);
        assert_eq!(hash, blake3::hash(&data).to_hex().to_string());
        let (last, full) = sizes.split_last().expect("sizes");
        assert!(
            full.iter().all(|s| *s == block.min(1000)),
            "{block}/{step}: {sizes:?}"
        );
        assert!(*last <= block && *last > 0);
    }
    let (total, _) =
        hash_blocks(&mut Trickle { data: &[], step: 5 }, 8, &mut |_| Ok(())).expect("empty");
    assert_eq!(total, 0);
}

#[test]
fn level_19_means_the_baseline_setting_and_the_long_variant_is_named() {
    let plain = codec::ZstdSettings::level19();
    assert_eq!(
        (plain.level, plain.window_log, plain.long_distance_matching),
        (19, None, false)
    );
    let long = codec::ZstdSettings::level19_long27();
    assert_eq!(
        (long.level, long.window_log, long.long_distance_matching),
        (19, Some(27), true)
    );
}

#[test]
fn a_run_that_wrote_nothing_leaves_no_new_directory() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_corpus(tmp.path(), false);
    std::fs::write(corpus.join("model-weights/readme.txt"), b"tampered!!!").expect("write");
    let cfg = config(corpus, tmp.path().join("results"), &["weights"]);
    let out = execute(&cfg).expect("execute");
    assert!(
        out.written.is_empty() && !out.results_dir.exists(),
        "{:?}",
        out.results_dir
    );
}

#[test]
fn the_scratch_helper_gives_empty_directories() {
    let tmp = tempfile::tempdir().expect("tmp");
    let corpus = tiny_class_corpus(tmp.path());
    with_ctx(&corpus, tmp.path(), |ctx| {
        let d = ctx.scratch_dir("work").expect("dir");
        std::fs::write(d.join("x"), b"x").expect("write");
        let again = ctx.scratch_dir("work").expect("dir");
        assert_eq!(d, again);
        assert!(std::fs::read_dir(&again).expect("read").next().is_none());
        assert_eq!(
            ctx.find_tool("definitely-not-a-tool-xyz"),
            Err(tool::NOT_INSTALLED.to_string())
        );
    });
}
