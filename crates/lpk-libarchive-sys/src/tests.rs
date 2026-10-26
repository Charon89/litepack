use super::*;
use std::io::{Seek, SeekFrom, Write};

/// A one-member Unix `ar` archive written by hand.
fn ar_archive(name: &str, data: &[u8]) -> Vec<u8> {
    let mut v = b"!<arch>\n".to_vec();
    let header = format!(
        "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        format!("{name}/"),
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
        assert!(e.message.contains("vendor/libarchive-3.8.9"), "{e}");
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
