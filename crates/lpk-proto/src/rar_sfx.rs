//! RAR signature validation shared by the host's detection and the libarchive worker's pre-check (E4-4, R208),
//! and the one definition of where libarchive looks for a RAR archive inside a self-extracting file.

/// RAR 4 and RAR 5 share these six bytes (RAR 5 continues `07 01 00`, RAR 4 `07 00`).
pub const RAR_MAGIC: [u8; 6] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07];

/// Where libarchive 3.8.11 finds a RAR signature in a file that starts with `MZ` or `\x7FELF`: from offset
/// 0x10000, every 16 bytes, up to `SFX_MAX_READAHEAD` = 512 KiB
/// (`vendor/libarchive-3.8.11/libarchive/archive_read_support_format_rar.c:806-826` and `:139`,
/// `archive_read_support_format_rar5.c:1161-1203` and `:166`). A file that starts with the signature itself
/// is read from 0.
pub const SFX_START: u64 = 0x10000;
/// The step of that scan.
pub const SFX_ALIGN: u64 = 16;
/// The end of the readahead; a signature must lie wholly below it.
pub const SFX_END: u64 = 512 * 1024;

/// Whether libarchive would find a signature at `offset` of a file: at 0, or inside the scan window and aligned.
pub fn libarchive_finds(offset: u64) -> bool {
    offset == 0
        || (offset >= SFX_START
            && offset.is_multiple_of(SFX_ALIGN)
            && offset.saturating_add(8) <= SFX_END)
}

/// CRC-32 (IEEE), bitwise: only header-sized inputs are ever checked here.
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

/// A RAR 4 or RAR 5 signature at `at` in `buf`.
pub fn rar_at(buf: &[u8], at: usize) -> bool {
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
