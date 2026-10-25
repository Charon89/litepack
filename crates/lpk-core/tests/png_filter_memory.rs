//! Revision 1.4's claim "Resources: none" for `png-filter` (spec section 8): the decoder holds
//! its output (the image's filtered length) and two rows beyond its input, so a block
//! `[M, png-filter, deflate-reconstruct]` needs no more than `max_block_plain` plus the deflate
//! term, which already counts the png-filter step's output as the deflate step's input and
//! seven more bytes per plain byte. Measured with a counting global allocator on a 12 MiB
//! image, above any test block size; its own test binary, its tests one at a time, so no other
//! test allocates meanwhile. A second test measures a whole `[store, png-filter,
//! deflate-reconstruct]` block through the full reader and the D-54 engine.
#![allow(clippy::unwrap_used)]

use lpk_core::peel::deflate::memory_term;
use lpk_core::peel::png::{Layout, FILTER_UP};
use lpk_core::PngFilterDecoder;
use lpk_format::{
    DecodeContext, FormatError, PngFilterRecord, PrimitiveDecoder, Record, RecordBody, Resources,
};
use lpk_procstat_sys::alloc::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc::new();

/// The tests of this binary measure one at a time (the allocator counts every thread).
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Ctx(Record);

impl DecodeContext for Ctx {
    fn block(&self) -> usize {
        0
    }
    fn record(&mut self, _id: u64) -> Result<Record, FormatError> {
        Ok(self.0.clone())
    }
    fn chunk(&mut self, _record: u64, index: u64) -> Result<Vec<u8>, FormatError> {
        Err(FormatError::ChunkIndexOutOfRange {
            chunk: index,
            len: 0,
        })
    }
}

#[test]
fn the_decoder_holds_its_output_and_two_rows() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    for (w, h, depth, ct, interlace) in [(2048u32, 2048u32, 8u8, 2u8, 0u8), (1500, 1100, 16, 6, 1)]
    {
        let l = Layout::new(w, h, depth, ct, interlace).unwrap();
        let mut x = 7u32;
        let raw: Vec<u8> = (0..l.raw_len().unwrap())
            .map(|i| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((x >> 28) as u8).wrapping_add((i % 251) as u8)
            })
            .collect();
        let n = l.scanlines() as usize;
        let filters: Vec<u8> = (0..n).map(|i| (i % 5) as u8).collect();
        let text = l.refilter(&raw, &filters).unwrap();
        let up = l.refilter_uniform(&raw, FILTER_UP).unwrap();
        drop(raw);
        let rec = Record::new(RecordBody::PngFilter(PngFilterRecord {
            width: w,
            height: h,
            bit_depth: depth,
            color_type: ct,
            interlace,
            filters,
            original_hash: *blake3::hash(&text).as_bytes(),
        }));
        let len = text.len() as u64;
        let mut ctx = Ctx(rec);
        ALLOC.reset_peak();
        let before = ALLOC.current();
        let out = PngFilterDecoder
            .decode_in(&[0], &up, len, true, &Resources::default(), &mut ctx)
            .unwrap();
        let peak = (ALLOC.peak() - before) as u64;
        assert!(out == text);
        let rows = 2 * l.row_bytes(w);
        // The output, two rows, the record's parsed copy (filters) and small change.
        let allowed = len + rows + n as u64 + (64 << 10);
        assert!(peak <= allowed, "{peak} > {allowed}");
        // Within the deflate term of a stream with this text (beyond the input, which the
        // term's first byte per plain byte counts).
        assert!(peak <= memory_term(len, 0), "{peak}");
        assert!(len > 8 << 20);
    }
}

/// A PNG of `l` with raw rows `raw` filtered with `filters`, zlib level 6, one IDAT.
fn png_file(l: &Layout, raw: &[u8], filters: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let text = l.refilter(raw, filters).unwrap();
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&text).unwrap();
    let z = z.finish().unwrap();
    let chunk = |out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(body);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    };
    let mut out = lpk_core::peel::png::SIGNATURE.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&l.width().to_be_bytes());
    ihdr.extend_from_slice(&l.height().to_be_bytes());
    ihdr.extend_from_slice(&[l.bit_depth(), l.color_type(), 0, 0, l.interlace()]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The design's block-level claim (`docs/notes/e2-16-design.md`, task C): a block
/// `[store, png-filter, deflate-reconstruct]` of a 3 MB image decoded through the full reader
/// (`Archive::extract`) and through the D-54 engine (`extract_archive`, one thread) peaks within
/// `max_block_plain` plus the `deflate` record's term. `store` keeps the model step's own
/// memory out of the measurement (an `lzma` or `zstd` step adds its window, counted by the
/// envelope's `max_window`, not by the term).
#[test]
fn a_png_filter_block_peaks_within_max_block_plain_plus_the_deflate_term() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    use lpk_core::peel::deflate::{examine, Limits};
    use lpk_core::peel::png::Header;
    use lpk_format::{
        Archive, DeflateRecord, Encoded, EntryFlags, Graph, PrimitiveId, Step, Writer,
        WriterOptions,
    };
    let l = Layout::new(1024, 1024, 8, 2, 0).unwrap();
    let mut x = 11u32;
    let raw: Vec<u8> = (0..l.raw_len().unwrap())
        .map(|i| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((x >> 29) as u8).wrapping_add((i / 3072) as u8)
        })
        .collect();
    let filters: Vec<u8> = (0..1024).map(|i| (i % 5) as u8).collect();
    let file = png_file(&l, &raw, &filters);
    drop(raw);
    let mut streams = Vec::new();
    examine(&file, "png", &Limits::standard(), |r| streams.push(r));
    let r = streams.remove(0);
    let (a, b) = r.spans[0];
    let deflate = file[a..b].to_vec();
    let text = r.plain.text().to_vec();
    let h = Header::parse(&file).unwrap().layout().unwrap();
    let fb = h.filter_bytes(&text).unwrap();
    let up = h.transcode(&text, &vec![FILTER_UP; fb.len()]).unwrap();
    let png = PngFilterRecord {
        width: 1024,
        height: 1024,
        bit_depth: 8,
        color_type: 2,
        interlace: 0,
        filters: fb,
        original_hash: *blake3::hash(&text).as_bytes(),
    };
    let dr = DeflateRecord {
        original_len: deflate.len() as u64,
        plain_len: text.len() as u64,
        corrections: r.corrections,
        library: 0,
        original_hash: *blake3::hash(&deflate).as_bytes(),
    };
    let term = memory_term(dr.plain_len, dr.corrections.len() as u64);
    let mut out = Vec::new();
    let mut w = Writer::new_revision(
        &mut out,
        WriterOptions {
            block_size: 8 << 20,
            ..WriterOptions::default()
        },
        4,
    )
    .unwrap();
    w.begin_entry("big.png", EntryFlags::EMPTY, 0).unwrap();
    w.add_part(0, &mut &file[..a]).unwrap();
    let d = w.add_record(Record::new(RecordBody::Deflate(dr))).unwrap();
    let p = w
        .add_record(Record::new(RecordBody::PngFilter(png)))
        .unwrap();
    let graph = Graph {
        steps: vec![
            Step {
                primitive: PrimitiveId::Store,
                params: Vec::new(),
            },
            Step {
                primitive: PrimitiveId::PngFilter,
                params: vec![p as u8],
            },
            Step {
                primitive: PrimitiveId::DeflateReconstruct,
                params: vec![d as u8],
            },
        ],
    };
    let block = Encoded {
        resources: graph.resources(),
        memory: 0,
        graph,
        bytes: up,
    };
    w.add_part_encoded(a as u64, &deflate, block, term).unwrap();
    w.add_part(b as u64, &mut &file[b..]).unwrap();
    w.end_entry().unwrap();
    w.finish().unwrap();
    drop((text, deflate));
    let open = || {
        let mut a =
            Archive::open(std::io::Cursor::new(out.clone()), &Resources::default()).unwrap();
        lpk_core::register_full_reader(&mut a);
        a
    };
    let mut archive = open();
    let max_block_plain = archive.index().envelope.max_block_plain;
    let allowed = max_block_plain + term;
    let entry = {
        let t = archive.entry_table().unwrap();
        let e = t.table().unwrap().iter().next().unwrap().unwrap();
        e
    };
    // The full reader. The extracted file is the caller's buffer, reserved beforehand.
    let mut got = Vec::with_capacity(file.len());
    ALLOC.reset_peak();
    let before = ALLOC.current();
    archive.extract(&entry, &mut got).unwrap();
    let peak = (ALLOC.peak() - before) as u64;
    assert!(got == file);
    assert!(peak <= allowed, "full reader: {peak} > {allowed}");
    // The D-54 engine, one thread.
    let dir = tempfile::tempdir().unwrap();
    let mut archive = open();
    ALLOC.reset_peak();
    let before = ALLOC.current();
    lpk_core::extract_archive(
        &mut archive,
        || Ok(std::io::Cursor::new(out.clone())),
        dir.path(),
        &lpk_core::DefaultPolicy,
        &lpk_core::ExtractOptions {
            threads: Some(1),
            ..lpk_core::ExtractOptions::default()
        },
    )
    .unwrap();
    let peak = (ALLOC.peak() - before) as u64;
    assert!(std::fs::read(dir.path().join("big.png")).unwrap() == file);
    assert!(peak <= allowed, "D-54 engine: {peak} > {allowed}");
    println!(
        "full reader and engine within {allowed} (max_block_plain {max_block_plain} + term {term})"
    );
}
