//! The extraction-policy tests of E6-6: the TOCTOU test (a verified parent swapped for a
//! junction between its verification and its child's creation), a parent swapped before its
//! verification, the held-handle cap with its file-ID check, the symlink privilege branches,
//! the length rule, deep paths and case-insensitive opens.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use lpk_format::{Archive, EntryFlags, FormatError, Resources, Writer, WriterOptions};

use super::seam;
use crate::{extract_archive, CoreError, ExtractOptions, ExtractSummary, Overwrite, Policy};
use crate::{ExtractPolicy, Symlinks};

fn pack(f: impl FnOnce(&mut Writer<&mut Vec<u8>>)) -> Vec<u8> {
    let mut out = Vec::new();
    let opts = WriterOptions {
        chunk_size: 4096,
        block_size: 16384,
        archive_id: [9; 16],
        dedup: true,
        ..WriterOptions::default()
    };
    let mut w = Writer::new(&mut out, opts).unwrap();
    f(&mut w);
    w.finish().unwrap();
    out
}

fn files(list: &[(&str, &[u8])]) -> Vec<u8> {
    pack(|w| {
        for (p, d) in list {
            w.add_file(p, EntryFlags::EMPTY, 0, &mut &d[..]).unwrap();
        }
    })
}

/// One thread (so the hooks, which are per thread, see every create), `max_open` files open.
fn extract_with(
    bytes: &[u8],
    dir: &Path,
    policy: &dyn ExtractPolicy,
    max_open: usize,
) -> Result<ExtractSummary, CoreError> {
    let mut a = Archive::open(Cursor::new(bytes.to_vec()), &Resources::default()).unwrap();
    let opts = ExtractOptions {
        threads: Some(1),
        max_open_files: max_open,
        ..ExtractOptions::default()
    };
    let b = bytes.to_vec();
    extract_archive(
        &mut a,
        move || Ok(Cursor::new(b.clone())),
        dir,
        policy,
        &opts,
    )
}

fn extract(
    bytes: &[u8],
    dir: &Path,
    policy: &dyn ExtractPolicy,
) -> Result<ExtractSummary, CoreError> {
    extract_with(bytes, dir, policy, 128)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Clears this thread's hooks and switches when a test ends, pass or fail.
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        seam::on_before_create(None);
        seam::on_before_open_dir(None);
        seam::force_symlink_privilege(None);
        seam::force_max_held(None);
        #[cfg(windows)]
        lpk_fs_sys::set_share_delete_for_tests(false);
    }
}

/// The TOCTOU setting: `out/sub/f.txt` exists (so `--overwrite replace` plans a replacement),
/// an outside folder holds a canary, and the hook, between `out/sub`'s verification and the
/// creation of `f.txt` in it, renames `out/sub` to `out/moved` and plants a junction
/// `out/sub` to the outside folder. Returns (out, outside, whether the rename succeeded).
#[cfg(windows)]
fn swap_at_create(
    share_delete: bool,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    bool,
    Result<ExtractSummary, CoreError>,
) {
    use std::cell::Cell;
    use std::rc::Rc;
    let _reset = Reset;
    let work = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("canary.txt"), b"canary").unwrap();
    let out = work.path().join("out");
    std::fs::create_dir_all(out.join("sub")).unwrap();
    std::fs::write(out.join("sub").join("f.txt"), b"old").unwrap();
    let bytes = files(&[("sub/f.txt", b"new")]);
    lpk_fs_sys::set_share_delete_for_tests(share_delete);
    let renamed = Rc::new(Cell::new(None));
    let (o, t, r) = (
        out.clone(),
        outside.path().to_path_buf(),
        Rc::clone(&renamed),
    );
    seam::on_before_create(Some(Box::new(move |p: &Path| {
        if p.ends_with("sub/f.txt") && r.get().is_none() {
            let ok = std::fs::rename(o.join("sub"), o.join("moved")).is_ok();
            r.set(Some(ok));
            if ok {
                assert!(crate::ingest::tests::junction(&t, &o.join("sub")));
            }
        }
    })));
    let res = extract(
        &bytes,
        &out,
        &Policy::new(Overwrite::Replace, Symlinks::Skip),
    );
    let renamed = renamed.get().expect("the hook ran");
    (work, outside, renamed, res)
}

/// (a) With the held handles, the verified parent cannot be renamed: the swap fails, the
/// file replaces the one in the real `out/sub`, and the outside folder is untouched.
#[cfg(windows)]
#[test]
fn toctou_a_verified_parent_cannot_be_swapped_for_a_junction() {
    let (work, outside, renamed, res) = swap_at_create(false);
    assert!(!renamed, "the held parent was renamed");
    res.unwrap();
    assert_eq!(listing(outside.path()), ["canary.txt"]);
    assert_eq!(
        std::fs::read(outside.path().join("canary.txt")).unwrap(),
        b"canary"
    );
    let out = work.path().join("out");
    assert_eq!(std::fs::read(out.join("sub/f.txt")).unwrap(), b"new");
    assert_eq!(listing(&out), ["sub"]);
}

/// (b) With `FILE_SHARE_DELETE` added (the test-only switch), the swap succeeds, and the
/// file still lands in the moved original: it is created relative to the held handle, never
/// by path. The outside folder is untouched.
#[cfg(windows)]
#[test]
fn toctou_with_share_delete_the_file_lands_in_the_moved_original() {
    let (work, outside, renamed, res) = swap_at_create(true);
    assert!(renamed, "the switch did not allow the rename");
    res.unwrap();
    assert_eq!(listing(outside.path()), ["canary.txt"]);
    assert_eq!(
        std::fs::read(outside.path().join("canary.txt")).unwrap(),
        b"canary"
    );
    let out = work.path().join("out");
    assert_eq!(std::fs::read(out.join("moved/f.txt")).unwrap(), b"new");
    // The junction planted by the test, removed as a link (never followed).
    std::fs::remove_dir(out.join("sub")).unwrap();
}

/// A parent swapped for a junction before its verification (the plan saw a real directory):
/// the relative open meets the junction and the extraction fails as "linked parent", with
/// nothing written outside.
#[cfg(windows)]
#[test]
fn a_parent_swapped_before_its_verification_fails_as_a_linked_parent() {
    let _reset = Reset;
    let work = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("canary.txt"), b"canary").unwrap();
    let out = work.path().join("out");
    std::fs::create_dir_all(out.join("sub")).unwrap();
    let bytes = files(&[("sub/deep/f.txt", b"new")]);
    let (o, t) = (out.clone(), outside.path().to_path_buf());
    let mut done = false;
    seam::on_before_open_dir(Some(Box::new(move |p: &Path| {
        if p.ends_with("sub") && !done {
            done = true;
            std::fs::rename(o.join("sub"), o.join("moved")).unwrap();
            assert!(crate::ingest::tests::junction(&t, &o.join("sub")));
        }
    })));
    let e = extract(&bytes, &out, &Policy::default()).unwrap_err();
    assert!(
        matches!(
            &e,
            CoreError::Format(FormatError::UnsafePath {
                reason: "linked parent",
                ..
            })
        ),
        "{e:?}"
    );
    assert_eq!(listing(outside.path()), ["canary.txt"]);
    assert_eq!(
        std::fs::read(outside.path().join("canary.txt")).unwrap(),
        b"canary"
    );
    std::fs::remove_dir(out.join("sub")).unwrap();
}

/// Past the cap the oldest held directory is closed; reopening it compares its file ID, so a
/// directory replaced in between fails the extraction instead of receiving the file.
#[cfg(windows)]
#[test]
fn a_closed_directory_is_reopened_only_when_its_file_id_is_unchanged() {
    let _reset = Reset;
    seam::force_max_held(Some(2));
    let work = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..5).map(|i| format!("d{i}/f.txt")).collect();
    let list: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
    let bytes = files(&list);
    // Without interference every file arrives through the reopened directories.
    let out = work.path().join("ok");
    let s = extract(&bytes, &out, &Policy::default()).unwrap();
    assert_eq!(s.files, 5);
    for n in &names {
        assert_eq!(std::fs::read(out.join(n)).unwrap(), b"x");
    }
    // `d0` replaced by another directory after it was verified and closed.
    let out = work.path().join("swapped");
    let o = out.clone();
    let mut seen = 0;
    seam::on_before_open_dir(Some(Box::new(move |p: &Path| {
        if p.ends_with("d0") {
            seen += 1;
            if seen == 2 {
                std::fs::rename(o.join("d0"), o.join("d0-old")).unwrap();
                std::fs::create_dir(o.join("d0")).unwrap();
            }
        }
    })));
    let e = extract(&bytes, &out, &Policy::default()).unwrap_err();
    assert!(
        matches!(&e, CoreError::Io { path, source } if path.ends_with("d0") && source.to_string().contains("file ID")),
        "{e:?}"
    );
    assert!(std::fs::read_dir(out.join("d0")).unwrap().next().is_none());
}

/// Without the privilege (forced here, so CI runs both branches) a link `--symlinks` would
/// create is skipped, recorded for the warning and counted; the extraction succeeds and
/// nothing is created at the link's path (D-79 (3) as amended).
#[test]
fn an_unprivileged_run_skips_links_with_a_record() {
    let _reset = Reset;
    seam::force_symlink_privilege(Some(false));
    let bytes = pack(|w| {
        w.add_file("a.txt", EntryFlags::EMPTY, 0, &mut &b"target"[..])
            .unwrap();
        w.add_symlink("link", EntryFlags::EMPTY, 0, b"a.txt")
            .unwrap();
        w.add_symlink("abs", EntryFlags::EMPTY, 0, b"/etc/passwd")
            .unwrap();
    });
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let p = Policy::new(Overwrite::Refuse, Symlinks::Create);
    let s = extract(&bytes, &out, &p).unwrap();
    assert_eq!((s.files, s.symlinks, s.symlinks_skipped), (1, 0, 2));
    assert_eq!(p.unprivileged_symlinks(), ["link"]);
    // The target rule comes first and is unchanged (E6-0).
    assert_eq!(p.refused_symlink_targets().len(), 1);
    assert!(std::fs::symlink_metadata(out.join("link")).is_err());
    assert_eq!(listing(&out), ["a.txt"]);
}

/// With the privilege forced on, links are planned; whether the machine can create them is
/// then the system's answer (privileged: created; not: the creation fails, as before).
#[test]
fn a_privileged_run_creates_links() {
    let _reset = Reset;
    seam::force_symlink_privilege(Some(true));
    let bytes = pack(|w| {
        w.add_file("a.txt", EntryFlags::EMPTY, 0, &mut &b"target"[..])
            .unwrap();
        w.add_symlink("link", EntryFlags::EMPTY, 0, b"a.txt")
            .unwrap();
    });
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let p = Policy::new(Overwrite::Refuse, Symlinks::Create);
    let real = {
        seam::force_symlink_privilege(None);
        let v = super::can_create_symlinks();
        seam::force_symlink_privilege(Some(true));
        v
    };
    match extract(&bytes, &out, &p) {
        Ok(s) => {
            assert!(real);
            assert_eq!(s.symlinks, 1);
            assert_eq!(std::fs::read(out.join("link")).unwrap(), b"target");
            assert!(p.unprivileged_symlinks().is_empty());
        }
        Err(e) => {
            assert!(!real && cfg!(windows), "{e:?}");
            assert!(
                matches!(&e, CoreError::Io { path, .. } if path.ends_with("link")),
                "{e:?}"
            );
            assert!(std::fs::symlink_metadata(out.join("link")).is_err());
        }
    }
}

#[test]
fn the_length_rule_names_long_names_and_long_paths() {
    use crate::policy::{check_path_length, MAX_NAME_UNITS, MAX_PATH_UNITS};
    let name = "n".repeat(MAX_NAME_UNITS);
    check_path_length(10, &name).unwrap();
    let e = check_path_length(10, &format!("{name}x")).unwrap_err();
    assert!(
        matches!(
            e,
            FormatError::UnsafePath {
                reason: "name too long",
                ..
            }
        ),
        "{e:?}"
    );
    // UTF-16 units, not bytes: 255 two-byte characters fit, one supplementary character
    // counts two.
    check_path_length(10, &"é".repeat(MAX_NAME_UNITS)).unwrap();
    let e = check_path_length(10, &"😀".repeat(128)).unwrap_err();
    assert!(
        matches!(
            e,
            FormatError::UnsafePath {
                reason: "name too long",
                ..
            }
        ),
        "{e:?}"
    );
    let deep = vec![&name[..200]; 170].join("/");
    let e = check_path_length(10, &deep).unwrap_err();
    assert!(
        matches!(
            e,
            FormatError::UnsafePath {
                reason: "path too long",
                ..
            }
        ),
        "{e:?}"
    );
    // Exactly at the limit passes.
    let one = "a".repeat(100);
    let n = (MAX_PATH_UNITS - 10) / 101;
    let p = vec![&one[..]; n].join("/");
    check_path_length(10, &p).unwrap();
    check_path_length(MAX_PATH_UNITS - 2, "a").unwrap();
    assert!(check_path_length(MAX_PATH_UNITS - 1, "a").is_err());
}

/// On Windows an entry whose path would pass 32,767 UTF-16 units under the destination is
/// refused by name before anything is created.
#[cfg(windows)]
#[test]
fn a_path_over_the_windows_limit_is_refused_before_output() {
    let seg = "s".repeat(200);
    let long = format!("{}/f", vec![&seg[..]; 165].join("/"));
    let bytes = files(&[("ok.txt", b"1"), (long.as_str(), b"2")]);
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let e = extract(&bytes, &out, &Policy::default()).unwrap_err();
    assert!(
        matches!(
            &e,
            CoreError::Format(FormatError::UnsafePath {
                reason: "path too long",
                ..
            })
        ),
        "{e:?}"
    );
    assert!(!out.exists());
}

/// No `MAX_PATH` anywhere: a tree far deeper than 260 characters is extracted, reopened
/// (one file open at a time over several blocks) and listed back.
#[test]
fn deep_paths_need_no_max_path() {
    let seg = "d".repeat(60);
    let dir = vec![&seg[..]; 20].join("/");
    let a = format!("{dir}/a.bin");
    let b = format!("{dir}/b.bin");
    let big: Vec<u8> = (0..40_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let bytes = files(&[(a.as_str(), &big[..]), (b.as_str(), &big[..1000])]);
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let s = extract_with(&bytes, &out, &Policy::default(), 1).unwrap();
    assert_eq!(s.files, 2);
    let mut p: PathBuf = out.clone();
    for c in a.split('/') {
        p.push(c);
    }
    assert!(p.as_os_str().len() > 1200);
    assert_eq!(std::fs::read(&p).unwrap(), big);
}

/// NTFS compares names without case: an existing `Sub` receives `sub/f.txt` (the opens are
/// case-insensitive), and no second directory appears.
#[cfg(windows)]
#[test]
fn opens_are_case_insensitive() {
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    std::fs::create_dir_all(out.join("Sub")).unwrap();
    let bytes = files(&[("sub/f.txt", b"x")]);
    extract(&bytes, &out, &Policy::default()).unwrap();
    assert_eq!(listing(&out), ["Sub"]);
    assert_eq!(std::fs::read(out.join("Sub/f.txt")).unwrap(), b"x");
}

/// A create that fails after the plan (something appeared at the name) fails the extraction
/// naming the file; files created and not finished are removed through the tree (by the file
/// ID recorded at creation), finished ones stay.
#[test]
fn a_failed_extraction_removes_its_unfinished_files() {
    let _reset = Reset;
    let big: Vec<u8> = (0..60_000u32).map(|i| (i * 13 % 251) as u8).collect();
    let bytes = files(&[("done.txt", b"1"), ("big.bin", &big[..])]);
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    // The second create fails (something appears at its name after the plan).
    let o = out.clone();
    seam::on_before_create(Some(Box::new(move |p: &Path| {
        if p.ends_with("big.bin") {
            std::fs::create_dir(o.join("big.bin")).unwrap();
        }
    })));
    let e = extract(&bytes, &out, &Policy::default()).unwrap_err();
    assert!(
        matches!(&e, CoreError::Io { path, .. } if path.ends_with("big.bin")),
        "{e:?}"
    );
    // Whatever the write order, no partial file is left: `done.txt` is complete or absent.
    match std::fs::read(out.join("done.txt")) {
        Ok(d) => assert_eq!(d, b"1"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
    }
    assert!(out.join("big.bin").is_dir());
}

/// Cleanup goes by the file ID recorded at creation: `a.bin` is half-written (its chunks span
/// two blocks) when the next create fails; before the cleanup runs, `a.bin` was swapped for
/// another file, which survives (the extraction removes only the file it created).
#[cfg(windows)]
#[test]
fn cleanup_removes_only_the_file_it_created() {
    let _reset = Reset;
    let noise: Vec<u8> = {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        (0..30_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    };
    // `b.bin` is `a.bin`'s first chunk: deduplicated into block 0, so it is created while
    // `a.bin` still waits for its chunks in the next block.
    let bytes = files(&[("a.bin", &noise[..]), ("b.bin", &noise[..4096])]);
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    let o = out.clone();
    let mut first: Option<PathBuf> = None;
    seam::on_before_create(Some(Box::new(move |p: &Path| match &first {
        None => first = Some(p.to_path_buf()),
        Some(f) => {
            assert!(f.ends_with("a.bin"), "{f:?} first");
            // `a.bin` (open, partial) moved away and replaced; `b.bin` blocked.
            std::fs::rename(f, o.join("a.orig")).unwrap();
            std::fs::write(f, b"swapped in").unwrap();
            std::fs::create_dir(o.join("b.bin")).unwrap();
        }
    })));
    let e = extract(&bytes, &out, &Policy::default()).unwrap_err();
    assert!(
        matches!(&e, CoreError::Io { path, .. } if path.ends_with("b.bin")),
        "{e:?}"
    );
    assert_eq!(std::fs::read(out.join("a.bin")).unwrap(), b"swapped in");
    // The moved original was partial when the failure came (its tail never written).
    let orig = std::fs::read(out.join("a.orig")).unwrap();
    assert_ne!(orig, noise);
}

/// `--overwrite rename` re-checks the name it picked: a 255-unit name taken on disk would
/// become `name (1).ext`, over the limit, and is refused by name before output.
#[cfg(windows)]
#[test]
fn a_renamed_name_over_the_limit_is_refused() {
    let name = format!("{}.txt", "n".repeat(251));
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join(&name), b"old").unwrap();
    let bytes = files(&[(name.as_str(), b"new")]);
    let e = extract(
        &bytes,
        &out,
        &Policy::new(Overwrite::Rename, Symlinks::Skip),
    )
    .unwrap_err();
    assert!(
        matches!(
            &e,
            CoreError::Format(FormatError::UnsafePath {
                reason: "name too long",
                ..
            })
        ),
        "{e:?}"
    );
    assert_eq!(listing(&out), [name]);
}
