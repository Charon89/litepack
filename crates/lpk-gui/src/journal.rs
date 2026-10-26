//! Interrupted writes (`docs/notes/e5-design.md` section 1). Every `<name>.lpk.partial` a
//! create writes and every running append is recorded in a file under the state folder
//! (`%LOCALAPPDATA%\LitePack\journal`) with the file's identity (volume serial and file ID).
//! The record is removed when the write ends, published or cleaned up by the engine. A record
//! left behind means the process died mid-write (a crash, a forced log-off: tao ignores
//! `WM_QUERYENDSESSION`); the next start then deletes the leftover partial, or rolls the
//! interrupted append back to its last complete generation (D-59), and only if the file at the
//! recorded path still has the recorded identity.
//!
//! The process that writes a record holds an exclusive lock on a lock file beside it until it
//! removes both,
//! so a second instance's recovery skips the records of a live one.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lpk_gui_sys::{path_identity, FileIdentity};
use serde::{Deserialize, Serialize};

/// A file's identity as recorded (the file ID in hex: JSON numbers stop at 2^53).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity(pub FileIdentity);

impl Serialize for Identity {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        format!("{:016x}:{:032x}", self.0.volume, self.0.file_id).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Identity {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bad = || serde::de::Error::custom("an identity is <volume hex>:<file id hex>");
        let (v, f) = s.split_once(':').ok_or_else(bad)?;
        Ok(Identity(FileIdentity {
            volume: u64::from_str_radix(v, 16).map_err(|_| bad())?,
            file_id: u128::from_str_radix(f, 16).map_err(|_| bad())?,
        }))
    }
}

/// One write in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Record {
    /// A create writing `path` (a `.partial`); the identity once the file exists.
    Partial {
        /// The partial file.
        path: PathBuf,
        /// Its identity, recorded once the engine created it.
        identity: Option<Identity>,
    },
    /// An append to the archive at `path`, whose last complete generation was `generation`.
    Append {
        /// The archive.
        path: PathBuf,
        /// Its identity.
        identity: Identity,
        /// The generation it ended with before the append.
        generation: u64,
    },
}

/// The journal folder.
#[derive(Debug)]
pub struct Journal {
    dir: PathBuf,
    next: AtomicU64,
}

/// A record on disk (`<id>.json`) with its lock file (`<id>.lock`), which this process holds
/// locked until [`Journal::remove`] (or drop) removes both.
#[derive(Debug)]
pub struct Entry {
    json: PathBuf,
    lock_path: PathBuf,
    lock: Option<File>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(f) = self.lock.take() {
            let _ = std::fs::remove_file(&self.json);
            let _ = f.unlock();
            drop(f);
            let _ = std::fs::remove_file(&self.lock_path);
        }
    }
}

impl Entry {
    /// Replace the record with `r` (written beside, then renamed over: never half a record).
    pub fn set(&mut self, r: &Record) -> std::io::Result<()> {
        let text = serde_json::to_vec(r).map_err(std::io::Error::other)?;
        let tmp = self.json.with_extension("tmp");
        let mut f = File::create(&tmp)?;
        f.write_all(&text)?;
        f.sync_data()?;
        drop(f);
        std::fs::rename(&tmp, &self.json)
    }

    /// Leave the record on disk, its lock released: what a process killed mid-write leaves.
    #[cfg(test)]
    pub(crate) fn keep(mut self) {
        if let Some(f) = self.lock.take() {
            let _ = f.unlock();
        }
    }
}

/// What the start-up recovery did with one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered {
    /// The leftover partial file was deleted.
    DeletedPartial(PathBuf),
    /// The interrupted append was cut back to `generation`.
    RolledBack {
        /// The archive.
        path: PathBuf,
        /// The generation it ends with now.
        generation: u64,
    },
    /// The append had committed (its process died before removing the record): nothing done.
    Committed(PathBuf),
    /// Nothing done: the file is gone, or is another file now, or its identity was never
    /// recorded.
    Left {
        /// The path recorded.
        path: PathBuf,
        /// Why.
        reason: &'static str,
    },
    /// The recovery failed (the record is dropped; the file is left as it is).
    Failed {
        /// The path recorded.
        path: PathBuf,
        /// The error.
        error: String,
    },
}

impl Journal {
    /// The journal in `dir` (created if missing).
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Journal {
            dir: dir.to_path_buf(),
            next: AtomicU64::new(0),
        })
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Record `r`; the record lasts until the returned entry is removed or dropped.
    pub fn add(&self, r: &Record) -> std::io::Result<Entry> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let stem = format!("{nanos}-{}-{n}", std::process::id());
        let lock_path = self.dir.join(format!("{stem}.lock"));
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)?;
        lock.lock()?;
        let mut entry = Entry {
            json: self.dir.join(format!("{stem}.json")),
            lock_path,
            lock: Some(lock),
        };
        entry.set(r)?;
        Ok(entry)
    }

    /// Replace the record of `entry` with `r`.
    pub fn update(&self, entry: &mut Entry, r: &Record) -> std::io::Result<()> {
        entry.set(r)
    }

    /// The write ended: remove its record.
    pub fn remove(&self, entry: Entry) {
        drop(entry);
    }

    /// The start-up recovery: act on every record no live process holds, then remove it.
    pub fn recover(&self) -> Vec<Recovered> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return out;
        };
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        paths.sort();
        for p in paths {
            let lock_path = p.with_extension("lock");
            let lock = match OpenOptions::new().write(true).open(&lock_path) {
                Ok(f) => {
                    // Held by a live process (another instance mid-write): not ours to touch.
                    if f.try_lock().is_err() {
                        continue;
                    }
                    Some(f)
                }
                Err(_) => None,
            };
            let record = std::fs::read(&p)
                .ok()
                .and_then(|text| serde_json::from_slice::<Record>(&text).ok());
            let mut keep = false;
            if let Some(r) = record {
                let done = recover_one(&r);
                keep = keeps_record(&done);
                out.push(done);
            }
            if !keep {
                let _ = std::fs::remove_file(&p);
            }
            if let Some(f) = lock {
                let _ = f.unlock();
                drop(f);
                let _ = std::fs::remove_file(&lock_path);
            }
        }
        out
    }
}

/// Why a record is left on disk for a later start: the file could not be read (held by another
/// process), its trailer chain could not be read, or it has fewer generations than recorded.
pub(crate) const UNREADABLE: &str = "unreadable";
pub(crate) const CHAIN_UNREADABLE: &str = "trailer chain unreadable";
pub(crate) const BEHIND: &str = "fewer generations than recorded";

/// Whether the record of `r` stays on disk (nothing was decided about its file).
fn keeps_record(r: &Recovered) -> bool {
    matches!(r, Recovered::Left { reason, .. } if [UNREADABLE, CHAIN_UNREADABLE, BEHIND].contains(reason))
}

fn same_file(path: &Path, id: &Identity) -> Result<bool, &'static str> {
    match path_identity(path) {
        Ok(now) => Ok(now == id.0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("gone"),
        Err(_) => Err(UNREADABLE),
    }
}

fn recover_one(r: &Record) -> Recovered {
    match r {
        Record::Partial { path, identity } => {
            let Some(id) = identity else {
                return Recovered::Left {
                    path: path.clone(),
                    reason: "no identity recorded",
                };
            };
            match same_file(path, id) {
                Ok(true) => match std::fs::remove_file(path) {
                    Ok(()) => Recovered::DeletedPartial(path.clone()),
                    Err(e) => Recovered::Failed {
                        path: path.clone(),
                        error: e.to_string(),
                    },
                },
                Ok(false) => Recovered::Left {
                    path: path.clone(),
                    reason: "another file now",
                },
                Err(reason) => Recovered::Left {
                    path: path.clone(),
                    reason,
                },
            }
        }
        Record::Append {
            path,
            identity,
            generation,
        } => match same_file(path, identity) {
            Ok(true) => {
                // The latest committed generation, from the trailer chain `rollback` reads.
                // Past the recorded one: the append committed (and maybe later generations,
                // appended after this process died): nothing is written. Equal: the append
                // never committed and at most a torn tail follows: cut back to it. Anything
                // else (the chain unreadable, an unsupported version, fewer generations than
                // recorded): nothing is touched and the record stays for a later start.
                match lpk_api::committed_generation(path) {
                    Ok(g) if g > *generation => Recovered::Committed(path.clone()),
                    Ok(g) if g == *generation => match lpk_api::rollback(path, *generation) {
                        Ok(g) => Recovered::RolledBack {
                            path: path.clone(),
                            generation: g.generation,
                        },
                        Err(e) => Recovered::Failed {
                            path: path.clone(),
                            error: e.to_string(),
                        },
                    },
                    Ok(_) => Recovered::Left {
                        path: path.clone(),
                        reason: BEHIND,
                    },
                    Err(_) => Recovered::Left {
                        path: path.clone(),
                        reason: CHAIN_UNREADABLE,
                    },
                }
            }
            Ok(false) => Recovered::Left {
                path: path.clone(),
                reason: "another file now",
            },
            Err(reason) => Recovered::Left {
                path: path.clone(),
                reason,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_and_is_removed_with_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("journal")).unwrap();
        let r = Record::Append {
            path: dir.path().join("a.lpk"),
            identity: Identity(FileIdentity {
                volume: u64::MAX,
                file_id: u128::MAX - 1,
            }),
            generation: 3,
        };
        let e = j.add(&r).unwrap();
        assert_eq!(
            std::fs::read_dir(j.dir()).unwrap().count(),
            2,
            "the record and its lock"
        );
        // A live record is locked: the recovery of another instance skips it.
        let live = Journal::open(j.dir()).unwrap();
        assert!(live.recover().is_empty());
        assert_eq!(std::fs::read_dir(j.dir()).unwrap().count(), 2);
        j.remove(e);
        assert_eq!(std::fs::read_dir(j.dir()).unwrap().count(), 0);
        let back: Record = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }
}
