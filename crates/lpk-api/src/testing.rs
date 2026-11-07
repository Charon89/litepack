//! Test helpers for front ends (feature `test-support`; dev-dependencies only): E3-1's generated
//! million-entry archive, written through the format writer, and a counting global allocator, so
//! that the GUI's command layer (E5-3) can state its memory bound without naming an engine crate
//! other than this one (the seam, `tests/gui_seam.rs`).

use std::path::Path;

use lpk_format::{EntryFlags, Writer, WriterOptions};

pub use lpk_procstat_sys::alloc::CountingAlloc;

/// E3-1's archive at `path`: `entries` entries, one directory per thousand (`d0000`, …), the
/// others empty files `d0000/f000001.txt`, … in table order (`crates/lpk-cli/tests/list_memory.rs`).
///
/// # Errors
/// The writer's.
pub fn million_entry_archive(path: &Path, entries: usize) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let io = |e: lpk_format::FormatError| std::io::Error::other(e.to_string());
    let mut w = Writer::new(std::io::BufWriter::new(file), WriterOptions::default()).map_err(io)?;
    for i in 0..entries {
        if i % 1000 == 0 {
            w.add_directory(&format!("d{:04}", i / 1000), EntryFlags::EMPTY, 0)
                .map_err(io)?;
        } else {
            w.add_file(
                &format!("d{:04}/f{:06}.txt", i / 1000, i),
                EntryFlags::READ_ONLY,
                1_700_000_000_000_000_000 + i as i64,
                &mut &b""[..],
            )
            .map_err(io)?;
        }
    }
    w.finish().map_err(io)?;
    Ok(())
}

/// An archive at `path` of `entries` (path, contents; `None` is a directory), in that order,
/// written through the format writer: names a file system cannot hold (a trailing space, a
/// second extension after a space) can be tested on every OS.
///
/// # Errors
/// The writer's.
pub fn archive_with(path: &Path, entries: &[(&str, Option<&[u8]>)]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let io = |e: lpk_format::FormatError| std::io::Error::other(e.to_string());
    let mut w = Writer::new(std::io::BufWriter::new(file), WriterOptions::default()).map_err(io)?;
    for (name, body) in entries {
        match body {
            None => w.add_directory(name, EntryFlags::EMPTY, 0).map_err(io)?,
            Some(b) => w
                .add_file(
                    name,
                    EntryFlags::EMPTY,
                    1_700_000_000_000_000_000,
                    &mut &b[..],
                )
                .map_err(io)?,
        }
    }
    w.finish().map_err(io)?;
    Ok(())
}

/// E3-1's open of the archive at `path`: `Archive::open` and the entry table read, nothing
/// listed (the reference of E5-1's bound; measure its peak with [`CountingAlloc`]). The
/// entries counted.
///
/// # Errors
/// The reader's.
pub fn open_entry_table(path: &Path) -> std::io::Result<u64> {
    let io = |e: lpk_format::FormatError| std::io::Error::other(e.to_string());
    let mut a = lpk_format::Archive::open(
        std::fs::File::open(path)?,
        &lpk_format::Resources::default(),
    )
    .map_err(io)?;
    Ok(a.entry_table().map_err(io)?.len())
}
