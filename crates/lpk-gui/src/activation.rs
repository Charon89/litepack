//! An activation verb as the page receives it (E5-7): each path the verb names is minted as a
//! grant in Rust, as `--open`'s archive always was (D-117), and the page learns only the grants,
//! their file names and the handler to run. The same path serves the first instance's own
//! command line and the verbs a second instance forwards (`tauri-plugin-single-instance`).
//!
//! The handlers, and the task that completes each (the page's `apps/gui/main.js` holds them):
//! - [`Handler::Open`]: opens the archive in the browser (done).
//! - [`Handler::CreateNamed`]: `Add to "<name>.lpk"`, created at once with the defaults through
//!   `create_archive` into the first input's folder (done; E5-5 adds its progress dialog).
//! - [`Handler::ExtractHere`], [`Handler::ExtractTo`]: E5-4 (the smart destination rule).
//! - [`Handler::CreateWizard`]: E5-5 (the create wizard).
//! - [`Handler::Test`]: E5-8 (the test dialog).
//! - [`Handler::Convert`]: E4's broker path into E5-1's `create` (re-creating a foreign archive).

use serde::Serialize;

use crate::cli::Verb;
use crate::grants::{GrantKind, Granted, Grants};

/// The page's handler for an activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Handler {
    /// Open the archive (`--open`, a bare path).
    Open,
    /// Extract each archive beside itself (`--extract-here`); E5-4 completes it.
    ExtractHere,
    /// Extract each archive into `<name>\` beside it (`--extract-to`); E5-4 completes it.
    ExtractTo,
    /// The create wizard with these inputs (`--create`); E5-5 completes it.
    CreateWizard,
    /// Create `<name>.lpk` at once with the defaults (`--create … --name`).
    CreateNamed,
    /// Test each archive (`--test`); E5-8 completes it.
    Test,
    /// Re-create a foreign archive as `.lpk` (`--convert`); E4's broker path completes it.
    Convert,
}

impl Handler {
    /// Whether the handler's action exists in this version (the others show the page's
    /// "not in this version yet" notice).
    pub fn implemented(self) -> bool {
        matches!(self, Handler::Open | Handler::CreateNamed)
    }
}

/// An activation for the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Activation {
    /// What runs.
    pub handler: Handler,
    /// A grant per path, in the command line's order.
    pub items: Vec<Granted>,
    /// `--name`'s file name, without its `.lpk` (the name `create_archive` takes).
    pub name: Option<String>,
    /// For [`Handler::CreateNamed`]: the first input's folder, the destination (derived in Rust
    /// from a path the user chose, as "extract here" derives from the archive's).
    pub folder: Option<Granted>,
}

/// Mint the grants of `verb`.
pub fn activate(verb: &Verb, grants: &Grants) -> Activation {
    let mint = |kind, v: &[&std::path::Path]| -> Vec<Granted> {
        v.iter().map(|p| grants.mint(kind, p)).collect()
    };
    let paths = verb.paths();
    match verb {
        Verb::Create { inputs, name } => {
            let (handler, folder, name) = match name {
                Some(n) => {
                    let folder = inputs
                        .first()
                        .and_then(|p| p.parent())
                        .filter(|p| !p.as_os_str().is_empty())
                        .map(|p| grants.mint(GrantKind::Folder, p));
                    let stem = n[..n.len() - ".lpk".len()].to_owned();
                    (Handler::CreateNamed, folder, Some(stem))
                }
                None => (Handler::CreateWizard, None, None),
            };
            Activation {
                handler,
                items: mint(GrantKind::Input, &paths),
                name,
                folder,
            }
        }
        _ => Activation {
            handler: match verb {
                Verb::Open(_) => Handler::Open,
                Verb::ExtractHere(_) => Handler::ExtractHere,
                Verb::ExtractTo(_) => Handler::ExtractTo,
                Verb::Test(_) => Handler::Test,
                _ => Handler::Convert,
            },
            items: mint(GrantKind::Archive, &paths),
            name: None,
            folder: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::*;
    use crate::cli;

    fn act(args: &[&str], cwd: &Path, g: &Grants) -> Activation {
        let s = cli::parse(args.iter().map(OsString::from), Some(cwd)).unwrap();
        activate(&s.verb.unwrap(), g)
    }

    #[test]
    fn each_path_becomes_a_grant_of_the_verbs_kind() {
        let g = Grants::new();
        let cwd = std::env::temp_dir();
        let a = act(&["--extract-here", "a.zip", "b.lpk"], &cwd, &g);
        assert_eq!(a.handler, Handler::ExtractHere);
        assert_eq!(a.items.len(), 2);
        assert_eq!(
            g.resolve(&a.items[1].grant, GrantKind::Archive).unwrap(),
            cwd.join("b.lpk")
        );
        assert_eq!(a.items[1].name, "b.lpk");
        // The page sees no folder: names and opaque tokens only.
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains(&*cwd.to_string_lossy().replace('\\', "\\\\")));
        let c = act(&["--create", "docs", "x.txt"], &cwd, &g);
        assert_eq!(c.handler, Handler::CreateWizard);
        assert!(g.resolve(&c.items[0].grant, GrantKind::Input).is_ok());
        assert!(g.resolve(&c.items[0].grant, GrantKind::Archive).is_err());
        for (args, h) in [
            (&["--open", "a"][..], Handler::Open),
            (&["--extract-to", "a"][..], Handler::ExtractTo),
            (&["--test", "a"][..], Handler::Test),
            (&["--convert", "a"][..], Handler::Convert),
        ] {
            assert_eq!(act(args, &cwd, &g).handler, h);
        }
    }

    #[test]
    fn a_named_create_gets_the_first_inputs_folder() {
        let g = Grants::new();
        let cwd = std::env::temp_dir();
        let a = act(&["--create", "sub/x.txt", "y", "--name", "x.lpk"], &cwd, &g);
        assert_eq!(a.handler, Handler::CreateNamed);
        assert_eq!(a.name.as_deref(), Some("x"));
        let folder = a.folder.unwrap();
        assert_eq!(
            g.resolve(&folder.grant, GrantKind::Folder).unwrap(),
            cwd.join("sub")
        );
        assert!(Handler::CreateNamed.implemented() && Handler::Open.implemented());
        assert!(!Handler::Convert.implemented());
    }
}
