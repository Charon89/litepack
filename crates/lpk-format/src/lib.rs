//! LitePack `.lpk` container format: header, varints and the hashed frame grammar.
//!
//! This is the reference reader side of the format; see `docs/spec/lpk-v1.md`.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod error;
pub mod frame;
pub mod header;
pub mod magic;
pub mod varint;

pub use error::FormatError;
pub use frame::{
    frame_flag_table, frame_kind_table, Frame, FrameFlags, FrameKind, ReadFrame, ReadLimits,
};
pub use header::{header_byte_table, header_flag_table, FormatVersion, Header, HeaderFlags};
pub use magic::MAGIC;
