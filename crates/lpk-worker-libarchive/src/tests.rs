use super::*;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn open_fixture(name: &str, wire: &str) -> Result<Box<dyn ForeignReader>, ReaderError> {
    let f = File::open(fixture(name)).unwrap();
    open(
        f,
        &[],
        &OpenArgs {
            format: wire,
            name: name.as_bytes(),
            password: None,
        },
    )
}

#[test]
fn each_rar_gap_is_a_named_unsupported_error() {
    // The pre-check runs before libarchive, so these hold with or without the library.
    for (name, words) in [
        ("gap-rar4-solid.rar", "solid RAR 4"),
        ("gap-rar4-encrypted.rar", "encrypted RAR 4 member"),
        ("gap-rar4-encrypted-headers.rar", "encrypted RAR 4 headers"),
        ("gap-rar20.rar", "RAR 1.5/2.x"),
        ("gap-rar5-encrypted.rar", "encrypted RAR 5 member"),
        ("gap-rar5-encrypted-headers.rar", "encrypted RAR 5 headers"),
    ] {
        let e = open_fixture(name, "rar").err().unwrap();
        assert_eq!(e.code, ErrorCode::Unsupported, "{name}: {e:?}");
        assert!(e.message.contains(words), "{name}: {}", e.message);
        assert!(e.message.contains("named gap"), "{name}: {}", e.message);
    }
}

#[test]
fn readable_rar_fixtures_pass_the_pre_check() {
    for name in [
        "rar5.rar",
        "rar4.rar",
        "rar5-vol.part1.rar",
        "rar4-vol.rar",
        "rar5-sfx.sfx",
    ] {
        let mut f = File::open(fixture(name)).unwrap();
        assert_eq!(rar::named_gap(&mut f).unwrap(), None, "{name}");
    }
}

/// A RAR5 file header body (after the CRC and size) with compression info `comp` and an
/// optional encryption record.
fn rar5_file_header(comp: u64, encrypted: bool) -> (Vec<u8>, usize, u64) {
    let extra: Vec<u8> = if encrypted { vec![2, 1, 0] } else { Vec::new() };
    let mut h = vec![2u8]; // type: file
    h.push(if extra.is_empty() { 0 } else { 1 }); // header flags
    if !extra.is_empty() {
        h.push(extra.len() as u8);
    }
    let fields_at = h.len();
    h.push(0); // file flags
    h.push(5); // unpacked size
    h.push(0x20); // attributes
    let mut c = comp;
    loop {
        let b = (c & 0x7F) as u8;
        c >>= 7;
        if c == 0 {
            h.push(b);
            break;
        }
        h.push(b | 0x80);
    }
    h.push(0); // host OS
    h.push(1); // name length
    h.push(b'a');
    h.extend_from_slice(&extra);
    (h, fields_at, extra.len() as u64)
}

#[test]
fn a_rar5_dictionary_above_64_mib_is_a_named_gap() {
    let method = 3u64 << 7;
    let (h, at, extra) = rar5_file_header(method | (9 << 10), false);
    assert_eq!(rar::named_gap_of_file_header(&h, at, extra), None);
    let (h, at, extra) = rar5_file_header(method | (10 << 10), false);
    let g = rar::named_gap_of_file_header(&h, at, extra).unwrap();
    assert!(g.contains("128 MiB"), "{g}");
    // A stored member's dictionary field does not matter.
    let (h, at, extra) = rar5_file_header(15 << 10, false);
    assert_eq!(rar::named_gap_of_file_header(&h, at, extra), None);
    let (h, at, extra) = rar5_file_header(method, true);
    let g = rar::named_gap_of_file_header(&h, at, extra).unwrap();
    assert!(g.contains("encrypted RAR 5 member"), "{g}");
}

#[test]
fn an_unknown_wire_format_is_refused() {
    let e = open_fixture("rar5.rar", "zip").err().unwrap();
    assert_eq!(e.code, ErrorCode::Unsupported);
}

#[test]
fn without_the_library_every_open_names_its_absence() {
    if lpk_libarchive_sys::BUILT {
        return;
    }
    let e = open_fixture("cab-mszip.cab", "cab").err().unwrap();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert!(e.message.contains("not built"), "{}", e.message);
}

#[test]
fn volumes_are_split_off_in_role_order() {
    let t = || tempfile::tempfile().unwrap();
    let handles = vec![
        (roles::ARCHIVE + 2, t()),
        (roles::REQUESTS, t()),
        (roles::ARCHIVE, t()),
        (roles::ARCHIVE + 1, t()),
        (roles::RESPONSES, t()),
    ];
    let (base, more) = split_volumes(handles);
    let mut roles_left: Vec<u32> = base.iter().map(|(r, _)| *r).collect();
    roles_left.sort_unstable();
    assert_eq!(
        roles_left,
        vec![roles::REQUESTS, roles::RESPONSES, roles::ARCHIVE]
    );
    assert_eq!(more.len(), 2);
}

#[test]
fn single_streams_are_named_from_the_archive() {
    assert_eq!(stream_name(b"hello.txt.Z", "z"), b"hello.txt");
    assert_eq!(stream_name(b"x.taz", "z"), b"x.tar");
    assert_eq!(stream_name(b"hello.txt.uue", "uue"), b"hello.txt");
    assert_eq!(stream_name(b"", "z"), b"data");
    assert_eq!(stream_name(b"odd", "z"), b"odd.out");
}

#[test]
fn libarchive_messages_map_to_named_codes() {
    let e = |m: &str| lpk_libarchive_sys::Error {
        kind: LaErrorKind::Fatal,
        errno: 0,
        message: m.into(),
    };
    assert_eq!(
        map_err("rar", e("Encryption is not supported")).code,
        ErrorCode::Unsupported
    );
    assert_eq!(
        map_err("rar", e("Unsupported compression method")).code,
        ErrorCode::Unsupported
    );
    assert_eq!(
        map_err("cab", e("Invalid CFDATA checksum")).code,
        ErrorCode::Corrupt
    );
}
