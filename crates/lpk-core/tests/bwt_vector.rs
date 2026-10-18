//! The revision 1.3 vector `bwt-text.lpk` (in `crates/lpk-format/tests/vectors/`), the two
//! malformed revision 1.3 vectors and their full-reader expectations (`expected-1.3.toml`
//! beside them). Spec section 11 lists them; section 8 ("Decoding `bwt`") is the layout;
//! CONFORMANCE.md says what a decoder must do with them.
//!
//! The builder calls the format's writer directly with explicit encoders: chunk size 4096 (fixed
//! cut), block size 16 KiB, archive id 0x5A repeated, mtime 1000, minor 3, no dedup; plain blocks
//! zstd level 3 (window_log 17). The entries:
//! - `prose.txt` (40000 bytes of generated text): three blocks `[lzma (dict 64 KiB, lc 3, lp 0,
//!   pb 2, end marker), bwt {4096}]`, four BWT blocks per full block; the first two blocks are
//!   exactly `max_block_plain` long and their `lzma` output is longer than it (the bound of
//!   section 8, "Bounds"); the third ends with a short BWT block;
//! - `log.txt` (21384 bytes of generated log lines): `[lzma, bwt {16384}]`, the second block a
//!   short last BWT block;
//! - `tiny.txt` (1 byte): `[bwt {1}]`, a first step with n = 1;
//! - `runs.bin` (3000 bytes of one value, then every byte value once): `[zstd (level 3,
//!   window_log 17), bwt {8192}]`;
//! - `notes.txt.gz` (the Deflate fixture): its stream as `[lzma, bwt {16384},
//!   deflate-reconstruct]`, the inflated text above `max_block_plain` (the look-ahead);
//! - `readme.txt`: a plain block, which every reader extracts (`files_1_0`).
//!
//! `malformed-bwt-bound.lpk`: `prose.txt`'s first block with 64 junk bytes after the layout
//! before `lzma`, so the `lzma` step's output passes I(O, B): `PayloadTooLarge` before `bwt`
//! runs. `malformed-bwt-inverse.lpk`: `prose.txt`'s first block as `[lzma, bwt {16384}]` with
//! its second index replaced by the third (in range; the walk from the primary index alone
//! gives the text): `BwtError` `inverse`. Both also hold `readme.txt`.
//!
//! The pre-1.3 expectations (listing, `info`, the `UnimplementedPrimitive` (3) of `verify`,
//! `files_1_0`) are in `expected.toml`, written by lpk-format's conformance test. Regenerate
//! with `cargo test -p lpk-core --test bwt_vector -- --ignored regenerate`, then
//! `cargo test -p lpk-format --test conformance -- --ignored`; byte identity needs the same
//! liblzma, libzstd, preflate-rs, bitcode and cabac versions (the BWT is unique by definition).
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::io::{Cursor, Write as _};
use std::path::PathBuf;

use lpk_codecs_bwt::layout::{self, index_count, input_len};
use lpk_core::peel::deflate::{examine, Limits};
use lpk_core::register_full_reader;
use lpk_format::{
    Archive, BlockEncoder, DeflateRecord, Encoded, EntryFlags, EntryKind, FormatError, Graph,
    PrimitiveId, Record, RecordBody, Resources, Step, Writer, WriterOptions,
};

const VECTOR: &str = "bwt-text.lpk";
const EXPECTED: &str = "expected-1.3.toml";
const MALFORMED_BOUND: &str = "malformed-bwt-bound.lpk";
const MALFORMED_INVERSE: &str = "malformed-bwt-inverse.lpk";
const README: &str = "readme.txt";
const MTIME: i64 = 1000;
const BLOCK: usize = 16 * 1024;
const JUNK: usize = 64;

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

/// Deterministic text of `n` bytes from a word list (the same generator as the Deflate vector).
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

fn prose() -> Vec<u8> {
    words(40_000, 11).into_bytes()
}

fn log() -> Vec<u8> {
    let levels = ["INFO", "WARN", "DEBUG", "ERROR"];
    let mut x = 77u32;
    let mut s = String::new();
    let mut i = 0u32;
    while s.len() < 21_384 {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let _ = writeln!(
            s,
            "2026-10-18T07:{:02}:{:02}Z {} worker-{} {}",
            (i / 60) % 60,
            i % 60,
            levels[(x >> 24) as usize % 4],
            (x >> 8) % 8,
            words(20 + (x as usize % 30), x).replace('\n', " ")
        );
        i += 1;
    }
    s.truncate(21_384);
    s.into_bytes()
}

fn tiny() -> Vec<u8> {
    b"x".to_vec()
}

fn runs() -> Vec<u8> {
    let mut v = vec![b'a'; 3000];
    v.extend(0u8..=255);
    v
}

fn readme() -> Vec<u8> {
    let mut s = String::from(
        "bwt-text.lpk: text through the Burrows-Wheeler transform before LZMA or zstd (revision\n\
         1.3), a gzip file whose Deflate stream is peeled and transformed, and this text file,\n\
         which every reader extracts.\n",
    );
    s.push_str(&words(1200, 13));
    s.push('\n');
    s.into_bytes()
}

// ---- The builder ----

/// zstd level 3, window_log 17, with a content checksum: the plain blocks and `runs.bin`'s step.
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

fn bwt_step(block_size: u32) -> Step {
    Step {
        primitive: PrimitiveId::Bwt,
        params: block_size.to_le_bytes().to_vec(),
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

fn model_bytes(model: Model, data: &[u8]) -> (Step, Vec<u8>) {
    match model {
        Model::Lzma => (lzma_step(), lzma_bytes(data)),
        Model::Zstd => (zstd_step(), zstd_bytes(data)),
    }
}

/// One block of `plain` (at most `BLOCK` bytes) at `offset` of the open entry: the BWT layout
/// with `block_size`, `tweak` applied to it, then the model step; graph `[M, bwt {block_size}]`
/// (or `[bwt]` alone when `model` is `None`). The declared memory is the step's term.
fn add_bwt_block(
    w: &mut Writer<&mut Vec<u8>>,
    offset: u64,
    plain: &[u8],
    block_size: u32,
    model: Option<Model>,
    tweak: impl FnOnce(&mut Vec<u8>),
) {
    let mut transformed = layout::encode(plain, block_size).unwrap();
    assert_eq!(
        transformed.len() as u64,
        input_len(plain.len() as u64, u64::from(block_size))
    );
    tweak(&mut transformed);
    let (steps, bytes) = match model {
        Some(m) => {
            let (step, bytes) = model_bytes(m, &transformed);
            (vec![step, bwt_step(block_size)], bytes)
        }
        None => (vec![bwt_step(block_size)], transformed),
    };
    let graph = Graph { steps };
    let memory = layout::memory_term(plain.len() as u64, u64::from(block_size));
    let encoded = Encoded {
        resources: graph.resources(),
        memory: 0,
        graph,
        bytes,
    };
    w.add_part_encoded(offset, plain, encoded, memory).unwrap();
}

/// A whole file in `BLOCK`-sized blocks, each `[M, bwt {block_size}]`.
fn add_bwt_file(
    w: &mut Writer<&mut Vec<u8>>,
    name: &str,
    data: &[u8],
    block_size: u32,
    model: Option<Model>,
) {
    w.begin_entry(name, EntryFlags::EMPTY, MTIME).unwrap();
    for (i, block) in data.chunks(BLOCK).enumerate() {
        add_bwt_block(w, (i * BLOCK) as u64, block, block_size, model, |_| {});
    }
    w.end_entry().unwrap();
}

fn options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: BLOCK as u64,
        archive_id: [0x5A; 16],
        encoder: Box::new(PlainZstd),
        ..WriterOptions::default()
    }
}

/// `notes.txt.gz` as `[lzma, bwt {16384}, deflate-reconstruct]`: the stream's inflated text,
/// transformed, then LZMA; the record as the Deflate vector makes it.
fn add_notes(w: &mut Writer<&mut Vec<u8>>) {
    let bytes = fixture("notes.txt.gz");
    let mut streams = Vec::new();
    examine(&bytes, "gz", &Limits::standard(), |r| streams.push(r));
    assert_eq!(streams.len(), 1);
    let r = streams.remove(0);
    let (a, b) = (r.spans[0].0, r.spans[0].1);
    assert_eq!(r.spans.len(), 1);
    let deflate = bytes[a..b].to_vec();
    let plain = r.plain.text().to_vec();
    assert!(
        plain.len() > BLOCK,
        "the inflated text is above max_block_plain"
    );
    let record = DeflateRecord {
        original_len: deflate.len() as u64,
        plain_len: plain.len() as u64,
        corrections: r.corrections,
        library: 0,
        original_hash: *blake3::hash(&deflate).as_bytes(),
    };
    w.begin_entry("notes.txt.gz", EntryFlags::EMPTY, MTIME)
        .unwrap();
    w.add_part(0, &mut &bytes[..a]).unwrap();
    let id = w
        .add_record(Record::new(RecordBody::Deflate(record.clone())))
        .unwrap();
    let mut params = Vec::new();
    lpk_format::varint::write(&mut params, id).unwrap();
    let block_size = BLOCK as u32;
    let transformed = layout::encode(&plain, block_size).unwrap();
    let graph = Graph {
        steps: vec![
            lzma_step(),
            bwt_step(block_size),
            Step {
                primitive: PrimitiveId::DeflateReconstruct,
                params,
            },
        ],
    };
    let encoded = Encoded {
        resources: graph.resources(),
        memory: 0,
        graph,
        bytes: lzma_bytes(&transformed),
    };
    let memory =
        lpk_core::peel::deflate::memory_term(record.plain_len, record.corrections.len() as u64)
            .max(layout::memory_term(record.plain_len, u64::from(block_size)));
    w.add_part_encoded(a as u64, &deflate, encoded, memory)
        .unwrap();
    w.add_part(b as u64, &mut &bytes[b..]).unwrap();
    w.end_entry().unwrap();
}

/// `bwt-text.lpk`'s bytes.
fn build() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(), 3).unwrap();
    add_bwt_file(&mut w, "prose.txt", &prose(), 4096, Some(Model::Lzma));
    add_bwt_file(&mut w, "log.txt", &log(), 16384, Some(Model::Lzma));
    add_bwt_file(&mut w, "tiny.txt", &tiny(), 1, None);
    add_bwt_file(&mut w, "runs.bin", &runs(), 8192, Some(Model::Zstd));
    add_notes(&mut w);
    w.add_file(README, EntryFlags::EMPTY, MTIME, &mut &readme()[..])
        .unwrap();
    w.finish().unwrap();
    out
}

/// `malformed-bwt-bound.lpk`: the first block of `prose.txt` with `JUNK` bytes appended to the
/// layout before `lzma`, so the `lzma` step's output would pass I(O, B).
fn build_malformed_bound() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(), 3).unwrap();
    let p = prose();
    w.begin_entry("prose.txt", EntryFlags::EMPTY, MTIME)
        .unwrap();
    add_bwt_block(&mut w, 0, &p[..BLOCK], 4096, Some(Model::Lzma), |t| {
        t.extend(std::iter::repeat_n(0x5Au8, JUNK));
    });
    w.end_entry().unwrap();
    w.add_file(README, EntryFlags::EMPTY, MTIME, &mut &readme()[..])
        .unwrap();
    w.finish().unwrap();
    out
}

/// `malformed-bwt-inverse.lpk`: the first block of `prose.txt` as one BWT block whose second
/// index is replaced by the third (in range, so only a decoder that checks every chain refuses
/// it).
fn build_malformed_inverse() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new_revision(&mut out, options(), 3).unwrap();
    let p = prose();
    w.begin_entry("prose.txt", EntryFlags::EMPTY, MTIME)
        .unwrap();
    add_bwt_block(&mut w, 0, &p[..BLOCK], 16384, Some(Model::Lzma), |t| {
        assert_eq!(index_count(BLOCK as u64), 32);
        let third: [u8; 4] = t[16..20].try_into().unwrap();
        assert_ne!(t[12..16], third);
        t[12..16].copy_from_slice(&third);
    });
    w.end_entry().unwrap();
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
        "# Expected results of a revision 1.3 reader (docs/spec/CONFORMANCE.md).\n\
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
    // The malformed vectors: `verify` of a revision 1.3 reader fails with the class and message.
    for name in [MALFORMED_BOUND, MALFORMED_INVERSE] {
        let mut a = open(std::fs::read(vectors().join(name)).unwrap());
        let err = a.verify().unwrap_err();
        writeln!(s, "\n[\"{name}\"]").unwrap();
        writeln!(s, "error = {:?}", class_of(&err)).unwrap();
        writeln!(s, "message = {:?}", err.to_string()).unwrap();
    }
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
fn the_malformed_vectors_regenerate_identically() {
    for (name, bytes) in [
        (MALFORMED_BOUND, build_malformed_bound()),
        (MALFORMED_INVERSE, build_malformed_inverse()),
    ] {
        let committed = std::fs::read(vectors().join(name)).unwrap();
        assert!(committed == bytes, "{name} differs from this build's");
    }
}

#[test]
fn the_vector_extracts_to_the_contents_and_matches_its_expectations() {
    let want = std::fs::read_to_string(vectors().join(EXPECTED)).unwrap();
    assert_eq!(want.replace("\r\n", "\n"), render());
    for (name, b) in [
        ("prose.txt", prose()),
        ("log.txt", log()),
        ("tiny.txt", tiny()),
        ("runs.bin", runs()),
        ("notes.txt.gz", fixture("notes.txt.gz")),
        (README, readme()),
    ] {
        let line = format!("{name} {} {}", b.len(), blake3::hash(&b).to_hex());
        assert!(want.contains(&line), "{line}\n{want}");
    }
}

/// Spec section 8, "Bounds": the `lzma` step before `bwt` is bounded by I(O, B); the junk makes
/// its output longer, `PayloadTooLarge` with `max` = I(O, B) before the `bwt` step runs, from
/// `verify` and from extracting `prose.txt`; `readme.txt` extracts.
#[test]
fn the_malformed_bound_vector_is_payload_too_large() {
    let mut a = open(std::fs::read(vectors().join(MALFORMED_BOUND)).unwrap());
    assert_eq!(a.header().version.minor, 3);
    let bound = input_len(BLOCK as u64, 4096);
    let check = |e: FormatError| match e {
        FormatError::PayloadTooLarge { len, max } => {
            assert_eq!(max, bound);
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
            assert!(got == readme());
        } else {
            check(a.extract(e, &mut got).unwrap_err());
        }
    }
}

/// Spec section 8, "Decoding `bwt`", check (5): a wrong auxiliary index is `BwtError` `inverse`
/// although the walk from the primary index alone would give the text.
#[test]
fn the_malformed_inverse_vector_is_bwt_error_inverse() {
    let mut a = open(std::fs::read(vectors().join(MALFORMED_INVERSE)).unwrap());
    assert_eq!(a.header().version.minor, 3);
    let check = |e: FormatError| {
        assert!(
            matches!(e, FormatError::BwtError { reason: "inverse" }),
            "{e:?}"
        )
    };
    check(a.verify().unwrap_err());
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    for e in &entries {
        let mut got = Vec::new();
        if e.path == README {
            a.extract(e, &mut got).unwrap();
        } else {
            check(a.extract(e, &mut got).unwrap_err());
        }
    }
    // The walk from the primary index alone gives the text (so a decoder that ran only it would
    // miss the damage): the probe's inverse with one chain.
    let p = prose();
    let t = lpk_codecs_bwt::forward(&p[..BLOCK]).unwrap();
    let one = lpk_codecs_bwt::Transformed {
        bwt: t.bwt.clone(),
        rate_log2: 14,
        aux: vec![t.primary()],
    };
    assert_eq!(lpk_codecs_bwt::inverse(&one).unwrap(), &p[..BLOCK]);
}

/// A reader before revision 1.3 (`lpk-format` alone, a revision 1.2 full reader with
/// `jpeg-reconstruct` and `deflate-reconstruct` registered but not `bwt`, and `lpk-decode`): the
/// archive opens and lists; `verify` and every entry in a `bwt` block (the
/// `[lzma, bwt, deflate-reconstruct]` one included: the first unrun primitive in step order)
/// are `UnimplementedPrimitive` (3); `readme.txt` extracts.
#[test]
fn a_1_2_reader_reports_unimplemented_primitive_3() {
    for name in [VECTOR, MALFORMED_BOUND, MALFORMED_INVERSE] {
        let bytes = std::fs::read(vectors().join(name)).unwrap();
        let mut plain = Archive::open(Cursor::new(bytes.clone()), &Resources::default()).unwrap();
        let mut full_1_2 = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        full_1_2.registry_mut().register(
            PrimitiveId::JpegReconstruct,
            Box::new(lpk_core::JpegDecoder),
        );
        full_1_2.registry_mut().register(
            PrimitiveId::DeflateReconstruct,
            Box::new(lpk_core::DeflateDecoder),
        );
        for a in [&mut plain, &mut full_1_2] {
            assert!(matches!(
                a.verify().unwrap_err(),
                FormatError::UnimplementedPrimitive { id: 3 }
            ));
            let t = a.entry_table().unwrap();
            let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
            let mut seen_notes = false;
            for e in &entries {
                let mut got = Vec::new();
                if e.path == README {
                    a.extract(e, &mut got).unwrap();
                    assert!(got == readme());
                } else {
                    seen_notes |= e.path == "notes.txt.gz";
                    assert!(matches!(
                        a.extract(e, &mut got).unwrap_err(),
                        FormatError::UnimplementedPrimitive { id: 3 }
                    ));
                }
            }
            assert_eq!(seen_notes, name == VECTOR);
        }
        // The 1.0 tool.
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
                .contains("primitive 0x0003 is not implemented"),
            "{name}"
        );
    }
}

/// What the vector proves (spec section 11, CONFORMANCE): minor 3; the graph shapes; a block
/// exactly `max_block_plain` long whose `lzma` output is longer; the declared decode memory.
#[test]
fn the_vector_has_the_shape_it_claims() {
    let bytes = std::fs::read(vectors().join(VECTOR)).unwrap();
    let mut a = open(bytes.clone());
    assert_eq!(a.header().version.minor, 3);
    let env = a.index().envelope;
    assert_eq!(env.max_block_plain, BLOCK as u64);
    assert_eq!(env.max_bwt_block, BLOCK as u64);
    let records: Vec<Record> = {
        let t = a.records().unwrap().unwrap();
        let t = t.table().unwrap();
        t.iter().map(|r| r.unwrap()).collect()
    };
    assert_eq!(records.len(), 1);
    let RecordBody::Deflate(d) = &records[0].body else {
        panic!("not a deflate record")
    };
    assert!(d.plain_len > env.max_block_plain, "the look-ahead");
    let mut shapes: Vec<Vec<PrimitiveId>> = Vec::new();
    let mut largest = 0;
    let mut full_blocks_over_bound = 0;
    for b in a.index().blocks.clone() {
        let frame = &bytes[b.frame_offset as usize..(b.frame_offset + b.frame_len) as usize];
        let Some(lpk_format::frame::ReadFrame::Known(f)) =
            lpk_format::Frame::read(&mut Cursor::new(frame), &Default::default()).unwrap()
        else {
            panic!("not a block frame")
        };
        let (graph, _) = Graph::parse(&f.payload).unwrap();
        let steps: Vec<PrimitiveId> = graph.steps.iter().map(|s| s.primitive).collect();
        if let Some(bwt) = graph.steps.iter().find(|s| s.primitive == PrimitiveId::Bwt) {
            let bs = u64::from(u32::from_le_bytes(bwt.params[..4].try_into().unwrap()));
            let o = if steps.last() == Some(&PrimitiveId::Bwt) {
                b.plain_len
            } else {
                d.plain_len
            };
            largest = largest.max(layout::memory_term(o, bs));
            if b.plain_len == BLOCK as u64 && input_len(o, bs) > env.max_block_plain {
                full_blocks_over_bound += 1;
            }
        }
        shapes.push(steps);
    }
    use PrimitiveId::*;
    for want in [
        vec![Lzma, Bwt],
        vec![Bwt],
        vec![Zstd, Bwt],
        vec![Lzma, Bwt, DeflateReconstruct],
        vec![Zstd],
    ] {
        assert!(shapes.contains(&want), "{want:?} in {shapes:?}");
    }
    assert!(full_blocks_over_bound >= 2, "{full_blocks_over_bound}");
    let deflate_term =
        lpk_core::peel::deflate::memory_term(d.plain_len, d.corrections.len() as u64);
    assert_eq!(
        env.decode_memory,
        env.max_block_plain + largest.max(deflate_term)
    );
}

#[test]
#[ignore = "rewrites bwt-text.lpk, malformed-bwt-bound.lpk, malformed-bwt-inverse.lpk and expected-1.3.toml in crates/lpk-format/tests/vectors"]
fn regenerate() {
    std::fs::write(vectors().join(VECTOR), build()).unwrap();
    std::fs::write(vectors().join(MALFORMED_BOUND), build_malformed_bound()).unwrap();
    std::fs::write(vectors().join(MALFORMED_INVERSE), build_malformed_inverse()).unwrap();
    std::fs::write(vectors().join(EXPECTED), render()).unwrap();
}
