//! Revision 1.4's PNG row-filter re-encoding through the Balanced pipeline (E2-16 task D, design
//! `docs/notes/e2-16-design.md` section 3): synthetic PNGs of every (bit depth, colour type)
//! pair with and without Adam7 round-trip through the full reader and the D-54 engine; the gates
//! (an image filtered with None throughout and a palette image are not tried, a sprite is
//! re-encoded, a small file goes whole through the file gate); minor 4; the archive is
//! byte-identical at one and eight threads and sealed; an append onto a revision 1.3 archive
//! runs without the re-encoding and says so; `--png-filter off` writes minor 3.
#![allow(clippy::unwrap_used)]

use std::io::{Cursor, Write};
use std::path::Path;

use lpk_core::peel::deflate::stage::png_candidate;
use lpk_core::peel::png::{Layout, PAIRS, SIGNATURE};
use lpk_core::{
    extract_file, register_full_reader, BalancedOptions, DefaultPolicy, ExtractOptions, Pipeline,
    RunSummary,
};
use lpk_format::{
    Archive, Argon2Params, Credentials, EntryKind, PrimitiveId, Resources, SealOptions, Suite,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 56) as u8).collect()
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(body);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

/// A PNG of `l` with raw rows `raw` filtered with `filters`, deflated at `level`, one IDAT.
fn png(l: &Layout, raw: &[u8], filters: &[u8], level: u32) -> Vec<u8> {
    let text = l.refilter(raw, filters).unwrap();
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
    z.write_all(&text).unwrap();
    let z = z.finish().unwrap();
    let mut out = SIGNATURE.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&l.width().to_be_bytes());
    ihdr.extend_from_slice(&l.height().to_be_bytes());
    ihdr.extend_from_slice(&[l.bit_depth(), l.color_type(), 0, 0, l.interlace()]);
    chunk(&mut out, b"IHDR", &ihdr);
    if l.color_type() == 3 {
        chunk(&mut out, b"PLTE", &[0u8; 768]);
    }
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A sprite: RGBA 8-bit, smooth columns (each row the one above plus a small step), its rows
/// filtered with Sub, Average and Paeth chosen at random (as an encoder's heuristic does), so
/// equal regions get different bytes; with Up throughout every row but the first is nearly
/// constant.
fn sprite(w: u32, h: u32, seed: u64) -> Vec<u8> {
    let l = Layout::new(w, h, 8, 6, 0).unwrap();
    let mut rng = Rng(seed);
    let rb = l.row_bytes(w) as usize;
    let base = rng.bytes(rb);
    let mut raw = Vec::with_capacity(rb * h as usize);
    for y in 0..h as usize {
        for (x, b) in base.iter().enumerate() {
            raw.push(b.wrapping_add(((y * (1 + x % 3)) / 2) as u8));
        }
    }
    let filters: Vec<u8> = (0..h)
        .map(|_| [1, 3, 4][(rng.next() % 3) as usize])
        .collect();
    png(&l, &raw, &filters, 1)
}

fn balanced(threads: usize) -> Pipeline {
    Pipeline::balanced(BalancedOptions {
        block_size: 1 << 20,
        dict_size: 1 << 20,
        sample_len: 1 << 16,
        threads: Some(threads),
        ..BalancedOptions::default()
    })
}

fn pin_mtimes(root: &Path) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(1000);
    for e in std::fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(t).unwrap();
    }
}

/// Every pair with and without Adam7 (random filters, so each is tried at depth 8 and 16), two
/// sprites, a palette image and an image filtered with None throughout.
fn png_tree(root: &Path) {
    let mut rng = Rng(0xC0FF_EE00_1234_5678);
    for (depth, ct) in PAIRS {
        for interlace in 0..=1u8 {
            let l = Layout::new(61, 37, depth, ct, interlace).unwrap();
            let raw = rng.bytes(l.raw_len().unwrap() as usize);
            let n = l.scanlines() as usize;
            let filters: Vec<u8> = (0..n).map(|_| (rng.next() % 5) as u8).collect();
            std::fs::write(
                root.join(format!("p-{depth}-{ct}-{interlace}.png")),
                png(&l, &raw, &filters, 6),
            )
            .unwrap();
        }
    }
    std::fs::write(root.join("sprite-a.png"), sprite(256, 192, 1)).unwrap();
    std::fs::write(root.join("sprite-b.png"), sprite(200, 300, 2)).unwrap();
    std::fs::write(root.join("palette-none.png"), palette_none()).unwrap();
    std::fs::write(root.join("rgb-none.png"), rgb_none()).unwrap();
    pin_mtimes(root);
}

fn palette_none() -> Vec<u8> {
    let l = Layout::new(300, 200, 8, 3, 0).unwrap();
    let raw: Vec<u8> = (0..l.raw_len().unwrap()).map(|i| (i % 7) as u8).collect();
    png(&l, &raw, &[0; 200], 1)
}

fn rgb_none() -> Vec<u8> {
    let l = Layout::new(200, 150, 8, 2, 0).unwrap();
    let mut rng = Rng(77);
    let row = rng.bytes(600);
    let raw: Vec<u8> = (0..150).flat_map(|_| row.iter().copied()).collect();
    png(&l, &raw, &[0; 150], 1)
}

fn open(bytes: &[u8], credentials: Option<&Credentials>) -> Archive<Cursor<Vec<u8>>> {
    let mut a = Archive::open_with(
        Cursor::new(bytes.to_vec()),
        &Resources::default(),
        credentials,
    )
    .unwrap();
    register_full_reader(&mut a);
    a
}

fn run(p: Pipeline, root: &Path) -> (Vec<u8>, RunSummary) {
    let mut out = Vec::new();
    let s = p.run(root, &mut out).unwrap();
    (out, s)
}

/// Every file reads back through the full reader and `verify` is clean.
fn check(bytes: &[u8], root: &Path, credentials: Option<&Credentials>) {
    let mut a = open(bytes, credentials);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut v = Vec::new();
        a.extract(e, &mut v).unwrap();
        assert!(
            v == std::fs::read(root.join(&e.path)).unwrap(),
            "{}",
            e.path
        );
    }
    a.verify().unwrap();
}

/// The `[M, png-filter, deflate-reconstruct]` blocks of an archive.
fn png_blocks(bytes: &[u8]) -> usize {
    let a = open(bytes, None);
    let mut n = 0;
    for b in a.index().blocks.clone() {
        let frame = &bytes[b.frame_offset as usize..(b.frame_offset + b.frame_len) as usize];
        let Some(lpk_format::frame::ReadFrame::Known(f)) =
            lpk_format::Frame::read(&mut Cursor::new(frame), &Default::default()).unwrap()
        else {
            continue;
        };
        let steps = lpk_format::Graph::parse(&f.payload).unwrap().0.steps;
        if steps.iter().any(|s| s.primitive == PrimitiveId::PngFilter) {
            assert_eq!(steps.len(), 3);
            assert_eq!(steps[1].primitive, PrimitiveId::PngFilter);
            assert_eq!(steps[2].primitive, PrimitiveId::DeflateReconstruct);
            n += 1;
        }
    }
    n
}

fn engine_extracts(archive: &Path, root: &Path, credentials: Option<&Credentials>) {
    let out = tempfile::tempdir().unwrap();
    let s = extract_file(
        archive,
        out.path(),
        &[],
        credentials,
        &DefaultPolicy,
        &ExtractOptions::default(),
    )
    .unwrap();
    let mut files = 0;
    for e in std::fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap();
        assert!(
            std::fs::read(out.path().join(name)).unwrap() == std::fs::read(&p).unwrap(),
            "{}",
            p.display()
        );
        files += 1;
    }
    assert_eq!(files, s.files);
}

#[test]
fn the_gates_choose_the_images_tried() {
    // Not tried: filter None throughout, a palette, depth below 8, not a PNG.
    for (bytes, tried) in [
        (palette_none(), false),
        (rgb_none(), false),
        (sprite(64, 48, 3), true),
    ] {
        let text = inflated(&bytes);
        assert_eq!(
            png_candidate(&bytes, idat_start(&bytes), &text).is_some(),
            tried
        );
    }
    let l = Layout::new(40, 10, 4, 0, 0).unwrap();
    let raw = Rng(5).bytes(l.raw_len().unwrap() as usize);
    let low = png(&l, &raw, &[1; 10], 6);
    assert!(png_candidate(&low, idat_start(&low), &inflated(&low)).is_none());
    // A text whose length is not the image's (an ancillary zlib stream) is not tried.
    let s = sprite(64, 48, 3);
    assert!(png_candidate(&s, idat_start(&s), &inflated(&s)[1..]).is_none());
    assert!(png_candidate(b"not a png", 0, b"x").is_none());
}

/// Where the Deflate data of the first IDAT chunk starts (after the 2-byte zlib header).
fn idat_start(file: &[u8]) -> usize {
    let mut at = 8;
    while at + 8 <= file.len() {
        let len = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        if &file[at + 4..at + 8] == b"IDAT" {
            return at + 8 + 2;
        }
        at += 12 + len;
    }
    panic!("no IDAT")
}

/// Review fix 6: an iCCP chunk's zlib stream is never tried, even one whose inflated text is
/// the image's own filtered text (so only its chunk tells it apart): with the profile before
/// the image data, the file's only tried stream is the IDAT one.
#[test]
fn an_iccp_stream_is_not_tried() {
    let s = sprite(64, 48, 3);
    let text = inflated(&s);
    // The same file with an iCCP chunk (name, NUL, method 0, the zlib stream of the image's
    // filtered text) after IHDR.
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&text).unwrap();
    let mut body = b"icc\0\0".to_vec();
    body.extend_from_slice(&z.finish().unwrap());
    let ihdr_end = 8 + 12 + 13;
    let mut file = s[..ihdr_end].to_vec();
    let iccp_at = file.len();
    chunk(&mut file, b"iCCP", &body);
    file.extend_from_slice(&s[ihdr_end..]);
    // The iCCP stream's Deflate data starts after the chunk header, the name, the NUL, the
    // method and the zlib header.
    let iccp_start = iccp_at + 8 + 5 + 2;
    assert!(png_candidate(&file, iccp_start, &text).is_none());
    assert!(png_candidate(&file, idat_start(&file), &text).is_some());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("iccp.png"), &file).unwrap();
    pin_mtimes(dir.path());
    let (bytes, s) = run(balanced(1), dir.path());
    assert!(s.peel.deflate.png_tried <= 1, "{:?}", s.peel.deflate);
    check(&bytes, dir.path(), None);
}

/// The inflated IDAT text of a one-IDAT PNG written by [`png`].
fn inflated(file: &[u8]) -> Vec<u8> {
    let mut at = 8;
    let mut z = Vec::new();
    while at + 8 <= file.len() {
        let len = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        if &file[at + 4..at + 8] == b"IDAT" {
            z.extend_from_slice(&file[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&z[..]), &mut out).unwrap();
    out
}

#[test]
fn every_pair_round_trips_sprites_are_re_encoded_and_minor_is_4() {
    let dir = tempfile::tempdir().unwrap();
    png_tree(dir.path());
    let work = tempfile::tempdir().unwrap();
    let mut archives = Vec::new();
    for threads in [1, 8] {
        let archive = work.path().join(format!("t{threads}.lpk"));
        let s = balanced(threads).run_file(dir.path(), &archive).unwrap();
        let d = &s.peel.deflate;
        assert!(d.png_filtered >= 2, "the sprites: {d:?}");
        assert!(d.png_tried > d.png_filtered, "{d:?}");
        let bytes = std::fs::read(&archive).unwrap();
        assert_eq!(png_blocks(&bytes) as u64, d.png_filtered);
        assert_eq!(open(&bytes, None).header().version.minor, 4);
        check(&bytes, dir.path(), None);
        engine_extracts(&archive, dir.path(), None);
        archives.push(bytes);
    }
    assert!(archives[0] == archives[1], "1 and 8 threads differ");
    // Off: minor 3, no png-filter block, a larger archive on this tree.
    let mut off = balanced(1);
    off.deflate.as_mut().unwrap().png_filter = false;
    let (bytes, s) = run(off, dir.path());
    assert_eq!(s.peel.deflate.png_filtered, 0);
    assert_eq!(s.peel.deflate.png_tried, 0);
    assert_eq!(png_blocks(&bytes), 0);
    assert_eq!(open(&bytes, None).header().version.minor, 3);
    assert!(bytes.len() > archives[0].len());
}

#[test]
fn a_small_image_goes_whole_through_the_file_gate() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("tiny.png"), sprite(6, 4, 9)).unwrap();
    pin_mtimes(dir.path());
    let (bytes, s) = run(balanced(1), dir.path());
    let d = &s.peel.deflate;
    assert_eq!(d.png_filtered, 0);
    assert_eq!(d.peeled.files, 0, "{d:?}");
    assert_eq!(png_blocks(&bytes), 0);
    check(&bytes, dir.path(), None);
}

#[test]
fn sealed_archives_re_encode_and_extract() {
    let dir = tempfile::tempdir().unwrap();
    png_tree(dir.path());
    let work = tempfile::tempdir().unwrap();
    let credentials = Credentials::password("png");
    let mut lens = Vec::new();
    for threads in [1, 8] {
        let mut p = balanced(threads);
        p.seal.writer.seal = Some(SealOptions {
            suite: Suite::AesGcm,
            argon2: Argon2Params {
                t: 1,
                m_kib: 8192,
                p: 1,
            },
            listable: false,
            credentials: credentials.clone(),
        });
        let archive = work.path().join(format!("s{threads}.lpk"));
        let s = p.run_file(dir.path(), &archive).unwrap();
        assert!(s.peel.deflate.png_filtered >= 2);
        let bytes = std::fs::read(&archive).unwrap();
        check(&bytes, dir.path(), Some(&credentials));
        engine_extracts(&archive, dir.path(), Some(&credentials));
        lens.push(bytes.len());
    }
    assert_eq!(lens[0], lens[1]);
}

#[test]
fn an_append_onto_a_revision_1_3_archive_runs_without_the_re_encoding_and_says_so() {
    let v1 = tempfile::tempdir().unwrap();
    std::fs::write(v1.path().join("a.png"), sprite(64, 48, 11)).unwrap();
    pin_mtimes(v1.path());
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("a.lpk");
    let mut first = balanced(1);
    first.deflate.as_mut().unwrap().png_filter = false;
    first.run_file(v1.path(), &archive).unwrap();
    let v2 = tempfile::tempdir().unwrap();
    png_tree(v2.path());
    let s = balanced(1).append_file(v2.path(), &archive, None).unwrap();
    assert_eq!(s.peel.dropped_stages, vec!["png-filter"]);
    assert_eq!(s.peel.deflate.png_filtered, 0);
    let bytes = std::fs::read(&archive).unwrap();
    assert_eq!(open(&bytes, None).header().version.minor, 3);
    assert_eq!(png_blocks(&bytes), 0);
    check(&bytes, v2.path(), None);
    // Onto a revision 1.4 archive the re-encoding runs.
    let archive = work.path().join("b.lpk");
    balanced(1).run_file(v1.path(), &archive).unwrap();
    let s = balanced(1).append_file(v2.path(), &archive, None).unwrap();
    assert!(s.peel.dropped_stages.is_empty());
    assert!(s.peel.deflate.png_filtered >= 2);
}
