//! Task E of E2-16 (D-49 item 5): hand-made `png-filter` records decoded by this decoder and by
//! the reference full reader (`lpk-format` with `lpk-core`'s decoders registered), called as a
//! black box through its public API: both must report the same class and, for `BadRecord`, the
//! same record and normative reason (section 16); a block both decode must give the same bytes.
//! The archives are written with `lpk-format`'s writer, its revision 1.4 record checks switched
//! off so that a reader's refusals can be reached.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Cursor, Write as _};

use lpk_check::png::{bits_per_pixel, passes, scanline_bytes, unit};
use lpk_check::record::PngRecord;
use lpk_core::peel::deflate::{examine, Limits};
use lpk_core::register_full_reader;
use lpk_format::{
    BlockEncoder, DeflateRecord, Encoded, EntryFlags, EntryKind, FormatError, Graph,
    PngFilterRecord, PrimitiveId, Record, RecordBody, Resources, Step, Writer, WriterOptions,
};

const MTIME: i64 = 1_700_000_000;
const IMG: &str = "img.bin";

// ---- Images ----

/// A small deterministic generator (xorshift64*).
struct Rng(u64);
impl Rng {
    fn byte(&mut self) -> u8 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
    }
}

fn predict(t: u8, a: u8, b: u8, c: u8) -> u8 {
    match t {
        1 => a,
        2 => b,
        3 => ((u16::from(a) + u16::from(b)) >> 1) as u8,
        4 => {
            let p = i32::from(a) + i32::from(b) - i32::from(c);
            let (pa, pb, pc) = (
                (p - i32::from(a)).abs(),
                (p - i32::from(b)).abs(),
                (p - i32::from(c)).abs(),
            );
            if pa <= pb && pa <= pc {
                a
            } else if pb <= pc {
                b
            } else {
                c
            }
        }
        _ => 0,
    }
}

/// The geometry of an image: per non-empty pass, (scanline bytes, scanline count), and u.
fn geometry(w: u32, h: u32, bd: u8, ct: u8, il: u8) -> (Vec<(usize, usize)>, usize) {
    let r = PngRecord {
        width: w,
        height: h,
        bit_depth: bd,
        color_type: ct,
        interlace: il,
        filters: Vec::new(),
        original_hash: [0; 32],
    };
    let b = bits_per_pixel(&r);
    let p = passes(&r)
        .into_iter()
        .map(|(pw, ph)| (scanline_bytes(pw, b) as usize, ph as usize))
        .collect();
    (p, unit(&r) as usize)
}

/// The filtered text of raw scanlines `raw` (per pass, its scanlines) with type `types(k)` for
/// the k-th scanline of the image (raw values above 4 are written as such, the bytes then
/// unfiltered).
fn filter(raw: &[Vec<Vec<u8>>], u: usize, types: &dyn Fn(usize) -> u8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut k = 0;
    for pass in raw {
        let mut prev = vec![0u8; pass.first().map_or(0, Vec::len)];
        for line in pass {
            let t = types(k);
            out.push(t);
            for i in 0..line.len() {
                let a = if i >= u { line[i - u] } else { 0 };
                let c = if i >= u { prev[i - u] } else { 0 };
                out.push(line[i].wrapping_sub(predict(t, a, prev[i], c)));
            }
            prev.clone_from(line);
            k += 1;
        }
    }
    out
}

/// An image with random raw scanlines: its record (filters `out(k)`), the step input (each
/// scanline filtered with `inp(k)`) and the expected output (filtered with `out(k)`).
struct Image {
    rec: PngFilterRecord,
    input: Vec<u8>,
    output: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
fn image(
    w: u32,
    h: u32,
    bd: u8,
    ct: u8,
    il: u8,
    inp: &dyn Fn(usize) -> u8,
    out: &dyn Fn(usize) -> u8,
    seed: u64,
) -> Image {
    let (geo, u) = geometry(w, h, bd, ct, il);
    let mut rng = Rng(seed | 1);
    let raw: Vec<Vec<Vec<u8>>> = geo
        .iter()
        .map(|&(n, rows)| {
            (0..rows)
                .map(|_| (0..n).map(|_| rng.byte()).collect())
                .collect()
        })
        .collect();
    let input = filter(&raw, u, inp);
    let output = filter(&raw, u, out);
    let count: usize = geo.iter().map(|&(_, rows)| rows).sum();
    Image {
        rec: PngFilterRecord {
            width: w,
            height: h,
            bit_depth: bd,
            color_type: ct,
            interlace: il,
            filters: (0..count).map(out).collect(),
            original_hash: *blake3::hash(&output).as_bytes(),
        },
        input,
        output,
    }
}

fn simple(w: u32, h: u32, bd: u8, ct: u8, il: u8, seed: u64) -> Image {
    image(
        w,
        h,
        bd,
        ct,
        il,
        &|k| (k % 5) as u8,
        &|k| ((k + 2) % 5) as u8,
        seed,
    )
}

// ---- Steps and archives ----

fn store() -> Step {
    Step {
        primitive: PrimitiveId::Store,
        params: Vec::new(),
    }
}

fn rid_step(primitive: PrimitiveId, id: u64) -> Step {
    let mut params = Vec::new();
    lpk_format::varint::write(&mut params, id).unwrap();
    Step { primitive, params }
}

fn png(id: u64) -> Step {
    rid_step(PrimitiveId::PngFilter, id)
}

fn deflate(id: u64) -> Step {
    rid_step(PrimitiveId::DeflateReconstruct, id)
}

fn bwt(block_size: u32) -> Step {
    Step {
        primitive: PrimitiveId::Bwt,
        params: block_size.to_le_bytes().to_vec(),
    }
}

/// A raw Deflate stream of `text` and its `deflate` record (made by the reference's analysis of
/// a gzip member, as the vectors are).
fn deflate_of(text: &[u8]) -> (Vec<u8>, DeflateRecord) {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(9));
    e.write_all(text).unwrap();
    let gz = e.finish().unwrap();
    let mut streams = Vec::new();
    examine(&gz, "gz", &Limits::standard(), |r| streams.push(r));
    assert_eq!(streams.len(), 1);
    let r = streams.remove(0);
    assert_eq!(r.spans.len(), 1);
    assert_eq!(r.plain.text(), text);
    let stream = gz[r.spans[0].0..r.spans[0].1].to_vec();
    let rec = DeflateRecord {
        original_len: stream.len() as u64,
        plain_len: text.len() as u64,
        corrections: r.corrections,
        library: 0,
        original_hash: *blake3::hash(&stream).as_bytes(),
    };
    (stream, rec)
}

/// Plain files: `[store]`.
struct StoreEnc;
impl BlockEncoder for StoreEnc {
    fn encode(&mut self, plain: &[u8]) -> Result<Encoded, FormatError> {
        let graph = Graph {
            steps: vec![store()],
        };
        Ok(Encoded {
            resources: graph.resources(),
            memory: 0,
            graph,
            bytes: plain.to_vec(),
        })
    }
}

/// One crafted block: its records (ids from 0), graph, encoded bytes, plain bytes (the block's
/// `plain_len` is their length) and declared memory.
struct Case {
    name: String,
    records: Vec<RecordBody>,
    steps: Vec<Step>,
    encoded: Vec<u8>,
    plain: Vec<u8>,
    memory: u64,
    /// The outcome this case is built to reach (class, reason) — a check of the case itself;
    /// the gate is the agreement of the two decoders.
    want: (&'static str, &'static str),
    /// Bytes changed in record bodies after the write, (record id, offset in its body, new
    /// byte), with the `body_hash` and the frame hash recomputed: how the records the writer
    /// refuses to write (section 12's rules) are made.
    patches: Vec<(usize, usize, u8)>,
    /// Swap the last two steps of the block's graph after the write (the frame hash
    /// recomputed): how graphs the writer refuses (a step before `png-filter` that is not a
    /// model step) are made.
    swap_last_two: bool,
}

fn case(
    name: impl Into<String>,
    records: Vec<RecordBody>,
    steps: Vec<Step>,
    encoded: Vec<u8>,
    plain: Vec<u8>,
    want: (&'static str, &'static str),
) -> Case {
    Case {
        name: name.into(),
        records,
        steps,
        encoded,
        plain,
        memory: 0,
        want,
        patches: Vec::new(),
        swap_last_two: false,
    }
}

const OK: (&str, &str) = ("ok", "");

fn bad(reason: &'static str) -> (&'static str, &'static str) {
    ("BadRecord", reason)
}

fn build(c: &Case) -> Vec<u8> {
    let mut out = Vec::new();
    let opts = WriterOptions {
        chunk_size: 4096,
        block_size: 1 << 20,
        archive_id: [0x3C; 16],
        encoder: Box::new(StoreEnc),
        ..WriterOptions::default()
    };
    let mut w = Writer::new_revision(&mut out, opts, 4).unwrap();
    w.allow_malformed_png_records();
    for r in &c.records {
        w.add_record(Record::new(r.clone())).unwrap();
    }
    w.begin_entry(IMG, EntryFlags::EMPTY, MTIME).unwrap();
    let graph = Graph {
        steps: c.steps.clone(),
    };
    let encoded = Encoded {
        resources: graph.resources(),
        memory: 0,
        graph,
        bytes: c.encoded.clone(),
    };
    w.add_part_encoded(0, &c.plain, encoded, c.memory)
        .unwrap_or_else(|e| panic!("{}: writer: {e}", c.name));
    w.end_entry().unwrap();
    w.add_file(
        "readme.txt",
        EntryFlags::EMPTY,
        MTIME,
        &mut &b"a plain file every reader extracts\n"[..],
    )
    .unwrap();
    w.finish().unwrap();
    if !c.patches.is_empty() {
        patch_records(&mut out, &c.patches);
    }
    if c.swap_last_two {
        swap_last_two_steps(&mut out);
    }
    out
}

/// Swaps the last two steps of block 0's graph in place and recomputes the frame's hash.
fn swap_last_two_steps(d: &mut [u8]) {
    let a = lpk_check::Archive::open(d.to_vec(), lpk_check::Options::default()).unwrap();
    let off = a.index.blocks[0].frame_offset as usize;
    let f = lpk_check::wire::parse_frame(d, off, u64::MAX).unwrap();
    let payload = f.end - 32 - f.payload.len()..f.end - 32;
    // step_count (one byte), then per step: u16 id, u8 flags, params_len (one byte), params.
    let n = usize::from(d[payload.start]);
    let mut starts = Vec::new();
    let mut pos = payload.start + 1;
    for _ in 0..n {
        starts.push(pos);
        pos += 4 + usize::from(d[pos + 3]);
    }
    let (a0, b0, end) = (starts[n - 2], starts[n - 1], pos);
    let mut swapped = d[b0..end].to_vec();
    swapped.extend_from_slice(&d[a0..b0]);
    d[a0..end].copy_from_slice(&swapped);
    let h = blake3::hash(&d[payload.clone()]);
    d[payload.end..payload.end + 32].copy_from_slice(h.as_bytes());
}

/// Applies `patches` to the record bodies of the `Records` frame, recomputing each changed
/// body's `body_hash` and the frame's hash.
fn patch_records(d: &mut [u8], patches: &[(usize, usize, u8)]) {
    let a = lpk_check::Archive::open(d.to_vec(), lpk_check::Options::default()).unwrap();
    let off = a.index.records.unwrap().offset as usize;
    let f = lpk_check::wire::parse_frame(d, off, u64::MAX).unwrap();
    let payload = f.end - 32 - f.payload.len()..f.end - 32;
    let varint = |d: &[u8], pos: &mut usize| {
        let (mut v, mut shift) = (0usize, 0);
        loop {
            let b = d[*pos];
            *pos += 1;
            v |= usize::from(b & 0x7F) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                return v;
            }
        }
    };
    let mut pos = payload.start;
    let count = varint(d, &mut pos);
    for id in 0..count {
        pos += 4; // kind, flags
        let len = varint(d, &mut pos);
        let body = pos..pos + len;
        let mine: Vec<_> = patches.iter().filter(|p| p.0 == id).collect();
        for &&(_, at, byte) in &mine {
            d[body.start + at] = byte;
        }
        if !mine.is_empty() {
            let h = blake3::hash(&d[body.clone()]);
            d[body.end..body.end + 32].copy_from_slice(h.as_bytes());
        }
        pos = body.end + 32;
    }
    let h = blake3::hash(&d[payload.clone()]);
    d[payload.end..payload.end + 32].copy_from_slice(h.as_bytes());
}

/// A case with record bytes patched after the write.
fn patched(mut c: Case, patches: &[(usize, usize, u8)]) -> Case {
    c.patches = patches.to_vec();
    c
}

/// The outcome as (class, normative reason, record id) — reason and id for `BadRecord` only —
/// and the bytes of `img.bin` when the block decodes.
type Outcome = (String, String, String, Option<Vec<u8>>);

fn reference(bytes: &[u8]) -> Outcome {
    let opened = lpk_format::Archive::open(Cursor::new(bytes.to_vec()), &Resources::default());
    let verified = opened.and_then(|mut a| {
        register_full_reader(&mut a);
        a.verify().map(|_| a)
    });
    match verified {
        Ok(mut a) => {
            let t = a.entry_table().unwrap();
            let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
            let e = entries
                .iter()
                .find(|e| e.path == IMG && e.kind == EntryKind::File)
                .unwrap();
            let mut got = Vec::new();
            a.extract(e, &mut got).unwrap();
            ("ok".into(), String::new(), String::new(), Some(got))
        }
        Err(err) => {
            let dbg = format!("{err:?}");
            let class = dbg.split([' ', '{', '(']).next().unwrap().to_string();
            if class == "BadRecord" {
                // `record N: bad record (reason)`.
                let msg = err.to_string();
                let rest = msg.strip_prefix("record ").unwrap();
                let (id, rest) = rest.split_once(": bad record (").unwrap();
                let reason = rest.trim_end_matches(')').to_string();
                (class, reason, id.to_string(), None)
            } else {
                (class, String::new(), String::new(), None)
            }
        }
    }
}

fn check(bytes: &[u8]) -> Outcome {
    let verified = lpk_check::Archive::open(bytes.to_vec(), lpk_check::Options::default())
        .and_then(|mut a| a.verify().map(|_| a));
    match verified {
        Ok(mut a) => {
            let e = a
                .entries()
                .unwrap()
                .into_iter()
                .find(|e| e.path == IMG)
                .unwrap();
            let got = a.read_file(&e).unwrap();
            ("ok".into(), String::new(), String::new(), Some(got))
        }
        Err(e) if e.class == "BadRecord" => {
            let rest = e.detail.strip_prefix("bad record ").unwrap();
            let (id, reason) = rest.split_once(": ").unwrap();
            (e.class.into(), reason.into(), id.into(), None)
        }
        Err(e) => (e.class.into(), String::new(), String::new(), None),
    }
}

// ---- The cases ----

/// Every (colour type, bit depth) pair the text allows.
const PAIRS: [(u8, u8); 15] = [
    (0, 1),
    (0, 2),
    (0, 4),
    (0, 8),
    (0, 16),
    (3, 1),
    (3, 2),
    (3, 4),
    (3, 8),
    (2, 8),
    (2, 16),
    (4, 8),
    (4, 16),
    (6, 8),
    (6, 16),
];

fn rec(i: &Image) -> RecordBody {
    RecordBody::PngFilter(i.rec.clone())
}

/// `[store, png-filter 0]`: the image's input stored, the block's plain bytes its output.
fn stored(name: impl Into<String>, i: &Image, want: (&'static str, &'static str)) -> Case {
    case(
        name,
        vec![rec(i)],
        vec![store(), png(0)],
        i.input.clone(),
        i.output.clone(),
        want,
    )
}

/// `[store, png-filter 1, deflate-reconstruct 0]`, as a writer lays out a re-filtered image:
/// record 0 the `deflate` record of the output, record 1 the `png-filter` record.
fn peeled(name: impl Into<String>, i: &Image, want: (&'static str, &'static str)) -> Case {
    let (stream, d) = deflate_of(&i.output);
    let mut c = case(
        name,
        vec![RecordBody::Deflate(d.clone()), rec(i)],
        vec![store(), png(1), deflate(0)],
        i.input.clone(),
        stream,
        want,
    );
    c.memory = lpk_check::deflate::term(d.plain_len, d.corrections.len() as u64);
    c
}

#[allow(clippy::too_many_lines)]
fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    // Every allowed pair, not interlaced, every filter type in and out.
    for (n, &(ct, bd)) in PAIRS.iter().enumerate() {
        let i = simple(7, 5, bd, ct, 0, n as u64 + 1);
        v.push(stored(format!("pair ct {ct} bd {bd}"), &i, OK));
    }
    // Adam7, empty passes included.
    for (n, &(w, h, ct, bd)) in [
        (1u32, 1u32, 0u8, 1u8),
        (3, 3, 2, 8),
        (5, 1, 0, 2),
        (1, 9, 4, 16),
        (8, 8, 6, 8),
        (9, 9, 3, 4),
        (33, 7, 6, 16),
        (2, 2, 0, 16),
    ]
    .iter()
    .enumerate()
    {
        let i = simple(w, h, bd, ct, 1, 100 + n as u64);
        v.push(stored(format!("adam7 {w}x{h} ct {ct} bd {bd}"), &i, OK));
    }
    // Packed bits past the last pixel are part of the scanline (grey 1-bit, 9 pixels).
    v.push(stored("grey1 9 wide", &simple(9, 4, 1, 0, 0, 7), OK));
    // Width and height at their lower bound.
    v.push(stored("1x1", &simple(1, 1, 1, 0, 0, 8), OK));
    // Every input type against every output type on one image.
    for t_in in 0..5u8 {
        let i = image(
            6,
            5,
            8,
            2,
            0,
            &|_| t_in,
            &|k| (k % 5) as u8,
            200 + u64::from(t_in),
        );
        v.push(stored(format!("input type {t_in}"), &i, OK));
    }
    // The step first and last: its input is the encoded bytes.
    let i = simple(10, 4, 8, 0, 0, 9);
    v.push(case(
        "first step",
        vec![rec(&i)],
        vec![png(0)],
        i.input.clone(),
        i.output.clone(),
        OK,
    ));
    // As a writer lays it out, and with the step first before deflate-reconstruct.
    let i = image(16, 8, 8, 2, 0, &|_| 2, &|k| (k % 5) as u8, 10);
    v.push(peeled("peeled", &i, OK));
    let i = image(12, 6, 16, 6, 1, &|_| 2, &|k| (4 - k % 5) as u8, 11);
    v.push(peeled("peeled adam7", &i, OK));
    {
        let i = simple(8, 3, 8, 4, 0, 12);
        let (stream, d) = deflate_of(&i.output);
        let mut c = case(
            "first step before deflate",
            vec![RecordBody::Deflate(d.clone()), rec(&i)],
            vec![png(1), deflate(0)],
            i.input.clone(),
            stream,
            OK,
        );
        c.memory = lpk_check::deflate::term(d.plain_len, d.corrections.len() as u64);
        v.push(c);
    }
    // Two png-filter steps: the first's bound is the second's F (the look-ahead).
    {
        let mid = image(9, 4, 8, 2, 0, &|_| 1, &|_| 3, 13);
        let last = image(9, 4, 8, 2, 0, &|_| 3, &|k| (k % 5) as u8, 13);
        assert_eq!(mid.output, last.input);
        v.push(case(
            "two png-filter steps",
            vec![rec(&mid), rec(&last)],
            vec![store(), png(0), png(1)],
            mid.input.clone(),
            last.output.clone(),
            OK,
        ));
        // The first image's F one row longer than the second's: PayloadTooLarge before
        // anything runs; one row shorter: its output is too short for the second (check (5)).
        let big = image(9, 5, 8, 2, 0, &|_| 1, &|_| 3, 13);
        v.push(case(
            "two steps, first F above the second's",
            vec![rec(&big), rec(&last)],
            vec![store(), png(0), png(1)],
            big.input.clone(),
            last.output.clone(),
            ("PayloadTooLarge", ""),
        ));
        let small = image(9, 3, 8, 2, 0, &|_| 1, &|_| 3, 13);
        v.push(case(
            "two steps, first F below the second's",
            vec![rec(&small), rec(&last)],
            vec![store(), png(0), png(1)],
            small.input.clone(),
            last.output.clone(),
            bad("scanlines"),
        ));
    }
    // Followed by a step that bounds nothing: bounded by max_block_plain.
    let i = simple(5, 5, 8, 0, 0, 14);
    v.push(case(
        "before store",
        vec![rec(&i)],
        vec![store(), png(0), store()],
        i.input.clone(),
        i.output.clone(),
        OK,
    ));

    // Check (3): width, height, the pair, the filters.
    let mut i = simple(4, 2, 8, 0, 0, 20);
    i.rec.width = 0;
    v.push(stored("width 0", &i, bad("width")));
    let mut i = simple(4, 2, 8, 0, 0, 21);
    i.rec.height = 0;
    i.rec.filters.clear();
    v.push(stored("height 0", &i, bad("height")));
    let mut i = simple(4, 2, 8, 0, 0, 21);
    i.rec.width = 0;
    i.rec.height = 0;
    i.rec.filters.clear();
    v.push(stored("width and height 0", &i, bad("width")));
    for (ct, bd) in [(2u8, 4u8), (3, 16), (4, 1), (6, 2), (2, 1), (4, 4), (6, 4)] {
        let mut i = simple(4, 2, 8, 0, 0, 22);
        i.rec.color_type = ct;
        i.rec.bit_depth = bd;
        v.push(stored(
            format!("pair ct {ct} bd {bd}"),
            &i,
            bad("bit_depth"),
        ));
    }
    let mut i = simple(0x10, 3, 8, 0, 0, 22);
    i.rec.width = 0;
    i.rec.color_type = 2;
    i.rec.bit_depth = 4;
    v.push(stored("width 0 and a bad pair", &i, bad("width")));
    let mut i = simple(6, 6, 8, 2, 1, 23);
    i.rec.filters.push(0);
    v.push(stored("adam7 one filter too many", &i, bad("filters")));
    let mut i = simple(6, 6, 8, 2, 1, 23);
    i.rec.filters.pop();
    v.push(stored("adam7 one filter too few", &i, bad("filters")));
    let mut i = simple(1, 1, 8, 0, 1, 24);
    i.rec.filters = vec![0; 7];
    v.push(stored("adam7 1x1 a filter per pass", &i, bad("filters")));
    let mut i = simple(6, 6, 8, 2, 1, 23);
    i.rec.filters[3] = 5;
    v.push(stored("filter byte 5", &i, bad("filters")));
    let mut i = simple(6, 6, 8, 2, 0, 23);
    *i.rec.filters.last_mut().unwrap() = 255;
    v.push(stored("filter byte 255", &i, bad("filters")));
    let mut i = simple(6, 6, 8, 2, 1, 23);
    i.rec.filters.pop();
    i.rec.filters[0] = 9;
    i.rec.height = 0;
    v.push(stored("height 0 before filters", &i, bad("height")));

    // Check (4) on the last step.
    let i = simple(7, 3, 8, 2, 0, 30);
    let mut short = i.output.clone();
    short.pop();
    let mut c = stored("plain_len F - 1", &i, ("BlockLengthMismatch", ""));
    c.plain = short;
    v.push(c);
    let mut long = i.output.clone();
    long.push(0);
    let mut c = stored("plain_len F + 1", &i, ("BlockLengthMismatch", ""));
    c.plain = long.clone();
    v.push(c);
    v.push(case(
        "first step plain_len F + 1",
        vec![rec(&i)],
        vec![png(0)],
        long.clone(),
        long,
        ("BlockLengthMismatch", ""),
    ));
    let mut huge = simple(1, 1, 16, 6, 0, 31);
    huge.rec.width = u32::MAX;
    v.push(stored(
        "width u32::MAX last",
        &huge,
        ("BlockLengthMismatch", ""),
    ));
    // Check (4) on a step that is not the last.
    {
        let i = simple(7, 3, 8, 2, 0, 32);
        let (stream, mut d) = deflate_of(&i.output);
        d.plain_len -= 1;
        let mut c = peeled("deflate plain_len F - 1", &i, ("PayloadTooLarge", ""));
        c.records[0] = RecordBody::Deflate(d.clone());
        c.plain = stream.clone();
        v.push(c);
        d.plain_len += 2;
        let mut c = peeled("deflate plain_len F + 1", &i, bad("plain_len"));
        c.memory = lpk_check::deflate::term(d.plain_len, d.corrections.len() as u64);
        c.records[0] = RecordBody::Deflate(d);
        c.plain = stream;
        v.push(c);
        let mut c = peeled("width u32::MAX not last", &i, ("PayloadTooLarge", ""));
        c.records[1] = rec(&huge);
        v.push(c);
    }
    v.push(case(
        "width u32::MAX before store",
        vec![rec(&huge)],
        vec![store(), png(0), store()],
        huge.input.clone(),
        huge.output.clone(),
        ("PayloadTooLarge", ""),
    ));
    // The look-ahead with bwt after: the bound is I(O, B).
    {
        let o = 100u64;
        let i_ob = lpk_check::bwt::input_len(o, 4096);
        for (w, want) in [
            (i_ob, ("PayloadTooLarge", "")),
            (i_ob - 1, ("BlockLengthMismatch", "")), // png-filter decodes; bwt check (3)
        ] {
            let i = simple(w as u32, 1, 8, 0, 0, 33);
            let mut c = case(
                format!("before bwt, F = {}", w + 1),
                vec![rec(&i)],
                vec![store(), png(0), bwt(4096)],
                i.input.clone(),
                vec![0x41; o as usize],
                want,
            );
            c.memory = lpk_check::bwt::term(o, 4096);
            v.push(c);
        }
    }

    // Check (5): the input's length.
    let i = simple(7, 3, 8, 2, 0, 40);
    let mut c = stored("input F - 1", &i, bad("scanlines"));
    c.encoded.pop();
    v.push(c);
    let mut c = stored("input F + 1", &i, ("PayloadTooLarge", ""));
    c.encoded.push(0);
    v.push(c);
    for (n, d) in [
        (-1i64, "first step input F - 1"),
        (1, "first step input F + 1"),
    ] {
        let mut enc = i.input.clone();
        if n < 0 {
            enc.pop();
        } else {
            enc.push(0);
        }
        v.push(case(
            d,
            vec![rec(&i)],
            vec![png(0)],
            enc,
            i.output.clone(),
            bad("scanlines"),
        ));
    }
    // Check (6): an input filter byte above 4.
    let mut c = stored("input filter 5 first", &i, bad("scanlines"));
    c.encoded[0] = 5;
    v.push(c);
    let line = 1 + 7 * 3;
    let mut c = stored("input filter 255 last", &i, bad("scanlines"));
    c.encoded[2 * line] = 255;
    v.push(c);
    let ia = simple(6, 6, 8, 2, 1, 41);
    let mut c = stored("adam7 input filter 7 last", &ia, bad("scanlines"));
    let last_line = 1 + 3 * 6; // pass 7: 6 pixels of RGB 8-bit
    let at = c.encoded.len() - last_line;
    c.encoded[at] = 7;
    v.push(c);
    // Check (7): the hash, and (6) before (7).
    let mut i2 = simple(7, 3, 8, 2, 0, 40);
    i2.rec.original_hash[5] ^= 0x10;
    v.push(stored("hash", &i2, bad("original_hash")));
    let mut c = stored("bad input filter and hash", &i2, bad("scanlines"));
    c.encoded[line] = 6;
    v.push(c);
    // An input that is a valid filtered text of the image but of other raw bytes.
    let other = simple(7, 3, 8, 2, 0, 99);
    let mut c = stored("other image's input", &i, bad("original_hash"));
    c.encoded.clone_from(&other.input);
    v.push(c);

    // Check (2): the kind, and the order of the pre-checks.
    {
        let i = simple(7, 3, 8, 2, 0, 50);
        let mut c = peeled("png step names the deflate record", &i, bad("kind"));
        c.steps[1] = png(0);
        v.push(c);
        // (The writer refuses a deflate-reconstruct step naming another kind, so that order is
        // not crafted here.) A palette image peeled, Adam7, 4-bit.
        v.push(peeled(
            "peeled palette adam7",
            &simple(17, 13, 4, 3, 1, 51),
            OK,
        ));
        let mut bad_png = i.rec.clone();
        bad_png.width = 0;
        let mut c = peeled(
            "deflate pre-check before png pre-check",
            &i,
            ("Refused", ""),
        );
        // The deflate record's term above the declared decode memory (its check (5)).
        c.records[1] = RecordBody::PngFilter(bad_png);
        c.memory = 0;
        v.push(c);
        let mut c = case(
            "png step names a deflate record, last",
            Vec::new(),
            vec![store(), png(0)],
            i.input.clone(),
            i.output.clone(),
            bad("kind"),
        );
        let (_, d) = deflate_of(&i.output);
        c.records = vec![RecordBody::Deflate(d)];
        v.push(c);
    }

    // Section 12's rules (clarified text): the non-interlaced count is raised by the lookup of
    // check (1), before `width` and `height`; `bit_depth` before `color_type` before `interlace`.
    // The writer refuses these records, so a valid one (4 x 3 grey 8-bit, three filter bytes) is
    // written and its body patched: width at 0..4, height at 4..8, bit_depth 8, color_type 9,
    // interlace 10.
    let base = simple(4, 3, 8, 0, 0, 60);
    for (name, p, want) in [
        (
            "non-interlaced count one short",
            vec![(0, 4, 4)],
            bad("filters"),
        ),
        (
            "non-interlaced count one long, width 0",
            vec![(0, 4, 2), (0, 0, 0)],
            bad("filters"),
        ),
        (
            "non-interlaced count wrong, width and height 0",
            vec![(0, 0, 0), (0, 4, 0)],
            bad("filters"),
        ),
        (
            "bit_depth and color_type out of range",
            vec![(0, 8, 3), (0, 9, 1), (0, 10, 2)],
            bad("bit_depth"),
        ),
        (
            "color_type and interlace out of range",
            vec![(0, 9, 5), (0, 10, 2)],
            bad("color_type"),
        ),
        ("interlace 2", vec![(0, 10, 2)], bad("interlace")),
        (
            "interlace 2 and a wrong count",
            vec![(0, 10, 2), (0, 4, 9)],
            bad("interlace"),
        ),
    ] {
        v.push(patched(stored(name, &base, want), &p));
    }
    // Check (1): the lookup of record 1 walks record 0, which breaks a rule.
    {
        let good = simple(4, 3, 8, 0, 0, 62);
        let c = case(
            "lookup walks a broken earlier record",
            vec![rec(&good), rec(&good)],
            vec![store(), png(1)],
            good.input.clone(),
            good.output.clone(),
            bad("interlace"),
        );
        v.push(patched(c, &[(0, 10, 2)]));
    }
    // The look-ahead reaching png-filter from a following bwt: `[store, bwt, png-filter]`, the
    // bwt step's O is the record's F and the store step is bounded by I(F, B).
    {
        let i = simple(10, 4, 8, 0, 0, 63);
        let f = i.output.len() as u64;
        let b = 16u32;
        let i_fb = lpk_check::bwt::input_len(f, b);
        for (len, want) in [(i_fb + 1, ("PayloadTooLarge", "")), (i_fb, ("any", ""))] {
            let mut c = case(
                format!("bwt before png-filter, input {len}"),
                vec![rec(&i)],
                vec![store(), png(0), bwt(b)], // swapped after the write
                vec![0x41; len as usize],
                i.output.clone(),
                want,
            );
            c.memory = lpk_check::bwt::term(f, b);
            c.swap_last_two = true;
            v.push(c);
        }
        // The png record's pre-checks run before the bwt's term is used: a bad width first.
        let mut w0 = i.rec.clone();
        w0.width = 0;
        let mut c = case(
            "bwt before png-filter, width 0",
            vec![RecordBody::PngFilter(w0)],
            vec![store(), png(0), bwt(b)], // swapped after the write
            vec![0x41; i_fb as usize],
            i.output.clone(),
            bad("width"),
        );
        c.memory = lpk_check::bwt::term(f, b);
        c.swap_last_two = true;
        v.push(c);
    }
    // The look-ahead reaching png-filter from a following deflate-reconstruct:
    // `[store, deflate-reconstruct 0, png-filter 1]`, the deflate record's own bound is the png
    // record's F. Written as `[store, png-filter 1, deflate-reconstruct 0]` with F equal to the
    // stream's length (what the writer accepts), then swapped; the deflate record's
    // `original_len` patched one above F reaches PayloadTooLarge (its check (3) against the png
    // record's F) before the store step runs.
    {
        let text = b"some text for a deflate stream, some text, some text".to_vec();
        let (stream, d) = deflate_of(&text);
        let l = stream.len() as u32;
        let i = simple(l - 1, 1, 8, 0, 0, 64);
        for (patch, want) in [(true, ("PayloadTooLarge", "")), (false, ("any", ""))] {
            let mut c = case(
                format!("deflate before png-filter, original_len patched {patch}"),
                vec![RecordBody::Deflate(d.clone()), rec(&i)],
                vec![store(), png(1), deflate(0)], // swapped after the write
                text.clone(),
                stream.clone(),
                want,
            );
            c.memory = lpk_check::deflate::term(d.plain_len, d.corrections.len() as u64);
            c.swap_last_two = true;
            if patch {
                c.patches = vec![(0, 0, (l + 1) as u8)];
            }
            v.push(c);
        }
    }
    v
}

#[test]
fn crafted_png_filter_records_agree_with_the_reference() {
    let cases = cases();
    assert!(cases.len() >= 40, "{}", cases.len());
    let mut disagreements = Vec::new();
    let mut agreed = 0;
    let mut missed = Vec::new();
    for c in &cases {
        let bytes = build(c);
        let r = reference(&bytes);
        eprintln!("{}: {:?} {:?}", c.name, r.0, r.1);
        let k = check(&bytes);
        let summary = |o: &Outcome| {
            format!(
                "{} {} {} {}",
                o.0,
                o.1,
                o.2,
                o.3.as_ref()
                    .map_or(String::new(), |b| blake3::hash(b).to_hex().to_string())
            )
        };
        if r == k {
            agreed += 1;
        } else {
            disagreements.push(format!(
                "{}: reference {} / lpk-check {}",
                c.name,
                summary(&r),
                summary(&k)
            ));
        }
        // The case reaches the outcome it was built for (by this decoder).
        if (c.want.0 != "any" && (k.0.as_str(), k.1.as_str()) != c.want)
            || (k.0 == "ok" && k.3.as_deref() != Some(&c.plain[..]))
        {
            missed.push(format!("{}: {}", c.name, summary(&k)));
        }
    }
    assert!(disagreements.is_empty(), "{disagreements:#?}");
    assert!(missed.is_empty(), "{missed:#?}");
    eprintln!(
        "crafted png-filter records: {agreed} of {} agree",
        cases.len()
    );
    assert_eq!(agreed, cases.len());
}
