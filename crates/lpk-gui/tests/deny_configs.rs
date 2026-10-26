//! The GUI's cargo-deny policy (`crates/lpk-gui/deny.toml`, run with `cargo deny --manifest-path
//! crates/lpk-gui/Cargo.toml check`) is the main policy (`deny.toml`) for the GUI's triples: the
//! duplicated tables must stay equal.
#![allow(clippy::unwrap_used)]

use std::path::Path;

fn load(p: &Path) -> toml::Table {
    std::fs::read_to_string(p).unwrap().parse().unwrap()
}

#[test]
fn the_gui_deny_policy_duplicates_the_main_one() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main = load(&dir.join("../../deny.toml"));
    let gui = load(&dir.join("deny.toml"));
    for table in ["advisories", "licenses", "bans", "sources"] {
        assert_eq!(main.get(table), gui.get(table), "[{table}] differs");
    }
    // The intended differences, all in [graph]: the main check excludes lpk-gui; the GUI's
    // check limits the graph to the Windows triples it ships on.
    let mg = main["graph"].as_table().unwrap();
    let gg = gui["graph"].as_table().unwrap();
    assert_eq!(mg.get("all-features"), gg.get("all-features"));
    assert_eq!(
        mg["exclude"].as_array().unwrap(),
        &[toml::Value::from("lpk-gui")]
    );
    let targets: Vec<&str> = gg["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        targets,
        ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]
    );
    let keys = |t: &toml::Table| t.keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(mg), ["all-features", "exclude"]);
    assert_eq!(keys(gg), ["all-features", "targets"]);
    // No other top-level table on either side.
    assert_eq!(keys(&main), keys(&gui));
}
