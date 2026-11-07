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
    "pdf", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub", // Text.
    "txt", "md", "log", "json", "xml", "yaml", "yml", "toml", "ini", // Images.
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

/// What is decided before anything is written for entry `path` (`/`-separated): an error when
/// E3-2's path rules refuse it (a colon, as in `a.exe:x.pdf` or `a.pdf::$DATA`; a device name
/// with any extension, as `CON.pdf`; a trailing dot or space), [`Opening::Preview`] when its
/// type is off [`ALLOWED`], `None` to go on and extract it.
pub fn refuse_first(path: &str) -> Result<Option<Opening>, crate::GuiError> {
    lpk_api::check_extraction_path(path)?;
    let name = path.rsplit('/').next().unwrap_or(path);
    if allowed(name) {
        Ok(None)
    } else {
        Ok(Some(Opening::Preview {
            name: name.to_string(),
            extension: last_extension(name).unwrap_or_default(),
        }))
    }
}

/// Whether an extracted copy must not reach the shell: a zone was due and the mark could not
/// be written (fail safe: the copy is removed and the open refused).
pub fn refuse_unmarked(zone_due: bool, unmarked: bool) -> bool {
    zone_due && unmarked
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

impl OpenFolders {
    /// The start-up sweep: remove the folders under the root an earlier run left (`<pid>-<n>`
    /// whose process `alive` says has ended; never this process's), best effort. Anything
    /// else under the root, and the folders of a running instance, are left alone. The count
    /// removed.
    pub fn sweep(&self, alive: impl Fn(u32) -> bool) -> usize {
        let Some(root) = &self.root else {
            return 0;
        };
        let Ok(rd) = std::fs::read_dir(root) else {
            return 0;
        };
        let me = std::process::id();
        let mut removed = 0;
        for e in rd.flatten() {
            let Some(pid) = folder_pid(&e.file_name().to_string_lossy()) else {
                continue;
            };
            if pid == me || alive(pid) || !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if std::fs::remove_dir_all(e.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// The process ID of a folder named `<pid>-<n>` (both decimal), else `None`.
fn folder_pid(name: &str) -> Option<u32> {
    let (pid, n) = name.split_once('-')?;
    // Exactly the form `fresh` writes: `007-1` or `7-01` is not one of ours.
    let pid_n: u32 = pid.parse().ok()?;
    let n_n: u64 = n.parse().ok()?;
    (pid_n.to_string() == pid && n_n.to_string() == n).then_some(pid_n)
}

/// The extracted file of entry `path` (`/`-separated) under `dir`.
pub fn file_in(dir: &Path, path: &str) -> PathBuf {
    path.split('/').fold(dir.to_path_buf(), |p, c| p.join(c))
}

/// The extracted file of entry `path` in its fresh folder `dir`; when the extraction left no
/// such file, the folder is removed and the open fails.
pub fn extracted_file(dir: &Path, path: &str) -> Result<PathBuf, crate::GuiError> {
    let file = file_in(dir, path);
    if file.is_file() {
        return Ok(file);
    }
    let _ = std::fs::remove_dir_all(dir);
    Err(lpk_api::Error::new(
        lpk_api::Class::Internal,
        "OpenEntry",
        format!("{path} was not extracted"),
        None,
    )
    .into())
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
            "letter.rtf",
            "table.csv",
            "table.tsv",
            "README",
            "doc.pdf.",
        ] {
            assert!(!allowed(bad), "{bad}");
        }
    }

    #[test]
    fn the_path_rules_refuse_before_the_allow_list() {
        for bad in [
            "a.exe:x.pdf",
            "a.pdf::$DATA",
            "a.exe::$DATA.pdf",
            "CON.pdf",
            "nul.txt",
            "dir/com1.png",
            "doc.pdf ",
            "doc.pdf.",
        ] {
            let e = refuse_first(bad).unwrap_err();
            assert!(e.is(lpk_api::Class::Unsafe), "{bad}: {e}");
        }
        assert!(matches!(
            refuse_first("x/run.cmd").unwrap(),
            Some(Opening::Preview { .. })
        ));
        assert_eq!(refuse_first("x/report.pdf").unwrap(), None);
    }

    #[test]
    fn an_unmarked_copy_of_a_zoned_source_is_refused() {
        assert!(refuse_unmarked(true, true));
        assert!(!refuse_unmarked(true, false));
        assert!(!refuse_unmarked(false, true));
        assert!(!refuse_unmarked(false, false));
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

    #[test]
    fn the_sweep_removes_ended_processes_folders_only() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("Open");
        let f = OpenFolders::new(Some(root.clone()));
        let mine = f.fresh().unwrap();
        std::fs::write(mine.join("x.txt"), b"x").unwrap();
        let ended = root.join("4000000001-1");
        let running = root.join("4000000002-7");
        let other = root.join("notes");
        let odd = root.join("12-x");
        for dir in [&ended, &running, &other, &odd] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("f.pdf"), b"f").unwrap();
        }
        let file = root.join("13-1");
        std::fs::write(&file, b"a file, not a folder").unwrap();
        let removed = f.sweep(|pid| pid == 4_000_000_002);
        assert_eq!(removed, 1);
        assert!(!ended.exists(), "an ended process's folder goes");
        assert!(running.join("f.pdf").exists(), "a running instance's stays");
        assert!(mine.join("x.txt").exists(), "this process's stays");
        assert!(other.exists() && odd.exists() && file.exists());
        // With the real check: no process 4000000002 runs, so its folder goes; this process
        // runs, so its folder stays.
        assert_eq!(f.sweep(lpk_gui_sys::process_alive), 1);
        assert!(!running.exists());
        assert!(mine.join("x.txt").exists());
        assert_eq!(OpenFolders::new(None).sweep(|_| false), 0);
    }

    #[test]
    fn only_the_exact_folder_form_names_a_process() {
        assert_eq!(folder_pid("7-1"), Some(7));
        assert_eq!(folder_pid("4000000001-12"), Some(4_000_000_001));
        for bad in [
            "007-1",
            "7-01",
            "+7-1",
            "7-+1",
            "7-",
            "-1",
            "7",
            "7-1-2",
            "a-1",
            "99999999999-1",
        ] {
            assert_eq!(folder_pid(bad), None, "{bad}");
        }
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("Open");
        let padded = root.join("004000000001-1");
        std::fs::create_dir_all(&padded).unwrap();
        assert_eq!(OpenFolders::new(Some(root)).sweep(|_| false), 0);
        assert!(padded.exists());
    }

    #[test]
    fn a_file_not_extracted_removes_its_fresh_folder() {
        let d = tempfile::tempdir().unwrap();
        let f = OpenFolders::new(Some(d.path().join("Open")));
        let dir = f.fresh().unwrap();
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let e = extracted_file(&dir, "a/missing.pdf").unwrap_err();
        assert!(e.is(lpk_api::Class::Internal), "{e}");
        assert!(!dir.exists(), "the fresh folder is removed");
        let dir = f.fresh().unwrap();
        std::fs::write(dir.join("ok.pdf"), b"ok").unwrap();
        assert_eq!(extracted_file(&dir, "ok.pdf").unwrap(), dir.join("ok.pdf"));
    }
}
