//! The preview pane's backend (E5-6; `docs/notes/e5-design.md` sections 2 and 4): one capped
//! `read_entry` of [`PREVIEW_CAP`] bytes, the content classified here, by what the bytes are and
//! never by the entry's name, and sent to the page as one binary message ([`Preview::encode`]).
//! The page never renders archive content as HTML: text goes to a `<pre>` through `textContent`
//! (with hidden and bidirectional characters already escaped here), an image through a `Blob`
//! of the MIME type chosen here, everything else through a hex view.
//!
//! The wire format (little-endian): magic `LPKV`, kind `u8` ([`Kind`]), MIME length `u8`, the
//! entry's size `u64` (for a folder: how many entries it holds directly), the MIME, the body.

use std::sync::{Arc, Mutex};

use lpk_api::{CancelToken, Class};

use crate::app::{lock, App};
use crate::extract::{open_unlocked, Prompter};
use crate::GuiError;

/// The most one preview reads: an entry over it is refused naming the cap (16 MiB).
pub const PREVIEW_CAP: u64 = 16 * 1024 * 1024;

/// The magic of a preview message.
pub const MAGIC: &[u8; 4] = b"LPKV";

/// What the pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Text; the body is the escaped text, UTF-8.
    Text = 0,
    /// An image the webview decodes; the body is the file, the MIME is set.
    Image = 1,
    /// Anything else; the body is the file, shown as hex.
    Hex = 2,
    /// A folder; no body, the size is its entry count.
    Folder = 3,
}

/// One preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// What to show.
    pub kind: Kind,
    /// The image MIME type (empty unless an image).
    pub mime: &'static str,
    /// The entry's size (a folder: its entry count).
    pub size: u64,
    /// The body.
    pub body: Vec<u8>,
}

impl Preview {
    /// The message the page receives.
    pub fn encode(self) -> Vec<u8> {
        let mime = self.mime.as_bytes();
        let mut out = Vec::with_capacity(14 + mime.len() + self.body.len());
        out.extend_from_slice(MAGIC);
        out.push(self.kind as u8);
        out.push(mime.len() as u8);
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(mime);
        out.extend_from_slice(&self.body);
        out
    }
}

const SIGNATURES: &[(&[u8], &str)] = &[
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
    (b"\x00\x00\x01\x00", "image/x-icon"),
];

/// The image MIME type of `b` by its signature (never SVG: an SVG is text).
pub fn image_mime(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    // A BMP: "BM", the file header's reserved fields zero (a text starting "BM" is not one).
    if b.len() >= 14 && b.starts_with(b"BM") && b[6..10] == [0; 4] {
        return Some("image/bmp");
    }
    SIGNATURES
        .iter()
        .find(|(sig, _)| b.starts_with(sig))
        .map(|(_, m)| *m)
}

/// The text of `b`: valid UTF-8 (a BOM allowed) or UTF-16 with a BOM, without a NUL; the
/// leading BOM is dropped.
pub fn decode_text(b: &[u8]) -> Option<String> {
    let text = if let Some(rest) = b.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, u16::from_le_bytes)?
    } else if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, u16::from_be_bytes)?
    } else {
        let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
        std::str::from_utf8(b).ok()?.to_string()
    };
    if text.contains('\0') {
        None
    } else {
        Some(text)
    }
}

fn utf16(b: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    let (pairs, rest) = b.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    let units = pairs.iter().map(|c| unit(*c));
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

/// Whether `c` is hidden or changes the direction of text (TR36): the bidi controls, the
/// invisible format characters, and the C0/C1 controls other than tab, LF and CR.
pub fn is_hidden(c: char) -> bool {
    matches!(c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        | '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2064}' | '\u{00AD}' | '\u{FEFF}')
        || (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
}

/// `text` with each hidden character written out as the visible text `\u{XXXX}`.
pub fn escape_hidden(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_hidden(c) {
            out.push_str(&format!("\\u{{{:04X}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// Classify the bytes of a file of the entry and prepare what the page shows.
pub fn classify(bytes: Vec<u8>) -> Preview {
    let size = bytes.len() as u64;
    if let Some(mime) = image_mime(&bytes) {
        return Preview {
            kind: Kind::Image,
            mime,
            size,
            body: bytes,
        };
    }
    match decode_text(&bytes) {
        Some(t) => Preview {
            kind: Kind::Text,
            mime: "",
            size,
            body: escape_hidden(&t).into_bytes(),
        },
        None => Preview {
            kind: Kind::Hex,
            mime: "",
            size,
            body: bytes,
        },
    }
}

/// One preview read at a time per window; a new request cancels the one before it.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    current: Mutex<Option<CancelToken>>,
    run: Mutex<()>,
}

impl App {
    /// Preview row `row` of view `generation` of session `id` (E5-6): a folder gives its entry
    /// count; a file is read once, at most [`PREVIEW_CAP`] bytes (a larger one is refused,
    /// naming the cap) and classified. A newer preview cancels this one (`token` is theirs to
    /// cancel too); only one read runs at a time and none queues behind a heavy operation. A
    /// password the content needs is asked of `prompter` (the session is reopened with it).
    pub fn preview(
        &self,
        id: u64,
        row: u32,
        generation: u32,
        token: &CancelToken,
        prompter: &dyn Prompter,
    ) -> Result<Preview, GuiError> {
        let slot = self.slot(id)?;
        let (entry, kind, child_count) = {
            let b = lock(&slot.browser);
            b.check_generation(generation)?;
            let entry = b.id_at(row)?;
            let r = b.model().row(entry);
            (
                entry,
                r.map(|r| r.kind),
                r.map_or(0, |r| u64::from(r.child_count)),
            )
        };
        match kind {
            Some(1) => {
                return Ok(Preview {
                    kind: Kind::Folder,
                    mime: "",
                    size: child_count,
                    body: Vec::new(),
                })
            }
            Some(0) => {}
            _ => return Err(GuiError::usage("only a file or a folder can be previewed")),
        }
        // Supersede the read before this one, then wait for it to let go.
        if let Some(old) = lock(&self.gate.current).replace(token.clone()) {
            old.cancel();
        }
        let _one = lock(&self.gate.run);
        if token.is_cancelled() {
            return Err(lpk_api::Error::cancelled().into());
        }
        loop {
            let session = slot.session();
            match session.read_entry(entry, PREVIEW_CAP, token) {
                Ok(bytes) => return Ok(classify(bytes)),
                Err(e)
                    if matches!(e.class, Class::PasswordRequired | Class::WrongPassword)
                        && prompter.interactive() =>
                {
                    let wrong = e.class == Class::WrongPassword;
                    let fresh =
                        open_unlocked(session.path(), self.motw_off(), prompter, Some(wrong))?;
                    let old = std::mem::replace(&mut *lock(&slot.cell), Arc::new(fresh));
                    let _ = old.close();
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_table() {
        let mut webp = b"RIFF\x10\0\0\0WEBPVP8 ".to_vec();
        webp.extend_from_slice(&[0; 8]);
        let cases: Vec<(Vec<u8>, Kind, &str)> = vec![
            (b"\x89PNG\r\n\x1a\n....".to_vec(), Kind::Image, "image/png"),
            (b"\xff\xd8\xff\xe0jfif".to_vec(), Kind::Image, "image/jpeg"),
            (b"GIF89a....".to_vec(), Kind::Image, "image/gif"),
            (b"GIF87a....".to_vec(), Kind::Image, "image/gif"),
            (webp, Kind::Image, "image/webp"),
            (
                b"BM\x36\0\0\0\0\0\0\0\x36\0\0\0".to_vec(),
                Kind::Image,
                "image/bmp",
            ),
            (b"BMW is a car maker".to_vec(), Kind::Text, ""),
            (b"\0\0\x01\0\x01\0".to_vec(), Kind::Image, "image/x-icon"),
            (b"<svg><script>1</script></svg>".to_vec(), Kind::Text, ""),
            (b"<html><script>1</script></html>".to_vec(), Kind::Text, ""),
            (b"plain".to_vec(), Kind::Text, ""),
            (b"\xef\xbb\xbfbom".to_vec(), Kind::Text, ""),
            (b"\xff\xfeh\0i\0".to_vec(), Kind::Text, ""),
            (b"\xfe\xff\0h\0i".to_vec(), Kind::Text, ""),
            (b"a\0b".to_vec(), Kind::Hex, ""),
            (b"\xff\xfe\xfd".to_vec(), Kind::Hex, ""),
            (b"\xff\xfeh".to_vec(), Kind::Hex, ""),
            (b"\xff\xfe\x00\xd8".to_vec(), Kind::Hex, ""),
            (Vec::new(), Kind::Text, ""),
            // An HTML file named .png is judged by content: the name is not an input.
            (b"<html>not a png</html>".to_vec(), Kind::Text, ""),
        ];
        for (bytes, kind, mime) in cases {
            let p = classify(bytes.clone());
            assert_eq!((p.kind, p.mime), (kind, mime), "{bytes:?}");
        }
        assert_eq!(classify(b"\xef\xbb\xbfbom".to_vec()).body, b"bom");
        assert_eq!(classify(b"\xff\xfeh\0i\0".to_vec()).body, b"hi");
        assert_eq!(classify(b"\xfe\xff\0h\0i".to_vec()).body, b"hi");
    }

    #[test]
    fn hidden_and_bidi_characters_are_escaped() {
        let all: Vec<char> = [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{200B}', '\u{200C}',
            '\u{200D}', '\u{2060}', '\u{2061}', '\u{2062}', '\u{2063}', '\u{2064}', '\u{00AD}',
            '\u{FEFF}', '\u{0001}', '\u{001B}', '\u{007F}', '\u{0085}', '\u{009F}',
        ]
        .to_vec();
        for c in all {
            let s = format!("a{c}b");
            assert_eq!(
                escape_hidden(&s),
                format!("a\\u{{{:04X}}}b", c as u32),
                "{c:?}"
            );
        }
        assert_eq!(
            escape_hidden("tab\tlf\ncr\r ok ü 漢"),
            "tab\tlf\ncr\r ok ü 漢"
        );
        // A BOM at the start is dropped; one inside is escaped.
        let p = classify("\u{FEFF}x\u{FEFF}y".as_bytes().to_vec());
        assert_eq!(p.body, b"x\\u{FEFF}y");
        // The text of a right-to-left override file name trick.
        let p = classify("evil\u{202E}gpj.exe".as_bytes().to_vec());
        assert_eq!(p.body, b"evil\\u{202E}gpj.exe");
    }

    #[test]
    fn the_message_has_the_documented_layout() {
        let m = Preview {
            kind: Kind::Image,
            mime: "image/png",
            size: 7,
            body: vec![1, 2, 3],
        }
        .encode();
        assert_eq!(&m[..4], MAGIC);
        assert_eq!((m[4], m[5] as usize), (1, 9));
        assert_eq!(u64::from_le_bytes(m[6..14].try_into().unwrap()), 7);
        assert_eq!(&m[14..23], b"image/png");
        assert_eq!(&m[23..], &[1, 2, 3]);
    }
}
