//! E4-7's `.lpk` fixtures: the hostile cases that apply to an `.lpk` and that no other test
//! drives through `lpk x` (the rest are listed in `workers/lpk-worker/tests/hostile.rs`'s
//! module documentation, with the tests that cover them). An `.lpk` is decoded in process,
//! so no worker is involved. Each archive is written in memory with the format's writer and
//! extracted by the real `lpk --json x`, under the bounds of the foreign set's small fixtures,
//! measured by `lpk-procstat-sys`: killed past 10 s, the host's peak working set at most
//! 256 MiB.
#![cfg(any(windows, target_os = "linux"))]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use lpk_format::{EntryFlags, Writer, WriterOptions};
use lpk_procstat_sys::{Output, Spec};

const WALL: Duration = Duration::from_secs(10);
const MEMORY: u64 = 256 << 20;

struct Run {
    code: Option<i32>,
    stderr: String,
}

impl Run {
    fn kind(&self) -> Option<&str> {
        let i = self.stderr.find("{\"error\":")?;
        let rest = &self.stderr[i..];
        let k = rest.find("\"kind\":\"")? + 8;
        let end = rest[k..].find('"')?;
        Some(&rest[k..k + end])
    }
}

fn packed_with(f: impl FnOnce(&mut Writer<&mut Vec<u8>>)) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, WriterOptions::default()).unwrap();
    f(&mut w);
    w.finish().unwrap();
    out
}

/// `lpk --json x <extra...> <archive> <out>` under the bounds.
fn x(dir: &Path, archive: &Path, out: &Path, extra: &[&str]) -> Run {
    let se = dir.join("stderr.txt");
    let spec = Spec::new(env!("CARGO_BIN_EXE_lpk"))
        .args(["--json", "x"])
        .args(extra.iter().copied())
        .args([archive.as_os_str(), out.as_os_str()])
        .stdout(Output::Discard)
        .stderr(Output::File(se.clone()))
        .timeout(WALL);
    let m = lpk_procstat_sys::run(&spec).unwrap();
    let run = Run {
        code: m.exit_code,
        stderr: std::fs::read_to_string(&se).unwrap_or_default(),
    };
    assert!(!m.timed_out, "past {WALL:?}: {}", run.stderr);
    assert!(
        m.peak_rss <= MEMORY,
        "host peak {} over {MEMORY}: {}",
        m.peak_rss,
        run.stderr
    );
    run
}

/// Everything under `root`, relative.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if std::fs::symlink_metadata(&p).unwrap().is_dir() {
                stack.push(p.clone());
            }
            found.push(p.strip_prefix(root).unwrap().to_path_buf());
        }
    }
    found.sort();
    found
}

/// A case-only pair (`Readme.txt`, `README.TXT`): on a case-insensitive target (Windows) the
/// pair is refused before any write (`CaseCollision`); on Linux both files are written.
#[test]
fn an_lpk_case_only_pair_is_a_case_collision() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("case.lpk");
    std::fs::write(
        &a,
        packed_with(|w| {
            w.add_file("Readme.txt", EntryFlags::EMPTY, 0, &mut &b"first"[..])
                .unwrap();
            w.add_file("README.TXT", EntryFlags::EMPTY, 0, &mut &b"second"[..])
                .unwrap();
        }),
    )
    .unwrap();
    let jail = d.path().join("jail");
    let out = jail.join("a/out");
    std::fs::create_dir_all(jail.join("a")).unwrap();
    let r = x(d.path(), &a, &out, &[]);
    if cfg!(windows) {
        assert_eq!(r.code, Some(2), "{}", r.stderr);
        assert_eq!(r.kind(), Some("CaseCollision"), "{}", r.stderr);
        assert!(!out.exists(), "something was written");
    } else {
        assert_eq!(r.code, Some(0), "{}", r.stderr);
        assert_eq!(std::fs::read(out.join("Readme.txt")).unwrap(), b"first");
        assert_eq!(std::fs::read(out.join("README.TXT")).unwrap(), b"second");
    }
    let outside: Vec<PathBuf> = tree(&jail)
        .into_iter()
        .filter(|p| !p.starts_with(Path::new("a").join("out")))
        .collect();
    assert_eq!(outside, [PathBuf::from("a")]);
}

/// An entry below a symlink entry (`up` -> `../..`, then `up/e4-7-through-link.txt`), the
/// write-through-a-link shape: a conflicting name, refused before any write (`UnsafePath`),
/// whether symlinks are skipped (the default) or created (`--symlinks`).
#[test]
fn an_lpk_entry_below_a_symlink_entry_is_a_conflicting_name() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("below.lpk");
    std::fs::write(
        &a,
        packed_with(|w| {
            w.add_symlink("up", EntryFlags::EMPTY, 0, b"../..").unwrap();
            w.add_file(
                "up/e4-7-through-link.txt",
                EntryFlags::EMPTY,
                0,
                &mut &b"escaped"[..],
            )
            .unwrap();
        }),
    )
    .unwrap();
    let jail = d.path().join("jail");
    let out = jail.join("a/b/out");
    std::fs::create_dir_all(jail.join("a/b")).unwrap();
    for extra in [&[][..], &["--symlinks"][..]] {
        let r = x(d.path(), &a, &out, extra);
        assert_eq!(r.code, Some(2), "{extra:?}: {}", r.stderr);
        assert_eq!(r.kind(), Some("UnsafePath"), "{extra:?}: {}", r.stderr);
        assert!(r.stderr.contains("conflicting name"), "{}", r.stderr);
        assert!(!out.exists(), "{extra:?}: something was written");
        assert_eq!(
            tree(&jail),
            [PathBuf::from("a"), Path::new("a").join("b")],
            "{extra:?}"
        );
    }
}
