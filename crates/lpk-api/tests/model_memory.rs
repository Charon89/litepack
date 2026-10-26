//! E5-1's memory clause, on an archive shaped as E3-1's (1,000,000 entries, one directory per
//! thousand) with names and sizes shuffled (a pseudo-random permutation names the files, the
//! sizes are pseudo-random, so neither the name nor the size order is the table's order).
//!
//! The reference is E3-1's open, measured as E3-1 measures it: `Archive::open` plus the entry
//! table read (`open_ref`). Asserted:
//! - `for_each` (what `lpk l` runs) over an open session grows the heap by at most
//!   `open_ref` + 1 MiB (E3-1's listing bound: the fork reads the table, nothing is kept);
//! - building the model, paging it, building a filtered view and re-sorting (a second view
//!   beside the first) grow the heap by at most `open_ref` +
//!   `N·(size_of::<Row>() + 4·size_of::<u32>()) + P + ⌈N/8⌉ + 1 MiB` ([`ListModel::bound`]).
//!
//! Its own test binary, so no other test allocates meanwhile.
#![allow(clippy::unwrap_used)]

use std::ops::ControlFlow;

use lpk_api::{CancelToken, Filter, Key, ListModel, Order, Scope, Selection, Session};
use lpk_format::{Archive, EntryFlags, Resources, Writer, WriterOptions};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

const ENTRIES: usize = 1_000_000;
/// E3-1's listing constant: a write buffer and one line's strings.
const LISTING_CONSTANT: usize = 1 << 20;

/// A 64-bit mix (splitmix64's finaliser): the shuffle and the sizes.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// E3-1's shape with shuffled names and pseudo-random sizes (0 to 15 bytes).
fn shuffled_archive(path: &std::path::Path) {
    let mut paths: Vec<(String, bool, u64)> = Vec::with_capacity(ENTRIES);
    for i in 0..ENTRIES {
        let d = i / 1000;
        if i % 1000 == 0 {
            paths.push((format!("d{d:04}"), true, 0));
        } else {
            let h = mix(i as u64);
            paths.push((format!("d{d:04}/f{:016x}.txt", h), false, h % 16));
        }
    }
    paths.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let file = std::fs::File::create(path).unwrap();
    let mut w = Writer::new(std::io::BufWriter::new(file), WriterOptions::default()).unwrap();
    let content = [b'x'; 16];
    for (i, (p, dir, size)) in paths.iter().enumerate() {
        if *dir {
            w.add_directory(p, EntryFlags::EMPTY, 0).unwrap();
        } else {
            w.add_file(
                p,
                EntryFlags::READ_ONLY,
                1_700_000_000_000_000_000 + mix(i as u64 ^ 7) as i64 % 1_000_000_000_000,
                &mut &content[..*size as usize],
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
fn the_model_pages_filters_and_resorts_a_million_entries_within_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("million.lpk");
    shuffled_archive(&path);
    let c = CancelToken::new();

    // E3-1's open: the archive opened and its entry table read, nothing listed.
    let (open_ref, count) = peak_growth(|| {
        let mut a =
            Archive::open(std::fs::File::open(&path).unwrap(), &Resources::default()).unwrap();
        a.entry_table().unwrap().len()
    });
    assert_eq!(count, ENTRIES as u64);
    eprintln!("E3-1's open (archive + entry table): peak growth {open_ref} bytes");

    let session = Session::open(&path, &Default::default()).unwrap();

    // `for_each` over the open session: within E3-1's listing bound.
    let (walk_peak, n) = peak_growth(|| {
        let mut n = 0usize;
        session
            .for_each(&Selection::All, &mut |_| {
                n += 1;
                ControlFlow::Continue(())
            })
            .unwrap();
        n
    });
    assert_eq!(n, ENTRIES);
    eprintln!("for_each: peak growth {walk_peak} bytes");
    assert!(
        walk_peak <= open_ref + LISTING_CONSTANT,
        "for_each peak {walk_peak} exceeds E3-1's open {open_ref} + {LISTING_CONSTANT}"
    );

    let (model_peak, (n, p)) = peak_growth(|| {
        let m = session.model(&c).unwrap();
        let by_size = Order {
            key: Key::Size,
            descending: true,
        };
        let v1 = m.view(Scope::All, by_size, &Filter::None, &c).unwrap();
        assert_eq!(v1.len(), ENTRIES);
        let sizes: Vec<u64> = m.rows(&v1, 0, 4096).iter().map(|r| r.row.size).collect();
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]));
        let mut seen = 0;
        for first in (0..ENTRIES).step_by(ENTRIES / 16) {
            seen += m.rows(&v1, first, 4096).len();
        }
        assert!(seen > 0);
        // A filtered view, and a re-sort built beside the view being served.
        let filtered = m
            .view(Scope::All, by_size, &Filter::Contains("7a".into()), &c)
            .unwrap();
        assert!(!filtered.is_empty() && filtered.len() < ENTRIES);
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
        let names: Vec<&str> = m.rows(&v2, 0, 1000).iter().map(|r| r.name).collect();
        assert!(names.windows(2).all(|w| w[0] <= w[1]));
        let mut picks = m.new_selection();
        for id in v2.ids().iter().take(1000) {
            picks.insert(*id);
        }
        let t = m.totals(&v2, Some(&picks));
        assert_eq!(t.picked, 1000);
        assert_eq!(m.children(lpk_api::ROOT).len(), ENTRIES / 1000);
        drop(filtered);
        (m.len(), m.name_bytes())
    });
    let bound = ListModel::bound(n, p);
    eprintln!(
        "model: {n} entries, {p} name bytes; peak growth {model_peak} bytes, E3-1's open {open_ref} + bound {bound} bytes (rows of {} bytes)",
        std::mem::size_of::<lpk_api::Row>()
    );
    assert!(
        model_peak <= open_ref + bound,
        "model peak {model_peak} exceeds E3-1's open {open_ref} + the bound {bound}"
    );
}
