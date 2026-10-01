//! Safe archive extraction with include/exclude globs and a deterministic cap.
//!
//! Guarantees: nothing is written outside the target directory; absolute paths, `..`, drive
//! prefixes (any `:`), symlink/hardlink/device entries and case-insensitive duplicate paths are
//! errors (checked on *every* entry, selected or not); malformed archives are errors, not panics.
//!
//! Selection order: validate all entries -> drop dirs -> `strip_components` -> include/exclude
//! globs -> sort by path bytes -> cap (`max_files`, then `max_bytes` on declared sizes; the
//! first file that would exceed the byte cap ends the selection).

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use globset::GlobSet;

use super::registry::{compile_globs, ArchiveFormat};

/// Which entries to extract.
#[derive(Debug)]
pub struct Selection {
    include: GlobSet,
    include_empty: bool,
    exclude: GlobSet,
    pub max_files: Option<usize>,
    pub max_bytes: Option<u64>,
    pub strip_components: usize,
}

impl Selection {
    pub fn new(
        include: &[String],
        exclude: &[String],
        max_files: Option<usize>,
        max_bytes: Option<u64>,
        strip_components: usize,
    ) -> Result<Selection> {
        Ok(Selection {
            include: compile_globs(include)?,
            include_empty: include.is_empty(),
            exclude: compile_globs(exclude)?,
            max_files,
            max_bytes,
            strip_components,
        })
    }

    /// Select everything.
    #[cfg(test)]
    pub fn all() -> Selection {
        Selection {
            include: GlobSet::empty(),
            include_empty: true,
            exclude: GlobSet::empty(),
            max_files: None,
            max_bytes: None,
            strip_components: 0,
        }
    }
}

/// A file entry that passed validation, with its identity inside the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Path after `strip_components`, `/`-separated.
    pub path: String,
    /// Declared uncompressed size.
    pub size: u64,
    /// Entry index (zip) or position in the stream (tar).
    pub ordinal: usize,
}

/// A file that was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// Path relative to the target directory, `/`-separated.
    pub path: String,
    pub bytes: u64,
    pub blake3: String,
}

/// Split an archive entry name into safe components, or explain why it is unsafe.
/// An empty result means the entry names the archive root (nothing to write).
pub fn sanitize_path(name: &str) -> Result<Vec<String>> {
    let unified = name.replace('\\', "/");
    if unified.starts_with('/') {
        bail!("absolute path `{name}`");
    }
    let mut parts = Vec::new();
    for comp in unified.split('/') {
        match comp {
            "" | "." => {}
            ".." => bail!("path traversal `{name}`"),
            c if c.contains(':') => bail!("drive prefix or stream marker in `{name}`"),
            c if c.contains(['\0', '\r', '\n']) => bail!("control character in `{name}`"),
            c => parts.push(c.to_string()),
        }
    }
    Ok(parts)
}

/// Apply globs, order and cap to validated candidates. Pure function: this is the determinism
/// guarantee for caps.
pub fn pick(mut cands: Vec<Candidate>, sel: &Selection) -> Vec<Candidate> {
    cands.retain(|c| {
        (sel.include_empty || sel.include.is_match(&c.path)) && !sel.exclude.is_match(&c.path)
    });
    cands.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    let mut total = 0u64;
    let mut out = Vec::new();
    for c in cands {
        if sel.max_files.is_some_and(|m| out.len() >= m) {
            break;
        }
        if let Some(limit) = sel.max_bytes {
            if total.saturating_add(c.size) > limit {
                break;
            }
        }
        total += c.size;
        out.push(c);
    }
    out
}

/// Extract `archive` into `target` (which must not contain conflicting files).
/// Returns the written files sorted by path.
pub fn extract(
    format: ArchiveFormat,
    archive: &Path,
    target: &Path,
    sel: &Selection,
) -> Result<Vec<Extracted>> {
    let mut out = match format {
        ArchiveFormat::Zip => extract_zip(archive, target, sel),
        ArchiveFormat::TarGz => extract_tar_gz(archive, target, sel),
    }
    .with_context(|| format!("extracting {}", archive.display()))?;
    out.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(out)
}

/// Collects validated entries and rejects case-insensitive duplicates.
#[derive(Default)]
struct Lister {
    cands: Vec<Candidate>,
    seen: BTreeSet<String>,
}

impl Lister {
    fn add(&mut self, name: &str, size: u64, ordinal: usize, strip: usize) -> Result<()> {
        let comps = sanitize_path(name)?;
        if comps.len() <= strip {
            return Ok(());
        }
        let path = comps[strip..].join("/");
        ensure!(
            self.seen.insert(path.to_lowercase()),
            "duplicate (case-insensitive) entry `{path}`"
        );
        self.cands.push(Candidate {
            path,
            size,
            ordinal,
        });
        Ok(())
    }
}

/// Write one file below `target`, creating parents; fails if it already exists.
fn write_entry(target: &Path, rel: &str, reader: &mut dyn Read, size: u64) -> Result<Extracted> {
    let mut dest = target.to_path_buf();
    for comp in rel.split('/') {
        dest.push(comp);
    }
    ensure!(
        dest.starts_with(target),
        "internal error: `{rel}` escapes the target"
    );
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(&dest)
        .with_context(|| format!("creating {}", dest.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut limited = reader.take(size.saturating_add(1));
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = limited
            .read(&mut buf)
            .with_context(|| format!("reading entry `{rel}`"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    ensure!(
        total == size,
        "entry `{rel}` has {total} bytes but declares {size}"
    );
    Ok(Extracted {
        path: rel.to_string(),
        bytes: total,
        blake3: hasher.finalize().to_hex().to_string(),
    })
}

fn extract_zip(archive: &Path, target: &Path, sel: &Selection) -> Result<Vec<Extracted>> {
    let file = File::open(archive)?;
    let mut za = zip::ZipArchive::new(BufReader::new(file)).context("not a readable zip")?;
    let mut lister = Lister::default();
    for i in 0..za.len() {
        let e = za
            .by_index_raw(i)
            .with_context(|| format!("zip entry #{i}"))?;
        let name = e.name().to_string();
        let kind = e.unix_mode().map(|m| m & 0o170_000);
        if e.is_symlink() || kind == Some(0o120_000) {
            bail!("symlink entry `{name}` is not allowed");
        }
        if !matches!(kind, None | Some(0) | Some(0o100_000) | Some(0o040_000)) {
            bail!("special file entry `{name}` is not allowed");
        }
        if e.is_dir() {
            sanitize_path(&name)?;
            continue;
        }
        lister.add(&name, e.size(), i, sel.strip_components)?;
    }
    let mut written = Vec::new();
    for c in pick(lister.cands, sel) {
        let mut e = za
            .by_index(c.ordinal)
            .with_context(|| format!("zip entry `{}`", c.path))?;
        written.push(write_entry(target, &c.path, &mut e, c.size)?);
    }
    Ok(written)
}

fn tar_entries(archive: &Path) -> Result<tar::Archive<flate2::read::GzDecoder<BufReader<File>>>> {
    let file = File::open(archive)?;
    Ok(tar::Archive::new(flate2::read::GzDecoder::new(
        BufReader::new(file),
    )))
}

fn extract_tar_gz(archive: &Path, target: &Path, sel: &Selection) -> Result<Vec<Extracted>> {
    use tar::EntryType as T;
    let mut lister = Lister::default();
    // Pass 1: validate and list.
    let mut tar = tar_entries(archive)?;
    for (i, entry) in tar.entries().context("not a readable tar.gz")?.enumerate() {
        let entry = entry.with_context(|| format!("tar entry #{i}"))?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        match entry.header().entry_type() {
            T::Regular | T::Continuous => {
                lister.add(&name, entry.size(), i, sel.strip_components)?
            }
            T::Directory => {
                sanitize_path(&name)?;
            }
            T::XGlobalHeader | T::XHeader | T::GNULongName | T::GNULongLink => {}
            T::Symlink | T::Link => bail!("link entry `{name}` is not allowed"),
            other => bail!("special entry `{name}` ({other:?}) is not allowed"),
        }
    }
    // Tar stops at its end-of-archive blocks; read the rest so a truncated or corrupt gzip
    // stream is an error instead of a silently shorter archive.
    std::io::copy(&mut tar.into_inner(), &mut std::io::sink())
        .context("truncated or corrupt gzip stream")?;
    let picked = pick(lister.cands, sel);
    let by_ordinal: std::collections::BTreeMap<usize, &Candidate> =
        picked.iter().map(|c| (c.ordinal, c)).collect();
    // Pass 2: extract the selected entries.
    let mut written = Vec::new();
    if !by_ordinal.is_empty() {
        for (i, entry) in tar_entries(archive)?.entries()?.enumerate() {
            let mut entry = entry.with_context(|| format!("tar entry #{i}"))?;
            if let Some(c) = by_ordinal.get(&i) {
                written.push(write_entry(target, &c.path, &mut entry, c.size)?);
            }
        }
    }
    ensure!(
        written.len() == picked.len(),
        "archive changed between passes"
    );
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            w.start_file(*name, opts).expect("start");
            w.write_all(data).expect("write");
        }
        w.finish().expect("finish").into_inner()
    }

    fn tar_gz_bytes(entries: &[(&str, tar::EntryType, &[u8], &str)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, kind, data, link) in entries {
            let mut h = tar::Header::new_gnu();
            // Write the raw name so unsafe names can be constructed on purpose.
            h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            if !link.is_empty() {
                h.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
            }
            h.set_entry_type(*kind);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append(&h, *data).expect("append");
        }
        let tar = b.into_inner().expect("tar");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).expect("gz");
        gz.finish().expect("gz finish")
    }

    fn run(
        format: ArchiveFormat,
        bytes: &[u8],
        sel: &Selection,
    ) -> (tempfile::TempDir, Result<Vec<Extracted>>) {
        let dir = tempfile::tempdir().expect("tmp");
        let archive = dir.path().join("a.bin");
        std::fs::write(&archive, bytes).expect("write");
        let target = dir.path().join("out");
        std::fs::create_dir(&target).expect("mkdir");
        let r = extract(format, &archive, &target, sel);
        (dir, r)
    }

    fn paths(r: &[Extracted]) -> Vec<&str> {
        r.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn sanitize_rejects_unsafe_names() {
        for bad in [
            "/etc/passwd",
            "\\evil",
            "../x",
            "a/../../x",
            "a\\..\\x",
            "C:/x",
            "C:x",
            "a/b:s",
            "a\0b",
        ] {
            assert!(sanitize_path(bad).is_err(), "{bad:?} must be rejected");
        }
        assert_eq!(sanitize_path("./a//b/./c/").expect("ok"), ["a", "b", "c"]);
        assert_eq!(sanitize_path("a\\b").expect("ok"), ["a", "b"]);
        assert!(sanitize_path("./").expect("root").is_empty());
    }

    #[test]
    fn zip_roundtrip_with_hashes_and_nested_dirs() {
        let z = zip_bytes(&[
            ("b/two.txt", b"22"),
            ("a.txt", b"1"),
            ("b/c/three.bin", b"333"),
        ]);
        let (dir, r) = run(ArchiveFormat::Zip, &z, &Selection::all());
        let r = r.expect("extract");
        assert_eq!(paths(&r), ["a.txt", "b/c/three.bin", "b/two.txt"]);
        assert_eq!(r[1].bytes, 3);
        assert_eq!(r[1].blake3, blake3::hash(b"333").to_hex().to_string());
        assert_eq!(
            std::fs::read(dir.path().join("out/b/c/three.bin")).expect("read"),
            b"333"
        );
    }

    #[test]
    fn zip_slip_absolute_and_drive_entries_are_rejected_and_nothing_escapes() {
        for bad in [
            "../evil.txt",
            "a/../../evil.txt",
            "/abs.txt",
            "C:/drive.txt",
            "..\\evil.txt",
        ] {
            let z = zip_bytes(&[("ok.txt", b"x"), (bad, b"pwn")]);
            let (dir, r) = run(ArchiveFormat::Zip, &z, &Selection::all());
            assert!(r.is_err(), "{bad} must fail");
            assert!(!dir.path().join("evil.txt").exists());
            assert!(!dir.path().join("out/evil.txt").exists());
        }
    }

    #[test]
    fn unsafe_entries_are_rejected_even_when_excluded_by_globs() {
        let z = zip_bytes(&[("ok.txt", b"x"), ("../evil.txt", b"pwn")]);
        let sel = Selection::new(&["ok.txt".into()], &[], None, None, 0).expect("sel");
        assert!(run(ArchiveFormat::Zip, &z, &sel).1.is_err());
    }

    #[test]
    fn zip_symlink_entry_is_rejected() {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        w.add_symlink("link", "/etc/passwd", opts).expect("symlink");
        let z = w.finish().expect("finish").into_inner();
        let err = run(ArchiveFormat::Zip, &z, &Selection::all())
            .1
            .expect_err("symlink");
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
    }

    #[test]
    fn malformed_archives_error_without_panicking() {
        let z = zip_bytes(&[("a.txt", b"hello hello hello")]);
        for cut in [0, 1, 10, z.len() / 2, z.len() - 1] {
            assert!(
                run(ArchiveFormat::Zip, &z[..cut], &Selection::all())
                    .1
                    .is_err(),
                "cut {cut}"
            );
        }
        assert!(run(
            ArchiveFormat::Zip,
            b"this is not a zip file at all",
            &Selection::all()
        )
        .1
        .is_err());
        let t = tar_gz_bytes(&[("a.txt", tar::EntryType::Regular, b"hello", "")]);
        for cut in [0, 5, t.len() / 2, t.len() - 1] {
            assert!(
                run(ArchiveFormat::TarGz, &t[..cut], &Selection::all())
                    .1
                    .is_err(),
                "cut {cut}"
            );
        }
        assert!(run(
            ArchiveFormat::TarGz,
            b"garbage garbage garbage",
            &Selection::all()
        )
        .1
        .is_err());
    }

    #[test]
    fn tar_gz_roundtrip_and_strip_components() {
        let t = tar_gz_bytes(&[
            ("top/", tar::EntryType::Directory, b"", ""),
            ("top/x/b.txt", tar::EntryType::Regular, b"bb", ""),
            ("top/a.txt", tar::EntryType::Regular, b"a", ""),
            ("README", tar::EntryType::Regular, b"r", ""),
        ]);
        let sel = Selection::new(&[], &[], None, None, 1).expect("sel");
        let (dir, r) = run(ArchiveFormat::TarGz, &t, &sel);
        assert_eq!(paths(&r.expect("extract")), ["a.txt", "x/b.txt"]);
        assert!(dir.path().join("out/x/b.txt").is_file());
        assert!(!dir.path().join("out/README").exists());
    }

    #[test]
    fn tar_links_and_traversal_are_rejected() {
        let sym = tar_gz_bytes(&[("l", tar::EntryType::Symlink, b"", "/etc/passwd")]);
        assert!(run(ArchiveFormat::TarGz, &sym, &Selection::all())
            .1
            .is_err());
        let hard = tar_gz_bytes(&[("l", tar::EntryType::Link, b"", "other")]);
        assert!(run(ArchiveFormat::TarGz, &hard, &Selection::all())
            .1
            .is_err());
        for bad in ["../evil", "/abs", "a/../../evil", "C:/x"] {
            let t = tar_gz_bytes(&[(bad, tar::EntryType::Regular, b"x", "")]);
            let (dir, r) = run(ArchiveFormat::TarGz, &t, &Selection::all());
            assert!(r.is_err(), "{bad}");
            assert!(!dir.path().join("evil").exists());
        }
    }

    #[test]
    fn globs_filter_and_caps_are_deterministic_in_sorted_order() {
        // Archive order is deliberately not sorted order.
        let entries: Vec<(String, Vec<u8>)> = ["d.txt", "a.txt", "c.log", "b.txt", "sub/e.txt"]
            .iter()
            .map(|n| (n.to_string(), vec![b'x'; 10]))
            .collect();
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        let z = zip_bytes(&refs);

        let sel = Selection::new(
            &["**/*.txt".into(), "*.txt".into()],
            &["b.txt".into()],
            Some(2),
            None,
            0,
        )
        .expect("sel");
        assert_eq!(
            paths(&run(ArchiveFormat::Zip, &z, &sel).1.expect("x")),
            ["a.txt", "d.txt"]
        );

        // `*` does not cross `/`.
        let sel = Selection::new(&["*.txt".into()], &[], None, None, 0).expect("sel");
        assert_eq!(
            paths(&run(ArchiveFormat::Zip, &z, &sel).1.expect("x")),
            ["a.txt", "b.txt", "d.txt"]
        );

        // Byte cap: 25 bytes admits two 10-byte files, in sorted order.
        let sel = Selection::new(&[], &[], None, Some(25), 0).expect("sel");
        assert_eq!(
            paths(&run(ArchiveFormat::Zip, &z, &sel).1.expect("x")),
            ["a.txt", "b.txt"]
        );

        // Same answer from the same bytes, every time, and for tar.gz in a different order.
        let t = tar_gz_bytes(&[
            ("sub/e.txt", tar::EntryType::Regular, &[b'x'; 10], ""),
            ("b.txt", tar::EntryType::Regular, &[b'x'; 10], ""),
            ("c.log", tar::EntryType::Regular, &[b'x'; 10], ""),
            ("a.txt", tar::EntryType::Regular, &[b'x'; 10], ""),
            ("d.txt", tar::EntryType::Regular, &[b'x'; 10], ""),
        ]);
        assert_eq!(
            paths(&run(ArchiveFormat::TarGz, &t, &sel).1.expect("x")),
            ["a.txt", "b.txt"]
        );
    }

    #[test]
    fn pick_is_a_pure_function_of_paths() {
        let mk = |p: &str, o: usize| Candidate {
            path: p.into(),
            size: 1,
            ordinal: o,
        };
        let sel = Selection::new(&[], &[], Some(2), None, 0).expect("sel");
        let a = pick(vec![mk("z", 0), mk("a", 1), mk("m", 2)], &sel);
        let b = pick(vec![mk("m", 9), mk("z", 5), mk("a", 3)], &sel);
        let names = |v: &[Candidate]| v.iter().map(|c| c.path.clone()).collect::<Vec<_>>();
        assert_eq!(names(&a), ["a", "m"]);
        assert_eq!(names(&a), names(&b));
    }

    #[test]
    fn case_insensitive_duplicates_are_rejected() {
        let z = zip_bytes(&[("A.txt", b"1"), ("a.TXT", b"2")]);
        assert!(run(ArchiveFormat::Zip, &z, &Selection::all()).1.is_err());
    }
}
