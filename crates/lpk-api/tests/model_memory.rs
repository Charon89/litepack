//! E5-1's memory clause: the listing model of E3-1's generated million-entry archive, built,
//! paged and re-sorted (a second view built beside the first), stays within the bound the
//! design states over the reader's open:
//! `N·(size_of::<Row>() + 4·size_of::<u32>()) + P + ⌈N/8⌉ + 1 MiB` ([`ListModel::bound`]).
//! `for_each` (what `lpk l` runs) adds only a constant to the open.
//!
//! Its own test binary, so no other test allocates meanwhile.
#![allow(clippy::unwrap_used)]

use std::ops::ControlFlow;

use lpk_api::{CancelToken, Filter, Key, ListModel, Order, Scope, Selection, Session};
use lpk_format::{EntryFlags, Writer, WriterOptions};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

const ENTRIES: usize = 1_000_000;

/// E3-1's archive: files without content, one directory per thousand.
fn million_entry_archive(path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut w = Writer::new(std::io::BufWriter::new(file), WriterOptions::default()).unwrap();
    for i in 0..ENTRIES {
        if i % 1000 == 0 {
            w.add_directory(&format!("d{:04}", i / 1000), EntryFlags::EMPTY, 0)
                .unwrap();
        } else {
            w.add_file(
                &format!("d{:04}/f{:06}.txt", i / 1000, i),
                EntryFlags::READ_ONLY,
                1_700_000_000_000_000_000 + i as i64,
                &mut &b""[..],
            )
            .unwrap();
        }
    }
    w.finish().unwrap();
}

/// The peak heap growth of `f` over its start, in bytes.
fn peak_growth<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = ALLOC.current();
    ALLOC.reset_peak();
    let t = f();
    (ALLOC.peak().saturating_sub(before), t)
}

#[test]
fn the_model_pages_and_resorts_a_million_entries_within_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("million.lpk");
    million_entry_archive(&path);
    let c = CancelToken::new();

    // The reader's open: the session and one pass over the entry table (what every listing
    // needs), nothing kept.
    let (open_peak, session) = peak_growth(|| {
        let s = Session::open(&path, &Default::default()).unwrap();
        let mut n = 0usize;
        s.for_each(&Selection::All, &mut |_| {
            n += 1;
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(n, ENTRIES);
        s
    });
    eprintln!("open + entry table: peak growth {open_peak} bytes");

    let (model_peak, (n, p)) = peak_growth(|| {
        let m = session.model(&c).unwrap();
        let by_size = Order {
            key: Key::Size,
            descending: true,
        };
        let v1 = m.view(Scope::All, by_size, &Filter::None, &c).unwrap();
        assert_eq!(v1.len(), ENTRIES);
        let mut seen = 0;
        for first in (0..ENTRIES).step_by(ENTRIES / 16) {
            seen += m.rows(&v1, first, 4096).len();
        }
        assert!(seen > 0);
        // A re-sort builds its view beside the one served.
        let v2 = m
            .view(
                Scope::All,
                Order {
                    key: Key::Name,
                    descending: false,
                },
                &Filter::None,
                &c,
            )
            .unwrap();
        let mut picks = m.new_selection();
        for id in v2.ids().iter().take(1000) {
            picks.insert(*id);
        }
        let t = m.totals(&v2, Some(&picks));
        assert_eq!(t.picked, 1000);
        assert_eq!(m.children(lpk_api::ROOT).len(), ENTRIES / 1000);
        (m.len(), m.name_bytes())
    });
    let bound = ListModel::bound(n, p);
    eprintln!(
        "model: {n} entries, {p} name bytes; peak growth {model_peak} bytes, bound {bound} bytes ({} rows of {} bytes)",
        n,
        std::mem::size_of::<lpk_api::Row>()
    );
    assert!(
        model_peak <= open_peak + bound,
        "model peak {model_peak} exceeds the open's {open_peak} + the bound {bound}"
    );
}
