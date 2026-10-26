//! The GUI's state behind the commands: grants, operations, open sessions with their listing
//! view, the journal; and the end of the process (cancel every token, close every session,
//! join).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lpk_api::{
    CancelToken, CreateSettings, Filter, Globs, ListModel, Monitor, OpenOptions, Order, Root,
    RunSummary, Scope, Session, Sources, View,
};
use serde::Serialize;

use crate::grants::{Grant, GrantKind, Grants};
use crate::journal::{Journal, Recovered};
use crate::ops::Ops;
use crate::{batch, write, GuiError};

/// What `open_archive` returns to the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Opened {
    /// The session's handle (for `rows` and `close_archive`).
    pub session: u64,
    /// The archive's file name (never its folder).
    pub name: String,
    /// `lpk` or the foreign format's name.
    pub format: String,
    /// The generation open.
    pub generation: u64,
    /// Encrypted.
    pub encrypted: bool,
    /// Opened without the key.
    pub keyless: bool,
    /// The rows of the flat view.
    pub total: u64,
}

#[derive(Debug)]
struct Slot {
    session: Arc<Session>,
    name: String,
    view: Mutex<Option<(Arc<ListModel>, Arc<View>)>>,
}

/// The process's state.
#[derive(Debug)]
pub struct App {
    /// The grants.
    pub grants: Grants,
    /// The operations.
    pub ops: Arc<Ops>,
    /// The journal of interrupted writes (none when the state folder is unavailable).
    pub journal: Option<Journal>,
    sessions: Mutex<HashMap<u64, Arc<Slot>>>,
    next: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl App {
    /// The state, with the journal in `journal_dir` (if any).
    pub fn new(journal_dir: Option<&Path>) -> Self {
        App {
            grants: Grants::new(),
            ops: Ops::new(),
            journal: journal_dir.and_then(|d| Journal::open(d).ok()),
            sessions: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    /// The start-up recovery of interrupted writes (before any operation starts).
    pub fn recover(&self) -> Vec<Recovered> {
        self.journal
            .as_ref()
            .map(Journal::recover)
            .unwrap_or_default()
    }

    /// Open the archive of `grant`.
    pub fn open(&self, grant: &Grant, cancel: &CancelToken) -> Result<Opened, GuiError> {
        let path = self.grants.resolve(grant, GrantKind::Archive)?;
        let session = Arc::new(Session::open(&path, &OpenOptions::default())?);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let model = session.model(cancel)?;
        let view = Arc::new(model.view(Scope::All, Order::default(), &Filter::None, cancel)?);
        let info = session.info().clone();
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let total = view.len() as u64;
        lock(&self.sessions).insert(
            id,
            Arc::new(Slot {
                session,
                name: name.clone(),
                view: Mutex::new(Some((model, view))),
            }),
        );
        Ok(Opened {
            session: id,
            name,
            format: info.format,
            generation: info.generation,
            encrypted: info.encrypted,
            keyless: info.keyless,
            total,
        })
    }

    fn slot(&self, id: u64) -> Result<Arc<Slot>, GuiError> {
        lock(&self.sessions)
            .get(&id)
            .cloned()
            .ok_or_else(|| GuiError::usage(format!("no session {id}")))
    }

    /// The file name of session `id`'s archive.
    pub fn session_name(&self, id: u64) -> Result<String, GuiError> {
        Ok(self.slot(id)?.name.clone())
    }

    /// A batch (v1) of `count` rows of session `id`'s view from row `first`.
    pub fn rows(&self, id: u64, first: u32, count: u32) -> Result<Vec<u8>, GuiError> {
        let slot = self.slot(id)?;
        let g = lock(&slot.view);
        let (model, view) = g
            .as_ref()
            .ok_or_else(|| GuiError::usage("the session has no view"))?;
        let n = (count as usize).min(batch::MAX_ROWS);
        let rows = model.rows(view, first as usize, n);
        Ok(batch::encode(
            view.generation,
            view.len(),
            first as usize,
            &rows,
            true,
        ))
    }

    /// Close session `id` (its reads in flight end `Cancelled`).
    pub fn close(&self, id: u64) -> Result<(), GuiError> {
        let slot = lock(&self.sessions).remove(&id);
        if let Some(s) = slot {
            s.session.close()?;
        }
        Ok(())
    }

    /// Create the archive `name` (a plain file name) in the folder of `folder` from the inputs
    /// of `inputs` (a lone folder's contents, else each input under its name).
    pub fn create(
        &self,
        inputs: &[Grant],
        folder: &Grant,
        name: &str,
        settings: &CreateSettings,
        monitor: &Monitor,
    ) -> Result<RunSummary, GuiError> {
        let paths: Vec<PathBuf> = inputs
            .iter()
            .map(|g| self.grants.resolve(g, GrantKind::Input))
            .collect::<Result<_, _>>()?;
        if paths.is_empty() {
            return Err(GuiError::usage("no inputs"));
        }
        let out = self
            .grants
            .resolve(folder, GrantKind::Folder)?
            .join(write::archive_name(name)?);
        let sources = if paths.len() == 1 && paths[0].is_dir() {
            Sources {
                roots: vec![Root::contents(&paths[0])],
                exclude: Globs::all(),
            }
        } else {
            let core = |e| GuiError::from(lpk_api::Error::core(e, None));
            let roots = paths
                .iter()
                .map(|p| Root::named(p).map_err(core))
                .collect::<Result<Vec<_>, _>>()?;
            Sources::new(roots, Globs::all()).map_err(core)?
        };
        write::create_archive(&sources, &out, settings, monitor, self.journal.as_ref())
    }

    /// The end of the process: refuse new operations, cancel every token, close every session
    /// and wait (at most `timeout`) for the runs to finish their cleanup (D-87 (2)). True when
    /// every run ended.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        self.ops.cancel_all();
        let slots: Vec<Arc<Slot>> = lock(&self.sessions).drain().map(|(_, s)| s).collect();
        for s in slots {
            let _ = s.session.close();
        }
        self.ops.join(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lpk_api::Tier;

    fn archive(dir: &Path, files: usize) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for i in 0..files {
            std::fs::write(src.join(format!("file-{i:03}.txt")), format!("body {i}")).unwrap();
        }
        let out = dir.join("a.lpk");
        let s = CreateSettings {
            tier: Tier::Store,
            ..CreateSettings::default()
        };
        lpk_api::create(&Sources::contents_of(&src), &out, &s, &Monitor::default()).unwrap();
        out
    }

    #[test]
    fn open_and_page_an_archive_through_a_grant() {
        let d = tempfile::tempdir().unwrap();
        let a = archive(d.path(), 10);
        let app = App::new(None);
        let g = app.grants.mint(GrantKind::Archive, &a);
        let o = app.open(&g.grant, &CancelToken::new()).unwrap();
        assert_eq!(o.name, "a.lpk");
        assert_eq!(o.total, 10);
        let (total, first, rows) = batch::decode(&app.rows(o.session, 2, 5).unwrap()).unwrap();
        assert_eq!((total, first, rows.len()), (10, 2, 5));
        assert_eq!(rows[0].name, "file-002.txt");
        app.close(o.session).unwrap();
        assert!(app.rows(o.session, 0, 1).is_err());
    }

    #[test]
    fn an_input_grant_cannot_open_an_archive() {
        let d = tempfile::tempdir().unwrap();
        let a = archive(d.path(), 1);
        let app = App::new(None);
        let g = app.grants.mint(GrantKind::Input, &a);
        let e = app.open(&g.grant, &CancelToken::new()).unwrap_err();
        assert!(e.is(lpk_api::Class::Usage), "{e}");
    }

    #[test]
    fn create_through_grants_writes_into_the_granted_folder_only() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("in");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("x.txt"), b"x").unwrap();
        let out_dir = d.path().join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = App::new(Some(&d.path().join("journal")));
        let input = app.grants.mint(GrantKind::Input, &src).grant;
        let folder = app.grants.mint(GrantKind::Folder, &out_dir).grant;
        let s = CreateSettings {
            tier: Tier::Store,
            ..CreateSettings::default()
        };
        app.create(
            std::slice::from_ref(&input),
            &folder,
            "made",
            &s,
            &Monitor::default(),
        )
        .unwrap();
        assert!(out_dir.join("made.lpk").is_file());
        for bad in ["../escape", "..\\escape", "C:\\x"] {
            let e = app
                .create(
                    std::slice::from_ref(&input),
                    &folder,
                    bad,
                    &s,
                    &Monitor::default(),
                )
                .unwrap_err();
            assert!(e.is(lpk_api::Class::Usage), "{bad}: {e}");
        }
        // An input grant is not a folder grant.
        let e = app
            .create(
                std::slice::from_ref(&input),
                &input,
                "y",
                &s,
                &Monitor::default(),
            )
            .unwrap_err();
        assert!(e.is(lpk_api::Class::Usage), "{e}");
    }

    #[test]
    fn shutdown_cancels_a_running_create_and_joins_it() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("in");
        std::fs::create_dir_all(&src).unwrap();
        // Enough data that the run is still going when the exit comes.
        for i in 0..64 {
            let body: Vec<u8> = (0..(256 * 1024u32)).map(|x| (x ^ i) as u8).collect();
            std::fs::write(src.join(format!("{i}.bin")), body).unwrap();
        }
        let out_dir = d.path().join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let app = Arc::new(App::new(Some(&d.path().join("journal"))));
        let input = app.grants.mint(GrantKind::Input, &src).grant;
        let folder = app.grants.mint(GrantKind::Folder, &out_dir).grant;
        let guard = app.ops.begin(1, true).unwrap();
        let started = Arc::new(std::sync::Barrier::new(2));
        let (a2, s2) = (app.clone(), started.clone());
        let run = std::thread::spawn(move || {
            let first = std::sync::atomic::AtomicBool::new(false);
            let m = Monitor::new(
                Arc::new(move |_: &lpk_api::Event| {
                    if !first.swap(true, Ordering::SeqCst) {
                        s2.wait();
                    }
                }),
                guard.token.clone(),
            );
            let s = CreateSettings {
                tier: Tier::Fast,
                ..CreateSettings::default()
            };
            let r = a2.create(&[input], &folder, "big", &s, &m);
            drop(guard);
            r
        });
        started.wait();
        assert!(app.shutdown(Duration::from_secs(60)), "the run joined");
        let r = run.join().unwrap();
        // Cancelled mid-run (or, on a very fast machine, finished just before the token was
        // seen): either way nothing half-written is left, and no record.
        match r {
            Err(e) => {
                assert!(e.is(lpk_api::Class::Cancelled), "{e}");
                assert!(!out_dir.join("big.lpk").exists());
            }
            Ok(_) => assert!(out_dir.join("big.lpk").is_file()),
        }
        assert!(!out_dir.join("big.lpk.partial").exists());
        let journal = app.journal.as_ref().unwrap().dir();
        assert_eq!(std::fs::read_dir(journal).unwrap().count(), 0);
        assert!(
            app.ops.begin(2, false).is_err(),
            "no new work after the exit"
        );
    }
}
