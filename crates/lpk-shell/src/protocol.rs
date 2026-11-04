//! The response file the menu hands the GUI (`docs/API.md`, "Activation verbs"; design section 1,
//! "Invoke: the contract with the GUI"): format version 1, written create-new into the GUI's
//! `Verbs` folder. Safe code, tested on every OS; the GUI's reader (`lpk-gui`, `shellfile`)
//! parses the same bytes, and one committed test vector (`crates/lpk-gui/tests/data/
//! shell-v1.verbs`) holds both to it.
//!
//! Little-endian. The 8 bytes `LPKVERBS`; `u16` version (1); the verb as a `u16` count of UTF-16
//! code units and the units; the `--name` value as a `u32` count and the units (0 when none);
//! a `u32` path count; per path a `u32` count and the UTF-16LE units, unpaired surrogates kept.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The file's first eight bytes.
pub const MAGIC: &[u8; 8] = b"LPKVERBS";

/// The protocol version this writer produces.
pub const VERSION: u16 = 1;

/// The verb flags a response file may carry (`--create` with `--name` is `Add to "<name>.lpk"`).
pub const VERBS: [&str; 6] = [
    "--open",
    "--create",
    "--extract-here",
    "--extract-to",
    "--test",
    "--convert",
];

fn units(out: &mut Vec<u8>, text: &[u16]) {
    for u in text {
        out.extend_from_slice(&u.to_le_bytes());
    }
}

/// The bytes of a version 1 response file. `name` and each path are UTF-16 code units
/// (`OsStr::encode_wide` on Windows), kept exactly. `None` when a count does not fit its field
/// (the verb is short; the name and the paths cannot reach 4 Gi units in a shell menu).
pub fn encode(verb: &str, name: &[u16], paths: &[Vec<u16>]) -> Option<Vec<u8>> {
    let verb: Vec<u16> = verb.encode_utf16().collect();
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&u16::try_from(verb.len()).ok()?.to_le_bytes());
    units(&mut out, &verb);
    out.extend_from_slice(&u32::try_from(name.len()).ok()?.to_le_bytes());
    units(&mut out, name);
    out.extend_from_slice(&u32::try_from(paths.len()).ok()?.to_le_bytes());
    for p in paths {
        out.extend_from_slice(&u32::try_from(p.len()).ok()?.to_le_bytes());
        units(&mut out, p);
    }
    Some(out)
}

/// 32 lowercase hex digits from the operating system's randomness (std seeds each
/// `RandomState` from it), the `<32 hex>` of `shell-<32 hex>.verbs`.
pub fn random_hex() -> String {
    let word = || {
        let mut h = RandomState::new().build_hasher();
        h.write_u8(0);
        h.finish()
    };
    format!("{:016x}{:016x}", word(), word())
}

/// Create `shell-<32 hex>.verbs` in `dir` (created if missing), create-new, holding `bytes`;
/// the file's path. A name that already exists is never reused.
pub fn write_new(dir: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let mut last = None;
    for _ in 0..8 {
        let path = dir.join(format!("shell-{}.verbs", random_hex()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                let written = f.write_all(bytes).and_then(|()| f.flush());
                drop(f);
                return match written {
                    Ok(()) => Ok(path),
                    Err(e) => {
                        let _ = std::fs::remove_file(&path);
                        Err(e)
                    }
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no unused file name")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// The paths of the committed vector: ASCII, non-BMP, and an unpaired high surrogate.
    pub(crate) fn vector_paths() -> Vec<Vec<u16>> {
        let mut lone = w("C:\\data\\lone-");
        lone.push(0xD800);
        lone.extend(w(".txt"));
        vec![
            w("C:\\data\\a b.txt"),
            w("C:\\data\\\u{1F600} caf\u{e9}"),
            lone,
        ]
    }

    #[test]
    fn the_writer_produces_the_committed_test_vector() {
        let bytes = encode("--create", &w("Docs.lpk"), &vector_paths()).expect("encodes");
        let committed = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-gui/tests/data/shell-v1.verbs"),
        )
        .expect("the committed vector");
        assert_eq!(bytes, committed);
    }

    #[test]
    fn the_layout_is_as_documented() {
        let bytes = encode("--open", &[], &[w("x")]).expect("encodes");
        let mut want = b"LPKVERBS".to_vec();
        want.extend([1, 0, 6, 0]);
        want.extend(w("--open").iter().flat_map(|u| u.to_le_bytes()));
        want.extend([0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, b'x', 0]);
        assert_eq!(bytes, want);
    }

    #[test]
    fn files_are_created_new_with_random_names() {
        let dir = std::env::temp_dir().join(format!("lpk-shell-proto-{}", random_hex()));
        let a = write_new(&dir.join("Verbs"), b"one").expect("first");
        let b = write_new(&dir.join("Verbs"), b"two").expect("second");
        assert_ne!(a, b);
        for p in [&a, &b] {
            let name = p.file_name().and_then(|n| n.to_str()).expect("name");
            let hex = name
                .strip_prefix("shell-")
                .and_then(|n| n.strip_suffix(".verbs"))
                .expect("pattern");
            assert!(hex.len() == 32 && hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
        }
        assert_eq!(std::fs::read(&a).expect("read"), b"one");
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
