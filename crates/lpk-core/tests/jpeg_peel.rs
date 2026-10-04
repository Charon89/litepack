//! The JPEG peel (E2-5a) on small JPEGs: round trip through the Fast pipeline and the full
//! reader, nested trailing data and a secondary image, and the fallbacks with their causes.
//!
//! Fixtures (`tests/fixtures/`): `baseline.jpg` (256x192, quality 90), `progressive.jpg` (the
//! same picture, progressive) and `secondary.jpg` (64x48, quality 80) were generated once from
//! synthetic gradients by this repository's in-tree baseline/progressive encoder
//! (`crates/lpk-bench/src/corpus/derive/jpegenc.rs`); no third-party image is involved. They are
//! dedicated to the public domain (CC0 1.0). The CMYK, MPF and truncated variants are derived
//! from them in the tests.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_core::peel::LEPTON_MAX_FILE;
use lpk_core::{
    archive_fast, register_full_reader, Cause, Fallback, FastOptions, JpegPeel, PeelStage,
};
use lpk_format::{
    Archive, Encoded, EntryFlags, EntryKind, FormatError, Graph, GraphResources, JpegRecord,
    Record, RecordBody, Resources, Step, Utf16Record, Writer, WriterOptions,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// The baseline fixture with an APP2 `MPF\0` segment after SOI, the secondary fixture after its
/// EOI, and a few trailing bytes after that.
fn with_secondary() -> Vec<u8> {
    let base = fixture("baseline.jpg");
    let mut v = base[..2].to_vec();
    v.extend_from_slice(&[0xFF, 0xE2, 0x00, 0x0A]);
    v.extend_from_slice(b"MPF\0\0\0\0\0");
    v.extend_from_slice(&base[2..]);
    v.extend_from_slice(b"gap");
    v.extend_from_slice(&fixture("secondary.jpg"));
    v.extend_from_slice(b"tail bytes");
    v
}

/// The baseline fixture with its frame header rewritten to declare four components.
fn cmyk() -> Vec<u8> {
    let base = fixture("baseline.jpg");
    let at = base.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    let len = usize::from(base[at + 2]) << 8 | usize::from(base[at + 3]);
    let mut sof = base[at + 4..at + 2 + len].to_vec();
    sof[5] = 4;
    sof.extend_from_slice(&[4, 0x11, 0]);
    let mut v = base[..at + 2].to_vec();
    v.extend_from_slice(&((len + 3) as u16).to_be_bytes());
    v.extend_from_slice(&sof);
    v.extend_from_slice(&base[at + 2 + len..]);
    v
}

fn tree(dir: &Path) {
    let base = fixture("baseline.jpg");
    std::fs::write(dir.join("a-baseline.jpg"), &base).unwrap();
    let mut t = base.clone();
    t.extend_from_slice(b"some trailing bytes after the EOI");
    std::fs::write(dir.join("b-trailing.jpg"), t).unwrap();
    std::fs::write(dir.join("c-secondary.jpg"), with_secondary()).unwrap();
    std::fs::write(dir.join("d-cmyk.jpg"), cmyk()).unwrap();
    std::fs::write(dir.join("e-truncated.jpg"), &base[..base.len() / 2]).unwrap();
    std::fs::write(dir.join("f-progressive.jpg"), fixture("progressive.jpg")).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a picture\n".repeat(50)).unwrap();
}

fn extract_all(bytes: Vec<u8>, full: bool) -> Result<Vec<(String, Vec<u8>)>, FormatError> {
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default())?;
    if full {
        register_full_reader(&mut a);
    }
    let t = a.entry_table()?;
    let entries: Vec<_> = t.table()?.iter().collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
        let mut v = Vec::new();
        a.extract(e, &mut v)?;
        out.push((e.path.clone(), v));
    }
    a.verify()?;
    Ok(out)
}

#[test]
fn the_fast_pipeline_peels_jpegs_and_the_full_reader_restores_them() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let mut out = Vec::new();
    let (_, fast) = archive_fast(dir.path(), &mut out, FastOptions::default()).unwrap();
    let p = fast.peel;
    // Baseline, secondary and progressive peel; the trailing file's primary image is the
    // baseline's, so it deduplicates (counted apart); CMYK and truncated fall back.
    assert_eq!(p.peeled.files, 3, "{p:?}");
    assert_eq!(p.deduplicated.files, 1, "{p:?}");
    assert_eq!(p.fallback(Cause::FourComponents).files, 1, "{p:?}");
    assert_eq!(p.fallback(Cause::NoEoi).files, 1, "{p:?}");
    assert_eq!(p.fallback_total().files, 2);
    assert!(p.peeled_output_bytes < p.peeled.bytes);
    let a = Archive::open(Cursor::new(out.clone()), &Resources::default()).unwrap();
    assert_eq!(a.header().version.minor, 1);
    let files = extract_all(out.clone(), true).unwrap();
    assert_eq!(files.len(), 7);
    for (path, bytes) in files {
        assert!(
            bytes == std::fs::read(dir.path().join(&path)).unwrap(),
            "{path}"
        );
    }
    // The 1.0 reader lists the archive but cannot rebuild a peeled file.
    assert!(matches!(
        extract_all(out, false),
        Err(FormatError::UnimplementedPrimitive { id: 7 })
    ));
}

#[test]
fn a_secondary_image_and_the_trailing_data_become_nested_parts() {
    let data = with_secondary();
    let plan = JpegPeel::default().peel(&data, 1 << 26).unwrap();
    let kinds: Vec<bool> = plan.nested.iter().map(|n| n.secondary).collect();
    assert_eq!(kinds, [false, true, false]);
    let covered: u64 = plan.nested.iter().map(|n| n.len).sum();
    assert_eq!(plan.primary_len + covered, data.len() as u64);
    assert_eq!(plan.nested[1].len, fixture("secondary.jpg").len() as u64);
    // Plain trailing data without an MPF or gain-map marker is one trailing part.
    let mut t = fixture("baseline.jpg");
    let n = t.len() as u64;
    t.extend_from_slice(&fixture("secondary.jpg"));
    let plan = JpegPeel::default().peel(&t, 1 << 26).unwrap();
    assert_eq!(plan.nested.len(), 1);
    assert!(!plan.nested[0].secondary);
    assert_eq!(plan.primary_len, n);
}

#[test]
fn fallbacks_carry_the_probe_causes() {
    let peel = JpegPeel::default();
    let max = 1 << 26;
    assert_eq!(peel.peel(&cmyk(), max).unwrap_err(), Cause::FourComponents);
    let base = fixture("baseline.jpg");
    assert_eq!(
        peel.peel(&base[..base.len() / 2], max).unwrap_err(),
        Cause::NoEoi
    );
    let no_progressive = JpegPeel {
        progressive: false,
        ..JpegPeel::default()
    };
    assert_eq!(
        no_progressive
            .peel(&fixture("progressive.jpg"), max)
            .unwrap_err(),
        Cause::Progressive
    );
    // A primary larger than one block, and one whose decoder memory exceeds the limit.
    assert_eq!(peel.peel(&base, 1000).unwrap_err(), Cause::SizeCap);
    let tight = JpegPeel {
        memory_limit: 1 << 20,
        ..JpegPeel::default()
    };
    assert_eq!(tight.peel(&base, max).unwrap_err(), Cause::MemoryCap);
    assert_ne!(Cause::MemoryCap.label(), Cause::DimensionCap.label());
    // A file longer than `max_file` is refused before it is read; one at the cap is read.
    let capped = JpegPeel {
        max_file: 100,
        ..JpegPeel::default()
    };
    assert_eq!(
        capped.refuse_unread(101),
        Some(Fallback::Jpeg(Cause::SizeCap))
    );
    assert_eq!(capped.refuse_unread(100), None);
    assert_eq!(JpegPeel::default().max_file, LEPTON_MAX_FILE);
}

#[test]
fn a_jpeg_over_the_file_cap_is_streamed_as_a_size_cap_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let base = fixture("baseline.jpg");
    // At the cap: read and peeled. Over it: streamed, never read whole.
    std::fs::write(dir.path().join("small.jpg"), &base).unwrap();
    let mut t = base.clone();
    t.extend_from_slice(&[0u8; 64]);
    std::fs::write(dir.path().join("big.jpg"), &t).unwrap();
    let options = FastOptions {
        jpeg_max_file: base.len() as u64,
        ..FastOptions::default()
    };
    let mut out = Vec::new();
    let (_, fast) = archive_fast(dir.path(), &mut out, options).unwrap();
    let p = fast.peel;
    assert_eq!(p.peeled.files, 1, "{p:?}");
    assert_eq!(
        p.fallback(Cause::SizeCap),
        lpk_core::Count {
            files: 1,
            bytes: t.len() as u64
        }
    );
    let files = extract_all(out, true).unwrap();
    assert!(files.iter().any(|(p, b)| p == "big.jpg" && *b == t));
}

#[test]
fn the_header_declares_revision_1_1_whenever_the_peel_is_enabled() {
    // No JPEG at all: the writer still writes under revision 1.1 (spec section 2).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "text\n".repeat(100)).unwrap();
    let mut out = Vec::new();
    let (_, fast) = archive_fast(dir.path(), &mut out, FastOptions::default()).unwrap();
    assert_eq!(fast.peel.peeled.files, 0);
    let a = Archive::open(Cursor::new(out.clone()), &Resources::default()).unwrap();
    assert_eq!(a.header().version.minor, 1);
    // A 1.0 reader extracts it: no block names a 1.1 primitive.
    let files = extract_all(out, false).unwrap();
    assert_eq!(files.len(), 1);
}

#[test]
fn a_stream_not_smaller_than_the_primary_is_no_gain() {
    // A stream padded to the primary's length after the encode: the gain gate, which runs
    // before the verifying decode, refuses it.
    let base = fixture("baseline.jpg");
    let n = base.len();
    let r = JpegPeel::default().peel_with(&base, 1 << 26, &|s| s.resize(n, 0), &|_| {});
    assert_eq!(r.unwrap_err(), Cause::NoGain);
}

/// `with_secondary()` peeled and written as the pipeline does, with the record changed by
/// `edit` and the block's declared decoder memory `memory` (None: the plan's).
fn peeled_archive(edit: &dyn Fn(Record) -> Record, memory: Option<u64>) -> Vec<u8> {
    let data = with_secondary();
    let peel = JpegPeel::default();
    let plan = peel.peel(&data, 1 << 20).unwrap();
    let mut out = Vec::new();
    let options = WriterOptions {
        chunk_size: 4096,
        ..WriterOptions::default()
    };
    let mut w = Writer::new_revision(&mut out, options, 1).unwrap();
    w.begin_entry("p.jpg", EntryFlags::EMPTY, 0).unwrap();
    let mut lists = Vec::new();
    for p in &plan.nested {
        let (a, b) = (p.offset as usize, (p.offset + p.len) as usize);
        lists.push(w.add_part(p.offset, &mut &data[a..b]).unwrap());
    }
    let id = w.add_record(edit(peel.record(&plan, &lists))).unwrap();
    let mut params = Vec::new();
    lpk_format::varint::write(&mut params, id).unwrap();
    let encoded = Encoded {
        graph: Graph {
            steps: vec![Step {
                primitive: plan.primitive,
                params,
            }],
        },
        bytes: plan.stream.clone(),
        resources: GraphResources::default(),
    };
    let primary = &data[..plan.primary_len as usize];
    w.add_part_encoded(0, primary, encoded, memory.unwrap_or(plan.memory))
        .unwrap();
    w.end_entry().unwrap();
    w.finish().unwrap();
    out
}

fn full_verify(bytes: Vec<u8>) -> Result<(), FormatError> {
    let mut a = Archive::open(Cursor::new(bytes), &Resources::default())?;
    register_full_reader(&mut a);
    a.verify().map(|_| ())
}

fn edit_jpeg(f: impl Fn(&mut JpegRecord)) -> impl Fn(Record) -> Record {
    move |r: Record| {
        let RecordBody::Jpeg(mut j) = r.body else {
            unreachable!()
        };
        f(&mut j);
        Record::new(RecordBody::Jpeg(j))
    }
}

#[test]
fn the_decoder_refuses_a_wrong_record_or_too_little_memory() {
    full_verify(peeled_archive(&|r| r, None)).unwrap();
    let reason = |bytes: Vec<u8>| match full_verify(bytes) {
        Err(FormatError::BadRecord { reason, .. }) => reason,
        other => panic!("{other:?}"),
    };
    let utf16 = |_: Record| {
        Record::new(RecordBody::Utf16(Utf16Record {
            endian: 0,
            bom: 1,
            original_len: 3,
            original_hash: [0; 32],
        }))
    };
    assert_eq!(reason(peeled_archive(&utf16, None)), "kind");
    let v1 = edit_jpeg(|j| j.lepton_version = 1);
    assert_eq!(reason(peeled_archive(&v1, None)), "lepton_version");
    let hash = edit_jpeg(|j| j.original_hash[0] ^= 1);
    assert_eq!(reason(peeled_archive(&hash, None)), "original_hash");
    // A declared decode_memory below the image's term: refused, never decoded.
    match full_verify(peeled_archive(&|r| r, Some(0))) {
        Err(FormatError::Refused(r)) => {
            assert_eq!(r.field, "decode_memory");
            assert!(r.needed > r.allowed);
        }
        other => panic!("{other:?}"),
    }
}

/// Two secondary images (MPF) with trailing data between and after them.
fn with_two_secondaries() -> Vec<u8> {
    let mut v = with_secondary();
    v.extend_from_slice(&fixture("secondary.jpg"));
    v.extend_from_slice(b"end");
    v
}

#[test]
fn two_secondary_images_peel_and_restore() {
    let data = with_two_secondaries();
    let plan = JpegPeel::default().peel(&data, 1 << 26).unwrap();
    let kinds: Vec<bool> = plan.nested.iter().map(|n| n.secondary).collect();
    assert_eq!(kinds, [false, true, false, true, false]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("two.jpg"), &data).unwrap();
    let mut out = Vec::new();
    let (_, fast) = archive_fast(dir.path(), &mut out, FastOptions::default()).unwrap();
    assert_eq!(fast.peel.peeled.files, 1);
    let files = extract_all(out, true).unwrap();
    assert!(files[0].1 == data);
}

#[test]
fn a_corrupted_stream_or_a_mismatch_falls_back() {
    let peel = JpegPeel::default();
    let base = fixture("baseline.jpg");
    // A changed decoded byte: the verification fails.
    let r = peel.peel_with(&base, 1 << 26, &|_| {}, &|d| d[100] ^= 1);
    assert_eq!(r.unwrap_err(), Cause::VerificationMismatch);
    // A damaged stream: whatever the library says, the file is not peeled.
    let r = peel.peel_with(&base, 1 << 26, &|s| s.truncate(s.len() / 2), &|_| {});
    assert!(r.is_err());
    let r = peel.peel_with(
        &base,
        1 << 26,
        &|s| {
            let n = s.len();
            s[n - 40] ^= 0x5A;
        },
        &|_| {},
    );
    assert!(r.is_err());
}
