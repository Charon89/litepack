//! Caller-supplied zstd dictionaries. No dictionary is bundled with the product: the caller
//! hands the Fast tier a [`ProvidedDictionaries`] naming, per dictionary kind, the dictionary
//! to use. An archive written with one names it by its BLAKE3 id (the prior id) in the zstd step.
//!
//! To extract such an archive, give the dictionary files to the format tool, one `--prior` each:
//! `lpk-decode extract <archive> --prior <dictionary file>`; through `lpk_format::Archive` pass
//! the same set (it is a [`PriorStore`]) to `set_priors`.

use lpk_format::{prior_id, PriorStore};

use crate::cluster::DictionaryKind;

/// Dictionaries chosen by the caller, at most one per [`DictionaryKind`] other than `None`.
#[derive(Debug, Clone, Default)]
pub struct ProvidedDictionaries {
    entries: Vec<(DictionaryKind, [u8; 32], Vec<u8>)>,
}

impl ProvidedDictionaries {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Use `bytes` as the dictionary of `kind` (replacing an earlier one). The id is the BLAKE3
    /// of the bytes. `DictionaryKind::None` is ignored.
    pub fn with(mut self, kind: DictionaryKind, bytes: Vec<u8>) -> Self {
        if kind == DictionaryKind::None {
            return self;
        }
        self.entries.retain(|(k, _, _)| *k != kind);
        self.entries.push((kind, prior_id(&bytes), bytes));
        self
    }

    /// The dictionary of a kind with its id.
    pub fn dictionary(&self, kind: DictionaryKind) -> Option<([u8; 32], &[u8])> {
        self.entries
            .iter()
            .find(|(k, _, _)| *k == kind)
            .map(|(_, id, b)| (*id, b.as_slice()))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (DictionaryKind, [u8; 32], &[u8])> {
        self.entries
            .iter()
            .map(|(k, id, b)| (*k, *id, b.as_slice()))
    }
}

impl PriorStore for ProvidedDictionaries {
    fn get(&self, id: &[u8; 32]) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(_, i, _)| i == id)
            .map(|(_, _, b)| b.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_the_blake3_of_the_bytes_and_none_is_ignored() {
        let p = ProvidedDictionaries::new()
            .with(DictionaryKind::Prose, b"abc".to_vec())
            .with(DictionaryKind::None, b"x".to_vec());
        let (id, bytes) = p.dictionary(DictionaryKind::Prose).unwrap();
        assert_eq!(id, *blake3::hash(b"abc").as_bytes());
        assert_eq!(p.get(&id), Some(bytes));
        assert!(p.dictionary(DictionaryKind::None).is_none());
        assert!(p.get(&[0; 32]).is_none());
        assert_eq!(p.iter().count(), 1);
    }
}
