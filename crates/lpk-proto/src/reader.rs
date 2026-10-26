//! The trait a foreign-format reader implements inside the worker (E4-3 plugs the pure-Rust
//! readers in; E4-2 has only [`crate::fixture`]).

use crate::frame::Record;

/// Error codes of the `Error` frame. The host maps each to a named error; a number it does not
/// know is a protocol error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    /// The archive is damaged or not of the format.
    Corrupt = 1,
    /// A method, feature or format version the reader does not implement.
    Unsupported = 2,
    /// A password is needed (normally answered `NeedPassword` instead).
    NeedPassword = 3,
    /// The password given is wrong.
    WrongPassword = 4,
    /// Reading the archive handle failed.
    Io = 5,
    /// A listing position that does not exist.
    NoSuchMember = 6,
    /// The request was out of order or malformed.
    Protocol = 7,
    /// The reader was stopped by its sink (cancel, a closed pipe).
    Stopped = 8,
    /// The sandbox refused something the reader tried (E4-5's "the parser's report").
    Denied = 9,
}

impl ErrorCode {
    /// The code for a number from the wire.
    pub fn from_u16(n: u16) -> Option<Self> {
        Some(match n {
            1 => ErrorCode::Corrupt,
            2 => ErrorCode::Unsupported,
            3 => ErrorCode::NeedPassword,
            4 => ErrorCode::WrongPassword,
            5 => ErrorCode::Io,
            6 => ErrorCode::NoSuchMember,
            7 => ErrorCode::Protocol,
            8 => ErrorCode::Stopped,
            9 => ErrorCode::Denied,
            _ => return None,
        })
    }
}

/// A reader's failure: a code and a message for the user (shown escaped by the host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderError {
    /// What kind of failure.
    pub code: ErrorCode,
    /// Details.
    pub message: String,
}

impl ReaderError {
    /// A failure of `code` with `message`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ReaderError {
            code,
            message: message.into(),
        }
    }
    /// The archive is damaged.
    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Corrupt, message)
    }
}

impl From<std::io::Error> for ReaderError {
    fn from(e: std::io::Error) -> Self {
        ReaderError::new(ErrorCode::Io, e.to_string())
    }
}

impl From<Stopped> for ReaderError {
    fn from(_: Stopped) -> Self {
        ReaderError::new(ErrorCode::Stopped, "stopped")
    }
}

/// The sink refused more data (the host cancelled, or the pipe closed); the reader returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

/// A member's bytes go to the sink in pieces of any size; the sink cuts them into frames.
pub type Sink<'a> = dyn FnMut(&[u8]) -> Result<(), Stopped> + 'a;

/// A foreign-format reader over the archive handle the worker received. One reader per worker
/// (one archive); it never sees the destination, and it writes nothing but what it hands to
/// the sink.
pub trait ForeignReader {
    /// The format's name, for messages (`Opened`).
    fn format(&self) -> String;
    /// At most `max` records from position `from`, and whether more follow.
    fn entries(&mut self, from: u64, max: usize) -> Result<(Vec<Record>, bool), ReaderError>;
    /// Stream the member at `index` to `sink`; `Ok(checked)` when it ended, `checked` when a
    /// checksum the archive stores was verified.
    fn read_member(&mut self, index: u64, sink: &mut Sink<'_>) -> Result<bool, ReaderError>;
}

/// What `Open` asked for: the format the host detected, the archive's file name (no
/// directory) and the password.
#[derive(Debug, Clone, Copy)]
pub struct OpenArgs<'a> {
    /// The host's detected format (wire name).
    pub format: &'a str,
    /// The archive's file name, possibly empty.
    pub name: &'a [u8],
    /// The password, when one was given.
    pub password: Option<&'a [u8]>,
}

/// Opens a reader over the archive as `args.format` with an optional password; `Err` with
/// [`ErrorCode::NeedPassword`] or [`ErrorCode::WrongPassword`] for the credential cases and
/// [`ErrorCode::Unsupported`] for a format the worker has no reader for.
pub type Opener =
    dyn Fn(std::fs::File, &OpenArgs<'_>) -> Result<Box<dyn ForeignReader>, ReaderError>;
