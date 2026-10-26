//! The stub used while `vendor/libarchive-3.8.9/` is absent: every open fails with
//! [`ErrorKind::NotBuilt`](crate::ErrorKind::NotBuilt), and no other method can be reached.

use crate::{Entry, Error, OpenSpec};

/// An open archive (the stub never makes one).
#[derive(Debug)]
pub struct Archive {
    never: std::convert::Infallible,
}

impl Archive {
    /// Always fails: libarchive is not built.
    pub fn open(_spec: OpenSpec<'_>) -> Result<Archive, Error> {
        Err(Error::not_built())
    }
    /// The next entry's header (unreachable in the stub).
    pub fn next_entry(&mut self) -> Result<Option<Entry>, Error> {
        match self.never {}
    }
    /// The current entry's data (unreachable in the stub).
    pub fn read_data(&mut self, _buf: &mut [u8]) -> Result<usize, Error> {
        match self.never {}
    }
    /// The detected format's name (unreachable in the stub).
    pub fn format_name(&self) -> String {
        match self.never {}
    }
    /// The filters in use, outermost first (unreachable in the stub).
    pub fn filter_names(&self) -> Vec<String> {
        match self.never {}
    }
}
