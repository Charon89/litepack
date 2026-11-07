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

/// R205: the two RAR 4 gaps libarchive reports only through its messages are named Unsupported errors.
#[test]
fn rar4_filter_messages_are_named_gaps() {
    let e = |m: &str| lpk_libarchive_sys::Error {
        kind: LaErrorKind::Fatal,
        errno: 0,
        message: m.into(),
    };
    let ppmd = map_err("rar", e(crate::RAR4_PPMD_FILTER));
    assert_eq!(ppmd.code, ErrorCode::Unsupported);
    assert!(
        ppmd.message.contains("RAR 4 filters inside PPMd"),
        "{}",
        ppmd.message
    );
    let vm = map_err("rar", e(crate::RAR4_VM_FILTER));
    assert_eq!(vm.code, ErrorCode::Unsupported);
    assert!(
        vm.message.contains("RAR 4 VM program filter"),
        "{}",
        vm.message
    );
}

/// A libarchive update that rewords a message the mapping relies on fails here.
#[test]
fn the_messages_the_mapping_relies_on_occur_verbatim_in_the_vendored_source() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/libarchive-3.8.11/libarchive");
    let mut all = String::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|x| x == "c") {
            all.push_str(&String::from_utf8_lossy(&std::fs::read(&p).unwrap()));
        }
    }
    for m in [crate::RAR4_PPMD_FILTER, crate::RAR4_VM_FILTER] {
        assert!(
            all.contains(&format!("\"{m}\"")),
            "{m} is not in the vendored libarchive"
        );
    }
}

fn crc32_ieee(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// A RAR 4 signature and a valid main header (HEAD_CRC over type, flags, size and the 6 reserved bytes).
fn rar4_prefix() -> Vec<u8> {
    let mut v = b"Rar!\x1a\x07\x00".to_vec();
    let mut body = vec![0x73u8, 0, 0, 13, 0];
    body.extend_from_slice(&[0u8; 6]);
    let crc = (crc32_ieee(&body) & 0xFFFF) as u16;
    v.extend_from_slice(&crc.to_le_bytes());
    v.extend_from_slice(&body);
    v
}

fn gap_of(bytes: &[u8]) -> Option<String> {
    let mut t = tempfile::tempfile().unwrap();
    std::io::Write::write_all(&mut t, bytes).unwrap();
    rar::named_gap(&mut t).unwrap()
}

/// B6 (R209): a HIGH_PACK_SIZE near 2^64 does not overflow the walker; the file goes on to libarchive.
#[test]
fn a_pack_size_near_2_pow_64_does_not_overflow() {
    let mut v = rar4_prefix();
    // A file header (type 0x74) with the large-size flag: PACK_SIZE 0xFFFFFFFF, and 0xFFFFFFFF at 32.
    let mut h = vec![0u8; 40];
    h[2] = 0x74;
    h[3..5].copy_from_slice(&0x8100u16.to_le_bytes());
    h[5..7].copy_from_slice(&40u16.to_le_bytes());
    h[7..11].copy_from_slice(&u32::MAX.to_le_bytes());
    h[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
    h[24] = 29;
    h[25] = 0x33;
    v.extend_from_slice(&h);
    assert_eq!(gap_of(&v), None);
}

/// B5 (R208): one window for the host and the worker, the header CRC checked.
#[test]
fn the_sfx_window_is_libarchives_and_a_bare_signature_is_no_archive() {
    let rar = rar4_prefix();
    let stub = |at: usize, bare_at: Option<usize>| {
        let mut v = vec![0u8; at];
        v[0] = b'M';
        v[1] = b'Z';
        if let Some(b) = bare_at {
            v[b..b + 8].copy_from_slice(b"Rar!\x1a\x07\x00\x00");
        }
        v.extend_from_slice(&rar);
        v
    };
    // Aligned inside the window: libarchive reads it, no gap.
    assert_eq!(gap_of(&stub(0x10000, None)), None);
    // Unaligned, and beyond 512 KiB: a named gap, not a generic corruption.
    for at in [0x10008usize, 0x90000, 0x80010] {
        let g = gap_of(&stub(at, None)).unwrap_or_default();
        assert!(
            g.contains("beyond libarchive's search window"),
            "{at:#x}: {g}"
        );
    }
    // A bare signature (bad CRC) earlier in the stub is skipped: no false gap.
    assert_eq!(gap_of(&stub(0x10000, Some(0x2000))), None);
}
