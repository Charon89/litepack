//! `lpk-bench corpus scan --private DIR --out DIR` (PLAN P0-2d).
//!
//! Walks a user folder, classifies every regular file ([`super::classify`]), hashes it with
//! BLAKE3 and writes `manifest.json` and `build-info.json` into `--out`. The manifest has the
//! public corpus schema and writer (`profile` is `"private"`, `source` and `licence` of every
//! file are `"private"`, `path` is relative to the scanned folder).
//!
//! Guarantees:
//! * The scanned folder is only ever opened for reading. Nothing is copied, moved or changed;
//!   the only writes are the two files in `--out`, created after the walk has finished.
//! * This module does not use the fetch or build code, so it cannot reach the network.
//! * Symlinks, junctions and other reparse points (on Windows also offline or cloud
//!   placeholders, whose reading would trigger a download) are not followed; they are listed
//!   in `skipped` of `build-info.json`, as are unreadable files and special files.
//! * If `--out` lies inside the scanned folder it is excluded from the walk.
//! * The manifest depends only on the folder's relative paths and contents, so two scans of an
//!   unchanged folder give identical bytes. `build-info.json` carries the timestamp, `"private":
//!   true` and `"root"` (the absolute scanned folder, which the runner needs to find the files).
//! * The scan root itself is resolved if it is a symlink (the user named it explicitly).

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use super::classify::{classify, HEAD_LEN};
use super::manifest::{now_rfc3339, BuildInfo, Manifest, ManifestFile, Skipped};
use super::registry::Profile;
use super::ScanArgs;

/// Value of `profile`, `source` and `licence` in a private manifest.
pub const PRIVATE: &str = "private";

/// What a scan wrote and found.
#[derive(Debug)]
pub struct ScanReport {
    pub manifest_path: PathBuf,
    pub manifest_blake3: String,
    pub files: usize,
    pub bytes_total: u64,
    pub skipped: Vec<Skipped>,
}

/// `build-info.json` of a private scan: the public fields plus `private` and `root`.
#[derive(Serialize)]
struct PrivateBuildInfo {
    #[serde(flatten)]
    info: BuildInfo,
    private: bool,
    root: String,
}

struct Walk {
    files: Vec<(String, ManifestFile)>,
    skipped: Vec<Skipped>,
    /// Directory entries seen (files, directories, skipped items), `--out` excluded.
    entries: usize,
}

/// Scan `root` and write the manifest and build info into `out`.
pub fn scan(root: &Path, out: &Path) -> Result<ScanReport> {
    let meta = fs::metadata(root).with_context(|| {
        format!(
            "scan folder `{}` does not exist or is not accessible",
            root.display()
        )
    })?;
    if !meta.is_dir() {
        bail!("scan folder `{}` is not a directory", root.display());
    }
    let canonical = fs::canonicalize(root)
        .with_context(|| format!("resolving scan folder `{}`", root.display()))?;
    let root_abs = std::path::absolute(root)
        .with_context(|| format!("making `{}` absolute", root.display()))?;
    let root_text = root_abs
        .to_str()
        .with_context(|| format!("scan folder `{}` is not valid UTF-8", root_abs.display()))?
        .to_string();
    // Only an existing `out` can lie inside the folder: nothing is created before the walk.
    let exclude = fs::canonicalize(out).ok();

    let walk = walk(&canonical, exclude.as_deref());
    if walk.entries == 0 {
        bail!("scan folder `{}` is empty: nothing to scan", root.display());
    }

    let mut manifest = Manifest::new(Profile::Small, walk.files);
    manifest.profile = PRIVATE.to_string();
    let manifest_text = manifest.render();
    let manifest_blake3 = blake3::hash(manifest_text.as_bytes()).to_hex().to_string();
    let mut skipped = walk.skipped;
    skipped.sort_by(|a, b| (&a.source, &a.reason).cmp(&(&b.source, &b.reason)));
    let info = PrivateBuildInfo {
        info: BuildInfo {
            built_at: now_rfc3339(),
            host_arch: std::env::consts::ARCH.to_string(),
            host_os: std::env::consts::OS.to_string(),
            lpk_bench_version: env!("CARGO_PKG_VERSION").to_string(),
            manifest_blake3: manifest_blake3.clone(),
            profile: PRIVATE.to_string(),
            skipped: skipped.clone(),
        },
        private: true,
        root: root_text,
    };
    let mut info_text = serde_json::to_string_pretty(&serde_json::to_value(&info)?)?;
    info_text.push('\n');

    fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let manifest_path = out.join("manifest.json");
    fs::write(&manifest_path, &manifest_text)
        .with_context(|| format!("writing {}", manifest_path.display()))?;
    let info_path = out.join("build-info.json");
    fs::write(&info_path, info_text).with_context(|| format!("writing {}", info_path.display()))?;

    Ok(ScanReport {
        manifest_path,
        manifest_blake3,
        files: manifest.classes.values().map(|c| c.files.len()).sum(),
        bytes_total: manifest.classes.values().map(|c| c.bytes_total).sum(),
        skipped,
    })
}

fn skip(list: &mut Vec<Skipped>, path: &str, reason: impl std::fmt::Display) {
    list.push(Skipped {
        reason: reason.to_string(),
        source: path.to_string(),
    });
}

/// Depth-first walk without following links. `root` is the canonical scan folder.
fn walk(root: &Path, exclude: Option<&Path>) -> Walk {
    let mut w = Walk {
        files: Vec::new(),
        skipped: Vec::new(),
        entries: 0,
    };
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let shown = if rel.is_empty() { "." } else { rel.as_str() };
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                skip(&mut w.skipped, shown, format!("cannot list directory: {e}"));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    skip(&mut w.skipped, shown, format!("cannot list directory: {e}"));
                    continue;
                }
            };
            let path = entry.path();
            if exclude == Some(path.as_path()) {
                continue;
            }
            w.entries += 1;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                let lossy = format!("{shown}/{}", file_name.to_string_lossy());
                skip(&mut w.skipped, &lossy, "name is not valid UTF-8");
                continue;
            };
            let rel_path = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{rel}/{name}")
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    skip(&mut w.skipped, &rel_path, format!("cannot stat: {e}"));
                    continue;
                }
            };
            let ft = meta.file_type();
            if ft.is_symlink() || is_reparse_or_offline(&meta) {
                skip(
                    &mut w.skipped,
                    &rel_path,
                    "symlink or reparse point, not followed",
                );
            } else if ft.is_dir() {
                stack.push((path, rel_path));
            } else if ft.is_file() {
                match hash_file(&path) {
                    Ok((blake3, bytes, head)) => {
                        let class = classify(&rel_path, &head);
                        w.files.push((
                            class.to_string(),
                            ManifestFile {
                                blake3,
                                bytes,
                                licence: PRIVATE.to_string(),
                                path: rel_path,
                                source: PRIVATE.to_string(),
                            },
                        ));
                    }
                    Err(e) => skip(&mut w.skipped, &rel_path, format!("unreadable: {e}")),
                }
            } else {
                skip(&mut w.skipped, &rel_path, "not a regular file");
            }
        }
    }
    w
}

/// Windows reparse points other than symlinks (cloud placeholders, dedup stubs) and offline
/// files: reading them can trigger a download, so they are never opened.
#[cfg(windows)]
fn is_reparse_or_offline(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const REPARSE_POINT: u32 = 0x400;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x40000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    meta.file_attributes() & (REPARSE_POINT | OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(not(windows))]
fn is_reparse_or_offline(_meta: &fs::Metadata) -> bool {
    false
}

/// BLAKE3 (hex), size and the first [`HEAD_LEN`] bytes of a file, read once, read-only.
fn hash_file(path: &Path) -> std::io::Result<(String, u64, Vec<u8>)> {
    let mut file = File::open(path)?;
    let mut head = vec![0u8; HEAD_LEN];
    let mut filled = 0;
    while filled < HEAD_LEN {
        let n = file.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    head.truncate(filled);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&head);
    let mut total = filled as u64;
    if filled == HEAD_LEN {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
    }
    Ok((hasher.finalize().to_hex().to_string(), total, head))
}

/// Entry point for `lpk-bench corpus scan`.
pub fn run(args: &ScanArgs) -> ExitCode {
    match scan(&args.private, &args.out) {
        Ok(r) => {
            println!(
                "{} files, {} bytes; manifest {} (blake3 {})",
                r.files,
                r.bytes_total,
                r.manifest_path.display(),
                r.manifest_blake3
            );
            for s in &r.skipped {
                println!("skipped {}: {}", s.source, s.reason);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::SystemTime;

    use tempfile::TempDir;

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        fs::write(p, bytes).expect("write");
    }

    /// Fixture folder with a few classes; returns (temp, folder).
    fn fixture() -> (TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("data");
        write(&dir, "pics/a.jpg", b"\xFF\xD8\xFF\xE0jpeg");
        write(&dir, "pics/liar.jpg", b"\x89PNG\r\n\x1A\nxx");
        write(&dir, "docs/r.pdf", b"%PDF-1.4 x");
        write(&dir, "src/.git/objects/ab/cd", b"blob");
        write(&dir, "misc/what.xyz", b"???");
        write(&dir, "notes.txt", b"hello");
        (tmp, dir)
    }

    fn read_manifest(out: &Path) -> Manifest {
        let text = fs::read_to_string(out.join("manifest.json")).expect("manifest");
        serde_json::from_str(&text).expect("parse")
    }

    fn listing(root: &Path) -> BTreeMap<PathBuf, (bool, u64, Option<SystemTime>)> {
        let mut map = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).expect("read_dir") {
                let e = e.expect("entry");
                // Not `DirEntry::metadata`: on NTFS its cached times can lag behind the file's.
                let m = fs::metadata(e.path()).expect("meta");
                if m.is_dir() {
                    stack.push(e.path());
                }
                map.insert(e.path(), (m.is_dir(), m.len(), m.modified().ok()));
            }
        }
        map
    }

    #[test]
    fn scan_classifies_and_uses_the_public_schema() {
        let (tmp, dir) = fixture();
        let out = tmp.path().join("out");
        let r = scan(&dir, &out).expect("scan");
        assert_eq!(r.files, 6);
        let m = read_manifest(&out);
        assert_eq!(m.profile, "private");
        let class_of = |path: &str| {
            m.classes
                .iter()
                .find(|(_, c)| c.files.iter().any(|f| f.path == path))
                .map(|(k, _)| k.as_str())
        };
        assert_eq!(class_of("pics/a.jpg"), Some("photo-jpeg"));
        assert_eq!(class_of("pics/liar.jpg"), Some("photo-raw-png"));
        assert_eq!(class_of("docs/r.pdf"), Some("office-pdf"));
        assert_eq!(class_of("src/.git/objects/ab/cd"), Some("source-git"));
        assert_eq!(class_of("misc/what.xyz"), Some("other"));
        assert_eq!(class_of("notes.txt"), Some("text-prose"));
        let f = &m.classes["text-prose"].files[0];
        assert_eq!(f.source, "private");
        assert_eq!(f.licence, "private");
        assert_eq!(f.bytes, 5);
        assert_eq!(f.blake3, blake3::hash(b"hello").to_hex().to_string());
        let jpeg_files = &m.classes["photo-jpeg"].files;
        assert_eq!(jpeg_files.len(), 1);
    }

    #[test]
    fn build_info_is_private_and_names_the_root() {
        let (tmp, dir) = fixture();
        let out = tmp.path().join("out");
        let r = scan(&dir, &out).expect("scan");
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("build-info.json")).expect("read"))
                .expect("json");
        assert_eq!(v["private"], true);
        let root = std::path::absolute(&dir).expect("abs");
        assert_eq!(v["root"], root.to_str().expect("utf8"));
        assert_eq!(v["profile"], "private");
        assert_eq!(v["manifest_blake3"], r.manifest_blake3.as_str());
        assert!(v["built_at"].is_string() && v["skipped"].is_array());
        assert!(v["lpk_bench_version"].is_string());
    }

    #[test]
    fn scan_does_not_touch_the_folder() {
        let (tmp, dir) = fixture();
        let before = listing(&dir);
        scan(&dir, &tmp.path().join("out")).expect("scan");
        scan(&dir, &tmp.path().join("out")).expect("scan again");
        assert_eq!(before, listing(&dir));
        assert!(!before.is_empty());
    }

    #[test]
    fn out_inside_the_folder_is_excluded_and_scans_are_byte_identical() {
        let (_tmp, dir) = fixture();
        let out = dir.join("scan-out");
        scan(&dir, &out).expect("first");
        let first = fs::read(out.join("manifest.json")).expect("read");
        // The second scan sees the first scan's output on disk.
        scan(&dir, &out).expect("second");
        let second = fs::read(out.join("manifest.json")).expect("read");
        assert_eq!(first, second);
        let m = read_manifest(&out);
        let n: usize = m.classes.values().map(|c| c.files.len()).sum();
        assert_eq!(n, 6);
        assert!(!m
            .classes
            .values()
            .flat_map(|c| &c.files)
            .any(|f| f.path.starts_with("scan-out")));
    }

    #[test]
    fn two_scans_into_different_outs_are_byte_identical() {
        let (tmp, dir) = fixture();
        scan(&dir, &tmp.path().join("o1")).expect("scan");
        scan(&dir, &tmp.path().join("o2")).expect("scan");
        assert_eq!(
            fs::read(tmp.path().join("o1/manifest.json")).expect("read"),
            fs::read(tmp.path().join("o2/manifest.json")).expect("read")
        );
    }

    #[test]
    fn missing_and_empty_folders_are_clear_errors() {
        let tmp = tempfile::tempdir().expect("tmp");
        let out = tmp.path().join("out");
        let e = scan(&tmp.path().join("nope"), &out).expect_err("missing");
        assert!(format!("{e:#}").contains("does not exist"), "{e:#}");
        let empty = tmp.path().join("empty");
        fs::create_dir(&empty).expect("mkdir");
        let e = scan(&empty, &out).expect_err("empty");
        assert!(format!("{e:#}").contains("empty"), "{e:#}");
        let file = tmp.path().join("f.txt");
        fs::write(&file, b"x").expect("write");
        let e = scan(&file, &out).expect_err("file");
        assert!(format!("{e:#}").contains("not a directory"), "{e:#}");
        assert!(!out.exists(), "a failed scan must not create the output");
    }

    #[test]
    fn long_paths_are_scanned() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("data");
        let mut rel = String::new();
        for i in 0..8 {
            rel.push_str(&format!("{}{i}/", "d".repeat(40)));
        }
        rel.push_str("deep.txt");
        assert!(rel.len() > 260);
        write(&dir, &rel, b"deep");
        let out = tmp.path().join("out");
        scan(&dir, &out).expect("scan");
        let m = read_manifest(&out);
        assert_eq!(m.classes["text-prose"].files[0].path, rel);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let (tmp, dir) = fixture();
        let outside = tmp.path().join("outside");
        write(&outside, "secret.txt", b"secret");
        std::os::unix::fs::symlink(&outside, dir.join("linked-dir")).expect("symlink dir");
        std::os::unix::fs::symlink(outside.join("secret.txt"), dir.join("linked.txt"))
            .expect("symlink file");
        let out = tmp.path().join("out");
        let r = scan(&dir, &out).expect("scan");
        assert_eq!(r.files, 6);
        let skipped: Vec<&str> = r.skipped.iter().map(|s| s.source.as_str()).collect();
        assert_eq!(skipped, ["linked-dir", "linked.txt"]);
        let m = read_manifest(&out);
        assert!(!m
            .classes
            .values()
            .flat_map(|c| &c.files)
            .any(|f| f.path.contains("linked") || f.path.contains("secret")));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_are_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, dir) = fixture();
        let locked = dir.join("locked.txt");
        fs::write(&locked, b"x").expect("write");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        let r = scan(&dir, &tmp.path().join("out")).expect("scan");
        // Running as root can still read the file; only assert when the open really failed.
        if File::open(&locked).is_err() {
            assert!(r.skipped.iter().any(|s| s.source == "locked.txt"));
            assert_eq!(r.files, 6);
        }
    }
}
