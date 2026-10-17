//! The text-encoding transforms (design `docs/notes/e2-17-design.md`, sections 1, 2 and 5,
//! task P): base64 run detection and the regularity rule with the decode and re-encode of a
//! regular run from exactly the fields of the frozen `Base64Record` ([`base64`]); UTF-16 text
//! detection and the conversion between UTF-16 and generalized UTF-8 ([`utf16`]).
//!
//! No format change: these are the transforms `probe text-encodings` measures. A writer stage
//! that applies them needs the revision the design describes (section 3) and is not here.

pub mod base64;
pub mod utf16;

pub use base64::{
    find_runs, Alphabet, Analysed, Irregular, Label, Layout, LineEnding, Run, MIN_RUN,
};
pub use utf16::{
    convert, detect, find_binary_runs, from_wtf8, to_utf16, to_wtf8, Converted, Detected, Endian,
    Wtf8Error, MIN_BINARY_RUN_UNITS,
};
