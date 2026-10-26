//! Opaque path grants (`docs/notes/e5-design.md` section 4). The page never names a path: Rust
//! mints a grant for each path the user chose — an archive given on the command line or
//! dropped on the window, a file or folder picked in a native dialog — and every command takes
//! grants. A grant is random (a window cannot guess another's), typed (an archive grant is not
//! a destination) and lives as long as the process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::GuiError;

/// What a grant may be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantKind {
    /// An archive to open.
    Archive,
    /// A folder to write into (a destination).
    Folder,
    /// A file or folder to put into an archive.
    Input,
}

/// An opaque token for one path: what the page holds instead of the path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Grant(pub String);

/// What the page may show of a granted path: its last component (never the folder it is in).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Granted {
    /// The grant.
    pub grant: Grant,
    /// The kind.
    pub kind: GrantKind,
    /// The file or folder name.
    pub name: String,
}

/// The process's grants.
#[derive(Debug, Default)]
pub struct Grants {
    map: Mutex<HashMap<String, (GrantKind, PathBuf)>>,
}

fn random_token() -> String {
    let mut b = [0u8; 16];
    // A failing system generator is not survivable for a security token.
    #[allow(clippy::expect_used)]
    getrandom::fill(&mut b).expect("the system's random number generator");
    let mut s = String::with_capacity(33);
    s.push('g');
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

impl Grants {
    /// No grants.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a grant for `path`, which the user chose (Rust callers only: the command line, a
    /// drop, a native dialog).
    pub fn mint(&self, kind: GrantKind, path: &Path) -> Granted {
        let token = random_token();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        #[allow(clippy::unwrap_used)]
        self.map
            .lock()
            .unwrap()
            .insert(token.clone(), (kind, path.to_path_buf()));
        Granted {
            grant: Grant(token),
            kind,
            name,
        }
    }

    /// The path of `grant`, which must be of `kind`; anything else is `Usage`.
    pub fn resolve(&self, grant: &Grant, kind: GrantKind) -> Result<PathBuf, GuiError> {
        #[allow(clippy::unwrap_used)]
        let map = self.map.lock().unwrap();
        match map.get(&grant.0) {
            Some((k, p)) if *k == kind => Ok(p.clone()),
            Some(_) => Err(GuiError::usage("the grant is of another kind")),
            None => Err(GuiError::usage("unknown grant")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_resolves_to_its_path_and_kind_only() {
        let g = Grants::new();
        let a = g.mint(GrantKind::Archive, Path::new("dir/x.lpk"));
        assert_eq!(a.name, "x.lpk");
        assert_eq!(
            g.resolve(&a.grant, GrantKind::Archive).unwrap(),
            Path::new("dir/x.lpk")
        );
        let e = g.resolve(&a.grant, GrantKind::Folder).unwrap_err();
        assert!(e.is(lpk_api::Class::Usage), "{e}");
    }

    #[test]
    fn a_path_is_not_a_grant() {
        let g = Grants::new();
        g.mint(GrantKind::Archive, Path::new("x.lpk"));
        for forged in ["x.lpk", "C:\\Windows\\win.ini", "/etc/passwd", "", "g00"] {
            let e = g
                .resolve(&Grant(forged.into()), GrantKind::Archive)
                .unwrap_err();
            assert!(e.is(lpk_api::Class::Usage), "{forged}: {e}");
        }
    }

    #[test]
    fn grants_are_distinct_and_opaque() {
        let g = Grants::new();
        let a = g.mint(GrantKind::Input, Path::new("secret-folder/a"));
        let b = g.mint(GrantKind::Input, Path::new("secret-folder/a"));
        assert_ne!(a.grant, b.grant);
        assert!(!a.grant.0.contains("secret"));
        assert_eq!(a.grant.0.len(), 33);
    }
}
