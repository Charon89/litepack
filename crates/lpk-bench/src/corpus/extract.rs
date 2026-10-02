//! Safe archive extraction with include/exclude globs and a deterministic cap.
//!
//! Guarantees: nothing is written outside the target directory; absolute paths, `..`, drive
//! prefixes (any `:`), non-portable names (see [`check_portable_component`]),
//! symlink/hardlink/device entries and case-insensitive duplicate or file/directory-clashing
//! paths are errors (checked on *every* entry, selected or not, identically on every OS);
//! malformed archives are errors, not panics.
//!
//! Selection order: validate all entries -> drop dirs -> `strip_components` -> include/exclude
//! globs -> sort by path bytes -> cap (`max_files`, then `max_bytes` on declared sizes; the
//! first file that would exceed the byte cap ends the selection).
//!
//! Formats: `zip`, `tar.gz`, `7z` (also a self-extracting `.7z.exe`: the archive starts at the
//! first 7z signature whose start-header CRC is valid) and `gz` (one gzip stream decompressed
//! to a single file whose name the caller supplies).
//!
//! `truncate_files` keeps only the first N bytes of every larger file, cut after the last line
//! break before the cap (or exactly at N when there is none). The extracted size is the
//! truncated size, and `max_bytes` counts it as at most N.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
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
    /// Keep only the first N bytes of larger files (cut at a line break). `None`: keep all.
    pub truncate: Option<u64>,
    /// Output file name for single-file formats (`gz`).
    pub single_name: Option<String>,
    /// tar: skip link entries instead of rejecting the archive.
    pub skip_links: bool,
}

/// Upper bound for a stream whose size is not declared (guards against decompression bombs).
const UNDECLARED_LIMIT: u64 = 16 << 30;

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
            truncate: None,
            single_name: None,
            skip_links: false,
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
            truncate: None,
            single_name: None,
            skip_links: false,
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

/// Check one path component (a file or directory name) against the portable-name rules, which
/// are applied on every OS so the same lock behaves identically everywhere: no control
/// characters, none of `<>:"|?*\`, no trailing `.` or space, no Windows device names
/// (`CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, with or without an extension).
pub fn check_portable_component(c: &str) -> Result<()> {
    ensure!(!c.is_empty(), "empty path component");
    if let Some(bad) = c.chars().find(|&ch| {
        (ch as u32) < 0x20
            || ch == '\u{7f}'
            || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\')
    }) {
        bail!("character {bad:?} is not allowed in `{c}`");
    }
    ensure!(
        !c.ends_with('.') && !c.ends_with(' '),
        "`{c}` ends with a dot or space (not portable to Windows)"
    );
    let stem = c
        .split('.')
        .next()
        .unwrap_or(c)
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    ensure!(!reserved, "`{c}` is a reserved Windows device name");
    Ok(())
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
            c => {
                check_portable_component(c).with_context(|| format!("unsafe entry `{name}`"))?;
                parts.push(c.to_string());
            }
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
        let kept = sel.truncate.map_or(c.size, |t| c.size.min(t));
        if let Some(limit) = sel.max_bytes {
            if total.checked_add(kept).is_none_or(|t| t > limit) {
                break;
            }
        }
        total = total.saturating_add(kept);
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
        ArchiveFormat::SevenZ => extract_7z(archive, target, sel),
        ArchiveFormat::Gz => extract_gz(archive, target, sel),
    }
    .with_context(|| format!("extracting {}", archive.display()))?;
    out.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(out)
}

/// Collects validated entries and rejects case-insensitive duplicates, including a file whose
/// name equals another entry's parent directory (`A` next to `a/x`), which Windows rejects.
#[derive(Default)]
struct Lister {
    cands: Vec<Candidate>,
    files: BTreeSet<String>,
    dirs: BTreeSet<String>,
}

impl Lister {
    fn add(&mut self, name: &str, size: u64, ordinal: usize, strip: usize) -> Result<()> {
        let comps = sanitize_path(name)?;
        if comps.len() <= strip {
            return Ok(());
        }
        let comps = &comps[strip..];
        let path = comps.join("/");
        let lower = path.to_lowercase();
        ensure!(
            !self.files.contains(&lower) && !self.dirs.contains(&lower),
            "duplicate (case-insensitive) entry `{path}`"
        );
        let mut prefix = String::new();
        for comp in &comps[..comps.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(&comp.to_lowercase());
            ensure!(
                !self.files.contains(&prefix),
                "entry `{path}` needs a directory that is also a file (case-insensitive clash)"
            );
            self.dirs.insert(prefix.clone());
        }
        self.files.insert(lower);
        self.cands.push(Candidate {
            path,
            size,
            ordinal,
        });
        Ok(())
    }
}

/// Validate a whole file listing before anything is fetched: every path must be safe and
/// portable, and no two may collide case-insensitively (as on Windows), as a duplicate or as a
/// file that is also another path's directory. Identical on every OS.
pub fn check_listing(paths: &[String]) -> Result<()> {
    let mut lister = Lister::default();
    for (i, p) in paths.iter().enumerate() {
        ensure!(
            !sanitize_path(p)?.is_empty(),
            "listed path `{p}` names no file"
        );
        lister.add(p, 0, i, 0)?;
    }
    Ok(())
}

/// Write one file below `target`, creating parents; fails if it already exists.
///
/// `declared` is the size the archive claims (`None` for a bare stream). With `truncate`, a
/// stream longer than that is cut after its last line break within the first `truncate` bytes
/// (exactly at `truncate` when it has none) and the hash covers the kept bytes only.
fn write_entry(
    target: &Path,
    rel: &str,
    reader: &mut dyn Read,
    declared: Option<u64>,
    truncate: Option<u64>,
) -> Result<Extracted> {
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
        .read(true)
        .write(true)
        .create_new(true)
        .open(&dest)
        .with_context(|| format!("creating {}", dest.display()))?;
    let read_max = match (declared, truncate) {
        (Some(d), Some(t)) => d.min(t),
        (Some(d), None) => d,
        (None, Some(t)) => t,
        (None, None) => UNDECLARED_LIMIT,
    };
    let mut hasher = blake3::Hasher::new();
    let mut limited = reader.take(read_max.saturating_add(1));
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    // End (exclusive) of the last line break among the first `truncate` bytes.
    let mut last_break = 0u64;
    loop {
        let n = limited
            .read(&mut buf)
            .with_context(|| format!("reading entry `{rel}`"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        if let Some(t) = truncate {
            let visible = (t.saturating_sub(total) as usize).min(n);
            if let Some(i) = buf[..visible].iter().rposition(|&b| b == b'\n') {
                last_break = total + i as u64 + 1;
            }
        }
        total += n as u64;
    }
    let cut = truncate.is_some_and(|t| total > t);
    if !cut {
        match declared {
            Some(d) => ensure!(
                total == d,
                "entry `{rel}` has {total} bytes but declares {d}"
            ),
            None => ensure!(
                total <= UNDECLARED_LIMIT,
                "entry `{rel}` is larger than {UNDECLARED_LIMIT} bytes"
            ),
        }
        return Ok(Extracted {
            path: rel.to_string(),
            bytes: total,
            blake3: hasher.finalize().to_hex().to_string(),
        });
    }
    let keep = if last_break > 0 {
        last_break
    } else {
        truncate.unwrap_or(total)
    };
    file.set_len(keep)
        .with_context(|| format!("truncating {}", dest.display()))?;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = blake3::Hasher::new();
    let n = std::io::copy(&mut (&mut file).take(keep), &mut hasher)?;
    ensure!(n == keep, "truncated file `{rel}` is shorter than expected");
    Ok(Extracted {
        path: rel.to_string(),
        bytes: keep,
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
        written.push(write_entry(
            target,
            &c.path,
            &mut e,
            Some(c.size),
            sel.truncate,
        )?);
    }
    Ok(written)
}

fn tar_entries(
    archive: &Path,
) -> Result<tar::Archive<flate2::read::MultiGzDecoder<BufReader<File>>>> {
    let file = File::open(archive)?;
    Ok(tar::Archive::new(flate2::read::MultiGzDecoder::new(
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
            T::Symlink | T::Link if sel.skip_links => {
                sanitize_path(&name)?;
            }
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
                written.push(write_entry(
                    target,
                    &c.path,
                    &mut entry,
                    Some(c.size),
                    sel.truncate,
                )?);
            }
        }
    }
    ensure!(
        written.len() == picked.len(),
        "archive changed between passes"
    );
    Ok(written)
}

/// 7z start-header signature.
const SEVENZ_SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];

/// Offset of the 7z archive inside `file`: 0 for a plain `.7z`, the end of the stub for a
/// self-extracting executable. A signature only counts when the start-header CRC (bytes 8..12
/// over bytes 12..32) matches, so a stray signature inside the stub is skipped.
fn find_7z_start(file: &mut File) -> Result<u64> {
    const CHUNK: usize = 1 << 20;
    let len = file.metadata()?.len();
    let mut pos = 0u64;
    let mut buf = Vec::with_capacity(CHUNK + 32);
    while pos < len {
        file.seek(SeekFrom::Start(pos))?;
        buf.clear();
        (&mut *file).take(CHUNK as u64 + 32).read_to_end(&mut buf)?;
        for i in 0..buf.len().saturating_sub(31) {
            if buf[i..i + 6] != SEVENZ_SIGNATURE || i >= CHUNK && pos + (i as u64) < len {
                continue;
            }
            let header = &buf[i..i + 32];
            let stored = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
            let mut crc = flate2::Crc::new();
            crc.update(&header[12..32]);
            if header[6] == 0 && crc.sum() == stored {
                return Ok(pos + i as u64);
            }
        }
        pos += CHUNK as u64;
    }
    bail!("no 7z signature with a valid start header found")
}

/// A `Read + Seek` view of `inner` that starts at `base`.
struct OffsetReader<R> {
    inner: R,
    base: u64,
}

impl<R: Read> Read for OffsetReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for OffsetReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let abs = match pos {
            SeekFrom::Start(p) => SeekFrom::Start(self.base.checked_add(p).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek overflow")
            })?),
            other => other,
        };
        let at = self.inner.seek(abs)?;
        at.checked_sub(self.base).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before archive start",
            )
        })
    }
}

fn extract_7z(archive: &Path, target: &Path, sel: &Selection) -> Result<Vec<Extracted>> {
    use sevenz_rust2::{ArchiveReader, Error as SzError, Password};
    let mut file = File::open(archive)?;
    let base = find_7z_start(&mut file)?;
    file.seek(SeekFrom::Start(base))?;
    let mut reader = ArchiveReader::new(OffsetReader { inner: file, base }, Password::empty())
        .context("not a readable 7z archive")?;

    let mut lister = Lister::default();
    for (i, e) in reader.archive().files.iter().enumerate() {
        let name = e.name();
        if e.is_anti_item() {
            bail!("anti-item entry `{name}` is not allowed");
        }
        // Windows reparse point (symlink, junction) or a Unix mode stored in the high bits.
        let attrs = e.windows_attributes();
        if attrs & 0x400 != 0 {
            bail!("reparse-point entry `{name}` is not allowed");
        }
        if attrs & 0x8000 != 0 {
            match (attrs >> 16) & 0o170_000 {
                0 | 0o100_000 | 0o040_000 => {}
                0o120_000 => bail!("symlink entry `{name}` is not allowed"),
                _ => bail!("special file entry `{name}` is not allowed"),
            }
        }
        if e.is_directory() {
            sanitize_path(name)?;
            continue;
        }
        lister.add(name, e.size(), i, sel.strip_components)?;
    }
    let picked = pick(lister.cands, sel);
    // Entries are decoded in archive order (solid blocks cannot be skipped into), so map the
    // raw names to their candidates and drain everything that is not (fully) read.
    let wanted: std::collections::HashMap<String, &Candidate> = picked
        .iter()
        .filter_map(|c| {
            reader
                .archive()
                .files
                .get(c.ordinal)
                .map(|e| (e.name().to_string(), c))
        })
        .collect();
    let mut written = Vec::new();
    let mut failure: Option<anyhow::Error> = None;
    let result = reader.for_each_entries(|entry, data| {
        if let Some(c) = wanted.get(entry.name()) {
            match write_entry(target, &c.path, data, Some(c.size), sel.truncate) {
                Ok(x) => written.push(x),
                Err(e) => {
                    failure = Some(e);
                    return Err(SzError::Other("entry could not be written".into()));
                }
            }
        }
        // Verifies the entry's CRC when it was not read to the end and keeps the stream aligned.
        std::io::copy(data, &mut std::io::sink())?;
        Ok(true)
    });
    if let Some(e) = failure {
        return Err(e);
    }
    result.context("decoding 7z entries")?;
    ensure!(
        written.len() == picked.len(),
        "7z archive yielded {} of {} selected entries",
        written.len(),
        picked.len()
    );
    Ok(written)
}

fn extract_gz(archive: &Path, target: &Path, sel: &Selection) -> Result<Vec<Extracted>> {
    let name = sel
        .single_name
        .as_deref()
        .context("single-file format needs an output name")?;
    let comps = sanitize_path(name)?;
    ensure!(
        comps.len() == 1,
        "output name `{name}` must be one component"
    );
    let mut decoder = flate2::read::MultiGzDecoder::new(BufReader::new(File::open(archive)?));
    let one = write_entry(target, &comps[0], &mut decoder, None, sel.truncate)
        .context("not a readable gzip stream")?;
    Ok(vec![one])
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

    #[test]
    fn file_and_directory_clashes_fail_in_either_order() {
        for entries in [
            [("A", &b"1"[..]), ("a/x", b"2")],
            [("a/x", b"2"), ("A", b"1")],
            [("dir/File", b"1"), ("DIR/file/deeper", b"2")],
        ] {
            let z = zip_bytes(&entries);
            assert!(run(ArchiveFormat::Zip, &z, &Selection::all()).1.is_err());
        }
        // Same directory, different files is fine.
        let z = zip_bytes(&[("a/x", b"1"), ("A/y", b"2")]);
        assert!(run(ArchiveFormat::Zip, &z, &Selection::all()).1.is_ok());
    }

    #[test]
    fn non_portable_names_are_rejected_on_every_os() {
        for bad in [
            "CON",
            "nul.txt",
            "dir/AUX",
            "com1",
            "LPT9.log",
            "a.",
            "a ",
            "dir./x",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "tab\there",
            "bell\u{7}",
            "del\u{7f}",
            "COM1.tar.gz",
        ] {
            assert!(sanitize_path(bad).is_err(), "{bad:?} must be rejected");
            let z = zip_bytes(&[(bad, b"x")]);
            assert!(
                run(ArchiveFormat::Zip, &z, &Selection::all()).1.is_err(),
                "{bad:?}"
            );
        }
        for ok in [
            "COM",
            "COM10",
            "console.txt",
            "nulls",
            ".gitignore",
            "a b",
            "a.b.c",
            "LPT0",
        ] {
            assert!(sanitize_path(ok).is_ok(), "{ok:?} must be accepted");
        }
        assert!(check_portable_component("").is_err());
    }

    #[test]
    fn huge_declared_sizes_do_not_overflow() {
        let mk = |p: &str, o: usize| Candidate {
            path: p.into(),
            size: u64::MAX,
            ordinal: o,
        };
        let sel = Selection::new(&[], &[], None, None, 0).expect("sel");
        assert_eq!(pick(vec![mk("a", 0), mk("b", 1)], &sel).len(), 2);
        let sel = Selection::new(&[], &[], None, Some(u64::MAX), 0).expect("sel");
        assert_eq!(pick(vec![mk("a", 0), mk("b", 1)], &sel).len(), 1);
    }

    #[test]
    fn multi_member_gzip_is_read_completely() {
        let mut b = tar::Builder::new(Vec::new());
        for (n, d) in [("one.txt", &b"1111"[..]), ("two.txt", b"2222")] {
            let mut h = tar::Header::new_gnu();
            h.set_path(n).expect("path");
            h.set_size(d.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append(&h, d).expect("append");
        }
        let tar_bytes = b.into_inner().expect("tar");
        // Split after the first entry (header block + one data block) into two gzip members.
        let gz = |part: &[u8]| {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(part).expect("gz");
            e.finish().expect("finish")
        };
        let mut two_members = gz(&tar_bytes[..1024]);
        two_members.extend(gz(&tar_bytes[1024..]));
        let r = run(ArchiveFormat::TarGz, &two_members, &Selection::all())
            .1
            .expect("extract");
        assert_eq!(paths(&r), ["one.txt", "two.txt"]);
    }

    fn sevenz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).expect("7z writer");
        for (name, data) in entries {
            w.push_archive_entry(
                sevenz_rust2::ArchiveEntry::new_file(name),
                Some(Cursor::new(data.to_vec())),
            )
            .expect("push");
        }
        w.finish().expect("finish").into_inner()
    }

    #[test]
    fn sevenz_roundtrip_selection_and_skipped_entries() {
        let z = sevenz_bytes(&[
            ("top/b.txt", b"bb"),
            ("top/skip.bin", &[7u8; 5000]),
            ("top/a.txt", b"a"),
        ]);
        let sel = Selection::new(&["*.txt".into()], &[], None, None, 1).expect("sel");
        let (dir, r) = run(ArchiveFormat::SevenZ, &z, &sel);
        let r = r.expect("extract");
        assert_eq!(paths(&r), ["a.txt", "b.txt"]);
        assert_eq!(r[1].blake3, blake3::hash(b"bb").to_hex().to_string());
        assert_eq!(
            std::fs::read(dir.path().join("out/b.txt")).expect("read"),
            b"bb"
        );
        assert!(!dir.path().join("out/skip.bin").exists());
    }

    #[test]
    fn sevenz_self_extracting_stub_is_skipped_by_signature_and_crc() {
        let z = sevenz_bytes(&[("x/data.txt", b"payload")]);
        let mut sfx = b"MZ stub ".to_vec();
        // A stray signature inside the stub must not be taken for the archive.
        sfx.extend_from_slice(&SEVENZ_SIGNATURE);
        sfx.extend_from_slice(&[0u8; 40]);
        sfx.extend(std::iter::repeat_n(0x90u8, 3000));
        sfx.extend_from_slice(&z);
        let (_dir, r) = run(ArchiveFormat::SevenZ, &sfx, &Selection::all());
        assert_eq!(paths(&r.expect("extract")), ["x/data.txt"]);
        assert!(run(
            ArchiveFormat::SevenZ,
            b"MZ no archive here",
            &Selection::all()
        )
        .1
        .is_err());
        // Truncated archives are errors, never panics.
        for cut in [0, 10, z.len() / 2, z.len() - 1] {
            assert!(run(ArchiveFormat::SevenZ, &z[..cut], &Selection::all())
                .1
                .is_err());
        }
    }

    #[test]
    fn gz_decompresses_to_one_named_file() {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(b"disk image bytes").expect("gz");
        let gz = e.finish().expect("finish");
        let mut sel = Selection::all();
        assert!(run(ArchiveFormat::Gz, &gz, &sel).1.is_err(), "needs a name");
        sel.single_name = Some("disk.img".into());
        let (dir, r) = run(ArchiveFormat::Gz, &gz, &sel);
        let r = r.expect("extract");
        assert_eq!(paths(&r), ["disk.img"]);
        assert_eq!(r[0].bytes, 16);
        assert_eq!(
            std::fs::read(dir.path().join("out/disk.img")).expect("read"),
            b"disk image bytes"
        );
        sel.single_name = Some("../evil".into());
        assert!(run(ArchiveFormat::Gz, &gz, &sel).1.is_err());
        assert!(run(ArchiveFormat::Gz, b"not gzip", &{
            let mut s = Selection::all();
            s.single_name = Some("x".into());
            s
        })
        .1
        .is_err());
    }

    #[test]
    fn truncation_cuts_at_the_last_line_break_before_the_cap() {
        let z = zip_bytes(&[
            ("log.txt", b"aaa\nbbb\nccc\nddd\n"),
            ("nobreak.bin", b"abcdefghij"),
            ("short.txt", b"x\n"),
        ]);
        let mut sel = Selection::all();
        sel.truncate = Some(9);
        let (dir, r) = run(ArchiveFormat::Zip, &z, &sel);
        let r = r.expect("extract");
        assert_eq!(paths(&r), ["log.txt", "nobreak.bin", "short.txt"]);
        // First 9 bytes are "aaa\nbbb\ncc": cut after the second break.
        assert_eq!(r[0].bytes, 8);
        assert_eq!(
            std::fs::read(dir.path().join("out/log.txt")).expect("read"),
            b"aaa\nbbb\n"
        );
        assert_eq!(
            r[0].blake3,
            blake3::hash(b"aaa\nbbb\n").to_hex().to_string()
        );
        assert_eq!(r[1].bytes, 9, "no break: cut exactly at the cap");
        assert_eq!(r[2].bytes, 2, "small files are untouched");
        // A chunk boundary inside the cap must not confuse the break search.
        let big: Vec<u8> = (0..300_000u32)
            .flat_map(|i| format!("line {i}\n").into_bytes())
            .collect();
        let z = zip_bytes(&[("big.log", &big)]);
        sel.truncate = Some(200_000);
        let r = run(ArchiveFormat::Zip, &z, &sel).1.expect("extract");
        let kept = &big[..r[0].bytes as usize];
        assert!(kept.len() <= 200_000 && kept.ends_with(b"\n"));
        assert!(big[kept.len()..200_000].iter().all(|&b| b != b'\n'));
        assert_eq!(r[0].blake3, blake3::hash(kept).to_hex().to_string());
        // The byte cap counts the truncated size.
        let z = zip_bytes(&[("a", &[b'x'; 100][..]), ("b", &[b'y'; 100][..])]);
        let mut sel = Selection::new(&[], &[], None, Some(20), 0).expect("sel");
        sel.truncate = Some(10);
        assert_eq!(
            paths(&run(ArchiveFormat::Zip, &z, &sel).1.expect("x")),
            ["a", "b"]
        );
    }

    #[test]
    fn listing_check_catches_case_and_file_directory_clashes() {
        let ok = |v: &[&str]| check_listing(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(ok(&["a/x.txt", "b/x.txt", "a/y.txt"]).is_ok());
        assert!(ok(&["Photo.jpg", "photo.JPG"]).is_err());
        assert!(ok(&["a", "A/x"]).is_err());
        assert!(ok(&["a/x", "a"]).is_err());
        assert!(ok(&["../x"]).is_err());
        assert!(ok(&["./"]).is_err());
    }

    #[test]
    fn tar_links_can_be_skipped_when_asked() {
        let t = tar_gz_bytes(&[
            ("src/zstd", tar::EntryType::Regular, b"bin", ""),
            ("src/unzstd", tar::EntryType::Symlink, b"", "zstd"),
            ("src/zstdcat", tar::EntryType::Link, b"", "src/zstd"),
        ]);
        let mut sel = Selection::all();
        assert!(run(ArchiveFormat::TarGz, &t, &sel).1.is_err());
        sel.skip_links = true;
        assert_eq!(
            paths(&run(ArchiveFormat::TarGz, &t, &sel).1.expect("x")),
            ["src/zstd"]
        );
        let evil = tar_gz_bytes(&[("../evil", tar::EntryType::Symlink, b"", "zstd")]);
        assert!(
            run(ArchiveFormat::TarGz, &evil, &sel).1.is_err(),
            "names still validated"
        );
    }
}
