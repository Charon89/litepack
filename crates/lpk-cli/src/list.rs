//! `lpk l`: the entry table streamed, one line per entry, in three shapes.
//!
//! Default: `kind<TAB>size<TAB>mtime<TAB>path` (the reference tool's `list` with the mtime
//! added: UTC, ISO 8601 to the second, `-` when the archive records none). `--long` adds a
//! flags column (`xhrs`: executable, hidden, read-only, system; `-` for each flag not set)
//! before the path, shows a symlink's target after `->`, and starts with one header line
//! `generation<TAB>N` (the format records a generation per archive, not per entry). `--json`
//! prints one object per entry per line with every field the format stores (the keys are
//! documented in the command's help, `lpk l --help`). Control characters in a path are escaped
//! as the reference tool escapes them ([`lpk_format::cli::escape_path`]); the JSON form
//! carries the path as a JSON string instead.
//!
//! Nothing is collected: the entries are read from the table the reader holds and written as
//! they come, so a listing's memory is the reader's open plus a write buffer.

use std::io::{BufWriter, Read, Seek, Write};

use lpk_core::Selection;
use lpk_format::cli::escape_path;
use lpk_format::entry::{Entry, EntryFlags};
use lpk_format::{Archive, FormatError};

/// What `--long` and `--json` select.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListOptions {
    /// The flags column, the symlink target and the generation header line.
    pub long: bool,
    /// One JSON object per entry per line (takes precedence over `long`).
    pub json: bool,
}

/// The names of the flags, in bit order, with the letter of the `--long` column.
const FLAGS: [(EntryFlags, &str, char); 4] = [
    (EntryFlags::EXECUTABLE, "executable", 'x'),
    (EntryFlags::HIDDEN, "hidden", 'h'),
    (EntryFlags::READ_ONLY, "read-only", 'r'),
    (EntryFlags::SYSTEM, "system", 's'),
];

/// The mtime of an entry as the listing prints it: `YYYY-MM-DDThh:mm:ssZ` (UTC, the
/// nanoseconds dropped), or `None` when the archive records none (`i64::MIN`).
pub fn format_mtime(mtime_ns: i64) -> Option<String> {
    if mtime_ns == i64::MIN {
        return None;
    }
    let secs = mtime_ns.div_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (proleptic Gregorian; Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        (sod / 60) % 60,
        sod % 60
    ))
}

/// The flags column of `--long`: `xhrs`, each letter replaced by `-` when its flag is not set.
pub fn flags_column(flags: EntryFlags) -> String {
    FLAGS
        .iter()
        .map(|(f, _, c)| if flags.contains(*f) { *c } else { '-' })
        .collect()
}

/// The names of the set flags, in bit order.
fn flag_names(flags: EntryFlags) -> Vec<&'static str> {
    FLAGS
        .iter()
        .filter(|(f, _, _)| flags.contains(*f))
        .map(|(_, n, _)| *n)
        .collect()
}

/// One entry as a text line (default or `--long`).
fn text_line(w: &mut impl Write, e: &Entry, long: bool) -> std::io::Result<()> {
    let mtime = format_mtime(e.mtime_ns);
    let mtime = mtime.as_deref().unwrap_or("-");
    let path = escape_path(&e.path);
    if long {
        write!(
            w,
            "{}\t{}\t{mtime}\t{}\t{path}",
            e.kind.name(),
            e.size,
            flags_column(e.flags)
        )?;
        if let Some(target) = &e.symlink_target {
            write!(w, " -> {}", escape_path(&String::from_utf8_lossy(target)))?;
        }
        writeln!(w)
    } else {
        writeln!(w, "{}\t{}\t{mtime}\t{path}", e.kind.name(), e.size)
    }
}

/// One entry as a JSON object line (the keys as `lpk l --help` documents them).
fn json_line(w: &mut impl Write, e: &Entry, generation: u64) -> std::io::Result<()> {
    let target = e
        .symlink_target
        .as_deref()
        .map(|t| String::from_utf8_lossy(t).into_owned());
    let mtime_ns = (e.mtime_ns != i64::MIN).then_some(e.mtime_ns);
    let object = serde_json::json!({
        "kind": e.kind.name(),
        "size": e.size,
        "mtime": format_mtime(e.mtime_ns),
        "mtime_ns": mtime_ns,
        "flags": flag_names(e.flags),
        "flag_bits": e.flags.bits(),
        "path": e.path,
        "symlink_target": target,
        "chunks": e.chunks.len(),
        "generation": generation,
    });
    serde_json::to_writer(&mut *w, &object)?;
    writeln!(w)
}

/// What a listing can fail with: the reader, or a selection pattern matching nothing.
#[derive(Debug)]
pub enum ListError {
    /// The archive could not be read or opened.
    Format(FormatError),
    /// A pattern matched no entry ([`lpk_core::CoreError::NoMatch`]).
    Core(lpk_core::CoreError),
}

impl From<FormatError> for ListError {
    fn from(e: FormatError) -> Self {
        ListError::Format(e)
    }
}

impl From<std::io::Error> for ListError {
    fn from(e: std::io::Error) -> Self {
        ListError::Format(FormatError::Io(e))
    }
}

/// List an opened archive to `out`: the entry table is read once (hash-checked by the
/// reader; `PasswordRequired` when the archive is sealed, not listable and was opened
/// without its key) and its entries are written as they are parsed, none held. With a
/// selection the table is passed twice: once to find a pattern that matches nothing (refused
/// before any line is written), then to write the matching entries.
pub fn list_archive<R: Read + Seek>(
    archive: &mut Archive<R>,
    options: ListOptions,
    select: &Selection,
    out: &mut dyn Write,
) -> Result<(), ListError> {
    let table = archive.entry_table()?;
    let generation = archive.generation();
    if !select.is_all() {
        let mut hits = select.hits();
        for entry in table.table()?.iter() {
            hits.record(&entry?.path);
        }
        hits.check().map_err(ListError::Core)?;
    }
    let mut w = BufWriter::new(out);
    if options.long && !options.json {
        writeln!(w, "generation\t{generation}")?;
    }
    for entry in table.table()?.iter() {
        let e = entry?;
        if !select.matches(&e.path) {
            continue;
        }
        if options.json {
            json_line(&mut w, &e, generation)?;
        } else {
            text_line(&mut w, &e, options.long)?;
        }
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use lpk_format::entry::EntryKind;

    #[test]
    fn mtime_is_utc_iso_8601_to_the_second() {
        assert_eq!(format_mtime(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(
            format_mtime(951_782_400 * 1_000_000_000).as_deref(),
            Some("2000-02-29T00:00:00Z")
        );
        // The nanoseconds are dropped, not rounded.
        assert_eq!(
            format_mtime(1_760_745_600 * 1_000_000_000 + 999_999_999).as_deref(),
            Some("2025-10-18T00:00:00Z")
        );
        assert_eq!(
            format_mtime(-1_000_000_000).as_deref(),
            Some("1969-12-31T23:59:59Z")
        );
        assert_eq!(format_mtime(-1).as_deref(), Some("1969-12-31T23:59:59Z"));
        assert_eq!(
            format_mtime(4_102_444_800 * 1_000_000_000).as_deref(),
            Some("2100-01-01T00:00:00Z")
        );
        assert_eq!(format_mtime(i64::MIN), None);
    }

    #[test]
    fn the_flags_column_is_fixed_width() {
        assert_eq!(flags_column(EntryFlags::EMPTY), "----");
        assert_eq!(
            flags_column(EntryFlags::EXECUTABLE.union(EntryFlags::SYSTEM)),
            "x--s"
        );
        assert_eq!(flags_column(EntryFlags::from_bits(0b1111).unwrap()), "xhrs");
        assert_eq!(
            flag_names(EntryFlags::HIDDEN.union(EntryFlags::READ_ONLY)),
            ["hidden", "read-only"]
        );
    }

    #[test]
    fn a_text_line_escapes_control_characters_as_the_reference_tool_does() {
        let e = Entry {
            kind: EntryKind::Symlink,
            flags: EntryFlags::READ_ONLY,
            path: "a\nb".to_string(),
            mtime_ns: i64::MIN,
            size: 3,
            symlink_target: Some(b"x\ty".to_vec()),
            chunks: Vec::new(),
        };
        let mut out = Vec::new();
        text_line(&mut out, &e, false).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("Symlink\t3\t-\t{}\n", escape_path("a\nb"))
        );
        let mut out = Vec::new();
        text_line(&mut out, &e, true).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Symlink\t3\t-\t--r-\ta\\nb -> x\\ty\n"
        );
    }

    #[test]
    fn a_json_line_carries_every_field() {
        let e = Entry {
            kind: EntryKind::File,
            flags: EntryFlags::EXECUTABLE,
            path: "d/\u{1}f\"".to_string(),
            mtime_ns: 1_500_000_000,
            size: 10,
            symlink_target: None,
            chunks: vec![0, 1],
        };
        let mut out = Vec::new();
        json_line(&mut out, &e, 2).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.ends_with('\n') && text.matches('\n').count() == 1,
            "{text}"
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["kind"], "File");
        assert_eq!(v["size"], 10);
        assert_eq!(v["mtime"], "1970-01-01T00:00:01Z");
        assert_eq!(v["mtime_ns"], 1_500_000_000);
        assert_eq!(v["flags"], serde_json::json!(["executable"]));
        assert_eq!(v["flag_bits"], 1);
        assert_eq!(v["path"], "d/\u{1}f\"");
        assert_eq!(v["symlink_target"], serde_json::Value::Null);
        assert_eq!(v["chunks"], 2);
        assert_eq!(v["generation"], 2);
        assert_eq!(v.as_object().unwrap().len(), 10);
    }
}
