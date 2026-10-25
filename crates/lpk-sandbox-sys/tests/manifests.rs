//! E4-1's manifest check: `unsafe` is allowed only in the `-sys` crates CLAUDE.md names; every
//! other workspace crate inherits the workspace lints (`unsafe_code = "forbid"`).

use std::path::Path;

/// Crates allowed `unsafe` (CLAUDE.md: `*-sys` and `*-simd`); a new one is a reviewed change here.
const ALLOWED: &[&str] = &["lpk-procstat-sys", "lpk-sandbox-sys"];

#[test]
fn only_the_sys_crates_allow_unsafe() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let ws = std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace manifest");
    assert!(
        ws.contains("unsafe_code = \"forbid\""),
        "workspace lints no longer forbid unsafe"
    );
    let mut seen = 0;
    for entry in std::fs::read_dir(root.join("crates")).expect("crates dir") {
        let dir = entry.expect("entry").path();
        let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        seen += 1;
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let inherits = manifest.contains("[lints]\nworkspace = true")
            || manifest.contains("[lints]\r\nworkspace = true");
        if ALLOWED.contains(&name.as_str()) {
            assert!(name.ends_with("-sys") || name.ends_with("-simd"), "{name}");
            assert!(
                manifest.contains("undocumented_unsafe_blocks"),
                "{name} must lint undocumented unsafe blocks"
            );
        } else {
            assert!(inherits, "{name} does not inherit the workspace lints");
            assert!(
                !manifest.contains("unsafe_code"),
                "{name} overrides unsafe_code"
            );
        }
    }
    assert!(seen >= 8, "found only {seen} crates");
}
