//! Encryption through the pipeline (E2-12): the Fast and Balanced tiers sealed with a password,
//! a keyfile or both, listable or not; the refusals of `extract_file` before any output exists;
//! the peel and the dedup unchanged under encryption; the independent decoder reading what the
//! pipeline sealed.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_core::{
    extract_file, BalancedOptions, CoreError, DefaultPolicy, ExtractOptions, FastOptions, Pipeline,
    RunSummary,
};
use lpk_format::{
    Archive, Argon2Params, Credentials, EntryKind, FormatError, Resources, SealOptions, Suite,
};

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

fn prose(seed: u64, len: usize) -> Vec<u8> {
    const WORDS: [&str; 8] = ["the", "king", "of", "France", "and", "army", "sea", "road"];
    let mut out = Vec::new();
    let r = noise(seed, len);
    for b in r {
        out.extend_from_slice(WORDS[(b % 8) as usize].as_bytes());
        out.push(b' ');
        if out.len() >= len {
            break;
        }
    }
    out.truncate(len);
    out
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// A tree with prose, noise, a duplicate file, a file repeating its own block, an empty file,
/// a nested directory and two JPEGs (one peels, one is the same picture with trailing bytes).
fn make_tree(root: &Path) {
    std::fs::create_dir_all(root.join("docs/deep")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("a.txt"), prose(1, 300_000)).unwrap();
    std::fs::write(root.join("docs/b.md"), prose(2, 120_000)).unwrap();
    let big = noise(7, 400_000);
    std::fs::write(root.join("docs/deep/r.bin"), &big).unwrap();
    std::fs::write(root.join("docs/deep/r-again.bin"), &big).unwrap();
    let mut twice = noise(8, 90_000);
    let again = twice.clone();
    twice.extend_from_slice(&again);
    std::fs::write(root.join("twice.bin"), twice).unwrap();
    std::fs::write(root.join("zero"), b"").unwrap();
    let jpeg = fixture("baseline.jpg");
    std::fs::write(root.join("p.jpg"), &jpeg).unwrap();
    let mut trailing = jpeg;
    trailing.extend_from_slice(b"some trailing bytes after the EOI");
    std::fs::write(root.join("p-trailing.jpg"), trailing).unwrap();
}

/// Relative path -> BLAKE3 of every file under `root` (directories as `None`).
fn tree(root: &Path) -> BTreeMap<String, Option<[u8; 32]>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Option<[u8; 32]>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(rel, None);
                walk(&p, base, out);
            } else {
                out.insert(
                    rel,
                    Some(*blake3::hash(&std::fs::read(&p).unwrap()).as_bytes()),
                );
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

#[derive(Clone, Copy)]
enum Tier {
    Fast,
    Balanced,
}

fn pipeline(tier: Tier) -> Pipeline {
    match tier {
        Tier::Fast => Pipeline::fast(FastOptions::default()),
        Tier::Balanced => Pipeline::balanced(BalancedOptions {
            dict_size: 1 << 20,
            ..BalancedOptions::default()
        }),
    }
}

fn seal(suite: Suite, listable: bool, credentials: &Credentials) -> SealOptions {
    SealOptions {
        suite,
        argon2: Argon2Params {
            t: 1,
            m_kib: 8192,
            p: 1,
        },
        listable,
        credentials: credentials.clone(),
    }
}

fn archive(
    tier: Tier,
    src: &Path,
    path: &Path,
    seal: Option<SealOptions>,
) -> Result<RunSummary, CoreError> {
    let mut p = pipeline(tier);
    p.seal.writer.seal = seal;
    p.run_file(src, path)
}

fn password() -> Credentials {
    Credentials::password("correct horse battery staple")
}

fn keyfile_only() -> Credentials {
    Credentials {
        password: Vec::new(),
        keyfile: Some(noise(99, 4096)),
    }
}

fn both() -> Credentials {
    Credentials {
        password: b"pw".to_vec(),
        keyfile: Some(noise(98, 100)),
    }
}

#[test]
fn both_tiers_round_trip_with_a_password_a_keyfile_or_both_listable_or_not() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let want = tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let cases = [
        (Tier::Fast, Suite::AesGcm, false, password()),
        (Tier::Fast, Suite::XChaCha, true, password()),
        (Tier::Fast, Suite::AesGcm, true, keyfile_only()),
        (Tier::Fast, Suite::XChaCha, false, both()),
        (Tier::Balanced, Suite::AesGcm, false, password()),
        (Tier::Balanced, Suite::XChaCha, true, keyfile_only()),
        (Tier::Balanced, Suite::AesGcm, true, both()),
    ];
    for (i, (tier, suite, listable, creds)) in cases.iter().enumerate() {
        let arch = work.path().join(format!("{i}.lpk"));
        let out = work.path().join(format!("out{i}"));
        archive(
            *tier,
            src.path(),
            &arch,
            Some(seal(*suite, *listable, creds)),
        )
        .unwrap();
        let a = Archive::open(std::fs::File::open(&arch).unwrap(), &Resources::default()).unwrap();
        assert!(a.is_encrypted() && a.is_keyless(), "case {i}");
        assert_eq!(a.is_listable(), *listable, "case {i}");
        assert_eq!(a.suite(), Some(*suite), "case {i}");
        drop(a);
        let s = extract_file(
            &arch,
            &out,
            &[],
            Some(creds),
            &DefaultPolicy,
            &ExtractOptions::default(),
        )
        .unwrap();
        assert_eq!(tree(&out), want, "case {i}");
        assert_eq!(s.files, 8, "case {i}");
    }
}

#[test]
fn a_wrong_or_missing_password_is_refused_before_any_output_exists() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("a.lpk");
    archive(
        Tier::Fast,
        src.path(),
        &arch,
        Some(seal(Suite::AesGcm, false, &password())),
    )
    .unwrap();
    let out = work.path().join("deep/out");
    let wrong = Credentials::password("correct horse battery stapler");
    let r = extract_file(
        &arch,
        &out,
        &[],
        Some(&wrong),
        &DefaultPolicy,
        &ExtractOptions::default(),
    );
    assert!(
        matches!(r, Err(CoreError::Format(FormatError::WrongKey))),
        "{r:?}"
    );
    assert!(!out.exists() && !out.parent().unwrap().exists());
    let r = extract_file(
        &arch,
        &out,
        &[],
        None,
        &DefaultPolicy,
        &ExtractOptions::default(),
    );
    assert!(
        matches!(r, Err(CoreError::Format(FormatError::PasswordRequired))),
        "{r:?}"
    );
    assert!(!out.exists() && !out.parent().unwrap().exists());
    // An archive sealed with a keyfile as well: the password alone, or the password with the
    // wrong keyfile, is refused the same way.
    let two = work.path().join("two.lpk");
    archive(
        Tier::Fast,
        src.path(),
        &two,
        Some(seal(Suite::AesGcm, false, &both())),
    )
    .unwrap();
    let wrong_keyfile = Credentials {
        password: both().password.clone(),
        keyfile: Some(vec![1, 2, 3]),
    };
    for creds in [
        Credentials::password(both().password.clone()),
        wrong_keyfile,
    ] {
        let r = extract_file(
            &two,
            &out,
            &[],
            Some(&creds),
            &DefaultPolicy,
            &ExtractOptions::default(),
        );
        assert!(r.is_err(), "{r:?}");
        assert!(!out.exists());
    }
    // With the right credentials the same call succeeds.
    extract_file(
        &arch,
        &out,
        &[],
        Some(&password()),
        &DefaultPolicy,
        &ExtractOptions::default(),
    )
    .unwrap();
    assert_eq!(tree(&out), tree(src.path()));
}

#[test]
fn a_listable_archive_lists_without_the_key_and_a_sealed_one_does_not() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    for listable in [true, false] {
        let arch = work.path().join(format!("{listable}.lpk"));
        archive(
            Tier::Fast,
            src.path(),
            &arch,
            Some(seal(Suite::AesGcm, listable, &password())),
        )
        .unwrap();
        let bytes = std::fs::read(&arch).unwrap();
        let mut a = Archive::open(Cursor::new(bytes), &Resources::default()).unwrap();
        assert!(a.is_keyless());
        match a.entry_table() {
            Ok(t) => {
                assert!(listable);
                let n = t
                    .table()
                    .unwrap()
                    .iter()
                    .filter(|e| e.as_ref().unwrap().kind == EntryKind::File)
                    .count();
                assert_eq!(n, 8);
            }
            Err(FormatError::PasswordRequired) => assert!(!listable),
            Err(e) => panic!("{e}"),
        }
        // The frame hashes verify without the key either way.
        let s = a.verify().unwrap();
        assert!(!s.chunks_checked);
    }
}

#[test]
fn the_peel_and_the_dedup_are_unchanged_under_encryption() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let plain = archive(Tier::Fast, src.path(), &work.path().join("p.lpk"), None).unwrap();
    let sealed = archive(
        Tier::Fast,
        src.path(),
        &work.path().join("s.lpk"),
        Some(seal(Suite::XChaCha, false, &both())),
    )
    .unwrap();
    assert_eq!(plain.peel, sealed.peel);
    // One of the two JPEGs peels; the other (the same picture) is found already stored.
    assert_eq!(plain.peel.peeled.files, 1, "{:?}", plain.peel);
    assert_eq!(
        plain.peel.deduplicated.files + plain.peel.primary_deduplicated.files,
        1,
        "{:?}",
        plain.peel
    );
    let w = |s: &RunSummary| {
        (
            s.writer.entries,
            s.writer.chunks,
            s.writer.blocks,
            s.writer.new_chunks,
            s.writer.deduped_chunks,
            s.writer.deduped_bytes,
        )
    };
    assert_eq!(w(&plain), w(&sealed));
    assert!(plain.writer.deduped_chunks > 0, "{:?}", plain.writer);
    assert_eq!(plain.fast, sealed.fast);
    // The sealed archive is longer by the per-frame nonce and tag and the key slot only.
    assert!(sealed.writer.archive_len > plain.writer.archive_len);
}

#[test]
fn the_independent_decoder_verifies_and_reads_what_the_pipeline_sealed() {
    let src = tempfile::tempdir().unwrap();
    make_tree(src.path());
    let work = tempfile::tempdir().unwrap();
    let arch = work.path().join("a.lpk");
    let creds = both();
    archive(
        Tier::Fast,
        src.path(),
        &arch,
        Some(seal(Suite::AesGcm, false, &creds)),
    )
    .unwrap();
    let bytes = std::fs::read(&arch).unwrap();
    let opts = lpk_check::archive::Options {
        password: Some(creds.password.clone()),
        keyfile: creds.keyfile.clone(),
        ..lpk_check::archive::Options::default()
    };
    let mut c = lpk_check::archive::Archive::open(bytes.clone(), opts).unwrap();
    c.verify().unwrap();
    let mut files = 0;
    for e in c.entries().unwrap() {
        if e.kind == lpk_check::entries::EntryKind::File {
            let want = std::fs::read(src.path().join(&e.path)).unwrap();
            assert!(c.read_file(&e).unwrap() == want, "check: {}", e.path);
            files += 1;
        }
    }
    assert_eq!(files, 8);
    // The wrong password is refused by the independent decoder too.
    let wrong = lpk_check::archive::Options {
        password: Some(b"not it".to_vec()),
        keyfile: creds.keyfile.clone(),
        ..lpk_check::archive::Options::default()
    };
    assert!(lpk_check::archive::Archive::open(bytes, wrong).is_err());
}
