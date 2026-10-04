//! Encryption (spec section 14): sealed archives, the key slot, listable mode,
//! tampering, recovery and the committed vectors.
#![allow(clippy::unwrap_used)]

mod common;

use common::sealed::*;
use common::{pattern, write_archive};
use lpk_format::{
    derive_nonce, repair_with_credentials, Archive, Argon2Params, Credentials, FormatError,
    FrameKind, FrameLocation, RecoveryOptions, Resources, Suite, WriterOptions,
};
use std::cell::Cell;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::rc::Rc;

const SEED: u64 = 7;

fn creds(pw: &str) -> Credentials {
    Credentials::password(pw.as_bytes().to_vec())
}

fn build(suite: Suite, listable: bool, keyfile: Option<Vec<u8>>) -> Vec<u8> {
    write_archive_seeded(
        seal_options(suite, listable, "pw", keyfile),
        &sealed_files(),
        SEED,
    )
}

fn open_with(
    bytes: &[u8],
    c: Option<&Credentials>,
) -> Result<Archive<Cursor<Vec<u8>>>, FormatError> {
    Archive::open_with(Cursor::new(bytes.to_vec()), &Resources::default(), c)
}

fn extract_all(a: &mut Archive<Cursor<Vec<u8>>>) -> Vec<(String, Vec<u8>)> {
    let table = a.entry_table().unwrap();
    let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
    entries
        .iter()
        .map(|e| {
            let mut got = Vec::new();
            a.extract(e, &mut got).unwrap();
            (e.path.clone(), got)
        })
        .collect()
}

fn assert_contents(a: &mut Archive<Cursor<Vec<u8>>>) {
    let got = extract_all(a);
    let want = sealed_files();
    assert_eq!(got.len(), want.len());
    for ((p, d), (wp, wd)) in got.iter().zip(&want) {
        assert_eq!(p, wp);
        assert!(d == wd, "{p}");
    }
}

#[test]
fn round_trip_every_combination() {
    for suite in [Suite::AesGcm, Suite::XChaCha] {
        for listable in [false, true] {
            for keyfile in [None, Some(b"keyfile bytes".to_vec())] {
                let bytes = build(suite, listable, keyfile.clone());
                let c = Credentials {
                    password: b"pw".to_vec(),
                    keyfile: keyfile.clone(),
                };
                let mut a = open_with(&bytes, Some(&c)).unwrap();
                assert!(a.is_encrypted());
                assert_eq!(a.is_listable(), listable);
                assert_eq!(a.suite(), Some(suite));
                assert_eq!(a.key_slot().unwrap().keyfile_required, keyfile.is_some());
                assert_contents(&mut a);
                let s = a.verify().unwrap();
                assert!(s.chunks_checked);
                assert!(s.chunks > 0 && s.blocks > 1);
            }
        }
    }
}

#[test]
fn nothing_readable_in_the_clear() {
    let bytes = build(Suite::AesGcm, false, None);
    let plain = &sealed_files()[1].1;
    assert!(!bytes.windows(64).any(|w| w == &plain[..64]));
    assert!(!bytes.windows(7).any(|w| w == b"s/a.txt"));
    let listable = build(Suite::AesGcm, true, None);
    assert!(listable.windows(7).any(|w| w == b"s/a.txt"));
}

#[test]
fn default_argon2_parameters_work() {
    let mut o = seal_options(Suite::AesGcm, false, "pw", None);
    o.seal.as_mut().unwrap().argon2 = Argon2Params::default();
    let bytes = write_archive_seeded(o, &sealed_files(), SEED);
    let mut a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    assert_eq!(a.key_slot().unwrap().argon2, Argon2Params::default());
    assert_contents(&mut a);
}

#[derive(Debug)]
struct CountRead {
    inner: Cursor<Vec<u8>>,
    read: Rc<Cell<u64>>,
}

impl Read for CountRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.read.set(self.read.get() + k as u64);
        Ok(k)
    }
}

impl Seek for CountRead {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn a_wrong_password_fails_at_the_key_slot() {
    let bytes = build(Suite::AesGcm, false, None);
    let read = Rc::new(Cell::new(0));
    let r = CountRead {
        inner: Cursor::new(bytes.clone()),
        read: Rc::clone(&read),
    };
    let e = Archive::open_with(r, &Resources::default(), Some(&creds("nope"))).unwrap_err();
    assert!(matches!(e, FormatError::WrongKey), "{e:?}");
    // The header, the key slot frame (4 + 1 + 111 + 32) and its two kind bytes peeked first; nothing else.
    assert_eq!(read.get(), 32 + 2 + 4 + 1 + 111 + 32);
    assert!(bytes.len() > 20_000);
}

#[test]
fn keyfile_rules() {
    let kf = b"the keyfile".to_vec();
    let bytes = build(Suite::AesGcm, false, Some(kf.clone()));
    let good = Credentials {
        password: b"pw".to_vec(),
        keyfile: Some(kf),
    };
    assert!(open_with(&bytes, Some(&good)).is_ok());
    let wrong = Credentials {
        password: b"pw".to_vec(),
        keyfile: Some(b"other".to_vec()),
    };
    assert!(matches!(
        open_with(&bytes, Some(&wrong)),
        Err(FormatError::WrongKey)
    ));
    assert!(matches!(
        open_with(&bytes, Some(&creds("pw"))),
        Err(FormatError::WrongKey)
    ));
}

#[test]
fn password_required_and_listable_mode() {
    // A sealed archive opens keyless, but nothing in it can be read.
    let sealed = build(Suite::AesGcm, false, None);
    let mut k = open_with(&sealed, None).unwrap();
    assert!(k.is_keyless() && !k.is_listable());
    assert!(matches!(
        k.entry_table(),
        Err(FormatError::PasswordRequired)
    ));
    let bytes = build(Suite::XChaCha, true, None);
    let mut a = open_with(&bytes, None).unwrap();
    assert!(a.is_keyless() && a.is_listable());
    let table = a.entry_table().unwrap();
    let entries: Vec<_> = table.table().unwrap().iter().map(|e| e.unwrap()).collect();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].path, "s/a.txt");
    let mut sink = Vec::new();
    assert!(matches!(
        a.extract(&entries[0], &mut sink),
        Err(FormatError::PasswordRequired)
    ));
    let s = a.verify().unwrap();
    assert!(!s.chunks_checked);
    assert_eq!(s.entries, 3);
    // Opened with the password the same archive extracts.
    let mut a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    assert!(!a.is_keyless());
    assert_contents(&mut a);
}

#[test]
fn memory_bound_on_argon2_is_refused() {
    let bytes = build(Suite::AesGcm, false, None);
    let res = Resources {
        memory: 4 << 20,
        ..Resources::default()
    };
    let e = Archive::open_with(Cursor::new(bytes), &res, Some(&creds("pw"))).unwrap_err();
    match e {
        FormatError::Refused(r) => {
            assert_eq!(r.field, "argon2_m");
            assert_eq!(r.needed, 8192 * 1024);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn nonces_are_unique_across_many_frames() {
    let mut o = seal_options(Suite::XChaCha, false, "pw", None);
    o.recovery = RecoveryOptions {
        percent: 20,
        shard_len: 4096,
        group_shards: 16,
    };
    let files: Vec<(String, Vec<u8>)> = (0..40)
        .map(|i| (format!("f/{i:03}"), pattern(100 + i, 6_000)))
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(p, d)| (p.as_str(), d.clone())).collect();
    let bytes = write_archive_seeded(o, &refs, SEED);
    let mut a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    let key = a
        .key_slot()
        .unwrap()
        .unwrap(&[0x6B; 16], 1, &creds("pw"))
        .unwrap();
    let mut locs: Vec<(u16, FrameLocation)> = a
        .index()
        .blocks
        .iter()
        .map(|b| {
            (
                FrameKind::ChunkData as u16,
                FrameLocation {
                    offset: b.frame_offset,
                    len: b.frame_len,
                    sequence: b.sequence,
                },
            )
        })
        .collect();
    locs.push((FrameKind::EntryTable as u16, a.index().entry_table));
    locs.extend(
        a.recovery_frames()
            .iter()
            .map(|r| (FrameKind::Recovery as u16, *r)),
    );
    assert!(locs.len() > 40);
    let mut seqs: Vec<u64> = locs.iter().map(|(_, l)| l.sequence).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), locs.len());
    let mut nonces = std::collections::HashSet::new();
    for (kind, l) in &locs {
        assert!(nonces.insert(derive_nonce(
            &key,
            &[0x6B; 16],
            *kind,
            l.sequence,
            &a.trailer().salt,
            24
        )));
    }
    assert_eq!(nonces.len(), locs.len());
    assert!(a.verify().unwrap().chunks_checked);
}

/// Position of the payload and the payload length of the frame at `l`.
fn payload_span(l: &FrameLocation) -> (usize, usize) {
    for vl in 1..=4u64 {
        let p = l.len - 4 - vl - 32;
        if lpk_format::varint::len(p) as u64 == vl {
            return ((l.offset + 4 + vl) as usize, p as usize);
        }
    }
    panic!("no payload length");
}

fn block_loc(a: &Archive<Cursor<Vec<u8>>>, i: usize) -> FrameLocation {
    let b = a.index().blocks[i];
    FrameLocation {
        offset: b.frame_offset,
        len: b.frame_len,
        sequence: b.sequence,
    }
}

#[test]
fn a_flipped_ciphertext_byte_fails_its_tag() {
    let bytes = build(Suite::AesGcm, false, None);
    let a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    let loc = block_loc(&a, 1);
    let (start, len) = payload_span(&loc);
    let mut t = bytes.clone();
    t[start + len / 2] ^= 1;
    // A flip without a new frame hash fails the hash first.
    let mut a = open_with(&t, Some(&creds("pw"))).unwrap();
    assert!(matches!(
        a.verify(),
        Err(FormatError::HashMismatch { kind: 2 })
    ));
    // With the frame hash repaired the tag fails.
    let h = blake3::hash(&t[start..start + len]);
    let hash_at = start + len;
    t[hash_at..hash_at + 32].copy_from_slice(h.as_bytes());
    let mut a = open_with(&t, Some(&creds("pw"))).unwrap();
    match a.verify() {
        Err(FormatError::AuthenticationFailed { kind, sequence }) => {
            assert_eq!(kind, 2);
            assert_eq!(sequence, loc.sequence);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn sealing_flag_rules() {
    // A sealed frame in a plain archive.
    let plain = write_archive(WriterOptions::default(), &sealed_files());
    let a = Archive::open(Cursor::new(plain.clone()), &Resources::default()).unwrap();
    let loc = block_loc(&a, 0);
    let mut t = plain.clone();
    t[loc.offset as usize + 2] |= 2;
    let mut a = Archive::open(Cursor::new(t), &Resources::default()).unwrap();
    assert!(matches!(
        a.verify(),
        Err(FormatError::UnexpectedSealedFrame { kind: 2 })
    ));
    // An unsealed block in an encrypted archive.
    let bytes = build(Suite::AesGcm, false, None);
    let a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    let loc = block_loc(&a, 0);
    let mut t = bytes.clone();
    t[loc.offset as usize + 2] &= !2;
    let mut a = open_with(&t, Some(&creds("pw"))).unwrap();
    assert!(matches!(
        a.verify(),
        Err(FormatError::UnsealedFrame { kind: 2 })
    ));
    // The walk applies the same rules without a key.
    let d = Archive::diagnose(Cursor::new(t), &lpk_format::ReadLimits::default());
    assert!(matches!(
        d.error,
        Some(FormatError::UnsealedFrame { kind: 2 })
    ));
}

#[test]
fn key_slot_placement_rules() {
    // No key slot first.
    let bytes = build(Suite::AesGcm, false, None);
    let mut t = bytes.clone();
    t[32..34].copy_from_slice(&2u16.to_le_bytes());
    assert!(matches!(
        open_with(&t, Some(&creds("pw"))),
        Err(FormatError::MissingKeySlot)
    ));
    // A key slot in an archive that is not encrypted.
    let plain = write_archive(WriterOptions::default(), &sealed_files());
    let mut t = plain.clone();
    t[32..34].copy_from_slice(&7u16.to_le_bytes());
    assert!(matches!(
        Archive::open(Cursor::new(t), &Resources::default()),
        Err(FormatError::UnexpectedKeySlot)
    ));
    // A second key slot.
    let listable = build(Suite::AesGcm, true, None);
    let a = open_with(&listable, Some(&creds("pw"))).unwrap();
    let loc = block_loc(&a, 0);
    let mut t = listable.clone();
    t[loc.offset as usize..loc.offset as usize + 2].copy_from_slice(&7u16.to_le_bytes());
    assert!(matches!(
        open_with(&t, None),
        Err(FormatError::UnexpectedKeySlot)
    ));
    let d = Archive::diagnose(Cursor::new(t), &lpk_format::ReadLimits::default());
    assert!(matches!(d.error, Some(FormatError::UnexpectedKeySlot)));
}

#[test]
fn recovery_works_on_an_encrypted_archive() {
    let mut o = seal_options(Suite::AesGcm, false, "pw", None);
    o.recovery = RecoveryOptions {
        percent: 20,
        shard_len: 4096,
        group_shards: 16,
    };
    let bytes = write_archive_seeded(o, &sealed_files(), SEED);
    let a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    assert!(!a.recovery_frames().is_empty());
    let loc = block_loc(&a, 0);
    let mut damaged = bytes.clone();
    for i in 0..300 {
        damaged[loc.offset as usize + 50 + i] ^= 0xFF;
    }
    let mut broken = open_with(&damaged, Some(&creds("pw"))).unwrap();
    assert!(broken.verify().is_err());
    let mut fixed = Cursor::new(Vec::new());
    let c = creds("pw");
    let (report, unrepairable) = repair_with_credentials(
        Cursor::new(damaged),
        &mut fixed,
        &Resources::default(),
        Some(&c),
    )
    .unwrap();
    assert!(unrepairable.is_none());
    assert!(report.shards_repaired >= 1);
    assert!(fixed.get_ref() == &bytes);
    let mut a = open_with(fixed.get_ref(), Some(&creds("pw"))).unwrap();
    assert_contents(&mut a);
}

fn vector(name: &str) -> Vec<u8> {
    std::fs::read(common::vectors_dir().join(name)).unwrap()
}

fn toml_value(name: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(common::vectors_dir().join("vectors.toml")).unwrap();
    let section = text
        .split("\n[")
        .find(|s| s.contains(&format!("\"{name}\"]")))?;
    let line = section
        .lines()
        .find(|l| l.starts_with(&format!("{key} = ")))?;
    Some(line.split('"').nth(1)?.to_string())
}

#[test]
fn the_sealed_vectors_decode_and_are_reproduced() {
    for name in SEALED_VECTORS {
        let pw = toml_value(name, "password").unwrap();
        assert_eq!(pw, sealed_password(name));
        let keyfile = toml_value(name, "keyfile").map(|f| vector(&f));
        let c = Credentials {
            password: pw.into_bytes(),
            keyfile,
        };
        let bytes = vector(name);
        let mut a = open_with(&bytes, Some(&c)).unwrap();
        assert_contents(&mut a);
        assert!(a.verify().unwrap().chunks_checked);
        assert!(matches!(
            open_with(&bytes, Some(&creds("wrong"))),
            Err(FormatError::WrongKey)
        ));
        // Seeded RNG: regeneration is byte for byte.
        assert!(build_sealed_vector(name) == bytes, "{name}");
    }
    let a = open_with(
        &vector("sealed-xchacha.lpk"),
        Some(&creds("battery staple")),
    )
    .unwrap();
    assert_eq!(a.suite(), Some(Suite::XChaCha));
    assert!(open_with(&vector("sealed-listable.lpk"), None).is_ok());
}

fn tool(args: &[&str]) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = lpk_format::cli::run(args.iter().copied(), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn lpk_decode_with_passwords() {
    let dir = common::vectors_dir();
    let p = |n: &str| dir.join(n).to_str().unwrap().to_string();
    let (code, out, err) = tool(&[
        "lpk-decode",
        "verify",
        "--password",
        "correct horse",
        &p("sealed-aes.lpk"),
    ]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.starts_with("ok: 3 entries"), "{out}");
    let (code, _, err) = tool(&["lpk-decode", "list", &p("sealed-aes.lpk")]);
    assert_eq!(code, 1);
    assert!(err.contains("password is required"), "{err}");
    let (code, out, _) = tool(&["lpk-decode", "verify", &p("sealed-aes.lpk")]);
    assert_eq!(code, 0);
    assert!(out.contains("chunks not checked"), "{out}");
    let (code, _, err) = tool(&[
        "lpk-decode",
        "list",
        "--password",
        "bad",
        &p("sealed-aes.lpk"),
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("wrong password"), "{err}");
    // A listable archive lists, and verifies partially, without a password.
    let (code, out, _) = tool(&["lpk-decode", "list", &p("sealed-listable.lpk")]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 3);
    let (code, out, _) = tool(&["lpk-decode", "verify", &p("sealed-listable.lpk")]);
    assert_eq!(code, 0);
    assert!(out.contains("chunks not checked"), "{out}");
    let (code, out, _) = tool(&[
        "lpk-decode",
        "info",
        &p("sealed-keyfile.lpk"),
        "--password",
        "two factors",
        "--keyfile",
        &p(SEALED_KEYFILE),
    ]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("suite: AES-256-GCM"), "{out}");
    assert!(out.contains("argon2id: t=1 m_kib=8192 p=1"), "{out}");
    assert!(out.contains("keyfile required: true"), "{out}");
    // Password file and extraction.
    let tmp = tempfile::tempdir().unwrap();
    let pf = tmp.path().join("pw.txt");
    std::fs::write(&pf, "battery staple\n").unwrap();
    let dest = tmp.path().join("out");
    let (code, out, err) = tool(&[
        "lpk-decode",
        "extract",
        "--password-file",
        pf.to_str().unwrap(),
        &p("sealed-xchacha.lpk"),
        dest.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("extracted 3 files"), "{out}");
    assert!(std::fs::read(dest.join("s").join("b.txt")).unwrap() == sealed_files()[1].1);
}

fn recovery_options(listable: bool) -> WriterOptions {
    let mut o = seal_options(Suite::AesGcm, listable, "pw", None);
    o.recovery = RecoveryOptions {
        percent: 20,
        shard_len: 4096,
        group_shards: 16,
    };
    o
}

#[test]
fn keyless_check_verify_and_repair_of_a_non_listable_archive() {
    let bytes = write_archive_seeded(recovery_options(false), &sealed_files(), SEED);
    let a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    let frames = a.recovery_frames().len();
    assert!(frames > 0);
    let loc = block_loc(&a, 0);
    drop(a);
    // Intact: keyless verify and check pass.
    let mut k = open_with(&bytes, None).unwrap();
    assert!(k.is_keyless());
    assert_eq!(k.recovery_frames().len(), frames);
    let s = k.verify().unwrap();
    assert!(!s.chunks_checked);
    assert_eq!(k.check_recovery().unwrap().shards_damaged, 0);
    // Damaged: keyless check sees it, verify fails, repair rebuilds it.
    let mut damaged = bytes.clone();
    for i in 0..300 {
        damaged[loc.offset as usize + 50 + i] ^= 0xFF;
    }
    let mut k = open_with(&damaged, None).unwrap();
    assert!(k.check_recovery().unwrap().shards_damaged >= 1);
    assert!(k.verify().is_err());
    let mut fixed = Cursor::new(Vec::new());
    let (report, unrepairable) = repair_with_credentials(
        Cursor::new(damaged),
        &mut fixed,
        &Resources::default(),
        None,
    )
    .unwrap();
    assert!(unrepairable.is_none() && report.shards_repaired >= 1);
    assert!(fixed.get_ref() == &bytes);
    let mut a = open_with(fixed.get_ref(), Some(&creds("pw"))).unwrap();
    assert_contents(&mut a);
    // The CLI does the same without a password.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.lpk");
    std::fs::write(&path, &bytes).unwrap();
    let (code, out, err) = tool(&["lpk-decode", "check", path.to_str().unwrap()]);
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
}

#[test]
fn keyless_verify_of_a_listable_archive_with_recovery_frames() {
    let bytes = write_archive_seeded(recovery_options(true), &sealed_files(), SEED);
    let mut k = open_with(&bytes, None).unwrap();
    assert!(!k.recovery_frames().is_empty());
    let s = k.verify().unwrap();
    assert!(!s.chunks_checked);
    assert_eq!(s.entries, 3);
    assert_eq!(k.check_recovery().unwrap().frames_unusable, 0);
}

/// Spec section 9 (E1-14d, verify and recovery): `verify` leaves recovery to
/// `check` in every mode. A damaged recovery frame fails neither verify with
/// the key nor verify without it; `check` reports it.
#[test]
fn verify_does_not_fail_on_a_damaged_recovery_frame() {
    let bytes = write_archive_seeded(recovery_options(false), &sealed_files(), SEED);
    let a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    let loc = a.recovery_frames()[0];
    drop(a);
    let mut damaged = bytes.clone();
    // A byte inside the recovery shards, past the 32 fixed bytes and the hashes.
    damaged[(loc.offset + loc.len - 40) as usize] ^= 0xFF;
    let mut k = open_with(&damaged, None).unwrap();
    assert!(!k.verify().unwrap().chunks_checked);
    assert_eq!(k.check_recovery().unwrap().frames_unusable, 1);
    let mut a = open_with(&damaged, Some(&creds("pw"))).unwrap();
    assert!(a.verify().unwrap().chunks_checked);
    assert_eq!(a.check_recovery().unwrap().frames_unusable, 1);
}

/// Rewrite the frame at `loc` in `bytes` with a new payload of the same length.
fn replace_payload(bytes: &mut [u8], loc: &FrameLocation, payload: &[u8]) {
    let (start, len) = payload_span(loc);
    assert_eq!(payload.len(), len);
    bytes[start..start + len].copy_from_slice(payload);
    let h = blake3::hash(payload);
    bytes[start + len..start + len + 32].copy_from_slice(h.as_bytes());
}

#[test]
fn the_entry_table_is_authenticated_by_the_index() {
    // Listable: rewriting the clear table (with a fresh frame hash) is caught.
    let bytes = build(Suite::AesGcm, true, None);
    let a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    let loc = a.index().entry_table;
    let (start, len) = payload_span(&loc);
    let mut t = bytes.clone();
    let mut payload = t[start..start + len].to_vec();
    let last = payload.len() - 1;
    payload[last] ^= 1;
    replace_payload(&mut t, &loc, &payload);
    let mut a = open_with(&t, Some(&creds("pw"))).unwrap();
    assert!(matches!(
        a.entry_table(),
        Err(FormatError::EntryTableMismatch)
    ));
    assert!(matches!(a.verify(), Err(FormatError::EntryTableMismatch)));
}

#[test]
fn flipping_a_header_flag_fails_at_the_key_slot() {
    // Set LISTABLE on a sealed archive and put a clear table of the same
    // length in place: the key slot binds the flags, so the password fails.
    let bytes = build(Suite::AesGcm, false, None);
    let a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    let loc = a.index().entry_table;
    let (_, len) = payload_span(&loc);
    let mut t = bytes.clone();
    t[12] |= 2;
    replace_payload(&mut t, &loc, &vec![0u8; len]);
    assert!(matches!(
        open_with(&t, Some(&creds("pw"))),
        Err(FormatError::WrongKey)
    ));
    // And clearing LISTABLE on a listable archive.
    let bytes = build(Suite::AesGcm, true, None);
    let mut t = bytes.clone();
    t[12] &= !2;
    assert!(matches!(
        open_with(&t, Some(&creds("pw"))),
        Err(FormatError::WrongKey)
    ));
}

#[test]
fn a_recorded_sequence_that_differs_from_the_real_one_fails_the_tag() {
    let bytes = build(Suite::AesGcm, false, None);
    let a = open_with(&bytes, Some(&creds("pw"))).unwrap();
    let key = a
        .key_slot()
        .unwrap()
        .unwrap(&[0x6B; 16], 1, &creds("pw"))
        .unwrap();
    let mut index = a.index().clone();
    let real = index.blocks[1].sequence;
    index.blocks[1].sequence = real + 1;
    let plain = index.encode().unwrap();
    let sealer = lpk_format::Sealer::new(Suite::AesGcm, key, [0x6B; 16]);
    let sealed = sealer
        .seal(
            &plain,
            FrameKind::Index as u16,
            lpk_format::INDEX_SEQUENCE,
            &a.trailer().salt,
        )
        .unwrap();
    let t = a.trailer();
    let idx_loc = FrameLocation {
        offset: t.index_offset,
        len: t.index_len,
        sequence: 0,
    };
    let mut forged = bytes.clone();
    replace_payload(&mut forged, &idx_loc, &sealed);
    let trailer = lpk_format::Trailer {
        index_hash: *blake3::hash(&sealed).as_bytes(),
        ..*t
    };
    let cut = (t.index_offset + t.index_len) as usize;
    forged.truncate(cut);
    trailer.write(&mut forged).unwrap();
    let mut a = open_with(&forged, Some(&creds("pw"))).unwrap();
    match a.verify() {
        Err(FormatError::AuthenticationFailed { kind, sequence }) => {
            assert_eq!((kind, sequence), (2, real + 1));
        }
        other => panic!("{other:?}"),
    }
}
