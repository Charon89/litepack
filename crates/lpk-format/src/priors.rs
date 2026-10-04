//! Priors by ID (spec section 10, D-10): a prior is a byte string a decoder
//! needs besides the archive, named by the BLAKE3 hash of its content. The
//! reader never fetches one: the caller hands it a [`PriorStore`].

use std::collections::BTreeMap;

/// The ID of a prior: BLAKE3-256 of its bytes.
pub fn prior_id(dictionary: &[u8]) -> [u8; 32] {
    *blake3::hash(dictionary).as_bytes()
}

/// Where a decoder finds the priors an archive names. Implemented by the
/// caller; this crate does no lookup of its own.
pub trait PriorStore: Send + Sync {
    /// The bytes of the prior with this ID, if the store has it.
    fn get(&self, id: &[u8; 32]) -> Option<&[u8]>;
}

/// The default store: it has nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoPriors;

impl PriorStore for NoPriors {
    fn get(&self, _id: &[u8; 32]) -> Option<&[u8]> {
        None
    }
}

/// A store over priors held in memory, keyed by their computed ID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryPriors {
    map: BTreeMap<[u8; 32], Vec<u8>>,
}

impl MemoryPriors {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a prior; returns its ID.
    pub fn insert(&mut self, bytes: Vec<u8>) -> [u8; 32] {
        let id = prior_id(&bytes);
        self.map.insert(id, bytes);
        id
    }

    /// Number of priors held.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when the store holds nothing.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl PriorStore for MemoryPriors {
    fn get(&self, id: &[u8; 32]) -> Option<&[u8]> {
        self.map.get(id).map(Vec::as_slice)
    }
}

/// The Markdown table of the prior list in the index payload, pasted verbatim
/// into the spec.
pub fn prior_list_table() -> String {
    "| Field | Size | Meaning |\n|---|---|---|\n\
     | prior_count | varint | number of priors the archive's blocks name; at most the bytes left after it divided by 32 |\n\
     | prior_id | 32 | per prior: the BLAKE3-256 of the prior's bytes; ascending, unique, never all zeros |\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_blake3() {
        assert_eq!(prior_id(b"abc"), *blake3::hash(b"abc").as_bytes());
        assert_ne!(prior_id(b""), [0u8; 32]);
    }

    #[test]
    fn stores() {
        assert!(NoPriors.get(&[1; 32]).is_none());
        let mut m = MemoryPriors::new();
        assert!(m.is_empty());
        let id = m.insert(b"dict".to_vec());
        assert_eq!(id, prior_id(b"dict"));
        assert_eq!(m.get(&id), Some(&b"dict"[..]));
        assert!(m.get(&[0; 32]).is_none());
        assert_eq!(m.len(), 1);
    }
}
