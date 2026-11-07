//! The binary response files of Explorer's menu (E6-2; `docs/API.md`, "Activation verbs";
//! `docs/notes/e6-design.md` section 1, "Invoke: the contract with the GUI").
//!
//! `lpk_shell.dll` writes `shell-<32 hex>.verbs` into `%LOCALAPPDATA%\org.litepack.gui\Verbs`
//! (the installed GUI's local state, whatever this copy's portable mode: the DLL cannot know it)
//! and starts the GUI with `@<that file>`. This module reads format version 1 (little-endian):
//! the 8 bytes `LPKVERBS`, a `u16` version, the verb (`u16` unit count, UTF-16 units), the
//! `--name` value (`u32` count, units, 0 when none), a `u32` path count, then per path a `u32`
//! count and its UTF-16LE units, unpaired surrogates kept; nothing follows. Each path is rebuilt
//! with `OsString::from_wide`, which is lossless. The reader refuses anything else with a
//! message; it is the only reader of these files, and a file outside that folder or not named
//! that way is E5-7's UTF-8 text format instead (`cli`).
//!
//! The file is deleted after use, by the instance that goes on to act on it (a second instance
//! only forwards the `@file` argument to the first, which must still find the file), and only
//! such a file, never another `@file`; at start-up files of that name older than 24 hours are
//! swept.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The first eight bytes.
pub const MAGIC: &[u8; 8] = b"LPKVERBS";

/// The version this reader accepts (the "current and previous" rule starts at the next one).
pub const VERSION: u16 = 1;

/// The largest file read.
pub const MAX_BYTES: u64 = 16 << 20;

/// The most paths one file may hold.
pub const MAX_PATHS: u32 = 1 << 20;

/// The longest path, in UTF-16 units (Windows' extended limit).
pub const MAX_PATH_UNITS: u32 = 32_767;

/// How old a file must be to be swept at start-up.
pub const SWEEP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The folder under the GUI's local state.
pub const FOLDER: &str = "Verbs";

/// What a response file asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The verb flag (one of `cli::VERBS`).
    pub verb: String,
    /// `--name`'s value in UTF-16 units, when the file has one.
    pub name: Option<Vec<u16>>,
    /// The paths, in selection order, as UTF-16 units kept exactly.
    pub paths: Vec<Vec<u16>>,
}

/// UTF-16 units as an `OsString`: lossless on Windows, where an unpaired surrogate is a legal
/// file name character; lossy elsewhere (the format exists for the Windows shell).
pub fn os_string(units: &[u16]) -> OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(units)
    }
    #[cfg(not(windows))]
    {
        OsString::from(String::from_utf16_lossy(units))
    }
}

/// A cursor over the bytes that refuses to run past the end.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.0.len() < n {
            return Err("truncated".into());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([
            *b.first().unwrap_or(&0),
            *b.get(1).unwrap_or(&0),
        ]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        let mut a = [0u8; 4];
        a.copy_from_slice(b);
        Ok(u32::from_le_bytes(a))
    }

    fn units(&mut self, count: usize) -> Result<Vec<u16>, String> {
        let bytes = self.take(count.checked_mul(2).ok_or("truncated")?)?;
        Ok(bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect())
    }
}

/// Parse a version 1 response file's bytes.
pub fn parse_bytes(bytes: &[u8]) -> Result<Request, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("more than {MAX_BYTES} bytes"));
    }
    let mut r = Reader(bytes);
    if r.take(MAGIC.len())
        .map_err(|_| "not a LitePack response file")?
        != MAGIC
    {
        return Err("not a LitePack response file (wrong magic)".into());
    }
    let version = r.u16()?;
    if version != VERSION {
        return Err(format!(
            "response file version {version}; this LitePack reads version {VERSION}"
        ));
    }
    let n = usize::from(r.u16()?);
    let verb = String::from_utf16(&r.units(n)?).map_err(|_| "the verb is not valid text")?;
    if !crate::cli::VERBS.contains(&verb.as_str()) {
        return Err(format!("{verb:?}: not a verb"));
    }
    let n = r.u32()?;
    if n > MAX_PATH_UNITS {
        return Err(format!("--name longer than {MAX_PATH_UNITS} units"));
    }
    let name = r.units(n as usize)?;
    let count = r.u32()?;
    if count > MAX_PATHS {
        return Err(format!("more than {MAX_PATHS} paths"));
    }
    // Each path takes at least four bytes: a count that cannot fit is truncation, found before
    // anything is allocated for it.
    if u64::from(count) * 4 > r.0.len() as u64 {
        return Err("truncated".into());
    }
    let mut paths = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let n = r.u32()?;
        if n > MAX_PATH_UNITS {
            return Err(format!("a path longer than {MAX_PATH_UNITS} units"));
        }
        if n == 0 {
            return Err("an empty path".into());
        }
        paths.push(r.units(n as usize)?);
    }
    if !r.0.is_empty() {
        return Err(format!("{} bytes after the last path", r.0.len()));
    }
    Ok(Request {
        verb,
        name: (!name.is_empty()).then_some(name),
        paths,
    })
}

/// The arguments the request stands for, as the command line would have carried them: the
/// verb, `--name <name>` if any, then `--` and every path (so that a path never reads as a
/// flag).
pub fn arguments(request: &Request) -> Vec<OsString> {
    let mut out = vec![OsString::from(&request.verb)];
    if let Some(name) = &request.name {
        out.push(OsString::from(crate::cli::NAME));
        out.push(os_string(name));
    }
    out.push(OsString::from("--"));
    out.extend(request.paths.iter().map(|p| os_string(p)));
    out
}

/// The folder of response files: `<local state>\Verbs` of the installed (non-portable) GUI.
pub fn verbs_dir_with(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    crate::paths::locate(None, var)
        .state
        .map(|s| s.join(FOLDER))
}

/// This process's response-file folder.
pub fn verbs_dir() -> Option<PathBuf> {
    verbs_dir_with(|k| std::env::var_os(k))
}

/// Whether `name` is `shell-<32 lowercase hex>.verbs`.
pub fn is_shell_name(name: &str) -> bool {
    name.strip_prefix("shell-")
        .and_then(|n| n.strip_suffix(".verbs"))
        .is_some_and(|hex| {
            hex.len() == 32 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

fn same_folder(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| {
                let s = c.as_os_str().to_string_lossy();
                if cfg!(windows) {
                    s.to_lowercase()
                } else {
                    s.into_owned()
                }
            })
            .collect()
    };
    norm(a) == norm(b)
}

/// Whether `path` is a response file of the shell: in `dir` itself and named as the shell names
/// them.
pub fn is_shell_file(path: &Path, dir: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_shell_name)
        && path.parent().is_some_and(|p| same_folder(p, dir))
}

/// Read and parse the response file at `path` through a single handle: a regular file, at most
/// [`MAX_BYTES`]. Error texts name the file.
pub fn read(path: &Path) -> Result<Request, String> {
    let err = |e: &dyn std::fmt::Display| format!("@{}: {e}", path.display());
    let file = std::fs::File::open(path).map_err(|e| err(&e))?;
    if !file.metadata().map_err(|e| err(&e))?.is_file() {
        return Err(err(&"not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| err(&e))?;
    parse_bytes(&bytes).map_err(|e| err(&e))
}

/// Delete a response file, but only a file [`is_shell_file`] accepts in `dir`; whether one was
/// removed.
pub fn delete(path: &Path, dir: &Path) -> bool {
    is_shell_file(path, dir) && std::fs::remove_file(path).is_ok()
}

/// Delete the response files of `dir` older than `max_age` at `now`; how many were removed.
/// Only regular files of the shell's name (never a link, never another name).
pub fn sweep_dir(dir: &Path, now: SystemTime, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(is_shell_name) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > max_age);
        if meta.file_type().is_file() && old && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The start-up sweep of this process's folder.
pub fn sweep() -> usize {
    verbs_dir().map_or(0, |d| sweep_dir(&d, SystemTime::now(), SWEEP_AGE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// The committed vector `lpk-shell`'s writer test also produces byte for byte.
    fn vector() -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/shell-v1.verbs"))
            .unwrap()
    }

    #[test]
    fn the_committed_vector_parses_to_the_expected_paths() {
        let r = parse_bytes(&vector()).unwrap();
        assert_eq!(r.verb, "--create");
        assert_eq!(r.name, Some(w("Docs.lpk")));
        let mut lone = w("C:\\data\\lone-");
        lone.push(0xD800);
        lone.extend(w(".txt"));
        assert_eq!(
            r.paths,
            vec![
                w("C:\\data\\a b.txt"),
                w("C:\\data\\\u{1F600} caf\u{e9}"),
                lone
            ]
        );
        let args = arguments(&r);
        assert_eq!(
            args[..4],
            ["--create", "--name", "Docs.lpk", "--"].map(OsString::from)
        );
        assert_eq!(args.len(), 7);
        assert_eq!(args[4], OsString::from("C:\\data\\a b.txt"));
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            let last: Vec<u16> = args[6].encode_wide().collect();
            assert!(last.contains(&0xD800), "the unpaired surrogate survives");
        }
    }

    #[test]
    fn refusals_have_clear_messages() {
        let good = vector();
        let bad = |b: &[u8]| parse_bytes(b).unwrap_err();
        assert!(bad(b"").contains("not a LitePack"));
        let mut wrong = good.clone();
        wrong[0] = b'X';
        assert!(bad(&wrong).contains("magic"));
        let mut v2 = good.clone();
        v2[8] = 2;
        assert!(bad(&v2).contains("version 2"));
        let mut v0 = good.clone();
        v0[8] = 0;
        assert!(bad(&v0).contains("version 0"));
        for cut in [9, 11, 20, good.len() - 1] {
            assert!(bad(&good[..cut]).contains("truncated"), "cut at {cut}");
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(bad(&trailing).contains("after the last path"));
        // An unknown verb, and a name or path too long.
        let mut unknown = good.clone();
        unknown[12] = b'x';
        assert!(bad(&unknown).contains("not a verb"));
        let header = |count: u32, first: u32| {
            let mut b = good[..8 + 2 + 2 + 16].to_vec();
            b.extend(0u32.to_le_bytes());
            b.extend(count.to_le_bytes());
            b.extend(first.to_le_bytes());
            b
        };
        assert!(bad(&header(MAX_PATHS + 1, 1)).contains("more than 1048576 paths"));
        assert!(bad(&header(1, MAX_PATH_UNITS + 1)).contains("longer than 32767"));
        assert!(bad(&header(1, 0)).contains("empty path"));
        let mut long_name = good[..8 + 2 + 2 + 16].to_vec();
        long_name.extend((MAX_PATH_UNITS + 1).to_le_bytes());
        assert!(bad(&long_name).contains("--name longer"));
        // A count that promises more paths than the file can hold is truncation, not a huge
        // allocation.
        assert!(bad(&header(MAX_PATHS, 1)).contains("truncated"));
        // More than 16 MiB.
        let big = vec![0u8; usize::try_from(MAX_BYTES).unwrap() + 1];
        assert!(bad(&big).contains("more than"));
    }

    #[test]
    fn names_and_folders_decide_what_is_a_shell_file() {
        let hex = "0123456789abcdef0123456789abcdef";
        assert!(is_shell_name(&format!("shell-{hex}.verbs")));
        assert!(!is_shell_name(&format!(
            "shell-{}.verbs",
            hex.to_uppercase()
        )));
        assert!(!is_shell_name(&format!("shell-{hex}0.verbs")));
        assert!(!is_shell_name(&format!("shell-{hex}.txt")));
        assert!(!is_shell_name("args.txt"));
        let dir = Path::new("base").join("Verbs");
        assert!(is_shell_file(&dir.join(format!("shell-{hex}.verbs")), &dir));
        assert!(!is_shell_file(
            &dir.join("sub").join(format!("shell-{hex}.verbs")),
            &dir
        ));
        assert!(!is_shell_file(
            &Path::new("other").join(format!("shell-{hex}.verbs")),
            &dir
        ));
    }

    #[test]
    fn the_folder_is_the_installed_guis_local_state() {
        let v = verbs_dir_with(|k| (k == "LOCALAPPDATA").then(|| OsString::from("L")));
        if cfg!(windows) {
            assert_eq!(
                v,
                Some(Path::new("L").join("org.litepack.gui").join("Verbs"))
            );
        }
    }

    #[test]
    fn read_refuses_a_directory_and_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_err());
        assert!(read(&dir.path().join("none")).is_err());
        let f = dir.path().join("a.verbs");
        std::fs::write(&f, vector()).unwrap();
        assert_eq!(read(&f).unwrap().verb, "--create");
    }

    #[test]
    fn delete_removes_only_a_shell_file_and_sweep_only_old_ones() {
        let dir = tempfile::tempdir().unwrap();
        let hex = |c: char| c.to_string().repeat(32);
        let mine = |c: char| dir.path().join(format!("shell-{}.verbs", hex(c)));
        let other = dir.path().join("args.txt");
        let elsewhere = tempfile::tempdir().unwrap();
        let foreign = elsewhere.path().join(format!("shell-{}.verbs", hex('a')));
        for p in [&mine('a'), &mine('b'), &other, &foreign] {
            std::fs::write(p, b"x").unwrap();
        }
        // Deletion: the shell's file in its folder; never another name or another folder.
        assert!(!delete(&other, dir.path()));
        assert!(!delete(&foreign, dir.path()));
        assert!(other.exists() && foreign.exists());
        assert!(delete(&mine('a'), dir.path()));
        assert!(!mine('a').exists());
        // Sweep: nothing is old yet; a day and a bit later the shell's file goes, not the other.
        let now = SystemTime::now();
        assert_eq!(sweep_dir(dir.path(), now, SWEEP_AGE), 0);
        let later = now + SWEEP_AGE + Duration::from_secs(60);
        assert_eq!(sweep_dir(dir.path(), later, SWEEP_AGE), 1);
        assert!(!mine('b').exists());
        assert!(other.exists(), "another name is never swept");
        assert_eq!(sweep_dir(&dir.path().join("none"), later, SWEEP_AGE), 0);
    }
}
