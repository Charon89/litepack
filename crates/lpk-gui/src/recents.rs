//! Recent archives and recent destinations (E5-4, R136; `docs/notes/e5-design.md` section 4).
//! Rust alone records them: an archive the user chose in a native dialog, on the command line
//! or by a drop; a destination chosen in the native folder dialog (never one derived from an
//! archive). The page never sends a path: it receives labels and opaque grants minted when
//! the lists are read ([`crate::app::App::recents`]), and `settings` has no path field.
//!
//! The file is versioned JSON in the local state folder ([`crate::paths::recents_file`]),
//! written temp-then-rename; at most [`MAX`] of each, newest first, de-duplicated (ignoring
//! case on Windows). Entries that no longer exist are dropped when listed. An unreadable or
//! newer file counts as empty and is overwritten by the next record.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The file format's version.
pub const VERSION: u32 = 1;

/// Entries kept of each list.
pub const MAX: usize = 10;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Data {
    version: u32,
    archives: Vec<String>,
    destinations: Vec<String>,
}

/// The recent lists.
#[derive(Debug, Default)]
pub struct Recents {
    file: Option<PathBuf>,
    data: Mutex<Data>,
}

fn same(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

fn push(list: &mut Vec<String>, path: &Path) {
    let Some(p) = path.to_str() else {
        return;
    };
    list.retain(|x| !same(x, p));
    list.insert(0, p.to_string());
    list.truncate(MAX);
}

impl Recents {
    /// The lists kept in `file` (none: kept in memory only), read now.
    pub fn open(file: Option<PathBuf>) -> Self {
        let data = file
            .as_deref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice::<Data>(&b).ok())
            .filter(|d| d.version == VERSION)
            .unwrap_or(Data {
                version: VERSION,
                ..Data::default()
            });
        Recents {
            file,
            data: Mutex::new(data),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Data> {
        self.data.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save(&self, d: &Data) {
        let Some(file) = &self.file else {
            return;
        };
        let tmp = file.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(d).map_err(std::io::Error::other)?)?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, file)
        };
        if write().is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Record an archive the user chose.
    pub fn record_archive(&self, path: &Path) {
        let mut d = self.lock();
        push(&mut d.archives, path);
        self.save(&d);
    }

    /// Record a destination the user chose in the folder dialog.
    pub fn record_destination(&self, path: &Path) {
        let mut d = self.lock();
        push(&mut d.destinations, path);
        self.save(&d);
    }

    /// The recent archives (files that still exist) and destinations (folders that still
    /// exist), newest first; the ones gone are dropped from the file.
    pub fn list(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut d = self.lock();
        let before = (d.archives.len(), d.destinations.len());
        d.archives.retain(|p| Path::new(p).is_file());
        d.destinations.retain(|p| Path::new(p).is_dir());
        if (d.archives.len(), d.destinations.len()) != before {
            self.save(&d);
        }
        (
            d.archives.iter().map(PathBuf::from).collect(),
            d.destinations.iter().map(PathBuf::from).collect(),
        )
    }

    /// Forget both lists.
    pub fn clear(&self) {
        let mut d = self.lock();
        d.archives.clear();
        d.destinations.clear();
        self.save(&d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_deduplicated_capped_and_persisted() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("recents.json");
        let files: Vec<PathBuf> = (0..12)
            .map(|i| {
                let p = d.path().join(format!("a{i}.lpk"));
                std::fs::write(&p, b"x").unwrap();
                p
            })
            .collect();
        let r = Recents::open(Some(file.clone()));
        for p in &files {
            r.record_archive(p);
        }
        r.record_archive(&files[5]);
        let (a, dest) = r.list();
        assert_eq!(a.len(), MAX);
        assert_eq!(a[0], files[5]);
        assert_eq!(a.iter().filter(|p| **p == files[5]).count(), 1);
        assert!(dest.is_empty());
        // A new process reads the same lists back.
        let again = Recents::open(Some(file.clone()));
        assert_eq!(again.list().0, a);
        // The file is gone-proof: a deleted archive is dropped when listed.
        std::fs::remove_file(&files[5]).unwrap();
        assert!(!again.list().0.contains(&files[5]));
        assert!(!file.with_extension("json.tmp").exists());
    }

    #[test]
    fn destinations_must_still_exist_and_clear_forgets_both() {
        let d = tempfile::tempdir().unwrap();
        let r = Recents::open(Some(d.path().join("r.json")));
        let gone = d.path().join("gone");
        r.record_destination(d.path());
        r.record_destination(&gone);
        let (_, dest) = r.list();
        assert_eq!(dest, vec![d.path().to_path_buf()]);
        r.record_archive(&d.path().join("nope.lpk"));
        r.clear();
        assert_eq!(r.list(), (vec![], vec![]));
    }

    #[test]
    fn a_newer_or_broken_file_counts_as_empty() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("r.json");
        std::fs::write(
            &file,
            br#"{"version":9,"archives":["x"],"destinations":[]}"#,
        )
        .unwrap();
        assert_eq!(Recents::open(Some(file.clone())).list().0.len(), 0);
        std::fs::write(&file, b"not json").unwrap();
        assert_eq!(Recents::open(Some(file)).list().0.len(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn windows_ignores_case_when_deduplicating() {
        let r = Recents::open(None);
        let mut d = r.lock();
        push(&mut d.archives, Path::new("C:\\A\\X.lpk"));
        push(&mut d.archives, Path::new("c:\\a\\x.LPK"));
        assert_eq!(d.archives.len(), 1);
    }
}
