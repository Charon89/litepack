//! Synthetic inputs of the walker and analysis tests (the same builders as the probe's tests).

use std::io::{Cursor, Write};

/// Deterministic compressible text.
pub fn text(n: usize, seed: u32) -> Vec<u8> {
    let words = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
    ];
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
    let mut out = Vec::new();
    while out.len() < n {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(words[(x >> 24) as usize % words.len()].as_bytes());
        out.push(b' ');
    }
    out.truncate(n);
    out
}

/// Deterministic pseudo-random bytes.
pub fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(data).expect("write");
    e.finish().expect("finish")
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(data).expect("write");
    e.finish().expect("finish")
}

pub fn raw_deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(data).expect("write");
    e.finish().expect("finish")
}

/// A ZIP with one deflated member per name (text of `len` bytes) and a stored member.
pub fn zip_with(names: &[&str], len: usize) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (i, n) in names.iter().enumerate() {
        w.start_file(*n, deflated).expect("start");
        w.write_all(&text(len, i as u32 + 1)).expect("write");
    }
    w.start_file("raw.bin", stored).expect("start");
    w.write_all(b"stored entry").expect("write");
    w.finish().expect("finish").into_inner()
}

/// A PNG built by hand, its zlib stream split over three `IDAT` chunks with a `tEXt` between.
pub fn multi_idat_png() -> Vec<u8> {
    let (w, h) = (48u32, 32u32);
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0);
        for x in 0..w * 3 {
            raw.push(((x * 7 + y * 13) % 251) as u8);
        }
    }
    let z = zlib(&raw);
    let chunk = |out: &mut Vec<u8>, ty: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(ty);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0; 4]);
    };
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    let third = z.len() / 3;
    chunk(&mut out, b"IDAT", &z[..third]);
    chunk(&mut out, b"tEXt", b"Comment\0between");
    chunk(&mut out, b"IDAT", &z[third..2 * third]);
    chunk(&mut out, b"IDAT", &z[2 * third..]);
    chunk(&mut out, b"IEND", &[]);
    out
}

pub fn pdf_bytes(stream: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n1 0 obj\n<< /Filter /FlateDecode >>\nstream\n".to_vec();
    out.extend_from_slice(stream);
    out.extend_from_slice(b"\nendstream\nendobj\n2 0 obj\n<< >>\nstream\nplain text\nendstream\n");
    out
}

/// A binary with an embedded zlib stream between noise and a tail.
pub fn binary_with_zlib() -> Vec<u8> {
    let mut bin = b"MZ\x90\x00binary head".to_vec();
    bin.extend_from_slice(&noise(4000, 7));
    bin.extend_from_slice(&zlib(&text(30_000, 21)));
    bin.extend_from_slice(b"binary tail");
    bin
}

/// One synthetic file of every kind the walker tells apart, with an extension.
pub fn every_kind() -> Vec<(&'static str, Vec<u8>)> {
    let two_gzip = [gzip(&text(9000, 22)), gzip(&text(5000, 9))].concat();
    vec![
        ("zip", zip_with(&["a.txt", "b.txt"], 6000)),
        (
            "docx",
            zip_with(&["[Content_Types].xml", "word/document.xml"], 4000),
        ),
        ("png", multi_idat_png()),
        ("gz", two_gzip),
        ("pdf", pdf_bytes(&zlib(&text(20_000, 23)))),
        ("bin", binary_with_zlib()),
    ]
}
