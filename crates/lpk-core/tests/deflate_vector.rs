//! The revision 1.2 vector `deflate-peel.lpk` (in `crates/lpk-format/tests/vectors/`) and its
//! full-reader expectations (`expected-1.2.toml` beside it). Spec section 11 lists it; section 9
//! ("How a writer lays out a peeled Deflate stream") is the layout it follows; CONFORMANCE.md says
//! what a decoder must do with it.
//!
//! Inputs: the committed fixtures `doc.zip`, `image.png`, `notes.txt.gz` and `readme.txt` in
//! `tests/fixtures/deflate/`, made once (the ignored test `write_missing_fixtures` writes only the
//! ones that are missing) so the vector does not depend on the `flate2` backend. The builder uses
//! `lpk-core`'s walker and analysis and calls the format's writer directly with explicit encoders:
//! chunk size 4096 (fixed cut), block size 16 KiB, archive id 0x5A repeated, mtime 1000, minor 2,
//! no dedup; plain blocks zstd level 3 (window_log 17); `content.xml` and `image.png` as
//! `[lzma (dict 64 KiB, lc 3, lp 0, pb 2, end marker), deflate-reconstruct]`, `styles.xml` and
//! `notes.txt.gz` as `[zstd (level 3, window_log 17), deflate-reconstruct]`.
//!
//! The revision 1.0 expectations (listing, `info`, the `UnimplementedPrimitive` of `verify`,
//! `files_1_0`) are in `expected.toml`, written by lpk-format's conformance test. Regenerate with
//! `cargo test -p lpk-core --test deflate_vector -- --ignored regenerate`, then
//! `cargo test -p lpk-format --test conformance -- --ignored`; byte identity needs the same
//! liblzma, libzstd, preflate-rs, bitcode and cabac versions.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::io::{Cursor, Write as _};
use std::path::PathBuf;

use lpk_core::peel::deflate::{examine, memory_term, Limits};
use lpk_core::register_full_reader;
use lpk_format::{
    Archive, BlockEncoder, DeflateRecord, Encoded, EntryFlags, EntryKind, FormatError, Graph,
    PrimitiveId, Record, RecordBody, Resources, Step, Writer, WriterOptions,
};

const VECTOR: &str = "deflate-peel.lpk";
const EXPECTED: &str = "expected-1.2.toml";
/// The malformed revision 1.2 vector: `notes.txt.gz` as `[lzma, deflate-reconstruct]` whose
/// record's `plain_len` is the inflated length minus [`SHORT_BY`], and `readme.txt`.
const MALFORMED: &str = "malformed-deflate-bound.lpk";
const SHORT_BY: u64 = 10_000;
/// The fixtures, with the walker's extension, in the order they are written.
const INPUTS: [(&str, &str); 3] = [
    ("doc.zip", "zip"),
    ("image.png", "png"),
    ("notes.txt.gz", "gz"),
];
const README: &str = "readme.txt";
const MTIME: i64 = 1000;

fn vectors() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("lpk-format")
        .join("tests")
        .join("vectors")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("deflate")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).unwrap()
}

// ---- The fixtures (made once) ----

/// Deterministic pseudo-random bytes (xorshift32).
fn noise(n: usize, mut x: u32) -> Vec<u8> {
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

/// Deterministic text of `n` bytes from a word list.
fn words(n: usize, seed: u32) -> String {
    let list = [
        "archive", "block", "chunk", "deflate", "entry", "frame", "graph", "header", "index",
        "journal", "record", "stream", "trailer", "vector",
    ];
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    let mut s = String::new();
    let mut col = 0;
    while s.len() < n {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let w = list[(x >> 8) as usize % list.len()];
        s.push_str(w);
        col += w.len() + 1;
        if col > 72 {
            s.push('\n');
            col = 0;
        } else {
            s.push(' ');
        }
    }
    s.truncate(n);
    s
}

fn xml(n: usize, seed: u32) -> Vec<u8> {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document>\n");
    let mut i = 0u32;
    while s.len() < n {
        let line = words(40 + (i as usize * 7) % 60, seed + i).replace('\n', " ");
        let _ = writeln!(
            s,
            "  <text:p text:style-name=\"P{}\">{line}</text:p>",
            i % 9
        );
        i += 1;
    }
    s.push_str("</office:document>\n");
    s.into_bytes()
}

fn doc_zip() -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod::{Deflated, Stored};
    let base = SimpleFileOptions::default().last_modified_time(zip::DateTime::default());
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let files: [(&str, SimpleFileOptions, Vec<u8>); 4] = [
        (
            "mimetype",
            base.compression_method(Stored),
            b"application/vnd.oasis.opendocument.text".to_vec(),
        ),
        (
            "content.xml",
            base.compression_method(Deflated).compression_level(Some(6)),
            xml(40 * 1024, 1),
        ),
        (
            "styles.xml",
            base.compression_method(Deflated).compression_level(Some(9)),
            xml(8 * 1024, 2),
        ),
        ("thumb.bin", base.compression_method(Stored), noise(2048, 3)),
    ];
    for (name, opts, data) in files {
        w.start_file(name, opts).unwrap();
        w.write_all(&data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// 64x48 RGB, 8 bits, filter 0 on every row; its zlib stream split over three IDAT chunks.
fn image_png() -> Vec<u8> {
    let (w, h) = (64u32, 48u32);
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            raw.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8]);
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&raw).unwrap();
    let z = z.finish().unwrap();
    let chunk = |out: &mut Vec<u8>, ty: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(ty);
        out.extend_from_slice(data);
        let mut crc = flate2::Crc::new();
        crc.update(ty);
        crc.update(data);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    };
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    let third = z.len() / 3;
    chunk(&mut out, b"IDAT", &z[..third]);
    chunk(&mut out, b"IDAT", &z[third..2 * third]);
    chunk(&mut out, b"IDAT", &z[2 * third..]);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn notes_gz() -> Vec<u8> {
    let mut e = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::new(6));
    e.write_all(words(20 * 1024, 4).as_bytes()).unwrap();
    e.finish().unwrap()
}

fn readme() -> Vec<u8> {
    let mut s = String::from(
        "deflate-peel.lpk: a ZIP, a PNG and a gzip file with their Deflate streams peeled\n\
         (revision 1.2), and this text file, which every reader extracts.\n",
    );
    s.push_str(&words(1500, 5));
    s.push('\n');
    s.into_bytes()
}

#[test]
#[ignore = "writes the deflate-peel fixtures that are missing (never overwrites one)"]
fn write_missing_fixtures() {
    type Maker = fn() -> Vec<u8>;
    let made: [(&str, Maker); 4] = [
        ("doc.zip", doc_zip),
        ("image.png", image_png),
        ("notes.txt.gz", notes_gz),
        (README, readme),
    ];
    for (name, f) in made {
        let p = fixtures().join(name);
        if !p.exists() {
            std::fs::write(p, f()).unwrap();
        }
    }
}

// ---- The builder ----

/// zstd level 3, window_log 17, with a content checksum: the plain blocks and the zstd model step.
fn zstd_bytes(plain: &[u8]) -> Vec<u8> {
    let mut e = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    e.include_checksum(true).unwrap();
    e.set_parameter(zstd::zstd_safe::CParameter::WindowLog(17))
        .unwrap();
    e.write_all(plain).unwrap();
    e.finish().unwrap()
}

fn zstd_step() -> Step {
    let mut params = vec![17u8];
    params.extend_from_slice(&[0; 32]);
    Step {
        primitive: PrimitiveId::Zstd,
        params,
    }
}

/// Raw LZMA1, preset 6 with a 64 KiB dictionary, lc 3, lp 0, pb 2; liblzma's raw encoder ends
/// the stream with the end-of-payload marker (a step that is not the last needs it).
fn lzma_bytes(plain: &[u8]) -> Vec<u8> {
    use liblzma::stream::{Action, Filters, LzmaOptions, Status, Stream};
    let mut o = LzmaOptions::new_preset(6).unwrap();
    o.dict_size(1 << 16)
        .literal_context_bits(3)
        .literal_position_bits(0)
        .position_bits(2);
    let mut filters = Filters::new();
    filters.lzma1(&o);
    let mut s = Stream::new_raw_encoder(&filters).unwrap();
    let mut out = Vec::with_capacity(plain.len() + 1024);
    loop {
        let at = usize::try_from(s.total_in()).unwrap();
        out.reserve(1 << 16);
        if s.process_vec(&plain[at..], &mut out, Action::Finish)
            .unwrap()
            == Status::StreamEnd
        {
            break;
        }
    }
    out
}

fn lzma_step() -> Step {
    let mut params = (1u32 << 16).to_le_bytes().to_vec();
    params.extend_from_slice(&[3, 0, 2]);
    Step {
        primitive: PrimitiveId::Lzma,
        params,
    }
}

/// The plain blocks' encoder.
struct PlainZstd;
impl BlockEncoder for PlainZstd {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let graph = Graph {
            steps: vec![zstd_step()],
        };
        Ok(Encoded {
            resources: graph.resources(),
            graph,
            bytes: zstd_bytes(plain),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Model {
    Lzma,
    Zstd,
}

/// The model of each stream, by file and stream position: `content.xml` and `image.png` lzma,
/// `styles.xml` and `notes.txt.gz` zstd.
fn model_of(file: &str, i: usize) -> Model {
    match (file, i) {
        ("doc.zip", 0) | ("image.png", 0) => Model::Lzma,
        ("doc.zip", 1) | ("notes.txt.gz", 0) => Model::Zstd,
        _ => unreachable!("{file} stream {i}"),
    }
}

/// One peeled stream of a fixture: its spans in the file, its plain text and its record.
struct Peeled {
    spans: Vec<(usize, usize)>,
    plain: Vec<u8>,
    record: DeflateRecord,
}

fn peel(bytes: &[u8], ext: &str) -> Vec<Peeled> {
    let mut out = Vec::new();
    examine(bytes, ext, &Limits::standard(), |r| {
        let deflate: Vec<u8> = r
            .spans
            .iter()
            .flat_map(|&(a, b)| bytes[a..b].iter().copied())
            .collect();
        let plain = r.plain.text().to_vec();
        out.push(Peeled {
            record: DeflateRecord {
                original_len: deflate.len() as u64,
                plain_len: plain.len() as u64,
                corrections: r.corrections,
                library: 0,
                original_hash: *blake3::hash(&deflate).as_bytes(),
            },
            spans: r.spans,
            plain,
        });
    });
    out
}

/// The framing ranges of a file: everything outside the streams' spans, in file order.
fn framing(len: usize, streams: &[Peeled]) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = streams.iter().flat_map(|p| p.spans.clone()).collect();
    spans.sort_unstable();
    let mut out = Vec::new();
    let mut at = 0;
    for (a, b) in spans {
        if a > at {
            out.push((at, a));
        }
        at = b;
    }
    if at < len {
        out.push((at, len));
    }
    out
}

/// `deflate-peel.lpk`'s bytes, laid out as spec section 9 says.
fn build() -> Vec<u8> {
    build_from(&INPUTS, false)
}

/// `malformed-deflate-bound.lpk`'s bytes: the same writer, `notes.txt.gz` only, its stream
/// through lzma, its record's `plain_len` [`SHORT_BY`] below the inflated text (the writer does
/// not check `plain_len`; spec section 8 makes the reader's lzma step cross its bound).
fn build_malformed() -> Vec<u8> {
    build_from(&[("notes.txt.gz", "gz")], true)
}

fn build_from(inputs: &[(&str, &str)], malformed: bool) -> Vec<u8> {
    let options = WriterOptions {
        chunk_size: 4096,
        block_size: 16 * 1024,
        archive_id: [0x5A; 16],
        encoder: Box::new(PlainZstd),
        ..WriterOptions::default()
    };
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options, 2).unwrap();
    for &(name, ext) in inputs {
        let bytes = fixture(name);
        let streams = peel(&bytes, ext);
        let want = if name == "doc.zip" { 2 } else { 1 };
        assert_eq!(streams.len(), want, "{name}");
        w.begin_entry(name, EntryFlags::EMPTY, MTIME).unwrap();
        // 1. The framing, in file order, as plain parts.
        for (a, b) in framing(bytes.len(), &streams) {
            w.add_part(a as u64, &mut &bytes[a..b]).unwrap();
        }
        // 2. Per stream: the record, then the stream as a block of its own.
        for (i, s) in streams.iter().enumerate() {
            let mut record = s.record.clone();
            let model = if malformed {
                record.plain_len -= SHORT_BY;
                Model::Lzma
            } else {
                model_of(name, i)
            };
            let id = w
                .add_record(Record::new(RecordBody::Deflate(record.clone())))
                .unwrap();
            let mut params = Vec::new();
            lpk_format::varint::write(&mut params, id).unwrap();
            let (step, model_bytes) = match model {
                Model::Lzma => (lzma_step(), lzma_bytes(&s.plain)),
                Model::Zstd => (zstd_step(), zstd_bytes(&s.plain)),
            };
            let graph = Graph {
                steps: vec![
                    step,
                    Step {
                        primitive: PrimitiveId::DeflateReconstruct,
                        params,
                    },
                ],
            };
            let encoded = Encoded {
                resources: graph.resources(),
                graph,
                bytes: model_bytes,
            };
            let spans: Vec<(u64, u64)> = s
                .spans
                .iter()
                .map(|&(a, b)| (a as u64, (b - a) as u64))
                .collect();
            let deflate: Vec<u8> = s
                .spans
                .iter()
                .flat_map(|&(a, b)| bytes[a..b].iter().copied())
                .collect();
            let memory = memory_term(record.plain_len, record.corrections.len() as u64);
            w.add_parts_encoded(&spans, &deflate, encoded, memory)
                .unwrap();
        }
        w.end_entry().unwrap();
    }
    w.add_file(README, EntryFlags::EMPTY, MTIME, &mut &fixture(README)[..])
        .unwrap();
    w.finish().unwrap();
    out
}

// ---- Expectations ----

fn tool(args: &[&str]) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = lpk_format::cli::run_with(
        args.iter().copied(),
        &mut out,
        &mut err,
        register_full_reader::<std::fs::File>,
    );
    assert_eq!(String::from_utf8(err).unwrap(), "");
    (code, String::from_utf8(out).unwrap())
}

fn open(b: Vec<u8>) -> Archive<Cursor<Vec<u8>>> {
    let mut a = Archive::open(Cursor::new(b), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    a
}

fn render() -> String {
    let mut s = String::from(
        "# Expected results of a revision 1.2 reader (docs/spec/CONFORMANCE.md).\n\
         # Generated by lpk-core's full reader; do not edit. `files`: path size BLAKE3-256 of the\n\
         # extracted bytes, in entry-table order; `verify` is the stdout of `verify` (exit code 0).\n",
    );
    let path = vectors().join(VECTOR);
    let mut a = open(std::fs::read(&path).unwrap());
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    writeln!(s, "\n[\"{VECTOR}\"]").unwrap();
    writeln!(s, "files = [").unwrap();
    for e in &entries {
        let mut got = Vec::new();
        if e.kind == EntryKind::File {
            a.extract(e, &mut got).unwrap();
        }
        let line = format!("{} {} {}", e.path, got.len(), blake3::hash(&got).to_hex());
        writeln!(s, "  {line:?},").unwrap();
    }
    writeln!(s, "]").unwrap();
    let (code, out) = tool(&["lpk", "verify", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    writeln!(s, "verify = {out:?}").unwrap();
    // The malformed vector: `verify` of a revision 1.2 reader fails with the class and message.
    let mut a = open(std::fs::read(vectors().join(MALFORMED)).unwrap());
    let err = a.verify().unwrap_err();
    let class = format!("{err:?}");
    let class = class.split([' ', '{', '(']).next().unwrap();
    writeln!(s, "\n[\"{MALFORMED}\"]").unwrap();
    writeln!(s, "error = {class:?}").unwrap();
    writeln!(s, "message = {:?}", err.to_string()).unwrap();
    s
}

#[test]
fn the_vector_regenerates_identically() {
    let committed = std::fs::read(vectors().join(VECTOR)).unwrap();
    assert!(
        committed == build(),
        "{VECTOR} differs from this build's (liblzma, libzstd or preflate-rs version?)"
    );
}

#[test]
fn the_malformed_vector_regenerates_identically() {
    let committed = std::fs::read(vectors().join(MALFORMED)).unwrap();
    assert!(
        committed == build_malformed(),
        "{MALFORMED} differs from this build's"
    );
}

/// Spec section 8, "Bounds", and lzma rule 6 (task C fix round, R17): a revision 1.2 reader
/// reports `PayloadTooLarge` with `max` = the record's `plain_len`, from `verify` and from
/// extracting `notes.txt.gz`; `readme.txt` extracts.
#[test]
fn the_malformed_vector_is_payload_too_large() {
    let mut a = open(std::fs::read(vectors().join(MALFORMED)).unwrap());
    assert_eq!(a.header().version.minor, 2);
    let RecordBody::Deflate(d) = a
        .records()
        .unwrap()
        .unwrap()
        .table()
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .unwrap()
        .body
    else {
        panic!("not a deflate record")
    };
    let check = |e: FormatError| match e {
        FormatError::PayloadTooLarge { len, max } => {
            assert_eq!(max, d.plain_len);
            assert!(len > max);
        }
        other => panic!("{other:?}"),
    };
    check(a.verify().unwrap_err());
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    for e in &entries {
        let mut got = Vec::new();
        if e.path == README {
            a.extract(e, &mut got).unwrap();
            assert!(got == fixture(README));
        } else {
            check(a.extract(e, &mut got).unwrap_err());
        }
    }
}

#[test]
fn the_vector_extracts_to_the_fixtures_and_matches_its_expectations() {
    let want = std::fs::read_to_string(vectors().join(EXPECTED)).unwrap();
    assert_eq!(want.replace("\r\n", "\n"), render());
    for name in ["doc.zip", "image.png", "notes.txt.gz", README] {
        let b = fixture(name);
        let line = format!("{name} {} {}", b.len(), blake3::hash(&b).to_hex());
        assert!(want.contains(&line), "{line}\n{want}");
    }
}

/// What the vector proves (spec section 11, CONFORMANCE): minor 2; four `deflate` records; both
/// model steps as non-final steps; one inflated text larger than `max_block_plain` (the bound
/// rule); the PNG stream cut over three spans; the declared decode memory.
#[test]
fn the_vector_has_the_shape_it_claims() {
    let bytes = std::fs::read(vectors().join(VECTOR)).unwrap();
    let mut a = open(bytes.clone());
    assert_eq!(a.header().version.minor, 2);
    let env = a.index().envelope;
    let records: Vec<Record> = {
        let t = a.records().unwrap().unwrap();
        let t = t.table().unwrap();
        t.iter().map(|r| r.unwrap()).collect()
    };
    assert_eq!(records.len(), 4);
    let mut over_bound = 0;
    let mut largest_term = 0;
    for r in &records {
        let RecordBody::Deflate(d) = &r.body else {
            panic!("not a deflate record")
        };
        over_bound += usize::from(d.plain_len > env.max_block_plain);
        largest_term = largest_term.max(memory_term(d.plain_len, d.corrections.len() as u64));
    }
    assert!(over_bound >= 1, "an inflated text above max_block_plain");
    assert_eq!(env.decode_memory, env.max_block_plain + largest_term);
    // The PNG stream's chunks: three spans, framing between them in the entry.
    let png = peel(&fixture("image.png"), "png");
    assert_eq!(png[0].spans.len(), 3);
    // Graph shapes: two lzma and two zstd model steps, each before deflate-reconstruct.
    let mut models = Vec::new();
    let mut plain_blocks = 0;
    for b in a.index().blocks.clone() {
        let frame = &bytes[b.frame_offset as usize..(b.frame_offset + b.frame_len) as usize];
        let Some(lpk_format::frame::ReadFrame::Known(f)) =
            lpk_format::Frame::read(&mut Cursor::new(frame), &Default::default()).unwrap()
        else {
            panic!("not a block frame")
        };
        let steps = Graph::parse(&f.payload).unwrap().0.steps;
        match steps.as_slice() {
            [m, d] => {
                assert_eq!(d.primitive, PrimitiveId::DeflateReconstruct);
                models.push(m.primitive);
            }
            [p] => {
                assert_eq!(p.primitive, PrimitiveId::Zstd);
                plain_blocks += 1;
            }
            _ => panic!("unexpected graph"),
        }
    }
    models.sort_by_key(|p| *p as u16);
    assert_eq!(
        models,
        [
            PrimitiveId::Zstd,
            PrimitiveId::Zstd,
            PrimitiveId::Lzma,
            PrimitiveId::Lzma
        ]
    );
    assert!(plain_blocks >= 1);
}

#[test]
#[ignore = "rewrites deflate-peel.lpk, malformed-deflate-bound.lpk and expected-1.2.toml in crates/lpk-format/tests/vectors"]
fn regenerate() {
    std::fs::write(vectors().join(VECTOR), build()).unwrap();
    std::fs::write(vectors().join(MALFORMED), build_malformed()).unwrap();
    std::fs::write(vectors().join(EXPECTED), render()).unwrap();
}
