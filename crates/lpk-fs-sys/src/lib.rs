//! Extraction output on Windows through verified directory handles (E6-6; design:
//! `docs/notes/e6-design.md` section 5). The safe API `lpk_core::outtree` builds on:
//!
//! * [`Dir`]: a directory handle held without `FILE_SHARE_DELETE` (so the directory cannot be
//!   renamed or deleted while held). [`Dir::open_path`] opens the destination as chosen;
//!   [`Dir::open_dir`] opens (or creates) one child component relative to it with
//!   `NtCreateFile` (`FILE_OPEN_REPARSE_POINT`, `OBJ_DONT_REPARSE`, `OBJ_CASE_INSENSITIVE`) and
//!   refuses a name-surrogate reparse tag (symlink, junction, mount point) as
//!   [`FsError::Linked`]; other tags (cloud placeholders, dedup) pass.
//! * [`Dir::create_file`] (`FILE_CREATE`), [`Dir::open_file`] and [`Dir::remove`] work on one
//!   component relative to the held directory and compare the [`FileId`] recorded at
//!   creation ([`FsError::Changed`] when another file took the name).
//! * [`Dir::symlink`]: a node created relative to the held directory, made a symlink with
//!   `FSCTL_SET_REPARSE_POINT` under `SeCreateSymbolicLinkPrivilege` (enabled on an
//!   impersonation copy of the process token, for the call only; the process token is not
//!   changed). [`symlink_privilege_held`] says whether the token holds it.
//!
//! * [`write_stream`]: a named stream of an open file (Mark of the Web's `Zone.Identifier`,
//!   E6-5), opened relative to the file's own handle; [`named_streams`] probes the volume's
//!   `FILE_NAMED_STREAMS` flag first.
//!
//! Every name is one component: empty, `.`, `..`, or a name with `\`, `/`, `:` or NUL is
//! refused before any call. Nothing here assumes `MAX_PATH`.
//!
//! Elsewhere the crate is empty: Linux keeps the path code of `lpk_core::outtree`.

#![deny(clippy::undocumented_unsafe_blocks)]

#[cfg(windows)]
mod windows_impl;

#[cfg(windows)]
pub use windows_impl::{
    named_streams, read_stream, symlink_privilege_held, write_stream, Dir, FileId, FsError,
};

#[cfg(all(windows, feature = "test-hooks"))]
pub use windows_impl::set_share_delete_for_tests;
