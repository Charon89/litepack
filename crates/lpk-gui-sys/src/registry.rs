//! The registry calls behind `lpk-gui.exe --register-shell` and `--unregister-shell` (E6-4,
//! `docs/notes/e6-design.md` section 3), as safe wrappers: create a key, read, write and delete a
//! value, list a key's values, delete a key only when it is empty, and tell Explorer that
//! associations changed. The only registry code of the GUI that holds `unsafe`.
//!
//! Every call takes its keys relative to a [`RegBase`]. Only [`RegBase::current_user`] names the
//! real `HKEY_CURRENT_USER`, and only the two `--*-shell` entry points call it; the tests of this
//! module work under a scratch key `HKCU\Software\LitePack.Test.<pid>.<n>` that they create and
//! delete (a drop guard that refuses any other path).

use std::io;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteValueW, RegEnumValueW, RegOpenKeyExW,
    RegQueryInfoKeyW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    KEY_SET_VALUE, KEY_WRITE, REG_NONE, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

/// A registry value as registration sees it: a string, an empty `REG_NONE` value (what
/// `OpenWithProgids` entries are), or anything else (never written, never equal to ours).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegValue {
    /// `REG_SZ`.
    Sz(String),
    /// `REG_NONE` with no data.
    None,
    /// Any other type, by its number.
    Other(u32),
}

/// The key every path is relative to.
#[derive(Debug, Clone)]
pub struct RegBase {
    root: HKEY,
    /// Prepended (with a backslash) to every path; empty for the real root.
    prefix: String,
}

impl RegBase {
    /// The real `HKEY_CURRENT_USER`: for the two `--register-shell` and `--unregister-shell`
    /// entry points alone.
    pub fn current_user() -> Self {
        RegBase {
            root: HKEY_CURRENT_USER,
            prefix: String::new(),
        }
    }

    fn full(&self, path: &str) -> String {
        match (self.prefix.is_empty(), path.is_empty()) {
            (true, _) => path.to_string(),
            (false, true) => self.prefix.clone(),
            (false, false) => format!("{}\\{}", self.prefix, path),
        }
    }

    /// Open `path` (absent: `None`).
    fn open(
        &self,
        path: &str,
        access: windows::Win32::System::Registry::REG_SAM_FLAGS,
    ) -> io::Result<Option<Key>> {
        let w = wide(&self.full(path));
        let mut h = HKEY::default();
        // SAFETY: `w` is a NUL-terminated UTF-16 string that outlives the call and `h` is a
        // valid out pointer.
        let r = unsafe { RegOpenKeyExW(self.root, PCWSTR(w.as_ptr()), None, access, &mut h) };
        if r == ERROR_FILE_NOT_FOUND || r == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        check(r)?;
        Ok(Some(Key(h)))
    }

    /// Whether the key exists.
    pub fn key_exists(&self, path: &str) -> io::Result<bool> {
        Ok(self.open(path, KEY_READ)?.is_some())
    }

    /// Create the key (every missing ancestor too); an existing key is left as it is.
    pub fn create_key(&self, path: &str) -> io::Result<()> {
        self.create(path).map(drop)
    }

    fn create(&self, path: &str) -> io::Result<Key> {
        let w = wide(&self.full(path));
        let mut h = HKEY::default();
        // SAFETY: `w` is a NUL-terminated UTF-16 string that outlives the call, the class is
        // null, the security attributes and the disposition are absent and `h` is a valid out
        // pointer.
        let r = unsafe {
            RegCreateKeyExW(
                self.root,
                PCWSTR(w.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE | KEY_READ,
                None,
                &mut h,
                None,
            )
        };
        check(r)?;
        Ok(Key(h))
    }

    /// A value of `path` (`name` empty: the default value); `None` when the key or the value is
    /// absent.
    pub fn get_value(&self, path: &str, name: &str) -> io::Result<Option<RegValue>> {
        let Some(key) = self.open(path, KEY_READ)? else {
            return Ok(None);
        };
        key.query(name)
    }

    /// Set a value, creating the key when absent (never `Other`).
    pub fn set_value(&self, path: &str, name: &str, value: &RegValue) -> io::Result<()> {
        let key = self.create(path)?;
        let w = wide(name);
        let (ty, data): (REG_VALUE_TYPE, Vec<u8>) = match value {
            RegValue::Sz(s) => (
                REG_SZ,
                wide(s).into_iter().flat_map(u16::to_le_bytes).collect(),
            ),
            RegValue::None => (REG_NONE, Vec::new()),
            RegValue::Other(_) => return Err(io::ErrorKind::InvalidInput.into()),
        };
        // SAFETY: the key handle is open for writing, `w` is NUL-terminated and `data` is a
        // slice that outlives the call.
        let r = unsafe { RegSetValueExW(key.0, PCWSTR(w.as_ptr()), None, ty, Some(&data)) };
        check(r)
    }

    /// Delete a value; an absent key or value is fine.
    pub fn delete_value(&self, path: &str, name: &str) -> io::Result<()> {
        let Some(key) = self.open(path, KEY_SET_VALUE)? else {
            return Ok(());
        };
        let w = wide(name);
        // SAFETY: the key handle is open for writing and `w` is NUL-terminated.
        let r = unsafe { RegDeleteValueW(key.0, PCWSTR(w.as_ptr())) };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(r)
    }

    /// Every value of `path`, in the order the registry gives them; empty when the key is absent.
    pub fn list_values(&self, path: &str) -> io::Result<Vec<(String, RegValue)>> {
        let Some(key) = self.open(path, KEY_READ)? else {
            return Ok(Vec::new());
        };
        let info = key.info()?;
        let mut name = vec![0u16; info.max_name as usize + 2];
        let mut data = vec![0u8; info.max_data as usize + 2];
        let mut out = Vec::new();
        for index in 0..info.values {
            let mut name_len = name.len() as u32;
            let mut data_len = data.len() as u32;
            let mut ty = 0u32;
            // SAFETY: the key handle is open for reading; the name and data buffers and their
            // length variables are valid for writes of the lengths passed, `ty` is a valid out
            // pointer.
            let r = unsafe {
                RegEnumValueW(
                    key.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut ty),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            check(r)?;
            let n = String::from_utf16_lossy(&name[..name_len as usize]);
            out.push((n, decode(ty, &data[..data_len as usize])));
        }
        Ok(out)
    }

    /// Delete the key only when it holds no value and no subkey; `Ok(false)` when it holds
    /// either, `Ok(true)` when it is gone (also when it was absent).
    pub fn delete_key_if_empty(&self, path: &str) -> io::Result<bool> {
        let Some(key) = self.open(path, KEY_READ)? else {
            return Ok(true);
        };
        let info = key.info()?;
        drop(key);
        if info.values != 0 || info.subkeys != 0 {
            return Ok(false);
        }
        let w = wide(&self.full(path));
        // SAFETY: `w` is a NUL-terminated UTF-16 string; `RegDeleteKeyW` itself refuses a key that
        // has subkeys.
        let r = unsafe { RegDeleteKeyW(self.root, PCWSTR(w.as_ptr())) };
        if r == ERROR_FILE_NOT_FOUND {
            return Ok(true);
        }
        check(r)?;
        Ok(true)
    }
}

/// Tell Explorer and other shell clients that file associations changed
/// (`SHChangeNotify(SHCNE_ASSOCCHANGED)`). Never called by a test.
pub fn notify_assoc_changed() {
    // SAFETY: `SHCNE_ASSOCCHANGED` with `SHCNF_IDLIST` takes no item pointers.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// An open key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open or create and is closed once, here.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

struct Info {
    subkeys: u32,
    values: u32,
    max_name: u32,
    max_data: u32,
}

impl Key {
    fn info(&self) -> io::Result<Info> {
        let mut subkeys = 0u32;
        let mut max_name = 0u32;
        let mut values = 0u32;
        let mut max_data = 0u32;
        // SAFETY: the key handle is open for reading and every pointer is a valid out pointer
        // or absent.
        let r = unsafe {
            RegQueryInfoKeyW(
                self.0,
                None,
                None,
                None,
                Some(&mut subkeys),
                None,
                None,
                Some(&mut values),
                Some(&mut max_name),
                Some(&mut max_data),
                None,
                None,
            )
        };
        check(r)?;
        Ok(Info {
            subkeys,
            values,
            max_name,
            max_data,
        })
    }

    fn query(&self, name: &str) -> io::Result<Option<RegValue>> {
        let w = wide(name);
        let mut data = vec![0u8; 256];
        loop {
            let mut ty = 0u32;
            let mut len = data.len() as u32;
            // SAFETY: the key handle is open for reading, `w` is NUL-terminated and `data` and
            // `len` are valid for writes of the length passed.
            let r = unsafe {
                RegQueryValueExW(
                    self.0,
                    PCWSTR(w.as_ptr()),
                    None,
                    Some(&mut ty as *mut u32 as *mut REG_VALUE_TYPE),
                    Some(data.as_mut_ptr()),
                    Some(&mut len),
                )
            };
            if r == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if r == ERROR_MORE_DATA {
                data = vec![0u8; (len as usize).max(data.len() * 2)];
                continue;
            }
            check(r)?;
            return Ok(Some(decode(ty, &data[..len as usize])));
        }
    }
}

fn decode(ty: u32, data: &[u8]) -> RegValue {
    if ty == REG_SZ.0 {
        let units: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        RegValue::Sz(String::from_utf16_lossy(&units[..end]))
    } else if ty == REG_NONE.0 && data.is_empty() {
        RegValue::None
    } else {
        RegValue::Other(ty)
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn check(r: WIN32_ERROR) -> io::Result<()> {
    if r == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(r.0 as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::RegDeleteTreeW;

    /// The only prefix a test key may have.
    const SCRATCH: &str = "Software\\LitePack.Test.";

    /// A scratch key under HKCU that is deleted, with everything below it, on drop (also when
    /// the test fails). The tree deletion exists only here and refuses any other path.
    struct Scratch(RegBase);

    impl Scratch {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let prefix = format!(
                "{SCRATCH}{}.{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            );
            let base = RegBase {
                root: HKEY_CURRENT_USER,
                prefix,
            };
            base.create_key("").unwrap();
            Scratch(base)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            assert!(self.0.prefix.starts_with(SCRATCH));
            let w = wide(&self.0.prefix);
            // SAFETY: `w` is a NUL-terminated UTF-16 string naming a key under
            // `HKCU\Software\LitePack.Test.` (asserted above).
            let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(w.as_ptr())) };
        }
    }

    #[test]
    fn values_round_trip_and_are_deleted() {
        let s = Scratch::new();
        let b = &s.0;
        assert!(!b.key_exists("A\\B").unwrap());
        assert_eq!(b.get_value("A\\B", "x").unwrap(), None);
        b.set_value("A\\B", "x", &RegValue::Sz("héllo \"q\"".into()))
            .unwrap();
        b.set_value("A\\B", "", &RegValue::Sz("default".into()))
            .unwrap();
        b.set_value("A\\B", "n", &RegValue::None).unwrap();
        assert!(b.key_exists("A").unwrap());
        assert_eq!(
            b.get_value("A\\B", "x").unwrap(),
            Some(RegValue::Sz("héllo \"q\"".into()))
        );
        assert_eq!(
            b.get_value("A\\B", "").unwrap(),
            Some(RegValue::Sz("default".into()))
        );
        assert_eq!(b.get_value("A\\B", "n").unwrap(), Some(RegValue::None));
        let mut listed = b.list_values("A\\B").unwrap();
        listed.sort_by(|a, c| a.0.cmp(&c.0));
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[1], ("n".to_string(), RegValue::None));
        let long = "z".repeat(5000);
        b.set_value("A\\B", "long", &RegValue::Sz(long.clone()))
            .unwrap();
        assert_eq!(
            b.get_value("A\\B", "long").unwrap(),
            Some(RegValue::Sz(long))
        );
        b.delete_value("A\\B", "x").unwrap();
        b.delete_value("A\\B", "x").unwrap();
        b.delete_value("Nope", "x").unwrap();
        assert_eq!(b.get_value("A\\B", "x").unwrap(), None);
        assert!(b.list_values("Nope").unwrap().is_empty());
    }

    #[test]
    fn a_key_is_deleted_only_when_empty() {
        let s = Scratch::new();
        let b = &s.0;
        b.set_value("P\\C", "v", &RegValue::None).unwrap();
        // `P` holds a subkey, `P\C` a value.
        assert!(!b.delete_key_if_empty("P").unwrap());
        assert!(!b.delete_key_if_empty("P\\C").unwrap());
        b.delete_value("P\\C", "v").unwrap();
        assert!(b.delete_key_if_empty("P\\C").unwrap());
        assert!(b.delete_key_if_empty("P").unwrap());
        assert!(b.delete_key_if_empty("P").unwrap());
        assert!(!b.key_exists("P").unwrap());
    }

    #[test]
    fn the_scratch_key_is_gone_after_the_guard() {
        let prefix = {
            let s = Scratch::new();
            s.0.set_value("K", "v", &RegValue::None).unwrap();
            s.0.prefix.clone()
        };
        let base = RegBase {
            root: HKEY_CURRENT_USER,
            prefix: String::new(),
        };
        assert!(!base.key_exists(&prefix).unwrap());
    }
}
