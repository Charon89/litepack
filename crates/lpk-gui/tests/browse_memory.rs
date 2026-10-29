//! E5-3's backend bound (`docs/notes/e5-design.md` sections 3 and 7): the command layer scrolls
//! E3-1's generated million-entry archive from top to bottom through `rows`, a window of
//! `visible + 2·MARGIN_ROWS` rows at a time, and its peak heap growth stays within E5-1's bound
//! (E3-1's open plus [`ListModel::bound`]) plus one batch ([`batch::MAX_BATCH_BYTES`]), measured
//! with the counting allocator. A re-sort, select-all and a filter join the scroll, so the second
//! view and the selection are inside the measurement too.
//!
//! Its own test binary, so no other test allocates meanwhile.
#![allow(clippy::unwrap_used)]

use lpk_api::testing::{million_entry_archive, open_entry_table, CountingAlloc};
use lpk_api::{CancelToken, ListModel};
use lpk_gui::app::App;
use lpk_gui::batch;
use lpk_gui::browse::{SelectAction, ViewRequest, MARGIN_ROWS};
use lpk_gui::grants::GrantKind;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

const ENTRIES: usize = 1_000_000;
/// The rows a tall window shows.
const VISIBLE: u32 = 60;

/// The peak heap growth of `f` over its start, in bytes.
fn peak_growth<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = ALLOC.current();
    ALLOC.reset_peak();
    let t = f();
    (ALLOC.peak().saturating_sub(before), t)
}

/// Scroll the whole view as the page does: the window starts `MARGIN_ROWS` above the first
/// visible row and holds `visible + 2·MARGIN_ROWS` rows; the visible rows move by a screen. The
/// last row seen.
fn scroll(app: &App, session: u64, total: u32) -> u32 {
    let window = VISIBLE + 2 * MARGIN_ROWS as u32;
    let mut top = 0u32;
    let mut last = 0u32;
    while top < total {
        let first = top.saturating_sub(MARGIN_ROWS as u32);
        let b = app.rows(session, first, window).unwrap();
        let (_, t, f, count) = batch::header(&b).unwrap();
        assert_eq!((t, f), (total, first));
        assert!(count > 0 && count <= window, "{count} rows");
        last = first + count - 1;
        top += VISIBLE;
    }
    last
}

#[test]
fn scrolling_a_million_entries_stays_within_the_model_bound_plus_one_batch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("million.lpk");
    million_entry_archive(&path, ENTRIES).unwrap();

    let (open_ref, count) = peak_growth(|| open_entry_table(&path).unwrap());
    assert_eq!(count, ENTRIES as u64);

    let app = App::new(None);
    let grant = app.grants.mint(GrantKind::Archive, &path).grant;
    let c = CancelToken::new();
    let (peak, (n, p)) = peak_growth(|| {
        let opened = app.open(&grant, &c).unwrap();
        assert_eq!(opened.total, ENTRIES as u64);
        let total = opened.total as u32;
        assert_eq!(scroll(&app, opened.session, total), total - 1, "the bottom");
        // A re-sort (built beside the served view), select-all, the scroll again, a filter.
        let by_name = ViewRequest {
            mode: "flat".into(),
            key: "name".into(),
            descending: true,
            ..ViewRequest::default()
        };
        let v = app.view(opened.session, &by_name, &c).unwrap();
        assert_eq!(v.total, ENTRIES as u64);
        let s = app.select(opened.session, SelectAction::All, None).unwrap();
        assert_eq!(s.picked, ENTRIES as u64);
        assert_eq!(scroll(&app, opened.session, total), total - 1);
        let filtered = ViewRequest {
            filter: "f0999".into(),
            ..by_name
        };
        let v = app.view(opened.session, &filtered, &c).unwrap();
        assert!(v.total > 0 && v.total < ENTRIES as u64);
        let size = app.model_size(opened.session).unwrap();
        app.close(opened.session).unwrap();
        size
    });
    assert_eq!(n, ENTRIES);
    let bound = ListModel::bound(n, p);
    eprintln!(
        "E3-1's open {open_ref} B; model bound {bound} B (N {n}, P {p}); one batch {} B; peak growth {peak} B",
        batch::MAX_BATCH_BYTES
    );
    assert!(
        peak <= open_ref + bound + batch::MAX_BATCH_BYTES,
        "peak {peak} exceeds E3-1's open {open_ref} + the bound {bound} + one batch {}",
        batch::MAX_BATCH_BYTES
    );
}
