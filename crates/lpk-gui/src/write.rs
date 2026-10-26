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
/// (failing with `Exists`, the partial removed, if `out` appeared meanwhile), removed by the
/// engine on failure or cancel. `Exists` up front when `out` or its partial exists.
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
                if !g.1 {
                    g.1 = true;
                    if let Ok(id) = path_identity(&partial_for) {
                        let _ = g.0.set(&Record::Partial {
                            path: partial_for.clone(),
                            identity: Some(Identity(id)),
                        });
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
        Ok(summary) => match publish_no_replace(&partial, out) {
            Ok(()) => Ok(summary),
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
                Err(io_error(e, out))
            }
        },
        Err(e) => Err(e.into()),
    };
    // Drop the record (the last clone is the progress closure's, gone with `m`).
    drop(m);
    drop(entry);
    result
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
        .abandon();
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
        .abandon();

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
        .abandon();

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
        .abandon();

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
        .abandon();

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
        .abandon();
        let got = Journal::open(j.dir()).unwrap().recover();
        assert_eq!(got, [Recovered::Committed(a.clone())]);
        assert_eq!(std::fs::read(&a).unwrap(), before, "byte for byte");
        let s = Session::open(&a, &OpenOptions::default()).unwrap();
        assert_eq!(s.info().latest_generation, 2);
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
