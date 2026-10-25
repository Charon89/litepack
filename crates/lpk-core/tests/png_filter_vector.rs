//! The revision 1.4 vector `png-filter.lpk` (in `crates/lpk-format/tests/vectors/`), the
//! malformed revision 1.4 vectors and their full-reader expectations (`expected-1.4.toml`
//! beside them). Spec section 11 lists them; section 8 ("Decoding `png-filter`") is the step;
//! CONFORMANCE.md says what a decoder must do with them.
//!
//! The builder calls the format's writer directly with explicit encoders: chunk size 4096 (fixed
//! cut), block size 8 KiB, archive id 0x5B repeated, mtime 1000, minor 4, no dedup; plain blocks
//! zstd level 3 (window_log 17). Each PNG's IDAT stream is one block `[M, png-filter {p},
//! deflate-reconstruct {d}]` (records `d` then `p` per image, in file order), the framing in
//! plain blocks before it, as section 9 lays a peeled PNG out:
//! - `image.png` (the walker's fixture, RGB8 64 × 48, filter None throughout, three IDATs): M
//!   `lzma`; the inflated text (9264 bytes) is above `max_block_plain` (the bound of task A);
//! - `sprite.png` (generated: RGB8 40 × 30, the five filter types in turn, two IDATs): M `lzma`;
//! - `grey2-adam7.png` (generated: 2-bit grey 400 × 3, Adam7, so pass 3 is empty): M
//!   `zstd`, and the step's input filtered with Paeth throughout instead of Up (a decoder undoes
//!   the input's own filter bytes);
//! - `rgba16.png` (generated: RGBA 16-bit 9 × 7, Average and Paeth rows): M `lzma`;
//! - `readme.txt`: a plain block, which every reader extracts (`files_1_0`).
//!
//! The step's input is otherwise the image filtered with Up throughout, as the reference writer
//! makes it. The malformed vectors each hold `sprite.png` with one fault, and `readme.txt`:
//! `bound` (the deflate record's `plain_len` one below the filtered length: `PayloadTooLarge`
//! before `lzma` runs), `width` (0), `height` (0, with no filter byte), `bit-depth` (4 with colour type 2),
//! `filters` (a filter byte 5), `scanlines` (the input one byte short), `hash` (the record's
//! `original_hash` with one bit flipped).
//!
//! The pre-1.4 expectations (listing, `info`, the `UnimplementedPrimitive` (9) of `verify`,
//! `files_1_0`) are in `expected.toml`, written by lpk-format's conformance test. Regenerate
//! with `cargo test -p lpk-core --test png_filter_vector -- --ignored regenerate`, then
//! `cargo test -p lpk-format --test conformance -- --ignored`; byte identity needs the same
//! zlib (flate2), liblzma, libzstd, preflate-rs, bitcode and cabac versions.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::io::{Cursor, Write as _};
use std::path::PathBuf;

use lpk_core::peel::deflate::{examine, memory_term, Limits};
use lpk_core::peel::png::{Header, Layout, FILTER_PAETH, FILTER_UP};
use lpk_core::register_full_reader;
use lpk_format::{
    Archive, BlockEncoder, DeflateRecord, Encoded, EntryFlags, EntryKind, FormatError, Graph,
    PngFilterRecord, PrimitiveId, Record, RecordBody, Resources, Step, Writer, WriterOptions,
};

const VECTOR: &str = "png-filter.lpk";
const EXPECTED: &str = "expected-1.4.toml";
const README: &str = "readme.txt";
const MTIME: i64 = 1000;
const BLOCK: usize = 8 * 1024;

/// The malformed vectors: name, fault.
const MALFORMED: [(&str, Fault); 10] = [
    ("malformed-png-filter-bound.lpk", Fault::Bound),
    ("malformed-png-filter-width.lpk", Fault::Width),
    ("malformed-png-filter-height.lpk", Fault::Height),
    ("malformed-png-filter-bit-depth.lpk", Fault::BitDepth),
    ("malformed-png-filter-filters.lpk", Fault::Filters),
    ("malformed-png-filter-scanlines.lpk", Fault::Scanlines),
    ("malformed-png-filter-hash.lpk", Fault::Hash),
    ("malformed-png-filter-lookahead.lpk", Fault::Lookahead),
    ("malformed-png-filter-kind.lpk", Fault::Kind),
    ("malformed-png-filter-input-filter.lpk", Fault::InputFilter),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fault {
    None,
    Bound,
    Width,
    Height,
    BitDepth,
    Filters,
    Scanlines,
    Hash,
    /// `[lzma, png-filter, bwt {4096}]`: the image's filtered length (3630) above the `bwt`
    /// step's input bound I(100, 4096) = 208, within `max_block_plain`.
    Lookahead,
    /// The `png-filter` step names the `deflate` record: `BadRecord` `kind`.
    Kind,
    /// The step input's first filter byte is 7: `BadRecord` `scanlines` (check (6)), never None.
    InputFilter,
}

fn vectors() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("lpk-format")
        .join("tests")
        .join("vectors")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("deflate")
            .join(name),
    )
    .unwrap()
}

// ---- The contents (generated) ----

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(body);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

/// A PNG file of `layout` whose raw rows are `raw`, filtered with `filters`, the zlib stream
/// (level 9) split into `idats` IDAT chunks.
fn png_file(l: &Layout, raw: &[u8], filters: &[u8], idats: usize) -> Vec<u8> {
    let text = l.refilter(raw, filters).unwrap();
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(9));
    z.write_all(&text).unwrap();
    let z = z.finish().unwrap();
    let mut out = lpk_core::peel::png::SIGNATURE.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&l.width().to_be_bytes());
    ihdr.extend_from_slice(&l.height().to_be_bytes());
    ihdr.extend_from_slice(&[l.bit_depth(), l.color_type(), 0, 0, l.interlace()]);
    chunk(&mut out, b"IHDR", &ihdr);
    let per = z.len().div_ceil(idats);
    for part in z.chunks(per) {
        chunk(&mut out, b"IDAT", part);
    }
    chunk(&mut out, b"IEND", &[]);
    out
}

fn pattern(n: usize, seed: u32, period: usize) -> Vec<u8> {
    let mut x = seed;
    let row: Vec<u8> = (0..period)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect();
    (0..n)
        .map(|i| row[i % period].wrapping_add((i / (3 * period)) as u8))
        .collect()
}

/// RGB8 40 × 30: rows repeat in runs (a sprite), the five filter types in turn, two IDATs.
fn sprite() -> Vec<u8> {
    let l = Layout::new(40, 30, 8, 2, 0).unwrap();
    let raw = pattern(l.raw_len().unwrap() as usize, 3, 120);
    let filters: Vec<u8> = (0..30).map(|i| (i % 5) as u8).collect();
    png_file(&l, &raw, &filters, 2)
}

/// 2-bit grey 400 × 3, Adam7 (pass 3 empty: no row at y = 4), Sub and Paeth rows.
fn grey2() -> Vec<u8> {
    let l = Layout::new(400, 3, 2, 0, 1).unwrap();
    let raw = pattern(l.raw_len().unwrap() as usize, 5, 3);
    let n = l.scanlines() as usize;
    let filters: Vec<u8> = (0..n).map(|i| if i % 2 == 0 { 1 } else { 4 }).collect();
    png_file(&l, &raw, &filters, 1)
}

/// RGBA 16-bit 9 × 7, Average and Paeth rows.
fn rgba16() -> Vec<u8> {
    let l = Layout::new(9, 7, 16, 6, 0).unwrap();
    let raw = pattern(l.raw_len().unwrap() as usize, 7, 72);
    let filters: Vec<u8> = (0..7).map(|i| if i % 2 == 0 { 3 } else { 4 }).collect();
    png_file(&l, &raw, &filters, 1)
}

fn readme() -> Vec<u8> {
    b"png-filter.lpk: PNG images whose IDAT streams are peeled (revision 1.2) and whose row\n\
      filters are re-encoded (revision 1.4, `png-filter`), and this text file, which every\n\
      reader extracts.\n"
        .to_vec()
}

/// The images in the order they are written: name, bytes, model, input layout filter.
fn images() -> Vec<(&'static str, Vec<u8>, Model, u8)> {
    vec![
        ("image.png", fixture("image.png"), Model::Lzma, FILTER_UP),
        ("sprite.png", sprite(), Model::Lzma, FILTER_UP),
        ("grey2-adam7.png", grey2(), Model::Zstd, FILTER_PAETH),
        ("rgba16.png", rgba16(), Model::Lzma, FILTER_UP),
    ]
}

// ---- The builder ----

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

/// Raw LZMA1, preset 6, 64 KiB dictionary, lc 3, lp 0, pb 2, with the end-of-payload marker.
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

struct PlainZstd;
impl BlockEncoder for PlainZstd {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let graph = Graph {
            steps: vec![zstd_step()],
        };
        Ok(Encoded {
            resources: graph.resources(),
            memory: 0,
            graph,
            bytes: zstd_bytes(plain),
        })
    }
}

#[derive(Clone, Copy)]
enum Model {
    Lzma,
    Zstd,
}

fn options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: BLOCK as u64,
        archive_id: [0x5B; 16],
        encoder: Box::new(PlainZstd),
        ..WriterOptions::default()
    }
}

fn record_param(id: u64) -> Vec<u8> {
    let mut v = Vec::new();
    lpk_format::varint::write(&mut v, id).unwrap();
    v
}

/// One PNG as section 9 lays it out: the framing, then its records and its stream's block.
fn add_png(
    w: &mut Writer<&mut Vec<u8>>,
    name: &str,
    bytes: &[u8],
    model: Model,
    layout_filter: u8,
    fault: Fault,
) {
    let mut streams = Vec::new();
    examine(bytes, "png", &Limits::standard(), |r| streams.push(r));
    assert_eq!(streams.len(), 1, "{name}");
    let r = streams.remove(0);
    let deflate: Vec<u8> = r
        .spans
        .iter()
        .flat_map(|&(a, b)| bytes[a..b].iter().copied())
        .collect();
    let text = r.plain.text().to_vec();
    let l = Header::parse(bytes).unwrap().layout().unwrap();
    let filters = l.filter_bytes(&text).unwrap();
    let mut input = l
        .transcode(&text, &vec![layout_filter; filters.len()])
        .unwrap();
    let mut png = PngFilterRecord {
        width: l.width(),
        height: l.height(),
        bit_depth: l.bit_depth(),
        color_type: l.color_type(),
        interlace: l.interlace(),
        filters,
        original_hash: *blake3::hash(&text).as_bytes(),
    };
    let mut dr = DeflateRecord {
        original_len: deflate.len() as u64,
        plain_len: text.len() as u64,
        corrections: r.corrections,
        library: 0,
        original_hash: *blake3::hash(&deflate).as_bytes(),
    };
    match fault {
        Fault::None | Fault::Lookahead | Fault::Kind => {}
        Fault::InputFilter => input[0] = 7,
        Fault::Bound => dr.plain_len -= 1,
        Fault::Width => png.width = 0,
        Fault::Height => {
            // The 1.0 parse wants one filter byte per row of an image that is not interlaced.
            png.height = 0;
            png.filters.clear();
        }
        Fault::BitDepth => png.bit_depth = 4,
        Fault::Filters => png.filters[3] = 5,
        Fault::Scanlines => drop(input.pop()),
        Fault::Hash => png.original_hash[0] ^= 1,
    }
    w.begin_entry(name, EntryFlags::EMPTY, MTIME).unwrap();
    let mut at = 0;
    for &(a, b) in &r.spans {
        if a > at {
            w.add_part(at as u64, &mut &bytes[at..a]).unwrap();
        }
        at = b;
    }
    if at < bytes.len() {
        w.add_part(at as u64, &mut &bytes[at..]).unwrap();
    }
    let term = memory_term(dr.plain_len, dr.corrections.len() as u64);
    let d = w.add_record(Record::new(RecordBody::Deflate(dr))).unwrap();
    let p = w
        .add_record(Record::new(RecordBody::PngFilter(png)))
        .unwrap();
    let (step, encoded) = match model {
        Model::Lzma => (lzma_step(), lzma_bytes(&input)),
        Model::Zstd => (zstd_step(), zstd_bytes(&input)),
    };
    let graph = Graph {
        steps: vec![
            step,
            Step {
                primitive: PrimitiveId::PngFilter,
                params: record_param(if fault == Fault::Kind { d } else { p }),
            },
            Step {
                primitive: PrimitiveId::DeflateReconstruct,
                params: record_param(d),
            },
        ],
    };
    let block = Encoded {
        resources: graph.resources(),
        memory: 0,
        graph,
        bytes: encoded,
    };
    let parts: Vec<(u64, u64)> = r
        .spans
        .iter()
        .map(|&(a, b)| (a as u64, (b - a) as u64))
        .collect();
    w.add_parts_encoded(&parts, &deflate, block, term).unwrap();
    w.end_entry().unwrap();
}

/// `png-filter.lpk`'s bytes.
fn build() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(), 4).unwrap();
    for (name, bytes, model, f) in images() {
        add_png(&mut w, name, &bytes, model, f, Fault::None);
    }
    w.add_file(README, EntryFlags::EMPTY, MTIME, &mut &readme()[..])
        .unwrap();
    w.finish().unwrap();
    out
}

/// `notes.txt` (100 bytes) as `[lzma, png-filter, bwt {4096}]` whose `png-filter` record is
/// `sprite.png`'s and whose `lzma` step encodes sprite's Up text: the `png-filter` step's output
/// bound is the `bwt` step's input bound I(100, 4096) = 208 (the look-ahead), which the
/// filtered length 3630 passes although `max_block_plain` (above 4096, `filler.bin`) does not:
/// `PayloadTooLarge` before `lzma` runs. A decoder that bounded the step by `max_block_plain`
/// would run it and fail later with `BwtError` `length`.
fn add_lookahead(w: &mut Writer<&mut Vec<u8>>) {
    let bytes = sprite();
    let mut streams = Vec::new();
    examine(&bytes, "png", &Limits::standard(), |r| streams.push(r));
    let r = streams.remove(0);
    let text = r.plain.text().to_vec();
    let l = Header::parse(&bytes).unwrap().layout().unwrap();
    let filters = l.filter_bytes(&text).unwrap();
    let up = l.transcode(&text, &vec![FILTER_UP; filters.len()]).unwrap();
    let png = PngFilterRecord {
        width: l.width(),
        height: l.height(),
        bit_depth: l.bit_depth(),
        color_type: l.color_type(),
        interlace: l.interlace(),
        filters,
        original_hash: *blake3::hash(&text).as_bytes(),
    };
    let notes = readme()[..100].to_vec();
    let p = w
        .add_record(Record::new(RecordBody::PngFilter(png)))
        .unwrap();
    let graph = Graph {
        steps: vec![
            lzma_step(),
            Step {
                primitive: PrimitiveId::PngFilter,
                params: record_param(p),
            },
            Step {
                primitive: PrimitiveId::Bwt,
                params: 4096u32.to_le_bytes().to_vec(),
            },
        ],
    };
    let block = Encoded {
        resources: graph.resources(),
        memory: 0,
        graph,
        bytes: lzma_bytes(&up),
    };
    let memory = lpk_codecs_bwt::layout::memory_term(100, 4096);
    w.begin_entry("notes.txt", EntryFlags::EMPTY, MTIME)
        .unwrap();
    w.add_part_encoded(0, &notes, block, memory).unwrap();
    w.end_entry().unwrap();
    let filler: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
    w.add_file("filler.bin", EntryFlags::EMPTY, MTIME, &mut &filler[..])
        .unwrap();
}

/// A malformed vector: `sprite.png` with `fault` (or the look-ahead block), then `readme.txt`.
fn build_malformed(fault: Fault) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(), 4).unwrap();
    w.allow_malformed_png_records();
    if fault == Fault::Lookahead {
        add_lookahead(&mut w);
        w.add_file(README, EntryFlags::EMPTY, MTIME, &mut &readme()[..])
            .unwrap();
        w.finish().unwrap();
        return out;
    }
    add_png(
        &mut w,
        "sprite.png",
        &sprite(),
        Model::Lzma,
        FILTER_UP,
        fault,
    );
    w.add_file(README, EntryFlags::EMPTY, MTIME, &mut &readme()[..])
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

fn class_of(err: &FormatError) -> String {
    let class = format!("{err:?}");
    class.split([' ', '{', '(']).next().unwrap().to_string()
}

fn render() -> String {
    let mut s = String::from(
        "# Expected results of a revision 1.4 reader (docs/spec/CONFORMANCE.md).\n\
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
    for (name, _) in MALFORMED {
        let mut a = open(std::fs::read(vectors().join(name)).unwrap());
        let err = a.verify().unwrap_err();
        writeln!(s, "\n[\"{name}\"]").unwrap();
        writeln!(s, "error = {:?}", class_of(&err)).unwrap();
        if let FormatError::BadRecord { reason, .. } = &err {
            writeln!(s, "reason = {reason:?}").unwrap();
        }
        writeln!(s, "message = {:?}", err.to_string()).unwrap();
    }
    s
}

#[test]
fn the_vector_regenerates_identically() {
    let committed = std::fs::read(vectors().join(VECTOR)).unwrap();
    assert!(
        committed == build(),
        "{VECTOR} differs from this build's (zlib, liblzma, libzstd or preflate-rs version?)"
    );
}

#[test]
fn the_malformed_vectors_regenerate_identically() {
    for (name, fault) in MALFORMED {
        let committed = std::fs::read(vectors().join(name)).unwrap();
        assert!(committed == build_malformed(fault), "{name} differs");
    }
}

#[test]
fn the_vector_extracts_to_the_contents_and_matches_its_expectations() {
    let want = std::fs::read_to_string(vectors().join(EXPECTED)).unwrap();
    assert_eq!(want.replace("\r\n", "\n"), render());
    let mut contents: Vec<(&str, Vec<u8>)> =
        images().into_iter().map(|(n, b, _, _)| (n, b)).collect();
    contents.push((README, readme()));
    for (name, b) in contents {
        let line = format!("{name} {} {}", b.len(), blake3::hash(&b).to_hex());
        assert!(want.contains(&line), "{line}\n{want}");
    }
}

/// Each malformed vector fails `verify` and the extraction of `sprite.png` with its class (and
/// reason); `readme.txt` extracts.
#[test]
fn the_malformed_vectors_fail_by_reason() {
    for (name, fault) in MALFORMED {
        let mut a = open(std::fs::read(vectors().join(name)).unwrap());
        assert_eq!(a.header().version.minor, 4);
        let check = |e: FormatError| match (fault, &e) {
            (Fault::Bound, FormatError::PayloadTooLarge { len, max }) => {
                assert_eq!(*len, max + 1, "{name}")
            }
            (Fault::Kind, FormatError::BadRecord { reason: "kind", .. })
            | (
                Fault::InputFilter,
                FormatError::BadRecord {
                    reason: "scanlines",
                    ..
                },
            ) => {}
            (Fault::Lookahead, FormatError::PayloadTooLarge { len, max }) => {
                assert_eq!((*len, *max), (3630, 208), "{name}")
            }
            (
                Fault::Width,
                FormatError::BadRecord {
                    reason: "width", ..
                },
            )
            | (
                Fault::Height,
                FormatError::BadRecord {
                    reason: "height", ..
                },
            )
            | (
                Fault::BitDepth,
                FormatError::BadRecord {
                    reason: "bit_depth",
                    ..
                },
            )
            | (
                Fault::Filters,
                FormatError::BadRecord {
                    reason: "filters", ..
                },
            )
            | (
                Fault::Scanlines,
                FormatError::BadRecord {
                    reason: "scanlines",
                    ..
                },
            )
            | (
                Fault::Hash,
                FormatError::BadRecord {
                    reason: "original_hash",
                    ..
                },
            ) => {}
            _ => panic!("{name}: {e:?}"),
        };
        check(a.verify().unwrap_err());
        let t = a.entry_table().unwrap();
        let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
        for e in &entries {
            let mut got = Vec::new();
            if e.path == README || e.path == "filler.bin" {
                a.extract(e, &mut got).unwrap();
                assert!(e.path != README || got == readme());
            } else {
                check(a.extract(e, &mut got).unwrap_err());
            }
        }
    }
}

/// A reader before revision 1.4 (`lpk-format` alone; a revision 1.3 full reader without
/// `png-filter`; `lpk-decode`): the archive opens and lists; `verify` and every PNG entry are
/// `UnimplementedPrimitive` (9); `readme.txt` extracts.
#[test]
fn a_1_3_reader_reports_unimplemented_primitive_9() {
    let mut names = vec![VECTOR];
    names.extend(MALFORMED.iter().map(|(n, _)| *n));
    for name in names {
        let bytes = std::fs::read(vectors().join(name)).unwrap();
        let mut plain = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
        let mut full_1_3 = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        let r = full_1_3.registry_mut();
        r.register(
            PrimitiveId::JpegReconstruct,
            Box::new(lpk_core::JpegDecoder),
        );
        r.register(
            PrimitiveId::DeflateReconstruct,
            Box::new(lpk_core::DeflateDecoder),
        );
        r.register(PrimitiveId::Bwt, Box::new(lpk_core::BwtDecoder));
        for a in [&mut plain, &mut full_1_3] {
            assert!(matches!(
                a.verify().unwrap_err(),
                FormatError::UnimplementedPrimitive { id: 9 }
            ));
            let t = a.entry_table().unwrap();
            let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
            for e in &entries {
                let mut got = Vec::new();
                if e.path == README || e.path == "filler.bin" {
                    a.extract(e, &mut got).unwrap();
                } else {
                    assert!(matches!(
                        a.extract(e, &mut got).unwrap_err(),
                        FormatError::UnimplementedPrimitive { id: 9 }
                    ));
                }
            }
        }
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let p = vectors().join(name);
        let code = lpk_format::cli::run(
            ["lpk-decode", "verify", p.to_str().unwrap()],
            &mut out,
            &mut err,
        );
        assert_eq!(code, 1);
        assert!(
            String::from_utf8(err)
                .unwrap()
                .contains("primitive 0x0009 is not implemented"),
            "{name}"
        );
    }
}

/// What the vector proves (spec section 11): minor 4; every PNG block is
/// `[M, png-filter, deflate-reconstruct]`; `image.png`'s inflated text is above
/// `max_block_plain`; the declared decode memory is `max_block_plain` plus the largest deflate
/// term (png-filter has none).
#[test]
fn the_vector_has_the_shape_it_claims() {
    let bytes = std::fs::read(vectors().join(VECTOR)).unwrap();
    let mut a = open(bytes.clone());
    assert_eq!(a.header().version.minor, 4);
    let env = a.index().envelope;
    // The largest block: image.png's stream (8174 bytes), within the 8 KiB block size.
    assert!(env.max_block_plain <= BLOCK as u64);
    let records: Vec<Record> = {
        let t = a.records().unwrap().unwrap();
        let t = t.table().unwrap();
        t.iter().map(|r| r.unwrap()).collect()
    };
    assert_eq!(records.len(), 8);
    let mut largest = 0;
    let mut above = 0;
    let mut interlaced = false;
    for pair in records.chunks(2) {
        let (RecordBody::Deflate(d), RecordBody::PngFilter(p)) = (&pair[0].body, &pair[1].body)
        else {
            panic!("records out of order")
        };
        assert_eq!(p.filtered_len(), Some(d.plain_len));
        largest = largest.max(memory_term(d.plain_len, d.corrections.len() as u64));
        above += usize::from(d.plain_len > env.max_block_plain);
        interlaced |= p.interlace == 1;
    }
    assert!(above >= 1 && interlaced);
    assert_eq!(env.decode_memory, env.max_block_plain + largest);
    let mut png_blocks = 0;
    for b in a.index().blocks.clone() {
        let frame = &bytes[b.frame_offset as usize..(b.frame_offset + b.frame_len) as usize];
        let Some(lpk_format::frame::ReadFrame::Known(f)) =
            lpk_format::Frame::read(&mut Cursor::new(frame), &Default::default()).unwrap()
        else {
            panic!("not a block frame")
        };
        let (graph, _) = Graph::parse(&f.payload).unwrap();
        let steps: Vec<PrimitiveId> = graph.steps.iter().map(|s| s.primitive).collect();
        if steps.contains(&PrimitiveId::PngFilter) {
            assert_eq!(
                steps[1..],
                [PrimitiveId::PngFilter, PrimitiveId::DeflateReconstruct]
            );
            png_blocks += 1;
        }
    }
    assert_eq!(png_blocks, 4);
}

#[test]
#[ignore = "rewrites png-filter.lpk, the malformed-png-filter-*.lpk vectors and expected-1.4.toml in crates/lpk-format/tests/vectors"]
fn regenerate() {
    std::fs::write(vectors().join(VECTOR), build()).unwrap();
    for (name, fault) in MALFORMED {
        std::fs::write(vectors().join(name), build_malformed(fault)).unwrap();
    }
    std::fs::write(vectors().join(EXPECTED), render()).unwrap();
}
