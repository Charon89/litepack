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
        lpk_format::entry_payload_table(),
        lpk_format::entry_byte_table(),
        lpk_format::entry_kind_table(),
        lpk_format::entry_flag_table(),
        lpk_format::chunk_record_table(),
        lpk_format::index_layout_table(),
        lpk_format::trailer_layout_table(),
        lpk_format::envelope_layout_table(),
        lpk_format::resources_default_table(),
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

#[test]
fn spec_states_the_envelope_caps_and_refusal_message() {
    let text = spec();
    assert_eq!(lpk_format::DEFAULT_MAX_WINDOW, 268_435_456);
    assert_eq!(lpk_format::DEFAULT_MAX_BWT_BLOCK, 67_108_864);
    assert!(text.contains("`max_window` at most 268435456 bytes (256 MiB)"));
    assert!(text.contains("`max_bwt_block` at most 67108864 bytes (64 MiB)"));
    assert!(
        text.contains("the archive needs <field> of <needed> bytes; this reader allows\n<allowed>")
    );
    let r = lpk_format::Refusal {
        field: "f",
        needed: 1,
        allowed: 0,
    };
    assert_eq!(
        r.to_string(),
        "the archive needs f of 1 bytes; this reader allows 0"
    );
}

#[test]
fn spec_states_the_trailer_constants() {
    let text = spec();
    assert_eq!(lpk_format::TRAILER_PAYLOAD_LEN, 72);
    assert_eq!(lpk_format::TRAILER_FRAME_LEN, 109);
    assert!(text.contains("fixed 72-byte payload"));
    assert!(text.contains("= 109 bytes"));
    assert!(text.contains("(141 bytes)"));
    assert_eq!(
        lpk_format::Header::LEN as u64 + lpk_format::TRAILER_FRAME_LEN,
        141
    );
}
