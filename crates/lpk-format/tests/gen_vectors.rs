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
}
