//! `lpk-bench corpus scan --private DIR --out DIR` (PLAN P0-2d).
//!
//! Walks a user folder, classifies every regular file ([`super::classify`]), hashes it with
//! BLAKE3 and writes `manifest.json` and `build-info.json` into `--out`. The manifest has the
//! public corpus schema and writer (`profile` is `"private"`, `source` and `licence` of every
//! file are `"private"`, `path` is relative to the scanned folder).
//!
//! Guarantees:
//! * The scanned folder is only ever opened for reading. Nothing is copied, moved or changed;
//!   the only writes are `manifest.json`, `build-info.json` and a `.lpk-corpus` marker in
//!   `--out`, created after the walk has finished.
//! * `--out` equal to the scanned folder or anywhere inside it (existing or not yet existing,
//!   compared after resolving both) is refused before anything is created. A non-empty `--out`
//!   without the `.lpk-corpus` marker is refused untouched, like a corpus build output.
//! * This module does not use the fetch or build code, so it cannot reach the network.
//! * Symlinks, junctions and other reparse points are not followed, and on Windows cloud
//!   placeholders (offline / recall-on-access files, whose reading would trigger a download)
//!   are not opened. They are listed in `skipped` of `build-info.json` with distinct reasons,
//!   as are unreadable files and special files; the summary line prints the skipped count.
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

/// Name of the file that marks a directory as owned by lpk-bench (same as the corpus build's).
const MARKER: &str = ".lpk-corpus";

struct Walk {
    files: Vec<(String, ManifestFile)>,
    skipped: Vec<Skipped>,
}

/// Canonical form of `path`, falling back to the absolute path where canonicalisation is not
/// supported (RAM disks, WinFsp/Dokan mounts, some network shares).
fn resolve_existing(path: &Path) -> std::io::Result<PathBuf> {
    fs::canonicalize(path).or_else(|_| std::path::absolute(path))
}

/// Append `tail` (names in path order) to `base`, folding `.` and `..` inside the tail only.
/// The tail does not exist, so no link can make the lexical fold wrong.
fn append_folded(mut base: PathBuf, tail: &[std::ffi::OsString]) -> PathBuf {
    let mut added = 0usize;
    for name in tail {
        match name.to_str() {
            Some(".") => {}
            Some("..") if added > 0 => {
                base.pop();
                added -= 1;
            }
            _ => {
                base.push(name);
                added += 1;
            }
        }
    }
    base
}

/// Resolve a path that need not exist. The longest existing prefix of the path *as written*
/// is canonicalised (so `link/../x` means what the kernel makes of it); `..` is folded only in
/// the missing tail. A link whose target does not exist is an error, not a missing path.
fn resolve_maybe_missing(path: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut base = abs.clone();
    loop {
        match fs::symlink_metadata(&base) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    if let Err(e) = fs::metadata(&base) {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            bail!(
                                "output path `{}` is a link whose target does not exist \
                                 (`{}`); refusing to guess where it would write",
                                path.display(),
                                base.display()
                            );
                        }
                        return Err(e).with_context(|| format!("checking `{}`", base.display()));
                    }
                }
                tail.reverse();
                return Ok(append_folded(resolve_existing(&base)?, &tail));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = base
                    .components()
                    .next_back()
                    .map(|c| c.as_os_str().to_os_string());
                match (name, base.parent().map(Path::to_path_buf)) {
                    (Some(name), Some(parent)) => {
                        tail.push(name);
                        base = parent;
                    }
                    _ => return Ok(abs),
                }
            }
            Err(e) => return Err(e).with_context(|| format!("checking `{}`", base.display())),
        }
    }
}

/// A marked output directory may be reused only if it is a previous scan output: nothing but
/// the marker, `manifest.json` and `build-info.json`, and a manifest whose profile is private.
fn check_reusable(dir: &Path, shown: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("listing `{}`", shown.display()))? {
        let name = entry?.file_name();
        if ![MARKER, "manifest.json", "build-info.json"]
            .iter()
            .any(|n| name == *n)
        {
            bail!(
                "output directory `{}` holds `{}`, so it is not a previous private scan output \
                 (it looks like a corpus build); refusing to touch it. Use a new or empty \
                 directory.",
                shown.display(),
                name.to_string_lossy()
            );
        }
    }
    let manifest = dir.join("manifest.json");
    if manifest.exists() {
        let profile = fs::read_to_string(&manifest)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["profile"].as_str().map(str::to_string));
        if profile.as_deref() != Some(PRIVATE) {
            bail!(
                "output directory `{}` holds a manifest whose profile is {}, not `{PRIVATE}`; \
                 refusing to overwrite it. Use a new or empty directory.",
                shown.display(),
                profile.map_or("missing".to_string(), |p| format!("`{p}`"))
            );
        }
    }
    Ok(())
}

/// Component-wise prefix test; case-insensitive on Windows, where paths are.
fn is_inside_or_equal(path: &Path, base: &Path) -> bool {
    let mut p = path.components();
    base.components().all(|b| {
        p.next().is_some_and(|c| {
            if cfg!(windows) {
                c.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
            } else {
                c == b
            }
        })
    })
}

/// Refuse an `out` that would write into the scanned folder or into somebody else's files.
/// Returns the resolved path, which is the one to create and write through.
fn check_out(root: &Path, out: &Path) -> Result<PathBuf> {
    let out_resolved = resolve_maybe_missing(out)
        .with_context(|| format!("resolving output directory `{}`", out.display()))?;
    if is_inside_or_equal(&out_resolved, root) {
        bail!(
            "output directory `{}` is the scanned folder or inside it; a scan never writes into \
             the folder it scans. Choose a directory outside it.",
            out.display()
        );
    }
    if out_resolved.exists() {
        if !out_resolved.is_dir() {
            bail!(
                "output path `{}` exists and is not a directory",
                out.display()
            );
        }
        let non_empty = fs::read_dir(&out_resolved)
            .with_context(|| format!("listing `{}`", out.display()))?
            .next()
            .is_some();
        if non_empty {
            if !out_resolved.join(MARKER).exists() {
                bail!(
                    "output directory `{}` is not empty and was not created by lpk-bench (no \
                     {MARKER} marker); refusing to touch it. Use a new or empty directory.",
                    out.display()
                );
            }
            check_reusable(&out_resolved, out)?;
        }
    }
    Ok(out_resolved)
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
    let walk_root = resolve_existing(root)
        .with_context(|| format!("resolving scan folder `{}`", root.display()))?;
    let root_abs = std::path::absolute(root)
        .with_context(|| format!("making `{}` absolute", root.display()))?;
    let root_text = root_abs
        .to_str()
        .with_context(|| format!("scan folder `{}` is not valid UTF-8", root_abs.display()))?
        .to_string();
    let out_dir = check_out(&walk_root, out)?;

    let mut top = fs::read_dir(&walk_root)
        .with_context(|| format!("cannot list scan folder `{}`", root.display()))?;
    if top.next().is_none() {
        bail!("scan folder `{}` is empty: nothing to scan", root.display());
    }
    drop(top);

    let walk = walk(&walk_root);
    let manifest = Manifest::with_profile_name(PRIVATE, walk.files);
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
            accounting: Vec::new(),
            summary: Default::default(),
        },
        private: true,
        root: root_text,
    };
    let mut info_text = serde_json::to_string_pretty(&serde_json::to_value(&info)?)?;
    info_text.push('\n');

    // Write through the resolved path, never the one as written.
    fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out.display()))?;
    let marker = out_dir.join(MARKER);
    if !marker.exists() {
        fs::write(
            &marker,
            "lpk-bench corpus output (private scan: manifest only, no file contents).\n",
        )
        .with_context(|| format!("writing {}", marker.display()))?;
    }
    fs::write(out_dir.join("manifest.json"), &manifest_text)
        .with_context(|| format!("writing manifest.json in {}", out.display()))?;
    let manifest_path = out.join("manifest.json");
    let info_path = out_dir.join("build-info.json");
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

/// Depth-first walk without following links. `root` is the resolved scan folder.
fn walk(root: &Path) -> Walk {
    let mut w = Walk {
        files: Vec::new(),
        skipped: Vec::new(),
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
            if let Some(reason) = link_skip_reason(ft.is_symlink(), file_attributes(&meta)) {
                skip(&mut w.skipped, &rel_path, reason);
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

const ATTR_REPARSE_POINT: u32 = 0x400;
const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

pub const REASON_CLOUD: &str = "cloud placeholder, not downloaded";
pub const REASON_LINK: &str = "symlink or junction, not followed";
pub const REASON_REPARSE: &str = "other reparse point, not followed";

/// Why an entry must not be opened, from its link flag and Windows file attribute bits
/// (always 0 elsewhere). Cloud placeholders are reparse points too, so they are checked first;
/// opening one would make the OS download it.
fn link_skip_reason(is_symlink: bool, attrs: u32) -> Option<&'static str> {
    if attrs & (ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0 {
        Some(REASON_CLOUD)
    } else if is_symlink {
        Some(REASON_LINK)
    } else if attrs & ATTR_REPARSE_POINT != 0 {
        Some(REASON_REPARSE)
    } else {
        None
    }
}

#[cfg(windows)]
fn file_attributes(meta: &fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
fn file_attributes(_meta: &fs::Metadata) -> u32 {
    0
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
                "{} files, {} bytes, {} skipped; manifest {} (blake3 {})",
                r.files,
                r.bytes_total,
                r.skipped.len(),
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

    /// Full snapshot (paths, sizes, contents) of a folder for "untouched" assertions.
    fn contents(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        listing(root)
            .into_keys()
            .map(|p| {
                let data = fs::read(&p).ok();
                (p, data)
            })
            .collect()
    }

    fn assert_refused_and_untouched(dir: &Path, out: &Path, needle: &str) {
        let (before_list, before_data) = (listing(dir), contents(dir));
        let e = scan(dir, out).expect_err("must be refused");
        assert!(format!("{e:#}").contains(needle), "{e:#}");
        assert_eq!(before_list, listing(dir));
        assert_eq!(before_data, contents(dir));
    }

    #[test]
    fn out_equal_to_or_inside_the_folder_is_refused_and_nothing_is_written() {
        let (_tmp, dir) = fixture();
        // The folder itself.
        assert_refused_and_untouched(&dir, &dir, "inside it");
        // Spelled differently: trailing `.` and a `..` detour.
        assert_refused_and_untouched(&dir, &dir.join("."), "inside it");
        assert_refused_and_untouched(&dir, &dir.join("pics").join(".."), "inside it");
        // An existing subfolder that holds files.
        assert_refused_and_untouched(&dir, &dir.join("pics"), "inside it");
        // A subfolder that does not exist yet, also nested.
        assert_refused_and_untouched(&dir, &dir.join("scan-out"), "inside it");
        assert_refused_and_untouched(&dir, &dir.join("new").join("deeper"), "inside it");
        assert!(!dir.join("scan-out").exists() && !dir.join("new").exists());
    }

    #[test]
    fn foreign_non_empty_out_is_refused_untouched_and_marked_out_is_reused() {
        let (tmp, dir) = fixture();
        let foreign = tmp.path().join("precious");
        write(&foreign, "keep.txt", b"mine");
        let before = contents(&foreign);
        let e = scan(&dir, &foreign).expect_err("foreign");
        assert!(format!("{e:#}").contains("refusing to touch"), "{e:#}");
        assert_eq!(before, contents(&foreign));
        // An empty existing directory is fine and gets the marker; a rescan reuses it.
        let empty = tmp.path().join("empty-out");
        fs::create_dir(&empty).expect("mkdir");
        scan(&dir, &empty).expect("first");
        assert!(empty.join(MARKER).exists());
        scan(&dir, &empty).expect("second");
    }

    #[test]
    fn marker_name_matches_the_corpus_build() {
        assert_eq!(MARKER, super::super::build::MARKER);
    }

    #[test]
    fn link_skip_reasons_from_attribute_bits() {
        assert_eq!(link_skip_reason(false, 0), None);
        assert_eq!(link_skip_reason(false, 0x20), None);
        assert_eq!(link_skip_reason(true, 0), Some(REASON_LINK));
        assert_eq!(
            link_skip_reason(true, ATTR_REPARSE_POINT),
            Some(REASON_LINK)
        );
        assert_eq!(
            link_skip_reason(false, ATTR_REPARSE_POINT),
            Some(REASON_REPARSE)
        );
        // Cloud placeholders are reparse points as well; they keep their own reason.
        for bits in [
            ATTR_OFFLINE,
            ATTR_RECALL_ON_OPEN,
            ATTR_RECALL_ON_DATA_ACCESS,
        ] {
            assert_eq!(link_skip_reason(false, bits), Some(REASON_CLOUD));
            assert_eq!(
                link_skip_reason(false, bits | ATTR_REPARSE_POINT),
                Some(REASON_CLOUD)
            );
        }
        let reasons = [REASON_CLOUD, REASON_LINK, REASON_REPARSE];
        assert!(reasons[0] != reasons[1] && reasons[1] != reasons[2] && reasons[0] != reasons[2]);
    }

    #[test]
    fn path_comparison_is_component_wise() {
        assert!(is_inside_or_equal(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(is_inside_or_equal(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!is_inside_or_equal(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!is_inside_or_equal(Path::new("/a"), Path::new("/a/b")));
        let tail = |names: &[&str]| -> Vec<std::ffi::OsString> {
            names.iter().map(std::ffi::OsString::from).collect()
        };
        assert_eq!(
            append_folded(PathBuf::from("/a"), &tail(&[".", "b", "..", "c"])),
            PathBuf::from("/a/c")
        );
        // `..` never climbs out of the existing base: that part was resolved by the OS.
        assert_eq!(
            append_folded(PathBuf::from("/a"), &tail(&["..", "c"])),
            PathBuf::from("/a/../c")
        );
    }

    #[test]
    fn empty_folder_error_is_not_reported_as_a_listing_error() {
        let tmp = tempfile::tempdir().expect("tmp");
        let empty = tmp.path().join("e");
        fs::create_dir(&empty).expect("mkdir");
        let e = scan(&empty, &tmp.path().join("o")).expect_err("empty");
        assert!(format!("{e:#}").contains("is empty"), "{e:#}");
        assert!(!format!("{e:#}").contains("cannot list"));
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

    #[test]
    fn marked_out_is_reused_only_if_it_is_a_previous_private_scan() {
        let (tmp, dir) = fixture();
        // A public corpus build output: marker, public manifest, class directories.
        let public = tmp.path().join("corpus-small");
        write(&public, MARKER, b"marker");
        write(
            &public,
            "manifest.json",
            b"{\"classes\":{},\"profile\":\"small\"}\n",
        );
        write(&public, "build-info.json", b"{}\n");
        write(&public, "photo-jpeg/a.jpg", b"\xFF\xD8\xFFx");
        let before = contents(&public);
        let e = scan(&dir, &public).expect_err("public corpus");
        assert!(format!("{e:#}").contains("photo-jpeg"), "{e:#}");
        assert_eq!(before, contents(&public));

        // Only the three scan files, but a public manifest: refused as well.
        let sneaky = tmp.path().join("sneaky");
        write(&sneaky, MARKER, b"marker");
        write(
            &sneaky,
            "manifest.json",
            b"{\"classes\":{},\"profile\":\"full\"}\n",
        );
        let before = contents(&sneaky);
        let e = scan(&dir, &sneaky).expect_err("public manifest");
        assert!(format!("{e:#}").contains("profile is `full`"), "{e:#}");
        assert_eq!(before, contents(&sneaky));

        // A previous scan output is reused.
        let mine = tmp.path().join("mine");
        scan(&dir, &mine).expect("first");
        scan(&dir, &mine).expect("reuse");
        assert_eq!(read_manifest(&mine).profile, "private");
    }

    #[cfg(unix)]
    #[test]
    fn out_through_a_link_and_dotdot_is_resolved_like_the_kernel_does() {
        let (tmp, dir) = fixture();
        // tmp/lnk -> tmp/data/pics, so tmp/lnk/../x is tmp/data/x, inside the scanned folder.
        let lnk = tmp.path().join("lnk");
        std::os::unix::fs::symlink(dir.join("pics"), &lnk).expect("symlink");
        let out = lnk.join("..").join("x");
        assert_refused_and_untouched(&dir, &out, "inside it");
        assert!(!dir.join("x").exists());
        assert!(!tmp.path().join("x").exists());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_out_is_refused_at_once() {
        let (tmp, dir) = fixture();
        let lnk = tmp.path().join("dangling");
        std::os::unix::fs::symlink(tmp.path().join("gone"), &lnk).expect("symlink");
        let e = scan(&dir, &lnk).expect_err("dangling");
        assert!(
            format!("{e:#}").contains("link whose target does not exist"),
            "{e:#}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn dangling_junction_out_is_refused_at_once() {
        let (tmp, dir) = fixture();
        let target = tmp.path().join("target");
        fs::create_dir(&target).expect("mkdir");
        let junction = tmp.path().join("junction");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .expect("spawn cmd for mklink /J");
        assert!(made.status.success(), "mklink /J failed");
        fs::remove_dir(&target).expect("remove target");
        let e = scan(&dir, &junction).expect_err("dangling junction");
        assert!(
            format!("{e:#}").contains("link whose target does not exist"),
            "{e:#}"
        );
        // The same through a missing child of the dangling junction.
        let e = scan(&dir, &junction.join("child")).expect_err("dangling junction child");
        assert!(
            format!("{e:#}").contains("link whose target does not exist"),
            "{e:#}"
        );
        fs::remove_dir(&junction).expect("remove junction");
    }

    #[cfg(unix)]
    #[test]
    fn unlistable_root_reports_the_os_error() {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, dir) = fixture();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).expect("chmod");
        let result = scan(&dir, &tmp.path().join("out"));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod back");
        if fs::read_dir(&dir).is_ok() {
            // Root ignores permissions: the scan then simply succeeds.
            return;
        }
        let e = result.expect_err("unlistable");
        assert!(
            format!("{e:#}").contains("cannot list scan folder"),
            "{e:#}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_not_followed() {
        let (tmp, dir) = fixture();
        let outside = tmp.path().join("outside");
        write(&outside, "secret.txt", b"secret");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("linked-dir"))
            .arg(&outside)
            .output()
            .expect("spawn cmd for mklink /J");
        assert!(
            status.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&status.stdout)
        );
        let out = tmp.path().join("out");
        let r = scan(&dir, &out).expect("scan");
        assert_eq!(r.files, 6);
        let skipped: Vec<(&str, &str)> = r
            .skipped
            .iter()
            .map(|s| (s.source.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(skipped, [("linked-dir", REASON_LINK)]);
        assert!(outside.join("secret.txt").exists());
        let m = read_manifest(&out);
        assert!(!m
            .classes
            .values()
            .flat_map(|c| &c.files)
            .any(|f| f.path.contains("secret")));
        // Remove the junction itself (not its target) before the temp dir is cleaned up.
        fs::remove_dir(dir.join("linked-dir")).expect("remove junction");
    }

    #[cfg(windows)]
    #[test]
    fn exclusively_locked_files_are_skipped_not_fatal() {
        use std::os::windows::fs::OpenOptionsExt;
        let (tmp, dir) = fixture();
        let locked = dir.join("locked.txt");
        fs::write(&locked, b"x").expect("write");
        let _guard = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&locked)
            .expect("lock");
        let r = scan(&dir, &tmp.path().join("out")).expect("scan");
        assert_eq!(r.files, 6);
        assert!(r.skipped.iter().any(|s| s.source == "locked.txt"));
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
