//! A deterministic tar of a class (or of one folder of it), made in process.
//!
//! Entries are the manifest's files in path order, named by their path inside the class with `/`
//! separators; every header has mtime 0, uid and gid 0, mode 0644 and empty user and group names.
//! Two calls on the same corpus give identical bytes on every operating system. Each file is
//! verified against the manifest as it is read.

#![allow(dead_code)] // shared helpers for probes that land in later tasks

use std::io::Write;

use anyhow::{Context as _, Result};

use super::Ctx;

/// What a tar holds. Headers, padding and long-name entries are a large share of `tar_bytes` for
/// classes of many small files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TarStats {
    pub files: u64,
    /// Bytes of file content.
    pub content_bytes: u64,
    /// Bytes of the whole tar stream.
    pub tar_bytes: u64,
}

struct CountingWriter<W: Write> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Write the tar of the files of `class` whose class-relative path is inside `folder`
/// (`None`: the whole class) to `out`. Returns the counts of what was written. This tar is not
/// byte-identical to the baseline `store` tool's (a different writer, GNU long-name entries,
/// fixed metadata): compare it with tool results only by what it contains.
pub fn write_tar<W: Write>(
    ctx: &Ctx<'_>,
    class: &str,
    folder: Option<&str>,
    out: W,
) -> Result<TarStats> {
    write_tar_named(ctx, class, folder, false, out)
}

/// Like [`write_tar`] for a folder, with entries named relative to the folder (the folder's own
/// name is not part of any entry name), so that two folders with the same content give the same
/// bytes. Used for the version tars of `probe dedup`.
pub fn write_tar_relative<W: Write>(
    ctx: &Ctx<'_>,
    class: &str,
    folder: &str,
    out: W,
) -> Result<TarStats> {
    write_tar_named(ctx, class, Some(folder), true, out)
}

/// [`write_tar_relative`] into memory.
pub fn tar_bytes_relative(ctx: &Ctx<'_>, class: &str, folder: &str) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    write_tar_relative(ctx, class, folder, &mut v)?;
    Ok(v)
}

fn write_tar_named<W: Write>(
    ctx: &Ctx<'_>,
    class: &str,
    folder: Option<&str>,
    relative_to_folder: bool,
    out: W,
) -> Result<TarStats> {
    let files = ctx.class_files(class).unwrap_or(&[]);
    let prefix = folder.map(|f| format!("{}/", f.trim_matches('/')));
    let mut counter = CountingWriter {
        inner: out,
        written: 0,
    };
    let mut builder = tar::Builder::new(&mut counter);
    let mut stats = TarStats::default();
    for f in files {
        let rel = ctx.rel_path(class, f);
        if let Some(p) = &prefix {
            if !rel.starts_with(p.as_str()) {
                continue;
            }
        }
        let name = match (&prefix, relative_to_folder) {
            (Some(p), true) => rel[p.len()..].to_string(),
            _ => rel,
        };
        let data = ctx.read_file(class, f)?;
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        builder
            .append_data(&mut h, name, &data[..])
            .context("writing a tar entry")?;
        stats.files += 1;
        stats.content_bytes += data.len() as u64;
    }
    builder.finish().context("finishing the tar")?;
    drop(builder);
    stats.tar_bytes = counter.written;
    Ok(stats)
}

/// [`write_tar`] into memory.
pub fn tar_bytes(ctx: &Ctx<'_>, class: &str, folder: Option<&str>) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    write_tar(ctx, class, folder, &mut v)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{tiny_class_corpus, with_ctx};
    use super::*;

    #[test]
    fn two_calls_give_identical_bytes_with_fixed_metadata() {
        let tmp = tempfile::tempdir().expect("tmp");
        let corpus = tiny_class_corpus(tmp.path());
        with_ctx(&corpus, tmp.path(), |ctx| {
            let a = tar_bytes(ctx, "docs", None).expect("a");
            let b = tar_bytes(ctx, "docs", None).expect("b");
            assert_eq!(a, b);
            let mut ar = tar::Archive::new(&a[..]);
            let mut names = Vec::new();
            for e in ar.entries().expect("entries") {
                let e = e.expect("entry");
                let h = e.header();
                assert_eq!(
                    (
                        h.mtime().expect("m"),
                        h.uid().expect("u"),
                        h.gid().expect("g")
                    ),
                    (0, 0, 0)
                );
                assert_eq!(h.mode().expect("mode"), 0o644);
                names.push(e.path().expect("path").to_string_lossy().into_owned());
            }
            assert_eq!(names, ["a.txt", "sub/b.txt", "sub/deep/c.txt"]);
            let mut sink = Vec::new();
            let stats = write_tar(ctx, "docs", None, &mut sink).expect("stats");
            assert_eq!(stats.files, 3);
            assert_eq!(stats.content_bytes, 5 + 11 + 7);
            assert_eq!(stats.tar_bytes, sink.len() as u64);
            assert!(stats.tar_bytes > stats.content_bytes);
            let sub = tar_bytes(ctx, "docs", Some("sub")).expect("sub");
            let mut ar = tar::Archive::new(&sub[..]);
            let n = ar.entries().expect("entries").count();
            assert_eq!(n, 2);
        });
    }
}
