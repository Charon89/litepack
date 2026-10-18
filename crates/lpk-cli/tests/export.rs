//! `lpk export` (E3-6) end to end: ZIP and 7z round trips through the crates' own readers
//! and through `7z t` / `7z x` and `unzip -t` when installed (skipped and recorded on stderr
//! otherwise, never a failure); the methods; AES-256 with the archive's password; symlinks,
//! directories, empty files, mtimes and modes; the refusals and their exit codes; and, ignored,
//! a file above 4 GiB written as Zip64.
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

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
    let mut out = Vec::new();
    while out.len() < len {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.extend_from_slice(WORDS[(s % 8) as usize].as_bytes());
        out.push(b' ');
    }
    out.truncate(len);
    out
}

fn random(seed: u64, len: usize) -> Vec<u8> {
    let mut s = 0x1234_5678_9ABC_DEF1u64 ^ seed;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

/// A tree with directories (one empty), an empty file, text, random bytes, a copy (dedup:
/// the copy's chunks point back to the original's block) and a non-ASCII name.
fn make_tree(root: &Path) {
    fs::create_dir_all(root.join("docs/deep")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::write(root.join("a.txt"), prose(1, 300_000)).unwrap();
    fs::write(root.join("docs/b.md"), prose(2, 70_000)).unwrap();
    fs::write(root.join("docs/deep/r.bin"), random(1, 150_000)).unwrap();
    fs::write(root.join("docs/deep/s.bin"), random(2, 900_000)).unwrap();
    fs::write(root.join("docs/copy.txt"), prose(1, 300_000)).unwrap();
    fs::write(root.join("zero"), b"").unwrap();
    fs::write(root.join("ünïcode名.txt"), b"named in UTF-8\n").unwrap();
}

/// Every path under `root` (`/`-separated), with a file's content hash or `None` for a
/// directory.
fn tree(root: &Path) -> BTreeMap<String, Option<[u8; 32]>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Option<[u8; 32]>>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(rel, None);
                walk(&p, base, out);
            } else {
                out.insert(rel, Some(*blake3::hash(&fs::read(&p).unwrap()).as_bytes()));
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

/// The archive of `make_tree` in a fresh work directory: (work, source, archive).
fn archived(extra: &[&str]) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    make_tree(&src);
    let arch = work.path().join("t.lpk");
    let mut args = vec!["a", arch.to_str().unwrap(), src.to_str().unwrap()];
    args.extend_from_slice(extra);
    let a = lpk(&args);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    (work, src, arch)
}

/// A program on `PATH` by any of `names`, else under an environment variable's directory
/// (`hints`: variable, relative path), else `None`.
fn find_tool(names: &[&str], hints: &[(&str, &str)]) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let exts: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };
    for d in &dirs {
        for n in names {
            for x in exts {
                let p = d.join(format!("{n}{x}"));
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    for (var, rel) in hints {
        if let Some(base) = std::env::var_os(var) {
            let p = Path::new(&base).join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn seven_zip() -> Option<PathBuf> {
    let found = if cfg!(windows) {
        find_tool(
            &["7z"],
            &[
                ("ProgramFiles", "7-Zip/7z.exe"),
                ("ProgramFiles(x86)", "7-Zip/7z.exe"),
                ("LOCALAPPDATA", "Programs/7-Zip/7z.exe"),
            ],
        )
    } else {
        find_tool(&["7zz", "7z"], &[])
    };
    if found.is_none() {
        eprintln!("skipped: 7z not installed (the external 7-Zip round trip)");
    }
    found
}

fn unzip() -> Option<PathBuf> {
    let found = find_tool(&["unzip"], &[]);
    if found.is_none() {
        eprintln!("skipped: unzip not installed (the external Info-ZIP test)");
    }
    found
}

/// `7z t` (with the password when given), then `7z x` into a fresh directory whose tree
/// must equal `want`.
fn seven_zip_round_trip(
    archive: &Path,
    password: Option<&str>,
    want: &BTreeMap<String, Option<[u8; 32]>>,
) {
    let Some(exe) = seven_zip() else { return };
    let pw = password.map(|p| format!("-p{p}"));
    let mut t = Command::new(&exe);
    t.arg("t").arg("-bd").arg("-y");
    if let Some(pw) = &pw {
        t.arg(pw);
    }
    let r = t.arg(archive).output().unwrap();
    assert_eq!(
        r.status.code(),
        Some(0),
        "7z t: {}{}",
        text(&r.stdout),
        text(&r.stderr)
    );
    let out = tempfile::tempdir().unwrap();
    let mut x = Command::new(&exe);
    x.arg("x").arg("-bd").arg("-y");
    if let Some(pw) = &pw {
        x.arg(pw);
    }
    let r = x
        .arg(format!("-o{}", out.path().display()))
        .arg(archive)
        .output()
        .unwrap();
    assert_eq!(
        r.status.code(),
        Some(0),
        "7z x: {}{}",
        text(&r.stdout),
        text(&r.stderr)
    );
    assert_eq!(&tree(out.path()), want, "7z x's tree differs");
}

/// `unzip -t` (Info-ZIP reads Deflate and store only; not zstd, not AES).
fn unzip_test(archive: &Path) {
    let Some(exe) = unzip() else { return };
    let r = Command::new(exe).arg("-t").arg(archive).output().unwrap();
    assert_eq!(
        r.status.code(),
        Some(0),
        "unzip -t: {}{}",
        text(&r.stdout),
        text(&r.stderr)
    );
    assert!(
        text(&r.stdout).contains("No errors detected"),
        "{}",
        text(&r.stdout)
    );
}

/// What the ZIP holds, read with the `zip` crate: path -> (content, is_dir, is_symlink,
/// unix mode, DOS mtime, extended-timestamp mtime, method, encrypted).
#[allow(clippy::type_complexity)]
fn read_zip(
    path: &Path,
    password: Option<&[u8]>,
) -> BTreeMap<
    String,
    (
        Vec<u8>,
        bool,
        bool,
        Option<u32>,
        Option<zip::DateTime>,
        Option<u32>,
        zip::CompressionMethod,
        bool,
    ),
> {
    let mut z = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut m = BTreeMap::new();
    for i in 0..z.len() {
        let mut f = match password {
            Some(pw) => z.by_index_decrypt(i, pw).unwrap(),
            None => z.by_index(i).unwrap(),
        };
        let name = f.name().unwrap().into_owned();
        let ext = f.extra_data_fields().find_map(|e| match e {
            zip::extra_fields::ExtraField::ExtendedTimestamp(t) => t.mod_time(),
            _ => None,
        });
        let meta = (
            f.is_dir(),
            f.is_symlink(),
            f.unix_mode(),
            f.last_modified(),
            ext,
            f.compression(),
            f.encrypted(),
        );
        let mut data = Vec::new();
        f.read_to_end(&mut data).unwrap();
        m.insert(
            name,
            (data, meta.0, meta.1, meta.2, meta.3, meta.4, meta.5, meta.6),
        );
    }
    m
}

/// The `/`-paths and contents of a tree as a ZIP names them (directories with a `/`).
fn zip_view(src: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    tree(src)
        .into_iter()
        .map(|(p, h)| match h {
            None => (format!("{p}/"), None),
            Some(_) => {
                let data = fs::read(src.join(&p)).unwrap();
                (p, Some(data))
            }
        })
        .collect()
}

fn mtime_secs(p: &Path) -> i64 {
    let m = fs::metadata(p).unwrap().modified().unwrap();
    m.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

#[test]
fn zip_export_round_trips_with_every_method_and_the_external_tools() {
    let (work, src, arch) = archived(&[]);
    let want = tree(&src);
    for (method, zip_method) in [
        ("deflate", zip::CompressionMethod::Deflated),
        ("zstd", zip::CompressionMethod::Zstd),
        ("store", zip::CompressionMethod::Stored),
    ] {
        let out = work.path().join(format!("t-{method}.zip"));
        let r = lpk(&[
            "export",
            arch.to_str().unwrap(),
            out.to_str().unwrap(),
            "--method",
            method,
            "--threads",
            "4",
            "-v",
        ]);
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(
            text(&r.stdout),
            format!(
                "exported 7 files, 3 directories, 0 symlinks to {}\n",
                out.display()
            )
        );
        let v = text(&r.stderr);
        assert!(v.contains(&format!("(ZIP, {method})")), "{v}");
        assert!(
            v.contains("blocks decoded") && v.contains("the writer's own memory is not counted"),
            "{v}"
        );
        assert!(
            !v.contains("again for back references), ") || v.contains("(0 again"),
            "{v}"
        );
        // The crate's reader: every entry, its content, kind and attributes.
        let got = read_zip(&out, None);
        let view = zip_view(&src);
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            view.keys().collect::<Vec<_>>()
        );
        for (name, (data, is_dir, is_symlink, mode, dos, ext, m, enc)) in &got {
            let expect = &view[name];
            assert!(!is_symlink && !enc, "{name}");
            assert_eq!(*is_dir, expect.is_none(), "{name}");
            if let Some(content) = expect {
                assert_eq!(data, content, "{name}");
                assert_eq!(*m, zip_method, "{name}");
                assert_eq!(mode.map(|m| m & 0o170_777), Some(0o100_644), "{name}");
            } else {
                assert_eq!(mode.map(|m| m & 0o170_777), Some(0o040_755), "{name}");
            }
            // The mtime: the DOS time within two seconds, the extended timestamp exact.
            let on_disk = mtime_secs(&src.join(name.trim_end_matches('/')));
            assert_eq!(*ext, Some(on_disk as u32), "{name}");
            let d = dos.unwrap();
            let (y, mo, da, h, mi, s) = lpk_cli::export::civil(on_disk);
            assert_eq!(
                (d.year(), d.month(), d.day(), d.hour(), d.minute()),
                (y as u16, mo, da, h, mi),
                "{name}"
            );
            assert!(d.second().abs_diff(s) <= 1, "{name}");
        }
        seven_zip_round_trip(&out, None, &want);
        if method != "zstd" {
            unzip_test(&out);
        }
    }
}

#[test]
fn sevenz_export_round_trips_with_every_method_and_the_external_tool() {
    let (work, src, arch) = archived(&["--balanced"]);
    let want = tree(&src);
    for method in ["lzma2", "deflate", "store"] {
        let out = work.path().join(format!("t-{method}.7z"));
        let r = lpk(&[
            "export",
            arch.to_str().unwrap(),
            out.to_str().unwrap(),
            "--method",
            method,
            "--threads",
            "1",
        ]);
        assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
        assert_eq!(
            text(&r.stdout),
            format!(
                "exported 7 files, 3 directories, 0 symlinks to {}\n",
                out.display()
            )
        );
        // The crate's reader.
        let mut rd = sevenz_rust2::ArchiveReader::new(
            fs::File::open(&out).unwrap(),
            sevenz_rust2::Password::empty(),
        )
        .unwrap();
        let mut got: BTreeMap<String, Option<[u8; 32]>> = BTreeMap::new();
        let mut mtimes = BTreeMap::new();
        rd.for_each_entries(|e, data| {
            let name = e.name().replace('\\', "/");
            if e.is_directory() {
                assert_eq!(e.windows_attributes() & 0x10, 0x10, "{name}");
                assert_eq!(e.windows_attributes() >> 16, 0o040_755, "{name}");
                got.insert(name.clone(), None);
            } else {
                let mut d = Vec::new();
                data.read_to_end(&mut d).unwrap();
                assert_eq!(d.len() as u64, e.size());
                assert_eq!(e.windows_attributes() >> 16, 0o100_644, "{name}");
                got.insert(name.clone(), Some(*blake3::hash(&d).as_bytes()));
            }
            let t: std::time::SystemTime = e.last_modified_date().into();
            mtimes.insert(
                name,
                t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64,
            );
            Ok(true)
        })
        .unwrap();
        assert_eq!(got, want, "{method}");
        for (name, secs) in &mtimes {
            assert_eq!(*secs, mtime_secs(&src.join(name)), "{name}");
        }
        seven_zip_round_trip(&out, None, &want);
    }
    // zstd inside 7z is a fork extension 7-Zip does not read: refused as a usage error.
    let out = work.path().join("z.7z");
    let r = lpk(&[
        "export",
        arch.to_str().unwrap(),
        out.to_str().unwrap(),
        "--method",
        "zstd",
    ]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("7-Zip-ZS"), "{}", text(&r.stderr));
    assert!(!out.exists());
}

#[test]
fn encrypt_writes_aes_entries_with_the_archives_password() {
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    make_tree(&src);
    let arch = work.path().join("e.lpk");
    let a = lpk(&[
        "a",
        arch.to_str().unwrap(),
        src.to_str().unwrap(),
        "--password",
        "open sesame",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let want = tree(&src);
    let out = work.path().join("e.zip");
    let r = lpk(&[
        "export",
        arch.to_str().unwrap(),
        out.to_str().unwrap(),
        "--password",
        "open sesame",
        "--encrypt",
        "-v",
    ]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(
        text(&r.stderr).contains("(ZIP, deflate, AES-256)"),
        "{}",
        text(&r.stderr)
    );
    assert!(!text(&r.stderr).contains("sesame"));
    let got = read_zip(&out, Some(b"open sesame"));
    let view = zip_view(&src);
    assert_eq!(got.len(), view.len());
    for (name, (data, _, _, _, _, _, _, enc)) in &got {
        if let Some(content) = &view[name] {
            assert_eq!(data, content, "{name}");
            assert!(*enc, "{name}: not encrypted");
        }
    }
    // Without the password the content does not open.
    let mut z = zip::ZipArchive::new(fs::File::open(&out).unwrap()).unwrap();
    assert!(z.by_name("a.txt").is_err());
    seven_zip_round_trip(&out, Some("open sesame"), &want);
    // 7z: AES-256 with the header encrypted (the crate's reader needs the password to list).
    let out7 = work.path().join("e.7z");
    let r = lpk(&[
        "export",
        arch.to_str().unwrap(),
        out7.to_str().unwrap(),
        "--password",
        "open sesame",
        "--encrypt",
    ]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(sevenz_rust2::ArchiveReader::new(
        fs::File::open(&out7).unwrap(),
        sevenz_rust2::Password::empty()
    )
    .is_err());
    let mut rd = sevenz_rust2::ArchiveReader::new(
        fs::File::open(&out7).unwrap(),
        sevenz_rust2::Password::new("open sesame"),
    )
    .unwrap();
    let mut got: BTreeMap<String, Option<[u8; 32]>> = BTreeMap::new();
    rd.for_each_entries(|e, data| {
        let name = e.name().replace('\\', "/");
        if e.is_directory() {
            got.insert(name, None);
        } else {
            let mut d = Vec::new();
            data.read_to_end(&mut d).unwrap();
            got.insert(name, Some(*blake3::hash(&d).as_bytes()));
        }
        Ok(true)
    })
    .unwrap();
    assert_eq!(got, want);
    seven_zip_round_trip(&out7, Some("open sesame"), &want);
    // --encrypt without a credential is a usage error; with a keyfile alone too; a wrong
    // password is refused at the key slot and leaves no output.
    let r = lpk(&[
        "export",
        arch.to_str().unwrap(),
        work.path().join("n.zip").to_str().unwrap(),
        "--encrypt",
    ]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("--encrypt"), "{}", text(&r.stderr));
    let key = work.path().join("k.bin");
    fs::write(&key, b"key").unwrap();
    let r = lpk(&[
        "export",
        arch.to_str().unwrap(),
        work.path().join("n.zip").to_str().unwrap(),
        "--keyfile",
        key.to_str().unwrap(),
        "--encrypt",
    ]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(
        text(&r.stderr).contains("keyfile alone"),
        "{}",
        text(&r.stderr)
    );
    assert!(!work.path().join("n.zip").exists());
    let r = lpk(&[
        "--json",
        "export",
        arch.to_str().unwrap(),
        work.path().join("w.zip").to_str().unwrap(),
        "--password",
        "wrong",
    ]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    let e: serde_json::Value = serde_json::from_str(text(&r.stderr).trim()).unwrap();
    assert_eq!(e["error"]["kind"], "WrongKey", "{e}");
    assert!(!work.path().join("w.zip").exists());
    let r = lpk(&[
        "export",
        arch.to_str().unwrap(),
        work.path().join("w.zip").to_str().unwrap(),
    ]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(
        text(&r.stderr).contains("a password is required"),
        "{}",
        text(&r.stderr)
    );
    assert!(!work.path().join("w.zip").exists());
}

#[test]
fn symlink_entries_become_zip_and_7z_symlinks_and_empty_files_are_empty_entries() {
    use lpk_format::{EntryFlags, Writer, WriterOptions};
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("links.lpk");
    {
        let mut out = fs::File::create(&arch).unwrap();
        let mut w = Writer::new(&mut out, WriterOptions::default()).unwrap();
        w.add_file(
            "a.txt",
            EntryFlags::EXECUTABLE,
            1_700_000_000_000_000_000,
            &mut &b"target"[..],
        )
        .unwrap();
        w.add_directory("d", EntryFlags::HIDDEN, 1_700_000_000_000_000_000)
            .unwrap();
        w.add_file("d/none", EntryFlags::READ_ONLY, i64::MIN, &mut &b""[..])
            .unwrap();
        w.add_symlink(
            "link",
            EntryFlags::EMPTY,
            1_700_000_001_000_000_000,
            b"a.txt",
        )
        .unwrap();
        w.finish().unwrap();
    }
    let out = work.path().join("links.zip");
    let r = lpk(&["export", arch.to_str().unwrap(), out.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert_eq!(
        text(&r.stdout),
        format!(
            "exported 2 files, 1 directories, 1 symlinks to {}\n",
            out.display()
        )
    );
    let got = read_zip(&out, None);
    assert_eq!(
        got.keys().cloned().collect::<Vec<_>>(),
        ["a.txt", "d/", "d/none", "link"]
    );
    let (data, _, is_symlink, mode, _, ext, m, _) = &got["link"];
    assert!(is_symlink);
    assert_eq!(data, b"a.txt");
    assert_eq!(mode.map(|m| m & 0o170_777), Some(0o120_777));
    assert_eq!(*m, zip::CompressionMethod::Stored);
    assert_eq!(*ext, Some(1_700_000_001));
    let (_, _, _, mode, _, ext, _, _) = &got["a.txt"];
    assert_eq!(mode.map(|m| m & 0o170_777), Some(0o100_755));
    assert_eq!(*ext, Some(1_700_000_000));
    let (data, _, _, _, dos, ext, _, _) = &got["d/none"];
    assert!(data.is_empty());
    assert_eq!(*ext, None, "an unknown mtime has no extended timestamp");
    assert_eq!(*dos, Some(zip::DateTime::default()));
    unzip_test(&out);
    // 7z: the symlink's mode in the attributes, the target as its content.
    let out7 = work.path().join("links.7z");
    let r = lpk(&["export", arch.to_str().unwrap(), out7.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    let mut rd = sevenz_rust2::ArchiveReader::new(
        fs::File::open(&out7).unwrap(),
        sevenz_rust2::Password::empty(),
    )
    .unwrap();
    let mut seen = Vec::new();
    rd.for_each_entries(|e, data| {
        let mut d = Vec::new();
        data.read_to_end(&mut d).unwrap();
        seen.push((e.name().replace('\\', "/"), e.windows_attributes(), d));
        Ok(true)
    })
    .unwrap();
    seen.sort();
    assert_eq!(
        seen[0],
        (
            "a.txt".to_string(),
            0x8000 | (0o100_755 << 16),
            b"target".to_vec()
        )
    );
    assert_eq!(seen[1].0, "d");
    assert_eq!(seen[1].1, 0x02 | 0x10 | 0x8000 | (0o040_755 << 16));
    assert_eq!(
        seen[2],
        (
            "d/none".to_string(),
            0x01 | 0x8000 | (0o100_644 << 16),
            Vec::new()
        )
    );
    assert_eq!(
        seen[3],
        (
            "link".to_string(),
            0x8000 | (0o120_777 << 16),
            b"a.txt".to_vec()
        )
    );
    if let Some(exe) = seven_zip() {
        let r = Command::new(exe)
            .arg("t")
            .arg("-bd")
            .arg(&out7)
            .output()
            .unwrap();
        assert_eq!(
            r.status.code(),
            Some(0),
            "7z t: {}{}",
            text(&r.stdout),
            text(&r.stderr)
        );
    }
}

#[test]
fn refusals_and_usage_errors_exit_as_the_contract_says() {
    let (work, _src, arch) = archived(&[]);
    let a = arch.to_str().unwrap();
    // The extension decides the container; anything else is a usage error.
    let rar = work.path().join("t.rar");
    let r = lpk(&["export", a, rar.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(
        text(&r.stderr).contains(".zip or .7z"),
        "{}",
        text(&r.stderr)
    );
    assert!(!rar.exists());
    let noext = work.path().join("t");
    let r = lpk(&["--json", "export", a, noext.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(1));
    let e: serde_json::Value = serde_json::from_str(text(&r.stderr).trim()).unwrap();
    assert_eq!(e["error"]["kind"], "Usage");
    assert_eq!(e["error"]["code"], 1);
    // lzma2 is a 7z method.
    let z = work.path().join("t.zip");
    let r = lpk(&["export", a, z.to_str().unwrap(), "--method", "lzma2"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("lzma2"), "{}", text(&r.stderr));
    assert!(!z.exists());
    // A bad method value: clap's usage error.
    let r = lpk(&["export", a, z.to_str().unwrap(), "--method", "bzip2"]);
    assert_eq!(r.status.code(), Some(1));
    // An existing output is refused (exit 2) unless --force.
    fs::write(&z, b"old").unwrap();
    let r = lpk(&["export", a, z.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("--force"), "{}", text(&r.stderr));
    assert_eq!(fs::read(&z).unwrap(), b"old");
    let r = lpk(&["export", a, z.to_str().unwrap(), "--force"]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    assert!(fs::metadata(&z).unwrap().len() > 1000);
    assert_eq!(read_zip(&z, None).len(), 10);
    // A missing archive: exit 2 and no output left behind.
    let missing = work.path().join("missing.lpk");
    let m = work.path().join("m.zip");
    let r = lpk(&["export", missing.to_str().unwrap(), m.to_str().unwrap()]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(!m.exists());
    // --threads over the cap and --memory below one block are usage errors (as for x).
    let r = lpk(&["export", a, m.to_str().unwrap(), "--threads", "100000"]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    // (A limit the envelope allows but below one block in flight, as cli.rs tests for x; a
    // limit below the envelope stays a refusal, exit 2.)
    let basic =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-format/tests/vectors/zstd-basic.lpk");
    let r = lpk(&[
        "export",
        basic.to_str().unwrap(),
        m.to_str().unwrap(),
        "--memory",
        "20000",
    ]);
    assert_eq!(r.status.code(), Some(1), "{}", text(&r.stderr));
    assert!(
        text(&r.stderr).contains("below what one block in flight needs"),
        "{}",
        text(&r.stderr)
    );
    assert!(!m.exists());
    let r = lpk(&["export", a, m.to_str().unwrap(), "--memory", "1000"]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(!m.exists());
    // The vectors export too (a peeled archive through the full reader).
    for v in [
        "zstd-multiblock.lpk",
        "deflate-peel.lpk",
        "jpeg-peel.lpk",
        "journal-3gen.lpk",
    ] {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../lpk-format/tests/vectors")
            .join(v);
        let out = work.path().join(format!("{v}.zip"));
        let r = lpk(&[
            "export",
            p.to_str().unwrap(),
            out.to_str().unwrap(),
            "--threads",
            "3",
        ]);
        assert_eq!(r.status.code(), Some(0), "{v}: {}", text(&r.stderr));
        let got = read_zip(&out, None);
        assert!(!got.is_empty(), "{v}");
        unzip_test(&out);
    }
}

/// A file above 4 GiB (all zeros: the archive deduplicates it to a few chunks, so the test
/// costs the disk the file and the ZIP, not the archive) exported as Zip64 and read back.
/// Run once with `cargo test --release -p lpk-cli --test export -- --ignored zip64`.
#[test]
#[ignore = "writes and reads back more than 8 GiB; run with --release -- --ignored"]
fn zip64_is_written_for_a_file_above_4_gib() {
    const SIZE: u64 = (4 << 30) + (1 << 20);
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let big = src.join("big.bin");
    {
        let f = fs::File::create(&big).unwrap();
        f.set_len(SIZE).unwrap();
    }
    fs::write(src.join("small.txt"), b"after the big one\n").unwrap();
    let arch = work.path().join("big.lpk");
    let a = lpk(&[
        "a",
        arch.to_str().unwrap(),
        src.to_str().unwrap(),
        "--store",
        "--no-dedup",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let out = work.path().join("big.zip");
    let r = lpk(&[
        "export",
        arch.to_str().unwrap(),
        out.to_str().unwrap(),
        "--method",
        "store",
        "-v",
    ]);
    assert_eq!(r.status.code(), Some(0), "{}", text(&r.stderr));
    eprintln!("{}", text(&r.stderr));
    assert!(fs::metadata(&out).unwrap().len() > SIZE);
    let mut z = zip::ZipArchive::new(fs::File::open(&out).unwrap()).unwrap();
    assert_eq!(z.len(), 2);
    {
        let mut f = z.by_name("big.bin").unwrap();
        assert_eq!(f.size(), SIZE);
        assert!(f.size() > u64::from(u32::MAX), "only Zip64 holds this size");
        let mut buf = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let n = f.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            assert!(buf[..n].iter().all(|&b| b == 0));
            total += n as u64;
        }
        assert_eq!(total, SIZE);
    }
    let mut s = String::new();
    z.by_name("small.txt")
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    assert_eq!(s, "after the big one\n");
    drop(z);
    if let Some(exe) = seven_zip() {
        let r = Command::new(exe)
            .arg("t")
            .arg("-bd")
            .arg(&out)
            .output()
            .unwrap();
        assert_eq!(
            r.status.code(),
            Some(0),
            "7z t: {}{}",
            text(&r.stdout),
            text(&r.stderr)
        );
        eprintln!("7z t: {}", text(&r.stdout));
    }
    unzip_test(&out);
}
