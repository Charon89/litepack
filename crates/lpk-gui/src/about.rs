//! The About box (`docs/notes/e7-design.md` section 5): the version, the licence, where the copy
//! keeps its settings and whether it is portable, links to the project's pages, and the removal
//! of the worker's AppContainer profile.
//!
//! - The page names a link by [`Link`], never by address: an unknown name does not deserialise,
//!   so the page cannot open anything but [`Link::url`]'s fixed `https:` pages (and
//!   `lpk_gui_sys::open_web_uri` refuses anything that is not `https:` besides).
//! - The removal runs only while no operation does, and no operation begins until it has ended
//!   ([`crate::ops::Ops::exclusive`]); it calls `lpk_api::remove_worker_profile`, the sandbox
//!   crate's delete, as `lpk-worker --remove-profile` does.

use std::path::Path;

use lpk_api::ProfileRemoval;
use serde::{Deserialize, Serialize};

use crate::ops::Ops;
use crate::paths::Dirs;

/// The licence line.
pub const LICENCE: &str = "Apache-2.0 or MIT";

/// The documentation site.
pub const DOCS_URL: &str = "https://charon89.github.io/litepack/";

/// The source repository.
pub const REPOSITORY_URL: &str = "https://github.com/Charon89/litepack";

/// A page the About box links to: the page sends one of these names, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    /// The documentation site.
    Docs,
    /// Its privacy page.
    Privacy,
    /// Its security-model page.
    Security,
    /// Its third-party notices.
    Notices,
    /// The source repository.
    Repository,
}

impl Link {
    /// Every link, in the order the box shows them.
    pub const ALL: [Link; 5] = [
        Link::Docs,
        Link::Privacy,
        Link::Security,
        Link::Notices,
        Link::Repository,
    ];

    /// The fixed address the link opens.
    pub fn url(self) -> &'static str {
        match self {
            Link::Docs => DOCS_URL,
            Link::Privacy => "https://charon89.github.io/litepack/privacy.html",
            Link::Security => "https://charon89.github.io/litepack/security.html",
            Link::Notices => "https://charon89.github.io/litepack/notices.html",
            Link::Repository => REPOSITORY_URL,
        }
    }
}

/// What the About box shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AboutInfo {
    /// The running build's version (the one the update check compares).
    pub version: String,
    /// [`LICENCE`].
    pub licence: &'static str,
    /// Whether this copy is portable (a `portable` marker beside the executable).
    pub portable: bool,
    /// The settings file, shown to the user; `None` when the environment names no folder.
    pub settings: Option<String>,
    /// Where an installed copy is installed (the executable's folder); `None` for a portable
    /// copy (its settings line says where it is) or an unknown executable.
    pub install: Option<String>,
    /// Every link, with its address shown as the link's text (the page opens it by name).
    pub links: Vec<LinkInfo>,
}

/// A link of the About box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkInfo {
    /// Its name, which the page sends back to open it.
    pub link: Link,
    /// The address it opens.
    pub url: &'static str,
}

/// The About box's facts for a build of `version`, with folders `dirs`, run from `exe_dir`.
pub fn info(version: &str, dirs: &Dirs, exe_dir: Option<&Path>) -> AboutInfo {
    AboutInfo {
        version: version.to_string(),
        licence: LICENCE,
        portable: dirs.portable,
        settings: dirs.settings.as_ref().map(|p| p.display().to_string()),
        install: exe_dir
            .filter(|_| !dirs.portable)
            .map(|d| d.display().to_string()),
        links: Link::ALL
            .iter()
            .map(|&link| LinkInfo {
                link,
                url: link.url(),
            })
            .collect(),
    }
}

/// What the profile's removal did, as the page sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Removal {
    /// The profile existed and is gone.
    Removed,
    /// There was no profile.
    Absent,
    /// Refused: an operation runs (or the window is closing).
    Busy,
    /// Windows refused the removal.
    Failed {
        /// Why.
        reason: String,
    },
}

/// Remove the worker's profile with `remove` (`lpk_api::remove_worker_profile` in the GUI, a
/// fake or a `LitePack.Test.*` profile in tests), refused while any operation runs. No
/// operation begins while it runs.
pub fn remove_profile(
    ops: &std::sync::Arc<Ops>,
    remove: impl FnOnce() -> std::io::Result<ProfileRemoval>,
) -> Removal {
    let Some(_hold) = ops.exclusive() else {
        return Removal::Busy;
    };
    match remove() {
        Ok(ProfileRemoval::Removed) => Removal::Removed,
        Ok(ProfileRemoval::NoneExisted) => Removal::Absent,
        Err(e) => Removal::Failed {
            reason: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn only_the_fixed_pages_can_be_named() {
        for link in Link::ALL {
            let url = link.url();
            assert!(
                url.starts_with(DOCS_URL) || url == REPOSITORY_URL,
                "{link:?}: {url}"
            );
            let name = serde_json::to_string(&link).unwrap();
            assert_eq!(serde_json::from_str::<Link>(&name).unwrap(), link);
        }
        for bad in [
            r#""https://example.com""#,
            r#""file:///C:/Windows/notepad.exe""#,
            r#""cmd.exe""#,
            r#""Docs""#,
            r#""docs ""#,
            r#""""#,
            r#"{"url":"https://example.com"}"#,
            "0",
        ] {
            assert!(serde_json::from_str::<Link>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_links_are_the_documented_pages() {
        let urls: Vec<_> = Link::ALL.iter().map(|l| l.url()).collect();
        assert_eq!(
            urls,
            [
                "https://charon89.github.io/litepack/",
                "https://charon89.github.io/litepack/privacy.html",
                "https://charon89.github.io/litepack/security.html",
                "https://charon89.github.io/litepack/notices.html",
                "https://github.com/Charon89/litepack",
            ]
        );
        // Each page the site builds (book/src).
        let book = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../book/src");
        for page in ["privacy.md", "security.md", "notices.md"] {
            assert!(book.join(page).is_file(), "{page}");
        }
    }

    #[test]
    fn an_installed_copy_shows_its_folder_and_a_portable_one_does_not() {
        let installed = Dirs {
            state: None,
            settings: Some(PathBuf::from("settings.json")),
            portable: false,
        };
        let a = info("1.2.3", &installed, Some(Path::new("inst")));
        assert_eq!(a.version, "1.2.3");
        assert_eq!(a.licence, "Apache-2.0 or MIT");
        assert!(!a.portable);
        assert_eq!(a.settings.as_deref(), Some("settings.json"));
        assert_eq!(a.install.as_deref(), Some("inst"));
        assert_eq!(a.links.len(), Link::ALL.len());
        let portable = Dirs {
            portable: true,
            ..installed
        };
        let p = info("1.2.3", &portable, Some(Path::new("inst")));
        assert!(p.portable);
        assert_eq!(p.install, None);
    }

    #[test]
    fn the_removal_is_refused_while_an_operation_runs() {
        let ops = Ops::new();
        let g = ops.begin(1, false).unwrap();
        let mut called = false;
        let r = remove_profile(&ops, || {
            called = true;
            Ok(ProfileRemoval::Removed)
        });
        assert_eq!(r, Removal::Busy);
        assert!(!called, "nothing removed while an operation runs");
        drop(g);
        // While it runs, no operation begins.
        let r = remove_profile(&ops, || {
            assert!(ops.begin(2, false).is_err());
            Ok(ProfileRemoval::Removed)
        });
        assert_eq!(r, Removal::Removed);
        drop(ops.begin(2, false).unwrap());
    }

    #[test]
    fn the_removal_reports_each_outcome() {
        let ops = Ops::new();
        assert_eq!(
            remove_profile(&ops, || Ok(ProfileRemoval::Removed)),
            Removal::Removed
        );
        assert_eq!(
            remove_profile(&ops, || Ok(ProfileRemoval::NoneExisted)),
            Removal::Absent
        );
        let r = remove_profile(&ops, || {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "access is denied",
            ))
        });
        assert_eq!(
            r,
            Removal::Failed {
                reason: "access is denied".into()
            }
        );
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["outcome"], "failed");
        assert_eq!(
            serde_json::to_value(Removal::Absent).unwrap()["outcome"],
            "absent"
        );
    }

    /// The real removal on a `LitePack.Test.*` profile of its own (never `LitePack.Worker`).
    #[cfg(windows)]
    #[test]
    fn a_missing_test_profile_is_reported_through_the_api() {
        let name = format!("LitePack.Test.{}.about", std::process::id());
        let ops = Ops::new();
        // A name with no profile (lpk-core's tests remove a real one; this crate cannot make one).
        assert_eq!(
            remove_profile(&ops, || lpk_api::remove_profile_named(&name)),
            Removal::Absent
        );
    }
}
