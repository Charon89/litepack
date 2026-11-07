//! `pick` (E5-3; `docs/notes/e5-design.md` sections 3 and 4): the native Open and folder pickers
//! mint grants the way `--open` and a drop do. The page names only what it wants picked
//! ([`Purpose`]); the path comes from the dialog, in Rust, and the page receives grants and
//! file names. The dialogs sit behind [`Picker`], so the minting is tested without one.

use std::path::PathBuf;

use serde::Deserialize;

use crate::grants::{GrantKind, Granted, Grants};
use crate::GuiError;

/// What the page asks to pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    /// An archive to open (the Open dialog, one file): an [`GrantKind::Archive`] grant.
    Archive,
    /// Files to put into an archive (the Open dialog, several): [`GrantKind::Input`] grants.
    Files,
    /// A folder to put into an archive (the folder picker): an [`GrantKind::Input`] grant.
    Folder,
    /// The folder a new archive goes into (the folder picker): a [`GrantKind::Folder`] grant.
    Destination,
}

impl Purpose {
    /// The kind of grant minted.
    pub fn kind(self) -> GrantKind {
        match self {
            Purpose::Archive => GrantKind::Archive,
            Purpose::Files | Purpose::Folder => GrantKind::Input,
            Purpose::Destination => GrantKind::Folder,
        }
    }

    /// The dialog's title, from the string table.
    pub fn title(self) -> &'static str {
        use crate::strings::get;
        match self {
            Purpose::Archive => get("pick.archive"),
            Purpose::Files => get("pick.files"),
            Purpose::Folder => get("pick.folder"),
            Purpose::Destination => get("pick.destination"),
        }
    }
}

/// The native dialogs (or a test's stand-in). Empty or `None`: the user cancelled.
pub trait Picker {
    /// Files (several when `multi`).
    fn files(&self, title: &str, multi: bool) -> std::io::Result<Vec<PathBuf>>;
    /// One folder.
    fn folder(&self, title: &str) -> std::io::Result<Option<PathBuf>>;
}

/// The shell's common item dialogs (`lpk-gui-sys`), owned by the window `owner`.
#[derive(Debug, Clone, Copy)]
pub struct NativePicker {
    /// The owner window's raw handle.
    pub owner: Option<isize>,
}

impl Picker for NativePicker {
    fn files(&self, title: &str, multi: bool) -> std::io::Result<Vec<PathBuf>> {
        lpk_gui_sys::pick_files(self.owner, title, multi)
    }

    fn folder(&self, title: &str) -> std::io::Result<Option<PathBuf>> {
        lpk_gui_sys::pick_folder(self.owner, title)
    }
}

/// Show the dialog for `purpose` and mint a grant for each path chosen (none when cancelled).
pub fn pick(
    grants: &Grants,
    picker: &dyn Picker,
    purpose: Purpose,
) -> Result<Vec<Granted>, GuiError> {
    let title = purpose.title();
    let failed = |e: std::io::Error| {
        GuiError::from(lpk_api::Error::new(
            lpk_api::Class::Io,
            "Pick",
            format!("the dialog failed: {e}"),
            None,
        ))
    };
    let paths = match purpose {
        Purpose::Archive => picker.files(title, false).map_err(failed)?,
        Purpose::Files => picker.files(title, true).map_err(failed)?,
        Purpose::Folder | Purpose::Destination => {
            picker.folder(title).map_err(failed)?.into_iter().collect()
        }
    };
    // One archive at a time, whatever the dialog returned.
    let take = if purpose == Purpose::Archive {
        1
    } else {
        usize::MAX
    };
    Ok(paths
        .iter()
        .take(take)
        .map(|p| grants.mint(purpose.kind(), p))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;

    /// Answers with fixed paths and records what was asked.
    struct Fake {
        files: Vec<PathBuf>,
        folder: Option<PathBuf>,
        asked: RefCell<Vec<(String, bool, bool)>>,
    }

    impl Picker for Fake {
        fn files(&self, title: &str, multi: bool) -> std::io::Result<Vec<PathBuf>> {
            self.asked.borrow_mut().push((title.into(), false, multi));
            Ok(self.files.clone())
        }
        fn folder(&self, title: &str) -> std::io::Result<Option<PathBuf>> {
            self.asked.borrow_mut().push((title.into(), true, false));
            Ok(self.folder.clone())
        }
    }

    fn fake(files: &[&str], folder: Option<&str>) -> Fake {
        Fake {
            files: files.iter().map(PathBuf::from).collect(),
            folder: folder.map(PathBuf::from),
            asked: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn each_purpose_mints_its_kind_from_its_dialog() {
        let g = Grants::new();
        let f = fake(&["dir/a.lpk", "dir/b.lpk"], Some("out/dest"));
        let a = pick(&g, &f, Purpose::Archive).unwrap();
        assert_eq!(a.len(), 1, "one archive");
        assert_eq!(a[0].name, "a.lpk");
        assert_eq!(
            g.resolve(&a[0].grant, GrantKind::Archive).unwrap(),
            Path::new("dir/a.lpk")
        );
        let files = pick(&g, &f, Purpose::Files).unwrap();
        assert_eq!(files.len(), 2);
        for x in &files {
            assert_eq!(x.kind, GrantKind::Input);
            g.resolve(&x.grant, GrantKind::Input).unwrap();
        }
        let d = pick(&g, &f, Purpose::Destination).unwrap();
        assert_eq!(d[0].name, "dest");
        assert_eq!(
            g.resolve(&d[0].grant, GrantKind::Folder).unwrap(),
            Path::new("out/dest")
        );
        assert!(g.resolve(&d[0].grant, GrantKind::Input).is_err());
        let i = pick(&g, &f, Purpose::Folder).unwrap();
        assert_eq!(
            g.resolve(&i[0].grant, GrantKind::Input).unwrap(),
            Path::new("out/dest")
        );
        let asked: Vec<(bool, bool)> = f.asked.borrow().iter().map(|a| (a.1, a.2)).collect();
        assert_eq!(
            asked,
            [(false, false), (false, true), (true, false), (true, false)]
        );
        assert_eq!(f.asked.borrow()[0].0, crate::strings::get("pick.archive"));
    }

    #[test]
    fn a_cancelled_dialog_mints_nothing() {
        let g = Grants::new();
        let f = fake(&[], None);
        for p in [
            Purpose::Archive,
            Purpose::Files,
            Purpose::Folder,
            Purpose::Destination,
        ] {
            assert!(pick(&g, &f, p).unwrap().is_empty(), "{p:?}");
        }
    }

    #[test]
    fn a_failing_dialog_is_an_error() {
        struct Broken;
        impl Picker for Broken {
            fn files(&self, _: &str, _: bool) -> std::io::Result<Vec<PathBuf>> {
                Err(std::io::ErrorKind::Unsupported.into())
            }
            fn folder(&self, _: &str) -> std::io::Result<Option<PathBuf>> {
                Err(std::io::ErrorKind::Unsupported.into())
            }
        }
        let e = pick(&Grants::new(), &Broken, Purpose::Destination).unwrap_err();
        assert!(e.is(lpk_api::Class::Io), "{e}");
    }

    #[test]
    fn the_page_names_a_purpose_never_a_path() {
        let p: Purpose = serde_json::from_str("\"destination\"").unwrap();
        assert_eq!(p, Purpose::Destination);
        for bad in ["\"C:\\\\Windows\"", "{\"path\":\"x\"}", "\"/etc\""] {
            assert!(serde_json::from_str::<Purpose>(bad).is_err(), "{bad}");
        }
    }
}
