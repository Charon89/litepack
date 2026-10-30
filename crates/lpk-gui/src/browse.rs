//! The archive browser's state in Rust (E5-3; `docs/notes/e5-design.md` section 3): the view
//! (flat or one folder of the tree), its order and filter, the selection and its anchor. The
//! page holds only the rows it shows: it asks for `visible + 2·`[`MARGIN_ROWS`] rows at a time
//! as a [`batch`] v1, and every sort, filter or folder change builds a new view (a new
//! generation) here, beside the one being served (E5-1's bound counts two views).

use std::sync::Arc;

use lpk_api::{CancelToken, Filter, IdSet, Key, ListModel, Order, RowRef, Scope, View, ROOT};
use serde::{Deserialize, Serialize};

use crate::{batch, GuiError};

/// How a refusal of a stale request begins (the page ignores those: the next view re-syncs it).
pub const STALE_VIEW: &str = "stale view";

/// The rows the page keeps above and below the visible ones (its DOM holds at most
/// `visible + 2·MARGIN_ROWS` rows; E5-11's harness counts them).
pub const MARGIN_ROWS: usize = 64;

/// The tallest spacer the page sets, in CSS pixels; beyond it, scrolling is scaled (E4's
/// 10-million cap at 24 px a row would pass what the webview lays out).
pub const MAX_SCROLL_PX: u32 = 8_000_000;

/// What the page asks for with `view`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewRequest {
    /// `flat` (every entry, by path) or `tree` (one folder's children).
    pub mode: String,
    /// The folder of the tree view (an entry id); `None` is the archive's root.
    pub dir: Option<u32>,
    /// The sort key: `path`, `name`, `size`, `modified` or `kind`.
    pub key: String,
    /// Largest first.
    pub descending: bool,
    /// The filter: a substring (ASCII case-insensitive), or D-90 globs when `glob`.
    pub filter: String,
    /// The filter is a list of globs (comma-separated).
    pub glob: bool,
}

/// A step in the tree view's breadcrumb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Crumb {
    /// The folder's entry id (`None`: the archive's root).
    pub id: Option<u32>,
    /// Its name.
    pub name: String,
}

/// The status bar's figures: the view and the selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Rows in the view.
    pub entries: u64,
    /// Files among them.
    pub files: u64,
    /// Folders among them.
    pub folders: u64,
    /// Their bytes.
    pub bytes: u64,
    /// Selected rows of the view.
    pub picked: u64,
    /// Their bytes.
    pub picked_bytes: u64,
}

/// What `view` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewInfo {
    /// The view's generation, as batches carry it (a page drops a batch of another).
    pub generation: u32,
    /// Rows in the view.
    pub total: u64,
    /// `flat` or `tree`.
    pub mode: &'static str,
    /// The tree view's folder (`None`: the root, or the flat view).
    pub dir: Option<u32>,
    /// The breadcrumb, root first (the tree view; empty in the flat view).
    pub crumbs: Vec<Crumb>,
    /// The status bar.
    pub status: Status,
}

/// A selection request (`select`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectAction {
    /// The row alone (a click, or the arrow keys).
    Only,
    /// Toggle the row (Ctrl+click, Ctrl+Space).
    Toggle,
    /// The rows from the anchor to this one, alone (Shift+click, Shift+arrows).
    Range,
    /// Add the rows from the anchor to this one (Ctrl+Shift+click).
    Extend,
    /// Every row of the view (Ctrl+A).
    All,
    /// None.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Tree,
}

/// One session's browser.
#[derive(Debug)]
pub struct Browser {
    model: Arc<ListModel>,
    mode: Mode,
    dir: u32,
    order: Order,
    view: Arc<View>,
    picks: IdSet,
    anchor: Option<u32>,
}

fn parse_key(key: &str) -> Result<Key, GuiError> {
    Ok(match key {
        "" | "path" => Key::Path,
        "name" => Key::Name,
        "size" => Key::Size,
        "modified" => Key::Modified,
        "kind" => Key::Kind,
        other => return Err(GuiError::usage(format!("sort key {other:?}"))),
    })
}

fn parse_filter(text: &str, glob: bool) -> Result<Filter, GuiError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Filter::None);
    }
    if !glob {
        return Ok(Filter::Contains(text.to_string()));
    }
    let patterns: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    let g = lpk_api::Globs::new(&patterns)
        .map_err(|e| GuiError::from(lpk_api::Error::core(e, None)))?;
    Ok(Filter::Globs(g))
}

impl Browser {
    /// The flat view of `model` in the archive's order.
    pub fn new(model: Arc<ListModel>, c: &CancelToken) -> Result<Self, GuiError> {
        let view = Arc::new(model.view(Scope::All, Order::default(), &Filter::None, c)?);
        let picks = model.new_selection();
        Ok(Browser {
            model,
            mode: Mode::Flat,
            dir: ROOT,
            order: Order::default(),
            view,
            picks,
            anchor: None,
        })
    }

    /// The model.
    pub fn model(&self) -> &Arc<ListModel> {
        &self.model
    }

    /// The view being served.
    pub fn view(&self) -> &Arc<View> {
        &self.view
    }

    /// The selection.
    pub fn picks(&self) -> &IdSet {
        &self.picks
    }

    /// The picks inside the current view (its scope and filter): what the status bar counts
    /// and what an extraction takes (E5-4), and how many picks the view hides.
    pub fn picked_in_view(&self) -> (IdSet, u64) {
        let mut inside = self.model.new_selection();
        for &id in self.view.ids() {
            if self.picks.contains(id) {
                inside.insert(id);
            }
        }
        let hidden = self.picks.count().saturating_sub(inside.count());
        (inside, hidden)
    }

    /// Build the view `r` asks for; the selection is kept across a sort or a filter and
    /// cleared when the scope changes (another folder, or the other mode).
    pub fn set_view(&mut self, r: &ViewRequest, c: &CancelToken) -> Result<ViewInfo, GuiError> {
        let mode = match r.mode.as_str() {
            "" | "flat" => Mode::Flat,
            "tree" => Mode::Tree,
            other => return Err(GuiError::usage(format!("view mode {other:?}"))),
        };
        let dir = match (mode, r.dir) {
            (Mode::Flat, _) | (Mode::Tree, None) => ROOT,
            (Mode::Tree, Some(d)) => match self.model.row(d) {
                Some(row) if row.kind == 1 => d,
                _ => return Err(GuiError::usage(format!("entry {d} is not a folder"))),
            },
        };
        let order = Order {
            key: parse_key(&r.key)?,
            descending: r.descending,
        };
        let filter = parse_filter(&r.filter, r.glob)?;
        let scope = match mode {
            Mode::Flat => Scope::All,
            Mode::Tree => Scope::Children(dir),
        };
        // Built beside the view being served, then swapped in.
        let view = Arc::new(self.model.view(scope, order, &filter, c)?);
        if mode != self.mode || dir != self.dir {
            self.picks = self.model.new_selection();
            self.anchor = None;
        }
        self.mode = mode;
        self.dir = dir;
        self.order = order;
        self.view = view;
        Ok(self.info())
    }

    /// The current view's description.
    pub fn info(&self) -> ViewInfo {
        ViewInfo {
            generation: self.view.generation as u32,
            total: self.view.len() as u64,
            mode: match self.mode {
                Mode::Flat => "flat",
                Mode::Tree => "tree",
            },
            dir: (self.mode == Mode::Tree && self.dir != ROOT).then_some(self.dir),
            crumbs: self.crumbs(),
            status: self.status(),
        }
    }

    fn crumbs(&self) -> Vec<Crumb> {
        if self.mode == Mode::Flat {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut at = self.dir;
        // A parent chain is finite (parents precede children); the bound guards a bad model.
        while at != ROOT && out.len() <= self.model.len() {
            out.push(Crumb {
                id: Some(at),
                name: self.model.name(at).to_string(),
            });
            at = self.model.row(at).map_or(ROOT, |r| r.parent);
        }
        out.push(Crumb {
            id: None,
            name: String::new(),
        });
        out.reverse();
        out
    }

    /// The status bar's figures.
    pub fn status(&self) -> Status {
        let t = self.model.totals(&self.view, Some(&self.picks));
        Status {
            entries: t.entries,
            files: t.files,
            folders: t.directories,
            bytes: t.bytes,
            picked: t.picked,
            picked_bytes: self.picked_bytes(),
        }
    }

    /// The bytes an extraction of the picks inside the view writes: a picked folder counts its
    /// whole subtree, and each file counts once even when it and its folder are both picked
    /// (the dialog's figures come from the engine's preflight of the same set and agree).
    fn picked_bytes(&self) -> u64 {
        let mut seen = self.model.new_selection();
        let mut total = 0u64;
        let mut stack: Vec<u32> = self
            .view
            .ids()
            .iter()
            .copied()
            .filter(|&id| self.picks.contains(id))
            .collect();
        while let Some(id) = stack.pop() {
            if seen.contains(id) {
                continue;
            }
            seen.insert(id);
            match self.model.row(id).map(|r| (r.kind, r.size)) {
                Some((0, size)) => total += size,
                Some((1, _)) => stack.extend_from_slice(self.model.children(id)),
                _ => {}
            }
        }
        total
    }

    /// A batch v1 of at most `count` rows (and [`batch::MAX_ROWS`]) from view row `first`.
    pub fn rows(&self, first: u32, count: u32) -> Vec<u8> {
        let ids = self.view.ids();
        let first = (first as usize).min(ids.len());
        let n = (count as usize).min(batch::MAX_ROWS);
        let rows = ids[first..].iter().take(n).filter_map(|&id| {
            self.model.row(id).map(|row| RowRef {
                id,
                row: *row,
                path: self.model.path(id),
                name: self.model.name(id),
            })
        });
        batch::encode(
            self.view.generation,
            ids.len(),
            first,
            rows,
            self.mode == Mode::Flat,
            |id| self.picks.contains(id),
        )
    }

    /// The entry id at view row `row`.
    pub fn id_at(&self, row: u32) -> Result<u32, GuiError> {
        self.view
            .ids()
            .get(row as usize)
            .copied()
            .ok_or_else(|| GuiError::usage(format!("no row {row}")))
    }

    fn row_of(&self, id: u32) -> Option<usize> {
        self.view.ids().iter().position(|&x| x == id)
    }

    /// Refuse a request made against another view than the one served: a row index means
    /// nothing in a view built since (a sort or filter the page has not seen yet).
    pub fn check_generation(&self, generation: u32) -> Result<(), GuiError> {
        let current = self.view.generation as u32;
        if generation == current {
            Ok(())
        } else {
            Err(GuiError::usage(format!(
                "{STALE_VIEW}: generation {generation}, the view is {current}"
            )))
        }
    }

    /// Change the selection of view `generation` (refused when it is not the view served,
    /// before anything changes); the status bar's figures after it.
    pub fn select(
        &mut self,
        action: SelectAction,
        row: Option<u32>,
        generation: u32,
    ) -> Result<Status, GuiError> {
        self.check_generation(generation)?;
        let need = |row: Option<u32>| row.ok_or_else(|| GuiError::usage("no row given"));
        match action {
            SelectAction::Only => {
                let id = self.id_at(need(row)?)?;
                self.picks = self.model.new_selection();
                self.picks.insert(id);
                self.anchor = Some(id);
            }
            SelectAction::Toggle => {
                let id = self.id_at(need(row)?)?;
                if self.picks.contains(id) {
                    self.picks.remove(id);
                } else {
                    self.picks.insert(id);
                }
                self.anchor = Some(id);
            }
            SelectAction::Range | SelectAction::Extend => {
                let to = need(row)? as usize;
                let id = self.id_at(to as u32)?;
                let from = self.anchor.and_then(|a| self.row_of(a)).unwrap_or(to);
                if action == SelectAction::Range {
                    self.picks = self.model.new_selection();
                }
                let (lo, hi) = (from.min(to), from.max(to));
                for &x in &self.view.ids()[lo..=hi] {
                    self.picks.insert(x);
                }
                if self.anchor.is_none() {
                    self.anchor = Some(id);
                }
            }
            SelectAction::All => {
                for &x in self.view.ids() {
                    self.picks.insert(x);
                }
            }
            SelectAction::None => {
                self.picks = self.model.new_selection();
                self.anchor = None;
            }
        }
        Ok(self.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lpk_api::{CreateSettings, Monitor, Session, Sources, Tier};
    use std::path::Path;

    fn browser(dir: &Path) -> Browser {
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("docs/deep")).unwrap();
        std::fs::write(src.join("b.txt"), b"bb").unwrap();
        std::fs::write(src.join("a.bin"), b"aaaa").unwrap();
        std::fs::write(src.join("docs/c.md"), b"c").unwrap();
        std::fs::write(src.join("docs/deep/d.txt"), b"ddd").unwrap();
        let out = dir.join("t.lpk");
        let s = CreateSettings {
            tier: Tier::Store,
            ..CreateSettings::default()
        };
        lpk_api::create(&Sources::contents_of(&src), &out, &s, &Monitor::default()).unwrap();
        let session = Session::open(&out, &Default::default()).unwrap();
        let c = CancelToken::new();
        Browser::new(session.model(&c).unwrap(), &c).unwrap()
    }

    fn names(b: &Browser) -> Vec<String> {
        let (_, _, rows) = batch::decode(&b.rows(0, 100)).unwrap();
        rows.into_iter().map(|r| r.name).collect()
    }

    fn req(mode: &str, dir: Option<u32>, key: &str, desc: bool, filter: &str) -> ViewRequest {
        ViewRequest {
            mode: mode.into(),
            dir,
            key: key.into(),
            descending: desc,
            filter: filter.into(),
            glob: false,
        }
    }

    #[test]
    fn flat_and_tree_views_sort_and_filter_in_rust_with_new_generations() {
        let d = tempfile::tempdir().unwrap();
        let mut b = browser(d.path());
        let c = CancelToken::new();
        let flat = b.info();
        assert_eq!(flat.mode, "flat");
        assert_eq!(flat.total, 6);
        assert!(names(&b).contains(&"docs/deep/d.txt".to_string()));

        let by_size = b
            .set_view(&req("flat", None, "size", true, ""), &c)
            .unwrap();
        assert!(by_size.generation > flat.generation);
        assert_eq!(names(&b)[0], "a.bin");

        let filtered = b
            .set_view(&req("flat", None, "path", false, "TXT"), &c)
            .unwrap();
        assert_eq!(filtered.total, 2);
        assert_eq!(names(&b), ["b.txt", "docs/deep/d.txt"]);

        let mut glob = req("flat", None, "name", false, "**/*.md, *.bin");
        glob.glob = true;
        assert_eq!(b.set_view(&glob, &c).unwrap().total, 2);
        assert_eq!(names(&b), ["a.bin", "docs/c.md"]);

        // The tree: the root's children by kind (folders first), names only.
        let root = b
            .set_view(&req("tree", None, "kind", false, ""), &c)
            .unwrap();
        assert_eq!(root.crumbs.len(), 1);
        assert_eq!(names(&b), ["docs", "a.bin", "b.txt"]);
        let docs = b.id_at(0).unwrap();
        let inside = b
            .set_view(&req("tree", Some(docs), "kind", false, ""), &c)
            .unwrap();
        assert_eq!(names(&b), ["deep", "c.md"]);
        assert_eq!(inside.dir, Some(docs));
        assert_eq!(
            inside
                .crumbs
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["", "docs"]
        );
        // A file is not a folder; an unknown key or mode is refused.
        let file = b.id_at(1).unwrap();
        for bad in [
            req("tree", Some(file), "", false, ""),
            req("list", None, "", false, ""),
            req("flat", None, "colour", false, ""),
        ] {
            assert!(b.set_view(&bad, &c).unwrap_err().is(lpk_api::Class::Usage));
        }
    }

    #[test]
    fn selection_ranges_toggles_and_marks_travel_in_the_batch() {
        let d = tempfile::tempdir().unwrap();
        let mut b = browser(d.path());
        let s = b
            .select(SelectAction::Only, Some(1), b.info().generation)
            .unwrap();
        assert_eq!(s.picked, 1);
        let s = b
            .select(SelectAction::Range, Some(3), b.info().generation)
            .unwrap();
        assert_eq!(s.picked, 3);
        let s = b
            .select(SelectAction::Toggle, Some(2), b.info().generation)
            .unwrap();
        assert_eq!(s.picked, 2);
        let (_, _, rows) = batch::decode(&b.rows(0, 10)).unwrap();
        let marked: Vec<bool> = rows
            .iter()
            .map(|r| r.marks & batch::MARK_PICKED != 0)
            .collect();
        assert_eq!(marked, [false, true, false, true, false, false]);
        // The anchor moved to row 2: a range from it.
        let s = b
            .select(SelectAction::Extend, Some(5), b.info().generation)
            .unwrap();
        assert_eq!(s.picked, 5);
        assert_eq!(
            b.select(SelectAction::All, None, b.info().generation)
                .unwrap()
                .picked,
            6
        );
        let s = b
            .select(SelectAction::None, None, b.info().generation)
            .unwrap();
        assert_eq!((s.picked, s.entries), (0, 6));
        assert!(b
            .select(SelectAction::Only, Some(6), b.info().generation)
            .is_err());
        assert!(b
            .select(SelectAction::Only, None, b.info().generation)
            .is_err());
        // A sort keeps the selection; another scope clears it.
        let c = CancelToken::new();
        b.select(SelectAction::All, None, b.info().generation)
            .unwrap();
        let v = b
            .set_view(&req("flat", None, "size", false, ""), &c)
            .unwrap();
        assert_eq!(v.status.picked, 6);
        let v = b.set_view(&req("tree", None, "", false, ""), &c).unwrap();
        assert_eq!(v.status.picked, 0);
    }

    #[test]
    fn a_request_against_an_older_view_is_refused_and_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut b = browser(d.path());
        let c = CancelToken::new();
        let old = b.info().generation;
        b.select(SelectAction::Only, Some(0), old).unwrap();
        let new = b
            .set_view(&req("flat", None, "size", true, ""), &c)
            .unwrap();
        assert_ne!(old, new.generation);
        for action in [
            SelectAction::Only,
            SelectAction::Range,
            SelectAction::Extend,
            SelectAction::Toggle,
        ] {
            let e = b.select(action, Some(3), old).unwrap_err();
            assert!(
                e.is(lpk_api::Class::Usage) && e.detail.starts_with(STALE_VIEW),
                "{e}"
            );
        }
        assert!(b.select(SelectAction::All, None, old).is_err());
        assert_eq!(b.status().picked, 1, "the selection is untouched");
        assert_eq!(
            b.select(SelectAction::Only, Some(3), new.generation)
                .unwrap()
                .picked,
            1
        );
    }

    #[test]
    fn a_batch_asked_past_the_end_is_empty_and_capped_in_rows() {
        let d = tempfile::tempdir().unwrap();
        let b = browser(d.path());
        let (total, first, rows) = batch::decode(&b.rows(100, 10)).unwrap();
        assert_eq!((total, first, rows.len()), (6, 6, 0));
        let (_, first, rows) = batch::decode(&b.rows(4, u32::MAX)).unwrap();
        assert_eq!((first, rows.len()), (4, 2));
    }
}
