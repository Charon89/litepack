//! E6-5: Mark of the Web through the CLI (`docs/notes/e6-design.md` section 4). An archive
//! zoned 3 or 4 gives every file derived from it (`x`, `export`, `repair`, a nested archive's
//! own extraction) a `Zone.Identifier` stream with that `ZoneId` only; zones 0-2, an unzoned
//! archive and `--motw off`: no stream.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn lpk(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_lpk"))
        .args(args)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn stream(p: &Path) -> PathBuf {
    let mut n = p.as_os_str().to_owned();
    n.push(":Zone.Identifier");
    PathBuf::from(n)
}

/// The stream's content, or `None` when the file has none.
fn zone_of(p: &Path) -> Option<String> {
    std::fs::read(stream(p))
        .ok()
        .map(|b| String::from_utf8(b).unwrap())
}

fn set_zone(p: &Path, content: &str) {
    std::fs::write(stream(p), content).unwrap();
}

const FILES: [&str; 4] = ["a.txt", "d/b.bin", "d/e/c.txt", "z.txt"];

/// A source tree and its archive `a.lpk` (unzoned), under `root`.
fn archive(root: &Path) -> PathBuf {
    let src = root.join("src");
    std::fs::create_dir_all(src.join("d/e")).unwrap();
    std::fs::write(src.join("a.txt"), b"alpha").unwrap();
    std::fs::write(src.join("d/b.bin"), vec![7u8; 20_000]).unwrap();
    std::fs::write(src.join("d/e/c.txt"), b"").unwrap();
    std::fs::write(src.join("z.txt"), b"zeta").unwrap();
    let a = root.join("a.lpk");
    let (code, _, err) = lpk(&["a", s(&a), s(&src), "--fast"]);
    assert_eq!(code, 0, "{err}");
    a
}

fn expect(dest: &Path, want: Option<&str>) {
    for f in FILES {
        assert_eq!(zone_of(&dest.join(f)).as_deref(), want, "{f}");
    }
}

const Z3: &str = "[ZoneTransfer]\r\nZoneId=3\r\n";
const Z4: &str = "[ZoneTransfer]\r\nZoneId=4\r\n";

#[test]
fn a_zoned_archive_gives_every_extracted_file_its_zone_id_only() {
    let d = tempfile::tempdir().unwrap();
    let a = archive(d.path());
    set_zone(
        &a,
        "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.invalid/\r\nHostUrl=https://example.invalid/a.lpk\r\n",
    );
    let out = d.path().join("out3");
    let (code, _, err) = lpk(&["x", s(&a), s(&out)]);
    assert_eq!(code, 0, "{err}");
    assert!(err.is_empty(), "{err}");
    expect(&out, Some(Z3));
    assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"alpha");
    set_zone(&a, Z4);
    let out = d.path().join("out4");
    let (code, _, err) = lpk(&["x", s(&a), s(&out), "--threads", "4"]);
    assert_eq!(code, 0, "{err}");
    expect(&out, Some(Z4));
    // An unparseable stream is read as the Internet zone.
    set_zone(&a, "not a zone file");
    let out = d.path().join("out-bad");
    let (code, _, err) = lpk(&["x", s(&a), s(&out)]);
    assert_eq!(code, 0, "{err}");
    expect(&out, Some(Z3));
}

#[test]
fn an_unzoned_archive_and_zones_zero_to_two_give_none() {
    let d = tempfile::tempdir().unwrap();
    let a = archive(d.path());
    let out = d.path().join("none");
    let (code, _, err) = lpk(&["x", s(&a), s(&out)]);
    assert_eq!(code, 0, "{err}");
    expect(&out, None);
    for z in 0..=2 {
        set_zone(&a, &format!("[ZoneTransfer]\r\nZoneId={z}\r\n"));
        let out = d.path().join(format!("z{z}"));
        let (code, _, err) = lpk(&["x", s(&a), s(&out)]);
        assert_eq!(code, 0, "{err}");
        expect(&out, None);
    }
}

#[test]
fn the_opt_out_gives_none() {
    let d = tempfile::tempdir().unwrap();
    let a = archive(d.path());
    set_zone(&a, Z3);
    let out = d.path().join("off");
    let (code, _, err) = lpk(&["x", s(&a), s(&out), "--motw", "off"]);
    assert_eq!(code, 0, "{err}");
    expect(&out, None);
    let (code, _, _) = lpk(&["x", s(&a), s(&out), "--motw", "maybe"]);
    assert_ne!(code, 0);
}

/// A nested archive is a file: it carries the zone, and its own extraction propagates it.
#[test]
fn a_nested_archive_propagates_again() {
    let d = tempfile::tempdir().unwrap();
    let inner = archive(&d.path().join("inner"));
    let outer_src = d.path().join("outer-src");
    std::fs::create_dir_all(&outer_src).unwrap();
    std::fs::copy(&inner, outer_src.join("inner.lpk")).unwrap();
    let outer = d.path().join("outer.lpk");
    let (code, _, err) = lpk(&["a", s(&outer), s(&outer_src), "--fast"]);
    assert_eq!(code, 0, "{err}");
    set_zone(&outer, Z4);
    let o1 = d.path().join("o1");
    let (code, _, err) = lpk(&["x", s(&outer), s(&o1)]);
    assert_eq!(code, 0, "{err}");
    let nested = o1.join("inner.lpk");
    assert_eq!(zone_of(&nested).as_deref(), Some(Z4));
    let o2 = d.path().join("o2");
    let (code, _, err) = lpk(&["x", s(&nested), s(&o2)]);
    assert_eq!(code, 0, "{err}");
    expect(&o2, Some(Z4));
}

#[test]
fn export_and_repair_copies_carry_the_zone_and_the_opt_out_none() {
    let d = tempfile::tempdir().unwrap();
    let a = archive(d.path());
    set_zone(&a, Z3);
    for (name, extra, want) in [
        ("e.zip", None, Some(Z3)),
        ("e.7z", None, Some(Z3)),
        ("off.zip", Some("off"), None),
    ] {
        let o = d.path().join(name);
        let mut args = vec!["export", s(&a), s(&o)];
        if let Some(v) = extra {
            args.extend(["--motw", v]);
        }
        let (code, _, err) = lpk(&args);
        assert_eq!(code, 0, "{err}");
        assert!(!err.contains("Mark of the Web"), "{err}");
        assert_eq!(zone_of(&o).as_deref(), want, "{name}");
    }
    let r = d.path().join("r.lpk");
    let (code, _, err) = lpk(&["repair", s(&a), s(&r)]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(zone_of(&r).as_deref(), Some(Z3));
    let r = d.path().join("r-off.lpk");
    let (code, _, err) = lpk(&["repair", s(&a), s(&r), "--motw", "off"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(zone_of(&r), None);
    // An unzoned archive's copies: none.
    std::fs::remove_file(stream(&a)).unwrap();
    let o = d.path().join("plain.zip");
    let (code, _, err) = lpk(&["export", s(&a), s(&o)]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(zone_of(&o), None);
}
