//! The broker protocol between the LitePack host and a sandboxed foreign-format worker (E4-2;
//! design: `docs/notes/e4-design.md` §2).
//!
//! * [`frame`]: versioned, length-prefixed frames — `u32` LE payload length, `u16` LE kind,
//!   payload; control frames at most [`frame::MAX_CONTROL`], data and list pages at most
//!   [`frame::MAX_BULK`]; every count field is checked against the bytes left in its frame before
//!   anything is allocated, so no input can make the decoder allocate more than one frame.
//! * [`reader`]: the [`reader::ForeignReader`] trait a format reader implements in the worker.
//! * [`worker`]: the worker loop — `Hello`, then `Open`, `List` pages, streamed `ReadMember`,
//!   `Cancel`, `Close`; a panic in a reader becomes a `Fatal` frame.
//! * [`fixture`]: a trivial test-only archive format and its reader, so the whole path (spawn,
//!   protocol, the host's limits and policy pass) is exercised before E4-3's real readers land.
//!
//! The crate holds no parser of a real format and depends on nothing but `zeroize`; it is shared
//! by path between the main workspace (the host, `lpk_core::foreign`) and the `workers/`
//! workspace (`lpk-worker`). The `faults` feature (test-only, never in a shipped worker) makes
//! the worker loop misbehave on purpose.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod fixture;
pub mod frame;
pub mod reader;
pub mod worker;

pub use frame::{ProtoError, Record, RecordKind, Request, Response};
pub use reader::{ErrorCode, ForeignReader, ReaderError, Stopped};
pub use zeroize::Zeroizing;

/// The protocol version both sides must speak (`Hello::proto`).
pub const PROTO: u16 = 1;

/// The build identity the worker announces in `Hello` and the host requires to be its own:
/// both sides are built from the same `lpk-proto`.
pub const BUILD: &str = concat!("lpk-proto/", env!("CARGO_PKG_VERSION"), "/1");

/// E8-7 red-run evidence (throwaway PR, never merged).
pub fn e8_7_planted(x: &u8) -> u8 {
    unsafe { *(x as *const u8) }
}
