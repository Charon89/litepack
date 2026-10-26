//! The LitePack engine API for front ends, v0 (E5-1; `docs/API.md`). The GUI (E5-2) reaches
//! the engine through this crate alone (a manifest test keeps the seam), and the `lpk` command
//! line runs its archive commands through it.
//!
//! - [`Session`]: one opened archive (`.lpk` in process, or a foreign archive through E4's
//!   sandboxed broker): [`Session::info`], streaming [`Session::for_each`], the lazily built
//!   listing [`Session::model`] ([`ListModel`]: pages, sort, filter, selection totals in
//!   Rust), [`Session::read_entry`] (capped, cancellable), [`Session::preflight`],
//!   [`Session::extract`], [`Session::export`], [`Session::test`], [`Session::check`],
//!   [`Session::repair`], [`Session::history`], [`Session::rollback`], [`Session::append`].
//! - [`create`], [`append`], [`rollback`] and [`calibrate`]: the write side.
//! - [`Error`] with its [`Class`] (user-facing messages) and D-77's `kind`.
//!
//! Long calls take a [`Monitor`] (progress events and a [`CancelToken`], D-87) or a token.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod create;
pub mod error;
pub mod export;
pub mod model;
mod session;

pub use create::{
    append, calibrate, committed_generation, create, rollback, CreateSettings, Rate, Seal, Tier,
};
pub use error::{Class, EntryError, Error};
pub use export::{Container, ExportOptions, Method};
pub use model::{Filter, IdSet, Key, ListModel, Order, Row, RowRef, Scope, Totals, View, ROOT};
pub use session::{
    check_export_target, Destination, EntryRef, ExtractOptions, ExtractReport, Extracted,
    GenerationInfo, Info, OpenOptions, Preflight, RecoveryReport, Selection, Session, TestReport,
};

/// Engine types a front end names through this crate (re-exported so that it needs no other
/// engine crate).
pub use lpk_core::foreign::{ForeignSummary, Format as ForeignFormat, Limits as ForeignLimits};
pub use lpk_core::policy::TargetRefusal;
pub use lpk_core::{
    CancelToken, Event, ExportSummary, ExtractSummary, FileOrder, Monitor, Overwrite, Progress,
    Root, RunSummary, Selection as Globs, Sources, Stage, Symlinks,
};
pub use lpk_format::{Credentials, Entry, EntryFlags, EntryKind, Refusal, Suite};

/// The broker's options for a foreign archive (E4's limits and the worker).
pub type ForeignOptions = lpk_core::foreign::Options;

#[cfg(test)]
mod tests {
    /// E5-1: a session can be shared across threads (the GUI's commands run on a pool).
    #[test]
    fn session_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::Session>();
        assert_send_sync::<super::ListModel>();
        assert_send_sync::<super::Error>();
    }
}
