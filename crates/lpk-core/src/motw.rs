//! Mark of the Web (E6-5; `docs/notes/e6-design.md` section 4).
//!
//! **Rule.** An archive whose `Zone.Identifier` stream gives `ZoneId` 3 (Internet) or 4
//! (Restricted), or whose stream is there but unparseable (read as 3), passes
//! `[ZoneTransfer]` / `ZoneId=<n>` to every file derived from it: extracted files (`.lpk` in
//! process and foreign archives through the broker, hard-link copies included), export and
//! repair copies. Zones 0–2, or no stream at all: nothing. Only `ZoneId` is written — never
//! `HostUrl` or `ReferrerUrl`, which would spread tokens or local paths. The zone is read once,
//! when the archive is opened ([`read_zone`]); the stream is written after the file's
//! create-new and before its first byte, relative to the file's own handle
//! ([`lpk_fs_sys::write_stream`]), never by path.
//!
//! A destination whose volume lacks `FILE_NAMED_STREAMS` ([`holds_streams`]), or where the
//! write fails, cannot hold the mark: the output goes on and the caller prints one warning.
//!
//! Outside Windows there are no zones: [`read_zone`] is always `None` and nothing is written.

use std::fs::File;
use std::path::Path;

/// The name of the stream Windows keeps an origin zone in.
pub const STREAM: &str = "Zone.Identifier";

/// A zone that propagates: 3 (Internet) or 4 (Restricted sites).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone(u8);

impl Zone {
    /// The Internet zone (3): also what an unparseable stream is read as.
    pub const INTERNET: Zone = Zone(3);
    /// The Restricted sites zone (4).
    pub const RESTRICTED: Zone = Zone(4);

    /// The zone for a `ZoneId` value: `Some` for 3 and 4 only.
    pub fn from_id(id: u8) -> Option<Zone> {
        matches!(id, 3 | 4).then_some(Zone(id))
    }

    /// The `ZoneId` value (3 or 4).
    pub fn id(self) -> u8 {
        self.0
    }

    /// The stream's bytes: `[ZoneTransfer]` and `ZoneId=<n>`, CRLF line ends, nothing else.
    pub fn stream_bytes(self) -> Vec<u8> {
        format!("[ZoneTransfer]\r\nZoneId={}\r\n", self.0).into_bytes()
    }
}

/// The propagating zone a `Zone.Identifier` stream's content gives (the rule of the module
/// documentation): `ZoneId` 0–2 is `None`, 3 or 4 that zone, and anything else — no
/// `[ZoneTransfer]` section, no `ZoneId` key, a value that is not an integer from 0 to 4,
/// text that is not UTF-8 or UTF-16 — the Internet zone. Section and key names are matched
/// without regard to ASCII case; spaces around `=` and the value are ignored; a UTF-8 or
/// UTF-16LE byte order mark is accepted.
pub fn parse(bytes: &[u8]) -> Option<Zone> {
    let text = match decode(bytes) {
        Some(t) => t,
        None => return Some(Zone::INTERNET),
    };
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case("[ZoneTransfer]");
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("ZoneId") {
            continue;
        }
        return match value.trim().parse::<u8>() {
            Ok(0..=2) => None,
            Ok(id) => Some(Zone::from_id(id).unwrap_or(Zone::INTERNET)),
            Err(_) => Some(Zone::INTERNET),
        };
    }
    Some(Zone::INTERNET)
}

fn decode(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        if rest.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return String::from_utf16(&units).ok();
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// The propagating zone of the archive at `path`, read once when it is opened: `None` when it
/// has no `Zone.Identifier` stream (or the stream cannot be opened: no streams on its volume),
/// else [`parse`] of the stream (a stream that opens and cannot be read is the Internet zone).
/// Always `None` outside Windows.
pub fn read_zone(path: &Path) -> Option<Zone> {
    #[cfg(windows)]
    {
        use std::io::Read;
        let mut name = path.as_os_str().to_owned();
        name.push(":");
        name.push(STREAM);
        let mut f = File::open(name).ok()?;
        let mut buf = Vec::new();
        // A stream larger than this is not a zone file Windows wrote; what was read is parsed.
        match f.by_ref().take(64 * 1024).read_to_end(&mut buf) {
            Ok(_) => parse(&buf),
            Err(_) => Some(Zone::INTERNET),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// Whether the volume `file` is on holds named streams (`FILE_NAMED_STREAMS`); a failed probe
/// is `false` (the mark cannot be held). Always `false` outside Windows.
pub fn holds_streams(file: &File) -> bool {
    #[cfg(test)]
    if let Some(v) = seam::forced_streams() {
        return v;
    }
    #[cfg(windows)]
    {
        lpk_fs_sys::named_streams(file).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        false
    }
}

/// Write `zone`'s `Zone.Identifier` stream onto the open, still empty `file`, relative to its
/// handle. `false` when the volume cannot hold it (no named streams, or the write failed):
/// the caller warns once and goes on. Outside Windows nothing is written (`false`).
pub fn mark(file: &File, zone: Zone) -> bool {
    if !holds_streams(file) {
        return false;
    }
    #[cfg(windows)]
    {
        lpk_fs_sys::write_stream(file, STREAM, &zone.stream_bytes()).is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = zone;
        false
    }
}

/// The one warning printed when files derived from a zoned archive could not carry the mark.
pub fn unmarked_warning(zone: Zone) -> String {
    format!(
        "Mark of the Web: the destination's volume cannot hold the Zone.Identifier stream \
         (no named streams, or the write failed); the output does not carry the archive's \
         zone (ZoneId={})",
        zone.id()
    )
}

/// Test hooks (`cfg(test)` only): force [`holds_streams`]'s answer on this thread, so the
/// branch of a volume without named streams is tested where no such volume can be made.
#[cfg(test)]
pub(crate) mod seam {
    use std::cell::Cell;

    thread_local! {
        static STREAMS: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// Force [`super::holds_streams`] on this thread (`None`: the real probe).
    pub(crate) fn force_streams(v: Option<bool>) {
        STREAMS.with(|c| c.set(v));
    }

    pub(super) fn forced_streams() -> Option<bool> {
        STREAMS.with(Cell::get)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_three_and_four_propagate_and_zero_to_two_do_not() {
        for id in 0..=2u8 {
            let s = format!("[ZoneTransfer]\r\nZoneId={id}\r\n");
            assert_eq!(parse(s.as_bytes()), None, "{id}");
        }
        assert_eq!(parse(b"[ZoneTransfer]\r\nZoneId=3\r\n"), Some(Zone(3)));
        assert_eq!(
            parse(b"[ZoneTransfer]\r\nZoneId=4\r\nHostUrl=https://x/\r\n"),
            Some(Zone(4))
        );
        // Case, spaces, LF-only line ends, and other keys first.
        assert_eq!(
            parse(b"[zonetransfer]\nReferrerUrl=a\n  zoneid = 4 \n"),
            Some(Zone(4))
        );
    }

    #[test]
    fn an_unparseable_stream_is_the_internet_zone() {
        for s in [
            &b""[..],
            b"garbage",
            b"[Other]\r\nZoneId=1\r\n",
            b"[ZoneTransfer]\r\nHostUrl=x\r\n",
            b"[ZoneTransfer]\r\nZoneId=x\r\n",
            b"[ZoneTransfer]\r\nZoneId=-1\r\n",
            b"[ZoneTransfer]\r\nZoneId=5\r\n",
            b"[ZoneTransfer]\r\nZoneId=300\r\n",
            b"\xff\xfe\x00",
            b"\xc3\x28",
        ] {
            assert_eq!(parse(s), Some(Zone::INTERNET), "{s:?}");
        }
    }

    #[test]
    fn byte_order_marks_are_read() {
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "[ZoneTransfer]\r\nZoneId=2\r\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(parse(&utf16), None);
        assert_eq!(
            parse(b"\xEF\xBB\xBF[ZoneTransfer]\nZoneId=4"),
            Some(Zone(4))
        );
    }

    #[test]
    fn the_stream_carries_the_zone_id_only() {
        assert_eq!(
            Zone::INTERNET.stream_bytes(),
            b"[ZoneTransfer]\r\nZoneId=3\r\n"
        );
        assert_eq!(Zone::from_id(2), None);
        assert_eq!(Zone::from_id(4), Some(Zone::RESTRICTED));
    }

    #[cfg(windows)]
    #[test]
    fn read_zone_reads_the_stream_and_none_without_one() {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let a = d.path().join("a.lpk");
        std::fs::write(&a, b"x").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_zone(&a), None);
        let s = d.path().join("a.lpk:Zone.Identifier");
        std::fs::write(&s, b"[ZoneTransfer]\r\nZoneId=1\r\n").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_zone(&a), None);
        std::fs::write(&s, b"[ZoneTransfer]\r\nZoneId=3\r\n").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_zone(&a), Some(Zone::INTERNET));
    }

    #[cfg(windows)]
    #[test]
    fn mark_writes_through_the_handle_and_skips_a_volume_without_streams() {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let p = d.path().join("f");
        let f = File::create(&p).unwrap_or_else(|e| panic!("{e}"));
        assert!(mark(&f, Zone::RESTRICTED));
        assert_eq!(read_zone(&p), Some(Zone::RESTRICTED));
        let q = d.path().join("g");
        let g = File::create(&q).unwrap_or_else(|e| panic!("{e}"));
        seam::force_streams(Some(false));
        let marked = mark(&g, Zone::RESTRICTED);
        seam::force_streams(None);
        assert!(!marked);
        assert_eq!(read_zone(&q), None);
    }
}
