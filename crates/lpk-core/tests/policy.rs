//! The extraction policy through the engine (E3-2): the overwrite modes over a half-extracted
//! tree, a target in the way, symlink entries skipped or created, every answer given before
//! anything is written.
#![allow(clippy::unwrap_used)]

use std::io::Cursor;
use std::path::Path;

use lpk_core::policy::{Found, Wanted};
use lpk_core::{
    extract_archive, register_full_reader, CoreError, ExtractOptions, ExtractPolicy,
    ExtractSummary, Overwrite, Policy, Symlinks,
};
use lpk_format::{Archive, EntryFlags, FormatError, Resources, Writer, WriterOptions};

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

fn options() -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 16384,
        archive_id: [7; 16],
        dedup: true,
        ..WriterOptions::default()
    }
}

/// `a.txt`, `b/one.bin`, `b/two.bin`, `empty`, `c/d/deep.bin`.
fn files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.txt", b"hello".to_vec()),
        ("b/one.bin", noise(1, 20_000)),
        ("b/two.bin", noise(2, 30_000)),
        ("empty", Vec::new()),
        ("c/d/deep.bin", noise(3, 5_000)),
    ]
}

fn pack(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options()).unwrap();
    for (p, d) in files {
        w.add_file(p, EntryFlags::EMPTY, 0, &mut &d[..]).unwrap();
    }
    w.finish().unwrap();
    out
}

fn packed_with(f: impl FnOnce(&mut Writer<&mut Vec<u8>>)) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, options()).unwrap();
    f(&mut w);
    w.finish().unwrap();
    out
}

fn extract(
    bytes: &[u8],
    dir: &Path,
    policy: &dyn ExtractPolicy,
) -> Result<ExtractSummary, CoreError> {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    register_full_reader(&mut a);
    let opts = ExtractOptions {
        threads: Some(4),
        ..ExtractOptions::default()
    };
    extract_archive(
        &mut a,
        || Ok(Cursor::new(bytes.to_vec())),
        dir,
        policy,
        &opts,
    )
}

fn policy(o: Overwrite) -> Policy {
    Policy::new(o, Symlinks::Skip)
}

fn check_tree(out: &Path, files: &[(&str, Vec<u8>)]) {
    for (p, d) in files {
        let got = std::fs::read(out.join(p)).unwrap();
        assert_eq!(&got, d, "{p}");
    }
}

/// Everything extracted once, two files removed and one changed: `skip` restores the missing
/// ones, leaves the rest (the changed one included) and counts them; `refuse` is `AlreadyExists`
/// before anything is written; `replace` restores the changed file; `rename` writes beside.
#[test]
fn overwrite_modes_over_a_half_extracted_tree() {
    let files = files();
    let bytes = pack(&files);
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let s = extract(&bytes, &out, &policy(Overwrite::Refuse)).unwrap();
    assert_eq!((s.files, s.skipped, s.renamed), (5, 0, 0));
    check_tree(&out, &files);
    // Half-extracted: two missing, one changed.
    std::fs::remove_file(out.join("b/one.bin")).unwrap();
    std::fs::remove_file(out.join("c/d/deep.bin")).unwrap();
    std::fs::write(out.join("a.txt"), b"changed").unwrap();

    let e = extract(&bytes, &out, &policy(Overwrite::Refuse)).unwrap_err();
    assert!(
        matches!(&e, CoreError::Format(FormatError::Io(io)) if io.kind() == std::io::ErrorKind::AlreadyExists),
        "{e:?}"
    );
    assert!(!out.join("b/one.bin").exists(), "refused before any output");

    let s = extract(&bytes, &out, &policy(Overwrite::Skip)).unwrap();
    assert_eq!((s.files, s.skipped, s.renamed), (2, 3, 0));
    assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"changed");
    assert_eq!(std::fs::read(out.join("b/one.bin")).unwrap(), files[1].1);
    assert_eq!(std::fs::read(out.join("c/d/deep.bin")).unwrap(), files[4].1);

    let s = extract(&bytes, &out, &policy(Overwrite::Replace)).unwrap();
    assert_eq!((s.files, s.skipped, s.renamed), (5, 0, 0));
    check_tree(&out, &files);

    let s = extract(&bytes, &out, &policy(Overwrite::Rename)).unwrap();
    assert_eq!((s.files, s.skipped, s.renamed), (5, 0, 5));
    check_tree(&out, &files);
    assert_eq!(std::fs::read(out.join("a (1).txt")).unwrap(), b"hello");
    assert_eq!(
        std::fs::read(out.join("b/one (1).bin")).unwrap(),
        files[1].1
    );
    assert_eq!(std::fs::read(out.join("empty (1)")).unwrap(), b"");
    // Once more: the smallest free k is 2.
    let s = extract(&bytes, &out, &policy(Overwrite::Rename)).unwrap();
    assert_eq!(s.renamed, 5);
    assert_eq!(std::fs::read(out.join("a (2).txt")).unwrap(), b"hello");
    assert!(!out.join("a (3).txt").exists());
}

#[test]
fn a_directory_where_a_file_goes_is_in_the_way_except_for_skip() {
    let files = files();
    let bytes = pack(&files);
    for mode in [Overwrite::Refuse, Overwrite::Rename, Overwrite::Replace] {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir_all(out.join("a.txt")).unwrap();
        let e = extract(&bytes, &out, &policy(mode)).unwrap_err();
        assert!(
            matches!(&e, CoreError::InTheWay { path, found: "directory", wanted: "file" } if path.ends_with("a.txt")),
            "{mode:?}: {e:?}"
        );
        assert!(!out.join("b").exists(), "{mode:?}: something was written");
    }
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    std::fs::create_dir_all(out.join("a.txt")).unwrap();
    let s = extract(&bytes, &out, &policy(Overwrite::Skip)).unwrap();
    assert_eq!((s.files, s.skipped), (4, 1));
    assert!(out.join("a.txt").is_dir());
}

#[test]
fn a_file_where_a_directory_goes_is_in_the_way_and_skip_skips_everything_below() {
    let files = files();
    let bytes = pack(&files);
    // An implicit parent (`b` is only the files' parent) and a directory entry.
    let with_dir = packed_with(|w| {
        w.add_directory("c", EntryFlags::EMPTY, 0).unwrap();
        w.add_file("c/x", EntryFlags::EMPTY, 0, &mut &b"x"[..])
            .unwrap();
        w.add_file("y", EntryFlags::EMPTY, 0, &mut &b"y"[..])
            .unwrap();
    });
    for (bytes, blocker, below) in [(&bytes, "b", "b/one.bin"), (&with_dir, "c", "c/x")] {
        for mode in [Overwrite::Refuse, Overwrite::Replace] {
            let d = tempfile::tempdir().unwrap();
            let out = d.path().join("out");
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join(blocker), b"file").unwrap();
            let e = extract(bytes, &out, &policy(mode)).unwrap_err();
            assert!(
                matches!(&e, CoreError::InTheWay { path, found: "file", wanted: "directory" } if path.ends_with(blocker)),
                "{mode:?} {blocker}: {e:?}"
            );
            assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "{mode:?}");
        }
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join(blocker), b"file").unwrap();
        let s = extract(bytes, &out, &policy(Overwrite::Skip)).unwrap();
        // `b`: its two files skipped, the other three written; `c`: the directory entry and
        // the file below it skipped, `y` written.
        let want = if blocker == "b" { (3, 2) } else { (1, 2) };
        assert_eq!((s.files, s.skipped), want, "{blocker}: {s:?}");
        assert_eq!(std::fs::read(out.join(blocker)).unwrap(), b"file");
        assert!(!out.join(below).exists());
    }
}

#[test]
fn replace_removes_a_link_at_the_target_as_a_link() {
    let files = files();
    let bytes = pack(&files);
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let elsewhere = d.path().join("elsewhere");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("keep"), b"keep").unwrap();
    // A link to a directory where the file `a.txt` goes.
    let link = out.join("a.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    #[cfg(windows)]
    {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&elsewhere)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !made {
            println!("SKIPPED: mklink /J refused on this machine");
            return;
        }
    }
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    // Refuse: a symlink is an existing target.
    let e = extract(&bytes, &out, &policy(Overwrite::Refuse)).unwrap_err();
    assert!(
        matches!(&e, CoreError::Format(FormatError::Io(io)) if io.kind() == std::io::ErrorKind::AlreadyExists),
        "{e:?}"
    );
    // Replace: the link goes, the directory it pointed to is untouched, the file is written.
    let s = extract(&bytes, &out, &policy(Overwrite::Replace)).unwrap();
    assert_eq!(s.files, 5);
    let m = std::fs::symlink_metadata(&link).unwrap();
    assert!(m.file_type().is_file());
    assert_eq!(std::fs::read(&link).unwrap(), b"hello");
    assert_eq!(std::fs::read(elsewhere.join("keep")).unwrap(), b"keep");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 1);
}

#[test]
fn symlink_entries_are_skipped_with_a_record_or_created() {
    let bytes = packed_with(|w| {
        w.add_file("a.txt", EntryFlags::EMPTY, 0, &mut &b"target"[..])
            .unwrap();
        w.add_symlink("link", EntryFlags::EMPTY, 0, b"a.txt")
            .unwrap();
        w.add_symlink("sub/link2", EntryFlags::EMPTY, 0, b"../a.txt")
            .unwrap();
    });
    // Skipped: counted, recorded, never created; the files still written.
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let p = Policy::default();
    let s = extract(&bytes, &out, &p).unwrap();
    assert_eq!((s.files, s.symlinks, s.symlinks_skipped), (1, 0, 2));
    assert_eq!(p.skipped_symlinks(), vec!["link", "sub/link2"]);
    assert!(std::fs::symlink_metadata(out.join("link")).is_err());
    assert!(!out.join("sub").exists());
    // Refused (the reference tool's rule): nothing written.
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let e = extract(
        &bytes,
        &out,
        &Policy::new(Overwrite::Refuse, Symlinks::Refuse),
    )
    .unwrap_err();
    assert!(matches!(
        e,
        CoreError::Format(FormatError::SymlinkRefused { .. })
    ));
    assert!(!out.exists());
    // Created, with the parent directory of a link made for it.
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let p = Policy::new(Overwrite::Refuse, Symlinks::Create);
    match extract(&bytes, &out, &p) {
        Ok(s) => {
            assert_eq!((s.files, s.symlinks, s.symlinks_skipped), (1, 2, 0));
            assert!(p.skipped_symlinks().is_empty());
            let l = out.join("link");
            assert!(std::fs::symlink_metadata(&l)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read_link(&l).unwrap(), Path::new("a.txt"));
            assert_eq!(std::fs::read(&l).unwrap(), b"target");
            assert_eq!(
                std::fs::read_link(out.join("sub/link2")).unwrap(),
                Path::new("../a.txt")
            );
            // An existing link where a link goes: refused; replaced on request.
            let e = extract(&bytes, &out, &p).unwrap_err();
            assert!(
                matches!(&e, CoreError::Format(FormatError::Io(io)) if io.kind() == std::io::ErrorKind::AlreadyExists),
                "{e:?}"
            );
            let s = extract(
                &bytes,
                &out,
                &Policy::new(Overwrite::Replace, Symlinks::Create),
            )
            .unwrap();
            assert_eq!((s.files, s.symlinks), (1, 2));
            assert_eq!(std::fs::read(out.join("link")).unwrap(), b"target");
        }
        Err(e) => {
            // Windows without the privilege: the link's creation fails the extraction,
            // naming the link, before any file is written.
            if !cfg!(windows) {
                panic!("{e:?}");
            }
            assert!(
                matches!(&e, CoreError::Io { path, .. } if path.ends_with("link")),
                "{e:?}"
            );
            assert!(!out.join("a.txt").exists());
            println!("SKIPPED: symlink creation refused on this machine ({e})");
        }
    }
}

/// `Found::of` tells the three kinds apart, and a policy's answer for each is what the engine
/// acts on (the table of the unit tests, seen through the hook).
#[test]
fn found_of_tells_the_kinds_apart() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("f");
    std::fs::write(&f, b"1").unwrap();
    let dd = d.path().join("d");
    std::fs::create_dir(&dd).unwrap();
    assert_eq!(
        Found::of(&std::fs::symlink_metadata(&f).unwrap()),
        Found::File
    );
    assert_eq!(
        Found::of(&std::fs::symlink_metadata(&dd).unwrap()),
        Found::Directory
    );
    let p = policy(Overwrite::Skip);
    assert_eq!(
        p.existing(&dd, Found::Directory, Wanted::File).unwrap(),
        lpk_core::policy::Existing::Skip
    );
}
