//! The GUI's seam (E5-1, for E5-2): `lpk-gui` reaches the engine through `lpk-api` alone, so
//! that a closed GUI build could later move to its own repository over a published `lpk-api`
//! (`docs/notes/e5-design.md` section 1). Its manifest may name no `lpk-*` crate but those in
//! [`ALLOWED`], in any dependency table, and its sources no `lpk_core::` or `lpk_format::`
//! path. Until `crates/lpk-gui` exists (E5-2) the test passes trivially; E5-2 inherits it.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

/// The engine crates the GUI may depend on.
const ALLOWED: &[&str] = &["lpk-api", "lpk-gui-sys"];

/// The engine paths its sources may not name.
const BARRED_PATHS: &[&str] = &["lpk_core::", "lpk_format::"];

fn gui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-gui")
}

/// Every `lpk-*` name in the manifest's dependency tables (target-specific ones included).
fn engine_deps(manifest: &toml::Table) -> Vec<String> {
    let mut found = Vec::new();
    let tables = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut scan = |t: &toml::Table| {
        for k in tables {
            if let Some(deps) = t.get(k).and_then(|d| d.as_table()) {
                for (name, spec) in deps {
                    let package = spec.get("package").and_then(|p| p.as_str()).unwrap_or(name);
                    if package.starts_with("lpk-") || package.starts_with("lpk_") {
                        found.push(package.to_string());
                    }
                }
            }
        }
    };
    scan(manifest);
    if let Some(targets) = manifest.get("target").and_then(|t| t.as_table()) {
        for t in targets.values().filter_map(|t| t.as_table()) {
            scan(t);
        }
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn the_gui_depends_on_the_api_alone() {
    let dir = gui_dir();
    let manifest = dir.join("Cargo.toml");
    if !manifest.is_file() {
        eprintln!("{}: not yet (E5-2); nothing to check", manifest.display());
        return;
    }
    let text = std::fs::read_to_string(&manifest).unwrap();
    let table: toml::Table = text.parse().unwrap();
    let bad: Vec<String> = engine_deps(&table)
        .into_iter()
        .filter(|d| !ALLOWED.contains(&d.as_str()))
        .collect();
    assert!(
        bad.is_empty(),
        "lpk-gui depends on {bad:?}; only {ALLOWED:?}"
    );
    let mut files = Vec::new();
    for sub in ["src", "tests", "build.rs"] {
        let p = dir.join(sub);
        if p.is_file() {
            files.push(p);
        } else {
            rust_files(&p, &mut files);
        }
    }
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        for barred in BARRED_PATHS {
            assert!(
                !src.contains(barred),
                "{} names {barred}: reach the engine through lpk_api",
                f.display()
            );
        }
    }
}

#[test]
fn the_check_sees_a_forbidden_dependency() {
    let t: toml::Table = r#"
        [dependencies]
        lpk-api = { path = "../lpk-api" }
        [target.'cfg(windows)'.dependencies]
        engine = { package = "lpk-core", path = "../lpk-core" }
    "#
    .parse()
    .unwrap();
    assert_eq!(engine_deps(&t), ["lpk-api", "lpk-core"]);
}
