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

use lpk_core::{archive_fast, register_full_reader, Cause, FastOptions, JpegPeel, PeelStage};
use lpk_format::{Archive, EntryKind, FormatError, Resources};

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
    // Baseline, trailing, secondary and progressive peel; CMYK and truncated fall back.
    assert_eq!(p.peeled.files, 4, "{p:?}");
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
    assert_eq!(tight.peel(&base, max).unwrap_err(), Cause::DimensionCap);
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
