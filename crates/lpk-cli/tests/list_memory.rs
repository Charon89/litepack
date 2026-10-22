//! E3-1's memory clause: `lpk l` adds no per-entry memory of its own. An archive of
//! 1,000,000 entries is written in memory through the format writer (no file on disk) and
//! listed through the CLI's listing code ([`lpk_cli::list_archive`], what `lpk l` calls) under
//! a counting global allocator: the listing's peak heap use is bounded by the reader's open
//! (`Archive::open` plus the entry table it reads, which every listing needs) plus a stated
//! constant, [`LISTING_CONSTANT`] (the write buffer and the transient strings of one line).
//! For contrast, the reference tool's `list` collects the entries first; that peak is printed.
//!
//! Its own test binary, so no other test allocates meanwhile.
#![allow(clippy::unwrap_used)]

use std::io::{Cursor, Write};

use lpk_cli::{list_archive, ListOptions};
use lpk_format::{Archive, Entry, EntryFlags, Resources, Writer, WriterOptions};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// What a listing may add to the reader's open: a `BufWriter` (8 KiB) and the strings of
/// one line at a time (a path, an escaped path, a JSON object), none kept. One MiB covers
/// the allocator's rounding and a long path many times over.
const LISTING_CONSTANT: usize = 1 << 20;

const ENTRIES: usize = 1_000_000;

/// One million entries: files without content (the entry table is what a listing reads;
/// chunks would only add to the archive's index, which the open reads whatever is listed),
/// one directory per thousand, written in memory with the store encoder.
fn million_entry_archive() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, WriterOptions::default()).unwrap();
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
    out
}

/// A sink that counts lines (and bytes) without keeping them.
struct LineCount {
    lines: usize,
    bytes: usize,
}

impl Write for LineCount {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.lines += buf.iter().filter(|&&b| b == b'\n').count();
        self.bytes += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The peak heap growth of `f` over its start, in bytes.
fn peak_growth<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = ALLOC.current();
    ALLOC.reset_peak();
    let t = f();
    (ALLOC.peak().saturating_sub(before), t)
}

/// About 20 s in a debug build on the measuring machine (under the 30 s that would make it
/// an ignored test); a release build is noticeably faster.
#[test]
fn listing_a_million_entries_adds_a_constant_to_the_readers_open() {
    let archive = million_entry_archive();
    eprintln!("archive: {} bytes, {ENTRIES} entries", archive.len());
    let resources = Resources::default();

    // The reader's open: the archive opened and its entry table read, nothing listed.
    let (open_peak, count) = peak_growth(|| {
        let mut a = Archive::open(Cursor::new(&archive[..]), &resources).unwrap();
        a.entry_table().unwrap().len()
    });
    assert_eq!(count, ENTRIES as u64);
    eprintln!("reader's open (archive + entry table): peak growth {open_peak} bytes");

    // The reference tool's list collects the entries before printing them.
    let (collected_peak, n) = peak_growth(|| {
        let mut a = Archive::open(Cursor::new(&archive[..]), &resources).unwrap();
        let t = a.entry_table().unwrap();
        let all: Vec<Entry> = t.table().unwrap().iter().collect::<Result<_, _>>().unwrap();
        all.len()
    });
    assert_eq!(n, ENTRIES);
    eprintln!(
        "entries collected as the reference tool's list does: peak growth {collected_peak} bytes"
    );

    for (name, options) in [
        ("default", ListOptions::default()),
        (
            "--long",
            ListOptions {
                long: true,
                json: false,
            },
        ),
        (
            "--json",
            ListOptions {
                long: false,
                json: true,
            },
        ),
    ] {
        let (list_peak, sink) = peak_growth(|| {
            let mut a = Archive::open(Cursor::new(&archive[..]), &resources).unwrap();
            let mut sink = LineCount { lines: 0, bytes: 0 };
            list_archive(&mut a, options, &lpk_core::Selection::all(), &mut sink).unwrap();
            sink
        });
        let expected_lines = ENTRIES + usize::from(options.long && !options.json);
        assert_eq!(sink.lines, expected_lines, "{name}");
        eprintln!(
            "lpk l {name}: {} lines, {} bytes; peak growth {list_peak} bytes ({} over the open)",
            sink.lines,
            sink.bytes,
            list_peak as i64 - open_peak as i64
        );
        assert!(
            list_peak <= open_peak + LISTING_CONSTANT,
            "{name}: listing peak {list_peak} exceeds the open's {open_peak} + {LISTING_CONSTANT}"
        );
        // And well under what collecting the entries costs.
        assert!(
            list_peak < collected_peak,
            "{name}: {list_peak} vs {collected_peak}"
        );
    }
}
