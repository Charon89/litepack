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

/// The largest stream read: a stream larger than this is not a zone file Windows wrote; what
/// was read is parsed.
const MAX_STREAM: u64 = 64 * 1024;

/// What reading an archive's zone found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneRead {
    /// The propagating zone, if any.
    pub zone: Option<Zone>,
    /// The stream (or the volume probe) could not be read: the zone is the Internet zone, fail
    /// safe, and the caller warns once ([`unreadable_warning`]).
    pub unreadable: bool,
}

impl ZoneRead {
    const NONE: ZoneRead = ZoneRead {
        zone: None,
        unreadable: false,
    };
    const UNREADABLE: ZoneRead = ZoneRead {
        zone: Some(Zone::INTERNET),
        unreadable: true,
    };
}

/// The propagating zone of the open archive `file`, read once when it is opened, through the
/// archive's own handle (the `:Zone.Identifier` stream opened relative to it,
/// [`lpk_fs_sys::read_stream`]), never by path: no other file swapped in at the archive's
/// path after the open can lend or withhold a zone. Unzoned only when the file's volume has no
/// named streams or the file has no such stream; else [`parse`] of the stream; and every other
/// failure (the probe, access denied, a sharing violation, a read error) is the Internet zone
/// with [`ZoneRead::unreadable`] set (fail safe). Always unzoned outside Windows.
pub fn read_zone_of(file: &File) -> ZoneRead {
    #[cfg(windows)]
    {
        match lpk_fs_sys::named_streams(file) {
            Ok(true) => {}
            Ok(false) => return ZoneRead::NONE,
            Err(_) => return ZoneRead::UNREADABLE,
        }
        match lpk_fs_sys::read_stream(file, STREAM, MAX_STREAM) {
            Ok(None) => ZoneRead::NONE,
            Ok(Some(bytes)) => ZoneRead {
                zone: parse(&bytes),
                unreadable: false,
            },
            Err(_) => ZoneRead::UNREADABLE,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        ZoneRead::NONE
    }
}

/// [`read_zone_of`] of the file at `path` (opened for the read; a file that cannot be opened
/// is unzoned: there is nothing to derive from it). For a file this process wrote (the CLI's
/// check of an export or repair output) and for tests; an archive is read through its open
/// handle.
pub fn read_zone(path: &Path) -> Option<Zone> {
    File::open(path).map_or(None, |f| read_zone_of(&f).zone)
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
/// handle, after probing its volume ([`holds_streams`]; one output file: export, repair).
/// `false` when the volume cannot hold it (no named streams, or the write failed): the caller
/// warns once and goes on. Outside Windows nothing is written (`false`).
pub fn mark(file: &File, zone: Zone) -> bool {
    holds_streams(file) && write(file, zone)
}

/// Write `zone`'s stream onto `file` without a probe: for a caller that probed the volume once
/// for all its outputs (the output tree).
pub(crate) fn write(file: &File, zone: Zone) -> bool {
    #[cfg(windows)]
    {
        lpk_fs_sys::write_stream(file, STREAM, &zone.stream_bytes()).is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = (file, zone);
        false
    }
}

/// The one warning printed when the archive's own zone could not be read (it is then taken
/// as the Internet zone).
pub fn unreadable_warning() -> String {
    "Mark of the Web: the archive's Zone.Identifier stream could not be read; its outputs are \
     marked with the Internet zone (ZoneId=3)"
        .to_string()
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

    pub(crate) fn forced_streams() -> Option<bool> {
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

    /// Fail safe: a stream that exists and cannot be opened (here a sharing violation: another
    /// handle holds it without sharing) is the Internet zone, flagged unreadable, even when the
    /// stream says zone 1.
    #[cfg(windows)]
    #[test]
    fn an_unreadable_stream_is_the_internet_zone_flagged() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let a = d.path().join("a.lpk");
        std::fs::write(&a, b"x").unwrap_or_else(|e| panic!("{e}"));
        let s = d.path().join("a.lpk:Zone.Identifier");
        std::fs::write(&s, b"[ZoneTransfer]\r\nZoneId=1\r\n").unwrap_or_else(|e| panic!("{e}"));
        let f = File::open(&a).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_zone_of(&f), ZoneRead::NONE);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&s)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_zone_of(&f), ZoneRead::UNREADABLE);
        drop(lock);
        assert_eq!(read_zone_of(&f), ZoneRead::NONE);
    }

    /// The zone is read through the archive's handle: a file swapped in at its path after the
    /// open lends it nothing.
    #[cfg(windows)]
    #[test]
    fn the_zone_is_read_through_the_open_handle() {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let a = d.path().join("a.lpk");
        std::fs::write(&a, b"x").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(
            d.path().join("a.lpk:Zone.Identifier"),
            Zone::RESTRICTED.stream_bytes(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let f = File::open(&a).unwrap_or_else(|e| panic!("{e}"));
        std::fs::rename(&a, d.path().join("moved")).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&a, b"unzoned impostor").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_zone_of(&f).zone, Some(Zone::RESTRICTED));
        assert_eq!(read_zone(&a), None);
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
