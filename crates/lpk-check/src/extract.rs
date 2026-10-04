//! Extraction with the reference tool's refusals (section 9 "Extraction by the reference tool").

use std::fs::{self, OpenOptions};
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
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|x| Error::io(&format!("create {}", escape(&e.path)), &x))?;
    if let Err(x) = f.write_all(&bytes).and_then(|()| f.sync_all()) {
        drop(f);
        let _ = fs::remove_file(&target);
        return Err(Error::io("write", &x));
    }
    Ok(())
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
mod tests {
    use super::*;

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
