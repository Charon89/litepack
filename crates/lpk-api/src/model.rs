//! The listing model (E5-1, for E5-3's virtual list): built once per session on the first
//! [`crate::Session::model`] call, never for a plain listing (`for_each` streams).
//!
//! It holds a name arena (every entry's path, `P` bytes in all), one [`Row`] per entry, the
//! children of every directory as one `u32` vector (each directory's children contiguous),
//! and serves [`View`]s: one `u32` vector of the ids in a scope, filtered and sorted. A
//! re-sort builds its view beside the one being served, so two views coexist; sorting is in
//! place (`sort_unstable_by`, ties broken by id). A selection is an [`IdSet`], one bit per
//! entry. The memory bound ([`ListModel::bound`]), over what the reader's open holds:
//!
//! `N·(size_of::<Row>() + 4·size_of::<u32>()) + P + ⌈N/8⌉ + 1 MiB`
//!
//! (the four `u32` per entry: the children vector, two views, and the build's slack).

use std::cmp::Ordering;

use lpk_core::CancelToken;
use lpk_format::{Entry, EntryKind};

use crate::error::{Class, Error};

/// The id of the root (no parent entry).
pub const ROOT: u32 = u32::MAX;

/// One entry of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// File size in bytes (0 for a directory).
    pub size: u64,
    /// Modification time (ns since the epoch; `i64::MIN` when none).
    pub mtime_ns: i64,
    /// The nearest ancestor directory entry, or [`ROOT`].
    pub parent: u32,
    /// Where this directory's children start in the children vector.
    pub first_child: u32,
    /// How many children it has.
    pub child_count: u32,
    /// The path's offset in the name arena.
    pub name_off: u32,
    /// The path's length.
    pub name_len: u32,
    /// 0 file, 1 directory, 2 symlink.
    pub kind: u8,
    /// The entry flags' bits.
    pub flags: u8,
}

/// A set of entry ids, one bit per entry (a selection; [`crate::Selection::Ids`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdSet {
    bits: Vec<u64>,
    len: u32,
}

impl IdSet {
    /// An empty set over `len` ids.
    pub fn new(len: u32) -> Self {
        IdSet {
            bits: vec![0; (len as usize).div_ceil(64)],
            len,
        }
    }

    /// The ids it ranges over.
    pub fn capacity(&self) -> u32 {
        self.len
    }

    /// Add `id` (ignored past the range).
    pub fn insert(&mut self, id: u32) {
        if id < self.len {
            self.bits[id as usize / 64] |= 1 << (id % 64);
        }
    }

    /// Remove `id`.
    pub fn remove(&mut self, id: u32) {
        if id < self.len {
            self.bits[id as usize / 64] &= !(1 << (id % 64));
        }
    }

    /// True when `id` is in the set.
    pub fn contains(&self, id: u32) -> bool {
        id < self.len && self.bits[id as usize / 64] & (1 << (id % 64)) != 0
    }

    /// How many ids are in the set.
    pub fn count(&self) -> u64 {
        self.bits.iter().map(|w| u64::from(w.count_ones())).sum()
    }
}

/// What a view lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every entry (the flat view).
    All,
    /// The children of a directory entry, or of the root ([`ROOT`]).
    Children(u32),
}

/// The sort key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// The full path (the archive's order).
    Path,
    /// The last path component.
    Name,
    /// The size.
    Size,
    /// The modification time.
    Modified,
    /// Directories, then files, then symlinks; by name within.
    Kind,
}

/// A sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Order {
    /// The key.
    pub key: Key,
    /// Largest first.
    pub descending: bool,
}

impl Default for Order {
    fn default() -> Self {
        Order {
            key: Key::Path,
            descending: false,
        }
    }
}

/// A filter over paths.
#[derive(Debug, Clone, Default)]
pub enum Filter {
    /// Everything.
    #[default]
    None,
    /// The path contains this text (ASCII case-insensitive).
    Contains(String),
    /// D-90's globs over the path.
    Globs(lpk_core::Selection),
}

/// A sorted, filtered scope: the ids in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// Increases with every view the model builds (a front end drops stale pages).
    pub generation: u64,
    ids: Vec<u32>,
}

impl View {
    /// Rows in the view.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// True when the view is empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The ids in display order.
    pub fn ids(&self) -> &[u32] {
        &self.ids
    }
}

/// One row of a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowRef<'m> {
    /// The entry id.
    pub id: u32,
    /// The row.
    pub row: Row,
    /// The full path.
    pub path: &'m str,
    /// The last component.
    pub name: &'m str,
}

/// The totals of a view and a selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Rows in the view.
    pub entries: u64,
    /// Files among them.
    pub files: u64,
    /// Directories among them.
    pub directories: u64,
    /// Their file bytes.
    pub bytes: u64,
    /// Picked rows in the view.
    pub picked: u64,
    /// Their file bytes.
    pub picked_bytes: u64,
}

/// The listing model (see the module documentation).
#[derive(Debug)]
pub struct ListModel {
    rows: Vec<Row>,
    names: Vec<u8>,
    children: Vec<u32>,
    root_first: u32,
    root_count: u32,
    archive_generation: u64,
    views: std::sync::atomic::AtomicU64,
}

/// Builds a [`ListModel`] from the entries in table order.
pub(crate) struct Builder {
    rows: Vec<Row>,
    names: Vec<u8>,
    sorted: bool,
    generation: u64,
}

fn kind_code(k: EntryKind) -> u8 {
    match k {
        EntryKind::File => 0,
        EntryKind::Directory => 1,
        _ => 2,
    }
}

fn too_large(what: &str) -> Error {
    Error::new(
        Class::TooLarge,
        "TooLarge",
        format!("the listing model holds at most 2^32 - 1 {what}"),
        None,
    )
}

impl Builder {
    /// Room for `n` entries whose paths total `names` bytes (both exact: no growth).
    pub(crate) fn new(n: u64, names: u64, generation: u64) -> Result<Self, Error> {
        if n >= u64::from(ROOT) {
            return Err(too_large("entries"));
        }
        if names > u64::from(u32::MAX) {
            return Err(too_large("name bytes"));
        }
        Ok(Builder {
            rows: Vec::with_capacity(n as usize),
            names: Vec::with_capacity(names as usize),
            sorted: true,
            generation,
        })
    }

    pub(crate) fn push(&mut self, e: &Entry) -> Result<(), Error> {
        let off = u32::try_from(self.names.len()).map_err(|_| too_large("name bytes"))?;
        let len = u32::try_from(e.path.len()).map_err(|_| too_large("name bytes"))?;
        if let Some(prev) = self.rows.last() {
            let p = &self.names[prev.name_off as usize..(prev.name_off + prev.name_len) as usize];
            if p >= e.path.as_bytes() {
                self.sorted = false;
            }
        }
        self.names.extend_from_slice(e.path.as_bytes());
        if self.rows.len() >= ROOT as usize - 1 {
            return Err(too_large("entries"));
        }
        self.rows.push(Row {
            size: e.size,
            mtime_ns: e.mtime_ns,
            parent: ROOT,
            first_child: 0,
            child_count: 0,
            name_off: off,
            name_len: len,
            kind: kind_code(e.kind),
            flags: e.flags.bits() as u8,
        });
        Ok(())
    }

    pub(crate) fn finish(self, c: &CancelToken) -> Result<ListModel, Error> {
        let Builder {
            mut rows,
            names,
            sorted,
            generation,
        } = self;
        let n = rows.len();
        let path = |rows: &[Row], i: usize| -> &[u8] {
            let r = &rows[i];
            &names[r.name_off as usize..(r.name_off + r.name_len) as usize]
        };
        // Path order for the parent search: the rows themselves when the table is sorted
        // (every `.lpk`), else a sorted id vector (a foreign listing), dropped after.
        let by_path: Option<Vec<u32>> = (!sorted).then(|| {
            let mut v: Vec<u32> = (0..n as u32).collect();
            v.sort_unstable_by(|a, b| path(&rows, *a as usize).cmp(path(&rows, *b as usize)));
            v
        });
        let find = |rows: &[Row], p: &[u8]| -> Option<u32> {
            match &by_path {
                None => {
                    let (mut lo, mut hi) = (0usize, n);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        match path(rows, mid).cmp(p) {
                            Ordering::Less => lo = mid + 1,
                            Ordering::Greater => hi = mid,
                            Ordering::Equal => return Some(mid as u32),
                        }
                    }
                    None
                }
                Some(v) => v
                    .binary_search_by(|id| path(rows, *id as usize).cmp(p))
                    .ok()
                    .map(|i| v[i]),
            }
        };
        let mut root_count = 0u32;
        for i in 0..n {
            if i % 4096 == 0 {
                c.check().map_err(|e| Error::core(e, None))?;
            }
            let mut p = path(&rows, i);
            let mut parent = ROOT;
            while let Some(cut) = p.iter().rposition(|&b| b == b'/') {
                p = &p[..cut];
                if let Some(id) = find(&rows, p) {
                    if rows[id as usize].kind == 1 {
                        parent = id;
                        break;
                    }
                }
            }
            rows[i].parent = parent;
            if parent == ROOT {
                root_count += 1;
            } else {
                rows[parent as usize].child_count += 1;
            }
        }
        drop(by_path);
        // The children vector: each directory's children contiguous, in table order. The
        // starts are set to the ends, then decremented as children are placed in reverse.
        let mut end = root_count;
        let root_first = 0u32;
        let mut root_cursor = root_count;
        for r in rows.iter_mut() {
            end += r.child_count;
            r.first_child = end;
        }
        let mut children = vec![0u32; n];
        for i in (0..n).rev() {
            let p = rows[i].parent;
            let slot = if p == ROOT {
                root_cursor -= 1;
                root_cursor
            } else {
                rows[p as usize].first_child -= 1;
                rows[p as usize].first_child
            };
            children[slot as usize] = i as u32;
        }
        Ok(ListModel {
            rows,
            names,
            children,
            root_first,
            root_count,
            archive_generation: generation,
            views: std::sync::atomic::AtomicU64::new(0),
        })
    }
}

impl ListModel {
    /// Entries in the model.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// True when the archive has no entry.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The archive generation the model lists.
    pub fn archive_generation(&self) -> u64 {
        self.archive_generation
    }

    /// The bytes of every path (`P` of the bound).
    pub fn name_bytes(&self) -> usize {
        self.names.len()
    }

    /// The memory bound over the reader's open, in bytes, for `n` entries whose paths total
    /// `p` bytes: `n·(size_of::<Row>() + 4·size_of::<u32>()) + p + ⌈n/8⌉ + 1 MiB`.
    pub fn bound(n: usize, p: usize) -> usize {
        n * (std::mem::size_of::<Row>() + 4 * std::mem::size_of::<u32>())
            + p
            + n.div_ceil(8)
            + (1 << 20)
    }

    /// The row of entry `id`.
    pub fn row(&self, id: u32) -> Option<&Row> {
        self.rows.get(id as usize)
    }

    /// The path of entry `id`.
    pub fn path(&self, id: u32) -> &str {
        let Some(r) = self.rows.get(id as usize) else {
            return "";
        };
        std::str::from_utf8(&self.names[r.name_off as usize..(r.name_off + r.name_len) as usize])
            .unwrap_or_default()
    }

    /// The last component of entry `id`'s path.
    pub fn name(&self, id: u32) -> &str {
        let p = self.path(id);
        p.rsplit('/').next().unwrap_or(p)
    }

    /// The children of a directory entry, or of the root ([`ROOT`]), in table order.
    pub fn children(&self, id: u32) -> &[u32] {
        let (first, count) = if id == ROOT {
            (self.root_first, self.root_count)
        } else {
            match self.rows.get(id as usize) {
                Some(r) => (r.first_child, r.child_count),
                None => (0, 0),
            }
        };
        &self.children[first as usize..(first + count) as usize]
    }

    fn compare(&self, a: u32, b: u32, order: Order) -> Ordering {
        let (ra, rb) = (&self.rows[a as usize], &self.rows[b as usize]);
        let o = match order.key {
            Key::Path => a.cmp(&b),
            Key::Name => self.name(a).cmp(self.name(b)),
            Key::Size => ra.size.cmp(&rb.size),
            Key::Modified => ra.mtime_ns.cmp(&rb.mtime_ns),
            Key::Kind => {
                let rank = |k: u8| match k {
                    1 => 0,
                    0 => 1,
                    _ => 2,
                };
                rank(ra.kind)
                    .cmp(&rank(rb.kind))
                    .then_with(|| self.name(a).cmp(self.name(b)))
            }
        };
        // Ties stay in table order whatever the direction.
        let o = if order.descending { o.reverse() } else { o };
        o.then_with(|| a.cmp(&b))
    }

    /// Build a view: the ids of `scope` passing `filter`, sorted by `order`. Cancellable
    /// while it filters.
    pub fn view(
        &self,
        scope: Scope,
        order: Order,
        filter: &Filter,
        c: &CancelToken,
    ) -> Result<View, Error> {
        let keep = |id: u32| -> bool {
            match filter {
                Filter::None => true,
                Filter::Contains(t) => {
                    let p = self.path(id).as_bytes();
                    let t = t.as_bytes();
                    t.is_empty() || p.windows(t.len()).any(|w| w.eq_ignore_ascii_case(t))
                }
                Filter::Globs(g) => g.matches(self.path(id)),
            }
        };
        let mut ids = Vec::new();
        let candidates: Box<dyn Iterator<Item = u32>> = match scope {
            Scope::All => Box::new(0..self.rows.len() as u32),
            Scope::Children(id) => Box::new(self.children(id).iter().copied()),
        };
        let count = match scope {
            Scope::All => self.rows.len(),
            Scope::Children(id) => self.children(id).len(),
        };
        if matches!(filter, Filter::None) {
            ids.reserve_exact(count);
        }
        for (i, id) in candidates.enumerate() {
            if i % 4096 == 0 {
                c.check().map_err(|e| Error::core(e, None))?;
            }
            if keep(id) {
                ids.push(id);
            }
        }
        ids.shrink_to_fit();
        if order != Order::default() {
            ids.sort_unstable_by(|a, b| self.compare(*a, *b, order));
        }
        Ok(View {
            generation: self
                .views
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1,
            ids,
        })
    }

    /// `n` rows of `view` from row `first` (fewer at the end).
    pub fn rows(&self, view: &View, first: usize, n: usize) -> Vec<RowRef<'_>> {
        view.ids
            .iter()
            .skip(first)
            .take(n)
            .map(|&id| RowRef {
                id,
                row: self.rows[id as usize],
                path: self.path(id),
                name: self.name(id),
            })
            .collect()
    }

    /// The totals of `view`, and of the rows of it in `picks`.
    pub fn totals(&self, view: &View, picks: Option<&IdSet>) -> Totals {
        let mut t = Totals::default();
        for &id in &view.ids {
            let r = &self.rows[id as usize];
            t.entries += 1;
            match r.kind {
                0 => {
                    t.files += 1;
                    t.bytes += r.size;
                }
                1 => t.directories += 1,
                _ => {}
            }
            if picks.is_some_and(|p| p.contains(id)) {
                t.picked += 1;
                if r.kind == 0 {
                    t.picked_bytes += r.size;
                }
            }
        }
        t
    }

    /// An empty selection sized for this model.
    pub fn new_selection(&self) -> IdSet {
        IdSet::new(self.rows.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lpk_format::EntryFlags;

    fn entry(path: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            path: path.to_string(),
            kind,
            flags: EntryFlags::EMPTY,
            mtime_ns: size as i64,
            size,
            chunks: Vec::new(),
            symlink_target: None,
        }
    }

    fn model(entries: &[Entry]) -> ListModel {
        let names = entries.iter().map(|e| e.path.len() as u64).sum();
        let mut b = Builder::new(entries.len() as u64, names, 0).unwrap();
        for e in entries {
            b.push(e).unwrap();
        }
        b.finish(&CancelToken::new()).unwrap()
    }

    #[test]
    fn the_tree_sort_filter_and_totals() {
        use EntryKind::*;
        let es = [
            entry("a", Directory, 0),
            entry("a-b.txt", File, 5),
            entry("a/x.txt", File, 3),
            entry("a/y", Directory, 0),
            entry("a/y/z.bin", File, 9),
            entry("q/orphan.txt", File, 1),
        ];
        let m = model(&es);
        assert_eq!(m.children(ROOT), &[0, 1, 5]);
        assert_eq!(m.children(0), &[2, 3]);
        assert_eq!(m.children(3), &[4]);
        assert_eq!(m.name(4), "z.bin");
        let c = CancelToken::new();
        let by_size = Order {
            key: Key::Size,
            descending: true,
        };
        let v = m.view(Scope::All, by_size, &Filter::None, &c).unwrap();
        assert_eq!(v.ids(), &[4, 1, 2, 5, 0, 3]);
        let v2 = m
            .view(
                Scope::All,
                Order::default(),
                &Filter::Contains("TXT".into()),
                &c,
            )
            .unwrap();
        assert_eq!(v2.ids(), &[1, 2, 5]);
        assert!(v2.generation > v.generation);
        let mut picks = m.new_selection();
        picks.insert(2);
        picks.insert(4);
        let t = m.totals(&v, Some(&picks));
        assert_eq!((t.entries, t.files, t.directories, t.bytes), (6, 4, 2, 18));
        assert_eq!((t.picked, t.picked_bytes), (2, 12));
        let page = m.rows(&v, 1, 2);
        assert_eq!(page[0].path, "a-b.txt");
        assert_eq!(page.len(), 2);
        let cancelled = CancelToken::new();
        cancelled.cancel();
        assert_eq!(
            m.view(Scope::All, by_size, &Filter::None, &cancelled)
                .unwrap_err()
                .class,
            Class::Cancelled
        );
    }

    #[test]
    fn an_unsorted_foreign_listing_finds_its_parents() {
        use EntryKind::*;
        let m = model(&[
            entry("d/f", File, 1),
            entry("d", Directory, 0),
            entry("e", File, 2),
        ]);
        assert_eq!(m.children(ROOT), &[1, 2]);
        assert_eq!(m.children(1), &[0]);
    }
}
