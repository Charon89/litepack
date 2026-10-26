//! The `ci` profile: a generated fixture set for the size-regression gate (PLAN E8-2).
//!
//! `lpk-bench corpus build --profile ci [--out DIR]` writes it without any network access, from
//! the fixed seed [`SEED`], with in-tree, integer-only generators (D-18: the in-tree JPEG encoder,
//! the in-tree ZIP writer, the bundled static zlib), so the same bytes come out on every machine.
//! Every file and directory gets the fixed modification time [`MTIME_SECS`] and plain attributes
//! (Unix: mode 0644 for files and 0755 for directories; Windows: no read-only, hidden or system
//! bit; no name starts with a dot), so an archiver that records metadata writes the same archive
//! on Windows and on Linux. The golden test `ci_manifest_is_pinned` pins the BLAKE3 of the
//! rendered manifest, so the Windows and Linux CI jobs fail on any byte difference.
//!
//! The classes exercise every peel and fold the Fast and Balanced tiers ship:
//!
//! | class        | what                                                                      |
//! |--------------|---------------------------------------------------------------------------|
//! | `text-prose` | generated prose (Zipf-distributed vocabulary)                             |
//! | `text-bwt`   | phrase-structured prose, one large file: text that favours the `bwt` stage |
//! | `logs`       | service log lines                                                         |
//! | `csv-json`   | CSV tables and the same records as JSON and JSON lines                    |
//! | `png`        | PNGs with filtered rows (each filter type and a mix), zlib levels 1, 6, 9 |
//! | `jpeg`       | baseline and progressive JPEGs from the in-tree encoder                   |
//! | `office`     | DOCX and XLSX (Deflate in a ZIP/OOXML container), zlib 1, 6, 9, miniz     |
//! | `pdf`        | PDFs whose content streams are FlateDecode at zlib levels 1, 6 and 9      |
//! | `dups`       | exact duplicates and near duplicates (line edits, a shifted copy)         |
//! | `versions`   | a source tree in three sibling version roots                              |
//! | `small-files`| many small text, JSON and CSV files                                       |
//! | `random`     | incompressible bytes (the store path)                                     |
//!
//! Files go to `<out>/<class>/gen/<path>` with `manifest.json` and `build-info.json`, as the
//! public profiles do (source id `gen`, licence `CC0-1.0`). The output directory is owned through
//! the same `.lpk-corpus` marker; every build removes everything below it and writes it again.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};

use super::build::MARKER;
use super::derive::jpegenc;
use super::derive::zips::{write_zip, Encoder, Entry};
use super::derive::SplitMix64;
use super::manifest::{now_rfc3339, rfc3339_utc, BuildInfo, Manifest, ManifestFile, Summary};
use super::registry::Profile;

/// Seed of every generator of the `ci` profile.
pub const SEED: u64 = 0x4C50_4B2D_4349_0001;
/// Modification time of every file and directory: 2024-01-01T00:00:00Z.
pub const MTIME_SECS: u64 = 1_704_067_200;
/// Source id of every file (the directory level below the class).
pub const SOURCE: &str = "gen";
/// Licence of every file: generated here, no third-party content.
pub const LICENCE: &str = "CC0-1.0";

/// One generated file: class, path below `<class>/gen/`, bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    pub class: &'static str,
    pub path: String,
    pub data: Vec<u8>,
}

/// What a build wrote.
#[derive(Debug, Clone)]
pub struct Built {
    pub manifest_blake3: String,
    pub summary: Summary,
}

// ---------------------------------------------------------------------------------------------
// Words and text

const COMMON: [&str; 32] = [
    "the", "of", "and", "to", "in", "a", "is", "that", "for", "it", "as", "with", "was", "on",
    "be", "by", "this", "are", "or", "from", "at", "which", "not", "have", "an", "but", "they",
    "had", "one", "all", "were", "we",
];
const ONSETS: [&str; 20] = [
    "b", "c", "d", "f", "g", "h", "l", "m", "n", "p", "r", "s", "t", "v", "w", "st", "tr", "ch",
    "pl", "gr",
];
const VOWELS: [&str; 7] = ["a", "e", "i", "o", "u", "ea", "ou"];
const CODAS: [&str; 8] = ["", "", "n", "r", "s", "t", "nd", "ll"];

/// A deterministic vocabulary: the common words, then generated words, all distinct.
fn vocabulary(rng: &mut SplitMix64, n: usize) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<String> = Vec::with_capacity(n);
    for w in COMMON {
        seen.insert(w.to_string());
        out.push(w.to_string());
    }
    while out.len() < n {
        let syl = 1 + rng.below(3) as usize;
        let mut w = String::new();
        for _ in 0..syl {
            w.push_str(ONSETS[rng.below(ONSETS.len() as u64) as usize]);
            w.push_str(VOWELS[rng.below(VOWELS.len() as u64) as usize]);
        }
        w.push_str(CODAS[rng.below(CODAS.len() as u64) as usize]);
        if seen.insert(w.clone()) {
            out.push(w);
        }
    }
    out
}

/// Zipf sampling over a vocabulary with integer weights (no floating point).
struct Zipf {
    cumulative: Vec<u64>,
}

impl Zipf {
    fn new(n: usize) -> Zipf {
        let mut total = 0u64;
        let cumulative = (0..n)
            .map(|r| {
                total += 1_000_000 / (r as u64 + 1);
                total
            })
            .collect();
        Zipf { cumulative }
    }

    fn sample(&self, rng: &mut SplitMix64) -> usize {
        let total = *self.cumulative.last().unwrap_or(&1);
        let x = rng.below(total);
        self.cumulative.partition_point(|c| *c <= x)
    }
}

struct Words {
    vocab: Vec<String>,
    zipf: Zipf,
}

impl Words {
    fn new(rng: &mut SplitMix64) -> Words {
        let vocab = vocabulary(rng, 4000);
        let zipf = Zipf::new(vocab.len());
        Words { vocab, zipf }
    }

    fn word(&self, rng: &mut SplitMix64) -> &str {
        &self.vocab[self.zipf.sample(rng)]
    }

    fn sentence(&self, rng: &mut SplitMix64) -> String {
        let n = 5 + rng.below(14) as usize;
        let mut s = String::new();
        for i in 0..n {
            let w = self.word(rng);
            if i == 0 {
                let mut c = w.chars();
                if let Some(f) = c.next() {
                    s.extend(f.to_uppercase());
                    s.push_str(c.as_str());
                }
            } else {
                s.push(' ');
                s.push_str(w);
                if i + 1 < n && rng.below(9) == 0 {
                    s.push(',');
                }
            }
        }
        s.push(if rng.below(10) == 0 { '?' } else { '.' });
        s
    }

    /// Prose of about `bytes` bytes: paragraphs of sentences, lines wrapped at 76 columns.
    fn prose(&self, rng: &mut SplitMix64, bytes: usize) -> String {
        let mut out = String::new();
        while out.len() < bytes {
            let mut line = String::new();
            for _ in 0..3 + rng.below(6) {
                for w in self.sentence(rng).split(' ') {
                    if line.len() + w.len() + 1 > 76 {
                        out.push_str(&line);
                        out.push('\n');
                        line.clear();
                    }
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(w);
                }
            }
            out.push_str(&line);
            out.push_str("\n\n");
        }
        out
    }
}

/// Phrase-structured prose: sentences assembled from a fixed pool of phrases with occasional
/// substitutions, so long contexts recur far apart (what the BWT stage is good at).
fn phrase_prose(words: &Words, rng: &mut SplitMix64, bytes: usize) -> String {
    let phrases: Vec<Vec<String>> = (0..600)
        .map(|_| {
            (0..3 + rng.below(6))
                .map(|_| words.word(rng).to_string())
                .collect()
        })
        .collect();
    let mut out = String::new();
    let mut col = 0usize;
    while out.len() < bytes {
        let parts = 2 + rng.below(3);
        for p in 0..parts {
            let phrase = &phrases[rng.below(phrases.len() as u64) as usize];
            for (i, w) in phrase.iter().enumerate() {
                let w = if rng.below(12) == 0 {
                    words.word(rng)
                } else {
                    w.as_str()
                };
                let first = p == 0 && i == 0;
                if col + w.len() + 1 > 72 {
                    out.push('\n');
                    col = 0;
                } else if !(first && col == 0) {
                    out.push(' ');
                    col += 1;
                }
                if first {
                    let mut c = w.chars();
                    if let Some(f) = c.next() {
                        out.extend(f.to_uppercase());
                        out.push_str(c.as_str());
                    }
                } else {
                    out.push_str(w);
                }
                col += w.len();
            }
            if p + 1 < parts {
                out.push(',');
                col += 1;
            }
        }
        out.push('.');
        col += 1;
    }
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------------------------
// Logs, CSV, JSON

const LEVELS: [&str; 4] = ["INFO", "INFO", "WARN", "DEBUG"];
const SERVICES: [&str; 5] = ["api", "auth", "billing", "search", "worker"];
const PATHS: [&str; 6] = [
    "/api/v1/users",
    "/api/v1/orders",
    "/api/v1/items",
    "/login",
    "/health",
    "/api/v2/search",
];

fn log_lines(rng: &mut SplitMix64, start: u64, bytes: usize) -> String {
    let mut out = String::new();
    let mut t = start * 1000;
    while out.len() < bytes {
        t += 1 + rng.below(900);
        let ts = rfc3339_utc(t / 1000);
        let svc = SERVICES[rng.below(SERVICES.len() as u64) as usize];
        let level = if rng.below(50) == 0 {
            "ERROR"
        } else {
            LEVELS[rng.below(LEVELS.len() as u64) as usize]
        };
        let path = PATHS[rng.below(PATHS.len() as u64) as usize];
        let status = match rng.below(20) {
            0 => 500,
            1 | 2 => 404,
            3 => 302,
            _ => 200,
        };
        out.push_str(&format!(
            "{}.{:03}Z host-{:02} {svc}[{}]: {level} req={:016x} {path}/{} status={status} dur_ms={} bytes={}\n",
            ts.trim_end_matches('Z'),
            t % 1000,
            rng.below(12),
            1000 + rng.below(40),
            rng.next_u64(),
            rng.below(5000),
            rng.below(400),
            rng.below(60_000)
        ));
    }
    out
}

const CITIES: [&str; 8] = [
    "Lisbon", "Oslo", "Kyiv", "Toronto", "Osaka", "Lima", "Nairobi", "Perth",
];
const STATUS: [&str; 4] = ["paid", "open", "shipped", "refunded"];

struct Record {
    id: u64,
    date: String,
    customer: String,
    city: &'static str,
    cents: u64,
    status: &'static str,
    note: String,
}

fn records(words: &Words, rng: &mut SplitMix64, n: usize) -> Vec<Record> {
    (0..n)
        .map(|i| Record {
            id: 100_000 + i as u64,
            date: rfc3339_utc(MTIME_SECS - rng.below(86_400 * 365))[..10].to_string(),
            customer: format!("{} {}", words.word(rng), words.word(rng)),
            city: CITIES[rng.below(CITIES.len() as u64) as usize],
            cents: rng.below(500_000),
            status: STATUS[rng.below(STATUS.len() as u64) as usize],
            note: (0..rng.below(6))
                .map(|_| words.word(rng))
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect()
}

fn csv(recs: &[Record]) -> String {
    let mut out = String::from("id,date,customer,city,amount,status,note\n");
    for r in recs {
        out.push_str(&format!(
            "{},{},\"{}\",{},{}.{:02},{},\"{}\"\n",
            r.id,
            r.date,
            r.customer,
            r.city,
            r.cents / 100,
            r.cents % 100,
            r.status,
            r.note
        ));
    }
    out
}

fn json_object(r: &Record) -> String {
    format!(
        "{{\"id\": {}, \"date\": \"{}\", \"customer\": \"{}\", \"city\": \"{}\", \"amount\": {}.{:02}, \"status\": \"{}\", \"note\": \"{}\"}}",
        r.id,
        r.date,
        r.customer,
        r.city,
        r.cents / 100,
        r.cents % 100,
        r.status,
        r.note
    )
}

// ---------------------------------------------------------------------------------------------
// Images

/// A synthetic RGB picture: gradients, discs and a little noise (so the filters matter).
fn picture(rng: &mut SplitMix64, w: usize, h: usize, noise: u64) -> Vec<u8> {
    let discs: Vec<(i64, i64, i64, [u8; 3])> = (0..6)
        .map(|_| {
            (
                rng.below(w as u64) as i64,
                rng.below(h as u64) as i64,
                8 + rng.below((w / 4) as u64) as i64,
                [
                    rng.below(256) as u8,
                    rng.below(256) as u8,
                    rng.below(256) as u8,
                ],
            )
        })
        .collect();
    let phase = rng.below(256) as usize;
    let mut v = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let mut px = [
                ((x * 255 / w) + phase) as u8,
                (y * 255 / h) as u8,
                (((x + y) * 128 / (w + h)) + 64) as u8,
            ];
            for (cx, cy, r, c) in &discs {
                let (dx, dy) = (x as i64 - cx, y as i64 - cy);
                if dx * dx + dy * dy <= r * r {
                    px = *c;
                }
            }
            for p in &mut px {
                let n = if noise == 0 { 0 } else { rng.below(noise) };
                *p = p.wrapping_add(n as u8);
            }
            v.extend_from_slice(&px);
        }
    }
    v
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// An 8-bit RGB PNG whose row `y` is filtered with `filter(y)` (0..=4) and whose IDAT is one
/// zlib stream at `level`.
fn encode_png(rgb: &[u8], w: usize, h: usize, level: u32, filter: impl Fn(usize) -> u8) -> Vec<u8> {
    const BPP: usize = 3;
    let stride = w * BPP;
    let mut raw = Vec::with_capacity((stride + 1) * h);
    let zero = vec![0u8; stride];
    for y in 0..h {
        let row = &rgb[y * stride..(y + 1) * stride];
        let up = if y == 0 {
            &zero[..]
        } else {
            &rgb[(y - 1) * stride..y * stride]
        };
        let f = filter(y) % 5;
        raw.push(f);
        for i in 0..stride {
            let a = if i >= BPP { row[i - BPP] } else { 0 };
            let b = up[i];
            let c = if i >= BPP { up[i - BPP] } else { 0 };
            let pred = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                _ => paeth(a, b, c),
            };
            raw.push(row[i].wrapping_sub(pred));
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
    // Writing into a Vec cannot fail.
    let _ = z.write_all(&raw);
    let idat = z.finish().unwrap_or_default();
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &idat);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

// ---------------------------------------------------------------------------------------------
// Containers

fn zlib(data: &[u8], level: u32) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
    let _ = z.write_all(data);
    z.finish().unwrap_or_default()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn docx(words: &Words, rng: &mut SplitMix64, paragraphs: usize, enc: Encoder) -> Result<Vec<u8>> {
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>",
    );
    for i in 0..paragraphs {
        body.push_str("<w:p><w:pPr><w:pStyle w:val=\"");
        body.push_str(if i % 9 == 0 { "Heading1" } else { "Normal" });
        body.push_str(
            "\"/></w:pPr><w:r><w:rPr><w:lang w:val=\"en-GB\"/></w:rPr><w:t xml:space=\"preserve\">",
        );
        for _ in 0..1 + rng.below(4) {
            body.push_str(&xml_escape(&words.sentence(rng)));
            body.push(' ');
        }
        body.push_str("</w:t></w:r></w:p>");
    }
    body.push_str("<w:sectPr/></w:body></w:document>");
    let entries = vec![
        Entry {
            name: "[Content_Types].xml".into(),
            data: b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>".to_vec(),
        },
        Entry {
            name: "_rels/.rels".into(),
            data: b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>".to_vec(),
        },
        Entry {
            name: "word/document.xml".into(),
            data: body.into_bytes(),
        },
        Entry {
            name: "docProps/core.xml".into(),
            data: b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>Generated</dc:title><dc:creator>lpk-bench</dc:creator></cp:coreProperties>".to_vec(),
        },
    ];
    write_zip(&entries, enc)
}

fn xlsx(recs: &[Record], enc: Encoder) -> Result<Vec<u8>> {
    let mut sheet = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
    );
    for (i, r) in recs.iter().enumerate() {
        let n = i + 1;
        sheet.push_str(&format!(
            "<row r=\"{n}\"><c r=\"A{n}\"><v>{}</v></c><c r=\"B{n}\" t=\"inlineStr\"><is><t>{}</t></is></c><c r=\"C{n}\" t=\"inlineStr\"><is><t>{}</t></is></c><c r=\"D{n}\" t=\"inlineStr\"><is><t>{}</t></is></c><c r=\"E{n}\"><v>{}.{:02}</v></c></row>",
            r.id, r.date, xml_escape(&r.customer), r.city, r.cents / 100, r.cents % 100
        ));
    }
    sheet.push_str("</sheetData></worksheet>");
    let entries = vec![
        Entry {
            name: "[Content_Types].xml".into(),
            data: b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/></Types>".to_vec(),
        },
        Entry {
            name: "_rels/.rels".into(),
            data: b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>".to_vec(),
        },
        Entry {
            name: "xl/workbook.xml".into(),
            data: b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Orders\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>".to_vec(),
        },
        Entry {
            name: "xl/_rels/workbook.xml.rels".into(),
            data: b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_vec(),
        },
        Entry {
            name: "xl/worksheets/sheet1.xml".into(),
            data: sheet.into_bytes(),
        },
    ];
    write_zip(&entries, enc)
}

fn pdf_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

/// A PDF of `pages` text pages whose content streams are FlateDecode at zlib `level`.
fn pdf(words: &Words, rng: &mut SplitMix64, pages: usize, level: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    // Objects: 1 catalog, 2 pages, 3 font, then (page, content) pairs.
    let kids: Vec<String> = (0..pages).map(|p| format!("{} 0 R", 4 + 2 * p)).collect();
    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(
        &mut out,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        )
        .as_bytes(),
    );
    obj(
        &mut out,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    for p in 0..pages {
        obj(
            &mut out,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
                5 + 2 * p
            )
            .as_bytes(),
        );
        let mut content = String::from("BT\n/F1 10 Tf\n12 TL\n72 740 Td\n");
        for _ in 0..600 {
            let mut line = String::new();
            while line.len() < 80 {
                line.push_str(words.word(rng));
                line.push(' ');
            }
            content.push_str(&format!("({}) Tj T*\n", pdf_string(line.trim_end())));
        }
        content.push_str("ET\n");
        let z = zlib(content.as_bytes(), level);
        let mut body =
            format!("<< /Length {} /Filter /FlateDecode >>\nstream\n", z.len()).into_bytes();
        body.extend_from_slice(&z);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, &body);
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    out
}

// ---------------------------------------------------------------------------------------------
// Source trees (versions)

fn source_file(words: &Words, rng: &mut SplitMix64, functions: usize) -> String {
    let mut out = String::from("// Generated source file.\n\nuse std::collections::BTreeMap;\n\n");
    for _ in 0..functions {
        let name = format!("{}_{}", words.word(rng), words.word(rng));
        out.push_str(&format!(
            "/// {}\npub fn {name}(input: &[u8], limit: usize) -> Option<usize> {{\n",
            words.sentence(rng)
        ));
        for _ in 0..2 + rng.below(8) {
            let v = words.word(rng);
            match rng.below(4) {
                0 => out.push_str(&format!("    let {v}_count = input.len().min(limit);\n")),
                1 => out.push_str(&format!(
                    "    if input.first() == Some(&{}) {{\n        return None;\n    }}\n",
                    rng.below(256)
                )),
                2 => out.push_str(&format!(
                    "    let mut {v}: BTreeMap<usize, u8> = BTreeMap::new();\n    {v}.insert({}, {});\n",
                    rng.below(100),
                    rng.below(256)
                )),
                _ => out.push_str(&format!("    // {}\n", words.sentence(rng))),
            }
        }
        out.push_str(&format!("    Some(limit + {})\n}}\n\n", rng.below(1000)));
    }
    out
}

/// Edit about `edits` lines of `text`: replace, insert or delete one.
fn edit_lines(words: &Words, rng: &mut SplitMix64, text: &str, edits: usize) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for _ in 0..edits {
        if lines.is_empty() {
            break;
        }
        let at = rng.below(lines.len() as u64) as usize;
        match rng.below(3) {
            0 => lines[at] = format!("    // {}", words.sentence(rng)),
            1 => lines.insert(at, format!("    // {}", words.sentence(rng))),
            _ => {
                lines.remove(at);
            }
        }
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------------------------
// The set

/// Every file of the `ci` profile, in a fixed order.
pub fn generate() -> Result<Vec<Fixture>> {
    let mut rng = SplitMix64::new(SEED);
    let words = Words::new(&mut rng);
    let mut out: Vec<Fixture> = Vec::new();
    let mut put = |class: &'static str, path: String, data: Vec<u8>| {
        out.push(Fixture { class, path, data });
    };

    for i in 0..4 {
        put(
            "text-prose",
            format!("prose-{i}.txt"),
            words.prose(&mut rng, 256 * 1024).into_bytes(),
        );
    }
    put(
        "text-bwt",
        "phrases.txt".into(),
        phrase_prose(&words, &mut rng, 1536 * 1024).into_bytes(),
    );
    for i in 0..3u64 {
        put(
            "logs",
            format!("service-{i}.log"),
            log_lines(&mut rng, MTIME_SECS - 86_400 * (3 - i), 320 * 1024).into_bytes(),
        );
    }
    for i in 0..2 {
        let recs = records(&words, &mut rng, 3000);
        put(
            "csv-json",
            format!("orders-{i}.csv"),
            csv(&recs).into_bytes(),
        );
        let arr: Vec<String> = recs
            .iter()
            .map(|r| format!("  {}", json_object(r)))
            .collect();
        put(
            "csv-json",
            format!("orders-{i}.json"),
            format!("[\n{}\n]\n", arr.join(",\n")).into_bytes(),
        );
        let lines: String = recs.iter().map(|r| json_object(r) + "\n").collect();
        put("csv-json", format!("orders-{i}.jsonl"), lines.into_bytes());
    }

    // PNG: each filter type alone, then a per-row mix, at zlib levels 1, 6 and 9.
    let png_plan: [(usize, usize, u32, Option<u8>, u64); 7] = [
        (320, 240, 6, Some(0), 4),
        (320, 240, 6, Some(1), 4),
        (320, 240, 9, Some(2), 4),
        (320, 240, 1, Some(3), 4),
        (384, 256, 9, Some(4), 6),
        (384, 256, 6, None, 6),
        (256, 256, 1, None, 0),
    ];
    for (i, (w, h, level, filter, noise)) in png_plan.into_iter().enumerate() {
        let rgb = picture(&mut rng, w, h, noise);
        let tag = filter.map_or("mixed".to_string(), |f| format!("f{f}"));
        put(
            "png",
            format!("image-{i}-{tag}-z{level}.png"),
            encode_png(&rgb, w, h, level, |y| filter.unwrap_or((y % 5) as u8)),
        );
    }

    // JPEG: baseline and progressive at several qualities.
    for (i, (w, h, q, progressive)) in [
        (512usize, 384usize, 75u8, false),
        (480, 320, 85, false),
        (400, 300, 92, false),
        (512, 384, 75, true),
        (480, 320, 85, true),
        (400, 300, 92, true),
    ]
    .into_iter()
    .enumerate()
    {
        let rgb = picture(&mut rng, w, h, 10);
        let kind = if progressive {
            "progressive"
        } else {
            "baseline"
        };
        put(
            "jpeg",
            format!("photo-{i}-{kind}-q{q}.jpg"),
            jpegenc::encode(&rgb, w, h, q, progressive)?,
        );
    }

    for enc in [
        Encoder::Zlib(1),
        Encoder::Zlib(6),
        Encoder::Zlib(9),
        Encoder::Miniz(6),
    ] {
        put(
            "office",
            format!("report-{}.docx", enc.name()),
            docx(&words, &mut rng, 400, enc)?,
        );
    }
    for enc in [Encoder::Zlib(6), Encoder::Zlib(9)] {
        let recs = records(&words, &mut rng, 1500);
        put(
            "office",
            format!("orders-{}.xlsx", enc.name()),
            xlsx(&recs, enc)?,
        );
    }

    for level in [1u32, 6, 9] {
        put(
            "pdf",
            format!("paper-z{level}.pdf"),
            pdf(&words, &mut rng, 6, level),
        );
    }

    // Duplicates: exact copies under other names and directories, near copies with line edits,
    // and a copy shifted by an inserted header.
    let base_text = words.prose(&mut rng, 200 * 1024);
    let base_csv = csv(&records(&words, &mut rng, 2500));
    put(
        "dups",
        "originals/notes.txt".into(),
        base_text.clone().into_bytes(),
    );
    put(
        "dups",
        "originals/table.csv".into(),
        base_csv.clone().into_bytes(),
    );
    put(
        "dups",
        "copies/notes-copy.txt".into(),
        base_text.clone().into_bytes(),
    );
    put(
        "dups",
        "copies/deep/a/b/table.csv".into(),
        base_csv.clone().into_bytes(),
    );
    put(
        "dups",
        "near/notes-edited.txt".into(),
        edit_lines(&words, &mut rng, &base_text, 25).into_bytes(),
    );
    put(
        "dups",
        "near/table-edited.csv".into(),
        edit_lines(&words, &mut rng, &base_csv, 40).into_bytes(),
    );
    put(
        "dups",
        "near/notes-shifted.txt".into(),
        format!(
            "Revised edition.\n{}\n\n{base_text}",
            words.sentence(&mut rng)
        )
        .into_bytes(),
    );

    // Versions: one source tree in three sibling roots, each a few edits from the last.
    let mut tree: Vec<(String, String)> = (0..36)
        .map(|i| {
            let dir = ["core", "io", "net", "util"][i % 4];
            (
                format!("src/{dir}/module_{i:02}.rs"),
                source_file(&words, &mut rng, 4 + (i % 7)),
            )
        })
        .collect();
    tree.push(("README.md".into(), words.prose(&mut rng, 6 * 1024)));
    for v in ["v1.0", "v1.1", "v1.2"] {
        if v != "v1.0" {
            for (_, text) in tree.iter_mut() {
                if rng.below(3) == 0 {
                    let edits = 1 + rng.below(4) as usize;
                    *text = edit_lines(&words, &mut rng, text, edits);
                }
            }
        }
        for (path, text) in &tree {
            put("versions", format!("{v}/{path}"), text.clone().into_bytes());
        }
    }

    // Many small files.
    let small_recs = records(&words, &mut rng, 400);
    for i in 0..1500usize {
        let dir = format!("batch-{:02}", i / 100);
        let (name, data) = match i % 4 {
            0 => {
                let len = 60 + rng.below(900) as usize;
                (
                    format!("{dir}/event-{i:04}.log"),
                    log_lines(&mut rng, MTIME_SECS + i as u64 * 60, len),
                )
            }
            1 => (
                format!("{dir}/record-{i:04}.json"),
                json_object(&small_recs[i % small_recs.len()]) + "\n",
            ),
            2 => {
                let start = i % (small_recs.len() - 8);
                (
                    format!("{dir}/rows-{i:04}.csv"),
                    csv(&small_recs[start..start + 1 + rng.below(8) as usize]),
                )
            }
            _ => (
                format!("{dir}/note-{i:04}.txt"),
                words.sentence(&mut rng) + "\n",
            ),
        };
        put("small-files", name, data.into_bytes());
    }

    // Incompressible bytes.
    for i in 0..2 {
        let data: Vec<u8> = (0..(768 * 1024) / 8)
            .flat_map(|_| rng.next_u64().to_le_bytes())
            .collect();
        put("random", format!("noise-{i}.bin"), data);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Writing

fn set_mtime_file(path: &Path) -> Result<()> {
    let t = UNIX_EPOCH + Duration::from_secs(MTIME_SECS);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.set_modified(t)
        .with_context(|| format!("setting the time of {}", path.display()))
}

#[cfg(windows)]
fn set_mtime_dir(path: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let t = UNIX_EPOCH + Duration::from_secs(MTIME_SECS);
    let f = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.set_modified(t)
        .with_context(|| format!("setting the time of {}", path.display()))
}

#[cfg(not(windows))]
fn set_mtime_dir(path: &Path) -> Result<()> {
    let t = UNIX_EPOCH + Duration::from_secs(MTIME_SECS);
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    f.set_modified(t)
        .with_context(|| format!("setting the time of {}", path.display()))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting the mode of {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// Every directory below `root` (not `root` itself), deepest first.
fn dirs_deepest_first(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut all = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let e = e?;
            if e.file_type()?.is_dir() {
                all.push(e.path());
                stack.push(e.path());
            }
        }
    }
    all.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    Ok(all)
}

/// Claim `out` (refusing a foreign non-empty directory), clear it except for the marker, write
/// every fixture with fixed metadata, then `manifest.json` and `build-info.json`.
pub fn build(out: &Path) -> Result<Built> {
    if out.exists() {
        ensure!(
            out.is_dir(),
            "{} exists and is not a directory",
            out.display()
        );
        let has_files = std::fs::read_dir(out).is_ok_and(|mut d| d.next().is_some());
        if !out.join(MARKER).exists() && has_files {
            bail!(
                "{} is not empty and was not created by lpk-bench (no {MARKER} marker); \
                 refusing to touch it. Use a new or empty directory.",
                out.display()
            );
        }
        for e in std::fs::read_dir(out)? {
            let e = e?;
            if e.file_name() == MARKER {
                continue;
            }
            let p = e.path();
            if e.file_type()?.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            }
            .with_context(|| format!("removing {}", p.display()))?;
        }
    }
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    std::fs::write(
        out.join(MARKER),
        "lpk-bench corpus output. A build of the ci profile deletes everything else here.\n",
    )?;

    let fixtures = generate()?;
    let mut files = Vec::with_capacity(fixtures.len());
    for f in &fixtures {
        let rel = format!("{}/{SOURCE}/{}", f.class, f.path);
        let path = out.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, &f.data).with_context(|| format!("writing {}", path.display()))?;
        set_mode(&path, 0o644)?;
        set_mtime_file(&path)?;
        files.push((
            f.class.to_string(),
            ManifestFile {
                blake3: blake3::hash(&f.data).to_hex().to_string(),
                bytes: f.data.len() as u64,
                licence: LICENCE.to_string(),
                path: rel,
                source: SOURCE.to_string(),
            },
        ));
    }
    for d in dirs_deepest_first(out)? {
        set_mode(&d, 0o755)?;
        set_mtime_dir(&d)?;
    }

    let manifest = Manifest::new(Profile::Ci, files);
    let rendered = manifest.render();
    let manifest_blake3 = blake3::hash(rendered.as_bytes()).to_hex().to_string();
    std::fs::write(out.join("manifest.json"), &rendered)?;
    let summary = Summary::of(&manifest);
    let info = BuildInfo {
        built_at: now_rfc3339(),
        host_arch: std::env::consts::ARCH.to_string(),
        host_os: std::env::consts::OS.to_string(),
        lpk_bench_version: env!("CARGO_PKG_VERSION").to_string(),
        manifest_blake3: manifest_blake3.clone(),
        profile: Profile::Ci.name().to_string(),
        skipped: Vec::new(),
        accounting: Vec::new(),
        summary: summary.clone(),
        tools: Default::default(),
        unavailable: Vec::new(),
    };
    std::fs::write(out.join("build-info.json"), info.render())?;
    Ok(Built {
        manifest_blake3,
        summary,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The BLAKE3 of the `ci` profile's manifest: pins every generated byte on every OS (the
    /// Windows and Linux CI jobs both run this). A deliberate generator change updates it, and
    /// the golden sizes (`bench/ci-golden.toml`) are refreshed in the same pull request.
    pub(crate) const MANIFEST_BLAKE3: &str =
        "15ceda8b57cabc749aa66b13b9f6c1a056b011ff3968cceea32f035451610d6e";

    fn manifest_of(fixtures: &[Fixture]) -> Manifest {
        Manifest::new(
            Profile::Ci,
            fixtures
                .iter()
                .map(|f| {
                    (
                        f.class.to_string(),
                        ManifestFile {
                            blake3: blake3::hash(&f.data).to_hex().to_string(),
                            bytes: f.data.len() as u64,
                            licence: LICENCE.to_string(),
                            path: format!("{}/{SOURCE}/{}", f.class, f.path),
                            source: SOURCE.to_string(),
                        },
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn ci_manifest_is_pinned() {
        let m = manifest_of(&generate().expect("generate"));
        let got = blake3::hash(m.render().as_bytes()).to_hex().to_string();
        assert_eq!(got, MANIFEST_BLAKE3, "the ci fixture set changed");
    }

    #[test]
    fn generation_repeats_exactly_and_names_are_portable() {
        let a = generate().expect("a");
        let b = generate().expect("b");
        assert_eq!(a, b);
        let mut seen = std::collections::BTreeSet::new();
        for f in &a {
            let full = format!("{}/{}", f.class, f.path);
            assert!(seen.insert(full.to_ascii_lowercase()), "duplicate {full}");
            assert!(
                full.bytes().all(|c| c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || b"/-_.".contains(&c)
                    || c.is_ascii_uppercase()
                    || c == b'['
                    || c == b']'),
                "{full}"
            );
            assert!(!full.split('/').any(|c| c.starts_with('.')), "{full}");
        }
    }

    #[test]
    fn every_class_is_present_and_the_set_stays_small() {
        let m = manifest_of(&generate().expect("generate"));
        let classes: Vec<&str> = m.classes.keys().map(String::as_str).collect();
        assert_eq!(
            classes,
            [
                "csv-json",
                "dups",
                "jpeg",
                "logs",
                "office",
                "pdf",
                "png",
                "random",
                "small-files",
                "text-bwt",
                "text-prose",
                "versions"
            ]
        );
        let total: u64 = m.classes.values().map(|c| c.bytes_total).sum();
        assert!(total < 24 * 1024 * 1024, "{total} bytes");
        assert!(m.classes["small-files"].files.len() >= 1000);
    }

    #[test]
    fn the_containers_are_readable_and_use_several_levels() {
        let all = generate().expect("generate");
        let by = |class: &'static str| all.iter().filter(move |f| f.class == class);
        for f in by("png") {
            let dec = png::Decoder::new(std::io::Cursor::new(&f.data[..]));
            let mut r = dec.read_info().expect("png header");
            let mut buf = vec![0; r.output_buffer_size().expect("size")];
            r.next_frame(&mut buf).expect("png frame");
        }
        for f in by("jpeg") {
            jpegenc::decode_rgb(&f.data).expect("jpeg");
            let sof = if f.path.contains("progressive") {
                0xC2
            } else {
                0xC0
            };
            assert!(f.data.windows(2).any(|w| w == [0xFF, sof]), "{}", f.path);
        }
        for f in by("office") {
            let z = zip::ZipArchive::new(std::io::Cursor::new(&f.data[..])).expect("zip");
            assert!(z.len() >= 4);
        }
        let pdfs: Vec<&Fixture> = by("pdf").collect();
        assert_eq!(pdfs.len(), 3);
        let mut first_streams = std::collections::BTreeSet::new();
        for f in pdfs {
            assert!(f.data.starts_with(b"%PDF-1.4") && f.data.ends_with(b"%%EOF\n"));
            let at = f
                .data
                .windows(7)
                .position(|w| w == b"stream\n")
                .expect("a stream");
            let mut d = flate2::read::ZlibDecoder::new(&f.data[at + 7..]);
            let mut text = Vec::new();
            std::io::Read::read_to_end(&mut d, &mut text).expect("inflate");
            assert!(text.starts_with(b"BT\n"));
            first_streams.insert(f.data[at + 7..at + 9].to_vec());
        }
        // zlib headers 78 01, 78 9C and 78 DA: three levels.
        assert_eq!(first_streams.len(), 3);
    }

    #[test]
    fn build_writes_fixed_times_and_a_manifest_and_refuses_foreign_dirs() {
        let dir = tempfile::tempdir().expect("tmp");
        let foreign = dir.path().join("foreign");
        std::fs::create_dir_all(&foreign).expect("mk");
        std::fs::write(foreign.join("x"), b"x").expect("w");
        assert!(build(&foreign).is_err());
        assert!(foreign.join("x").exists());

        let out = dir.path().join("ci");
        let built = build(&out).expect("build");
        // A second build over the first works (the marker owns the directory).
        let again = build(&out).expect("rebuild");
        assert_eq!(built.manifest_blake3, again.manifest_blake3);
        let text = std::fs::read_to_string(out.join("manifest.json")).expect("manifest");
        assert_eq!(
            blake3::hash(text.as_bytes()).to_hex().to_string(),
            built.manifest_blake3
        );
        let want = UNIX_EPOCH + Duration::from_secs(MTIME_SECS);
        let f = out.join("versions/gen/v1.1/README.md");
        assert_eq!(
            std::fs::metadata(&f).expect("md").modified().expect("t"),
            want
        );
        let d = out.join("versions/gen/v1.1/src");
        assert_eq!(
            std::fs::metadata(&d).expect("md").modified().expect("t"),
            want
        );
        assert_eq!(
            std::fs::metadata(out.join("versions"))
                .expect("md")
                .modified()
                .expect("t"),
            want
        );
        assert!(!std::fs::metadata(&f).expect("md").permissions().readonly());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(&f).expect("md").permissions().mode();
            assert_eq!(m & 0o777, 0o644);
        }
        let info: BuildInfo = serde_json::from_str(
            &std::fs::read_to_string(out.join("build-info.json")).expect("info"),
        )
        .expect("parse");
        assert_eq!(info.profile, "ci");
        assert_eq!(info.summary.total_files, built.summary.total_files);
    }
}
