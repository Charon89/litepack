//! Opening an entry (E5-3; `docs/notes/e5-design.md` section 4): a double-click extracts that
//! entry alone, under E3-2's policy, into a fresh folder under the GUI's `Open` folder, zoned like
//! the archive (the Internet zone for a foreign archive without one; the Mark of the Web opt-out
//! never applies), and the shell opens it only when its type is on [`ALLOWED`], judged by the real
//! last extension: `doc.pdf .cmd` (CVE-2023-38831's class) is a `.cmd`. Everything else is refused
//! before anything is written, and goes to the preview (E5-6). The folders are removed at exit;
//! nothing is written back.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;

/// The types the shell may open (lower case, without the dot): documents without macros,
/// images, audio and video, plain text. No script, executable, shortcut, installer, HTML, SVG,
/// macro-enabled or legacy Office type (those can carry code), and no archive (LitePack opens
/// those itself).
pub const ALLOWED: &[&str] = &[
    // Documents.
    "pdf", "docx", "xlsx", "pptx", "odt", "ods", "odp", "rtf", "epub", // Text.
    "txt", "md", "csv", "tsv", "log", "json", "xml", "yaml", "yml", "toml", "ini",
    // Images.
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "heic", "avif", // Audio.
    "mp3", "wav", "flac", "ogg", "opus", "m4a", "aac", "wma", // Video.
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv",
];

/// The real last extension of `name` (its last component), lower case: what follows the last
/// dot, spaces included (`doc.pdf .cmd` gives `cmd`, `doc.pdf ` gives `pdf `). `None` without
/// a dot.
pub fn last_extension(name: &str) -> Option<String> {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (_, ext) = leaf.rsplit_once('.')?;
    Some(ext.to_ascii_lowercase())
}

/// Whether the shell may open an entry named `name`.
pub fn allowed(name: &str) -> bool {
    last_extension(name).is_some_and(|e| ALLOWED.contains(&e.as_str()))
}

/// What opening an entry did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Opening {
    /// Extracted; the host hands `file` to the shell.
    Shell {
        /// The entry's name.
        name: String,
        /// The extracted file (never sent to the page).
        #[serde(skip)]
        file: PathBuf,
    },
    /// Not on the allow-list: nothing written; the preview shows it (E5-6).
    Preview {
        /// The entry's name.
        name: String,
        /// Its real last extension (empty without one).
        extension: String,
    },
}

/// The fresh folders opened entries went into, removed at exit.
#[derive(Debug)]
pub struct OpenFolders {
    root: Option<PathBuf>,
    next: AtomicU64,
    made: Mutex<Vec<PathBuf>>,
}

impl OpenFolders {
    /// Folders under `root` (none: opening is refused).
    pub fn new(root: Option<PathBuf>) -> Self {
        OpenFolders {
            root,
            next: AtomicU64::new(1),
            made: Mutex::new(Vec::new()),
        }
    }

    /// A new, empty folder of this process (`<pid>-<n>`; never one that exists).
    pub fn fresh(&self) -> std::io::Result<PathBuf> {
        let root = self.root.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no folder for opened entries")
        })?;
        std::fs::create_dir_all(root)?;
        loop {
            let n = self.next.fetch_add(1, Ordering::Relaxed);
            let dir = root.join(format!("{}-{n}", std::process::id()));
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    self.made
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(dir.clone());
                    return Ok(dir);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Remove every folder made (best effort: a program may still hold a file open).
    pub fn clean(&self) {
        let made: Vec<PathBuf> =
            std::mem::take(&mut *self.made.lock().unwrap_or_else(|p| p.into_inner()));
        for d in made {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}

/// The extracted file of entry `path` (`/`-separated) under `dir`.
pub fn file_in(dir: &Path, path: &str) -> PathBuf {
    path.split('/').fold(dir.to_path_buf(), |p, c| p.join(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_last_extension_decides() {
        assert_eq!(last_extension("doc.pdf .cmd").as_deref(), Some("cmd"));
        assert_eq!(last_extension("doc.pdf ").as_deref(), Some("pdf "));
        assert_eq!(last_extension("a/b/Photo.JPG").as_deref(), Some("jpg"));
        assert_eq!(last_extension("README"), None);
        assert!(allowed("x/report.pdf"));
        assert!(allowed("Photo.JPEG"));
        for bad in [
            "doc.pdf .cmd",
            "doc.pdf ",
            "doc.pdf.exe",
            "run.bat",
            "x.lnk",
            "page.html",
            "pic.svg",
            "macro.docm",
            "old.doc",
            "setup.msi",
            "script.ps1",
            "inner.zip",
            "README",
            "doc.pdf.",
        ] {
            assert!(!allowed(bad), "{bad}");
        }
    }

    #[test]
    fn fresh_folders_are_new_and_cleaned() {
        let d = tempfile::tempdir().unwrap();
        let f = OpenFolders::new(Some(d.path().join("Open")));
        let a = f.fresh().unwrap();
        let b = f.fresh().unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        f.clean();
        assert!(!a.exists() && !b.exists());
        assert!(OpenFolders::new(None).fresh().is_err());
    }
}
