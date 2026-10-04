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
        lpk_format::primitive_table(),
        lpk_format::graph_layout_table(),
        lpk_format::block_header_table(),
        lpk_format::prior_list_table(),
        lpk_format::record_kind_table(),
        lpk_format::record_layout_tables(),
        lpk_format::recovery_layout_table(),
        lpk_format::key_slot_table(),
        lpk_format::sealing_rules_table(),
        lpk_format::generation_rules_table(),
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
fn primitive_table_lists_every_id_and_name() {
    let table = lpk_format::primitive_table();
    for p in lpk_format::PrimitiveId::ALL {
        let row = format!("| 0x{:04X} | `{}` |", p as u16, p.name());
        assert!(table.contains(&row), "table lacks {row}");
    }
    // Each row's claims, checked against the validators and `resources`.
    use lpk_format::PrimitiveId as P;
    let row = |p: P| {
        let tag = format!("| 0x{:04X} | `{}` |", p as u16, p.name());
        table
            .lines()
            .find(|l| l.starts_with(&tag))
            .unwrap()
            .to_string()
    };
    let lens = [
        (P::Store, 0),
        (P::Zstd, 33),
        (P::Lzma, 7),
        (P::Bwt, 4),
        (P::BcjX86, 0),
        (P::BcjArm64, 0),
        (P::Delta, 9),
    ];
    for p in [
        P::JpegReconstruct,
        P::DeflateReconstruct,
        P::PngFilter,
        P::Base64,
        P::Utf16,
        P::ContainerReconstruct,
    ] {
        assert_eq!(p.params_len(), None, "{}", p.name());
        p.validate_params(&[0]).unwrap();
        assert!(p.validate_params(&[]).is_err());
        assert!(row(p).contains("`record_id: varint`"), "{}", p.name());
    }
    for (p, len) in lens {
        assert_eq!(p.params_len(), Some(len), "{}", p.name());
        let mut ok = vec![0u8; len];
        if p == P::Zstd {
            ok[0] = 10;
        }
        if p == P::Bwt {
            ok[0] = 1;
        }
        p.validate_params(&ok).unwrap();
        assert!(p.validate_params(&vec![0u8; len + 1]).is_err());
        if len > 0 {
            assert!(p.validate_params(&ok[..len - 1]).is_err());
        }
        // The row's parameter cell says "none" exactly for the empty layouts.
        let cell = row(p).split(" | ").nth(2).unwrap().to_string();
        assert_eq!(cell.starts_with("none"), len == 0, "{}", p.name());
    }
    let mut z = vec![0u8; 33];
    for (wl, ok) in [(9, false), (10, true), (31, true), (32, false)] {
        z[0] = wl;
        assert_eq!(P::Zstd.validate_params(&z).is_ok(), ok);
    }
    assert!(row(P::Zstd).contains("10..=31"));
    z[0] = 31;
    assert_eq!(P::Zstd.resources(&z).window, 1 << 31);
    assert!(row(P::Zstd).contains("window = 2^window_log"));
    // The row's own limits; `lc + lp <= 4` is what bounds lc below 8.
    for (i, max, name, doc) in [(4, 4, "lc", 8), (5, 4, "lp", 4), (6, 4, "pb", 4)] {
        let mut l = vec![0u8; 7];
        l[i] = max;
        P::Lzma.validate_params(&l).unwrap();
        l[i] = doc + 1;
        assert!(P::Lzma.validate_params(&l).is_err(), "{name}");
        assert!(row(P::Lzma).contains(&format!("{name} <= {doc}")));
    }
    assert!(row(P::Lzma).contains("lc + lp <= 4"));
    assert!(P::Lzma.validate_params(&[0, 0, 0, 0, 2, 3, 0]).is_err());
    let l = [0x78, 0x56, 0x34, 0x12, 0, 0, 0];
    assert_eq!(P::Lzma.resources(&l).window, 0x1234_5678);
    assert!(row(P::Lzma).contains("window = dict_size"));
    let b = [0x78, 0x56, 0x34, 0x12];
    assert_eq!(P::Bwt.resources(&b).bwt_block, 0x1234_5678);
    assert!(P::Bwt.validate_params(&[0; 4]).is_err());
    assert!(row(P::Bwt).contains("bwt block = block_size"));
    {
        let mut v = vec![0u8; 9];
        v[8] = 1;
        P::Delta.validate_params(&v).unwrap();
        v[8] = 2;
        assert!(P::Delta.validate_params(&v).is_err());
    }
    for p in P::ALL {
        let mut v = vec![0u8; p.params_len().unwrap_or(1)];
        if p == P::Zstd {
            v[0] = 20;
        }
        if p == P::Bwt {
            v[0] = 1;
        }
        let r = p.resources(&v);
        let has_res = !row(p).ends_with("| none |");
        let nonzero = r.window != 0 || r.bwt_block != 0;
        // Only zstd, lzma and bwt declare window/bwt resources; lzma with a
        // zero dict_size is the one row whose zero parameters give zero.
        assert_eq!(nonzero, matches!(p, P::Zstd | P::Bwt), "{}", p.name());
        if matches!(p, P::Zstd | P::Lzma | P::Bwt | P::JpegReconstruct) {
            assert!(has_res, "{}", p.name());
        } else {
            assert!(!has_res, "{}", p.name());
        }
    }
    assert_eq!(lpk_format::MAX_STEPS, 16);
    assert_eq!(lpk_format::MAX_PARAMS, 256);
}

#[test]
fn spec_states_the_envelope_caps_and_refusal_message() {
    let text = spec();
    assert_eq!(lpk_format::DEFAULT_MAX_WINDOW, 268_435_456);
    assert_eq!(lpk_format::DEFAULT_MAX_BWT_BLOCK, 67_108_864);
    assert!(text.contains("`max_window` at most 268435456 bytes (256 MiB)"));
    assert!(text.contains("`max_bwt_block` at most 67108864 bytes (64 MiB)"));
    assert!(text
        .contains("`the archive needs <field> of <needed> bytes; this reader allows <allowed>`"));
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
fn spec_states_the_zstd_and_prior_rules() {
    let text = spec();
    for needle in [
        "## 10. Priors",
        "## 11. Test vectors",
        "gen_vectors -- --ignored",
        "`zstd-dict.prior`",
        "### What the reference decoder enforces for `zstd`",
        "`WindowTooLarge`",
        "`MissingPrior`",
        "`UnlistedPrior`",
        "`BadPriorList`",
        "`ZstdError`",
        "frame window exceeds\n   declared",
        "no file lookup",
        "| prior_list | variable |",
    ] {
        assert!(text.contains(needle), "spec lacks {needle:?}");
    }
    // The prior-list table is the one in the index table's neighbour, and the
    // index table names it.
    let index = lpk_format::index_layout_table();
    assert!(index.contains("prior_list"));
    assert!(lpk_format::prior_list_table().contains("prior_count"));
    assert!(lpk_format::prior_list_table().contains("32"));
}

#[test]
fn spec_states_the_lzma_rules() {
    let text = spec();
    for needle in [
        "### What the reference decoder enforces for `lzma`",
        "raw LZMA1 stream",
        "`lc + lp` <= 4",
        "`BadParams`, `lc`, `lp`, `pb` or `lc + lp`",
        "end-of-payload marker",
        "`trailing input`",
        "`truncated`",
        "`LzmaError`",
        "no prior ID",
        "- Implemented now: `store`, `zstd`, `lzma`.",
        "in exactly one of two groups",
        "`LZMA_LCLP_MAX`",
        "`range coder`",
        "is checked only when the
   end-of-payload marker is present",
        "never depends on the reader's
   `max_window`",
        "`lzma-basic.lpk`",
        "`lzma-multiblock.lpk`",
        "`lzma-props.lpk`",
    ] {
        assert!(text.contains(needle), "spec lacks {needle:?}");
    }
    assert!(lpk_format::primitive_table().contains("lc + lp <= 4"));
}

#[test]
fn spec_states_the_record_rules() {
    let text = spec();
    for needle in [
        "## 12. Reconstruction records",
        "### The `Records` frame (kind 3)",
        "### What each body verifies",
        "### What the reference decoder does with records",
        "`RecordOutOfRange`",
        "`UnknownRecordKind`",
        "`ReservedRecordBits`",
        "`RecordHashMismatch`",
        "`BadRecord`",
        "divided by 5",
        "it applies none",
        "its record count is 0",
        "`record_id`",
    ] {
        assert!(text.contains(needle), "spec lacks {needle:?}");
    }
    // Every kind and every body field appears in the rendered tables.
    let kinds = lpk_format::record_kind_table();
    for k in lpk_format::RecordKind::ALL {
        assert!(kinds.contains(&format!("| {} | `{}` |", k as u16, k.name())));
        assert!(kinds.contains(k.primitive().name()));
    }
    let layouts = lpk_format::record_layout_tables();
    for field in [
        "record_count",
        "body_hash",
        "primary_len",
        "nested_trailing_chunks",
        "gainmap_count",
        "lepton_version",
        "corrections",
        "library",
        "bit_depth",
        "color_type",
        "interlace",
        "filters",
        "line_ending",
        "padding",
        "endian",
        "bom",
        "framing",
        "member_count",
    ] {
        assert!(
            layouts.contains(&format!("| {field} |")),
            "tables lack {field}"
        );
    }
    assert_eq!(lpk_format::RECORD_COUNT_BOUND, 5);
}

#[test]
fn spec_states_the_trailer_constants() {
    let text = spec();
    assert_eq!(lpk_format::TRAILER_PAYLOAD_LEN, 96);
    assert_eq!(lpk_format::TRAILER_FRAME_LEN, 133);
    assert!(text.contains("fixed 96-byte payload"));
    assert!(text.contains("= 133 bytes"));
    assert!(text.contains("(165 bytes)"));
    assert_eq!(
        lpk_format::Header::LEN as u64 + lpk_format::TRAILER_FRAME_LEN,
        165
    );
}
