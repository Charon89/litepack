//! An in-memory registry for tests (shared by `register_tests.rs` and `shell_setting.rs`'s): no
//! test reaches the real one.

use std::collections::BTreeMap;
use std::io;

use crate::register::{RegValue, Registry};

pub(crate) type Values = BTreeMap<String, (String, RegValue)>;

/// An in-memory registry: case-insensitive like the real one, with an optional injected failure
/// after N mutating calls.
#[derive(Debug, Default, Clone)]
pub(crate) struct Fake {
    pub(crate) keys: BTreeMap<String, Values>,
    /// Mutating calls so far.
    pub(crate) writes: usize,
    pub(crate) fail_at: Option<usize>,
    pub(crate) notifies: usize,
}

pub(crate) type Snapshot = Vec<(String, Vec<(String, RegValue)>)>;

impl Fake {
    fn tick(&mut self) -> io::Result<()> {
        self.writes += 1;
        if self.fail_at.is_some_and(|n| self.writes > n) {
            return Err(io::Error::other("injected"));
        }
        Ok(())
    }

    fn add(&mut self, path: &str) {
        let mut prefix = String::new();
        for part in path.split('\\') {
            if !prefix.is_empty() {
                prefix.push('\\');
            }
            prefix.push_str(&part.to_lowercase());
            self.keys.entry(prefix.clone()).or_default();
        }
    }

    pub(crate) fn with(mut self, path: &str, name: &str, v: RegValue) -> Self {
        self.add(path);
        self.keys
            .get_mut(&path.to_lowercase())
            .unwrap()
            .insert(name.to_lowercase(), (name.to_string(), v));
        self
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.keys
            .iter()
            .map(|(k, vs)| (k.clone(), vs.values().cloned().collect()))
            .collect()
    }

    pub(crate) fn value(&self, path: &str, name: &str) -> Option<RegValue> {
        self.get_value(path, name).unwrap()
    }
}

impl Registry for Fake {
    fn key_exists(&self, path: &str) -> io::Result<bool> {
        Ok(self.keys.contains_key(&path.to_lowercase()))
    }
    fn create_key(&mut self, path: &str) -> io::Result<()> {
        self.tick()?;
        self.add(path);
        Ok(())
    }
    fn get_value(&self, path: &str, name: &str) -> io::Result<Option<RegValue>> {
        Ok(self
            .keys
            .get(&path.to_lowercase())
            .and_then(|vs| vs.get(&name.to_lowercase()))
            .map(|(_, v)| v.clone()))
    }
    fn set_value(&mut self, path: &str, name: &str, value: &RegValue) -> io::Result<()> {
        self.tick()?;
        self.add(path);
        self.keys
            .get_mut(&path.to_lowercase())
            .unwrap()
            .insert(name.to_lowercase(), (name.to_string(), value.clone()));
        Ok(())
    }
    fn delete_value(&mut self, path: &str, name: &str) -> io::Result<()> {
        self.tick()?;
        if let Some(vs) = self.keys.get_mut(&path.to_lowercase()) {
            vs.remove(&name.to_lowercase());
        }
        Ok(())
    }
    fn list_values(&self, path: &str) -> io::Result<Vec<(String, RegValue)>> {
        Ok(self
            .keys
            .get(&path.to_lowercase())
            .map(|vs| vs.values().cloned().collect())
            .unwrap_or_default())
    }
    fn delete_key_if_empty(&mut self, path: &str) -> io::Result<bool> {
        let p = path.to_lowercase();
        let Some(vs) = self.keys.get(&p) else {
            return Ok(true);
        };
        let child = format!("{p}\\");
        if !vs.is_empty() || self.keys.keys().any(|k| k.starts_with(&child)) {
            return Ok(false);
        }
        self.tick()?;
        self.keys.remove(&p);
        Ok(true)
    }
    fn notify_assoc_changed(&mut self) {
        self.notifies += 1;
    }
}
