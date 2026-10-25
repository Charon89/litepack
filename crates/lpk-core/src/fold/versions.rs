//! Version groups (E2-9, design `docs/notes/e2-9-design.md` section 1): which inputs of a run
//! are versions of one tree, found from paths and names alone (nothing is read).
//!
//! **Version roots.** The run's inputs when several are given ([`RootSpec::Named`], the names
//! of `Sources::roots`), else the one input's child directories ([`RootSpec::Children`]: for
//! the contents form `lpk a out.lpk dir`, the top-level directories of the archive; for one
//! named input `x`, the directories `x/<child>`). Only files count; entries `--exclude`
//! removed are not in the walk, so they are not counted.
//!
//! **Siblings.** Two roots are siblings when at least half of the smaller root's files have
//! the same relative path in the other ([`SIBLING_NUM`]/[`SIBLING_DEN`], a writer threshold;
//! a root without files has no sibling). Connected sibling sets of at least two roots are
//! version groups; inside a group the roots are ranked by the natural order of their names
//! ([`natural_cmp`]: digit runs compared as numbers, the rest by bytes, so `v2` < `v10`).
//!
//! **Pairing.** A file's base file is the same relative path in the nearest lower rank that
//! has it ([`Versions::base_of`]); the delta probe's pairing rules use the same groups.
//!
//! **Ordering** ([`versions_order`], `FileOrder::Versions`): inside a cluster the sort key is
//! (relative path, rank, path) for a file of a group and (path, 0, path) for any other, so a
//! file's versions are adjacent; a run without version groups is left in path order, byte for
//! byte the default.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use lpk_format::EntryKind;

use crate::ingest::Input;

/// Siblings share at least `SIBLING_NUM / SIBLING_DEN` of the smaller root's files.
pub const SIBLING_NUM: u64 = 1;
/// See [`SIBLING_NUM`].
pub const SIBLING_DEN: u64 = 2;

/// Where a run's version roots come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSpec {
    /// The child directories of the root entered under this archive path (`""`: the top-level
    /// directories of the archive).
    Children(String),
    /// These archive paths (several inputs, each under its name).
    Named(Vec<String>),
}

impl RootSpec {
    /// The spec of a run over `sources`: several roots give their names, one root its children.
    pub fn of_sources(sources: &crate::ingest::Sources) -> RootSpec {
        match sources.roots.as_slice() {
            [one] => RootSpec::Children(one.name.clone()),
            many => RootSpec::Named(many.iter().map(|r| r.name.clone()).collect()),
        }
    }
}

/// Order of names with runs of ASCII digits compared as numbers (`v2` before `v10`, leading
/// zeros ignored), the rest by bytes; ties fall back to the plain byte order.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn parts(s: &str) -> Vec<(bool, &str)> {
        let mut out: Vec<(bool, &str)> = Vec::new();
        let bytes = s.as_bytes();
        let mut start = 0;
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (pa, pb) = (parts(a), parts(b));
    for (x, y) in pa.iter().zip(pb.iter()) {
        let ord = if x.0 && y.0 {
            let (tx, ty) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
            tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty))
        } else {
            x.1.as_bytes().cmp(y.1.as_bytes())
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    pa.len().cmp(&pb.len()).then_with(|| a.cmp(b))
}

/// A root of a version group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRoot {
    /// Its archive path.
    pub name: String,
    /// Its group (0, 1, ... in order of the groups' first root by name).
    pub group: usize,
    /// Its rank inside the group (0 the lowest).
    pub rank: usize,
}

/// The version groups of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Versions {
    /// Every root of every group, ordered by (group, rank).
    pub roots: Vec<VersionRoot>,
    /// Per root (same order), its files' relative paths.
    files: Vec<HashSet<String>>,
    by_name: HashMap<String, usize>,
}

impl Versions {
    /// The groups among `roots`, each given as (archive path, relative paths of its files).
    pub fn detect(roots: &[(String, Vec<String>)]) -> Versions {
        let n = roots.len();
        // Per relative path, the roots that have it; then the shared count of every pair that
        // shares one.
        let mut holders: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut sizes = vec![0u64; n];
        for (i, (_, files)) in roots.iter().enumerate() {
            let distinct: HashSet<&str> = files.iter().map(String::as_str).collect();
            sizes[i] = distinct.len() as u64;
            for f in distinct {
                holders.entry(f).or_default().push(i);
            }
        }
        let mut shared: BTreeMap<(usize, usize), u64> = BTreeMap::new();
        for list in holders.values() {
            for (k, &a) in list.iter().enumerate() {
                for &b in &list[k + 1..] {
                    *shared.entry((a.min(b), a.max(b))).or_default() += 1;
                }
            }
        }
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut x: usize) -> usize {
            while p[x] != x {
                p[x] = p[p[x]];
                x = p[x];
            }
            x
        }
        for (&(a, b), &common) in &shared {
            let smaller = sizes[a].min(sizes[b]);
            if smaller > 0 && common * SIBLING_DEN >= smaller * SIBLING_NUM {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                if ra != rb {
                    parent[ra.max(rb)] = ra.min(rb);
                }
            }
        }
        let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..n {
            let r = find(&mut parent, i);
            members.entry(r).or_default().push(i);
        }
        let mut groups: Vec<Vec<usize>> = members.into_values().filter(|g| g.len() > 1).collect();
        for g in &mut groups {
            g.sort_by(|&a, &b| natural_cmp(&roots[a].0, &roots[b].0));
        }
        groups.sort_by(|a, b| natural_cmp(&roots[a[0]].0, &roots[b[0]].0));
        let mut v = Versions::default();
        for (group, g) in groups.iter().enumerate() {
            for (rank, &i) in g.iter().enumerate() {
                v.by_name.insert(roots[i].0.clone(), v.roots.len());
                v.roots.push(VersionRoot {
                    name: roots[i].0.clone(),
                    group,
                    rank,
                });
                v.files.push(roots[i].1.iter().cloned().collect());
            }
        }
        v
    }

    /// The groups among a run's walked inputs, the roots taken as `spec` says.
    pub fn from_inputs(spec: &RootSpec, inputs: &[Input]) -> Versions {
        let mut roots: BTreeMap<String, Vec<String>> = BTreeMap::new();
        match spec {
            RootSpec::Named(names) => {
                let set: HashSet<&str> = names.iter().map(String::as_str).collect();
                for name in names {
                    roots.entry(name.clone()).or_default();
                }
                for i in inputs.iter().filter(|i| i.kind == EntryKind::File) {
                    if let Some((root, rel)) = split_at_root(&i.path, |p| set.contains(p)) {
                        if let Some(files) = roots.get_mut(root) {
                            files.push(rel.to_string());
                        }
                    }
                }
            }
            RootSpec::Children(parent) => {
                let below = if parent.is_empty() {
                    String::new()
                } else {
                    format!("{parent}/")
                };
                for i in inputs.iter().filter(|i| i.kind == EntryKind::File) {
                    let Some(rest) = i.path.strip_prefix(below.as_str()) else {
                        continue;
                    };
                    if let Some((child, rel)) = rest.split_once('/') {
                        roots
                            .entry(format!("{below}{child}"))
                            .or_default()
                            .push(rel.to_string());
                    }
                }
            }
        }
        let list: Vec<(String, Vec<String>)> = roots.into_iter().collect();
        Versions::detect(&list)
    }

    /// No version group: the ordering leaves the run in path order.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// The number of groups.
    pub fn group_count(&self) -> usize {
        self.roots.last().map_or(0, |r| r.group + 1)
    }

    /// The root of a group at a rank.
    pub fn root(&self, group: usize, rank: usize) -> Option<&VersionRoot> {
        self.roots
            .iter()
            .find(|r| r.group == group && r.rank == rank)
    }

    /// The group root `path` lies under, with the path relative to it.
    pub fn locate<'p>(&self, path: &'p str) -> Option<(&VersionRoot, &'p str)> {
        let (root, rel) = split_at_root(path, |p| self.by_name.contains_key(p))?;
        self.by_name.get(root).map(|&i| (&self.roots[i], rel))
    }

    /// Whether the root of `group` at `rank` has a file at the relative path `rel`.
    pub fn has(&self, group: usize, rank: usize, rel: &str) -> bool {
        self.roots
            .iter()
            .position(|r| r.group == group && r.rank == rank)
            .is_some_and(|i| self.files[i].contains(rel))
    }

    /// The base file of `path` (rule P1): the same relative path in the nearest lower rank of
    /// its group that has it, as an archive path.
    pub fn base_of(&self, path: &str) -> Option<String> {
        let (root, rel) = self.locate(path)?;
        (0..root.rank)
            .rev()
            .find(|&k| self.has(root.group, k, rel))
            .and_then(|k| self.root(root.group, k))
            .map(|r| format!("{}/{rel}", r.name))
    }

    /// The first version's file of `path` (rule P2): the same relative path in the lowest rank
    /// of its group that has it, when that is below its own.
    pub fn first_of(&self, path: &str) -> Option<String> {
        let (root, rel) = self.locate(path)?;
        (0..root.rank)
            .find(|&k| self.has(root.group, k, rel))
            .and_then(|k| self.root(root.group, k))
            .map(|r| format!("{}/{rel}", r.name))
    }
}

/// `path` split at the first `/` whose prefix `is_root` accepts: (root, the rest).
fn split_at_root(path: &str, is_root: impl Fn(&str) -> bool) -> Option<(&str, &str)> {
    path.match_indices('/')
        .map(|(at, _)| (&path[..at], &path[at + 1..]))
        .find(|(root, _)| is_root(root))
}

/// Order one cluster's files (given in path order) so that a file's versions are adjacent: the
/// key (relative path, rank, path) for a file under a group root and (path, 0, path) for any
/// other. Without groups nothing moves.
pub fn versions_order(inputs: &mut [Input], versions: &Versions) {
    if versions.is_empty() {
        return;
    }
    inputs.sort_by_cached_key(|i| match versions.locate(&i.path) {
        Some((root, rel)) => (rel.to_string(), root.rank, i.path.clone()),
        None => (i.path.clone(), 0, i.path.clone()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use lpk_format::EntryFlags;
    use std::path::PathBuf;

    fn file(path: &str) -> Input {
        Input {
            path: path.into(),
            kind: EntryKind::File,
            len: 1,
            mtime_ns: 0,
            flags: EntryFlags::default(),
            source: PathBuf::from(path),
            symlink_target: None,
            identity: None,
        }
    }

    fn files(paths: &[&str]) -> Vec<Input> {
        let mut v: Vec<Input> = paths.iter().map(|p| file(p)).collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }

    fn names(v: &Versions) -> Vec<(String, usize, usize)> {
        v.roots
            .iter()
            .map(|r| (r.name.clone(), r.group, r.rank))
            .collect()
    }

    #[test]
    fn natural_order_compares_digit_runs_as_numbers() {
        let mut v = vec!["v10", "v2", "v1", "v02", "v1.10", "v1.9", "a", "v"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "v", "v1", "v1.9", "v1.10", "v02", "v2", "v10"]);
        assert_eq!(natural_cmp("node-v22.1.0", "node-v22.10.0"), Ordering::Less);
    }

    #[test]
    fn the_threshold_is_half_of_the_smaller_root() {
        // a has 4 files, b shares 2 of them (half of the smaller, b's 4): siblings.
        let inputs = files(&[
            "a/1", "a/2", "a/3", "a/4", "b/1", "b/2", "b/x", "b/y", "c/1", "c/p", "c/q", "c/r",
        ]);
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &inputs);
        assert_eq!(
            names(&v),
            [("a".into(), 0, 0), ("b".into(), 0, 1)],
            "c shares 1 of 4 with each: below half"
        );
        // One file fewer shared: no group, and the order is untouched.
        let inputs = files(&["a/1", "a/2", "a/3", "b/1", "b/x", "b/y"]);
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &inputs);
        assert!(v.is_empty());
        let mut sorted = inputs.clone();
        versions_order(&mut sorted, &v);
        assert_eq!(sorted, inputs);
        // The smaller root decides: 2 files, both in a's 10.
        let mut p: Vec<String> = (0..10).map(|i| format!("big/{i}")).collect();
        p.extend(["small/0".to_string(), "small/1".to_string()]);
        let refs: Vec<&str> = p.iter().map(String::as_str).collect();
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &files(&refs));
        assert_eq!(v.group_count(), 1);
    }

    #[test]
    fn excluded_files_are_not_counted() {
        // The walk leaves excluded entries out; without b's two excluded files b shares 2 of
        // its 3 remaining files with a (6 files), with them it would share 2 of 5.
        let walked = files(&[
            "a/1", "a/2", "a/3", "a/4", "a/5", "a/6", "b/1", "b/2", "b/z",
        ]);
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &walked);
        assert_eq!(v.group_count(), 1);
        let all = files(&[
            "a/1",
            "a/2",
            "a/3",
            "a/4",
            "a/5",
            "a/6",
            "b/1",
            "b/2",
            "b/z",
            "b/big.iso",
            "b/big2.iso",
        ]);
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &all);
        assert!(v.is_empty());
    }

    #[test]
    fn groups_are_connected_sets_ranked_in_natural_order() {
        let inputs = files(&[
            "v10/lib/x.c",
            "v10/README",
            "v2/lib/x.c",
            "v2/README",
            "v1/lib/x.c",
            "v1/README",
            "docs/manual.txt",
            "other/a",
            "other2/b",
        ]);
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &inputs);
        assert_eq!(
            names(&v),
            [
                ("v1".into(), 0, 0),
                ("v2".into(), 0, 1),
                ("v10".into(), 0, 2)
            ]
        );
        assert_eq!(v.base_of("v10/lib/x.c").as_deref(), Some("v2/lib/x.c"));
        assert_eq!(v.first_of("v10/lib/x.c").as_deref(), Some("v1/lib/x.c"));
        assert_eq!(v.base_of("v1/lib/x.c"), None);
        assert_eq!(v.base_of("docs/manual.txt"), None);
        let mut order = inputs.clone();
        versions_order(&mut order, &v);
        let got: Vec<&str> = order.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            got,
            [
                "v1/README",
                "v2/README",
                "v10/README",
                "docs/manual.txt",
                "v1/lib/x.c",
                "v2/lib/x.c",
                "v10/lib/x.c",
                "other/a",
                "other2/b",
            ]
        );
        // A chain: a~b and b~c make one group even where a and c share little.
        let inputs = files(&["a/1", "a/2", "b/2", "b/3", "c/3", "c/4"]);
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &inputs);
        assert_eq!(v.group_count(), 1);
        assert_eq!(v.roots.len(), 3);
        // The nearest lower rank that has the file; none has c/4.
        assert_eq!(v.base_of("c/3").as_deref(), Some("b/3"));
        assert_eq!(v.base_of("c/4"), None);
        let inputs = files(&["a/1", "a/2", "b/1", "b/3", "c/1", "c/2"]);
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &inputs);
        // b lacks 2: c/2's base skips it to a/2.
        assert_eq!(v.base_of("c/2").as_deref(), Some("a/2"));
        assert_eq!(v.base_of("c/1").as_deref(), Some("b/1"));
        assert_eq!(v.first_of("c/1").as_deref(), Some("a/1"));
        assert_eq!(v.base_of("b/2").as_deref(), Some("a/2"));
    }

    #[test]
    fn named_roots_and_a_named_parent() {
        let inputs = files(&["old/rel-1/a", "old/rel-1/b", "new/rel-2/a", "new/rel-2/b"]);
        let spec = RootSpec::Named(vec!["old/rel-1".into(), "new/rel-2".into()]);
        let v = Versions::from_inputs(&spec, &inputs);
        assert_eq!(
            names(&v),
            [("new/rel-2".into(), 0, 0), ("old/rel-1".into(), 0, 1)],
            "ranked by the natural order of the names"
        );
        let inputs = files(&["backup/2024/a", "backup/2025/a", "backup/notes.txt"]);
        let v = Versions::from_inputs(&RootSpec::Children("backup".into()), &inputs);
        assert_eq!(
            names(&v),
            [("backup/2024".into(), 0, 0), ("backup/2025".into(), 0, 1)]
        );
    }

    #[test]
    fn no_sibling_roots_leaves_the_order_unchanged() {
        let inputs = files(&["src/a.rs", "src/b.rs", "docs/x.md", "README", "z/q"]);
        let v = Versions::from_inputs(&RootSpec::Children(String::new()), &inputs);
        assert!(v.is_empty());
        let mut order = inputs.clone();
        versions_order(&mut order, &v);
        assert_eq!(order, inputs);
    }
}
