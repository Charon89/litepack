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

/// Write the tar of the files of `class` whose class-relative path is inside `folder`
/// (`None`: the whole class) to `out`. Returns the number of files written.
pub fn write_tar<W: Write>(
    ctx: &Ctx<'_>,
    class: &str,
    folder: Option<&str>,
    out: W,
) -> Result<u64> {
    let files = ctx.class_files(class).unwrap_or(&[]);
    let prefix = folder.map(|f| format!("{}/", f.trim_matches('/')));
    let mut builder = tar::Builder::new(out);
    let mut count = 0u64;
    for f in files {
        let rel = ctx.rel_path(class, f);
        if let Some(p) = &prefix {
            if !rel.starts_with(p.as_str()) {
                continue;
            }
        }
        let data = ctx.read_file(class, f)?;
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        builder
            .append_data(&mut h, rel, &data[..])
            .context("writing a tar entry")?;
        count += 1;
    }
    builder.into_inner().context("finishing the tar")?;
    Ok(count)
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
            let sub = tar_bytes(ctx, "docs", Some("sub")).expect("sub");
            let mut ar = tar::Archive::new(&sub[..]);
            let n = ar.entries().expect("entries").count();
            assert_eq!(n, 2);
        });
    }
}
