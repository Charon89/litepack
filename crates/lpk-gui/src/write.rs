//! The GUI's writes (`docs/notes/e5-design.md` section 1): a create writes
//! `<name>.lpk.partial` beside the final name and publishes it with a move that never replaces
//! anything ([`lpk_gui_sys::publish_no_replace`]); an append is recorded with the generation
//! it started from. Both are in the [`Journal`] while they run, so that the next start can
//! clean up after a process that died mid-write.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lpk_api::{
    CreateSettings, Credentials, Event, Monitor, OpenOptions, RunSummary, Session, Sources,
};
use lpk_gui_sys::{file_identity, path_identity, publish_no_replace};

use crate::journal::{Identity, Journal, Record};
use crate::{io_error, GuiError};

/// `<out>.partial`.
pub fn partial_path(out: &Path) -> PathBuf {
    let mut s = OsString::from(out.as_os_str());
    s.push(".partial");
    PathBuf::from(s)
}

/// A user-typed archive name: a plain file name (no folder part, no device, no `..`), ending
/// `.lpk` (added when missing). Anything else is `Usage`.
pub fn archive_name(name: &str) -> Result<String, GuiError> {
    let n = name.trim();
    let bad = |why: &str| GuiError::usage(format!("archive name {name:?}: {why}"));
    if n.is_empty() || n == "." || n == ".." {
        return Err(bad("empty"));
    }
    if n.chars().any(|c| {
        c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return Err(bad("a file name only, without / \\ : * ? \" < > |"));
    }
    if n.ends_with('.') {
        return Err(bad("a name cannot end with a dot"));
    }
    let n = if n.to_ascii_lowercase().ends_with(".lpk") {
        n.to_string()
    } else {
        format!("{n}.lpk")
    };
    let stem = n[..n.len() - 4].to_ascii_uppercase();
    let stem = stem.split('.').next().unwrap_or("");
    let reserved = ["CON", "PRN", "AUX", "NUL"];
    let numbered = ["COM", "LPT"];
    if reserved.contains(&stem)
        || (stem.len() == 4
            && numbered.contains(&&stem[..3])
            && stem.as_bytes()[3].is_ascii_digit())
    {
        return Err(bad("a device name"));
    }
    Ok(n)
}

/// Create the archive `out` from `inputs` through `<out>.partial`: published when complete
/// (if that fails, for example because `out` appeared meanwhile, the finished partial is kept,
/// the error names it, and its journal record is removed so that no later start touches it),
/// removed by the engine on failure or cancel. `Exists` up front when `out` or its partial exists.
pub fn create_archive(
    inputs: &Sources,
    out: &Path,
    settings: &CreateSettings,
    monitor: &Monitor,
    journal: Option<&Journal>,
) -> Result<RunSummary, GuiError> {
    let partial = partial_path(out);
    for p in [out, partial.as_path()] {
        if p.symlink_metadata().is_ok() {
            return Err(lpk_api::Error::new(
                lpk_api::Class::Exists,
                "OutputExists",
                format!("{} exists", p.display()),
                Some(p),
            )
            .into());
        }
    }
    let entry = match journal {
        Some(j) => Some(Arc::new(Mutex::new((
            j.add(&Record::Partial {
                path: partial.clone(),
                identity: None,
            })
            .map_err(|e| io_error(e, j.dir()))?,
            false,
        )))),
        None => None,
    };
    // The engine creates the partial (`create_new`) before its first event: the first event
    // records the file's identity.
    let mut m = monitor.clone();
    if let Some(entry) = &entry {
        let inner = monitor.progress.clone();
        let entry = Arc::clone(entry);
        let partial_for = partial.clone();
        m.progress = Some(Arc::new(move |e: &Event| {
            if let Ok(mut g) = entry.lock() {
                // Tried again on every event until it succeeds.
                if !g.1 {
                    if let Ok(id) = path_identity(&partial_for) {
                        g.1 =
                            g.0.set(&Record::Partial {
                                path: partial_for.clone(),
                                identity: Some(Identity(id)),
                            })
                            .is_ok();
                    }
                }
            }
            if let Some(p) = &inner {
                p.event(e);
            }
        }));
    }
    let r = lpk_api::create(inputs, &partial, settings, &m);
    let result = match r {
        Ok(summary) => match publish(&partial, out) {
            Ok(()) => Ok(summary),
            Err(e) => {
                let class = match e.kind() {
                    std::io::ErrorKind::AlreadyExists => lpk_api::Class::Exists,
                    std::io::ErrorKind::PermissionDenied => lpk_api::Class::AccessDenied,
                    _ => lpk_api::Class::Io,
                };
                let error: GuiError = lpk_api::Error::new(
                    class,
                    "PublishFailed",
                    format!(
                        "{}: {e}; the finished archive is kept as {}",
                        out.display(),
                        partial.display()
                    ),
                    Some(&partial),
                )
                .into();
                // The finished archive is never deleted: the error names the partial, which is
                // now the user's file, so its record goes and no later start touches it (R97).
                drop(m);
                drop(entry);
                return Err(error);
            }
        },
        Err(e) => Err(e.into()),
    };
    // Drop the record (the last clone is the progress closure's, gone with `m`).
    drop(m);
    drop(entry);
    result
}

/// The attempts of [`publish`] while the partial is in use (a scanner or indexer holding it
/// open): about 2 s in all.
const PUBLISH_BACKOFF_MS: [u64; 8] = [10, 20, 40, 80, 160, 320, 500, 870];

/// [`publish_no_replace`], retried with a bounded backoff on a sharing violation or access
/// denied (transient on Windows while another process has the new file open).
fn publish(partial: &Path, out: &Path) -> std::io::Result<()> {
    let mut waits = PUBLISH_BACKOFF_MS.iter();
    loop {
        match publish_no_replace(partial, out) {
            Ok(()) => return Ok(()),
            Err(e) if transient(&e) => match waits.next() {
                Some(ms) => std::thread::sleep(std::time::Duration::from_millis(*ms)),
                None => return Err(e),
            },
            Err(e) => return Err(e),
        }
    }
}

/// A sharing violation (Windows error 32) or access denied.
fn transient(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
        || (cfg!(windows) && e.raw_os_error() == Some(32))
}

/// Append `dir` to the archive at `archive` as a new generation, recorded with the generation
/// it starts from (and the archive's identity) until it ends.
pub fn append_archive(
    archive: &Path,
    dir: &Path,
    settings: &CreateSettings,
    credentials: Option<&Credentials>,
    monitor: &Monitor,
    journal: Option<&Journal>,
) -> Result<RunSummary, GuiError> {
    let generation = {
        let s = Session::open(archive, &OpenOptions::default())?;
        let g = s.info().latest_generation;
        let _ = s.close();
        g
    };
    let identity = std::fs::File::open(archive)
        .and_then(|f| file_identity(&f))
        .map_err(|e| io_error(e, archive))?;
    let entry = match journal {
        Some(j) => Some(
            j.add(&Record::Append {
                path: archive.to_path_buf(),
                identity: Identity(identity),
                generation,
            })
            .map_err(|e| io_error(e, j.dir()))?,
        ),
        None => None,
    };
    let r = lpk_api::append(archive, dir, settings, credentials, monitor);
    drop(entry);
    Ok(r?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Recovered;
    use lpk_api::{CancelToken, Globs, Root, Tier};

    fn tree(dir: &Path, files: usize) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        for i in 0..files {
            let body: Vec<u8> = (0..2000u32).map(|x| (x * 31 + i as u32) as u8).collect();
            std::fs::write(src.join("sub").join(format!("f{i}.bin")), body).unwrap();
        }
        src
    }

    fn sources(src: &Path) -> Sources {
        Sources {
            roots: vec![Root::contents(src)],
            exclude: Globs::all(),
        }
    }

    fn store() -> CreateSettings {
        CreateSettings {
            tier: Tier::Store,
            ..CreateSettings::default()
        }
    }

    #[test]
    fn create_publishes_the_partial_and_leaves_no_record() {
        let d = tempfile::tempdir().unwrap();
        let src = tree(d.path(), 3);
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let out = d.path().join("a.lpk");
        let seen = Arc::new(Mutex::new(false));
        let s2 = seen.clone();
        let m = Monitor::new(
            Arc::new(move |_: &Event| *s2.lock().unwrap() = true),
            CancelToken::new(),
        );
        create_archive(&sources(&src), &out, &store(), &m, Some(&j)).unwrap();
        assert!(out.is_file());
        assert!(!partial_path(&out).exists());
        assert!(
            *seen.lock().unwrap(),
            "the caller's observer still sees events"
        );
        assert_eq!(std::fs::read_dir(j.dir()).unwrap().count(), 0);
        let s = Session::open(&out, &OpenOptions::default()).unwrap();
        let mut n = 0;
        s.for_each(&lpk_api::Selection::All, &mut |_| {
            n += 1;
            std::ops::ControlFlow::Continue(())
        })
        .unwrap();
        assert!(n >= 3);
    }

    #[test]
    fn create_never_replaces_an_existing_archive() {
        let d = tempfile::tempdir().unwrap();
        let src = tree(d.path(), 1);
        let out = d.path().join("a.lpk");
        std::fs::write(&out, b"mine").unwrap();
        let e =
            create_archive(&sources(&src), &out, &store(), &Monitor::default(), None).unwrap_err();
        assert!(e.is(lpk_api::Class::Exists), "{e}");
        assert_eq!(std::fs::read(&out).unwrap(), b"mine");
        // A leftover partial of the same name is not overwritten either.
        let out2 = d.path().join("b.lpk");
        std::fs::write(partial_path(&out2), b"other").unwrap();
        let e =
            create_archive(&sources(&src), &out2, &store(), &Monitor::default(), None).unwrap_err();
        assert!(e.is(lpk_api::Class::Exists), "{e}");
        assert_eq!(std::fs::read(partial_path(&out2)).unwrap(), b"other");
    }

    #[test]
    fn a_failed_publish_keeps_the_finished_partial_across_restarts() {
        // The final name appears while the archive is being written: the publish refuses to
        // replace it, and the finished archive stays as the partial.
        let d = tempfile::tempdir().unwrap();
        let src = tree(d.path(), 2);
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let out = d.path().join("a.lpk");
        let out2 = out.clone();
        let m = Monitor::new(
            Arc::new(move |_: &Event| {
                if !out2.exists() {
                    std::fs::write(&out2, b"the user's file").unwrap();
                }
            }),
            CancelToken::new(),
        );
        let e = create_archive(&sources(&src), &out, &store(), &m, Some(&j)).unwrap_err();
        assert!(e.is(lpk_api::Class::Exists), "{e}");
        let partial = partial_path(&out);
        assert!(e.detail.contains(&partial.display().to_string()), "{e}");
        assert_eq!(std::fs::read(&out).unwrap(), b"the user's file");
        let s = Session::open(&partial, &OpenOptions::default()).unwrap();
        assert_eq!(s.info().latest_generation, 0);
        drop(s);
        let records = std::fs::read_dir(j.dir())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count();
        assert_eq!(
            records, 0,
            "the record goes once the error names the partial"
        );
        // A new start's recovery leaves the partial alone, byte for byte.
        let bytes = std::fs::read(&partial).unwrap();
        assert!(Journal::open(j.dir()).unwrap().recover().is_empty());
        assert_eq!(std::fs::read(&partial).unwrap(), bytes);
    }

    #[test]
    fn a_cancelled_create_leaves_nothing() {
        let d = tempfile::tempdir().unwrap();
        let src = tree(d.path(), 2);
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let out = d.path().join("a.lpk");
        let t = CancelToken::new();
        t.cancel();
        let e = create_archive(
            &sources(&src),
            &out,
            &store(),
            &Monitor::with_cancel(t),
            Some(&j),
        )
        .unwrap_err();
        assert!(e.is(lpk_api::Class::Cancelled), "{e}");
        assert!(!out.exists() && !partial_path(&out).exists());
        assert_eq!(std::fs::read_dir(j.dir()).unwrap().count(), 0);
    }

    #[test]
    fn the_next_start_deletes_a_leftover_partial_only_if_it_is_the_same_file() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        // A process died mid-create: its partial and its record are left.
        let ours = d.path().join("a.lpk.partial");
        std::fs::write(&ours, b"half an archive").unwrap();
        let id = Identity(path_identity(&ours).unwrap());
        j.add(&Record::Partial {
            path: ours.clone(),
            identity: Some(id),
        })
        .unwrap()
        .keep();
        // Another partial was recorded, then replaced by a different file of the same name.
        let replaced = d.path().join("b.lpk.partial");
        std::fs::write(&replaced, b"x").unwrap();
        let old = Identity(path_identity(&replaced).unwrap());
        std::fs::rename(&replaced, d.path().join("kept")).unwrap();
        std::fs::write(&replaced, b"the user's own file").unwrap();
        j.add(&Record::Partial {
            path: replaced.clone(),
            identity: Some(old),
        })
        .unwrap()
        .keep();

        let next = Journal::open(j.dir()).unwrap();
        let mut got = next.recover();
        got.sort_by_key(|r| format!("{r:?}"));
        assert_eq!(
            got,
            [
                Recovered::DeletedPartial(ours.clone()),
                Recovered::Left {
                    path: replaced.clone(),
                    reason: "another file now"
                },
            ]
        );
        assert!(!ours.exists());
        assert_eq!(std::fs::read(&replaced).unwrap(), b"the user's own file");
        assert_eq!(std::fs::read_dir(j.dir()).unwrap().count(), 0);
    }

    #[test]
    fn a_running_create_records_its_partials_identity() {
        // From the first event on, the record names the partial and its identity: what the
        // next start needs if this process dies now.
        let d = tempfile::tempdir().unwrap();
        let src = tree(d.path(), 2);
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let out = d.path().join("a.lpk");
        let dir = j.dir().to_path_buf();
        let seen = Arc::new(Mutex::new(None));
        let s2 = seen.clone();
        let m = Monitor::new(
            Arc::new(move |_: &Event| {
                let mut s = s2.lock().unwrap();
                if s.is_none() {
                    let json = std::fs::read_dir(&dir)
                        .unwrap()
                        .flatten()
                        .map(|e| e.path())
                        .find(|p| p.extension().is_some_and(|x| x == "json"))
                        .unwrap();
                    let r: Record = serde_json::from_slice(&std::fs::read(json).unwrap()).unwrap();
                    *s = Some(r);
                }
            }),
            CancelToken::new(),
        );
        let partial = partial_path(&out);
        create_archive(&sources(&src), &out, &store(), &m, Some(&j)).unwrap();
        let first = seen.lock().unwrap().take().unwrap();
        match first {
            Record::Partial { path, identity } => {
                assert_eq!(path, partial);
                // The partial was published (moved), so its identity is now the archive's.
                assert_eq!(identity, Some(Identity(path_identity(&out).unwrap())));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_next_start_rolls_an_interrupted_append_back_and_keeps_a_committed_one() {
        let d = tempfile::tempdir().unwrap();
        let src = tree(d.path(), 2);
        let more = d.path().join("more");
        std::fs::create_dir_all(&more).unwrap();
        std::fs::write(more.join("new.bin"), vec![7u8; 5000]).unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();

        let a = d.path().join("a.lpk");
        create_archive(&sources(&src), &a, &store(), &Monitor::default(), None).unwrap();
        let gen0_len = std::fs::metadata(&a).unwrap().len();
        append_archive(&a, &more, &store(), None, &Monitor::default(), Some(&j)).unwrap();
        assert_eq!(std::fs::read_dir(j.dir()).unwrap().count(), 0);
        let gen1_len = std::fs::metadata(&a).unwrap().len();
        assert!(gen1_len > gen0_len);

        // Interrupted: the second generation half written, the record left.
        let f = std::fs::OpenOptions::new().write(true).open(&a).unwrap();
        f.set_len(gen0_len + (gen1_len - gen0_len) / 2).unwrap();
        let id = Identity(file_identity(&f).unwrap());
        drop(f);
        j.add(&Record::Append {
            path: a.clone(),
            identity: id,
            generation: 0,
        })
        .unwrap()
        .keep();

        // Committed: a full append whose record was never removed.
        let b = d.path().join("b.lpk");
        create_archive(&sources(&src), &b, &store(), &Monitor::default(), None).unwrap();
        append_archive(&b, &more, &store(), None, &Monitor::default(), None).unwrap();
        let b_bytes = std::fs::read(&b).unwrap();
        j.add(&Record::Append {
            path: b.clone(),
            identity: Identity(path_identity(&b).unwrap()),
            generation: 0,
        })
        .unwrap()
        .keep();

        // Another file at the recorded path: untouched.
        let c = d.path().join("c.lpk");
        create_archive(&sources(&src), &c, &store(), &Monitor::default(), None).unwrap();
        let c_len = std::fs::metadata(&c).unwrap().len();
        j.add(&Record::Append {
            path: c.clone(),
            identity: Identity(lpk_gui_sys::FileIdentity {
                volume: 1,
                file_id: 2,
            }),
            generation: 0,
        })
        .unwrap()
        .keep();

        let mut got = Journal::open(j.dir()).unwrap().recover();
        got.sort_by_key(|r| format!("{r:?}"));
        let mut want = vec![
            Recovered::RolledBack {
                path: a.clone(),
                generation: 0,
            },
            Recovered::Committed(b.clone()),
            Recovered::Left {
                path: c.clone(),
                reason: "another file now",
            },
        ];
        want.sort_by_key(|r| format!("{r:?}"));
        assert_eq!(got, want);
        assert_eq!(std::fs::metadata(&a).unwrap().len(), gen0_len);
        assert_eq!(
            std::fs::read(&b).unwrap(),
            b_bytes,
            "a committed append is untouched"
        );
        assert_eq!(std::fs::metadata(&c).unwrap().len(), c_len);
        // The rolled-back archive opens at generation 0.
        let s = Session::open(&a, &OpenOptions::default()).unwrap();
        assert_eq!(s.info().latest_generation, 0);
    }

    #[test]
    fn a_later_generation_survives_the_recovery_of_a_stale_append_record() {
        // The GUI died after its append (generation 1) committed but before it removed the
        // record; the user then appended generation 2 with `lpk`. The next start must keep both.
        let d = tempfile::tempdir().unwrap();
        let src = tree(d.path(), 2);
        let more = d.path().join("more");
        std::fs::create_dir_all(&more).unwrap();
        std::fs::write(more.join("one.bin"), vec![1u8; 3000]).unwrap();
        let more2 = d.path().join("more2");
        std::fs::create_dir_all(&more2).unwrap();
        std::fs::write(more2.join("two.bin"), vec![2u8; 3000]).unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let a = d.path().join("a.lpk");
        create_archive(&sources(&src), &a, &store(), &Monitor::default(), None).unwrap();
        append_archive(&a, &more, &store(), None, &Monitor::default(), None).unwrap();
        append_archive(&a, &more2, &store(), None, &Monitor::default(), None).unwrap();
        let before = std::fs::read(&a).unwrap();
        j.add(&Record::Append {
            path: a.clone(),
            identity: Identity(path_identity(&a).unwrap()),
            generation: 0,
        })
        .unwrap()
        .keep();
        let got = Journal::open(j.dir()).unwrap().recover();
        assert_eq!(got, [Recovered::Committed(a.clone())]);
        assert_eq!(std::fs::read(&a).unwrap(), before, "byte for byte");
        let s = Session::open(&a, &OpenOptions::default()).unwrap();
        assert_eq!(s.info().latest_generation, 2);
    }

    fn records(j: &Journal) -> usize {
        std::fs::read_dir(j.dir())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count()
    }

    /// An archive of two generations with a stale record naming generation 0.
    fn two_generations(d: &Path, j: &Journal) -> (PathBuf, u64) {
        let src = tree(d, 2);
        let more = d.join("more");
        std::fs::create_dir_all(&more).unwrap();
        std::fs::write(more.join("one.bin"), vec![1u8; 3000]).unwrap();
        let a = d.join("a.lpk");
        create_archive(&sources(&src), &a, &store(), &Monitor::default(), None).unwrap();
        let gen0_len = std::fs::metadata(&a).unwrap().len();
        append_archive(&a, &more, &store(), None, &Monitor::default(), None).unwrap();
        j.add(&Record::Append {
            path: a.clone(),
            identity: Identity(path_identity(&a).unwrap()),
            generation: 0,
        })
        .unwrap()
        .keep();
        (a, gen0_len)
    }

    #[test]
    fn a_kept_record_is_given_up_with_a_notice_after_its_tries() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let (a, gen0_len) = two_generations(d.path(), &j);
        let mut bytes = std::fs::read(&a).unwrap();
        bytes[gen0_len as usize - 40] ^= 0xFF;
        std::fs::write(&a, &bytes).unwrap();
        for start in 1..crate::journal::MAX_TRIES {
            let got = Journal::open(j.dir()).unwrap().recover();
            assert!(
                matches!(&got[..], [Recovered::Left { reason, .. }] if *reason == crate::journal::CHAIN_UNREADABLE),
                "start {start}: {got:?}"
            );
            assert_eq!(records(&j), 1, "start {start}: kept");
        }
        let got = Journal::open(j.dir()).unwrap().recover();
        assert_eq!(
            got,
            [Recovered::GaveUp {
                path: a.clone(),
                reason: crate::journal::CHAIN_UNREADABLE,
                tries: crate::journal::MAX_TRIES,
            }]
        );
        assert_eq!(
            std::fs::read_dir(j.dir()).unwrap().count(),
            0,
            "the record, its lock and its count are gone"
        );
        assert_eq!(std::fs::read(&a).unwrap(), bytes, "the file is untouched");
        assert!(Journal::open(j.dir()).unwrap().recover().is_empty());
    }

    /// A torn append (generation 1 cut in half) recorded at generation 0.
    fn torn_append(d: &Path, j: &Journal) -> (PathBuf, u64) {
        let src = tree(d, 2);
        let more = d.join("more");
        std::fs::create_dir_all(&more).unwrap();
        std::fs::write(more.join("new.bin"), vec![7u8; 5000]).unwrap();
        let a = d.join("torn.lpk");
        create_archive(&sources(&src), &a, &store(), &Monitor::default(), None).unwrap();
        let gen0_len = std::fs::metadata(&a).unwrap().len();
        append_archive(&a, &more, &store(), None, &Monitor::default(), None).unwrap();
        let gen1_len = std::fs::metadata(&a).unwrap().len();
        let f = std::fs::OpenOptions::new().write(true).open(&a).unwrap();
        f.set_len(gen0_len + (gen1_len - gen0_len) / 2).unwrap();
        drop(f);
        j.add(&Record::Append {
            path: a.clone(),
            identity: Identity(path_identity(&a).unwrap()),
            generation: 0,
        })
        .unwrap()
        .keep();
        (a, gen0_len)
    }

    #[cfg(windows)]
    #[test]
    fn a_held_file_is_never_given_up_and_is_noticed_from_the_last_try() {
        use crate::journal::{startup_report, Notice};
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let (a, gen0_len) = torn_append(d.path(), &j);
        let writer = std::fs::OpenOptions::new().write(true).open(&a).unwrap();
        for start in 1..=crate::journal::MAX_TRIES + 2 {
            let got = Journal::open(j.dir()).unwrap().recover();
            if start < crate::journal::MAX_TRIES {
                assert!(
                    matches!(&got[..], [Recovered::Left { reason, .. }] if *reason == crate::journal::UNREADABLE),
                    "start {start}: {got:?}"
                );
                assert_eq!(startup_report(&got).notices, [], "start {start}");
            } else {
                assert_eq!(
                    got,
                    [Recovered::Held {
                        path: a.clone(),
                        tries: start
                    }],
                    "start {start}"
                );
                assert_eq!(
                    startup_report(&got).notices,
                    [Notice::Held {
                        name: "torn.lpk".into(),
                        tries: start
                    }]
                );
            }
            assert_eq!(records(&j), 1, "start {start}: kept");
            assert_eq!(startup_report(&got).recovered, 0);
        }
        drop(writer);
        let got = Journal::open(j.dir()).unwrap().recover();
        assert!(
            matches!(&got[..], [Recovered::RolledBack { .. }]),
            "{got:?}"
        );
        assert_eq!(startup_report(&got).recovered, 1);
        assert_eq!(std::fs::metadata(&a).unwrap().len(), gen0_len);
        assert_eq!(std::fs::read_dir(j.dir()).unwrap().count(), 0);
    }

    #[test]
    fn a_failed_recovery_keeps_its_record_and_only_settled_ones_count() {
        use crate::journal::{startup_report, Notice};
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        // A partial that cannot be deleted. Windows: another handle holds it without sharing
        // delete. Unix: a folder at the recorded path (remove_file fails on a folder).
        let p = d.path().join("x.lpk.partial");
        #[cfg(windows)]
        let _holder = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::write(&p, b"partial").unwrap();
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1 | 2) // read and write, not delete
                .open(&p)
                .unwrap()
        };
        #[cfg(not(windows))]
        std::fs::create_dir_all(p.join("inner")).unwrap();
        let id = Identity(path_identity(&p).unwrap());
        j.add(&Record::Partial {
            path: p.clone(),
            identity: Some(id),
        })
        .unwrap()
        .keep();
        // Gone, and another file: decided, but not recovered.
        j.add(&Record::Partial {
            path: d.path().join("gone.lpk.partial"),
            identity: Some(id),
        })
        .unwrap()
        .keep();
        for start in 1..=2 {
            let got = Journal::open(j.dir()).unwrap().recover();
            assert!(
                got.iter()
                    .any(|r| matches!(r, Recovered::Failed { path, .. } if *path == p)),
                "start {start}: {got:?}"
            );
            let report = startup_report(&got);
            assert_eq!(report.recovered, 0, "start {start}: {got:?}");
            assert!(
                matches!(&report.notices[..], [Notice::Failed { name, .. }] if name == "x.lpk.partial"),
                "start {start}: {report:?}"
            );
            assert_eq!(records(&j), 1, "start {start}: the failed record stays");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_torn_append_another_writer_holds_is_not_cut() {
        // The check and the cut go through one handle that admits no other writer: a writer
        // holding the file (sharing read and write, std's default) makes the recovery leave it.
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let (a, gen0_len) = torn_append(d.path(), &j);
        let before = std::fs::read(&a).unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&a).unwrap();
        let got = Journal::open(j.dir()).unwrap().recover();
        assert!(
            matches!(&got[..], [Recovered::Left { reason, .. }] if *reason == crate::journal::UNREADABLE),
            "{got:?}"
        );
        assert_eq!(std::fs::read(&a).unwrap(), before, "byte for byte");
        assert_eq!(records(&j), 1);
        drop(writer);
        let got = Journal::open(j.dir()).unwrap().recover();
        assert_eq!(
            got,
            [Recovered::RolledBack {
                path: a.clone(),
                generation: 0
            }]
        );
        assert_eq!(std::fs::metadata(&a).unwrap().len(), gen0_len);
    }

    #[test]
    fn a_torn_append_replaced_by_another_file_is_not_cut() {
        // The identity is read through the handle that cuts: a file moved over the recorded
        // path is another file, left as it is.
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let (a, _) = torn_append(d.path(), &j);
        let copy = d.path().join("copy.lpk");
        std::fs::copy(&a, &copy).unwrap();
        std::fs::rename(&a, d.path().join("moved.lpk")).unwrap();
        std::fs::rename(&copy, &a).unwrap();
        let before = std::fs::read(&a).unwrap();
        let got = Journal::open(j.dir()).unwrap().recover();
        assert_eq!(
            got,
            [Recovered::Left {
                path: a.clone(),
                reason: "another file now"
            }]
        );
        assert_eq!(std::fs::read(&a).unwrap(), before);
    }

    #[test]
    fn a_damaged_chain_is_left_untouched_with_its_record() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let (a, gen0_len) = two_generations(d.path(), &j);
        // Damage generation 0's trailer: the tail (generation 1) still reads, the chain does not.
        let mut bytes = std::fs::read(&a).unwrap();
        bytes[gen0_len as usize - 40] ^= 0xFF;
        std::fs::write(&a, &bytes).unwrap();
        let got = Journal::open(j.dir()).unwrap().recover();
        assert!(
            matches!(&got[..], [Recovered::Left { reason, .. }] if *reason == crate::journal::CHAIN_UNREADABLE),
            "{got:?}"
        );
        assert_eq!(std::fs::read(&a).unwrap(), bytes, "byte for byte");
        assert_eq!(records(&j), 1, "the record stays for a later start");
    }

    #[cfg(windows)]
    #[test]
    fn a_held_archive_is_left_untouched_with_its_record() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let (a, _) = two_generations(d.path(), &j);
        let before = std::fs::read(&a).unwrap();
        // Another process holds it open with no sharing: it cannot even be opened.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&a)
            .unwrap();
        let got = Journal::open(j.dir()).unwrap().recover();
        assert!(
            matches!(&got[..], [Recovered::Left { reason, .. }] if *reason == crate::journal::UNREADABLE),
            "{got:?}"
        );
        assert_eq!(records(&j), 1, "the record stays for a later start");
        drop(held);
        assert_eq!(std::fs::read(&a).unwrap(), before, "byte for byte");
        // Once released, a later start decides: the append had committed.
        let got = Journal::open(j.dir()).unwrap().recover();
        assert_eq!(got, [Recovered::Committed(a.clone())]);
        assert_eq!(records(&j), 0);
    }

    #[cfg(unix)]
    #[test]
    fn an_unopenable_archive_is_left_untouched_with_its_record() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(&d.path().join("journal")).unwrap();
        let (a, _) = two_generations(d.path(), &j);
        let before = std::fs::read(&a).unwrap();
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&a).is_ok() {
            // Running as root: permissions do not stop it; nothing to show here.
            std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }
        let got = Journal::open(j.dir()).unwrap().recover();
        assert!(
            matches!(&got[..], [Recovered::Left { reason, .. }] if *reason == crate::journal::UNREADABLE),
            "{got:?}"
        );
        assert_eq!(records(&j), 1, "the record stays for a later start");
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), before, "byte for byte");
    }

    #[test]
    fn archive_names_are_plain_file_names() {
        assert_eq!(archive_name("photos").unwrap(), "photos.lpk");
        assert_eq!(archive_name("a.LPK").unwrap(), "a.LPK");
        for bad in [
            "", "..", "a/b", "a\\b", "C:x", "x.", "con", "LPT1.lpk", "a\u{1}",
        ] {
            assert!(archive_name(bad).is_err(), "{bad:?}");
        }
    }
}
