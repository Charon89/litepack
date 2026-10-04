//! `lpk-check`: an independent decoder of the `.lpk` v1 format.
//!
//! Written from `docs/spec/lpk-v1.md` and the conformance vectors only, without
//! reading the reference implementation (task E1-14c). Section numbers in the
//! comments refer to that specification.
#![forbid(unsafe_code)]

pub mod archive;
pub mod block;
pub mod crypto;
pub mod entries;
pub mod error;
pub mod extract;
pub mod index;
pub mod inflate;
pub mod journal;
pub mod jpeg;
pub mod keyless;
pub mod merkle;
pub mod record;
pub mod recovery;
pub mod wire;

pub use archive::{Archive, Options, Resources};
pub use error::{Error, Result};
