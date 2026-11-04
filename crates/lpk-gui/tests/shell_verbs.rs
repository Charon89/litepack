//! E6-2: the Explorer menu's binary response files through the command line's parser
//! (`lpk_gui::cli::parse_in`) and the reader's deletion rule; the bytes are the committed test
//! vector that `lpk-shell`'s writer test produces exactly.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use lpk_gui::cli::{parse_in, Verb};
use lpk_gui::shellfile;

const HEX: &str = "0123456789abcdef0123456789abcdef";

fn vector() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/shell-v1.verbs")).unwrap()
}

fn at(p: &Path) -> OsString {
    OsString::from(format!("@{}", p.display()))
}

fn verbs_dir(root: &Path) -> PathBuf {
    let verbs = root.join("Verbs");
    std::fs::create_dir_all(&verbs).unwrap();
    verbs
}

#[test]
fn a_menu_response_file_becomes_the_verb_and_is_listed_for_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let verbs = verbs_dir(dir.path());
    let file = verbs.join(format!("shell-{HEX}.verbs"));
    std::fs::write(&file, vector()).unwrap();
    let s = parse_in([at(&file)], None, Some(&verbs)).unwrap();
    assert_eq!(s.shell_files, vec![file.clone()]);
    let Some(Verb::Create { inputs, name }) = s.verb else {
        panic!("a create verb")
    };
    assert_eq!(name.as_deref(), Some("Docs.lpk"));
    assert_eq!(inputs.len(), 3);
    assert_eq!(inputs[0], PathBuf::from("C:\\data\\a b.txt"));
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let lone: Vec<u16> = inputs[2].as_os_str().encode_wide().collect();
        assert!(lone.contains(&0xD800), "the unpaired surrogate round-trips");
    }
    assert!(file.exists(), "parse reads; the instance that acts deletes");
    assert!(shellfile::delete(&file, &verbs));
    assert!(!file.exists());
}

#[test]
fn any_other_at_file_stays_the_text_format_even_in_that_folder() {
    let dir = tempfile::tempdir().unwrap();
    let verbs = verbs_dir(dir.path());
    // The binary bytes under another name are not read as binary (and are not UTF-8 text).
    let odd = verbs.join("args.txt");
    std::fs::write(&odd, vector()).unwrap();
    assert!(parse_in([at(&odd)], None, Some(&verbs)).is_err());
    assert!(!shellfile::delete(&odd, &verbs), "never another name");
    assert!(odd.exists());
    // A text file named as the shell names its files is read as binary, and refused.
    let named = verbs.join(format!("shell-{HEX}.verbs"));
    std::fs::write(&named, "--open\nx.lpk\n").unwrap();
    let e = parse_in([at(&named)], None, Some(&verbs)).unwrap_err();
    assert!(e.contains("magic") || e.contains("not a LitePack"), "{e}");
    // The same file outside the folder is text.
    let s = parse_in([at(&named)], None, Some(&dir.path().join("elsewhere"))).unwrap();
    assert_eq!(s.verb, Some(Verb::Open(PathBuf::from("x.lpk"))));
    assert!(s.shell_files.is_empty());
    assert!(named.exists());
}

#[test]
fn a_path_that_looks_like_a_flag_stays_a_path() {
    let dir = tempfile::tempdir().unwrap();
    let verbs = verbs_dir(dir.path());
    let file = verbs.join(format!("shell-{HEX}.verbs"));
    let mut bytes = b"LPKVERBS".to_vec();
    bytes.extend([1, 0, 6, 0]);
    bytes.extend("--open".encode_utf16().flat_map(u16::to_le_bytes));
    bytes.extend([0, 0, 0, 0, 1, 0, 0, 0]);
    bytes.extend(8u32.to_le_bytes());
    bytes.extend("--create".encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&file, bytes).unwrap();
    let s = parse_in([at(&file)], None, Some(&verbs)).unwrap();
    assert_eq!(s.verb, Some(Verb::Open(PathBuf::from("--create"))));
}

#[test]
fn the_verbs_that_take_no_name_refuse_one() {
    let dir = tempfile::tempdir().unwrap();
    let verbs = verbs_dir(dir.path());
    let file = verbs.join(format!("shell-{HEX}.verbs"));
    let mut bytes = b"LPKVERBS".to_vec();
    bytes.extend([1, 0, 6, 0]);
    bytes.extend("--test".encode_utf16().flat_map(u16::to_le_bytes));
    bytes.extend(5u32.to_le_bytes());
    bytes.extend("a.lpk".encode_utf16().flat_map(u16::to_le_bytes));
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend("x".encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&file, bytes).unwrap();
    assert!(parse_in([at(&file)], None, Some(&verbs)).is_err());
}
