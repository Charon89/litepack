//! The bundled priors: three zstd dictionaries trained once on redistributable corpus files
//! (`crates/lpk-core/priors/priors.toml` records the training command, the zstd version and the
//! sample files; `docs/LICENSING.md` names the sources and their licences).
//!
//! An archive written with a bundled dictionary names it by its BLAKE3 id in the zstd step. To
//! extract such an archive outside this crate, pass the dictionary file to the format tool:
//! `lpk-decode extract <archive> --prior crates/lpk-core/priors/prose.dict` (one `--prior` per
//! dictionary the archive uses).

use lpk_format::{prior_id, PriorStore};

use crate::cluster::DictionaryKind;

static PROSE: &[u8] = include_bytes!("../priors/prose.dict");
static SOURCE: &[u8] = include_bytes!("../priors/source.dict");
static STRUCTURED: &[u8] = include_bytes!("../priors/structured.dict");

/// The bundled dictionaries as a [`PriorStore`].
#[derive(Debug, Clone)]
pub struct BundledPriors {
    entries: [(DictionaryKind, [u8; 32], &'static [u8]); 3],
}

impl Default for BundledPriors {
    fn default() -> Self {
        Self::new()
    }
}

impl BundledPriors {
    /// The three bundled dictionaries with their ids.
    pub fn new() -> Self {
        let e = |k, b: &'static [u8]| (k, prior_id(b), b);
        BundledPriors {
            entries: [
                e(DictionaryKind::Prose, PROSE),
                e(DictionaryKind::Structured, STRUCTURED),
                e(DictionaryKind::Source, SOURCE),
            ],
        }
    }

    /// The dictionary of a kind, with its id; `None` for [`DictionaryKind::None`].
    pub fn dictionary(&self, kind: DictionaryKind) -> Option<([u8; 32], &'static [u8])> {
        self.entries
            .iter()
            .find(|(k, _, _)| *k == kind)
            .map(|(_, id, b)| (*id, *b))
    }
}

impl PriorStore for BundledPriors {
    fn get(&self, id: &[u8; 32]) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(_, i, _)| i == id)
            .map(|(_, _, b)| *b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// `(name, id)` pairs of the `[[dictionary]]` tables of `priors.toml`.
    fn recorded() -> Vec<(String, String)> {
        let text = include_str!("../priors/priors.toml");
        let mut out = Vec::new();
        let mut name = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("name = ") {
                name = Some(v.trim_matches('"').to_string());
            } else if let Some(v) = line.strip_prefix("id = ") {
                if let Some(n) = name.take() {
                    out.push((n, v.trim_matches('"').to_string()));
                }
            }
        }
        out
    }

    #[test]
    fn ids_match_priors_toml() {
        let p = BundledPriors::new();
        let rec = recorded();
        assert_eq!(rec.len(), 3);
        for (kind, name) in [
            (DictionaryKind::Prose, "prose"),
            (DictionaryKind::Structured, "structured"),
            (DictionaryKind::Source, "source"),
        ] {
            let (id, bytes) = p.dictionary(kind).unwrap();
            assert_eq!(id, *blake3::hash(bytes).as_bytes());
            let want = &rec.iter().find(|(n, _)| n == name).unwrap().1;
            assert_eq!(&hex(&id), want, "{name}");
            assert_eq!(p.get(&id), Some(bytes));
        }
        assert!(p.dictionary(DictionaryKind::None).is_none());
        assert!(p.get(&[0; 32]).is_none());
    }
}
