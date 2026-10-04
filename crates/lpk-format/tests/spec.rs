#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

fn spec() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("spec")
        .join("lpk-v1.md");
    // Normalise line endings so a Windows checkout with CRLF still matches.
    std::fs::read_to_string(p).unwrap().replace("\r\n", "\n")
}

#[test]
fn spec_tables_match_code() {
    let text = spec();
    for table in [
        lpk_format::frame_layout_table(),
        lpk_format::frame_kind_table(),
        lpk_format::frame_flag_table(),
        lpk_format::header_flag_table(),
        lpk_format::header_byte_table(),
        lpk_format::entry_kind_table(),
        lpk_format::entry_flag_table(),
    ] {
        assert!(text.contains(&table), "spec lacks table:\n{table}");
    }
    let magic = lpk_format::MAGIC
        .iter()
        .map(|b| format!("0x{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(magic, "0x89 0x4C 0x50 0x4B 0x0D 0x0A 0x1A 0x0A");
    assert!(text.contains(&magic));
}
