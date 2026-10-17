//! Base64 runs (design `docs/notes/e2-17-design.md`, section 1): finding them in a text,
//! labelling them by what precedes them, the regularity rule with every irregular cause, and
//! the decode and re-encode of a regular run from exactly the fields of the frozen
//! [`Base64Record`] (`variant`, `line_len`, `line_ending`, `padding`).
//!
//! A candidate is a maximal run of at least [`MIN_RUN`] characters over one RFC 4648 alphabet.
//! The scan is line-oriented and linear: inside a line, a segment is a maximal span of
//! characters of one alphabet (the 62 shared symbols, `=`, and either `+`/`/` or `-`/`_`);
//! segments on consecutive lines join into one run when each inner segment spans its whole
//! line (the first may start inside its line, the last may end inside its line). A line whose
//! alphabet differs from the run's ends the run; when that line is a whole-line segment, the
//! run's trailing lines that carry no alphabet-specific symbol move to the new run (so a PEM
//! block's first line, which follows `-----BEGIN ...-----`, is not lost to the header's `-`).
//!
//! A run is **regular** when every line but the last has the same length, one line ending (LF
//! or CRLF) separates the lines and none follows the last line, `=` appears only as final
//! padding, and the unused bits of the last quantum are zero. Anything else is an
//! [`Irregular`] cause.

use lpk_format::Base64Record;

/// Fewest characters (line endings not counted) of a candidate run.
pub const MIN_RUN: usize = 256;

/// The longest line the record can state.
pub const MAX_LINE_LEN: usize = u16::MAX as usize;

/// How far back the label looks for a MIME header.
const MIME_WINDOW: usize = 2048;

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// An RFC 4648 alphabet: the record's `variant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Alphabet {
    /// Section 4: `+` and `/` (variant 0).
    Standard,
    /// Section 5: `-` and `_` (variant 1).
    UrlSafe,
}

impl Alphabet {
    /// Both, in variant order.
    pub const ALL: [Alphabet; 2] = [Alphabet::Standard, Alphabet::UrlSafe];

    /// The record's `variant`.
    pub fn variant(self) -> u8 {
        match self {
            Alphabet::Standard => 0,
            Alphabet::UrlSafe => 1,
        }
    }

    /// The alphabet of a record's `variant`.
    pub fn from_variant(v: u8) -> Option<Alphabet> {
        match v {
            0 => Some(Alphabet::Standard),
            1 => Some(Alphabet::UrlSafe),
            _ => None,
        }
    }

    /// Lower-kebab name.
    pub fn label(self) -> &'static str {
        match self {
            Alphabet::Standard => "standard",
            Alphabet::UrlSafe => "url-safe",
        }
    }

    fn symbols(self) -> &'static [u8; 64] {
        match self {
            Alphabet::Standard => STANDARD,
            Alphabet::UrlSafe => URL_SAFE,
        }
    }

    /// The value of `b` in this alphabet.
    fn value(self, b: u8) -> Option<u8> {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' if self.specific(b) => 62,
            b'/' | b'_' if self.specific(b) => 63,
            _ => return None,
        };
        Some(v)
    }

    /// True for the two symbols that belong to this alphabet only.
    fn specific(self, b: u8) -> bool {
        match self {
            Alphabet::Standard => b == b'+' || b == b'/',
            Alphabet::UrlSafe => b == b'-' || b == b'_',
        }
    }
}

/// The alphabet a symbol belongs to only: `None` for the 62 shared symbols and `=`.
fn specific_of(b: u8) -> Option<Alphabet> {
    match b {
        b'+' | b'/' => Some(Alphabet::Standard),
        b'-' | b'_' => Some(Alphabet::UrlSafe),
        _ => None,
    }
}

/// True for a byte that may belong to a run of some alphabet.
fn is_symbol(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_' | b'=')
}

/// The line ending a record states (`line_ending` 0 or 1; 2 is none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// `\n` (0).
    Lf,
    /// `\r\n` (1).
    CrLf,
}

impl LineEnding {
    /// Both.
    pub const ALL: [LineEnding; 2] = [LineEnding::Lf, LineEnding::CrLf];

    /// The bytes.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            LineEnding::Lf => b"\n",
            LineEnding::CrLf => b"\r\n",
        }
    }

    /// The record's `line_ending`.
    pub fn code(self) -> u8 {
        match self {
            LineEnding::Lf => 0,
            LineEnding::CrLf => 1,
        }
    }

    /// Lower-kebab name.
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "lf",
            LineEnding::CrLf => "crlf",
        }
    }
}

/// The fields of a regular run that the record keeps beside its length and hash: exactly what
/// the re-encoding needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Layout {
    /// The alphabet (`variant`).
    pub alphabet: Alphabet,
    /// Characters per line; 0 when the text has no line breaks.
    pub line_len: u16,
    /// The line ending; `None` exactly when `line_len` is 0.
    pub line_ending: Option<LineEnding>,
    /// `=` padding written.
    pub padding: bool,
}

impl Layout {
    /// The layout a record states; `None` when its fields are not a layout (a `variant` or
    /// `line_ending` outside the enumerations, or `line_len` and `line_ending` disagreeing).
    pub fn from_record(r: &Base64Record) -> Option<Layout> {
        let alphabet = Alphabet::from_variant(r.variant)?;
        let line_ending = match (r.line_len, r.line_ending) {
            (0, 2) => None,
            (0, _) | (_, 2) => return None,
            (_, 0) => Some(LineEnding::Lf),
            (_, 1) => Some(LineEnding::CrLf),
            _ => return None,
        };
        if r.padding > 1 {
            return None;
        }
        Some(Layout {
            alphabet,
            line_len: r.line_len,
            line_ending,
            padding: r.padding == 1,
        })
    }

    /// The record of `text`, a run with this layout.
    pub fn record(&self, text: &[u8]) -> Base64Record {
        Base64Record {
            variant: self.alphabet.variant(),
            line_len: self.line_len,
            line_ending: self.line_ending.map_or(2, LineEnding::code),
            padding: u8::from(self.padding),
            original_len: text.len() as u64,
            original_hash: *blake3::hash(text).as_bytes(),
        }
    }

    /// The length of the text that encodes `payload_len` bytes: `c + e * b` with `c` the
    /// characters (`4 * ceil(n / 3)` padded, else `4 * floor(n / 3) + (0, 2, 3)[n mod 3]`),
    /// `b` the line breaks (`ceil(c / line_len) - 1` when both are nonzero, else 0) and `e`
    /// the line ending's length. `None` when the arithmetic overflows.
    pub fn encoded_len(&self, payload_len: u64) -> Option<u64> {
        let full = payload_len / 3;
        let rem = payload_len % 3;
        let c = if self.padding {
            full.checked_add(u64::from(rem != 0))?.checked_mul(4)?
        } else {
            full.checked_mul(4)?.checked_add([0, 2, 3][rem as usize])?
        };
        let breaks = match (self.line_len, self.line_ending) {
            (0, _) | (_, None) => 0,
            (l, Some(_)) if c > 0 => c.div_ceil(u64::from(l)) - 1,
            _ => 0,
        };
        let e = self.line_ending.map_or(0, |le| le.bytes().len() as u64);
        c.checked_add(breaks.checked_mul(e)?)
    }

    /// Encode `payload` by RFC 4648 with this layout: the alphabet's symbols, unused bits
    /// zero, `=` exactly when `padding`, a line ending after every `line_len` characters
    /// except the last.
    pub fn encode(&self, payload: &[u8]) -> Vec<u8> {
        let symbols = self.alphabet.symbols();
        let mut out = Vec::with_capacity(
            usize::try_from(self.encoded_len(payload.len() as u64).unwrap_or(0))
                .unwrap_or(payload.len()),
        );
        let mut on_line = 0usize;
        let line_len = usize::from(self.line_len);
        let ending = self.line_ending.map(LineEnding::bytes);
        let mut put = |out: &mut Vec<u8>, c: u8| {
            if let Some(e) = ending {
                if line_len > 0 && on_line == line_len {
                    out.extend_from_slice(e);
                    on_line = 0;
                }
            }
            out.push(c);
            on_line += 1;
        };
        let (quanta, rest) = payload.as_chunks::<3>();
        for q in quanta {
            let v = (u32::from(q[0]) << 16) | (u32::from(q[1]) << 8) | u32::from(q[2]);
            for shift in [18u32, 12, 6, 0] {
                put(&mut out, symbols[((v >> shift) & 63) as usize]);
            }
        }
        match rest.len() {
            1 => {
                let v = u32::from(rest[0]) << 16;
                put(&mut out, symbols[((v >> 18) & 63) as usize]);
                put(&mut out, symbols[((v >> 12) & 63) as usize]);
                if self.padding {
                    put(&mut out, b'=');
                    put(&mut out, b'=');
                }
            }
            2 => {
                let v = (u32::from(rest[0]) << 16) | (u32::from(rest[1]) << 8);
                put(&mut out, symbols[((v >> 18) & 63) as usize]);
                put(&mut out, symbols[((v >> 12) & 63) as usize]);
                put(&mut out, symbols[((v >> 6) & 63) as usize]);
                if self.padding {
                    put(&mut out, b'=');
                }
            }
            _ => {}
        }
        out
    }
}

/// Why a run is not regular (or not even one alphabet). Each is counted by the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Irregular {
    /// A byte that is no symbol, `=`, CR or LF.
    OtherByte,
    /// Both `+`/`/` and `-`/`_`.
    MixedAlphabet,
    /// A CR not followed by LF.
    BareCr,
    /// Some lines end with LF, others with CRLF.
    MixedLineEndings,
    /// A line ending follows the last line.
    TrailingLineEnding,
    /// An empty line inside the run.
    EmptyLine,
    /// A line other than the last differs in length from the first, or the last is longer.
    RaggedLines,
    /// Lines longer than the record can state.
    LineTooLong,
    /// `=` somewhere other than the end.
    PaddingInside,
    /// More than two `=`, or `=` not completing the last quantum.
    BadPadding,
    /// The character count leaves one character in the last quantum, or there is none.
    BadLength,
    /// The unused bits of the last quantum are not zero.
    NonzeroUnusedBits,
    /// The re-encoding from the record's fields differs from the text.
    Mismatch,
}

impl Irregular {
    /// Every cause, in report order.
    pub const ALL: [Irregular; 13] = [
        Irregular::OtherByte,
        Irregular::MixedAlphabet,
        Irregular::BareCr,
        Irregular::MixedLineEndings,
        Irregular::TrailingLineEnding,
        Irregular::EmptyLine,
        Irregular::RaggedLines,
        Irregular::LineTooLong,
        Irregular::PaddingInside,
        Irregular::BadPadding,
        Irregular::BadLength,
        Irregular::NonzeroUnusedBits,
        Irregular::Mismatch,
    ];

    /// Lower-kebab name.
    pub fn label(self) -> &'static str {
        match self {
            Irregular::OtherByte => "other-byte",
            Irregular::MixedAlphabet => "mixed-alphabet",
            Irregular::BareCr => "bare-cr",
            Irregular::MixedLineEndings => "mixed-line-endings",
            Irregular::TrailingLineEnding => "trailing-line-ending",
            Irregular::EmptyLine => "empty-line",
            Irregular::RaggedLines => "ragged-lines",
            Irregular::LineTooLong => "line-too-long",
            Irregular::PaddingInside => "padding-inside",
            Irregular::BadPadding => "bad-padding",
            Irregular::BadLength => "bad-length",
            Irregular::NonzeroUnusedBits => "nonzero-unused-bits",
            Irregular::Mismatch => "mismatch",
        }
    }
}

impl std::fmt::Display for Irregular {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The regularity rule: the layout of `text` when it is a regular run.
pub fn layout(text: &[u8]) -> Result<Layout, Irregular> {
    // Lines and line endings.
    let mut ending: Option<LineEnding> = None;
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut at = 0usize;
    let mut i = 0usize;
    while i < text.len() {
        match text[i] {
            b'\n' => {
                let (line, e) = match text[at..i].strip_suffix(b"\r") {
                    Some(l) => (l, LineEnding::CrLf),
                    None => (&text[at..i], LineEnding::Lf),
                };
                match ending {
                    None => ending = Some(e),
                    Some(have) if have != e => return Err(Irregular::MixedLineEndings),
                    Some(_) => {}
                }
                lines.push(line);
                at = i + 1;
            }
            b'\r' if text.get(i + 1) != Some(&b'\n') => return Err(Irregular::BareCr),
            _ => {}
        }
        i += 1;
    }
    if at == text.len() && !text.is_empty() {
        return Err(Irregular::TrailingLineEnding);
    }
    lines.push(&text[at..]);
    // Symbols and the alphabet.
    let mut alphabet: Option<Alphabet> = None;
    for line in &lines {
        for &b in *line {
            if !is_symbol(b) {
                return Err(Irregular::OtherByte);
            }
            if let Some(a) = specific_of(b) {
                match alphabet {
                    None => alphabet = Some(a),
                    Some(have) if have != a => return Err(Irregular::MixedAlphabet),
                    Some(_) => {}
                }
            }
        }
    }
    let alphabet = alphabet.unwrap_or(Alphabet::Standard);
    // Line lengths.
    if lines.iter().any(|l| l.is_empty()) {
        return Err(Irregular::EmptyLine);
    }
    let (line_len, line_ending) = if lines.len() == 1 {
        (0u16, None)
    } else {
        let first = lines[0].len();
        if lines[1..lines.len() - 1].iter().any(|l| l.len() != first)
            || lines[lines.len() - 1].len() > first
        {
            return Err(Irregular::RaggedLines);
        }
        let l = u16::try_from(first).map_err(|_| Irregular::LineTooLong)?;
        if l == 0 {
            return Err(Irregular::EmptyLine);
        }
        (l, ending)
    };
    // Padding and the character count.
    let chars: Vec<u8> = lines.iter().flat_map(|l| l.iter().copied()).collect();
    let pad = chars.iter().rev().take_while(|&&b| b == b'=').count();
    let body = &chars[..chars.len() - pad];
    if body.contains(&b'=') {
        return Err(Irregular::PaddingInside);
    }
    if body.is_empty() || body.len() % 4 == 1 {
        return Err(if pad > 0 && body.is_empty() {
            Irregular::BadPadding
        } else {
            Irregular::BadLength
        });
    }
    if pad > 0 && (pad > 2 || !(body.len() + pad).is_multiple_of(4)) {
        return Err(Irregular::BadPadding);
    }
    // The unused bits of the last quantum.
    let last = alphabet
        .value(body[body.len() - 1])
        .ok_or(Irregular::OtherByte)?;
    let unused = match body.len() % 4 {
        2 => last & 0x0F,
        3 => last & 0x03,
        _ => 0,
    };
    if unused != 0 {
        return Err(Irregular::NonzeroUnusedBits);
    }
    Ok(Layout {
        alphabet,
        line_len,
        line_ending,
        // A text with no `=` whose last quantum is full re-encodes the same either way: it
        // is recorded as padded (what MIME and PEM write).
        padding: pad > 0 || body.len().is_multiple_of(4),
    })
}

/// Decode the symbols of `text` in `alphabet`, skipping line endings and stopping at `=`.
/// Only the symbol mapping is checked (the regularity rule is [`layout`]'s).
pub fn decode(text: &[u8], alphabet: Alphabet) -> Result<Vec<u8>, Irregular> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 2);
    let mut acc = 0u32;
    let mut n = 0u32;
    for &b in text {
        match b {
            b'\r' | b'\n' => continue,
            b'=' => break,
            _ => {}
        }
        let v = alphabet.value(b).ok_or(Irregular::OtherByte)?;
        acc = (acc << 6) | u32::from(v);
        n += 1;
        if n == 4 {
            out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
            acc = 0;
            n = 0;
        }
    }
    match n {
        0 => {}
        1 => return Err(Irregular::BadLength),
        2 => out.push((acc >> 4) as u8),
        _ => {
            out.push((acc >> 10) as u8);
            out.push((acc >> 2) as u8);
        }
    }
    Ok(out)
}

/// A regular run: its record, and the payload the record's fields encode back to the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysed {
    /// The layout.
    pub layout: Layout,
    /// The record (its fields, the run's length and hash).
    pub record: Base64Record,
    /// The decoded bytes.
    pub payload: Vec<u8>,
}

/// The regularity rule, the decode, and the re-encode from exactly the record's fields,
/// compared with `text` ([`Irregular::Mismatch`] when it differs).
pub fn analyse(text: &[u8]) -> Result<Analysed, Irregular> {
    let layout = layout(text)?;
    let payload = decode(text, layout.alphabet)?;
    let record = layout.record(text);
    let back = Layout::from_record(&record).ok_or(Irregular::Mismatch)?;
    if back.encoded_len(payload.len() as u64) != Some(text.len() as u64)
        || back.encode(&payload) != text
    {
        return Err(Irregular::Mismatch);
    }
    Ok(Analysed {
        layout,
        record,
        payload,
    })
}

/// What precedes a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Label {
    /// `Content-Transfer-Encoding: base64` within the preceding bytes.
    Mime,
    /// A `-----BEGIN` line right before.
    Pem,
    /// `;base64,` right before (a data URI).
    DataUri,
    /// A quote right before (a JSON string).
    Json,
    /// `>` as the last non-blank byte before (an XML element's text).
    Xml,
    /// Anything else.
    Bare,
}

impl Label {
    /// Every label, in report order.
    pub const ALL: [Label; 6] = [
        Label::Mime,
        Label::Pem,
        Label::DataUri,
        Label::Json,
        Label::Xml,
        Label::Bare,
    ];

    /// Lower-kebab name.
    pub fn label(self) -> &'static str {
        match self {
            Label::Mime => "mime",
            Label::Pem => "pem",
            Label::DataUri => "data-uri",
            Label::Json => "json",
            Label::Xml => "xml",
            Label::Bare => "bare",
        }
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A candidate run of `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// Where it starts.
    pub offset: usize,
    /// Its length in bytes, line endings included.
    pub len: usize,
    /// Its characters (line endings not counted).
    pub chars: usize,
    /// Its lines.
    pub lines: usize,
    /// What precedes it.
    pub label: Label,
    /// Every character is a hexadecimal digit: counted apart, never a base64 candidate.
    pub hex_only: bool,
}

/// One line's segment while scanning.
#[derive(Debug, Clone, Copy)]
struct Seg {
    start: usize,
    end: usize,
    alphabet: Option<Alphabet>,
    at_line_start: bool,
    at_line_end: bool,
    hex_only: bool,
}

/// A run being built: its lines (start, end, carries a specific symbol).
#[derive(Debug)]
struct Build {
    lines: Vec<(usize, usize, bool)>,
    alphabet: Option<Alphabet>,
    hex_only: bool,
    /// The last line's segment ends at its line's end (a next line may join).
    open: bool,
}

impl Build {
    fn new(seg: &Seg) -> Build {
        Build {
            lines: vec![(seg.start, seg.end, seg.alphabet.is_some())],
            alphabet: seg.alphabet,
            hex_only: seg.hex_only,
            open: seg.at_line_end,
        }
    }

    fn push(&mut self, seg: &Seg) {
        self.lines
            .push((seg.start, seg.end, seg.alphabet.is_some()));
        if self.alphabet.is_none() {
            self.alphabet = seg.alphabet;
        }
        self.hex_only &= seg.hex_only;
        self.open = seg.at_line_end;
    }

    fn compatible(&self, seg: &Seg) -> bool {
        match (self.alphabet, seg.alphabet) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
    }

    /// The lines after the last one with a specific symbol, taken out.
    fn split_neutral_tail(&mut self) -> Vec<(usize, usize, bool)> {
        let keep = self.lines.iter().rposition(|l| l.2).map_or(0, |i| i + 1);
        self.lines.split_off(keep)
    }

    fn finish(self, text: &[u8], out: &mut Vec<Run>) {
        let chars: usize = self.lines.iter().map(|l| l.1 - l.0).sum();
        if chars < MIN_RUN || self.lines.is_empty() {
            return;
        }
        let offset = self.lines[0].0;
        let end = self.lines[self.lines.len() - 1].1;
        out.push(Run {
            offset,
            len: end - offset,
            chars,
            lines: self.lines.len(),
            label: label_of(text, offset),
            hex_only: self.hex_only,
        });
    }
}

/// The segments of the line `text[ls..le]`.
fn segments(text: &[u8], ls: usize, le: usize) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut i = ls;
    while i < le {
        if !is_symbol(text[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut alphabet: Option<Alphabet> = None;
        let mut hex_only = true;
        while i < le && is_symbol(text[i]) {
            let b = text[i];
            if let Some(a) = specific_of(b) {
                match alphabet {
                    None => alphabet = Some(a),
                    Some(have) if have != a => break,
                    Some(_) => {}
                }
            }
            hex_only &= b.is_ascii_hexdigit();
            i += 1;
        }
        segs.push(Seg {
            start,
            end: i,
            alphabet,
            at_line_start: start == ls,
            at_line_end: i == le,
            hex_only,
        });
    }
    segs
}

/// The candidate runs of `text`, in order: maximal runs of at least [`MIN_RUN`] characters
/// over one alphabet (the module's scan). Linear in the text.
pub fn find_runs(text: &[u8]) -> Vec<Run> {
    let mut out = Vec::new();
    let mut run: Option<Build> = None;
    let mut ls = 0usize;
    while ls <= text.len() {
        // The line [ls, le) and where the next starts.
        let (le, next) = match text[ls..].iter().position(|&b| b == b'\n') {
            Some(p) => {
                let lf = ls + p;
                let le = if lf > ls && text[lf - 1] == b'\r' {
                    lf - 1
                } else {
                    lf
                };
                (le, lf + 1)
            }
            None => (text.len(), text.len() + 1),
        };
        let segs = segments(text, ls, le);
        for (k, seg) in segs.iter().enumerate() {
            let joined = match run.as_mut() {
                Some(r) if k == 0 && seg.at_line_start && r.open => {
                    if r.compatible(seg) {
                        r.push(seg);
                        true
                    } else if seg.at_line_end {
                        // A whole-line segment of the other alphabet: the run's trailing
                        // neutral lines go with it.
                        let tail = r.split_neutral_tail();
                        let mut next_run = Build {
                            lines: tail,
                            alphabet: None,
                            hex_only: true,
                            open: true,
                        };
                        next_run.hex_only = next_run
                            .lines
                            .iter()
                            .all(|l| text[l.0..l.1].iter().all(u8::is_ascii_hexdigit));
                        next_run.push(seg);
                        if let Some(old) = run.replace(next_run) {
                            old.finish(text, &mut out);
                        }
                        if run.as_ref().is_some_and(|r| r.lines.is_empty()) {
                            run = None;
                        }
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if !joined {
                if let Some(old) = run.take() {
                    old.finish(text, &mut out);
                }
                run = Some(Build::new(seg));
            }
        }
        // A run whose last segment ended inside the line, or a line without segments, cannot
        // continue.
        if let Some(r) = run.as_ref() {
            if !r.open || segs.is_empty() {
                if let Some(old) = run.take() {
                    old.finish(text, &mut out);
                }
            }
        }
        ls = next;
    }
    if let Some(old) = run.take() {
        old.finish(text, &mut out);
    }
    out
}

/// The label of a run starting at `offset`.
fn label_of(text: &[u8], offset: usize) -> Label {
    let before = &text[..offset];
    if before.ends_with(b";base64,") {
        return Label::DataUri;
    }
    if previous_line(before).is_some_and(|l| l.starts_with(b"-----BEGIN")) {
        return Label::Pem;
    }
    let window = &before[before.len().saturating_sub(MIME_WINDOW)..];
    if contains_ignore_case(window, b"content-transfer-encoding: base64") {
        return Label::Mime;
    }
    match before.last() {
        Some(b'"') | Some(b'\'') => return Label::Json,
        _ => {}
    }
    if ends_in_opening_tag(before) {
        return Label::Xml;
    }
    Label::Bare
}

/// True when `before`, trimmed of trailing blanks, ends with `>` closing a tag that is not a
/// closing tag (`<a>` yes, `</a>` no).
fn ends_in_opening_tag(before: &[u8]) -> bool {
    let end = before
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |p| p + 1);
    let trimmed = &before[..end];
    if trimmed.last() != Some(&b'>') {
        return false;
    }
    match trimmed.iter().rposition(|&b| b == b'<') {
        Some(lt) => trimmed.get(lt + 1) != Some(&b'/'),
        None => false,
    }
}

/// The line before the one `before` ends in (the run's first line), without its ending.
fn previous_line(before: &[u8]) -> Option<&[u8]> {
    let cur = before.iter().rposition(|&b| b == b'\n')?;
    let end = if cur > 0 && before[cur - 1] == b'\r' {
        cur - 1
    } else {
        cur
    };
    let start = before[..end]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    Some(&before[start..end])
}

fn contains_ignore_case(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 section 10's base64 vectors.
    const VECTORS: [(&[u8], &[u8]); 7] = [
        (b"", b""),
        (b"f", b"Zg=="),
        (b"fo", b"Zm8="),
        (b"foo", b"Zm9v"),
        (b"foob", b"Zm9vYg=="),
        (b"fooba", b"Zm9vYmE="),
        (b"foobar", b"Zm9vYmFy"),
    ];

    fn plain() -> Layout {
        Layout {
            alphabet: Alphabet::Standard,
            line_len: 0,
            line_ending: None,
            padding: true,
        }
    }

    fn lines(line_len: u16, ending: LineEnding, padding: bool, alphabet: Alphabet) -> Layout {
        Layout {
            alphabet,
            line_len,
            line_ending: Some(ending),
            padding,
        }
    }

    fn bytes(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn the_rfc_4648_vectors_encode_and_decode() {
        for (payload, text) in VECTORS {
            assert_eq!(plain().encode(payload), text);
            assert_eq!(decode(text, Alphabet::Standard).unwrap(), payload);
            assert_eq!(
                plain().encoded_len(payload.len() as u64),
                Some(text.len() as u64)
            );
            if !text.is_empty() {
                let a = analyse(text).unwrap();
                assert_eq!(a.payload, payload);
                assert_eq!(a.layout, plain());
                assert_eq!(a.record.original_len, text.len() as u64);
            }
        }
        // Unpadded: the same vectors without `=`.
        let unpadded = Layout {
            padding: false,
            ..plain()
        };
        for (payload, text) in VECTORS {
            let t: Vec<u8> = text.iter().copied().filter(|&b| b != b'=').collect();
            assert_eq!(unpadded.encode(payload), t);
            if !t.is_empty() {
                let a = analyse(&t).unwrap();
                assert_eq!(a.payload, payload);
                assert_eq!(a.layout.padding, t.len().is_multiple_of(4));
            }
            assert_eq!(
                unpadded.encoded_len(payload.len() as u64),
                Some(t.len() as u64)
            );
        }
        // The url-safe alphabet's two symbols.
        let p = [0xFBu8, 0xFF, 0xBF];
        assert_eq!(plain().encode(&p), b"+/+/");
        let url = Layout {
            alphabet: Alphabet::UrlSafe,
            ..plain()
        };
        assert_eq!(url.encode(&p), b"-_-_");
        assert_eq!(decode(b"-_-_", Alphabet::UrlSafe).unwrap(), p);
        assert_eq!(
            decode(b"-_-_", Alphabet::Standard),
            Err(Irregular::OtherByte)
        );
    }

    #[test]
    fn every_layout_round_trips_payloads_of_0_to_300_bytes() {
        let mut cases = 0u64;
        for n in 0..=300usize {
            let payload = bytes(n, n as u64 * 7919 + 1);
            for alphabet in Alphabet::ALL {
                for padding in [false, true] {
                    let mut layouts = vec![Layout {
                        alphabet,
                        line_len: 0,
                        line_ending: None,
                        padding,
                    }];
                    for line_len in [1u16, 4, 64, 76, 65535] {
                        for ending in LineEnding::ALL {
                            layouts.push(lines(line_len, ending, padding, alphabet));
                        }
                    }
                    for layout in layouts {
                        let text = layout.encode(&payload);
                        assert_eq!(
                            layout.encoded_len(n as u64),
                            Some(text.len() as u64),
                            "{layout:?} n={n}"
                        );
                        if n == 0 {
                            assert!(text.is_empty());
                            continue;
                        }
                        let a = analyse(&text).unwrap_or_else(|e| panic!("{layout:?} n={n}: {e}"));
                        assert_eq!(a.payload, payload);
                        // The detected layout is the one used, except that a text that
                        // fits one line has no line ending, and a text with neither
                        // specific symbol is standard.
                        let mut expect = layout;
                        if text.len() <= usize::from(layout.line_len) || layout.line_len == 0 {
                            expect.line_len = 0;
                            expect.line_ending = None;
                        }
                        if !text.iter().any(|&b| specific_of(b).is_some()) {
                            expect.alphabet = Alphabet::Standard;
                        }
                        let chars = text.iter().filter(|&&b| b != b'\r' && b != b'\n').count();
                        expect.padding = text.contains(&b'=') || chars % 4 == 0;
                        assert_eq!(a.layout, expect, "n={n}");
                        assert_eq!(Layout::from_record(&a.record), Some(expect));
                        assert_eq!(a.record.original_len, text.len() as u64);
                        assert!(a.record.verify_hash(&text));
                        cases += 1;
                    }
                }
            }
        }
        assert_eq!(cases, 300 * 2 * 2 * 11);
    }

    #[test]
    fn each_irregular_cause_is_found() {
        let payload = bytes(100, 3);
        let l = lines(16, LineEnding::Lf, true, Alphabet::Standard);
        let text = l.encode(&payload);
        assert!(analyse(&text).is_ok());
        let replace = |t: &[u8], at: usize, with: &[u8]| {
            let mut v = t[..at].to_vec();
            v.extend_from_slice(with);
            v.extend_from_slice(&t[at + with.len()..]);
            v
        };
        let cases: Vec<(Irregular, Vec<u8>)> = vec![
            (Irregular::OtherByte, replace(&text, 5, b" ")),
            (Irregular::MixedAlphabet, {
                let mut t = replace(&text, 5, b"+");
                t[6] = b'-';
                t
            }),
            (Irregular::BareCr, replace(&text, 5, b"\r")),
            (Irregular::MixedLineEndings, {
                let mut t = text.clone();
                t.insert(16, b'\r');
                t
            }),
            (Irregular::TrailingLineEnding, {
                let mut t = text.clone();
                t.push(b'\n');
                t
            }),
            (Irregular::EmptyLine, {
                let mut t = text.clone();
                t.insert(17, b'\n');
                t
            }),
            (Irregular::RaggedLines, {
                let mut t = text.clone();
                t.remove(16);
                t.insert(20, b'\n');
                t
            }),
            (Irregular::RaggedLines, {
                // The last line longer than the others.
                let mut t = text.clone();
                let last_break = t.iter().rposition(|&b| b == b'\n').unwrap();
                t.remove(last_break);
                t
            }),
            (Irregular::LineTooLong, {
                let p = bytes(49_200, 9);
                let mut t = plain().encode(&p);
                t.insert(65_536, b'\n');
                t
            }),
            (Irregular::PaddingInside, replace(&text, 5, b"=")),
            (Irregular::BadPadding, {
                let mut t = text.clone();
                t.push(b'=');
                t
            }),
            (Irregular::BadPadding, b"====".to_vec()),
            (Irregular::BadLength, b"Zm9vY".to_vec()),
            (Irregular::BadLength, {
                let mut t = text.clone();
                t.truncate(t.len() - 3);
                t
            }),
            (Irregular::NonzeroUnusedBits, b"Zm9vYh==".to_vec()),
            (Irregular::NonzeroUnusedBits, b"Zm9vYmF".to_vec()),
        ];
        for (cause, t) in cases {
            assert_eq!(
                analyse(&t).err(),
                Some(cause),
                "{:?}",
                String::from_utf8_lossy(&t)
            );
        }
        for cause in Irregular::ALL {
            assert!(!cause.label().is_empty());
        }
        // A padded text whose padding is wrong for its length.
        assert_eq!(analyse(b"Zm9vYmFy=").err(), Some(Irregular::BadPadding));
        // A record with fields outside the layout is not a layout.
        let mut r = l.record(&text);
        r.variant = 2;
        assert_eq!(Layout::from_record(&r), None);
        let mut r = l.record(&text);
        r.line_ending = 2;
        assert_eq!(Layout::from_record(&r), None);
        let mut r = l.record(&text);
        r.line_len = 0;
        assert_eq!(Layout::from_record(&r), None);
    }

    fn pem_block(payload: &[u8], ending: &str) -> String {
        let body = lines(64, LineEnding::Lf, true, Alphabet::Standard).encode(payload);
        let body = String::from_utf8(body).unwrap().replace('\n', ending);
        format!(
            "-----BEGIN CERTIFICATE-----{ending}{body}{ending}-----END CERTIFICATE-----{ending}"
        )
    }

    #[test]
    fn runs_are_found_whole_and_labelled() {
        // A PEM bundle: two blocks, the second's first line carrying no `+` or `/`, so it
        // first looks like the header's url-safe line's continuation.
        let p1 = bytes(600, 11);
        let mut p2 = bytes(600, 12);
        // Force the first 48 bytes (the first 64 characters) to encode without `+` or `/`:
        // zeros do.
        p2[..48].fill(0);
        let text = format!("{}{}", pem_block(&p1, "\n"), pem_block(&p2, "\r\n"));
        let runs = find_runs(text.as_bytes());
        assert_eq!(runs.len(), 2, "{runs:?}");
        for (run, p) in runs.iter().zip([&p1, &p2]) {
            assert_eq!(run.label, Label::Pem);
            assert!(!run.hex_only);
            let t = &text.as_bytes()[run.offset..run.offset + run.len];
            let a = analyse(t).unwrap();
            assert_eq!(&a.payload, p);
            assert_eq!(a.layout.line_len, 64);
            assert_eq!(run.lines, 13);
            assert_eq!(run.chars, 800);
        }
        assert_eq!(runs[0].chars, 800);
        // A MIME part with CRLF and 76-character lines.
        let body = lines(76, LineEnding::CrLf, true, Alphabet::Standard).encode(&p1);
        let mime = format!(
            "Content-Type: application/octet-stream\r\nContent-Transfer-Encoding: BASE64\r\n\r\n{}\r\n--boundary--\r\n",
            String::from_utf8(body).unwrap()
        );
        let runs = find_runs(mime.as_bytes());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].label, Label::Mime);
        assert!(analyse(&mime.as_bytes()[runs[0].offset..runs[0].offset + runs[0].len]).is_ok());
        // A data URI, a JSON string, an XML element, a bare run, and the url-safe alphabet.
        let one_line = String::from_utf8(plain().encode(&p1)).unwrap();
        let url = String::from_utf8(
            Layout {
                alphabet: Alphabet::UrlSafe,
                padding: false,
                ..plain()
            }
            .encode(&p1),
        )
        .unwrap();
        let text = format!(
            "<img src=\"data:image/png;base64,{one_line}\">\n{{\"blob\": \"{one_line}\"}}\n<data>\n  {one_line}\n</data>\n{one_line}\n{url}\n"
        );
        let runs = find_runs(text.as_bytes());
        let labels: Vec<Label> = runs.iter().map(|r| r.label).collect();
        assert_eq!(
            labels,
            [
                Label::DataUri,
                Label::Json,
                Label::Xml,
                Label::Bare,
                Label::Bare
            ]
        );
        // After a closing tag a run is bare; after an opening tag on the same line, XML.
        assert_eq!(
            find_runs(format!("</a>\n{one_line}").as_bytes())[0].label,
            Label::Bare
        );
        assert_eq!(
            find_runs(format!("<a> {one_line}").as_bytes())[0].label,
            Label::Xml
        );
        for r in &runs {
            let a = analyse(&text.as_bytes()[r.offset..r.offset + r.len]).unwrap();
            assert_eq!(a.payload, p1);
            assert_eq!(a.layout.line_len, 0);
        }
        assert_eq!(runs[4].len, url.len());
        // Hex-only runs are counted apart; short runs are not candidates.
        let hex: String = (0..300).map(|i| format!("{:x}", i % 16)).collect();
        let short = String::from_utf8(plain().encode(&bytes(100, 5))).unwrap();
        let text = format!("{hex}\n\n{short}\n");
        let runs = find_runs(text.as_bytes());
        assert_eq!(runs.len(), 1);
        assert!(runs[0].hex_only);
        assert_eq!(runs[0].chars, 300);
        // A run of 255 characters is not one; 256 is.
        let t: String = "A".repeat(255);
        assert!(find_runs(t.as_bytes()).is_empty());
        let t: String = "A".repeat(256);
        assert_eq!(find_runs(t.as_bytes()).len(), 1);
        // Both alphabets' symbols on one line are two segments, two runs.
        let both = format!("{}{}", "+".repeat(300), "-".repeat(300));
        let runs = find_runs(both.as_bytes());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len, 300);
        assert_eq!(runs[1].offset, 300);
        // Empty text and text without symbols.
        assert!(find_runs(b"").is_empty());
        assert!(find_runs(b"   \n\n").is_empty());
    }

    #[test]
    fn an_irregular_run_in_a_text_is_reported_by_its_cause() {
        // JSON with an escaped line break inside the base64: the run breaks at the backslash,
        // so the two halves are two bare runs; and a run with indented lines is ragged.
        let p = bytes(600, 21);
        let body = lines(64, LineEnding::Lf, true, Alphabet::Standard).encode(&p);
        let indented = String::from_utf8(body).unwrap().replace('\n', "\n    ");
        let text = format!("<x>\n    {indented}\n</x>\n");
        let runs = find_runs(text.as_bytes());
        // Each indented line is its own segment that does not start at the line start, so
        // the lines do not join: no run reaches the minimum.
        assert!(runs.is_empty(), "{runs:?}");
        // The same without indentation but with one line shortened: ragged.
        let body = lines(64, LineEnding::Lf, true, Alphabet::Standard).encode(&p);
        let mut t = body.clone();
        t.remove(70);
        assert_eq!(analyse(&t).err(), Some(Irregular::RaggedLines));
        let runs = find_runs(&t);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, t.len());
    }

    #[test]
    fn encoded_len_matches_the_formula_and_overflow_is_none() {
        let l = lines(76, LineEnding::CrLf, true, Alphabet::Standard);
        assert_eq!(l.encoded_len(0), Some(0));
        assert_eq!(l.encoded_len(57), Some(76));
        assert_eq!(l.encoded_len(58), Some(80 + 2));
        assert_eq!(l.encoded_len(u64::MAX), None);
        let u = lines(4, LineEnding::Lf, false, Alphabet::Standard);
        assert_eq!(u.encoded_len(4), Some(6 + 1));
        assert_eq!(u.encoded_len(5), Some(7 + 1));
    }
}
