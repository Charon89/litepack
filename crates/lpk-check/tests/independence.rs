//! D-49 item 5: the independent decoder never links the reference. `lpk-format` and `lpk-core`
//! may be dev-dependencies (the crafted-record test calls them as a black box), never normal
//! ones, directly or through another crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

const REFERENCE: [&str; 3] = ["lpk-format", "lpk-core", "lpk-decode"];

/// `cargo tree -p lpk-check -e normal` lists no reference crate.
#[test]
fn no_normal_dependency_on_the_reference() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let out = Command::new(cargo)
        .args(["tree", "--offline", "-e", "normal", "--prefix", "none"])
        .args(["-p", "lpk-check", "--manifest-path"])
        .arg(&manifest)
        .output()
        .expect("cargo runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.starts_with("lpk-check "), "{stdout}");
    for line in stdout.lines() {
        let name = line.split(' ').next().unwrap();
        assert!(
            !REFERENCE.contains(&name),
            "lpk-check depends on {name} (normal edge): {stdout}"
        );
    }
}

/// The same, read from the manifest: `[dependencies]` names no reference crate.
#[test]
fn manifest_dependencies_name_no_reference_crate() {
    let m =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let t: toml::Table = m.parse().unwrap();
    let deps = t["dependencies"].as_table().unwrap();
    for r in REFERENCE {
        assert!(!deps.contains_key(r), "{r} is a normal dependency");
    }
    for key in ["build-dependencies", "target"] {
        assert!(!t.contains_key(key), "{key} is not expected in lpk-check");
    }
}
