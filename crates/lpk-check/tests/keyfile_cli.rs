//! `lpk-check --keyfile` on archives made at test time (E2-12's follow-up, D-165): refused on an
//! archive encrypted with a password alone, as `lpk` and `lpk-decode` refuse it; still required
//! and accepted on an archive whose key slot takes a keyfile.
#![allow(clippy::unwrap_used)]

use lpk_format::{
    Argon2Params, Credentials, EntryFlags, SealOptions, Suite, Writer, WriterOptions,
};
use std::path::Path;
use std::process::Command;

fn sealed(path: &Path, keyfile: Option<Vec<u8>>) {
    let options = WriterOptions {
        chunk_size: 4096,
        block_size: 8 * 1024,
        seal: Some(SealOptions {
            suite: Suite::AesGcm,
            argon2: Argon2Params {
                t: 1,
                m_kib: 8192,
                p: 1,
            },
            listable: false,
            credentials: Credentials {
                password: b"pw".to_vec(),
                keyfile,
            },
        }),
        ..WriterOptions::default()
    };
    let mut w = Writer::new(std::fs::File::create(path).unwrap(), options).unwrap();
    w.add_file("a.txt", EntryFlags::EMPTY, 0, &mut &b"some text"[..])
        .unwrap();
    w.finish().unwrap();
}

fn bin(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_lpk-check"))
        .args(args)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// A folder removed when the test ends.
struct Tmp(std::path::PathBuf);

impl Tmp {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_unneeded_keyfile_is_refused() {
    // No temporary-folder crate here (conformance.rs's pattern): a folder named by the process.
    let dir = std::env::temp_dir().join(format!("lpk-check-{}-keyfile", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = Tmp(dir);
    let kf_bytes = b"the second factor".to_vec();
    let kf = tmp.path().join("key.bin");
    std::fs::write(&kf, &kf_bytes).unwrap();
    let kf = kf.to_str().unwrap();
    let pw_only = tmp.path().join("pw.lpk");
    sealed(&pw_only, None);
    let pw_only = pw_only.to_str().unwrap();
    let two = tmp.path().join("two.lpk");
    sealed(&two, Some(kf_bytes));
    let two = two.to_str().unwrap();
    for cmd in ["list", "verify", "info", "check"] {
        let (code, out, err) = bin(&[cmd, "--password", "pw", "--keyfile", kf, pw_only]);
        assert_eq!(code, 1, "{cmd}: {out}");
        assert!(out.is_empty(), "{cmd}: {out}");
        assert!(
            err.contains("takes no keyfile; --keyfile is not needed"),
            "{cmd}: {err}"
        );
        let (code, _, err) = bin(&[cmd, "--password", "pw", pw_only]);
        assert_eq!(code, 0, "{cmd}: {err}");
        // A keyfile archive still needs its keyfile, and takes it.
        let (code, _, err) = bin(&[cmd, "--password", "pw", two]);
        assert_eq!(code, 1, "{cmd}");
        assert!(!err.contains("not needed"), "{cmd}: {err}");
        let (code, _, err) = bin(&[cmd, "--password", "pw", "--keyfile", kf, two]);
        assert_eq!(code, 0, "{cmd}: {err}");
    }
    // Extraction writes nothing when the keyfile is refused.
    let dest = tmp.path().join("out");
    let (code, _, err) = bin(&[
        "extract",
        "--password",
        "pw",
        "--keyfile",
        kf,
        pw_only,
        dest.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "{err}");
    assert!(!dest.exists());
}
