//! Non-ASCII names everywhere (E2-23 (a)): a tree whose file and directory names carry emoji
//! (outside the BMP, and a ZWJ sequence), CJK (Japanese, Chinese, Korean), combining marks, two
//! names that differ only by Unicode normalisation form, a 200-character name and a path above
//! 260 characters goes through `lpk a`, `lpk l` (the escaped listing and `--json`), `lpk x`
//! (the tree equal by BLAKE3, the names byte-equal as UTF-8), `lpk export` to ZIP (the UTF-8
//! flag set; read back with the `zip` crate) and to 7z (read back with `sevenz-rust2`), and the
//! independent decoder `lpk-check`. A name the file system refuses is skipped with a message
//! naming it; everything else must survive unchanged.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn lpk(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lpk"))
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The names of the tree, `/`-separated and relative to its root; a directory ends with `/`.
fn names() -> Vec<String> {
    let long_name = {
        // 200 characters: 190 ASCII letters and ten two-byte characters (210 bytes, under the
        // 255-byte limit of ext4 and NTFS's 255 UTF-16 units).
        let mut s = String::new();
        for i in 0..190 {
            s.push((b'a' + (i % 26) as u8) as char);
        }
        s.push_str("éàüößñçø€");
        s.push('日');
        assert_eq!(s.chars().count(), 200);
        s
    };
    let deep = {
        // Eleven directories of 24 characters each put the relative path above 260 characters.
        let mut p = String::new();
        for i in 0..11 {
            p.push_str(&format!("deep-directory-level-{i:02}/"));
        }
        assert!(p.chars().count() > 260, "{}", p.chars().count());
        p
    };
    vec![
        "😀 emoji/".to_string(),
        "😀 emoji/📁 folder/".to_string(),
        "😀 emoji/📁 folder/snapshot 🚀.txt".to_string(),
        "😀 emoji/👨‍👩‍👧 family.txt".to_string(),
        "😀 emoji/empty 🗂️/".to_string(),
        "日本語/".to_string(),
        "日本語/ファイル.txt".to_string(),
        "中文/".to_string(),
        "中文/文件.bin".to_string(),
        "한국어/".to_string(),
        "한국어/파일.txt".to_string(),
        "marks/".to_string(),
        "marks/café.txt".to_string(),
        "marks/cafe\u{301}.txt".to_string(),
        "marks/Zalgo ṫ̖̗e̊̊x̀̓t̆.txt".to_string(),
        "marks/Ωmega-Ω.txt".to_string(),
        "marks/привет мир.txt".to_string(),
        format!("long/{long_name}"),
        format!("{deep}end 🏁.txt"),
    ]
}

fn content(name: &str) -> Vec<u8> {
    let mut v = format!("content of {name}\n").into_bytes();
    let mut s = 0x9E37_79B9u32 ^ v.len() as u32;
    for _ in 0..(v.len() * 7) {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        v.push((s >> 24) as u8);
    }
    v
}

/// A path under `root` from a `/`-separated name, pushed one component at a time (a `/` inside
/// one `join` argument is not converted under a `\\?\` prefix on Windows).
fn under(root: &Path, name: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in name.trim_end_matches('/').split('/') {
        p.push(c);
    }
    p
}

/// Create the tree; returns the names the file system took (a refused name is reported and left
/// out, with everything under it).
fn make_tree(root: &Path) -> Vec<String> {
    let mut made = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for name in names() {
        if refused.iter().any(|r| name.starts_with(r.as_str())) {
            continue;
        }
        let path = under(root, &name);
        let r = if name.ends_with('/') {
            fs::create_dir_all(&path)
        } else {
            fs::create_dir_all(path.parent().unwrap())
                .and_then(|()| fs::write(&path, content(&name)))
        };
        match r {
            Ok(()) => made.push(name),
            Err(e) => {
                eprintln!("skipped: the file system refused the name {name:?}: {e}");
                refused.push(name);
            }
        }
    }
    made
}

/// Every path under `root` (`/`-separated, read back from the file system as UTF-8), with a
/// file's BLAKE3 or `None` for a directory.
fn tree(root: &Path) -> BTreeMap<String, Option<[u8; 32]>> {
    fn walk(dir: &Path, base: &str, out: &mut BTreeMap<String, Option<[u8; 32]>>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name();
            let name = name
                .to_str()
                .unwrap_or_else(|| panic!("a name read back is not UTF-8: {name:?}"));
            let rel = format!("{base}{name}");
            if e.path().is_dir() {
                out.insert(format!("{rel}/"), None);
                walk(&e.path(), &format!("{rel}/"), out);
            } else {
                let data = fs::read(e.path()).unwrap();
                out.insert(rel, Some(*blake3::hash(&data).as_bytes()));
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, "", &mut m);
    m
}

/// The expected tree from the names made: every directory (the implicit parents included) as
/// `name/`, files by their content.
fn expected(made: &[String]) -> BTreeMap<String, Option<[u8; 32]>> {
    let mut m = BTreeMap::new();
    for n in made {
        for (i, c) in n.char_indices() {
            if c == '/' {
                m.insert(n[..=i].to_string(), None);
            }
        }
        if !n.ends_with('/') {
            m.insert(n.clone(), Some(*blake3::hash(&content(n)).as_bytes()));
        }
    }
    m
}

/// The general-purpose flags of every central-directory record of a ZIP, by name.
fn zip_flags(path: &Path) -> BTreeMap<String, u16> {
    let data = fs::read(path).unwrap();
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i + 46 <= data.len() {
        if data[i..i + 4] == [b'P', b'K', 1, 2] {
            let flags = u16::from_le_bytes([data[i + 8], data[i + 9]]);
            let name_len = u16::from_le_bytes([data[i + 28], data[i + 29]]) as usize;
            let extra_len = u16::from_le_bytes([data[i + 30], data[i + 31]]) as usize;
            let comment_len = u16::from_le_bytes([data[i + 32], data[i + 33]]) as usize;
            let name = String::from_utf8(data[i + 46..i + 46 + name_len].to_vec()).unwrap();
            out.insert(name, flags);
            i += 46 + name_len + extra_len + comment_len;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn non_ascii_and_long_names_survive_every_command_unchanged() {
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let made = make_tree(&src);
    assert!(
        made.iter().any(|n| !n.is_ascii()),
        "the file system refused every non-ASCII name; nothing to test"
    );
    let want = expected(&made);
    assert_eq!(
        tree(&src),
        want,
        "the tree as written and as read back differ"
    );

    // a
    let arch = work.path().join("names.lpk");
    let a = lpk(&["a", arch.to_str().unwrap(), src.to_str().unwrap()]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));

    // l: the escaped listing is the name itself (no control characters), tab-separated columns.
    let l = lpk(&["l", arch.to_str().unwrap()]);
    assert_eq!(l.status.code(), Some(0), "{}", text(&l.stderr));
    let listing = String::from_utf8(l.stdout).expect("the listing is UTF-8");
    let listed: Vec<String> = listing
        .lines()
        .map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            assert_eq!(cols.len(), 4, "{line}");
            let path = cols[3].to_string();
            if cols[0] == "Directory" {
                format!("{path}/")
            } else {
                path
            }
        })
        .collect();
    let mut sorted = listed.clone();
    sorted.sort();
    let want_names: Vec<String> = want.keys().cloned().collect();
    assert_eq!(
        sorted, want_names,
        "the listing's names differ from the tree's"
    );
    // l --json: the path field is the name as JSON decodes it.
    let j = lpk(&["l", "--json", arch.to_str().unwrap()]);
    assert_eq!(j.status.code(), Some(0), "{}", text(&j.stderr));
    let mut json_names: Vec<String> = text(&j.stdout)
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let path = v["path"].as_str().unwrap().to_string();
            if v["kind"] == "Directory" {
                format!("{path}/")
            } else {
                path
            }
        })
        .collect();
    json_names.sort();
    assert_eq!(
        json_names, want_names,
        "the JSON listing's names differ from the tree's"
    );

    // x: the tree equal by BLAKE3, the names byte-equal as UTF-8.
    let out = work.path().join("x");
    let x = lpk(&["x", arch.to_str().unwrap(), out.to_str().unwrap()]);
    assert_eq!(x.status.code(), Some(0), "{}", text(&x.stderr));
    assert_eq!(tree(&out), want, "the extracted tree differs");
    for n in want.keys() {
        assert!(
            under(&out, n).exists(),
            "extracted name not found byte-for-byte: {n:?}"
        );
    }

    // export to ZIP: the UTF-8 flag (bit 11) set on every entry; the crate's reader gives the
    // names back unchanged.
    let zip_path = work.path().join("names.zip");
    let e = lpk(&["export", arch.to_str().unwrap(), zip_path.to_str().unwrap()]);
    assert_eq!(e.status.code(), Some(0), "{}", text(&e.stderr));
    let flags = zip_flags(&zip_path);
    assert_eq!(flags.len(), want.len(), "{flags:?}");
    for (name, f) in &flags {
        // An ASCII name is the same bytes with or without the flag; a non-ASCII one needs it.
        assert!(
            name.is_ascii() || f & (1 << 11) != 0,
            "UTF-8 flag not set on {name:?}"
        );
        assert!(want.contains_key(name), "unexpected ZIP entry {name:?}");
    }
    let mut z = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
    let mut from_zip = BTreeMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        if f.is_dir() {
            from_zip.insert(name, None);
        } else {
            let mut d = Vec::new();
            f.read_to_end(&mut d).unwrap();
            from_zip.insert(name, Some(*blake3::hash(&d).as_bytes()));
        }
    }
    assert_eq!(from_zip, want, "the ZIP's names or contents differ");

    // export to 7z: read back with sevenz-rust2.
    let sz_path = work.path().join("names.7z");
    let e = lpk(&["export", arch.to_str().unwrap(), sz_path.to_str().unwrap()]);
    assert_eq!(e.status.code(), Some(0), "{}", text(&e.stderr));
    let mut rd = sevenz_rust2::ArchiveReader::new(
        fs::File::open(&sz_path).unwrap(),
        sevenz_rust2::Password::empty(),
    )
    .unwrap();
    let mut from_7z = BTreeMap::new();
    rd.for_each_entries(|e, data| {
        let name = e.name().replace('\\', "/");
        if e.is_directory() {
            from_7z.insert(format!("{name}/"), None);
        } else {
            let mut d = Vec::new();
            data.read_to_end(&mut d).unwrap();
            from_7z.insert(name, Some(*blake3::hash(&d).as_bytes()));
        }
        Ok(true)
    })
    .unwrap();
    assert_eq!(from_7z, want, "the 7z's names or contents differ");

    // lpk-check, the independent decoder: the entries' paths and bytes, and its extraction.
    let mut a = lpk_check::Archive::open(fs::read(&arch).unwrap(), lpk_check::Options::default())
        .unwrap_or_else(|e| panic!("lpk-check open: {e}"));
    let entries = a
        .entries()
        .unwrap_or_else(|e| panic!("lpk-check entries: {e}"));
    let mut from_check = BTreeMap::new();
    for e in &entries {
        match e.kind {
            lpk_check::entries::EntryKind::Directory => {
                from_check.insert(format!("{}/", e.path), None);
            }
            _ => {
                let d = a
                    .read_file(e)
                    .unwrap_or_else(|x| panic!("lpk-check read {:?}: {x}", e.path));
                from_check.insert(e.path.clone(), Some(*blake3::hash(&d).as_bytes()));
            }
        }
    }
    assert_eq!(from_check, want, "lpk-check's names or contents differ");
    let check_out = work.path().join("check-x");
    fs::create_dir_all(&check_out).unwrap();
    let (done, first_err) = lpk_check::extract::extract_all(&mut a, &check_out)
        .unwrap_or_else(|e| panic!("lpk-check extract: {e}"));
    assert!(first_err.is_none(), "lpk-check extract: {first_err:?}");
    assert_eq!(done.len(), want.len());
    assert_eq!(tree(&check_out), want, "lpk-check's extracted tree differs");
}
