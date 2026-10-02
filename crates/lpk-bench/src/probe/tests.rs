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
    Config {
        probes: probes.iter().map(|s| s.to_string()).collect(),
        corpus,
        results_dir: None,
        results_root: root,
        threads: 2,
        allow_dirty: true,
    }
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
    assert!(md.contains("## By dtype, all files") && md.contains("## Not parsed"));
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
        v["data"]["files"][0]["dtypes"][1]["split_compressed_bytes"] = Value::from(1)
    });
    assert!(problems(&dir)
        .iter()
        .any(|m| m.contains("/data/files/0/dtypes/1/split_compressed_bytes")));

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
    assert_eq!(
        out.written,
        ["weights"],
        "weights ran after jpeg, deflate, dedup and text failed"
    );
    let failed: Vec<&str> = out.failed.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(failed, ["jpeg", "deflate", "dedup", "text", "entropy-gate"]);
    assert!(out.failed[0].1.contains("not implemented yet"));
    assert_eq!(out.problems, 0, "the written files validate");
    assert!(out.results_dir.join("probe-weights.json").is_file());
    assert!(!out.results_dir.join("probe-jpeg.json").exists());
}

#[test]
fn an_existing_results_directory_is_extended_only_for_the_same_corpus_and_host() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (dir, _) = weights_run(tmp.path());
    let host_before = std::fs::read_to_string(dir.join("host.json")).expect("host");
    let corpus = tmp.path().join("corpus");
    let mut cfg = config(corpus.clone(), tmp.path().join("results"), &["weights"]);
    cfg.results_dir = Some(dir.clone());
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
    cfg.results_dir = Some(stray);
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
        host: "box".to_string(),
        date: "2026-10-02T10:00:00Z".to_string(),
        threads: 4,
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
    assert_eq!(check_common(&env).len(), 7, "{:?}", check_common(&env));
}
