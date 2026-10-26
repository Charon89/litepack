//! Detection of foreign archives by magic, in the host (E4-3; design §3 "Detection by magic, in
//! the host"): byte comparisons only, no parsing. [`detect_file`] is the one entry point every
//! front end routes by (the CLI's `x` and `l`, E5-1's `Session::open`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A foreign format the broker recognises (by magic, [`detect_file`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `lpk_proto::fixture`: the test-only format of the test worker.
    TestFixture,
    /// ZIP (and JAR, EPUB, Office files): a local header at the start, or an end record at
    /// the end (which finds self-extracting archives).
    Zip,
    /// 7z, at the start or (self-extracting) inside a PE file's first [`PE_SCAN`] bytes.
    SevenZ,
    /// tar (POSIX `ustar`, GNU, or a v7 header whose checksum holds).
    Tar,
    /// gzip (a single file or a `.tar.gz`; the worker tells which).
    Gzip,
    /// bzip2 (a single file or a `.tar.bz2`).
    Bzip2,
    /// xz (a single file or a `.tar.xz`).
    Xz,
    /// Zstandard (a single file or a `.tar.zst`).
    Zstd,
    /// lzip (a single file or a `.tar.lz`).
    Lzip,
    /// The legacy `.lzma` (LZMA-alone) stream (a single file or a `.tar.lzma`).
    Lzma,
    /// RAR 4 or 5 (also inside a PE file, and as a volume set): read by the libarchive worker
    /// (E4-4).
    Rar,
    /// ISO 9660 (Rock Ridge, Joliet): `CD001` at 0x8001; the libarchive worker.
    Iso,
    /// Microsoft Cabinet (`MSCF`, version 1.3); the libarchive worker.
    Cab,
    /// LHA/LZH (`-lh?-`/`-lz?-` at 2); the libarchive worker.
    Lha,
    /// Unix `ar` (`!<arch>\n`, also `.deb`); the libarchive worker.
    Ar,
    /// cpio (odc, newc, crc, binary); the libarchive worker.
    Cpio,
    /// RPM (its lead's magic); the libarchive worker.
    Rpm,
    /// Unix `compress` (`.Z`, a single file or a `.tar.Z`); the libarchive worker.
    Compress,
    /// uuencode or base64 (`begin ` / `begin-base64 ` at a line start); the libarchive worker.
    Uue,
}

/// Which worker binary reads a format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerKind {
    /// `lpk-worker`: the pure-Rust readers (E4-3), either sandbox mode.
    Rust,
    /// `lpk-worker-libarchive` (E4-4): LPAC only.
    Libarchive,
}

/// How the host requires a format's packed ranges (D-99 (4): without them a lying worker could
/// slip past the overlap check with `packed_len = 0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ranges {
    /// Not required (the test fixture).
    Advisory,
    /// Every record has a non-empty range (ZIP: local header to the end of the data; tar:
    /// header to the end of the data).
    Every,
    /// Every file record with a size has a non-empty range (7z: its block's pack streams; the
    /// compressed single streams: the whole stream as one block).
    NonEmptyFiles,
}

impl Format {
    /// The format's wire name (what `Open` carries and the worker selects its reader by).
    pub fn name(self) -> &'static str {
        match self {
            Format::TestFixture => lpk_proto::fixture::FORMAT,
            Format::Zip => "zip",
            Format::SevenZ => "7z",
            Format::Tar => "tar",
            Format::Gzip => "gz",
            Format::Bzip2 => "bz2",
            Format::Xz => "xz",
            Format::Zstd => "zst",
            Format::Lzip => "lz",
            Format::Lzma => "lzma",
            Format::Rar => "rar",
            Format::Iso => "iso",
            Format::Cab => "cab",
            Format::Lha => "lzh",
            Format::Ar => "ar",
            Format::Cpio => "cpio",
            Format::Rpm => "rpm",
            Format::Compress => "z",
            Format::Uue => "uue",
        }
    }

    /// The worker binary that reads the format.
    pub fn worker(self) -> WorkerKind {
        match self {
            Format::Rar
            | Format::Iso
            | Format::Cab
            | Format::Lha
            | Format::Ar
            | Format::Cpio
            | Format::Rpm
            | Format::Compress
            | Format::Uue => WorkerKind::Libarchive,
            _ => WorkerKind::Rust,
        }
    }

    /// The smallest member header of the format, for the entries cap (ZIP: a central
    /// directory record; 7z: a one-character name in its header; tar and the compressed
    /// streams, which may hold a tar: a 512-byte header).
    pub fn min_header(self) -> u64 {
        match self {
            Format::TestFixture => lpk_proto::fixture::MIN_MEMBER,
            Format::Zip => 46,
            Format::SevenZ => 4,
            Format::Rar => 7,
            // A directory record; a CFFILE with a one-byte name; an LHA level-0 header; an ar
            // header; a binary cpio header (an RPM's payload too).
            Format::Iso => 34,
            Format::Cab => 17,
            Format::Lha => 22,
            Format::Ar => 60,
            Format::Cpio | Format::Rpm => 26,
            Format::Compress | Format::Uue => 512,
            Format::Tar
            | Format::Gzip
            | Format::Bzip2
            | Format::Xz
            | Format::Zstd
            | Format::Lzip
            | Format::Lzma => 512,
        }
    }

    /// Whether member headers are compressed (the entries cap is then multiplied by the
    /// expansion limit).
    pub fn compressed_headers(self) -> bool {
        !matches!(
            self,
            Format::TestFixture
                | Format::Zip
                | Format::Tar
                | Format::Iso
                | Format::Cab
                | Format::Lha
                | Format::Ar
                | Format::Cpio
        )
    }

    /// How the host requires the format's packed ranges.
    pub fn ranges(self) -> Ranges {
        match self {
            Format::TestFixture => Ranges::Advisory,
            Format::Zip | Format::Tar => Ranges::Every,
            _ => Ranges::NonEmptyFiles,
        }
    }

    /// How many times the archive's length the job's read bytes are credited as progress. A
    /// compressed stream or 7z is read more than once by the worker (the listing decompresses
    /// it, streaming decompresses it again and skips to each member, a solid block read out of
    /// order restarts), and the reads of a later pass are the only progress the host can see
    /// while the worker skips decompressed bytes it does not send; capped, so a worker reading
    /// in a loop cannot stay alive forever.
    /// libarchive's formats read twice too (the listing pass, then the streaming pass, which
    /// restarts for a member before its position).
    pub fn read_passes(self) -> u64 {
        if self.compressed_headers() || self.worker() == WorkerKind::Libarchive {
            3
        } else {
            1
        }
    }

    /// Whether a worker of this build reads the format (every recognised format since E4-4).
    pub fn read_by_worker(self) -> bool {
        true
    }
}

/// The `.lpk` magic: an `.lpk` is never routed to a worker.
const LPK_MAGIC: [u8; 8] = lpk_format::MAGIC;
/// 7z's signature (then the major version `0` and the start header's CRC).
const SEVENZ_MAGIC: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
/// RAR 4 and RAR 5 share these six bytes (RAR 5 continues `07 01 00`, RAR 4 `07 00`).
const RAR_MAGIC: [u8; 6] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07];
/// How far into a PE file an embedded 7z or RAR signature is looked for.
pub const PE_SCAN: u64 = 4 << 20;
/// The bytes [`detect`] looks at from the start of a file.
pub const HEAD: usize = 4096;
/// The ZIP end record's fixed part, and the longest comment after it.
const ZIP_EOCD: usize = 22;
const ZIP_COMMENT_MAX: usize = 0xFFFF;

/// CRC-32 (IEEE), bitwise: only the 20 bytes of a 7z start header are ever checked here.
fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// A 7z start header at `at` in `buf`: the signature, major version 0, and the stored CRC of
/// the next 20 bytes (so the signature constant inside an SFX stub does not match).
pub fn sevenz_at(buf: &[u8], at: usize) -> bool {
    let Some(h) = buf.get(at..at.saturating_add(32)) else {
        return false;
    };
    h[..6] == SEVENZ_MAGIC
        && h[6] == 0
        && u32::from_le_bytes([h[8], h[9], h[10], h[11]]) == crc32(&h[12..32])
}

/// A RAR 4 or RAR 5 signature at `at` in `buf`.
fn rar_at(buf: &[u8], at: usize) -> bool {
    match buf.get(at..at.saturating_add(8)) {
        Some(h) => h[..6] == RAR_MAGIC && (h[6] == 0 || (h[6] == 1 && h[7] == 0)),
        None => false,
    }
}

/// A RAR signature at `at` in `buf` followed by a main archive header whose CRC holds (so the
/// bare signature bytes inside a PE file do not match): RAR 5's header CRC-32 over the header
/// size and the header; RAR 4's MAIN_HEAD (type 0x73) whose 16-bit CRC is the low half of the
/// CRC-32 from its type byte to its end.
pub fn rar_valid_at(buf: &[u8], at: usize) -> bool {
    if !rar_at(buf, at) {
        return false;
    }
    let Some(rest) = buf.get(at..) else {
        return false;
    };
    if rest[6] == 1 {
        // RAR 5: CRC32 (4), then the header size as a vint (at most 3 bytes here), then that
        // many header bytes.
        let Some(h) = rest.get(8..) else {
            return false;
        };
        let Some(crc) = h.get(..4) else {
            return false;
        };
        let (mut size, mut n) = (0usize, 0usize);
        loop {
            let Some(&b) = h.get(4 + n) else {
                return false;
            };
            size |= usize::from(b & 0x7F) << (7 * n);
            n += 1;
            if b & 0x80 == 0 {
                break;
            }
            if n == 3 {
                return false;
            }
        }
        let Some(body) = h.get(4..4 + n + size) else {
            return false;
        };
        size > 0 && u32::from_le_bytes([crc[0], crc[1], crc[2], crc[3]]) == crc32(body)
    } else {
        // RAR 4: HEAD_CRC (2), HEAD_TYPE (1), HEAD_FLAGS (2), HEAD_SIZE (2), ...
        let Some(h) = rest.get(7..14) else {
            return false;
        };
        let size = usize::from(u16::from_le_bytes([h[5], h[6]]));
        if h[2] != 0x73 || size < 7 {
            return false;
        }
        let Some(body) = rest.get(9..7 + size) else {
            return false;
        };
        u16::from_le_bytes([h[0], h[1]]) == (crc32(body) & 0xFFFF) as u16
    }
}

/// A tar header in the first 512 bytes: `ustar` at 257, or a v7 header whose checksum (the
/// sum of the header's bytes with the checksum field read as spaces) equals its octal field.
fn tar_header(head: &[u8]) -> bool {
    let Some(h) = head.get(..512) else {
        return false;
    };
    if &h[257..262] == b"ustar" {
        return true;
    }
    if h[0] == 0 {
        return false;
    }
    let digits: Vec<u8> = h[148..156]
        .iter()
        .copied()
        .skip_while(|&b| b == b' ')
        .take_while(|b| (b'0'..=b'7').contains(b))
        .collect();
    if digits.is_empty() || digits.len() > 7 {
        return false;
    }
    let stored = digits
        .iter()
        .fold(0u32, |a, &d| a * 8 + u32::from(d - b'0'));
    let sum: u32 = h
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u32::from(b)
            }
        })
        .sum();
    stored == sum
}

/// The legacy `.lzma` header: the usual properties byte (lc=3, lp=0, pb=2), a dictionary size
/// of 2^n or 3 * 2^n (as LZMA encoders write) of at least 4 KiB, and an unknown or plausible
/// size.
fn lzma_alone(head: &[u8]) -> bool {
    let Some(h) = head.get(..13) else {
        return false;
    };
    if h[0] != 0x5D {
        return false;
    }
    let dict = u32::from_le_bytes([h[1], h[2], h[3], h[4]]);
    let shape = dict.is_power_of_two() || (dict >> dict.trailing_zeros()) == 3;
    let size = u64::from_le_bytes([h[5], h[6], h[7], h[8], h[9], h[10], h[11], h[12]]);
    dict >= 4096 && shape && (size == u64::MAX || size < 1 << 48)
}

/// Byte comparisons on the first bytes of a file (at most [`HEAD`] are looked at); `None` for
/// an `.lpk` or anything not recognised from the head alone. [`detect_file`] adds what needs
/// the file's end (a ZIP end record) or a scan (a PE file's embedded 7z or RAR).
pub fn detect(head: &[u8]) -> Option<Format> {
    if head.starts_with(&LPK_MAGIC) {
        return None;
    }
    if lpk_proto::fixture::detect(head) {
        return Some(Format::TestFixture);
    }
    if head.starts_with(&SEVENZ_MAGIC) {
        return Some(Format::SevenZ);
    }
    if rar_at(head, 0) {
        return Some(Format::Rar);
    }
    // A local header, an empty archive's end record, or a split archive's spanning marker.
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08PK\x03\x04")
    {
        return Some(Format::Zip);
    }
    if head.starts_with(&[0x1F, 0x8B, 0x08]) {
        return Some(Format::Gzip);
    }
    if head.len() >= 10
        && head.starts_with(b"BZh")
        && (b'1'..=b'9').contains(&head[3])
        && (head[4..10] == [0x31, 0x41, 0x59, 0x26, 0x53, 0x59]
            || head[4..10] == [0x17, 0x72, 0x45, 0x38, 0x50, 0x90])
    {
        return Some(Format::Bzip2);
    }
    if head.starts_with(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]) {
        return Some(Format::Xz);
    }
    if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(Format::Zstd);
    }
    if head.starts_with(b"LZIP") {
        return Some(Format::Lzip);
    }
    if head.starts_with(b"MSCF\0\0\0\0") && head.get(24..26) == Some(&[3, 1][..]) {
        return Some(Format::Cab);
    }
    if head.starts_with(b"!<arch>\n") {
        return Some(Format::Ar);
    }
    if head.starts_with(&[0xED, 0xAB, 0xEE, 0xDB]) {
        return Some(Format::Rpm);
    }
    if head.starts_with(b"070707")
        || head.starts_with(b"070701")
        || head.starts_with(b"070702")
        || (head.len() >= 26
            && (head.starts_with(&[0xC7, 0x71]) || head.starts_with(&[0x71, 0xC7])))
    {
        return Some(Format::Cpio);
    }
    if head.starts_with(&[0x1F, 0x9D]) {
        return Some(Format::Compress);
    }
    if lha_header(head) {
        return Some(Format::Lha);
    }
    if tar_header(head) {
        return Some(Format::Tar);
    }
    if lzma_alone(head) {
        return Some(Format::Lzma);
    }
    if uu_begin(head) {
        return Some(Format::Uue);
    }
    None
}

/// An LHA header at the start: `-lh?-` or `-lz?-` at 2 with a method libarchive knows, and a
/// header level of 0 to 3 at 20.
fn lha_header(head: &[u8]) -> bool {
    let Some(h) = head.get(..22) else {
        return false;
    };
    let method_ok = match &h[2..5] {
        b"-lh" => matches!(h[5], b'0'..=b'7' | b'd'),
        b"-lz" => matches!(h[5], b's' | b'4' | b'5'),
        _ => false,
    };
    method_ok && h[6] == b'-' && h[20] <= 3
}

/// A uuencode (`begin` and an octal mode) or base64 (`begin-base64`) start line at the head's
/// start or at a line start within it.
fn uu_begin(head: &[u8]) -> bool {
    let line_at = |i: usize| -> bool {
        let rest = &head[i..];
        let after = if let Some(r) = rest.strip_prefix(b"begin-base64 ") {
            r
        } else if let Some(r) = rest.strip_prefix(b"begin ") {
            r
        } else {
            return false;
        };
        let digits = after
            .iter()
            .take_while(|b| (b'0'..=b'7').contains(b))
            .count();
        (3..=4).contains(&digits) && after.get(digits) == Some(&b' ')
    };
    line_at(0)
        || head
            .iter()
            .enumerate()
            .any(|(i, &b)| b == b'\n' && i + 1 < head.len() && line_at(i + 1))
}

/// Where an ISO 9660 primary (or any) volume descriptor's `CD001` is (sector 16).
const ISO_MAGIC_AT: u64 = 0x8001;

/// The most volumes of a set a worker takes: the bootstrap page holds 16 handles, three of them
/// pipes (a named limit until the page grows).
pub const MAX_VOLUMES: usize = 13;

/// A RAR main header's volume flag at the start of `head` (RAR 4: `MHD_VOLUME`; RAR 5: the
/// main archive header's volume flag).
fn rar_volume_flag(head: &[u8]) -> bool {
    if !rar_valid_at(head, 0) {
        return false;
    }
    if head[6] == 0 {
        // RAR 4: HEAD_FLAGS of the main header at 7 + 3.
        return head.get(10).is_some_and(|f| f & 0x01 != 0);
    }
    // RAR 5: CRC32, size vint, type vint (1), flags vint, [extra vint], [data vint], archive
    // flags vint.
    let mut p = 12usize;
    let vint = |p: &mut usize| -> Option<u64> {
        let mut v = 0u64;
        for s in 0..10 {
            let b = *head.get(*p)?;
            *p += 1;
            v |= u64::from(b & 0x7F) << (7 * s);
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    };
    let Some(_size) = vint(&mut p) else {
        return false;
    };
    let (Some(ty), Some(flags)) = (vint(&mut p), vint(&mut p)) else {
        return false;
    };
    if ty != 1 {
        return false;
    }
    if flags & 1 != 0 && vint(&mut p).is_none() {
        return false;
    }
    if flags & 2 != 0 && vint(&mut p).is_none() {
        return false;
    }
    vint(&mut p).is_some_and(|a| a & 1 != 0)
}

/// The volumes of the archive at `path`, first to last: itself alone unless it is the first
/// volume of a RAR set (its main header's volume flag) named `<stem>.partN.rar` (N of any
/// width, all zeros but the last digit `1`) or `<stem>.rar` with `<stem>.r00` beside it; then
/// the following names while they exist. `Err(n)` when the set has more than [`MAX_VOLUMES`].
pub fn volume_set(path: &Path, format: Format) -> std::io::Result<Result<Vec<PathBuf>, usize>> {
    let single = Ok(Ok(vec![path.to_path_buf()]));
    if format != Format::Rar {
        return single;
    }
    let mut head = vec![0u8; 64];
    let mut f = File::open(path)?;
    let n = read_at(&mut f, 0, &mut head)?;
    head.truncate(n);
    if !rar_volume_flag(&head) {
        return single;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return single;
    };
    let lower = name.to_ascii_lowercase();
    let mut names: Vec<String> = Vec::new();
    if let Some(stem) = lower.strip_suffix(".rar") {
        if let Some(i) = stem.rfind(".part") {
            let digits = &stem[i + 5..];
            let w = digits.len();
            if w > 0
                && digits.bytes().all(|b| b.is_ascii_digit())
                && digits.trim_start_matches('0') == "1"
            {
                let prefix = &name[..i + 5];
                let suffix = &name[name.len() - 4..];
                for k in 2..=usize::MAX {
                    let candidate = format!("{prefix}{k:0w$}{suffix}");
                    if !path.with_file_name(&candidate).is_file() {
                        break;
                    }
                    names.push(candidate);
                    if names.len() >= MAX_VOLUMES {
                        break;
                    }
                }
            }
        } else {
            let base = &name[..name.len() - 4];
            let r = if name.ends_with(".RAR") { "R" } else { "r" };
            for k in 0..100 {
                let candidate = format!("{base}.{r}{k:02}");
                if !path.with_file_name(&candidate).is_file() {
                    break;
                }
                names.push(candidate);
                if names.len() >= MAX_VOLUMES {
                    break;
                }
            }
        }
    }
    let mut out = vec![path.to_path_buf()];
    out.extend(names.iter().map(|n| path.with_file_name(n)));
    if out.len() > MAX_VOLUMES {
        return Ok(Err(out.len()));
    }
    Ok(Ok(out))
}

/// Read up to `buf.len()` bytes at `at`; the count read.
fn read_at(f: &mut File, at: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    f.seek(SeekFrom::Start(at))?;
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            m => n += m,
        }
    }
    Ok(n)
}

/// A ZIP end record in `tail` (the file's last bytes) whose comment ends exactly at the end.
pub fn zip_end_record_in(tail: &[u8]) -> bool {
    if tail.len() < ZIP_EOCD {
        return false;
    }
    for i in (0..=tail.len() - ZIP_EOCD).rev() {
        if tail[i..].starts_with(b"PK\x05\x06") {
            let comment = usize::from(u16::from_le_bytes([tail[i + 20], tail[i + 21]]));
            if i + ZIP_EOCD + comment == tail.len() {
                return true;
            }
        }
    }
    false
}

/// A ZIP end record in the file's last 64 KiB + 22 bytes (how a self-extracting ZIP, or any
/// ZIP with data in front of it, is found).
fn zip_end_record(f: &mut File, len: u64) -> std::io::Result<bool> {
    let want = (ZIP_EOCD + ZIP_COMMENT_MAX) as u64;
    let from = len.saturating_sub(want);
    let mut tail = vec![0u8; usize::try_from(len - from).unwrap_or(0)];
    let n = read_at(f, from, &mut tail)?;
    tail.truncate(n);
    Ok(zip_end_record_in(&tail))
}

/// A PE file (`MZ`, and `PE\0\0` where `e_lfanew` points) carrying a 7z or RAR archive in its
/// first [`PE_SCAN`] bytes (a self-extracting archive).
fn pe_embedded(f: &mut File, head: &[u8]) -> std::io::Result<Option<Format>> {
    let Some(l) = head.get(0x3C..0x40) else {
        return Ok(None);
    };
    let lfanew = u32::from_le_bytes([l[0], l[1], l[2], l[3]]) as usize;
    if !head.starts_with(b"MZ")
        || head.get(lfanew..lfanew.saturating_add(4)) != Some(b"PE\0\0".as_slice())
    {
        return Ok(None);
    }
    // Windows overlapping by 512 bytes (more than a 7z start header or a RAR main header's
    // first part), so an archive starting near a boundary is seen whole.
    const WINDOW: usize = 1 << 20;
    let mut buf = vec![0u8; WINDOW + 512];
    let mut at = 0u64;
    while at < PE_SCAN {
        let n = read_at(f, at, &mut buf)?;
        let window = &buf[..n];
        // The last window (short read) is scanned to its end; others leave their overlap to
        // the next window.
        let limit = if n < buf.len() { n } else { WINDOW };
        for i in 0..limit {
            if at == 0 && i == 0 {
                continue;
            }
            if window[i] == SEVENZ_MAGIC[0] && sevenz_at(window, i) {
                return Ok(Some(Format::SevenZ));
            }
            if window[i] == RAR_MAGIC[0] && rar_valid_at(window, i) {
                return Ok(Some(Format::Rar));
            }
        }
        if n < buf.len() {
            break;
        }
        at += WINDOW as u64;
    }
    Ok(None)
}

/// The one entry point for routing: the foreign format of the file at `path`, by byte
/// comparisons only, or `None` for an `.lpk` or a file that is not a foreign archive this
/// build recognises.
pub fn detect_file(path: &Path) -> std::io::Result<Option<Format>> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = vec![0u8; HEAD];
    let n = read_at(&mut f, 0, &mut head)?;
    head.truncate(n);
    if head.starts_with(&LPK_MAGIC) {
        return Ok(None);
    }
    if let Some(found) = detect(&head) {
        return Ok(Some(found));
    }
    let mut cd = [0u8; 5];
    if len >= ISO_MAGIC_AT + 5 && read_at(&mut f, ISO_MAGIC_AT, &mut cd)? == 5 && &cd == b"CD001" {
        return Ok(Some(Format::Iso));
    }
    if zip_end_record(&mut f, len)? {
        return Ok(Some(Format::Zip));
    }
    pe_embedded(&mut f, &head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sevenz_header() -> Vec<u8> {
        let mut h = SEVENZ_MAGIC.to_vec();
        h.extend_from_slice(&[0, 4]);
        let rest = [7u8; 20];
        h.extend_from_slice(&crc32(&rest).to_le_bytes());
        h.extend_from_slice(&rest);
        h
    }

    #[test]
    fn heads_are_recognised_by_their_magic() {
        assert_eq!(detect(&LPK_MAGIC), None);
        assert_eq!(detect(&sevenz_header()), Some(Format::SevenZ));
        assert_eq!(detect(b"Rar!\x1a\x07\x01\x00xx"), Some(Format::Rar));
        assert_eq!(detect(b"Rar!\x1a\x07\x00xx"), Some(Format::Rar));
        assert_eq!(detect(b"PK\x03\x04rest"), Some(Format::Zip));
        assert_eq!(detect(b"PK\x05\x06rest"), Some(Format::Zip));
        assert_eq!(detect(&[0x1F, 0x8B, 0x08, 0]), Some(Format::Gzip));
        assert_eq!(detect(b"BZh91AY&SYxx"), Some(Format::Bzip2));
        assert_eq!(detect(b"BZh9\x17rE8P\x90"), Some(Format::Bzip2));
        assert_eq!(detect(b"BZh0\x31AY&SY"), None);
        assert_eq!(detect(&[0xFD, b'7', b'z', b'X', b'Z', 0]), Some(Format::Xz));
        assert_eq!(detect(&[0x28, 0xB5, 0x2F, 0xFD, 0]), Some(Format::Zstd));
        assert_eq!(detect(b"LZIP\x01"), Some(Format::Lzip));
        let mut lzma = vec![0x5D, 0, 0, 0x80, 0];
        lzma.extend_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(detect(&lzma), Some(Format::Lzma));
        lzma[1] = 1; // a dictionary size no encoder writes
        assert_eq!(detect(&lzma), None);
        assert_eq!(detect(b"plain text"), None);
        assert_eq!(detect(b""), None);
    }

    #[test]
    fn tar_headers_by_ustar_or_checksum() {
        let mut h = vec![0u8; 512];
        h[..5].copy_from_slice(b"a.txt");
        h[257..263].copy_from_slice(b"ustar\0");
        assert_eq!(detect(&h), Some(Format::Tar));
        // v7: no ustar, the checksum decides.
        h[257..263].fill(0);
        let sum: u32 = h
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u32::from(b)
                }
            })
            .sum();
        let field = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(field.as_bytes());
        assert_eq!(detect(&h), Some(Format::Tar));
        h[0] = b'b';
        assert_eq!(detect(&h), None);
    }

    #[test]
    fn a_sevenz_signature_needs_its_crc() {
        let mut h = sevenz_header();
        assert!(sevenz_at(&h, 0));
        h[20] ^= 1;
        assert!(!sevenz_at(&h, 0));
        assert!(!sevenz_at(&h[..20], 0));
    }

    /// A RAR 5 signature and a minimal main header (type 1, no flags) with its CRC.
    fn rar5() -> Vec<u8> {
        let mut v = b"Rar!\x1a\x07\x01\x00".to_vec();
        let body = [3u8, 1, 0, 0];
        v.extend_from_slice(&crc32(&body).to_le_bytes());
        v.extend_from_slice(&body);
        v
    }

    /// A RAR 4 signature and MAIN_HEAD (type 0x73, size 13) with its CRC.
    fn rar4() -> Vec<u8> {
        let mut v = b"Rar!\x1a\x07\x00".to_vec();
        let mut head = vec![0x73u8, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0];
        let crc = (crc32(&head) & 0xFFFF) as u16;
        v.extend_from_slice(&crc.to_le_bytes());
        v.append(&mut head);
        v
    }

    #[test]
    fn a_rar_signature_needs_its_main_header_crc() {
        assert!(rar_valid_at(&rar5(), 0));
        assert!(rar_valid_at(&rar4(), 0));
        for mut v in [rar5(), rar4()] {
            let last = v.len() - 1;
            v[last] ^= 1;
            assert!(!rar_valid_at(&v, 0));
        }
        assert!(!rar_valid_at(b"Rar!\x1a\x07\x01\x00", 0));
    }

    #[test]
    fn a_pe_with_bare_rar_bytes_is_not_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let mut pe = vec![0u8; 4096];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x300..0x308].copy_from_slice(b"Rar!\x1a\x07\x01\x00");
        let p = dir.path().join("tool.exe");
        std::fs::write(&p, &pe).unwrap();
        assert_eq!(detect_file(&p).unwrap(), None);
        let r = rar5();
        pe[0x600..0x600 + r.len()].copy_from_slice(&r);
        std::fs::write(&p, &pe).unwrap();
        assert_eq!(detect_file(&p).unwrap(), Some(Format::Rar));
    }

    #[test]
    fn the_zip_end_record_must_end_the_file() {
        let mut eocd = b"PK\x05\x06".to_vec();
        eocd.extend_from_slice(&[0u8; 16]);
        eocd.extend_from_slice(&3u16.to_le_bytes());
        let mut tail = b"stub bytes".to_vec();
        tail.extend_from_slice(&eocd);
        tail.extend_from_slice(b"abc");
        assert!(zip_end_record_in(&tail));
        tail.push(b'!');
        assert!(!zip_end_record_in(&tail));
        assert!(!zip_end_record_in(b"PK\x05\x06"));
    }

    #[test]
    fn files_route_by_end_record_and_pe_scan() {
        let dir = tempfile::tempdir().unwrap();
        // An SFX-like ZIP: junk in front, an end record at the end.
        let mut sfx = vec![b'x'; 5000];
        sfx.extend_from_slice(b"PK\x05\x06");
        sfx.extend_from_slice(&[0u8; 18]);
        let p = dir.path().join("sfx.bin");
        std::fs::write(&p, &sfx).unwrap();
        assert_eq!(detect_file(&p).unwrap(), Some(Format::Zip));
        // A PE file with a 7z after its stub, past the first window.
        let mut pe = vec![0u8; (1 << 20) + 77];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        // The bare signature in the stub (no valid CRC) is not taken.
        pe[0x200..0x206].copy_from_slice(&SEVENZ_MAGIC);
        let at = pe.len() - 40;
        pe.splice(at..at + 32, sevenz_header());
        let p = dir.path().join("sfx.exe");
        std::fs::write(&p, &pe).unwrap();
        assert_eq!(detect_file(&p).unwrap(), Some(Format::SevenZ));
        // The same PE without the archive is not foreign.
        pe.truncate(at);
        std::fs::write(&p, &pe).unwrap();
        assert_eq!(detect_file(&p).unwrap(), None);
        // An .lpk is never foreign, whatever follows its magic.
        let mut lpk = LPK_MAGIC.to_vec();
        lpk.extend_from_slice(b"PK\x05\x06");
        lpk.extend_from_slice(&[0u8; 18]);
        let p = dir.path().join("a.lpk");
        std::fs::write(&p, &lpk).unwrap();
        assert_eq!(detect_file(&p).unwrap(), None);
    }
}
