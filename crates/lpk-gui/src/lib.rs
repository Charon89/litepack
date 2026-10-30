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
//! - [`open`]: opening an entry with the shell (the allow-list, fresh zoned folders, the
//!   start-up sweep).
//! - [`pick`]: the native pickers, which mint grants.
//! - [`cli`] and [`activation`]: the verbs of `docs/API.md` (E5-7), each path minted as a
//!   grant; [`settings`]: the versioned settings file; [`update`]: the update check's gate.
//! - [`paths`], [`strings`], [`security`].
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod activation;
pub mod app;
pub mod batch;
pub mod browse;
pub mod cli;
pub mod extract;
pub mod forward;
pub mod grants;
pub mod journal;
pub mod open;
pub mod ops;
pub mod outbox;
pub mod paths;
pub mod pick;
pub mod recents;
pub mod security;
pub mod settings;
pub mod strings;
pub mod update;
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
    /// The entries of a partial operation that failed, each with its reason.
    #[serde(skip_serializing_if = "no_entries")]
    pub entries: Box<[EntryFailure]>,
    /// An envelope refusal's figures: what the archive needs and what this PC allows (the
    /// page offers "Allow X" for a memory refusal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Box<RefusalFigures>>,
}

/// One entry of a partial operation that failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryFailure {
    /// The entry's path in the archive.
    pub path: String,
    /// The error's variant name.
    pub kind: String,
    /// Why it failed.
    pub reason: String,
}

/// The figures of an envelope refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefusalFigures {
    /// The envelope field (`--memory` for a decode that needs more memory).
    pub field: String,
    /// What the archive needs.
    pub needed: u64,
    /// What this PC allows.
    pub allowed: u64,
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
        let refusal = e.refusal().map(|r| {
            Box::new(RefusalFigures {
                field: r.field.to_string(),
                needed: r.needed,
                allowed: r.allowed,
            })
        });
        // The operation's own entry list (lpk-api fills none today), else the one entry the
        // failure names, with the engine's reason.
        let mut entries: Vec<EntryFailure> = e
            .entries
            .iter()
            .map(|x| EntryFailure {
                path: x.path.clone(),
                kind: x.kind.clone(),
                reason: x.message.clone(),
            })
            .collect();
        if entries.is_empty() {
            if let Some(path) = e.entry_path() {
                entries.push(EntryFailure {
                    path: path.to_string(),
                    kind: e.kind.clone(),
                    reason: e.message.clone(),
                });
            }
        }
        let entries = entries.into_boxed_slice();
        GuiError {
            class: e.class.name().to_string(),
            kind: e.kind.clone(),
            message: e.class.message().to_string(),
            detail: e.message,
            entries,
            refusal,
        }
    }
}

fn no_entries(v: &[EntryFailure]) -> bool {
    v.is_empty()
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
