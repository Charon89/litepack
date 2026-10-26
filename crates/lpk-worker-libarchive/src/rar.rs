//! RAR's named gaps, found by walking the first volume's block headers before libarchive sees
//! the archive (design §3's RAR row). Safe Rust over untrusted bytes, inside the sandbox; every
//! length is bounded by the file's size.
//!
//! Written from RARLAB's published format descriptions (RAR 4.x `technote.txt`, the RAR 5.0
//! archive format page); no unRAR code. What is looked at:
//!
//! * RAR 4: the main header's `MHD_PASSWORD` (encrypted headers) and `MHD_SOLID`; each file
//!   header's encrypted flag and its `UNP_VER` (below 29: RAR 1.5/2.x compression, which
//!   libarchive does not decode) unless the member is stored.
//! * RAR 5: an archive encryption header; each file header's encryption extra record and its
//!   dictionary size (above 64 MiB is beyond libarchive 3.8.9's RAR5 reader).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// How far into the first volume the signature is looked for (a self-extracting stub).
pub const SFX_SCAN: u64 = 4 << 20;

/// The most block headers walked (beyond it the rest is left to libarchive).
const MAX_BLOCKS: usize = 1 << 20;

/// RAR 5's largest header (2 MiB, by its format description).
const RAR5_MAX_HEADER: u64 = 2 << 20;

/// The largest RAR5 dictionary libarchive 3.8.9 decodes.
pub const RAR5_MAX_DICT: u64 = 64 << 20;

const MAGIC: [u8; 6] = *b"Rar!\x1a\x07";

fn gap(what: &str) -> Option<String> {
    Some(format!(
        "rar: {what} (named gap: libarchive 3.8.9 does not read it; see docs/FORMATS.md)"
    ))
}

fn read_at(f: &mut File, at: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    f.seek(SeekFrom::Start(at))?;
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Where the signature starts and whether it is RAR 5.
fn find_signature(f: &mut File, len: u64) -> std::io::Result<Option<(u64, bool)>> {
    const WINDOW: usize = 1 << 20;
    let mut buf = vec![0u8; WINDOW + 16];
    let mut at = 0u64;
    while at < SFX_SCAN.min(len) {
        let n = read_at(f, at, &mut buf)?;
        let w = &buf[..n];
        let limit = if n < buf.len() { n } else { WINDOW };
        for i in 0..limit {
            if w[i] != b'R' || !w[i..].starts_with(&MAGIC) {
                continue;
            }
            match w.get(i + 6..i + 8) {
                Some([0x00, _]) => return Ok(Some((at + i as u64, false))),
                Some([0x01, 0x00]) => return Ok(Some((at + i as u64, true))),
                _ => {}
            }
        }
        if n < buf.len() {
            break;
        }
        at += WINDOW as u64;
    }
    Ok(None)
}

/// The named gap of the RAR archive in `f` (its first volume), if any. `Ok(None)` also when
/// the headers cannot be walked: libarchive then reports what it finds.
pub fn named_gap(f: &mut File) -> std::io::Result<Option<String>> {
    let len = f.metadata()?.len();
    let Some((sig, v5)) = find_signature(f, len)? else {
        return Ok(None);
    };
    if v5 {
        rar5(f, sig + 8, len)
    } else {
        rar4(f, sig + 7, len)
    }
}

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn rar4(f: &mut File, mut at: u64, len: u64) -> std::io::Result<Option<String>> {
    // Base block: HEAD_CRC u16, HEAD_TYPE u8, HEAD_FLAGS u16, HEAD_SIZE u16, then ADD_SIZE u32
    // when HEAD_FLAGS has 0x8000; file headers continue with PACK_SIZE (= ADD_SIZE), UNP_SIZE,
    // HOST_OS, FILE_CRC, FTIME, UNP_VER, METHOD.
    let mut h = [0u8; 32];
    for _ in 0..MAX_BLOCKS {
        if at + 7 > len {
            return Ok(None);
        }
        let n = read_at(f, at, &mut h)?;
        let b = &h[..n];
        let (Some(flags), Some(size)) = (u16le(b, 3), u16le(b, 5)) else {
            return Ok(None);
        };
        let ty = b[2];
        if size < 7 {
            return Ok(None);
        }
        let mut add = 0u64;
        if flags & 0x8000 != 0 {
            match u32le(b, 7) {
                Some(a) => add = u64::from(a),
                None => return Ok(None),
            }
        }
        match ty {
            0x73 => {
                if flags & 0x0080 != 0 {
                    return Ok(gap("encrypted RAR 4 headers"));
                }
                if flags & 0x0008 != 0 {
                    return Ok(gap("solid RAR 4 archive"));
                }
            }
            0x74 => {
                if flags & 0x0004 != 0 {
                    return Ok(gap("encrypted RAR 4 member"));
                }
                let (Some(&unp_ver), Some(&method)) = (b.get(24), b.get(25)) else {
                    return Ok(None);
                };
                let directory = flags & 0x00E0 == 0x00E0;
                if !directory && method != 0x30 && unp_ver < 29 {
                    return Ok(gap("RAR 1.5/2.x compression"));
                }
                if flags & 0x0100 != 0 {
                    // HIGH_PACK_SIZE after NAME_SIZE and ATTR.
                    let mut hi = [0u8; 4];
                    if read_at(f, at + 32, &mut hi)? == 4 {
                        add += u64::from(u32::from_le_bytes(hi)) << 32;
                    }
                }
            }
            0x7B => return Ok(None),
            _ => {}
        }
        at = at.saturating_add(u64::from(size)).saturating_add(add);
    }
    Ok(None)
}

/// A RAR5 variable-length integer at `*pos` of `b` (7 bits a byte, low first, at most 10 bytes).
fn vint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *b.get(*pos)?;
        *pos += 1;
        if shift < 64 {
            v |= u64::from(byte & 0x7F) << shift;
        }
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn rar5(f: &mut File, mut at: u64, len: u64) -> std::io::Result<Option<String>> {
    // Block: CRC32 u32, HeaderSize vint (from HeaderType to the header's end), HeaderType vint,
    // HeaderFlags vint, [ExtraSize vint if 0x1], [DataSize vint if 0x2], type fields, extra.
    for _ in 0..MAX_BLOCKS {
        let mut pre = [0u8; 14];
        let n = read_at(f, at, &mut pre)?;
        let mut p = 4usize;
        let Some(hsize) = vint(&pre[..n], &mut p) else {
            return Ok(None);
        };
        if hsize == 0 || hsize > RAR5_MAX_HEADER || at + p as u64 + hsize > len {
            return Ok(None);
        }
        let mut h = vec![0u8; hsize as usize];
        if read_at(f, at + p as u64, &mut h)? != h.len() {
            return Ok(None);
        }
        let mut q = 0usize;
        let (Some(ty), Some(hflags)) = (vint(&h, &mut q), vint(&h, &mut q)) else {
            return Ok(None);
        };
        let extra = if hflags & 1 != 0 {
            vint(&h, &mut q)
        } else {
            Some(0)
        };
        let data = if hflags & 2 != 0 {
            vint(&h, &mut q)
        } else {
            Some(0)
        };
        let (Some(extra), Some(data)) = (extra, data) else {
            return Ok(None);
        };
        match ty {
            4 => return Ok(gap("encrypted RAR 5 headers")),
            2 => {
                if let Some(g) = rar5_file(&h, q, extra) {
                    return Ok(Some(g));
                }
            }
            5 => return Ok(None),
            _ => {}
        }
        at = at
            .saturating_add(p as u64)
            .saturating_add(hsize)
            .saturating_add(data);
    }
    Ok(None)
}

/// A RAR5 file header's gap: an encryption record in its extra area, or a dictionary above
/// [`RAR5_MAX_DICT`] for a compressed member.
fn rar5_file(h: &[u8], mut q: usize, extra: u64) -> Option<String> {
    let file_flags = vint(h, &mut q)?;
    let _unpacked = vint(h, &mut q)?;
    let _attributes = vint(h, &mut q)?;
    if file_flags & 0x2 != 0 {
        q += 4;
    }
    if file_flags & 0x4 != 0 {
        q += 4;
    }
    let comp = vint(h, &mut q)?;
    let method = (comp >> 7) & 0x7;
    let dict_log = (comp >> 10) & 0x1F;
    let directory = file_flags & 0x1 != 0;
    if !directory && method != 0 {
        let dict = 128u64 << dict_log.min(40);
        if dict > RAR5_MAX_DICT {
            return gap(&format!(
                "a RAR 5 dictionary of {} MiB (above 64 MiB)",
                dict >> 20
            ));
        }
    }
    // The extra area is the header's last `extra` bytes: records of Size vint (from Type on),
    // Type vint, data. Type 1 is file encryption.
    let start = h.len().checked_sub(usize::try_from(extra).ok()?)?;
    let area = &h[start..];
    let mut r = 0usize;
    while r < area.len() {
        let size = vint(area, &mut r)?;
        let body = r;
        let ty = vint(area, &mut r)?;
        if ty == 1 {
            return gap("encrypted RAR 5 member");
        }
        r = body.checked_add(usize::try_from(size).ok()?)?;
    }
    None
}
