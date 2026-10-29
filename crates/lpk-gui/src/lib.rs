//! LitePack's GUI, the command layer (E5-2; `docs/notes/e5-design.md` sections 1, 3 and 4).
//!
//! Everything the Tauri host (`src/host.rs`, Windows only) does sits here as plain Rust,
//! built and tested on both OSes without a webview; the host's commands are thin adapters that
//! move each call to a blocking thread. The engine is reached through `lpk-api` alone (the
//! closed-build seam, `crates/lpk-api/tests/gui_seam.rs`).
//!
//! - [`grants`]: opaque tokens for paths the user chose (command line, drops, native dialogs);
//!   no command takes a raw path.
//! - [`ops`]: running operations, their cancel tokens, one heavy operation at a time, and the
//!   exit's cancel-and-join.
//! - [`journal`] and [`write`]: `create` through `<name>.lpk.partial` published without
//!   replacing anything, appends recorded, and the next start's recovery.
//! - [`app`]: sessions, the browser ([`browse`]: views, selection, pages as [`batch`] v1) and
//!   the shutdown.
//! - [`open`]: opening an entry with the shell (the allow-list, fresh zoned folders).
//! - [`cli`], [`paths`], [`strings`], [`security`].
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod app;
pub mod batch;
pub mod browse;
pub mod cli;
pub mod grants;
pub mod journal;
pub mod open;
pub mod ops;
pub mod paths;
pub mod security;
pub mod strings;
pub mod write;

include!("command_list.rs");

use serde::Serialize;

/// An error as the page sees it: the class (its message keys the page's text), D-77's kind and
/// the detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuiError {
    /// [`lpk_api::Class`]'s name, or `Usage`.
    pub class: String,
    /// D-77's kind (the engine variant's name).
    pub kind: String,
    /// The class's user-facing message.
    pub message: String,
    /// The detail.
    pub detail: String,
}

impl GuiError {
    /// A request the command layer refuses (a bad grant, a bad name).
    pub fn usage(detail: impl Into<String>) -> Self {
        lpk_api::Error::usage(detail).into()
    }

    /// The engine's error class of this error, by name.
    pub fn is(&self, class: lpk_api::Class) -> bool {
        self.class == class.name()
    }
}

impl From<lpk_api::Error> for GuiError {
    fn from(e: lpk_api::Error) -> Self {
        GuiError {
            class: e.class.name().to_string(),
            kind: e.kind.clone(),
            message: e.class.message().to_string(),
            detail: e.message,
        }
    }
}

impl std::fmt::Display for GuiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.message, self.kind, self.detail)
    }
}

impl std::error::Error for GuiError {}

/// An I/O failure of the command layer itself, as a [`GuiError`].
pub(crate) fn io_error(e: std::io::Error, path: &std::path::Path) -> GuiError {
    let class = match e.kind() {
        std::io::ErrorKind::NotFound => lpk_api::Class::NotFound,
        std::io::ErrorKind::PermissionDenied => lpk_api::Class::AccessDenied,
        std::io::ErrorKind::AlreadyExists => lpk_api::Class::Exists,
        _ => lpk_api::Class::Io,
    };
    lpk_api::Error::new(class, "Io", format!("{}: {e}", path.display()), Some(path)).into()
}
