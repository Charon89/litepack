//! Globs over entry paths (E3-4): the selection of `lpk x`, `lpk l` and `lpk export`, and the
//! exclusion of `lpk a --exclude`.
//!
//! A pattern is matched against the whole entry path, `/`-separated, as the archive stores it
//! (`docs/deep/r.bin`): `*` and `?` match within one component (`*` does not cross a `/`),
//! `**` crosses components (`**/*.log`, `docs/**`), `[...]` is a character class, `{a,b}` an
//! alternation. A pattern that matches a directory's path selects (or excludes) its subtree:
//! `docs` selects `docs/b.md` and `docs/deep/r.bin` too. A backslash is read as a separator
//! (so a Windows user may type `docs\deep`), never as an escape; `[*]` matches a literal `*`.
//! Matching is case-sensitive on every platform. No pattern is anchored to a name: `*.log`
//! matches `a.log` at the top only, `**/*.log` at any depth.
//!
//! A selection may instead be an id set ([`Selection::from_ids`], E5-4): the indices of the
//! entries in the archive's entry table (for a foreign archive, in its listing), one bit each.
//! It is exact: no path is spelled, so any name (a backslash included) can be picked and a
//! million picks cost about 125 KB; a directory's subtree is the caller's to include. It
//! applies where the entries are filtered by position ([`Selection::filter`], [`Hits`] fed
//! every entry in order); asked about a bare path it selects nothing.

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::CoreError;

/// A set of patterns over entry paths, or of entry indices; empty selects everything.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    patterns: Vec<String>,
    set: Option<GlobSet>,
    /// The id-set form: bit `i` is entry `i`.
    ids: Option<std::sync::Arc<Vec<u64>>>,
}

fn id_picked(bits: &[u64], i: usize) -> bool {
    bits.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
}

/// Compile one pattern (the rules of the module documentation).
fn glob(pattern: &str) -> Result<Glob, CoreError> {
    let pattern = pattern.replace('\\', "/");
    let bad = |why: String| CoreError::InvalidOption(format!("pattern {pattern:?}: {why}"));
    if pattern.is_empty() {
        return Err(bad("empty".to_string()));
    }
    if pattern.starts_with('/') {
        return Err(bad(
            "entry paths are relative; drop the leading `/`".to_string()
        ));
    }
    GlobBuilder::new(&pattern)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .map_err(|e| bad(e.kind().to_string()))
}

impl Selection {
    /// Everything.
    pub fn all() -> Self {
        Selection::default()
    }

    /// The patterns, compiled; an empty list is [`all`](Self::all). A pattern that does not
    /// compile is `InvalidOption`, naming it.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self, CoreError> {
        if patterns.is_empty() {
            return Ok(Selection::all());
        }
        let mut b = GlobSetBuilder::new();
        let mut kept = Vec::with_capacity(patterns.len());
        for p in patterns {
            b.add(glob(p.as_ref())?);
            kept.push(p.as_ref().to_string());
        }
        let set = b
            .build()
            .map_err(|e| CoreError::InvalidOption(format!("patterns: {}", e.kind())))?;
        Ok(Selection {
            patterns: kept,
            set: Some(set),
            ids: None,
        })
    }

    /// The entries with these indices (the position in the entry table); an index past the
    /// end of the archive selects nothing. No directory closure: the caller adds a picked
    /// directory's subtree.
    pub fn from_ids(ids: impl IntoIterator<Item = u32>) -> Self {
        let mut bits: Vec<u64> = Vec::new();
        for id in ids {
            let w = id as usize / 64;
            if w >= bits.len() {
                bits.resize(w + 1, 0);
            }
            bits[w] |= 1 << (id % 64);
        }
        Selection {
            ids: Some(std::sync::Arc::new(bits)),
            ..Selection::default()
        }
    }

    /// True when no pattern and no id set was given.
    pub fn is_all(&self) -> bool {
        self.set.is_none() && self.ids.is_none()
    }

    /// The patterns as given.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// The indices of the patterns matching `path` or one of its ancestors (`a/b/c` is tried
    /// as `a`, `a/b`, `a/b/c`), appended to `into` (duplicates possible); nothing when the
    /// selection is everything. (`GlobSet::matches_into` clears its vector, hence the
    /// scratch one.)
    fn hits_into(&self, path: &str, into: &mut Vec<usize>, scratch: &mut Vec<usize>) {
        let Some(set) = &self.set else { return };
        for (i, b) in path.bytes().enumerate() {
            if b == b'/' && i > 0 {
                set.matches_into(&path[..i], scratch);
                into.extend_from_slice(scratch);
            }
        }
        set.matches_into(path, scratch);
        into.extend_from_slice(scratch);
    }

    /// True when the path, or an ancestor of it, matches one of the patterns; false when
    /// there are none (the form an exclusion uses).
    pub fn is_match(&self, path: &str) -> bool {
        let Some(set) = &self.set else { return false };
        if set.is_match(path) {
            return true;
        }
        for (i, b) in path.bytes().enumerate() {
            if b == b'/' && i > 0 && set.is_match(&path[..i]) {
                return true;
            }
        }
        false
    }

    /// True when the path is selected: [`is_match`](Self::is_match), or the selection is
    /// everything.
    pub fn matches(&self, path: &str) -> bool {
        self.is_all() || self.is_match(path)
    }

    /// True when entry `index` at `path` is selected: by its position for an id set, else
    /// [`matches`](Self::matches).
    pub fn selects(&self, index: usize, path: &str) -> bool {
        match &self.ids {
            Some(bits) => id_picked(bits, index),
            None => self.matches(path),
        }
    }

    /// Keep the items whose path (given by `path`) matches, in their order; every pattern
    /// must match at least one item, else [`CoreError::NoMatch`] names the first (in the
    /// order given) that matched none. The selection of everything keeps everything.
    pub fn filter<T>(&self, items: Vec<T>, path: impl Fn(&T) -> &str) -> Result<Vec<T>, CoreError> {
        if self.is_all() {
            return Ok(items);
        }
        if let Some(bits) = &self.ids {
            return Ok(items
                .into_iter()
                .enumerate()
                .filter(|(i, _)| id_picked(bits, *i))
                .map(|(_, t)| t)
                .collect());
        }
        let mut hit = vec![false; self.patterns.len()];
        let (mut buf, mut scratch) = (Vec::new(), Vec::new());
        let mut kept = Vec::with_capacity(items.len());
        for item in items {
            buf.clear();
            self.hits_into(path(&item), &mut buf, &mut scratch);
            if buf.is_empty() {
                continue;
            }
            for &i in &buf {
                hit[i] = true;
            }
            kept.push(item);
        }
        self.check(&hit)?;
        Ok(kept)
    }

    /// A streaming form of [`filter`](Self::filter): a tracker fed one path at a time, then
    /// [`Hits::check`] for the patterns that matched nothing.
    pub fn hits(&self) -> Hits<'_> {
        Hits {
            selection: self,
            hit: vec![false; self.patterns.len()],
            buf: Vec::new(),
            scratch: Vec::new(),
            next: 0,
        }
    }

    fn check(&self, hit: &[bool]) -> Result<(), CoreError> {
        match hit.iter().position(|h| !h) {
            Some(i) => Err(CoreError::NoMatch {
                pattern: self.patterns[i].clone(),
            }),
            None => Ok(()),
        }
    }
}

/// The patterns matched so far (see [`Selection::hits`]).
#[derive(Debug)]
pub struct Hits<'a> {
    selection: &'a Selection,
    hit: Vec<bool>,
    buf: Vec<usize>,
    scratch: Vec<usize>,
    /// The position of the next recorded entry (an id set is read by position).
    next: usize,
}

impl Hits<'_> {
    /// Record `path`; true when it is selected.
    pub fn record(&mut self, path: &str) -> bool {
        let index = self.next;
        self.next += 1;
        if let Some(bits) = &self.selection.ids {
            return id_picked(bits, index);
        }
        if self.selection.is_all() {
            return true;
        }
        self.buf.clear();
        self.selection
            .hits_into(path, &mut self.buf, &mut self.scratch);
        for &i in &self.buf {
            self.hit[i] = true;
        }
        !self.buf.is_empty()
    }

    /// [`CoreError::NoMatch`] for the first pattern that matched no recorded path.
    pub fn check(&self) -> Result<(), CoreError> {
        self.selection.check(&self.hit)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sel(p: &[&str]) -> Selection {
        Selection::new(p).unwrap()
    }

    #[test]
    fn an_id_set_selects_by_position_whatever_the_names() {
        let s = Selection::from_ids([0, 2, 70, 200_000]);
        assert!(!s.is_all());
        let names = vec!["a", r"b\c", "a", "d"];
        let kept = s.filter(names.clone(), |p| p).unwrap();
        assert_eq!(
            kept,
            ["a", "a"],
            "positions 0 and 2, though both are named a"
        );
        assert!(s.selects(2, r"b\c") && !s.selects(1, "a"));
        assert!(s.selects(200_000, "x") && !s.selects(200_001, "x"));
        assert!(!s.matches("a"), "a bare path selects nothing");
        let mut h = s.hits();
        let got: Vec<bool> = names.iter().map(|p| h.record(p)).collect();
        assert_eq!(got, [true, false, true, false]);
        h.check().unwrap();
        let none = Selection::from_ids([]);
        assert!(!none.is_all());
        assert!(none.filter(names, |p| p).unwrap().is_empty());
    }

    #[test]
    fn everything_matches_the_empty_selection() {
        let s = Selection::all();
        assert!(s.is_all());
        assert!(s.matches("a/b"));
        assert!(!s.is_match("a/b"), "an empty exclusion excludes nothing");
        let kept = s.filter(vec!["x", "y"], |p| p).unwrap();
        assert_eq!(kept, ["x", "y"]);
    }

    #[test]
    fn a_directory_pattern_selects_its_subtree_and_a_star_stays_in_one_component() {
        let s = sel(&["docs"]);
        assert!(s.matches("docs"));
        assert!(s.matches("docs/b.md"));
        assert!(s.matches("docs/deep/r.bin"));
        assert!(!s.matches("docsx"));
        assert!(!s.matches("a/docs"));
        let s = sel(&["*.txt"]);
        assert!(s.matches("a.txt"));
        assert!(!s.matches("d/a.txt"));
        let s = sel(&["**/*.txt"]);
        assert!(s.matches("a.txt"));
        assert!(s.matches("d/e/a.txt"));
        let s = sel(&["docs/*"]);
        assert!(s.matches("docs/b.md"));
        assert!(
            s.matches("docs/deep/r.bin"),
            "docs/deep matches, so its subtree does"
        );
        assert!(!s.matches("docs"));
    }

    #[test]
    fn backslashes_are_separators_and_case_matters() {
        let s = sel(&["docs\\deep"]);
        assert!(s.matches("docs/deep/r.bin"));
        assert!(!sel(&["DOCS"]).matches("docs/b.md"));
        assert!(sel(&["[*]"]).matches("*"));
        assert!(!sel(&["[*]"]).matches("a"));
    }

    #[test]
    fn filter_keeps_order_and_names_the_first_pattern_without_a_match() {
        let s = sel(&["b", "nope", "a*"]);
        let e = s.filter(vec!["a1", "b", "c"], |p| p).unwrap_err();
        assert!(
            matches!(e, CoreError::NoMatch { ref pattern } if pattern == "nope"),
            "{e}"
        );
        let s = sel(&["b", "a*"]);
        assert_eq!(s.filter(vec!["a1", "b", "c"], |p| p).unwrap(), ["a1", "b"]);
        let mut h = s.hits();
        assert!(h.record("a2"));
        assert!(!h.record("c"));
        assert!(h.check().is_err());
        assert!(h.record("b/x"));
        assert!(h.check().is_ok());
    }

    #[test]
    fn bad_patterns_are_invalid_options() {
        for p in ["", "/abs", "[unclosed"] {
            let e = Selection::new(&[p]).unwrap_err();
            assert!(matches!(e, CoreError::InvalidOption(_)), "{p}: {e}");
        }
    }
}
