//! E7-7 (R320): the updater is compiled out by the `updater` feature (on by default). The Store
//! build (E7-4) turns it off; its tree then holds no `tauri-plugin-updater`, and the GUI offers
//! no update setting (`startup`'s `updater` is false, `apps/gui/main.js` hides the switch).
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

/// The packages in `lpk-gui`'s normal dependency tree for every target, with `features`.
fn tree(features: &[&str]) -> String {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let out = Command::new(env!("CARGO"))
        .args(["tree", "--manifest-path"])
        .arg(&manifest)
        .args([
            "-p",
            "lpk-gui",
            "-e",
            "normal",
            "--target",
            "all",
            "--locked",
            "--offline",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .args(features)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cargo tree: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn has_updater(tree: &str) -> bool {
    tree.lines().any(|l| l.starts_with("tauri-plugin-updater "))
}

#[test]
fn the_store_build_has_no_updater_in_its_tree() {
    assert!(
        has_updater(&tree(&[])),
        "the default build lost its updater"
    );
    let store = tree(&["--no-default-features"]);
    assert!(store.lines().any(|l| l.starts_with("tauri ")), "{store}");
    assert!(
        !has_updater(&store),
        "tauri-plugin-updater in the Store build's tree"
    );
}

#[test]
fn the_page_hides_the_update_setting_without_the_updater() {
    // The host tells the page whether it has the updater, from the feature alone.
    let host = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("host.rs"),
    )
    .unwrap();
    assert!(host.contains("updater: cfg!(feature = \"updater\")"));
    let page = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/gui/main.js"),
    )
    .unwrap();
    assert!(
        page.contains("startup.updater"),
        "main.js reads no updater flag"
    );
}
