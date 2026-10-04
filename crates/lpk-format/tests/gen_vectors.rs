//! Regenerates the committed vectors:
//! `cargo test -p lpk-format --test gen_vectors -- --ignored`
#![allow(clippy::unwrap_used)]

mod common;

use common::{build_vector, trained_dictionary, vectors_dir, DICT_FILE, VECTORS};

#[test]
#[ignore = "writes tests/vectors; run on purpose"]
fn regenerate_vectors() {
    let dir = vectors_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let dict = trained_dictionary(5);
    std::fs::write(dir.join(DICT_FILE), &dict).unwrap();
    for name in VECTORS {
        let bytes = build_vector(name, Some(&dict));
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    for name in common::sealed::SEALED_VECTORS {
        std::fs::write(dir.join(name), common::sealed::build_sealed_vector(name)).unwrap();
    }
    std::fs::write(
        dir.join(common::journal::JOURNAL_VECTOR),
        &common::journal::build_journal(&|| common::journal::options(0), None).0[2],
    )
    .unwrap();
    std::fs::write(
        dir.join(common::sealed::SEALED_KEYFILE),
        common::sealed::sealed_keyfile_bytes(),
    )
    .unwrap();
    std::fs::write(dir.join("vectors.toml"), common::sealed::vectors_toml()).unwrap();
    write_recovery_vector();
}

/// Only the recovery vector and its damaged companion (the zstd and LZMA bytes depend on
/// library versions, these do not): `-- --ignored regenerate_recovery_vector`.
#[test]
#[ignore = "writes tests/vectors; run on purpose"]
fn regenerate_recovery_vector() {
    write_recovery_vector();
    std::fs::write(
        vectors_dir().join("vectors.toml"),
        common::sealed::vectors_toml(),
    )
    .unwrap();
}

fn write_recovery_vector() {
    use common::recovery::{
        build_recovery_vector, damaged_from, RECOVERY_DAMAGED, RECOVERY_VECTOR,
    };
    let good = build_recovery_vector();
    std::fs::write(vectors_dir().join(RECOVERY_VECTOR), &good).unwrap();
    std::fs::write(vectors_dir().join(RECOVERY_DAMAGED), damaged_from(&good)).unwrap();
}

/// Real payloads cut from the vectors as seeds of the fuzz targets (`fuzz/seeds/`):
/// `cargo test -p lpk-format --test gen_vectors -- --ignored regenerate_fuzz_seeds`.
#[test]
#[ignore = "writes fuzz/seeds; run on purpose"]
fn regenerate_fuzz_seeds() {
    use lpk_format::{Archive, BlockHeader, Frame, ReadFrame, ReadLimits, Resources};
    use std::io::Cursor;
    use std::path::Path;

    let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds");
    let put = |target: &str, name: &str, bytes: &[u8]| {
        let d = seeds.join(target);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(name), bytes).unwrap();
    };
    let get = |name: &str| std::fs::read(vectors_dir().join(name)).unwrap();
    let frame_at = |bytes: &[u8], at: u64| -> Vec<u8> {
        let mut r = &bytes[at as usize..];
        match Frame::read(&mut r, &ReadLimits::default()) {
            Ok(Some(ReadFrame::Known(f))) => f.payload,
            other => panic!("no frame at {at}: {other:?}"),
        }
    };
    let prefix = |offset: u64| (((offset.max(64) - 64) / 16) as u16).to_le_bytes();

    // One unmutated seed per vector for archive_mutate: byte 0 picks the vector.
    let order = [
        "zstd-basic",
        "zstd-multiblock",
        "zstd-dict",
        "zstd-window",
        "lzma-basic",
        "lzma-multiblock",
        "lzma-props",
        "sealed-aes",
        "sealed-xchacha",
        "sealed-listable",
        "sealed-keyfile",
        "journal-3gen",
        "recovery-groups",
    ];
    for (i, n) in order.iter().enumerate() {
        put("archive_mutate", &format!("plain-{n}"), &[i as u8]);
    }

    for name in [
        "zstd-basic.lpk",
        "lzma-basic.lpk",
        "journal-3gen.lpk",
        "recovery-groups.lpk",
    ] {
        let bytes = get(name);
        let stem = name.trim_end_matches(".lpk");
        let mut a = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
        put("entry_table", stem, &a.entries().unwrap());
        put("chunk_table", stem, &a.index().chunk_table);
        let t = *a.trailer();
        let mut idx = prefix(t.index_offset).to_vec();
        idx.extend(frame_at(&bytes, t.index_offset));
        put("index_parse", stem, &idx);
        put("frame_read", stem, &bytes[32..]);
        put("header_read", stem, &bytes[..32]);
        put("trailer_read_tail", stem, &bytes);
        let b = a.index().blocks[0];
        let payload = frame_at(&bytes, b.frame_offset);
        put("block_header_and_decode", stem, &payload);
        let (h, used) = BlockHeader::parse(&payload, 0, 0).unwrap();
        let expected = h.plain_len.div_ceil(16) as u16;
        let mut body = expected.to_le_bytes().to_vec();
        body.extend(&payload[used..]);
        if name == "zstd-basic.lpk" {
            let mut z = vec![20];
            z.extend(&body);
            put("zstd_decode", stem, &z);
        }
        if name == "lzma-basic.lpk" {
            let mut l = h.graph.steps[0].params.clone();
            l.extend(&body);
            put("lzma_decode", stem, &l);
        }
        if let Some(r) = a.recovery_frames().first() {
            let mut f = prefix(r.offset).to_vec();
            f.extend(frame_at(&bytes, r.offset));
            put("recovery_frame", stem, &f);
        }
        put("archive_open", stem, &bytes);
    }
    let sealed = get("sealed-aes.lpk");
    put("key_slot", "sealed-aes", &frame_at(&sealed, 32));
    put("archive_open", "sealed-aes", &sealed);
}
