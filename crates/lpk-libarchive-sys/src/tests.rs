use super::*;
use std::io::{Seek, SeekFrom, Write};

/// A one-member Unix `ar` archive written by hand.
fn ar_archive(name: &str, data: &[u8]) -> Vec<u8> {
    let mut v = b"!<arch>\n".to_vec();
    let mut field = format!("{name}/").into_bytes();
    field.resize(16, b' ');
    let header = format!(
        "{}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        String::from_utf8(field).unwrap(),
        1_700_000_000u64,
        0,
        0,
        "100644",
        data.len()
    );
    assert_eq!(header.len(), 60);
    v.extend_from_slice(header.as_bytes());
    v.extend_from_slice(data);
    if data.len() % 2 == 1 {
        v.push(b'\n');
    }
    v
}

fn temp_with(bytes: &[u8]) -> File {
    let mut f = tempfile::tempfile().unwrap();
    f.write_all(bytes).unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    f
}

fn skip_loudly(what: &str) -> bool {
    if !BUILT {
        eprintln!(
            "SKIPPED {what}: libarchive is not built (vendor/libarchive-{VERSION} absent); \
             this test runs once the sources are vendored"
        );
        return true;
    }
    false
}

#[test]
fn the_stub_or_the_library_answers_an_open() {
    let f = temp_with(&ar_archive("hello.txt", b"hello"));
    let r = Archive::open(OpenSpec {
        volumes: vec![f],
        formats: &[ReadFormat::Ar],
        filters: &[],
        passphrase: None,
    });
    if BUILT {
        assert!(r.is_ok(), "{r:?}");
    } else {
        let e = r.unwrap_err();
        assert_eq!(e.kind, ErrorKind::NotBuilt);
        assert!(e.message.contains("vendor/libarchive-3.8.11"), "{e}");
    }
}

#[test]
fn an_ar_member_reads_back() {
    if skip_loudly("an_ar_member_reads_back") {
        return;
    }
    let f = temp_with(&ar_archive("hello.txt", b"hello, world"));
    let mut a = Archive::open(OpenSpec {
        volumes: vec![f],
        formats: &[ReadFormat::Ar],
        filters: &[],
        passphrase: None,
    })
    .unwrap();
    let e = a.next_entry().unwrap().unwrap();
    assert_eq!(e.path.as_deref(), Some(b"hello.txt".as_slice()));
    assert_eq!(e.kind, EntryKind::File);
    assert_eq!(e.size, Some(12));
    let mut buf = [0u8; 64];
    let mut got = Vec::new();
    loop {
        let n = a.read_data(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, b"hello, world");
    assert!(a.next_entry().unwrap().is_none());
}

#[test]
fn a_reader_not_registered_does_not_read() {
    if skip_loudly("a_reader_not_registered_does_not_read") {
        return;
    }
    // An `ar` archive offered to the cpio reader only: refused, never misread.
    let f = temp_with(&ar_archive("x", b"y"));
    let r = Archive::open(OpenSpec {
        volumes: vec![f],
        formats: &[ReadFormat::Cpio],
        filters: &[],
        passphrase: None,
    });
    assert!(r.is_err());
}

#[test]
fn garbage_is_an_error_not_a_crash() {
    if skip_loudly("garbage_is_an_error_not_a_crash") {
        return;
    }
    let all = [
        ReadFormat::Rar,
        ReadFormat::Rar5,
        ReadFormat::Iso9660,
        ReadFormat::Cab,
        ReadFormat::Lha,
        ReadFormat::Ar,
        ReadFormat::Cpio,
    ];
    for seed in 0u8..16 {
        let bytes: Vec<u8> = (0..4096u32)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect();
        let r = Archive::open(OpenSpec {
            volumes: vec![temp_with(&bytes)],
            formats: &all,
            filters: &[ReadFilter::Gzip, ReadFilter::Xz, ReadFilter::Zstd],
            passphrase: None,
        });
        if let Ok(mut a) = r {
            while let Ok(Some(_)) = a.next_entry() {}
        }
    }
}

/// A newc cpio archive of one empty file named `name` (raw bytes).
fn newc_cpio(name: &[u8]) -> Vec<u8> {
    fn pad4(v: &mut Vec<u8>) {
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
    }
    let mut v = Vec::new();
    for (nm, mode) in [(name, 0o100644u32), (b"TRAILER!!!".as_slice(), 0)] {
        let fields = [
            1,
            mode,
            0,
            0,
            1,
            1_700_000_000u32,
            0,
            0,
            0,
            0,
            0,
            u32::try_from(nm.len() + 1).unwrap(),
            0,
        ];
        v.extend_from_slice(b"070701");
        for f in fields {
            v.extend_from_slice(format!("{f:08X}").as_bytes());
        }
        v.extend_from_slice(nm);
        v.push(0);
        pad4(&mut v);
    }
    v
}

fn first_name(bytes: &[u8], format: ReadFormat) -> Option<Vec<u8>> {
    let _ = use_utf8_locale();
    let mut a = Archive::open(OpenSpec {
        volumes: vec![temp_with(bytes)],
        formats: &[format],
        filters: &[],
        passphrase: None,
    })
    .unwrap();
    a.next_entry().unwrap().unwrap().path
}

/// R204: a name's bytes do not depend on the code page or the locale.
#[test]
fn utf8_names_are_exact_and_other_bytes_are_left_for_the_host_to_refuse() {
    if skip_loudly("utf8_names_are_exact") {
        return;
    }
    let cafe = "caf\u{e9}-\u{65e5}\u{672c}.txt";
    assert_eq!(
        first_name(&newc_cpio(cafe.as_bytes()), ReadFormat::Cpio).as_deref(),
        Some(cafe.as_bytes())
    );
    assert_eq!(
        first_name(&ar_archive(cafe, b"x"), ReadFormat::Ar).as_deref(),
        Some(cafe.as_bytes())
    );
    // A Latin-1 0xE9: whatever comes back is not valid UTF-8 (the host refuses it), never a silent guess.
    let latin1 = first_name(&newc_cpio(b"caf\xe9.txt"), ReadFormat::Cpio);
    assert!(
        latin1
            .as_deref()
            .is_none_or(|n| std::str::from_utf8(n).is_err()),
        "{latin1:?}"
    );
}

/// A ustar archive of one empty regular file named `name` (raw bytes).
fn ustar(name: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len()].copy_from_slice(name);
    h[100..107].copy_from_slice(b"0000644");
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    h[124..135].copy_from_slice(b"00000000000");
    h[136..147].copy_from_slice(b"14000000000");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    h[155] = b' ';
    h.extend(vec![0u8; 1024]);
    h
}

/// R204 on a reader with an undeclared charset: UTF-8 bytes are the name; Latin-1 bytes are refused,
/// never converted through a code page.
#[test]
fn tar_names_are_utf8_or_refused() {
    if skip_loudly("tar_names_are_utf8_or_refused") {
        return;
    }
    let cafe = "caf\u{e9}\u{65e5}.txt";
    assert_eq!(
        first_name(&ustar(cafe.as_bytes()), ReadFormat::Tar).as_deref(),
        Some(cafe.as_bytes())
    );
    let latin1 = {
        let _ = use_utf8_locale();
        let mut a = Archive::open(OpenSpec {
            volumes: vec![temp_with(&ustar(b"caf\xe9.txt"))],
            formats: &[ReadFormat::Tar],
            filters: &[],
            passphrase: None,
        })
        .unwrap();
        a.next_entry().map(|e| e.and_then(|e| e.path))
    };
    assert!(
        matches!(&latin1, Ok(None) | Err(_)),
        "a Latin-1 name must not come back as a name: {latin1:?}"
    );
}
