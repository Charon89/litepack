//! The native pieces of LitePack's GUI (E5-2, `docs/notes/e5-design.md` sections 1 and 4):
//!
//! - [`publish_no_replace`]: moves a finished `<name>.lpk.partial` onto its final name and
//!   fails when that name exists (`MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on
//!   Windows; `std::fs::rename` would replace it).
//! - [`file_identity`]: the volume serial number and file ID of an open file, so that the next
//!   start touches a recorded partial file or interrupted append only if it is still the same
//!   file.
//! - [`password_prompt`] and [`confirm`]: the native password dialog (an `ES_PASSWORD` edit,
//!   twice to create) whose text goes straight into [`Zeroizing`] memory and never reaches the
//!   web process, and the rollback confirmation box. Windows only; elsewhere they return
//!   [`std::io::ErrorKind::Unsupported`].
//!
//! `unsafe` lives in `src/windows_impl.rs` alone (docs/UNSAFE.md).
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::fs::File;
use std::io;
use std::path::Path;

pub use zeroize::Zeroizing;

#[cfg(windows)]
mod windows_impl;

/// Which file a record names: the volume (its serial number, or the device on Unix) and the
/// file's ID on it (the 128-bit file ID, or the inode). Two paths with the same identity are
/// the same file; a file deleted and recreated under the same name has another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    /// The volume serial number (Windows) or the device number (Unix).
    pub volume: u64,
    /// The file ID (Windows `FILE_ID_INFO`, 128 bits) or the inode number (Unix).
    pub file_id: u128,
}

/// The identity of an open file.
pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    #[cfg(windows)]
    {
        windows_impl::file_identity(file)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata()?;
        Ok(FileIdentity {
            volume: m.dev(),
            file_id: u128::from(m.ino()),
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// The identity of the file at `path` (opened for reading attributes only).
pub fn path_identity(path: &Path) -> io::Result<FileIdentity> {
    file_identity(&File::open(path)?)
}

/// Move `from` to `to`, failing with [`io::ErrorKind::AlreadyExists`] when `to` exists: never
/// replaces anything. Both must be on one volume (a `.partial` is written beside its final
/// name). The move is written through to disk before it returns.
pub fn publish_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_impl::move_no_replace(from, to)
    }
    #[cfg(not(windows))]
    {
        // A hard link fails when the name exists, atomically; then the old name goes.
        std::fs::hard_link(from, to)?;
        std::fs::remove_file(from)
    }
}

/// The texts of the password dialog (from `apps/gui/strings/en.json`, so that the page and the
/// native dialogs share one string table).
#[derive(Debug, Clone, Default)]
pub struct PasswordTexts {
    /// The window title.
    pub title: String,
    /// The label above the first field.
    pub prompt: String,
    /// The label above the second field; `Some` asks twice (creating an archive).
    pub confirm: Option<String>,
    /// The OK button.
    pub ok: String,
    /// The Cancel button.
    pub cancel: String,
    /// Shown when the two fields differ.
    pub mismatch: String,
}

/// Ask for a password in a native modal window owned by `owner` (a raw `HWND`, or none), in
/// the dark theme when `dark`. `Ok(None)` when the user cancels. Runs its own message loop:
/// call it on the GUI's main thread.
pub fn password_prompt(
    owner: Option<isize>,
    texts: &PasswordTexts,
    dark: bool,
) -> io::Result<Option<Zeroizing<String>>> {
    #[cfg(windows)]
    {
        windows_impl::password_prompt(owner, texts, dark, None)
    }
    #[cfg(not(windows))]
    {
        let _ = (owner, texts, dark);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Open the file at `path` with the program the shell associates with its type
/// (`ShellExecuteW`'s `open` verb), owned by `owner`. The GUI calls it only for an entry it
/// extracted and whose type is on its allow-list (`docs/notes/e5-design.md` section 4).
pub fn shell_open(owner: Option<isize>, path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows_impl::shell_open(owner, path)
    }
    #[cfg(not(windows))]
    {
        let _ = (owner, path);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// A native Yes/No warning box owned by `owner`; `true` on Yes. The rollback confirmation
/// (`docs/notes/e5-design.md` section 4): the page cannot answer it for the user.
pub fn confirm(owner: Option<isize>, title: &str, text: &str) -> io::Result<bool> {
    #[cfg(windows)]
    {
        Ok(windows_impl::confirm(owner, title, text))
    }
    #[cfg(not(windows))]
    {
        let _ = (owner, title, text);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_refuses_an_existing_name_and_keeps_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.lpk.partial");
        let to = dir.path().join("a.lpk");
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        let e = publish_no_replace(&from, &to).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(std::fs::read(&to).unwrap(), b"old");
        assert_eq!(std::fs::read(&from).unwrap(), b"new");
    }

    #[test]
    fn publish_moves_onto_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.lpk.partial");
        let to = dir.path().join("a.lpk");
        std::fs::write(&from, b"new").unwrap();
        let before = path_identity(&from).unwrap();
        publish_no_replace(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert_eq!(path_identity(&to).unwrap(), before, "a move keeps the file");
    }

    #[test]
    fn identity_tells_a_recreated_file_apart() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let other = dir.path().join("g");
        std::fs::write(&p, b"1").unwrap();
        std::fs::write(&other, b"2").unwrap();
        let first = path_identity(&p).unwrap();
        assert_eq!(first, path_identity(&p).unwrap());
        assert_ne!(first, path_identity(&other).unwrap());
        // Keep the old file alive under another name so its ID cannot be reused.
        std::fs::rename(&p, dir.path().join("kept")).unwrap();
        std::fs::write(&p, b"1").unwrap();
        assert_ne!(first, path_identity(&p).unwrap());
    }
}
