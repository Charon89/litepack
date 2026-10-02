//! Golden hashes: one fixed small input per in-process derivation and the BLAKE3 of the bytes it
//! must produce. They pin the output across operating systems: if a different zlib, PNG, JPEG or
//! cipher backend (or a different build of one) produced other bytes on the Linux CI job than on
//! Windows, the corpus manifest would differ between machines, and this test fails first.
//!
//! When an output changes on purpose (a deliberate encoder change), the failure message prints
//! the new table; paste it into `EXPECTED` and say why in the commit.

use super::jpegenc::{encode, tests_support::picture};
use super::jpegs::{bmp_size, encode_bmp, encode_png};
use super::testutil::{put, read_all, run, source};
use super::wav::wav_bytes;
use super::zips::{write_zip, Encoder, Entry};
use crate::corpus::registry::{EncryptedRandomSpec, SmallFilesSpec, SourceSpec};

fn h(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

fn text(n: usize, salt: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| {
            format!(
                "line {i} of file {salt}: the quick brown fox {}\n",
                i * 31 % 17
            )
            .into_bytes()
        })
        .collect()
}

/// Hash of every file of a source directory: names and contents, in path order.
fn dir_hash(root: &std::path::Path, class: &str, id: &str) -> String {
    let mut all = Vec::new();
    for (name, data) in read_all(root, class, id) {
        all.extend_from_slice(name.as_bytes());
        all.push(0);
        all.extend_from_slice(&(data.len() as u64).to_le_bytes());
        all.extend_from_slice(&data);
    }
    h(&all)
}

fn computed() -> Vec<(&'static str, String)> {
    let mut got = Vec::new();
    let rgb = picture(64, 48);
    got.push((
        "jpeg-baseline-q90",
        h(&encode(&rgb, 64, 48, 90, false).expect("jpeg")),
    ));
    got.push((
        "jpeg-progressive-q85",
        h(&encode(&rgb, 64, 48, 85, true).expect("jpeg")),
    ));
    got.push(("png", h(&encode_png(&rgb, 64, 48).expect("png"))));
    let bmp = encode_bmp(&rgb, 64, 48);
    assert_eq!(bmp.len() as u64, bmp_size(64, 48));
    got.push(("bmp", h(&bmp)));

    let entries: Vec<Entry> = (0..5)
        .map(|i| Entry {
            name: format!("dir/f{i}.txt"),
            data: text(80 + 20 * i, i),
        })
        .collect();
    for (name, enc) in [
        ("zip-zlib-1", Encoder::Zlib(1)),
        ("zip-zlib-6", Encoder::Zlib(6)),
        ("zip-zlib-9", Encoder::Zlib(9)),
        ("zip-miniz-6", Encoder::Miniz(6)),
    ] {
        got.push((name, h(&write_zip(&entries, enc).expect("zip"))));
    }

    let samples: Vec<i32> = (0..500).map(|i| (i * 37) % 60000 - 30000).collect();
    got.push((
        "wav-16-stereo",
        h(&wav_bytes(&samples, 2, 44100, 16).expect("wav")),
    ));

    let d = tempfile::tempdir().expect("tmp");
    let enc = source(
        "enc",
        "encrypted-random",
        &[],
        SourceSpec::EncryptedRandom(EncryptedRandomSpec { bytes: 4096 }),
    );
    run(d.path(), &enc, &[]).expect("encrypted-random");
    for (name, data) in read_all(d.path(), "encrypted-random", "enc") {
        got.push((
            if name == "plain.bin" {
                "random-plain-4096"
            } else {
                "random-aes-4096"
            },
            h(&data),
        ));
    }

    let d = tempfile::tempdir().expect("tmp");
    put(d.path(), "logs-text", "logs", "a.log", &text(400, 7));
    let mut csv = String::from("id,name,amount\n");
    for n in 0..300 {
        csv.push_str(&format!("{n},\"Doe, J {n}\",{}.{:02}\n", n * 3, n % 100));
    }
    put(d.path(), "logs-text", "taxi", "t.csv", csv.as_bytes());
    let sf = source(
        "small",
        "small-files",
        &["logs-text"],
        SourceSpec::SmallFiles(SmallFilesSpec {
            count: 60,
            max_file_bytes: 1500,
            json_percent: 30,
            csv_percent: 20,
            from: vec![],
        }),
    );
    run(
        d.path(),
        &sf,
        &[("logs-text", "logs"), ("logs-text", "taxi")],
    )
    .expect("small-files");
    got.push(("small-files-60", dir_hash(d.path(), "small-files", "small")));
    got
}

const EXPECTED: &[(&str, &str)] = &[
    (
        "jpeg-baseline-q90",
        "3f73b4954d836172e53285dcba492fd75a610d4fe9b31f136e1079bf52825b60",
    ),
    (
        "jpeg-progressive-q85",
        "ac7e60e9057f7288da8f7074673b0124ce404b301b54a9df2dcd4c5d4b680d49",
    ),
    (
        "png",
        "083ae784c3e67fc7de75370da0b6dad8cd6a3497c32d975abd6830d3e0c6004e",
    ),
    (
        "bmp",
        "ade8d42c9b6e6fc634d06b0f808510409e7121dc4ef075cce66e9524c54e42f1",
    ),
    (
        "zip-zlib-1",
        "62ac4eba04263809c1a17a1665ddb37f0a43893918bc26839498b6554a82f50b",
    ),
    (
        "zip-zlib-6",
        "f913deab303b636d2b4533678bf4bf9eb2e01eaa54e670e0d5e260df8b2a4cc1",
    ),
    (
        "zip-zlib-9",
        "fe1355d1f433ce34f72960acb9d3722fdd8a6734a51935f127cd0185244b43b7",
    ),
    (
        "zip-miniz-6",
        "991a6b47e91266f55d2b350e9c3ed46cba75a31d91381a8943ee9a01a91a0d0a",
    ),
    (
        "wav-16-stereo",
        "1c582635bc57955d44e8ebec65ede1e9157e9026c97d180c8dbc3f07ef3d3ef9",
    ),
    (
        "random-aes-4096",
        "a0dc76c8727d96365a476a2b56c5ba49fbee7b562e80c6b299249e70521d3d0b",
    ),
    (
        "random-plain-4096",
        "5dd3059842009e5c66d2a41de4ac438661900a96c40182518943cdb4a910a155",
    ),
    (
        "small-files-60",
        "180016d0a49da00c1d242ae871732da905c7642162e73b6901143858d89fddc4",
    ),
];

#[test]
fn outputs_match_the_golden_hashes() {
    let got = computed();
    let want: Vec<(&str, &str)> = EXPECTED.to_vec();
    let got_ref: Vec<(&str, &str)> = got.iter().map(|(a, b)| (*a, b.as_str())).collect();
    if got_ref != want {
        let table: String = got
            .iter()
            .map(|(n, v)| format!("    (\"{n}\", \"{v}\"),\n"))
            .collect();
        panic!("golden hashes differ; new table:\nconst EXPECTED: &[(&str, &str)] = &[\n{table}];");
    }
}
