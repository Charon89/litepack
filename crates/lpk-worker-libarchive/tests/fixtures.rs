//! E4-4's fixtures read by the libarchive reader directly (no sandbox): every member listed and
//! streamed, BLAKE3-equal to the manifest; each format's fixture truncated or garbled refused
//! with a named error, never a crash. Skipped with a message while libarchive is not built.
#![allow(clippy::unwrap_used)]

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use lpk_proto::frame::RecordKind;
use lpk_proto::{ErrorCode, OpenArgs};

mod common;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// The further volumes of a set fixture.
fn more_volumes(name: &str) -> Vec<File> {
    let names: &[&str] = match name {
        "rar5-vol.part1.rar" => &["rar5-vol.part2.rar", "rar5-vol.part3.rar"],
        "rar4-vol.rar" => &["rar4-vol.r00", "rar4-vol.r01"],
        _ => &[],
    };
    names
        .iter()
        .map(|n| File::open(fixture(n)).unwrap())
        .collect()
}

fn skipped(what: &str) -> bool {
    if !lpk_libarchive_sys::BUILT {
        eprintln!("SKIPPED {what}: libarchive is not built (vendor/libarchive-3.8.9 absent)");
        return true;
    }
    false
}

#[test]
fn the_manifest_covers_every_expected_file() {
    let m = common::manifest();
    for (_, files) in common::CASES {
        for f in *files {
            assert!(m.contains_key(*f), "{f} missing from manifest.b3");
        }
    }
}

#[test]
fn every_fixture_lists_and_reads_back_blake3_equal() {
    if skipped("every_fixture_lists_and_reads_back_blake3_equal") {
        return;
    }
    let m = common::manifest();
    for (archive, expected) in common::CASES {
        let args = OpenArgs {
            format: common::wire(archive),
            name: archive.as_bytes(),
            password: None,
        };
        let first = File::open(fixture(archive)).unwrap();
        let mut r = lpk_worker_libarchive::open(first, &more_volumes(archive), &args)
            .unwrap_or_else(|e| panic!("{archive}: {e:?}"));
        let (records, more) = r.entries(0, 1 << 20).unwrap();
        assert!(!more);
        for want in *expected {
            let i = records
                .iter()
                .position(|rec| rec.kind == RecordKind::File && rec.name == want.as_bytes())
                .unwrap_or_else(|| panic!("{archive}: {want} not listed"));
            let mut got = Vec::new();
            r.read_member(i as u64, &mut |d: &[u8]| {
                got.extend_from_slice(d);
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{archive}: {want}: {e:?}"));
            assert_eq!(records[i].size, got.len() as u64, "{archive}: {want}");
            assert_eq!(
                blake3::hash(&got).to_hex().to_string(),
                m[*want],
                "{archive}: {want}"
            );
        }
    }
}

#[test]
fn malformed_inputs_are_named_errors() {
    if skipped("malformed_inputs_are_named_errors") {
        return;
    }
    for (archive, _) in common::CASES {
        let bytes = std::fs::read(fixture(archive)).unwrap();
        for cut in [bytes.len() / 2, bytes.len() * 9 / 10] {
            let mut variants = vec![bytes[..cut].to_vec()];
            let mut garbled = bytes.clone();
            for b in garbled.iter_mut().skip(cut / 3).step_by(97) {
                *b ^= 0x5A;
            }
            variants.push(garbled);
            for v in variants {
                let mut f = tempfile::tempfile().unwrap();
                f.write_all(&v).unwrap();
                let args = OpenArgs {
                    format: common::wire(archive),
                    name: archive.as_bytes(),
                    password: None,
                };
                let Ok(mut r) = lpk_worker_libarchive::open(f, &[], &args) else {
                    continue;
                };
                let Ok((records, _)) = r.entries(0, 1 << 20) else {
                    continue;
                };
                for i in 0..records.len() {
                    if let Err(e) = r.read_member(i as u64, &mut |_| Ok(())) {
                        assert!(
                            matches!(
                                e.code,
                                ErrorCode::Corrupt | ErrorCode::Unsupported | ErrorCode::Io
                            ),
                            "{archive}: {e:?}"
                        );
                    }
                }
            }
        }
    }
}

/// Regenerates `manifest.b3` from the generator's source tree:
/// `LPK_FIXTURE_SRC=<scratch>/src cargo test -p lpk-worker-libarchive --test fixtures -- --ignored write_manifest`.
#[test]
#[ignore = "writes tests/fixtures/manifest.b3 from LPK_FIXTURE_SRC"]
fn write_manifest() {
    let src = PathBuf::from(std::env::var_os("LPK_FIXTURE_SRC").expect("LPK_FIXTURE_SRC"));
    let mut out = String::from(
        "# BLAKE3 (hex), size, path of the files the fixtures hold (tests/fixtures/make_fixtures.py's tree)\n",
    );
    for rel in common::TREE {
        let data = std::fs::read(src.join(rel)).unwrap();
        out.push_str(&format!(
            "{}  {}  {}\n",
            blake3::hash(&data).to_hex(),
            data.len(),
            rel
        ));
    }
    std::fs::write(fixture("manifest.b3"), out).unwrap();
}
