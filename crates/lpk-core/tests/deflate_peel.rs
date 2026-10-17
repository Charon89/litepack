//! The Deflate peel through the pipeline (E2-10/11 task D; design
//! `docs/notes/e2-10-11-design.md`, sections 1.3 and 3): synthetic ZIP, docx, JAR, multi-IDAT
//! PNG, gzip, PDF and scan inputs round-trip through the full reader (every kept record recreates
//! through the decoder's path, spec section 8's writer obligation); the gates; dedup; the header
//! minor; byte identity across threads and budgets; sealed archives; appends.
#![allow(clippy::unwrap_used)]

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use lpk_core::peel::deflate::DeflatePeel;
use lpk_core::{register_full_reader, BalancedOptions, FastOptions, Pipeline, RunSummary};
use lpk_format::{
    Archive, Argon2Params, Credentials, EntryKind, PrimitiveId, Resources, SealOptions, Suite,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/deflate")
            .join(name),
    )
    .unwrap()
}

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 12] = [
        "the", "king", "of", "France", "and", "army", "sea", "road", "<w:t>", "</w:t>", "river",
        "\n",
    ];
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(WORDS[(x % 12) as usize].as_bytes());
        out.push(b' ');
    }
    out.truncate(len);
    out
}

/// Prose that repeats a 40 KiB passage with a few changes per copy: beyond Deflate's 32 KiB
/// window, within the model's dictionary, so peeled streams gain.
fn repetitive(seed: u64, len: usize) -> Vec<u8> {
    let base = prose(seed, 40_000);
    let mut out = Vec::with_capacity(len + base.len());
    let mut copy = 0usize;
    while out.len() < len {
        let at = out.len();
        out.extend_from_slice(&base);
        out[at + (copy * 7919) % base.len()] = b'#';
        copy += 1;
    }
    out.truncate(len);
    out
}

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn zip_of(members: &[(&str, Vec<u8>, bool)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data, deflated) in members {
        let method = if *deflated {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        };
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(zip::DateTime::default());
        w.start_file(*name, o).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn zlib(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn gzip(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn pdf() -> Vec<u8> {
    let mut p = b"%PDF-1.4\n".to_vec();
    for (i, seed) in [(1, 21u64), (2, 22)] {
        p.extend_from_slice(format!("{i} 0 obj\n<< /Filter /FlateDecode >>\nstream\n").as_bytes());
        p.extend_from_slice(&zlib(&repetitive(seed, 90_000), 6));
        p.extend_from_slice(b"\nendstream\nendobj\n");
    }
    p.extend_from_slice(b"trailer\n<< >>\n%%EOF\n");
    p
}

/// Every kind the walker knows, plus a tiny stream that cannot pay for its record.
fn make_tree(root: &Path) {
    std::fs::write(root.join("doc.zip"), fixture("doc.zip")).unwrap();
    std::fs::write(root.join("image.png"), fixture("image.png")).unwrap();
    std::fs::write(root.join("notes.txt.gz"), fixture("notes.txt.gz")).unwrap();
    std::fs::write(root.join("readme.txt"), fixture("readme.txt")).unwrap();
    let docx = zip_of(&[
        ("[Content_Types].xml", prose(31, 2_000), true),
        ("word/document.xml", repetitive(32, 200_000), true),
        ("word/media/pic.bin", noise(33, 5_000), false),
    ]);
    std::fs::write(root.join("report.docx"), docx).unwrap();
    let jar = zip_of(&[
        (
            "META-INF/MANIFEST.MF",
            b"Manifest-Version: 1.0\n".repeat(20),
            true,
        ),
        ("a/A.class", repetitive(41, 90_000), true),
        ("a/B.class", repetitive(42, 85_000), true),
    ]);
    std::fs::write(root.join("app.jar"), jar).unwrap();
    std::fs::write(root.join("doc.pdf"), pdf()).unwrap();
    let mut scan = noise(51, 20_000);
    scan.extend_from_slice(&zlib(&repetitive(52, 100_000), 9));
    scan.extend_from_slice(&noise(53, 10_000));
    std::fs::write(root.join("scan.bin"), scan).unwrap();
    std::fs::write(root.join("data.gz"), gzip(&repetitive(61, 300_000), 6)).unwrap();
    std::fs::write(
        root.join("tiny.zip"),
        zip_of(&[("t.txt", b"a tiny member of thirty bytes".to_vec(), true)]),
    )
    .unwrap();
}

fn balanced(threads: usize) -> Pipeline {
    let mut p = Pipeline::balanced(BalancedOptions {
        block_size: 4 << 20,
        dict_size: 1 << 20,
        sample_len: 1 << 16,
        threads: Some(threads),
        ..BalancedOptions::default()
    });
    p.deflate = Some(DeflatePeel {
        record_files: true,
        ..DeflatePeel::default()
    });
    p
}

fn run(p: Pipeline, root: &Path) -> (Vec<u8>, RunSummary) {
    let mut out = Vec::new();
    let s = p.run(root, &mut out).unwrap();
    (out, s)
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

/// Every file of `root` reads back through the full reader, and `verify` (every step, so every
/// record's `original_hash`) is clean.
fn check(bytes: &[u8], root: &Path, credentials: Option<&Credentials>) {
    let mut a = open(bytes, credentials);
    let t = a.entry_table().unwrap();
    let entries: Vec<_> = t.table().unwrap().iter().map(|e| e.unwrap()).collect();
    let mut files = 0;
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut v = Vec::new();
        a.extract(e, &mut v).unwrap();
        assert!(
            v == std::fs::read(root.join(&e.path)).unwrap(),
            "{} differs",
            e.path
        );
        files += 1;
    }
    assert_eq!(files, std::fs::read_dir(root).unwrap().count());
    a.verify().unwrap();
}

/// The blocks of an unsealed archive naming `deflate-reconstruct`; each is `[M,
/// deflate-reconstruct]` with M `lzma` or `zstd`.
fn peeled_blocks(bytes: &[u8]) -> usize {
    let a = open(bytes, None);
    let mut peeled = 0;
    for b in a.index().blocks.clone() {
        let frame = &bytes[b.frame_offset as usize..(b.frame_offset + b.frame_len) as usize];
        let Some(lpk_format::frame::ReadFrame::Known(f)) =
            lpk_format::Frame::read(&mut Cursor::new(frame), &Default::default()).unwrap()
        else {
            panic!("not a block frame")
        };
        let steps = lpk_format::Graph::parse(&f.payload).unwrap().0.steps;
        if steps
            .iter()
            .any(|s| s.primitive == PrimitiveId::DeflateReconstruct)
        {
            assert_eq!(steps.len(), 2, "[M, deflate-reconstruct]");
            assert_eq!(steps[1].primitive, PrimitiveId::DeflateReconstruct);
            assert!(matches!(
                steps[0].primitive,
                PrimitiveId::Lzma | PrimitiveId::Zstd
            ));
            peeled += 1;
        }
    }
    peeled
}

#[test]
fn every_kind_round_trips_and_the_streams_are_partitioned() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let (bytes, s) = run(balanced(1), dir.path());
    let d = &s.peel.deflate;
    let a = open(&bytes, None);
    assert_eq!(a.header().version.minor, 2);
    check(&bytes, dir.path(), None);
    assert_eq!(peeled_blocks(&bytes) as u64, d.outcomes.kept, "{d:?}");
    // The partition of reconstructed (design 3.3).
    assert_eq!(d.outcomes.total(), d.streams.reconstructed, "{d:?}");
    assert_eq!(
        d.streams.found,
        d.streams.reconstructed + d.streams.skipped + d.streams.failed_total()
    );
    // The repetitive members: docx 1, jar 2, pdf 2, scan 1, data.gz 1.
    assert!(d.outcomes.kept >= 7, "{d:?}");
    for kind in ["zip", "office", "jar", "png", "gzip", "pdf", "other"] {
        assert!(d.walked_by_kind.contains_key(kind), "{kind}: {d:?}");
    }
    // Member streams reach the classifier: the docx's XML is text.
    assert!(
        d.kept_by_class.get("text").copied().unwrap_or(0) >= 1,
        "{d:?}"
    );
    let files = d.files.as_ref().unwrap();
    let docx = files.iter().find(|f| f.path == "report.docx").unwrap();
    assert_eq!(docx.kind, "office");
    assert!(docx.outcomes.kept >= 1, "{docx:?}");
    // A tiny stream is not kept (no gain), and its file goes through the normal path whole.
    let tiny = files.iter().find(|f| f.path == "tiny.zip").unwrap();
    assert_eq!(tiny.counts.reconstructed, 1, "{tiny:?}");
    assert_eq!((tiny.outcomes.no_gain, tiny.outcomes.kept), (1, 0));
    assert!(d.none_kept >= 1);
    // readme.txt is text: never walked.
    assert!(files.iter().all(|f| f.path != "readme.txt"));
    // The three IDAT payloads are one stream.
    let png = files.iter().find(|f| f.path == "image.png").unwrap();
    assert_eq!((png.kind, png.counts.reconstructed), ("png", 1), "{png:?}");
    assert!(d.model_bytes < d.stream_bytes && d.record_bytes > 0);
    assert_eq!(d.lzma_blocks + d.zstd_blocks, d.outcomes.kept);
    // The part blocks are not counted in the tier's summary.
    let b = s.balanced.unwrap();
    assert_eq!(b.peel.deflate, s.peel.deflate);
}

#[test]
fn the_file_gate_sends_small_gains_through_the_normal_path() {
    // Stored-block gzip members of repetitive text: the gain is the stream's length minus a
    // small model output, the record and the overhead; across the sizes some files gain more
    // than the file gate's 256 bytes and some less.
    let dir = tempfile::tempdir().unwrap();
    for len in (300..900).step_by(20) {
        let text = b"abcabcabd ".repeat(len / 10);
        std::fs::write(dir.path().join(format!("s{len}.gz")), gzip(&text, 0)).unwrap();
    }
    let (bytes, s) = run(balanced(1), dir.path());
    let d = &s.peel.deflate;
    assert!(d.file_gate >= 1 && d.outcomes.file_gate >= 1, "{d:?}");
    assert!(d.outcomes.kept >= 1, "{d:?}");
    assert_eq!(d.outcomes.total(), d.streams.reconstructed);
    check(&bytes, dir.path(), None);
}

#[test]
fn equal_streams_and_whole_duplicates_are_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let member = prose(71, 120_000);
    // The same member, deflated the same way, in two containers with different framing.
    std::fs::write(
        dir.path().join("a.zip"),
        zip_of(&[("same.xml", member.clone(), true)]),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.zip"),
        zip_of(&[
            ("other.txt", prose(72, 3_000), false),
            ("same.xml", member, true),
        ]),
    )
    .unwrap();
    std::fs::write(dir.path().join("c.zip"), fixture("doc.zip")).unwrap();
    std::fs::write(dir.path().join("d.zip"), fixture("doc.zip")).unwrap();
    let (bytes, s) = run(balanced(1), dir.path());
    let d = &s.peel.deflate;
    assert_eq!(d.deduplicated.files, 1, "{d:?}");
    assert!(d.outcomes.already_stored >= 1, "{d:?}");
    let b = d.files.as_ref().unwrap().iter().find(|f| f.path == "b.zip");
    assert_eq!(b.unwrap().outcomes.already_stored, 1);
    check(&bytes, dir.path(), None);
}

#[test]
fn the_header_minor_follows_the_stages() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("n.gz"), gzip(&repetitive(81, 200_000), 6)).unwrap();
    let minor = |p: Pipeline| {
        let (bytes, s) = run(p, dir.path());
        (open(&bytes, None).header().version.minor, s)
    };
    assert_eq!(minor(balanced(1)).0, 2);
    let mut b = balanced(1);
    b.deflate = None;
    assert_eq!(minor(b).0, 1);
    let fast = || {
        Pipeline::fast(FastOptions {
            threads: Some(1),
            ..FastOptions::default()
        })
    };
    let (m, s) = minor(fast());
    assert_eq!((m, s.peel.deflate.outcomes.kept), (1, 0));
    let mut f = fast();
    f.deflate = Some(DeflatePeel::default());
    let (m, s) = minor(f);
    assert_eq!((m, s.peel.deflate.outcomes.kept), (2, 1));
    let mut f = fast();
    f.deflate = Some(DeflatePeel::default());
    let (bytes, _) = run(f, dir.path());
    check(&bytes, dir.path(), None);
    assert_eq!(peeled_blocks(&bytes), 1);
}

#[test]
fn one_thread_eight_threads_and_a_one_block_budget_write_the_same_bytes() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let (one, s1) = run(balanced(1), dir.path());
    let (eight, s8) = run(balanced(8), dir.path());
    assert!(one == eight, "1 and 8 threads differ");
    assert_eq!(s1.peel, s8.peel);
    assert!(s8.blocks_in_flight > 1);
    let mut narrow = balanced(8);
    narrow.seal.memory = Some(s8.memory.minimum());
    let (n, sn) = run(narrow, dir.path());
    assert_eq!(sn.blocks_in_flight, 1);
    assert!(n == one, "the budget changed the bytes");
}

#[test]
fn the_part_blocks_declare_the_memory_term() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let (bytes, _) = run(balanced(1), dir.path());
    let a = open(&bytes, None);
    let e = a.index().envelope;
    // The largest kept text: the docx's document.xml (200000 bytes) or data.gz (300000 bytes).
    let term = lpk_core::peel::deflate::memory_term(300_000, 0);
    assert!(e.decode_memory >= e.max_block_plain + term, "{e:?}");
    assert!(e.threads_hint >= 1);
}

#[test]
fn sealed_archives_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    make_tree(dir.path());
    let credentials = Credentials::password("correct horse battery staple");
    let mut p = balanced(1);
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
    let (bytes, s) = run(p, dir.path());
    assert!(s.peel.deflate.outcomes.kept >= 7);
    check(&bytes, dir.path(), Some(&credentials));
}

#[test]
fn an_append_to_a_revision_1_1_archive_runs_without_the_deflate_stage() {
    let v1 = tempfile::tempdir().unwrap();
    std::fs::write(v1.path().join("readme.txt"), fixture("readme.txt")).unwrap();
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("a.lpk");
    // Revision 1.1: Fast with the JPEG peel only.
    let s0 = Pipeline::fast(FastOptions::default())
        .run_file(v1.path(), &archive)
        .unwrap();
    assert!(s0.peel.dropped_stages.is_empty());
    let v2 = tempfile::tempdir().unwrap();
    make_tree(v2.path());
    let s1 = balanced(1).append_file(v2.path(), &archive, None).unwrap();
    assert_eq!(s1.peel.dropped_stages, ["deflate"]);
    assert_eq!(s1.peel.deflate.outcomes.kept, 0);
    let bytes = std::fs::read(&archive).unwrap();
    assert_eq!(open(&bytes, None).header().version.minor, 1);
    check(&bytes, v2.path(), None);
    assert_eq!(peeled_blocks(&bytes), 0);
    // A revision 1.2 archive keeps the stage in its appends.
    let archive2 = work.path().join("b.lpk");
    balanced(1).run_file(v1.path(), &archive2).unwrap();
    let s2 = balanced(1).append_file(v2.path(), &archive2, None).unwrap();
    assert!(s2.peel.dropped_stages.is_empty());
    assert!(s2.peel.deflate.outcomes.kept >= 7);
    check(&std::fs::read(&archive2).unwrap(), v2.path(), None);
}
