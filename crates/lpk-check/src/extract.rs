//! Extraction with the reference tool's refusals (section 9 "Extraction by the reference tool").

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::archive::Archive;
use crate::entries::{Entry, EntryKind};
use crate::error::{Error, Result};

const DEVICES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

/// Whether a path component must be refused (`UnsafePath`).
pub fn unsafe_component(c: &str) -> bool {
    if c.contains(':') || c.ends_with('.') || c.ends_with(' ') {
        return true;
    }
    let stem = c.split('.').next().unwrap_or("").trim_end_matches(' ');
    let up = stem.to_uppercase();
    if DEVICES.contains(&up.as_str()) {
        return true;
    }
    for p in ["COM", "LPT"] {
        if let Some(rest) = up.strip_prefix(p) {
            let digit = rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit() && rest != "0";
            if digit || matches!(rest, "\u{b9}" | "\u{b2}" | "\u{b3}") {
                return true;
            }
        }
    }
    false
}

/// Maps an entry path into `dir`, applying the refusals; errors before anything is written.
pub fn target_path(dir: &Path, e: &Entry) -> Result<PathBuf> {
    if e.kind == EntryKind::Symlink {
        return Err(Error::new(
            "SymlinkRefused",
            format!("symlink {} refused", escape(&e.path)),
        ));
    }
    let mut p = dir.to_path_buf();
    for c in e.path.split('/') {
        if unsafe_component(c) {
            return Err(Error::new(
                "UnsafePath",
                format!("unsafe path {}", escape(&e.path)),
            ));
        }
        p.push(c);
    }
    Ok(p)
}

/// Escapes control characters for display.
pub fn escape(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Refuses an entry one of whose parent paths is an entry that is not a directory
/// (`UnsafePath`, reason `conflicting name`, section 9).
pub fn check_conflicts(entries: &[Entry]) -> Result<()> {
    let kinds: std::collections::HashMap<&str, EntryKind> =
        entries.iter().map(|e| (e.path.as_str(), e.kind)).collect();
    for e in entries {
        for (i, _) in e.path.match_indices('/') {
            let parent = &e.path[..i];
            if kinds
                .get(parent)
                .is_some_and(|k| *k != EntryKind::Directory)
            {
                return Err(Error::new(
                    "UnsafePath",
                    format!("unsafe path {}: conflicting name", escape(&e.path)),
                ));
            }
        }
    }
    Ok(())
}

/// Extracts one entry into `dir` ("create new", no overwrite, a partial file removed).
pub fn extract_entry(a: &mut Archive, e: &Entry, dir: &Path) -> Result<()> {
    let target = target_path(dir, e)?;
    if e.kind == EntryKind::Directory {
        return fs::create_dir_all(&target).map_err(|x| Error::io("create directory", &x));
    }
    let bytes = a.read_file(e)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|x| Error::io("create directory", &x))?;
    }
    write_new(&target, &bytes, &escape(&e.path))
}

/// Writes `bytes` to a new file at `path` and syncs it ("create new": an existing file is
/// refused and left untouched). A file this call created and could not finish is removed, on an
/// error and on a panic alike, and the panic goes on (section 9). The errors name the step and
/// `what`: `create <what>`, `write <what>`.
pub fn write_new(path: &Path, bytes: &[u8], what: &str) -> Result<()> {
    fill_new(path, what, |mut f| {
        f.write_all(bytes).and_then(|()| f.sync_all())
    })
}

/// [`write_new`] with the writing done by `fill`, which owns the new file's handle.
fn fill_new(path: &Path, what: &str, fill: impl FnOnce(File) -> std::io::Result<()>) -> Result<()> {
    let f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|x| Error::io(&format!("create {what}"), &x))?;
    // Only now is the file this call's to remove. `fill` owns the handle, so the handle is
    // closed by the time the guard removes the file.
    let mut unfinished = Unfinished { path, armed: true };
    fill(f).map_err(|x| Error::io(&format!("write {what}"), &x))?;
    unfinished.armed = false;
    Ok(())
}

/// A new file that is not complete yet: removed when the guard is dropped while armed, which is
/// on an error return and while a panic unwinds through [`fill_new`].
struct Unfinished<'a> {
    path: &'a Path,
    armed: bool,
}

impl Drop for Unfinished<'_> {
    fn drop(&mut self) {
        // A removal that fails is not reported: the error or the panic that left the file
        // unfinished is the one the caller needs.
        if self.armed {
            let _ = fs::remove_file(self.path);
        }
    }
}

/// Extracts every entry; returns the paths extracted and the first error, if any. Refusals of
/// all entries are applied before anything is written.
pub fn extract_all(a: &mut Archive, dir: &Path) -> Result<(Vec<String>, Option<Error>)> {
    let entries = a.entries()?;
    for e in &entries {
        target_path(dir, e)?;
    }
    check_conflicts(&entries)?;
    let mut done = Vec::new();
    let mut first = None;
    for e in &entries {
        match extract_entry(a, e, dir) {
            Ok(()) => done.push(e.path.clone()),
            Err(x) => {
                first.get_or_insert(x);
            }
        }
    }
    Ok((done, first))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A new, empty directory for one test under the system's temporary directory.
    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lpk-check-unit-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_panic_mid_write_removes_the_new_file_then_goes_on() {
        let dir = scratch("panic");
        let path = dir.join("f");
        let r = std::panic::catch_unwind(|| {
            fill_new(&path, "f", |mut f| {
                f.write_all(b"the first half")?;
                panic!("test hook: the write panics mid-file")
            })
        });
        let payload = r.expect_err("the panic did not go on");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"test hook: the write panics mid-file")
        );
        assert!(!path.exists(), "a partial file was left");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_error_mid_write_removes_the_new_file() {
        let dir = scratch("error");
        let path = dir.join("f");
        let e = fill_new(&path, "f", |mut f| {
            f.write_all(b"the first half")?;
            Err(std::io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(e.to_string(), "write f: disk full");
        assert!(!path.exists(), "a partial file was left");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_file_is_refused_and_left_untouched() {
        let dir = scratch("existing");
        let path = dir.join("f");
        fs::write(&path, b"mine").unwrap();
        let e = write_new(&path, b"theirs", "f").unwrap_err();
        assert!(e.to_string().starts_with("create f: "), "{e}");
        assert_eq!(fs::read(&path).unwrap(), b"mine");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn device_names() {
        for c in [
            "CON",
            "con.txt",
            "con .txt",
            "COM1",
            "lpt9.x",
            "COM\u{b9}",
            "a:b",
            "x.",
            "y ",
        ] {
            assert!(unsafe_component(c), "{c}");
        }
        for c in ["CONSOLE", "COM0", "COM10", "a.txt", "nulx"] {
            assert!(!unsafe_component(c), "{c}");
        }
    }
}
