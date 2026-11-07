//! The fixtures, what each must yield, and the BLAKE3 manifest of the expected contents
//! (`tests/fixtures/manifest.b3`, written by `fixtures::write_manifest` from the generator's
//! source tree).

use std::collections::HashMap;
use std::path::Path;

/// Every file of the generated tree.
pub const TREE: &[&str] = &[
    "hello.txt",
    "Mixed Case Name.txt",
    "sub/nested.txt",
    "data.bin",
];

/// The non-ASCII names fixtures hold (R204's proof; contents in `make_fixtures.py`'s `NAMES`).
pub const NAMES_ONE: &[&str] = &["sub/caf\u{e9} \u{fc} \u{65e5}\u{672c}.txt"];
pub const NAMES_BOTH: &[&str] = &[
    "sub/caf\u{e9} \u{fc} \u{65e5}\u{672c}.txt",
    "na\u{ef}ve/\u{fc}.txt",
];

/// (archive, wire format, files it must yield).
pub const CASES: &[(&str, &[&str])] = &[
    ("rar5.rar", TREE),
    ("rar4.rar", TREE),
    ("rar5-vol.part1.rar", TREE),
    ("rar4-vol.rar", &["hello.txt", "data.bin"]),
    ("rar5-sfx.sfx", TREE),
    ("cab-stored.cab", TREE),
    ("cab-mszip.cab", TREE),
    ("cab-lzx.cab", TREE),
    ("iso-rockridge.iso", TREE),
    ("iso-joliet.iso", TREE),
    ("fixture.a", &["hello.txt", "data.bin"]),
    ("fixture.cpio", TREE),
    ("names-rar5.rar", NAMES_BOTH),
    ("names-rr.iso", NAMES_ONE),
    ("names-joliet.iso", NAMES_ONE),
    ("names.lzh", NAMES_BOTH),
    ("names.cpio", NAMES_ONE),
    ("names.rpm", NAMES_ONE),
    ("level0.lzh", TREE),
    ("level1.lzh", TREE),
    ("fixture.rpm", TREE),
    ("hello.txt.uue", &["hello.txt"]),
    ("hello.txt.Z", &["hello.txt"]),
    ("small.tar.Z", &["hello.txt"]),
];

/// The wire format the host detects for each fixture.
#[allow(dead_code)]
pub fn wire(archive: &str) -> &'static str {
    match archive.rsplit('.').next().unwrap_or("") {
        "rar" | "sfx" => "rar",
        "cab" => "cab",
        "iso" => "iso",
        "a" => "ar",
        "cpio" => "cpio",
        "lzh" => "lzh",
        "rpm" => "rpm",
        "uue" => "uue",
        "Z" => "z",
        _ => "",
    }
}

/// path -> BLAKE3 hex.
pub fn manifest() -> HashMap<String, String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/manifest.b3");
    let text = std::fs::read_to_string(&p).unwrap();
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (hash, rest) = l.split_once("  ").unwrap();
            let (_size, path) = rest.split_once("  ").unwrap();
            (path.to_string(), hash.to_string())
        })
        .collect()
}
