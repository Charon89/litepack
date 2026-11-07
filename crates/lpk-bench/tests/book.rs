//! E9-6: the docs book's benchmark and corpus pages are generated from committed files, are the same on
//! every run, equal the committed pages, and hold neither a gate verdict nor a machine detail.
//! `LPK_BENCH_BOOK_WRITE=1 cargo nextest run -p lpk-bench book` rewrites the committed pages.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PAGES: [&str; 2] = ["results.md", "corpus.md"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn committed_dir() -> PathBuf {
    root().join("book/src/bench")
}

fn book(out_dir: &Path, check: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lpk-bench"));
    cmd.current_dir(root())
        .args(["book", "--root", "."])
        .arg("--out-dir")
        .arg(out_dir);
    if check {
        cmd.arg("--check");
    }
    cmd.output().expect("run lpk-bench book")
}

fn generate_into(dir: &Path) {
    let out = book(dir, false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The host names and the scanner product names recorded in the published inputs.
fn recorded_machine_details() -> (BTreeSet<String>, BTreeSet<String>) {
    let site: toml::Table =
        toml::from_str(&std::fs::read_to_string(root().join("bench/site.toml")).unwrap())
            .expect("bench/site.toml");
    let dirs: Vec<String> = ["results", "probes"]
        .iter()
        .filter_map(|k| site.get(*k))
        .flat_map(|v| v.as_array().expect("array").iter())
        .map(|v| v.as_str().expect("path").to_string())
        .collect();
    let (mut hosts, mut products) = (BTreeSet::new(), BTreeSet::new());
    for d in &dirs {
        let json = |f: &str| -> serde_json::Value {
            let text = std::fs::read_to_string(root().join(d).join(f)).unwrap();
            serde_json::from_str(&text).unwrap()
        };
        let host = json("host.json");
        hosts.insert(host["host"].as_str().expect("host").to_string());
        // A probe directory has no run.json.
        let run = if root().join(d).join("run.json").is_file() {
            json("run.json")
        } else {
            serde_json::Value::Null
        };
        for list in [&host["antivirus"], &run["antivirus_end"]] {
            for p in list.as_array().into_iter().flatten() {
                products.insert(p["name"].as_str().expect("name").to_string());
            }
        }
    }
    (hosts, products)
}

#[test]
fn pages_are_deterministic_and_equal_the_committed_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    generate_into(&a);
    generate_into(&b);
    for name in PAGES {
        assert_eq!(read(&a, name), read(&b, name), "{name}: two runs differ");
    }
    if std::env::var_os("LPK_BENCH_BOOK_WRITE").is_some() {
        std::fs::create_dir_all(committed_dir()).unwrap();
        for name in PAGES {
            std::fs::write(committed_dir().join(name), read(&a, name)).unwrap();
        }
    }
    for name in PAGES {
        assert_eq!(
            read(&committed_dir(), name),
            read(&a, name),
            "{name} differs from the generated page; run `cargo run -p lpk-bench -- book` and commit"
        );
    }
    let ok = book(&committed_dir(), true);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

#[test]
fn check_fails_with_a_short_message_on_a_changed_page() {
    let tmp = tempfile::tempdir().unwrap();
    generate_into(tmp.path());
    let page = tmp.path().join("results.md");
    let text = std::fs::read_to_string(&page).unwrap();
    std::fs::write(&page, text.replacen("# Results", "# Results (edited)", 1)).unwrap();
    let out = book(tmp.path(), true);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("results.md: line 1"), "{err}");
    assert!(err.len() < 600, "{err}");
}

#[test]
fn pages_hold_no_verdict_and_no_machine_detail() {
    let (hosts, products) = recorded_machine_details();
    assert!(!hosts.is_empty(), "no host name recorded in the inputs");
    for name in PAGES {
        let text = read(&committed_dir(), name);
        let lower = text.to_lowercase();
        for host in &hosts {
            assert!(
                !lower.contains(&host.to_lowercase()),
                "{name} names host `{host}`"
            );
        }
        assert!(!lower.contains("antivirus"), "{name} says antivirus");
        for p in &products {
            assert!(
                !lower.contains(&p.to_lowercase()),
                "{name} names scanner `{p}`"
            );
        }
        for p in &products {
            // "Windows" is the OS name, which the site shows; the product's other words are banned
            // through "defender" below.
            if let Some(first) = p
                .split_whitespace()
                .next()
                .filter(|w| !w.eq_ignore_ascii_case("windows"))
            {
                assert!(
                    !lower.contains(&first.to_lowercase()),
                    "{name} holds the first word `{first}` of scanner `{p}`"
                );
            }
        }
        for word in [
            "avast",
            "defender",
            "builds ran",
            "background",
            "while builds",
        ] {
            assert!(!lower.contains(word), "{name} holds `{word}`");
        }
        for banned in ["NO-GO", "GO proposal"] {
            assert!(!text.contains(banned), "{name} holds `{banned}`");
        }
        for line in text.lines() {
            assert!(
                !(line.starts_with("## ") && line.contains("Gates")),
                "{name}: gate heading `{line}`"
            );
            for g in ["| G1", "| G2", "| G3", "| G4"] {
                assert!(!line.starts_with(g), "{name}: gate row `{line}`");
            }
        }
        assert!(!text.contains('\r'), "{name} has CR");
        assert!(
            text.trim_end().ends_with("{{#include ../footer.md}}"),
            "{name} lacks the footer include"
        );
    }
}
