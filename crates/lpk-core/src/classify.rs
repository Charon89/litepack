//! File classifier: magic bytes first, byte statistics second.
//!
//! The result routes a file (or a stream) in the pipeline. `Video` and `Compressed` are
//! additions to the planned list so that already-compressed containers skip the gate.

use crate::gate::{self, GATE_BLOCK};

/// Bytes of a file the statistics look at.
pub const SAMPLE_LEN: usize = 64 << 10;

/// Routing label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Text in a UTF-8-compatible encoding.
    Text,
    /// JPEG image.
    Jpeg,
    /// A container whose members are Deflate streams (ZIP family, gzip, PDF).
    DeflateContainer,
    /// PNG image.
    Png,
    /// Executable code (PE, ELF, Mach-O).
    Executable,
    /// Audio.
    Audio,
    /// Uncompressed raster image (BMP, TIFF, PNM, GIF).
    ImageRaw,
    /// Video container (added so it skips the gate).
    Video,
    /// A format that is already entropy coded and is not peeled (7z, xz, zstd, bzip2, RAR,
    /// WebP, AVIF/HEIC, JPEG XL); added so it skips the gate.
    Compressed,
    /// No known format and the gate says incompressible.
    HighEntropy,
    /// Anything else.
    Other,
}

impl Class {
    /// Lower-kebab name.
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Text => "text",
            Class::Jpeg => "jpeg",
            Class::DeflateContainer => "deflate-container",
            Class::Png => "png",
            Class::Executable => "executable",
            Class::Audio => "audio",
            Class::ImageRaw => "image-raw",
            Class::Video => "video",
            Class::Compressed => "compressed",
            Class::HighEntropy => "high-entropy",
            Class::Other => "other",
        }
    }
}

/// Magic rows that need no second check: offset, bytes, class.
const MAGIC: &[(usize, &[u8], Class)] = &[
    (0, &[0xFF, 0xD8, 0xFF], Class::Jpeg),
    (
        0,
        &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        Class::Png,
    ),
    (0, &[0x50, 0x4B, 0x03, 0x04], Class::DeflateContainer),
    (0, &[0x50, 0x4B, 0x05, 0x06], Class::DeflateContainer),
    (0, &[0x50, 0x4B, 0x07, 0x08], Class::DeflateContainer),
    (0, &[0x1F, 0x8B], Class::DeflateContainer),
    (0, b"%PDF-", Class::DeflateContainer),
    (0, &[0x7F, 0x45, 0x4C, 0x46], Class::Executable),
    (0, &[0xFE, 0xED, 0xFA, 0xCE], Class::Executable),
    (0, &[0xFE, 0xED, 0xFA, 0xCF], Class::Executable),
    (0, &[0xCE, 0xFA, 0xED, 0xFE], Class::Executable),
    (0, &[0xCF, 0xFA, 0xED, 0xFE], Class::Executable),
    (0, &[0xCA, 0xFE, 0xBA, 0xBE], Class::Executable),
    (0, b"fLaC", Class::Audio),
    (0, b"ID3", Class::Audio),
    (0, b"OggS", Class::Audio),
    (0, &[0x1A, 0x45, 0xDF, 0xA3], Class::Video),
    (0, &[0x49, 0x49, 0x2A, 0x00], Class::ImageRaw),
    (0, &[0x4D, 0x4D, 0x00, 0x2A], Class::ImageRaw),
    (0, b"GIF8", Class::ImageRaw),
    (0, &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C], Class::Compressed),
    (0, &[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00], Class::Compressed),
    (0, &[0x28, 0xB5, 0x2F, 0xFD], Class::Compressed),
    (0, &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07], Class::Compressed),
    (0, &[0xFF, 0x0A], Class::Compressed),
    (
        0,
        &[0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20],
        Class::Compressed,
    ),
];

fn at(b: &[u8], off: usize, pat: &[u8]) -> bool {
    b.get(off..off + pat.len()) == Some(pat)
}

/// Rows that need a second check, or whose position depends on the format.
fn magic_special(b: &[u8]) -> Option<Class> {
    if at(b, 0, b"MZ") {
        // A plain MZ without the PE signature is not an executable. A slice too short to hold
        // e_lfanew, or whose signature lies beyond it, cannot be checked and is accepted.
        let Some(p) = b.get(0x3C..0x40) else {
            return Some(Class::Executable);
        };
        let off = u32::from_le_bytes([p[0], p[1], p[2], p[3]]) as usize;
        return match b.get(off..off.saturating_add(4)) {
            Some(sig) if sig == b"PE\0\0" => Some(Class::Executable),
            Some(_) => None,
            None if b.len() < SAMPLE_LEN => None,
            None => Some(Class::Executable),
        };
    }
    if at(b, 0, b"RIFF") && b.len() >= 12 {
        return match &b[8..12] {
            b"WAVE" => Some(Class::Audio),
            b"AVI " => Some(Class::Video),
            b"WEBP" => Some(Class::Compressed),
            _ => None,
        };
    }
    if at(b, 4, b"ftyp") && b.len() >= 12 {
        return Some(match &b[8..12] {
            b"M4A " => Class::Audio,
            b"avif" | b"heic" | b"mif1" => Class::Compressed,
            _ => Class::Video,
        });
    }
    if at(b, 0, b"BZh") && b.get(3).is_some_and(|d| (b'1'..=b'9').contains(d)) {
        return Some(Class::Compressed);
    }
    // BMP: the two reserved fields at 6..10 are zero.
    if at(b, 0, b"BM") && b.len() >= 14 && b[6..10] == [0, 0, 0, 0] {
        return Some(Class::ImageRaw);
    }
    if b.len() >= 3
        && b[0] == b'P'
        && (b'1'..=b'6').contains(&b[1])
        && matches!(b[2], b' ' | b'\t' | b'\n' | b'\r')
    {
        return Some(Class::ImageRaw);
    }
    // MPEG audio frame sync with plausible header bits.
    if b.len() >= 3 && b[0] == 0xFF && b[1] & 0xE0 == 0xE0 {
        let version_ok = (b[1] >> 3) & 3 != 1;
        // Layer I (bits 11) is excluded: it is rare and FF FE is the UTF-16LE byte order mark.
        let layer_ok = matches!((b[1] >> 1) & 3, 1 | 2);
        let bitrate_ok = b[2] >> 4 != 0xF && b[2] >> 4 != 0;
        let rate_ok = (b[2] >> 2) & 3 != 3;
        if version_ok && layer_ok && bitrate_ok && rate_ok {
            return Some(Class::Audio);
        }
    }
    None
}

/// Byte statistics of a sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    /// Bytes examined.
    pub len: usize,
    /// Order-0 Shannon entropy, bits per byte.
    pub entropy: f64,
    /// Share of bytes in `0x20..=0x7E` plus tab, line feed and carriage return.
    pub printable_ratio: f64,
    /// Share of NUL bytes.
    pub nul_ratio: f64,
    /// Share of bytes in `0x00..=0x08`, `0x0E..=0x1F` and `0x7F`.
    pub control_ratio: f64,
    /// Valid UTF-8, allowing one truncated sequence at the end.
    pub utf8_valid: bool,
    /// Share of line feeds.
    pub newline_ratio: f64,
    /// Share of the most frequent byte.
    pub max_byte_share: f64,
}

impl Features {
    /// Statistics of exactly the slice given (callers pass the sample).
    pub fn of(b: &[u8]) -> Features {
        let h = gate::histogram(b);
        let n = b.len().max(1) as f64;
        let share = |idx: &[usize]| idx.iter().map(|&i| h[i]).sum::<u64>() as f64 / n;
        let control: Vec<usize> = (0x00..=0x08).chain(0x0E..=0x1F).chain([0x7F]).collect();
        let printable: Vec<usize> = (0x20..=0x7E).chain([0x09, 0x0A, 0x0D]).collect();
        let utf8_valid = match std::str::from_utf8(b) {
            Ok(_) => true,
            Err(e) => e.error_len().is_none(),
        };
        Features {
            len: b.len(),
            entropy: gate::entropy(b),
            printable_ratio: share(&printable),
            nul_ratio: h[0] as f64 / n,
            control_ratio: share(&control),
            utf8_valid,
            newline_ratio: h[0x0A] as f64 / n,
            max_byte_share: h.iter().copied().max().unwrap_or(0) as f64 / n,
        }
    }
}

/// Classify a file from its leading bytes (pass up to the first 1 MiB; more is ignored).
pub fn classify(bytes: &[u8]) -> Class {
    if bytes.is_empty() {
        return Class::Other;
    }
    for &(off, pat, class) in MAGIC {
        if at(bytes, off, pat) {
            return class;
        }
    }
    if let Some(c) = magic_special(bytes) {
        return c;
    }
    // UTF-16 (BOM FF FE or FE FF) has NULs or control bytes and so is Other for now; the
    // text-encodings task adds it.
    let sample = &bytes[..bytes.len().min(SAMPLE_LEN)];
    let f = Features::of(sample);
    if f.nul_ratio == 0.0 && f.utf8_valid && f.control_ratio < 0.005 {
        return Class::Text;
    }
    if gate::is_incompressible(&bytes[..bytes.len().min(GATE_BLOCK)]) {
        Class::HighEntropy
    } else {
        Class::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::tests::xorshift;

    fn head(prefix: &[u8], len: usize) -> Vec<u8> {
        let mut v = prefix.to_vec();
        v.resize(len, 0x11);
        v
    }

    fn at_offset(off: usize, tag: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; off];
        v.extend_from_slice(tag);
        v.resize(64, 0);
        v
    }

    #[test]
    fn simple_magic_rows() {
        let rows: &[(&[u8], Class)] = &[
            (&[0xFF, 0xD8, 0xFF, 0xE0], Class::Jpeg),
            (
                &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
                Class::Png,
            ),
            (b"PK\x03\x04", Class::DeflateContainer),
            (b"PK\x05\x06", Class::DeflateContainer),
            (b"PK\x07\x08", Class::DeflateContainer),
            (&[0x1F, 0x8B, 8], Class::DeflateContainer),
            (b"%PDF-1.7", Class::DeflateContainer),
            (b"\x7FELF\x02", Class::Executable),
            (&[0xFE, 0xED, 0xFA, 0xCE], Class::Executable),
            (&[0xFE, 0xED, 0xFA, 0xCF], Class::Executable),
            (&[0xCF, 0xFA, 0xED, 0xFE], Class::Executable),
            (&[0xCA, 0xFE, 0xBA, 0xBE], Class::Executable),
            (b"fLaC\0", Class::Audio),
            (b"ID3\x04", Class::Audio),
            (b"OggS\0", Class::Audio),
            (&[0x1A, 0x45, 0xDF, 0xA3], Class::Video),
            (&[0x49, 0x49, 0x2A, 0x00], Class::ImageRaw),
            (&[0x4D, 0x4D, 0x00, 0x2A], Class::ImageRaw),
            (b"GIF89a", Class::ImageRaw),
            (&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C], Class::Compressed),
            (&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00], Class::Compressed),
            (&[0x28, 0xB5, 0x2F, 0xFD], Class::Compressed),
            (b"Rar!\x1A\x07\x00", Class::Compressed),
            (&[0xFF, 0x0A], Class::Compressed),
            (
                &[0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A],
                Class::Compressed,
            ),
        ];
        for (prefix, want) in rows {
            assert_eq!(classify(&head(prefix, 200)), *want, "{prefix:02X?}");
        }
    }

    #[test]
    fn special_rows() {
        // PE with and without the signature.
        let mut pe = vec![0u8; 256];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3C] = 0x80;
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        assert_eq!(classify(&pe), Class::Executable);
        pe[0x80] = b'X';
        assert_eq!(classify(&pe), Class::Other);
        assert_eq!(classify(b"MZ"), Class::Executable);
        // RIFF.
        let riff = |t: &[u8]| {
            let mut v = b"RIFF\x24\0\0\0".to_vec();
            v.extend_from_slice(t);
            v.resize(64, 0);
            v
        };
        assert_eq!(classify(&riff(b"WAVEfmt ")), Class::Audio);
        assert_eq!(classify(&riff(b"AVI LIST")), Class::Video);
        assert_eq!(classify(&riff(b"WEBPVP8 ")), Class::Compressed);
        assert_eq!(classify(&riff(b"ACONanih")), Class::Other);
        // ftyp brands.
        assert_eq!(classify(&at_offset(4, b"ftypM4A ")), Class::Audio);
        assert_eq!(classify(&at_offset(4, b"ftypisom")), Class::Video);
        assert_eq!(classify(&at_offset(4, b"ftypqt  ")), Class::Video);
        for brand in [b"avif", b"heic", b"mif1"] {
            let mut tag = b"ftyp".to_vec();
            tag.extend_from_slice(brand);
            assert_eq!(classify(&at_offset(4, &tag)), Class::Compressed);
        }
        // bzip2, BMP, PNM, MP3 sync.
        assert_eq!(classify(b"BZh91AY&SY"), Class::Compressed);
        let mut bmp = vec![0u8; 64];
        bmp[..2].copy_from_slice(b"BM");
        assert_eq!(classify(&bmp), Class::ImageRaw);
        for p in 1..=6u8 {
            assert_eq!(
                classify(&[b'P', b'0' + p, b'\n', b'1', b' ', b'2']),
                Class::ImageRaw
            );
        }
        assert_eq!(classify(b"P7\n1 2"), Class::Text);
        assert_eq!(
            classify(&head(&[0xFF, 0xFB, 0x90, 0x64], 100)),
            Class::Audio
        );
        assert_eq!(
            classify(&head(&[0xFF, 0xE3, 0x18, 0xC4], 100)),
            Class::Audio
        );
        // Invalid header bits are not MP3 (bitrate index 15).
        assert_ne!(
            classify(&head(&[0xFF, 0xFB, 0xF0, 0x00], 100)),
            Class::Audio
        );
    }

    #[test]
    fn statistics() {
        let prose = "The quick brown fox jumps over the lazy dog.\n".repeat(500);
        assert_eq!(classify(prose.as_bytes()), Class::Text);
        // A multi-byte sequence cut at the end.
        let mut u = "caf\u{e9} na\u{ef}ve\n".repeat(100).into_bytes();
        u.push(0xE2);
        u.push(0x82);
        assert_eq!(classify(&u), Class::Text);
        // A NUL inside prose.
        let mut n = prose.clone().into_bytes();
        n[100] = 0;
        assert_ne!(classify(&n), Class::Text);
        // Invalid UTF-8 in the middle.
        let mut bad = prose.into_bytes();
        bad[100] = 0xFF;
        assert_ne!(classify(&bad), Class::Text);
        assert_eq!(classify(&xorshift(9, SAMPLE_LEN)), Class::HighEntropy);
        assert_eq!(classify(&vec![0u8; SAMPLE_LEN]), Class::Other);
        assert_eq!(classify(&[]), Class::Other);
        // UTF-16 with a BOM is Other for now.
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("hello world".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(classify(&utf16), Class::Other);
    }

    #[test]
    fn features_on_a_hand_built_slice() {
        // 4 NUL, 2 control (0x01, 0x7F), 3 line feeds, 1 tab, 10 letters = 20 bytes.
        let mut b = vec![0u8; 4];
        b.extend_from_slice(&[0x01, 0x7F, b'\n', b'\n', b'\n', b'\t']);
        b.extend_from_slice(b"abcdefghij");
        let f = Features::of(&b);
        assert_eq!(f.len, 20);
        assert_eq!(f.nul_ratio, 0.2);
        assert_eq!(f.control_ratio, 0.3);
        assert_eq!(f.newline_ratio, 0.15);
        assert_eq!(f.printable_ratio, 0.7);
        assert_eq!(f.max_byte_share, 0.2);
        assert!(f.utf8_valid);
        assert!(f.entropy > 2.0 && f.entropy < 4.0);
        assert!(!Features::of(&[0xC3, b'a']).utf8_valid);
        assert!(Features::of(&[b'a', 0xC3]).utf8_valid);
        let empty = Features::of(&[]);
        assert_eq!(
            (empty.len, empty.entropy, empty.max_byte_share),
            (0, 0.0, 0.0)
        );
    }

    #[test]
    fn names_are_lower_kebab() {
        assert_eq!(Class::DeflateContainer.as_str(), "deflate-container");
        assert_eq!(Class::ImageRaw.as_str(), "image-raw");
        assert_eq!(Class::HighEntropy.as_str(), "high-entropy");
    }
}
