//! UTF-16 texts (design `docs/notes/e2-17-design.md`, sections 1 and 2): detection (a byte
//! order mark, or alternating zero bytes), and the conversion between UTF-16 and generalized
//! UTF-8 — UTF-8's shortest forms of every code point up to U+10FFFF with the surrogates
//! U+D800–U+DFFF allowed as three-byte forms (WTF-8, cited for information). Every sequence
//! of 16-bit units maps to exactly one byte string and back: a high surrogate followed by a
//! low one is one supplementary code point (a four-byte form), any other surrogate is lone
//! (a three-byte form). An odd last byte is not part of the text: the caller keeps it as
//! framing ([`convert`] returns it).
//!
//! The frozen [`Utf16Record`] keeps `endian`, `bom` and the text's length and hash; `bom` 1
//! puts U+FEFF first in the record's byte order, counted in `original_len`.

use lpk_format::Utf16Record;

/// Fewest units of a run counted inside a binary ([`find_binary_runs`]).
pub const MIN_BINARY_RUN_UNITS: usize = 32;

/// Bytes the statistical detection examines at most.
const DETECT_SAMPLE: usize = 1 << 16;

/// Byte order: the record's `endian`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endian {
    /// Little endian (0).
    Little,
    /// Big endian (1).
    Big,
}

impl Endian {
    /// Both.
    pub const ALL: [Endian; 2] = [Endian::Little, Endian::Big];

    /// The record's `endian`.
    pub fn code(self) -> u8 {
        match self {
            Endian::Little => 0,
            Endian::Big => 1,
        }
    }

    /// The byte order of a record's `endian`.
    pub fn from_code(c: u8) -> Option<Endian> {
        match c {
            0 => Some(Endian::Little),
            1 => Some(Endian::Big),
            _ => None,
        }
    }

    /// Lower-kebab name.
    pub fn label(self) -> &'static str {
        match self {
            Endian::Little => "le",
            Endian::Big => "be",
        }
    }

    fn unit(self, b: &[u8]) -> u16 {
        match self {
            Endian::Little => u16::from_le_bytes([b[0], b[1]]),
            Endian::Big => u16::from_be_bytes([b[0], b[1]]),
        }
    }

    fn bytes(self, u: u16) -> [u8; 2] {
        match self {
            Endian::Little => u.to_le_bytes(),
            Endian::Big => u.to_be_bytes(),
        }
    }
}

/// What the detection found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    /// The byte order.
    pub endian: Endian,
    /// A byte order mark starts the text.
    pub bom: bool,
}

/// Whether `bytes` is a UTF-16 text: a byte order mark decides; otherwise, over the first
/// [`DETECT_SAMPLE`] bytes, at least half the bytes of one parity are zero and at most one
/// in fifty of the other (ASCII-heavy text in UTF-16; a text with few zero bytes, such as
/// CJK without a mark, is not detected). Fewer than four bytes is never a text.
pub fn detect(bytes: &[u8]) -> Option<Detected> {
    if bytes.len() < 4 {
        return None;
    }
    match bytes[..2] {
        [0xFF, 0xFE] => {
            return Some(Detected {
                endian: Endian::Little,
                bom: true,
            })
        }
        [0xFE, 0xFF] => {
            return Some(Detected {
                endian: Endian::Big,
                bom: true,
            })
        }
        _ => {}
    }
    let sample = &bytes[..bytes.len().min(DETECT_SAMPLE) & !1];
    let units = sample.len() / 2;
    let (mut even_zero, mut odd_zero) = (0usize, 0usize);
    for pair in sample.as_chunks::<2>().0 {
        even_zero += usize::from(pair[0] == 0);
        odd_zero += usize::from(pair[1] == 0);
    }
    let many = |z: usize| z * 2 >= units;
    let few = |z: usize| z * 50 <= units;
    if many(odd_zero) && few(even_zero) {
        Some(Detected {
            endian: Endian::Little,
            bom: false,
        })
    } else if many(even_zero) && few(odd_zero) {
        Some(Detected {
            endian: Endian::Big,
            bom: false,
        })
    } else {
        None
    }
}

/// The UTF-16 runs inside a binary, for counting only: maximal runs of at least
/// [`MIN_BINARY_RUN_UNITS`] units that are each a printable ASCII byte (or tab, LF, CR)
/// paired with a zero, in either byte order, as `(offset, length in bytes)`. The scan tries
/// both alignments and both orders and keeps the runs in order, without overlaps.
pub fn find_binary_runs(bytes: &[u8]) -> Vec<(usize, usize)> {
    let printable = |b: u8| (0x20..=0x7E).contains(&b) || matches!(b, 0x09 | 0x0A | 0x0D);
    let mut out = Vec::new();
    for align in 0..2usize {
        for endian in Endian::ALL {
            let mut i = align;
            while i + 1 < bytes.len() {
                let is_unit = |j: usize| {
                    let (lo, hi) = match endian {
                        Endian::Little => (bytes[j], bytes[j + 1]),
                        Endian::Big => (bytes[j + 1], bytes[j]),
                    };
                    hi == 0 && printable(lo)
                };
                if !is_unit(i) {
                    i += 2;
                    continue;
                }
                let start = i;
                while i + 1 < bytes.len() && is_unit(i) {
                    i += 2;
                }
                if (i - start) / 2 >= MIN_BINARY_RUN_UNITS {
                    out.push((start, i - start));
                }
            }
        }
    }
    // Both orders see a run of the other with one unit less: keep the first of overlapping runs.
    out.sort_unstable();
    let mut kept: Vec<(usize, usize)> = Vec::with_capacity(out.len());
    for r in out {
        if kept.last().is_none_or(|k| k.0 + k.1 <= r.0) {
            kept.push(r);
        }
    }
    kept
}

/// The units of an even-length `bytes` in `endian`.
pub fn units(bytes: &[u8], endian: Endian) -> impl Iterator<Item = u16> + '_ {
    bytes.as_chunks::<2>().0.iter().map(move |p| endian.unit(p))
}

/// Generalized UTF-8 of a unit sequence.
pub fn to_wtf8(units: impl IntoIterator<Item = u16>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut it = units.into_iter().peekable();
    while let Some(u) = it.next() {
        let cp = match u {
            0xD800..=0xDBFF => match it.peek() {
                Some(&lo @ 0xDC00..=0xDFFF) => {
                    it.next();
                    0x1_0000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(lo) - 0xDC00)
                }
                _ => u32::from(u),
            },
            _ => u32::from(u),
        };
        push_cp(&mut out, cp);
    }
    out
}

fn push_cp(out: &mut Vec<u8>, cp: u32) {
    match cp {
        0..=0x7F => out.push(cp as u8),
        0x80..=0x7FF => out.extend_from_slice(&[0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8]),
        0x800..=0xFFFF => out.extend_from_slice(&[
            0xE0 | (cp >> 12) as u8,
            0x80 | ((cp >> 6) & 0x3F) as u8,
            0x80 | (cp & 0x3F) as u8,
        ]),
        _ => out.extend_from_slice(&[
            0xF0 | (cp >> 18) as u8,
            0x80 | ((cp >> 12) & 0x3F) as u8,
            0x80 | ((cp >> 6) & 0x3F) as u8,
            0x80 | (cp & 0x3F) as u8,
        ]),
    }
}

/// Why a byte string is not generalized UTF-8 (the decoder's `BadRecord` `input` reasons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wtf8Error {
    /// Where the sequence starts.
    pub offset: usize,
    /// The reason.
    pub reason: &'static str,
}

impl std::fmt::Display for Wtf8Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.reason, self.offset)
    }
}

/// The units of a generalized UTF-8 byte string: the shortest forms only (RFC 3629's table
/// plus the three-byte surrogates), a code point above U+FFFF as a pair.
pub fn from_wtf8(bytes: &[u8]) -> Result<Vec<u16>, Wtf8Error> {
    let mut out = Vec::with_capacity(bytes.len() / 2 + 1);
    let mut i = 0usize;
    while i < bytes.len() {
        let b0 = bytes[i];
        let err = |reason| Wtf8Error { offset: i, reason };
        let cont = |k: usize| -> Result<u32, Wtf8Error> {
            match bytes.get(i + k) {
                None => Err(Wtf8Error {
                    offset: i,
                    reason: "input ends inside a sequence",
                }),
                Some(&c) if c & 0xC0 == 0x80 => Ok(u32::from(c & 0x3F)),
                Some(_) => Err(Wtf8Error {
                    offset: i,
                    reason: "bad continuation byte",
                }),
            }
        };
        let (cp, len) = match b0 {
            0x00..=0x7F => (u32::from(b0), 1),
            0xC2..=0xDF => ((u32::from(b0 & 0x1F) << 6) | cont(1)?, 2),
            0xE0..=0xEF => {
                let c1 = cont(1)?;
                // E0 needs A0..BF (else overlong); ED takes 80..BF (surrogates allowed).
                if b0 == 0xE0 && c1 < 0x20 {
                    return Err(err("overlong form"));
                }
                let cp = (u32::from(b0 & 0x0F) << 12) | (c1 << 6) | cont(2)?;
                (cp, 3)
            }
            0xF0..=0xF4 => {
                let c1 = cont(1)?;
                if b0 == 0xF0 && c1 < 0x10 {
                    return Err(err("overlong form"));
                }
                if b0 == 0xF4 && c1 >= 0x10 {
                    return Err(err("code point above U+10FFFF"));
                }
                let cp = (u32::from(b0 & 0x07) << 18) | (c1 << 12) | (cont(2)? << 6) | cont(3)?;
                (cp, 4)
            }
            0x80..=0xBF => return Err(err("continuation byte starts a sequence")),
            0xC0 | 0xC1 => return Err(err("overlong form")),
            _ => return Err(err("code point above U+10FFFF")),
        };
        if cp > 0xFFFF {
            let v = cp - 0x1_0000;
            out.push(0xD800 | (v >> 10) as u16);
            out.push(0xDC00 | (v & 0x3FF) as u16);
        } else {
            out.push(cp as u16);
        }
        i += len;
    }
    Ok(out)
}

/// The bytes of `units` in `endian`.
pub fn to_utf16(units: &[u16], endian: Endian) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len() * 2);
    for &u in units {
        out.extend_from_slice(&endian.bytes(u));
    }
    out
}

/// A UTF-16 text converted: its record and its generalized UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converted {
    /// The record of the even-length text (the odd last byte, if any, excluded).
    pub record: Utf16Record,
    /// The generalized UTF-8 (the byte order mark included when `bom`).
    pub wtf8: Vec<u8>,
    /// An odd last byte, left as framing.
    pub tail: Option<u8>,
    /// Code points of the text (surrogate pairs once, lone surrogates once each).
    pub code_points: u64,
    /// Of those, in the CJK ranges (U+2E80–U+9FFF, U+AC00–U+D7AF, U+F900–U+FAFF,
    /// U+FF00–U+FFEF, U+20000–U+3134F): their UTF-8 is longer than their UTF-16.
    pub cjk_code_points: u64,
}

/// Convert a detected UTF-16 text: the text is the even-length prefix of `bytes` (an odd last
/// byte stays framing), converted in `d.endian`; the record states `d.bom`. The conversion
/// is checked back to the bytes before it is returned.
pub fn convert(bytes: &[u8], d: Detected) -> Converted {
    let (text, tail) = if bytes.len() % 2 == 1 {
        (&bytes[..bytes.len() - 1], bytes.last().copied())
    } else {
        (bytes, None)
    };
    let us: Vec<u16> = units(text, d.endian).collect();
    let wtf8 = to_wtf8(us.iter().copied());
    let (mut code_points, mut cjk) = (0u64, 0u64);
    let mut i = 0usize;
    while i < us.len() {
        let u = us[i];
        let cp = match (u, us.get(i + 1)) {
            (0xD800..=0xDBFF, Some(&lo @ 0xDC00..=0xDFFF)) => {
                i += 1;
                0x1_0000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(lo) - 0xDC00)
            }
            _ => u32::from(u),
        };
        i += 1;
        code_points += 1;
        cjk += u64::from(matches!(
            cp,
            0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x2_0000..=0x3_134F
        ));
    }
    debug_assert_eq!(
        from_wtf8(&wtf8)
            .map(|u| to_utf16(&u, d.endian))
            .ok()
            .as_deref(),
        Some(text)
    );
    Converted {
        record: Utf16Record {
            endian: d.endian.code(),
            bom: u8::from(d.bom),
            original_len: text.len() as u64,
            original_hash: *blake3::hash(text).as_bytes(),
        },
        wtf8,
        tail,
        code_points,
        cjk_code_points: cjk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut x = seed | 1;
        move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        }
    }

    /// Random units: a mix of ASCII, BMP, lone and paired surrogates.
    fn random_units(next: &mut impl FnMut() -> u64, n: usize) -> Vec<u16> {
        let mut out = Vec::with_capacity(n + 1);
        while out.len() < n {
            match next() % 8 {
                0..=2 => out.push((next() % 0x80) as u16),
                3 => out.push((next() % 0x800) as u16),
                4 => out.push((next() % 0x1_0000) as u16),
                5 => out.push(0xD800 + (next() % 0x400) as u16),
                6 => out.push(0xDC00 + (next() % 0x400) as u16),
                _ => {
                    out.push(0xD800 + (next() % 0x400) as u16);
                    out.push(0xDC00 + (next() % 0x400) as u16);
                }
            }
        }
        out
    }

    /// What `std` says: well-formed text equals `String::from_utf16`; otherwise each unpaired
    /// unit reported by `char::decode_utf16` is a three-byte form.
    fn std_wtf8(units: &[u16]) -> Vec<u8> {
        if let Ok(s) = String::from_utf16(units) {
            return s.into_bytes();
        }
        let mut out = Vec::new();
        for r in char::decode_utf16(units.iter().copied()) {
            match r {
                Ok(c) => {
                    let mut b = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                }
                Err(e) => {
                    let u = u32::from(e.unpaired_surrogate());
                    out.extend_from_slice(&[
                        0xE0 | (u >> 12) as u8,
                        0x80 | ((u >> 6) & 0x3F) as u8,
                        0x80 | (u & 0x3F) as u8,
                    ]);
                }
            }
        }
        out
    }

    #[test]
    fn random_unit_sequences_round_trip_and_agree_with_std() {
        let mut next = rng(0x5EED);
        let mut lone = 0usize;
        for round in 0..400usize {
            let units = random_units(&mut next, round % 97);
            let w = to_wtf8(units.iter().copied());
            assert_eq!(w, std_wtf8(&units), "round {round}: {units:04X?}");
            assert_eq!(from_wtf8(&w).unwrap(), units, "round {round}");
            lone += usize::from(String::from_utf16(&units).is_err());
            for endian in Endian::ALL {
                for bom in [false, true] {
                    let mut text = Vec::new();
                    if bom {
                        text.extend_from_slice(&endian.bytes(0xFEFF));
                    }
                    text.extend_from_slice(&to_utf16(&units, endian));
                    // An odd last byte on half the rounds.
                    let odd = round % 2 == 1;
                    if odd {
                        text.push(0x41);
                    }
                    let d = Detected { endian, bom };
                    let c = convert(&text, d);
                    assert_eq!(c.tail, odd.then_some(0x41));
                    assert_eq!(c.record.endian, endian.code());
                    assert_eq!(c.record.bom, u8::from(bom));
                    let even = &text[..text.len() & !1];
                    assert_eq!(c.record.original_len, even.len() as u64);
                    assert!(c.record.verify_hash(even));
                    if bom {
                        assert!(c.wtf8.starts_with(&[0xEF, 0xBB, 0xBF]));
                        assert_eq!(&c.wtf8[3..], &w[..]);
                    } else {
                        assert_eq!(c.wtf8, w);
                    }
                    // Back, from the record's fields.
                    let e = Endian::from_code(c.record.endian).unwrap();
                    let back = to_utf16(&from_wtf8(&c.wtf8).unwrap(), e);
                    assert_eq!(back, even);
                    // With a mark the detection needs no statistics.
                    if bom && !units.is_empty() {
                        assert_eq!(detect(&text), Some(d));
                    }
                }
            }
        }
        assert!(
            lone > 50,
            "the generator must produce lone surrogates ({lone})"
        );
    }

    #[test]
    fn code_points_and_cjk_are_counted() {
        let s = "abc 漢字 😀 한";
        let units: Vec<u16> = s.encode_utf16().collect();
        let c = convert(
            &to_utf16(&units, Endian::Little),
            Detected {
                endian: Endian::Little,
                bom: false,
            },
        );
        assert_eq!(c.code_points, s.chars().count() as u64);
        assert_eq!(c.cjk_code_points, 3);
        assert_eq!(c.wtf8, s.as_bytes());
        // A lone surrogate counts once and is not CJK.
        let c = convert(
            &to_utf16(&[0x41, 0xDC00, 0xD800], Endian::Big),
            Detected {
                endian: Endian::Big,
                bom: false,
            },
        );
        assert_eq!((c.code_points, c.cjk_code_points), (3, 0));
    }

    #[test]
    fn bad_generalized_utf8_is_refused_with_a_reason() {
        let bad: [(&[u8], &str); 9] = [
            (&[0x80], "continuation byte starts a sequence"),
            (&[0xC0, 0x80], "overlong form"),
            (&[0xC1, 0xBF], "overlong form"),
            (&[0xE0, 0x80, 0x80], "overlong form"),
            (&[0xF0, 0x80, 0x80, 0x80], "overlong form"),
            (&[0xF4, 0x90, 0x80, 0x80], "code point above U+10FFFF"),
            (&[0xF5, 0x80, 0x80, 0x80], "code point above U+10FFFF"),
            (&[0xE2, 0x82], "input ends inside a sequence"),
            (&[0xE2, 0x41, 0x82], "bad continuation byte"),
        ];
        for (b, reason) in bad {
            let e = from_wtf8(b).unwrap_err();
            assert_eq!(e.reason, reason, "{b:02X?}");
            assert_eq!(e.offset, 0);
            assert!(e.to_string().contains(reason));
        }
        // The surrogates' three-byte forms are accepted; U+10FFFF is the last code point.
        assert_eq!(from_wtf8(&[0xED, 0xA0, 0x80]).unwrap(), [0xD800]);
        assert_eq!(from_wtf8(&[0xED, 0xBF, 0xBF]).unwrap(), [0xDFFF]);
        assert_eq!(
            from_wtf8(&[0xF4, 0x8F, 0xBF, 0xBF]).unwrap(),
            [0xDBFF, 0xDFFF]
        );
        // An error after a good sequence carries its offset.
        assert_eq!(from_wtf8(&[0x41, 0xC3, 0xA9, 0xFF]).unwrap_err().offset, 3);
    }

    #[test]
    fn detection_uses_the_mark_or_the_zero_pattern() {
        let ascii = "Windows Registry Editor Version 5.00\r\n\r\n[HKEY_LOCAL_MACHINE]\r\n";
        let units: Vec<u16> = ascii.encode_utf16().collect();
        let le = to_utf16(&units, Endian::Little);
        let be = to_utf16(&units, Endian::Big);
        assert_eq!(
            detect(&le),
            Some(Detected {
                endian: Endian::Little,
                bom: false
            })
        );
        assert_eq!(
            detect(&be),
            Some(Detected {
                endian: Endian::Big,
                bom: false
            })
        );
        let mut with_bom = vec![0xFF, 0xFE];
        with_bom.extend_from_slice(&le);
        assert_eq!(
            detect(&with_bom),
            Some(Detected {
                endian: Endian::Little,
                bom: true
            })
        );
        let mut with_bom = vec![0xFE, 0xFF];
        with_bom.extend_from_slice(&be);
        assert_eq!(
            detect(&with_bom),
            Some(Detected {
                endian: Endian::Big,
                bom: true
            })
        );
        // Plain ASCII, binary noise, CJK without a mark, and short inputs are not texts.
        assert_eq!(detect(ascii.as_bytes()), None);
        let noise: Vec<u8> = (0..4000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        assert_eq!(detect(&noise), None);
        let cjk: Vec<u16> = "漢字漢字漢字漢字漢字".encode_utf16().collect();
        assert_eq!(detect(&to_utf16(&cjk, Endian::Little)), None);
        assert_eq!(detect(&[0xFF, 0xFE, 0x41]), None);
        assert_eq!(detect(&[]), None);
        // Zeros on both parities (a binary) are not a text.
        assert_eq!(detect(&[0u8; 4096]), None);
    }

    #[test]
    fn binary_runs_are_counted_in_both_orders_and_alignments() {
        let text: Vec<u16> = "a".repeat(40).encode_utf16().collect();
        let mut bin = vec![0x7Fu8, 0x45, 0x4C, 0x46, 0x01];
        bin.extend_from_slice(&to_utf16(&text, Endian::Little));
        bin.extend_from_slice(&[0xDE, 0xAD]);
        bin.extend_from_slice(&to_utf16(&text, Endian::Big));
        bin.extend_from_slice(&[0xBE]);
        let runs = find_binary_runs(&bin);
        assert_eq!(runs, [(5, 80), (87, 80)]);
        // Below the minimum: none.
        let short: Vec<u16> = "a".repeat(31).encode_utf16().collect();
        assert!(find_binary_runs(&to_utf16(&short, Endian::Little)).is_empty());
        assert!(find_binary_runs(&[]).is_empty());
    }
}
