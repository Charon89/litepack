//! E4-3's readers over committed 7-Zip fixtures (`tests/fixtures/`, README there) and over
//! archives the tests write with the `zip`, `tar` and `sevenz-rust2` writers and the stream
//! encoders: every member's bytes equal (BLAKE3) the known content, packed ranges are present
//! and apart, credentials map to their codes, damaged archives are refused without a panic.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use lpk_foreign::open;
use lpk_proto::frame::{FLAG_ENCRYPTED, NO_BLOCK};
use lpk_proto::{ErrorCode, OpenArgs, ReaderError, Record, RecordKind};

const PW: &[u8] = b"LitePack";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The fixture content (as `README.md` describes; `data.bin` from the same LCG).
fn content() -> BTreeMap<&'static str, Vec<u8>> {
    let mut x: u32 = 12345;
    let data: Vec<u8> = (0..65536)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345) & 0x7fff_ffff;
            (x >> 16) as u8
        })
        .collect();
    let text: Vec<u8> = (0..5000)
        .flat_map(|i| format!("line {i} of the LitePack fixture text\n").into_bytes())
        .collect();
    BTreeMap::from([
        ("hello.txt", b"hello world\n".to_vec()),
        ("dir/data.bin", data),
        ("dir/empty.txt", Vec::new()),
        ("dir/text.txt", text),
    ])
}

#[derive(Debug)]
struct Read {
    records: Vec<Record>,
    data: BTreeMap<String, (Vec<u8>, bool)>,
}

fn open_file(
    path: &Path,
    format: &str,
    pw: Option<&[u8]>,
) -> Result<Box<dyn lpk_proto::ForeignReader>, ReaderError> {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let args = OpenArgs {
        format,
        name: name.as_bytes(),
        password: pw,
    };
    open(std::fs::File::open(path).unwrap(), &args)
}

/// Every record (in pages of 2), then every file member streamed in ascending order.
fn read_all(path: &Path, format: &str, pw: Option<&[u8]>) -> Result<Read, ReaderError> {
    let mut r = open_file(path, format, pw)?;
    let mut records = Vec::new();
    loop {
        let (page, more) = r.entries(records.len() as u64, 2)?;
        records.extend(page);
        if !more {
            break;
        }
    }
    let mut data = BTreeMap::new();
    for (i, rec) in records.iter().enumerate() {
        if rec.kind != RecordKind::File {
            continue;
        }
        let mut bytes = Vec::new();
        let checked = r.read_member(i as u64, &mut |d: &[u8]| {
            bytes.extend_from_slice(d);
            Ok(())
        })?;
        assert_eq!(
            bytes.len() as u64,
            rec.size,
            "{}",
            String::from_utf8_lossy(&rec.name)
        );
        data.insert(
            String::from_utf8(rec.name.clone()).unwrap(),
            (bytes, checked),
        );
    }
    Ok(Read { records, data })
}

fn assert_content(got: &Read, what: &str) {
    for (name, want) in content() {
        let (bytes, _) = got
            .data
            .get(name)
            .unwrap_or_else(|| panic!("{what}: {name} missing: {:?}", got.data.keys()));
        assert_eq!(blake3::hash(bytes), blake3::hash(&want), "{what}: {name}");
    }
}

/// Ranges: present for every record (or every non-empty file), inside the file, and blocks
/// apart (members of one block share it).
fn assert_ranges(got: &Read, len: u64, every: bool, what: &str) {
    let mut blocks: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    for (i, r) in got.records.iter().enumerate() {
        let needed = every || (r.kind == RecordKind::File && r.size > 0);
        if r.packed_len == 0 {
            assert!(!needed, "{what}: record {i} has no packed range");
            continue;
        }
        let end = r.packed_offset + r.packed_len;
        assert!(end <= len, "{what}: record {i} past the end");
        let key = if r.block == NO_BLOCK {
            u64::MAX - i as u64
        } else {
            r.block
        };
        let e = blocks.entry(key).or_insert((r.packed_offset, end));
        e.0 = e.0.min(r.packed_offset);
        e.1 = e.1.max(end);
    }
    let v: Vec<_> = blocks.values().copied().collect::<Vec<_>>();
    let mut v = v;
    v.sort_unstable();
    for w in v.windows(2) {
        assert!(w[1].0 >= w[0].1, "{what}: blocks overlap: {w:?}");
    }
}

#[test]
fn the_fixtures_are_the_committed_ones() {
    // `LPK_BLESS_FIXTURES=1` rewrites the manifest (only when a fixture is deliberately added).
    if std::env::var_os("LPK_BLESS_FIXTURES").is_some() {
        let mut names: Vec<String> = std::fs::read_dir(fixtures())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("7zip"))
            .collect();
        names.sort();
        let lines: String = names
            .iter()
            .map(|n| {
                let h = blake3::hash(&std::fs::read(fixtures().join(n)).unwrap());
                format!(
                    "{}  {n}
",
                    h.to_hex()
                )
            })
            .collect();
        std::fs::write(fixtures().join("MANIFEST.blake3"), lines).unwrap();
    }
    let manifest = std::fs::read_to_string(fixtures().join("MANIFEST.blake3")).unwrap();
    let mut n = 0;
    for line in manifest.lines().filter(|l| !l.is_empty()) {
        let (hash, name) = line.split_once("  ").unwrap();
        let bytes = std::fs::read(fixtures().join(name)).unwrap();
        assert_eq!(blake3::hash(&bytes).to_hex().as_str(), hash, "{name}");
        n += 1;
    }
    assert_eq!(n, 17);
}

#[test]
fn seven_zip_zips_round_trip() {
    for (name, pw) in [
        ("7zip-deflate.zip", None),
        ("7zip-deflate64.zip", None),
        ("7zip-bzip2.zip", None),
        ("7zip-lzma.zip", None),
        ("7zip-ppmd.zip", None),
        ("7zip-zipcrypto.zip", Some(PW)),
        ("7zip-aes256.zip", Some(PW)),
    ] {
        let p = fixtures().join(name);
        let got = read_all(&p, "zip", pw).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_content(&got, name);
        assert!(
            got.data.values().all(|(_, checked)| *checked),
            "{name}: CRC"
        );
        assert_ranges(&got, std::fs::metadata(&p).unwrap().len(), true, name);
        let enc = got
            .records
            .iter()
            .filter(|r| r.flags & FLAG_ENCRYPTED != 0)
            .count();
        assert_eq!(enc > 0, pw.is_some(), "{name}: encryption marks");
        assert!(got
            .records
            .iter()
            .any(|r| r.kind == RecordKind::Directory && r.name == b"dir"));
    }
}

#[test]
fn seven_zip_7z_round_trip() {
    for (name, pw) in [
        ("7zip-lzma2-solid.7z", None),
        ("7zip-ppmd.7z", None),
        ("7zip-aes-content.7z", Some(PW)),
        ("7zip-aes-header.7z", Some(PW)),
    ] {
        let p = fixtures().join(name);
        let got = read_all(&p, "7z", pw).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_content(&got, name);
        for (n, (b, checked)) in &got.data {
            assert!(b.is_empty() || *checked, "{name}: {n} CRC");
        }
        assert_ranges(&got, std::fs::metadata(&p).unwrap().len(), false, name);
    }
}

#[test]
fn seven_zip_tar_and_streams_round_trip() {
    for (name, format) in [("7zip.tar", "tar"), ("7zip.tar.bz2", "bz2")] {
        let p = fixtures().join(name);
        let got = read_all(&p, format, None).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_content(&got, name);
        assert_ranges(
            &got,
            std::fs::metadata(&p).unwrap().len(),
            format == "tar",
            name,
        );
    }
    for (name, format) in [("7zip-hello.txt.gz", "gz"), ("7zip-hello.txt.xz", "xz")] {
        let got = read_all(&fixtures().join(name), format, None).unwrap();
        let (b, checked) = &got.data["7zip-hello.txt"];
        assert_eq!(b, b"hello world\n");
        assert!(checked);
    }
}

#[test]
fn credentials_map_to_their_codes() {
    let f = fixtures();
    let code =
        |p: &str, fmt: &str, pw: Option<&[u8]>| read_all(&f.join(p), fmt, pw).err().map(|e| e.code);
    // Header encryption: the password is needed at open.
    assert_eq!(
        code("7zip-aes-header.7z", "7z", None),
        Some(ErrorCode::NeedPassword)
    );
    assert_eq!(
        code("7zip-aes-header.7z", "7z", Some(b"bad")),
        Some(ErrorCode::WrongPassword)
    );
    // Content encryption: listing works, the members need it.
    let r = open_file(&f.join("7zip-aes-content.7z"), "7z", None).unwrap();
    drop(r);
    assert_eq!(
        code("7zip-aes-content.7z", "7z", None),
        Some(ErrorCode::NeedPassword)
    );
    assert_eq!(
        code("7zip-aes-content.7z", "7z", Some(b"bad")),
        Some(ErrorCode::WrongPassword)
    );
    for z in ["7zip-zipcrypto.zip", "7zip-aes256.zip"] {
        assert!(
            open_file(&f.join(z), "zip", None).is_ok(),
            "{z} lists without a password"
        );
        assert_eq!(code(z, "zip", None), Some(ErrorCode::NeedPassword), "{z}");
        assert_eq!(
            code(z, "zip", Some(b"bad")),
            Some(ErrorCode::WrongPassword),
            "{z}"
        );
    }
}

#[test]
fn named_gaps_are_unsupported() {
    let e = read_all(&fixtures().join("7zip-bzip2-gap.7z"), "7z", None)
        .err()
        .unwrap();
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    // Behind an encrypted header, an unsupported coder is still unsupported with the right
    // password, and a wrong password is still a wrong password.
    let p = fixtures().join("7zip-bzip2-gap-aes-header.7z");
    let e = read_all(&p, "7z", Some(PW)).err().unwrap();
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    let e = read_all(&p, "7z", Some(b"bad")).err().unwrap();
    assert_eq!(e.code, ErrorCode::WrongPassword, "{e:?}");
}

fn write_zip(dir: &Path) -> PathBuf {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let c = content();
    w.add_directory("dir/", SimpleFileOptions::default())
        .unwrap();
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    w.start_file("hello.txt", stored).unwrap();
    w.write_all(&c["hello.txt"]).unwrap();
    for n in ["dir/data.bin", "dir/empty.txt", "dir/text.txt"] {
        let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.start_file(n, o).unwrap();
        w.write_all(&c[n]).unwrap();
    }
    let aes = SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "LitePack");
    w.start_file("secret.txt", aes).unwrap();
    w.write_all(b"secret").unwrap();
    let p = dir.join("written.zip");
    std::fs::write(&p, w.finish().unwrap().into_inner()).unwrap();
    p
}

fn tar_bytes() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let c = content();
    let mut dir = tar::Header::new_ustar();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_size(0);
    dir.set_mode(0o755);
    dir.set_mtime(1_700_000_000);
    b.append_data(&mut dir, "dir/", std::io::empty()).unwrap();
    for (n, d) in &c {
        let mut h = tar::Header::new_ustar();
        h.set_size(d.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(1_700_000_000);
        b.append_data(&mut h, n, &d[..]).unwrap();
    }
    let mut l = tar::Header::new_ustar();
    l.set_entry_type(tar::EntryType::Symlink);
    l.set_size(0);
    b.append_link(&mut l, "link", "hello.txt").unwrap();
    let mut h = tar::Header::new_ustar();
    h.set_entry_type(tar::EntryType::Link);
    h.set_size(0);
    b.append_link(&mut h, "hard", "hello.txt").unwrap();
    b.into_inner().unwrap()
}

#[test]
fn written_zip_round_trips() {
    let d = tempfile::tempdir().unwrap();
    let p = write_zip(d.path());
    let got = read_all(&p, "zip", Some(PW)).unwrap();
    assert_content(&got, "written.zip");
    assert_eq!(got.data["secret.txt"].0, b"secret");
    assert_ranges(
        &got,
        std::fs::metadata(&p).unwrap().len(),
        true,
        "written.zip",
    );
    // Without the password the plain members still read; the encrypted one needs it.
    let mut r = open_file(&p, "zip", None).unwrap();
    let (recs, _) = r.entries(0, 100).unwrap();
    let i = recs.iter().position(|r| r.name == b"secret.txt").unwrap();
    assert_ne!(recs[i].flags & FLAG_ENCRYPTED, 0);
    let e = r.read_member(i as u64, &mut |_| Ok(())).err().unwrap();
    assert_eq!(e.code, ErrorCode::NeedPassword);
}

#[test]
fn written_tars_round_trip_plain_and_compressed() {
    let d = tempfile::tempdir().unwrap();
    let tar = tar_bytes();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar).unwrap();
    let gz = gz.finish().unwrap();
    let mut xz =
        lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    xz.write_all(&tar).unwrap();
    let xz = xz.finish().unwrap();
    let mut lz = lzma_rust2::LzipWriter::new(Vec::new(), lzma_rust2::LzipOptions::with_preset(1));
    lz.write_all(&tar).unwrap();
    let lz = lz.finish().unwrap();
    let zst =
        ruzstd::encoding::compress_to_vec(&tar[..], ruzstd::encoding::CompressionLevel::Fastest);
    for (name, format, bytes) in [
        ("w.tar", "tar", tar.clone()),
        ("w.tar.gz", "gz", gz),
        ("w.tar.xz", "xz", xz),
        ("w.tar.lz", "lz", lz),
        ("w.tar.zst", "zst", zst),
    ] {
        let p = d.path().join(name);
        std::fs::write(&p, &bytes).unwrap();
        let r = open_file(&p, format, None).unwrap();
        let want = if format == "tar" {
            "tar".to_string()
        } else {
            format!("tar.{format}")
        };
        assert_eq!(r.format(), want);
        let got = read_all(&p, format, None).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_content(&got, name);
        assert_ranges(&got, bytes.len() as u64, format == "tar", name);
        let link = got.records.iter().find(|r| r.name == b"link").unwrap();
        assert_eq!(
            (link.kind, link.target.as_slice()),
            (RecordKind::Symlink, b"hello.txt".as_slice())
        );
        let hard = got.records.iter().find(|r| r.name == b"hard").unwrap();
        assert_eq!(hard.kind, RecordKind::HardLink);
        assert_eq!(got.records[0].mtime_ns, 1_700_000_000_000_000_000);
    }
}

#[test]
fn single_streams_name_their_member() {
    let d = tempfile::tempdir().unwrap();
    let body = content()["dir/text.txt"].clone();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&body).unwrap();
    // Two gzip members: both are read.
    let mut two = gz.finish().unwrap();
    let mut g2 = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    g2.write_all(b"tail").unwrap();
    two.extend(g2.finish().unwrap());
    let p = d.path().join("notes.txt.gz");
    std::fs::write(&p, &two).unwrap();
    let got = read_all(&p, "gz", None).unwrap();
    let (b, checked) = &got.data["notes.txt"];
    let mut want = body.clone();
    want.extend_from_slice(b"tail");
    assert_eq!(blake3::hash(b), blake3::hash(&want));
    assert!(checked);
    // Two zstd frames with a skippable frame between.
    let mut zst =
        ruzstd::encoding::compress_to_vec(&body[..], ruzstd::encoding::CompressionLevel::Fastest);
    zst.extend_from_slice(&0x184D_2A50u32.to_le_bytes());
    zst.extend_from_slice(&3u32.to_le_bytes());
    zst.extend_from_slice(b"abc");
    zst.extend(ruzstd::encoding::compress_to_vec(
        &b"tail"[..],
        ruzstd::encoding::CompressionLevel::Fastest,
    ));
    let p = d.path().join("notes.zst");
    std::fs::write(&p, &zst).unwrap();
    let got = read_all(&p, "zst", None).unwrap();
    assert_eq!(blake3::hash(&got.data["notes"].0), blake3::hash(&want));
}

#[test]
fn written_7z_round_trips() {
    let d = tempfile::tempdir().unwrap();
    let mut w = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    w.push_archive_entry::<&[u8]>(sevenz_rust2::ArchiveEntry::new_directory("dir"), None)
        .unwrap();
    for (n, data) in content() {
        w.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file(n), Some(&data[..]))
            .unwrap();
    }
    let bytes = w.finish().unwrap().into_inner();
    // Behind a fake self-extractor stub (with a bare signature in it, which has no valid CRC).
    let mut sfx = b"MZ stub \x37\x7A\xBC\xAF\x27\x1C stub".to_vec();
    sfx.extend_from_slice(&bytes);
    for (name, b) in [("w.7z", bytes.clone()), ("w-sfx.exe", sfx)] {
        let p = d.path().join(name);
        std::fs::write(&p, &b).unwrap();
        let got = read_all(&p, "7z", None).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_content(&got, name);
        assert_ranges(&got, b.len() as u64, false, name);
    }
}

#[test]
fn members_in_a_solid_block_read_in_any_order() {
    let p = fixtures().join("7zip-lzma2-solid.7z");
    let mut r = open_file(&p, "7z", None).unwrap();
    let (recs, _) = r.entries(0, 100).unwrap();
    let c = content();
    let files: Vec<usize> = (0..recs.len())
        .filter(|&i| recs[i].kind == RecordKind::File)
        .collect();
    // Backwards: each one restarts the block, and still reads right.
    for &i in files.iter().rev() {
        let mut b = Vec::new();
        r.read_member(i as u64, &mut |d| {
            b.extend_from_slice(d);
            Ok(())
        })
        .unwrap();
        let name = String::from_utf8(recs[i].name.clone()).unwrap();
        assert_eq!(b, c[name.as_str()], "{name}");
    }
}

/// Every prefix cut and a spread of byte flips of each fixture: an error or a reading, never a
/// panic (a panic fails the test).
#[test]
fn damaged_archives_are_refused_without_a_panic() {
    let cases = [
        ("7zip-deflate.zip", "zip", None),
        ("7zip-bzip2.zip", "zip", None),
        ("7zip-aes256.zip", "zip", Some(PW)),
        ("7zip-lzma2-solid.7z", "7z", None),
        ("7zip-aes-header.7z", "7z", Some(PW)),
        ("7zip.tar", "tar", None),
        ("7zip.tar.bz2", "bz2", None),
        ("7zip-hello.txt.gz", "gz", None),
        ("7zip-hello.txt.xz", "xz", None),
    ];
    let d = tempfile::tempdir().unwrap();
    let mut refused = 0;
    for (name, format, pw) in cases {
        let orig = std::fs::read(fixtures().join(name)).unwrap();
        let p = d.path().join(name);
        let mut variants = Vec::new();
        for cut in [
            0,
            1,
            7,
            31,
            100,
            orig.len() / 2,
            orig.len().saturating_sub(23),
            orig.len() - 1,
        ] {
            variants.push(orig[..cut.min(orig.len())].to_vec());
        }
        for k in 0..24 {
            let mut v = orig.clone();
            let at = (k * 7919 + 13) % v.len();
            v[at] ^= 0x5A;
            variants.push(v);
        }
        for v in variants {
            std::fs::write(&p, &v).unwrap();
            if read_all(&p, format, pw).is_err() {
                refused += 1;
            }
        }
    }
    assert!(refused > 0);
}
