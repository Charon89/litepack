//! E4-7, the hostile-archive set (`docs/notes/e4-design.md` §4): every fixture is generated
//! here by a small documented builder (never downloaded, never committed) and extracted with
//! the main workspace's debug `lpk x` through this workspace's `lpk-worker`, staged as in
//! `tests/cli.rs` (`LPK_TEST_WORKER`; on Windows an AppContainer profile per test; a missing
//! `lpk` skips with a note unless `LPK_REQUIRE_E2E=1`, as the `workers` CI job sets).
//!
//! Each test asserts the named reason of the design's table (the `kind` of `lpk --json`'s
//! error line), that nothing appears outside the destination (the destination is
//! `jail/a/b/out` and `jail` is compared before and after, so `../` and `../../` land inside
//! it), the partial output the design removes, and two bounds it states: the wall time (the
//! run is killed past it, `lpk-procstat-sys`'s timeout; the worker's own CPU ceiling is set
//! short with `--max-cpu 1`) and the memory, as the **host's peak working set** measured by
//! `lpk-procstat-sys` (Windows: `PeakWorkingSetSize` of `lpk` alone, the worker being a
//! separate process in its own job; Linux: `ru_maxrss`, the largest resident set of any process
//! of the tree, so an upper bound of the host's). Outside the jail, the roots an escaped name
//! would reach (the temporary root, the current directory, the C: drive's current directory
//! and root on Windows, `/` on Linux) are listed before and after each run and must gain no
//! entry named like a fixture's member.
//!
//! **Time bounds.** The design's bounds are the broker's timeouts set short; of those only the
//! CPU ceiling has a flag (`--max-cpu`, in minutes). The 60 s no-progress timeout
//! (`lpk_core::foreign::NO_PROGRESS`) has no CLI flag, so the tests bound the wall time
//! instead (10 s for the small fixtures); a flag for it is a follow-up.
//!
//! **Formats.** ZIP and tar fixtures for every case. 7z: the unsafe names. A 7z whose
//! members' pack streams overlap cannot be written: 7z has no per-stream offsets, a pack
//! stream starts where the previous one ends (`PackInfo` gives sizes only, from one
//! `PackPos`), so the reader's ranges (`sevenz.rs`, from `pack_stream_offsets`) are disjoint
//! by construction; members of one solid block share that block's range by design, and the
//! host's overlap check is per block. No 7z overlap fixture therefore exists, and none is
//! owed.
//!
//! **`.lpk`.** An `.lpk` is decoded in process, not by the worker (D-66, D-67, D-68), and its
//! parsers are fuzzed (`fuzz/fuzz_targets/{archive_open,archive_mutate,entry_table,...}.rs`).
//! Its hostile cases are covered elsewhere: unsafe names cannot be written and are refused at
//! parse (`lpk-format/src/entry.rs`, `validate_path`'s unit test; `lpk-format/tests/cli.rs`
//! `extraction_path_rules`), device and `:` names and symlinks by the default policy before
//! any write (`lpk-core/tests/extract.rs` `the_default_policy_refuses_before_writing_anything`),
//! sizes by the chunk hashes and the entry table's checks (`lpk-format` conformance and
//! mutation tests), a linked parent by `a_linked_parent_is_refused`. The two cases without an
//! `lpk x` test of their own, a case-only pair and an entry below a symlink entry, are in
//! `crates/lpk-cli/tests/hostile_lpk.rs` (the main workspace, which has the format's writer).
//!
//! The million-member test reads the charge from `lpk x -v`'s text ("listing charge N
//! bytes"): a change of that line's wording breaks the test, deliberately.
#![cfg(any(windows, target_os = "linux"))]
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lpk_procstat_sys::{Output, Spec};

// ---------------------------------------------------------------------------------------------
// Staging (as tests/cli.rs).

struct Staged {
    lpk: PathBuf,
    exe: PathBuf,
    profile: Option<String>,
    dir: tempfile::TempDir,
}

impl Drop for Staged {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(p) = &self.profile {
            let _ = lpk_sandbox_sys::delete_appcontainer_profile(p);
        }
    }
}

fn lpk() -> Option<PathBuf> {
    let worker = PathBuf::from(env!("CARGO_BIN_EXE_lpk-worker"));
    let lpk = worker.with_file_name(if cfg!(windows) { "lpk.exe" } else { "lpk" });
    if lpk.is_file() {
        return Some(lpk);
    }
    let msg = format!(
        "{} missing: build the main workspace (`cargo build -p lpk-cli` at the root) to run the end-to-end tests",
        lpk.display()
    );
    assert!(
        std::env::var_os("LPK_REQUIRE_E2E").is_none(),
        "{msg} (LPK_REQUIRE_E2E is set)"
    );
    eprintln!("SKIPPED: {msg}");
    None
}

fn stage() -> Option<Staged> {
    static N: AtomicU32 = AtomicU32::new(0);
    let lpk = lpk()?;
    let dir = tempfile::tempdir().unwrap();
    let wdir = dir.path().join("worker");
    std::fs::create_dir(&wdir).unwrap();
    let src = PathBuf::from(env!("CARGO_BIN_EXE_lpk-worker"));
    let exe = wdir.join(src.file_name().unwrap());
    std::fs::copy(&src, &exe).unwrap();
    let n = N.fetch_add(1, Ordering::Relaxed);
    #[cfg(windows)]
    let profile = {
        const READ_EXECUTE: u32 = 0x0012_0089 | 0x0012_00A0;
        let name = format!("LitePack.Test.Hostile.{}.{n}", std::process::id());
        lpk_sandbox_sys::create_appcontainer_profile(&name).unwrap();
        let sid = lpk_sandbox_sys::appcontainer_sid_string(&name).unwrap();
        lpk_sandbox_sys::grant_path_access(&wdir, &sid, READ_EXECUTE, true).unwrap();
        Some(name)
    };
    #[cfg(not(windows))]
    let profile = {
        let _ = n;
        None
    };
    std::fs::create_dir_all(dir.path().join("jail/a/b")).unwrap();
    Some(Staged {
        lpk,
        exe,
        profile,
        dir,
    })
}

/// The bounds a run is held to.
#[derive(Clone, Copy)]
struct Bounds {
    /// Killed past this (and the test fails).
    wall: Duration,
    /// The host's peak working set at most this.
    memory: u64,
}

/// A small fixture's bounds: a debug `lpk` and worker start, refuse or extract a few KiB to a
/// few MiB within 10 s (they take well under 1 s), the host within 256 MiB.
const SMALL: Bounds = Bounds {
    wall: Duration::from_secs(10),
    memory: 256 << 20,
};

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    peak: u64,
}

impl Run {
    /// The `kind` of `--json`'s error line.
    fn kind(&self) -> Option<&str> {
        let i = self.stderr.find("{\"error\":")?;
        let rest = &self.stderr[i..];
        let k = rest.find("\"kind\":\"")? + 8;
        let end = rest[k..].find('"')?;
        Some(&rest[k..k + end])
    }
    fn message(&self) -> &str {
        &self.stderr
    }
}

static RUNS: AtomicU32 = AtomicU32::new(0);

impl Staged {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn jail(&self) -> PathBuf {
        self.path("jail")
    }
    /// The destination, two levels below the jail's top.
    fn out(&self) -> PathBuf {
        self.path("jail/a/b/out")
    }
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.path(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// `lpk --json x --max-cpu 1 <extra...> <archive> <out> <patterns...>` under `bounds`.
    fn x(&self, archive: &Path, extra: &[&str], patterns: &[&str], bounds: Bounds) -> Run {
        let n = RUNS.fetch_add(1, Ordering::Relaxed);
        let (so, se) = (
            self.path(&format!("stdout-{n}.txt")),
            self.path(&format!("stderr-{n}.txt")),
        );
        let before = roots();
        let mut spec = Spec::new(&self.lpk)
            .args(["--json", "x", "--max-cpu", "1"])
            .args(extra.iter().copied())
            .args([archive.as_os_str(), self.out().as_os_str()])
            .args(patterns.iter().copied())
            .env("LPK_TEST_WORKER", &self.exe)
            .stdout(Output::File(so.clone()))
            .stderr(Output::File(se.clone()))
            .timeout(bounds.wall);
        if let Some(p) = &self.profile {
            spec = spec.env("LPK_TEST_APPCONTAINER", p);
        }
        let m = lpk_procstat_sys::run(&spec).unwrap();
        let run = Run {
            code: m.exit_code,
            stdout: std::fs::read_to_string(&so).unwrap_or_default(),
            stderr: std::fs::read_to_string(&se).unwrap_or_default(),
            peak: m.peak_rss,
        };
        assert!(
            !m.timed_out,
            "the time bound of {:?} was exceeded: {}",
            bounds.wall, run.stderr
        );
        assert!(
            m.peak_rss <= bounds.memory,
            "the host's peak working set {} exceeds the bound of {}: {}",
            m.peak_rss,
            bounds.memory,
            run.stderr
        );
        assert_no_new_member_names(&before, &roots());
        eprintln!(
            "{}: exit {:?} ({}), {:?} wall, host peak {} bytes",
            archive.file_name().unwrap().to_string_lossy(),
            m.exit_code,
            run.kind().unwrap_or("-"),
            m.wall,
            m.peak_rss
        );
        run
    }

    /// The jail's paths outside the destination: always exactly `a` and `a/b`.
    fn assert_nothing_outside(&self) {
        let jail = self.jail();
        let out = self.out();
        let mut found = BTreeSet::new();
        let mut stack = vec![jail.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p == out {
                    continue;
                }
                if std::fs::symlink_metadata(&p).unwrap().is_dir() {
                    stack.push(p.clone());
                }
                found.insert(p.strip_prefix(&jail).unwrap().to_path_buf());
            }
        }
        let want: BTreeSet<PathBuf> = [PathBuf::from("a"), Path::new("a").join("b")].into();
        assert_eq!(found, want, "something was written outside the destination");
    }

    /// The files under the destination (relative paths), none when it does not exist.
    fn written(&self) -> BTreeSet<PathBuf> {
        let out = self.out();
        let mut found = BTreeSet::new();
        if !out.exists() {
            return found;
        }
        let mut stack = vec![out.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                let meta = std::fs::symlink_metadata(&p).unwrap();
                if meta.is_dir() {
                    stack.push(p);
                } else {
                    found.insert(p.strip_prefix(&out).unwrap().to_path_buf());
                }
            }
        }
        found
    }

    fn clear_out(&self) {
        if self.out().exists() {
            std::fs::remove_dir_all(self.out()).unwrap();
        }
    }
}

/// The places an escaped name would land outside the jail: the temporary root, the current
/// directory, and on Windows the C: drive's current directory (where `C:name` goes) and its
/// root (where `C:\name` and `\\?\C:\name` go); on Linux `/`. Each with its entries' names.
fn roots() -> Vec<(PathBuf, BTreeSet<String>)> {
    let mut dirs = vec![std::env::temp_dir()];
    if let Ok(d) = std::env::current_dir() {
        dirs.push(d);
    }
    if cfg!(windows) {
        if let Some(d) = std::path::absolute("C:x")
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            dirs.push(d);
        }
        dirs.push(PathBuf::from("C:\\"));
    } else {
        dirs.push(PathBuf::from("/"));
    }
    dirs.sort();
    dirs.dedup();
    dirs.into_iter()
        .map(|d| {
            let names = std::fs::read_dir(&d)
                .map(|r| {
                    r.filter_map(Result::ok)
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            (d, names)
        })
        .collect()
}

/// A name as Windows stores it, for comparison: lower case, trailing dots and spaces gone.
fn normal(name: &str) -> String {
    name.trim_end_matches(['.', ' ']).to_lowercase()
}

/// Every name a fixture's member could land under: the components of the unsafe names and the
/// other fixtures' member names.
fn member_names() -> BTreeSet<String> {
    let mut set: BTreeSet<String> = UNSAFE_NAMES
        .iter()
        .flat_map(|n| n.split(['/', '\\', ':']))
        .filter(|c| !matches!(*c, "" | "." | ".." | "?" | "a" | "C" | "server" | "share"))
        .map(normal)
        .collect();
    for n in [
        "ok.txt",
        "a.bin",
        "b.bin",
        "big.bin",
        "zeros.bin",
        "passwd",
        "up",
        "abs",
        "readme.txt",
        "e4-7-through-link.txt",
        "f0999999",
    ] {
        set.insert(normal(n));
    }
    set
}

/// No root gained an entry named like a fixture's member during the run.
fn assert_no_new_member_names(
    before: &[(PathBuf, BTreeSet<String>)],
    after: &[(PathBuf, BTreeSet<String>)],
) {
    let watched = member_names();
    for ((dir, b), (_, a)) in before.iter().zip(after) {
        for n in a.difference(b) {
            assert!(
                !watched.contains(&normal(n)),
                "{} gained {n:?} during the run",
                dir.display()
            );
        }
    }
}

fn assert_refused(run: &Run, kind: &str) {
    assert_eq!(run.code, Some(2), "{}", run.message());
    assert_eq!(run.kind(), Some(kind), "{}", run.message());
}

// ---------------------------------------------------------------------------------------------
// Builders.

/// One ZIP member as written: the sizes and CRC are what the headers *declare* (a hostile
/// fixture lies in them), `data` is what follows the local header.
struct ZipMember<'a> {
    name: &'a [u8],
    /// 0 stored, 8 Deflate.
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    data: &'a [u8],
}

impl<'a> ZipMember<'a> {
    /// A stored member whose headers tell the truth.
    fn stored(name: &'a [u8], data: &'a [u8]) -> Self {
        ZipMember {
            name,
            method: 0,
            crc: crc32(data),
            compressed: data.len() as u64,
            size: data.len() as u64,
            data,
        }
    }
    /// Sizes past 32 bits go in a ZIP64 extra field (both sizes there, as APPNOTE 4.5.3 asks
    /// of a local header).
    fn zip64(&self) -> bool {
        self.size >= 0xFFFF_FFFF || self.compressed >= 0xFFFF_FFFF
    }
    fn extra(&self) -> Vec<u8> {
        let mut x = Vec::new();
        if self.zip64() {
            x.extend_from_slice(&1u16.to_le_bytes());
            x.extend_from_slice(&16u16.to_le_bytes());
            x.extend_from_slice(&self.size.to_le_bytes());
            x.extend_from_slice(&self.compressed.to_le_bytes());
        }
        x
    }
    fn sizes32(&self) -> (u32, u32) {
        if self.zip64() {
            (0xFFFF_FFFF, 0xFFFF_FFFF)
        } else {
            (self.compressed as u32, self.size as u32)
        }
    }
    /// The local header and the data (APPNOTE 4.3.7): UTF-8 names (flag bit 11), a fixed
    /// DOS time.
    fn local(&self) -> Vec<u8> {
        let (c, s) = self.sizes32();
        let extra = self.extra();
        let mut v = Vec::with_capacity(30 + self.name.len() + extra.len() + self.data.len());
        v.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        v.extend_from_slice(&45u16.to_le_bytes());
        v.extend_from_slice(&0x0800u16.to_le_bytes());
        v.extend_from_slice(&self.method.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes()); // time
        v.extend_from_slice(&0x5821u16.to_le_bytes()); // 2024-01-01
        v.extend_from_slice(&self.crc.to_le_bytes());
        v.extend_from_slice(&c.to_le_bytes());
        v.extend_from_slice(&s.to_le_bytes());
        v.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        v.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        v.extend_from_slice(self.name);
        v.extend_from_slice(&extra);
        v.extend_from_slice(self.data);
        v
    }
    /// The central directory header (APPNOTE 4.3.12) pointing at a local header at `offset`
    /// (below 4 GiB in every fixture here).
    fn central(&self, offset: u64) -> Vec<u8> {
        let (c, s) = self.sizes32();
        let extra = self.extra();
        let mut v = Vec::with_capacity(46 + self.name.len() + extra.len());
        v.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        v.extend_from_slice(&((3u16 << 8) | 45).to_le_bytes()); // made by: Unix
        v.extend_from_slice(&45u16.to_le_bytes());
        v.extend_from_slice(&0x0800u16.to_le_bytes());
        v.extend_from_slice(&self.method.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&0x5821u16.to_le_bytes());
        v.extend_from_slice(&self.crc.to_le_bytes());
        v.extend_from_slice(&c.to_le_bytes());
        v.extend_from_slice(&s.to_le_bytes());
        v.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        v.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes()); // comment
        v.extend_from_slice(&0u16.to_le_bytes()); // disk
        v.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        v.extend_from_slice(&(0o100_644u32 << 16).to_le_bytes()); // a regular file
        v.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
        v.extend_from_slice(self.name);
        v.extend_from_slice(&extra);
        v
    }
}

/// The end of a ZIP: a ZIP64 end record and locator when the entries do not fit 16 bits, then
/// the end of central directory record (APPNOTE 4.3.14–4.3.16); `out` starts at `base` in
/// the archive.
fn zip_end(out: &mut Vec<u8>, base: u64, entries: u64, cd_offset: u64, cd_len: u64) {
    let zip64 = entries >= 0xFFFF;
    if zip64 {
        let at = base + out.len() as u64;
        out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&entries.to_le_bytes());
        out.extend_from_slice(&entries.to_le_bytes());
        out.extend_from_slice(&cd_len.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    let n16 = if zip64 { 0xFFFF } else { entries as u16 };
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&n16.to_le_bytes());
    out.extend_from_slice(&n16.to_le_bytes());
    let (len32, off32) = if zip64 {
        (0xFFFF_FFFF, 0xFFFF_FFFF)
    } else {
        (
            u32::try_from(cd_len).unwrap(),
            u32::try_from(cd_offset).unwrap(),
        )
    };
    out.extend_from_slice(&len32.to_le_bytes());
    out.extend_from_slice(&off32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
}

/// A well-formed ZIP of `members`, each local header followed by its data, in order.
fn zip(members: &[ZipMember<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for m in members {
        offsets.push(out.len() as u64);
        out.extend_from_slice(&m.local());
    }
    let cd = out.len() as u64;
    for (m, &o) in members.iter().zip(&offsets) {
        out.extend_from_slice(&m.central(o));
    }
    let cd_len = out.len() as u64 - cd;
    zip_end(&mut out, 0, members.len() as u64, cd, cd_len);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(data);
    h.finalize()
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// One ustar header block (POSIX.1-1988): `name` (at most 100 bytes, written raw, so `..` and
/// absolute names survive), the type flag (`b'0'` file, `b'1'` hard link, `b'2'` symlink,
/// `b'5'` directory), the link name, then the data padded to 512 bytes.
fn tar_member(out: &mut Vec<u8>, name: &[u8], kind: u8, link: &[u8], data: &[u8]) {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    h[136..148].copy_from_slice(b"14571641600\0"); // 2024-01-01
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
}

/// A 7z NUMBER (7zFormat.txt): one byte below 0x80, two below 0x4000, else 0xFF and 8 bytes.
fn sz_number(out: &mut Vec<u8>, v: u64) {
    if v < 0x80 {
        out.push(v as u8);
    } else if v < 0x4000 {
        out.push(0x80 | (v >> 8) as u8);
        out.push(v as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// A 7z (7zFormat.txt) of `members`, names written raw as UTF-16 (so `..` and drive names
/// survive), each member its own block of one Copy coder (id 0x00) over its bytes: the pack
/// streams in order after the signature header, then a plain (uncompressed) header with
/// `PackInfo`, `UnpackInfo`, `SubStreamsInfo` (each member's CRC-32) and the names.
fn sevenz(members: &[(&str, &[u8])]) -> Vec<u8> {
    let n = members.len() as u64;
    let mut h = vec![0x01, 0x04, 0x06]; // Header, MainStreamsInfo, PackInfo
    sz_number(&mut h, 0); // PackPos
    sz_number(&mut h, n);
    h.push(0x09); // Size
    for (_, d) in members {
        sz_number(&mut h, d.len() as u64);
    }
    h.extend_from_slice(&[0x00, 0x07, 0x0B]); // End, UnpackInfo, Folder
    sz_number(&mut h, n);
    h.push(0x00); // not external
    for _ in members {
        h.extend_from_slice(&[0x01, 0x01, 0x00]); // one coder, a 1-byte id, Copy
    }
    h.push(0x0C); // CodersUnpackSize
    for (_, d) in members {
        sz_number(&mut h, d.len() as u64);
    }
    h.extend_from_slice(&[0x00, 0x08, 0x0A, 0x01]); // End, SubStreamsInfo, CRC, all defined
    for (_, d) in members {
        h.extend_from_slice(&crc32(d).to_le_bytes());
    }
    h.extend_from_slice(&[0x00, 0x00, 0x05]); // End, End, FilesInfo
    sz_number(&mut h, n);
    let mut names = vec![0x00]; // not external
    for (name, _) in members {
        for u in name.encode_utf16().chain([0]) {
            names.extend_from_slice(&u.to_le_bytes());
        }
    }
    h.push(0x11); // Name
    sz_number(&mut h, names.len() as u64);
    h.extend_from_slice(&names);
    h.extend_from_slice(&[0x00, 0x00]); // End of FilesInfo, End of Header
    let packed: Vec<u8> = members
        .iter()
        .flat_map(|(_, d)| d.iter().copied())
        .collect();
    let mut tail = Vec::with_capacity(20);
    tail.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    tail.extend_from_slice(&(h.len() as u64).to_le_bytes());
    tail.extend_from_slice(&crc32(&h).to_le_bytes());
    let mut out = vec![0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0x00, 0x04];
    out.extend_from_slice(&crc32(&tail).to_le_bytes());
    out.extend_from_slice(&tail);
    out.extend_from_slice(&packed);
    out.extend_from_slice(&h);
    out
}

/// The two zero blocks that end a tar.
fn tar_end(mut out: Vec<u8>) -> Vec<u8> {
    out.resize(out.len() + 1024, 0);
    out
}

// ---------------------------------------------------------------------------------------------
// The fixtures.

/// Overlapping entries (the "non-recursive" bomb of Fifield 2019, reduced to its shape):
/// member `a.bin` is stored and its data *is* member `b.bin`'s local header and data, so the
/// central directory names two members whose packed ranges overlap. Refused by the host's
/// range check before any write: `OverlappingEntries`.
#[test]
fn overlapping_entries_are_refused() {
    let Some(s) = stage() else { return };
    let inner = ZipMember::stored(b"b.bin", b"the shared body");
    let inner_local = inner.local();
    let outer = ZipMember::stored(b"a.bin", &inner_local);
    let mut bytes = outer.local();
    let cd = bytes.len() as u64;
    bytes.extend_from_slice(&outer.central(0));
    bytes.extend_from_slice(&inner.central(30 + 5));
    let cd_len = bytes.len() as u64 - cd;
    zip_end(&mut bytes, 0, 2, cd, cd_len);
    let a = s.write("overlap.zip", &bytes);
    let r = s.x(&a, &[], &[], SMALL);
    assert_refused(&r, "OverlappingEntries");
    assert!(s.written().is_empty(), "{:?}", s.written());
    s.assert_nothing_outside();
}

/// Nested archives (the shape of 42.zip: an archive of archives of archives): one level is
/// extracted, the inner archives are written as files byte for byte and never opened.
#[test]
fn nested_archives_are_written_as_files() {
    let Some(s) = stage() else { return };
    let leaf = vec![0u8; 64 << 10];
    let leaf_z = deflate(&leaf);
    let level1 = zip(&[ZipMember {
        name: b"zeros.bin",
        method: 8,
        crc: crc32(&leaf),
        compressed: leaf_z.len() as u64,
        size: leaf.len() as u64,
        data: &leaf_z,
    }]);
    let names: Vec<String> = (0..8).map(|i| format!("l1-{i}.zip")).collect();
    let level2 = zip(&names
        .iter()
        .map(|n| ZipMember::stored(n.as_bytes(), &level1))
        .collect::<Vec<_>>());
    let outer_names: Vec<String> = (0..8).map(|i| format!("l2-{i}.zip")).collect();
    let outer = zip(&outer_names
        .iter()
        .map(|n| ZipMember::stored(n.as_bytes(), &level2))
        .collect::<Vec<_>>());
    let a = s.write("nested.zip", &outer);
    let r = s.x(&a, &[], &[], SMALL);
    assert_eq!(r.code, Some(0), "{}", r.message());
    assert!(r.stdout.contains("extracted 8 files"), "{}", r.stdout);
    let want: BTreeSet<PathBuf> = outer_names.iter().map(PathBuf::from).collect();
    assert_eq!(s.written(), want);
    for n in &outer_names {
        assert_eq!(std::fs::read(s.out().join(n)).unwrap(), level2, "{n}");
    }
    s.assert_nothing_outside();
}

/// 4 GiB declared over a 1 KiB body: a stored member whose ZIP64 sizes say 4 GiB and whose
/// data is 1 KiB (its CRC that of the 1 KiB). By default the declared total is refused
/// before any write (`LimitExceeded`, the expansion limit); with `--max-expansion off` the
/// host writes what arrives and refuses at the member's end, after at most the body:
/// `SizeMismatch`, the partial file removed (D-54).
#[test]
fn a_4_gib_declaration_over_a_1_kib_body_is_refused() {
    let Some(s) = stage() else { return };
    let body = vec![b'x'; 1024];
    let liar = ZipMember {
        name: b"big.bin",
        method: 0,
        crc: crc32(&body),
        compressed: 1024,
        size: 4 << 30,
        data: &body,
    };
    let a = s.write("declared-4g.zip", &zip(&[liar]));
    let r = s.x(&a, &[], &[], SMALL);
    assert_refused(&r, "LimitExceeded");
    assert!(r.message().contains("declared output"), "{}", r.message());
    assert!(s.written().is_empty());
    s.clear_out();
    let r = s.x(&a, &["--max-expansion", "off"], &[], SMALL);
    let free = lpk_procstat_sys::free_space(s.dir.path()).unwrap_or(u64::MAX);
    if free < (4 << 30) {
        // The free-space check comes first on a small volume: still refused before a write.
        eprintln!("only {free} bytes free: NoSpace expected instead of SizeMismatch");
        assert_refused(&r, "NoSpace");
    } else {
        assert_refused(&r, "SizeMismatch");
        assert!(
            r.message().contains("declared 4294967296 bytes"),
            "{}",
            r.message()
        );
    }
    assert!(
        s.written().is_empty(),
        "partial file left: {:?}",
        s.written()
    );
    s.assert_nothing_outside();
}

/// A high-ratio bomb: 32 MiB of zeros deflated to about 32 KiB (about 1000:1), its headers
/// honest. With the expansion limit set to 100 times the archive it is refused from the
/// declared size before any write (`LimitExceeded`). The same body declared as 1 KiB (a lying
/// header) is cut at the declaration: `SizeMismatch` once the worker sends more, the partial
/// file removed and never more than the declared 1 KiB written.
#[test]
fn a_high_ratio_bomb_meets_the_expansion_limit() {
    let Some(s) = stage() else { return };
    let plain = vec![0u8; 32 << 20];
    let packed = deflate(&plain);
    let honest = ZipMember {
        name: b"zeros.bin",
        method: 8,
        crc: crc32(&plain),
        compressed: packed.len() as u64,
        size: plain.len() as u64,
        data: &packed,
    };
    let a = s.write("bomb.zip", &zip(&[honest]));
    let r = s.x(&a, &["--max-expansion", "100"], &[], SMALL);
    assert_refused(&r, "LimitExceeded");
    assert!(r.message().contains("--max-expansion"), "{}", r.message());
    assert!(s.written().is_empty());
    s.clear_out();
    let lying = ZipMember {
        name: b"zeros.bin",
        method: 8,
        crc: crc32(&plain),
        compressed: packed.len() as u64,
        size: 1024,
        data: &packed,
    };
    let a = s.write("bomb-lying.zip", &zip(&[lying]));
    let r = s.x(&a, &["--max-expansion", "100"], &[], SMALL);
    // Deterministic, no race: the `zip` crate's reader fails a member whose decoder yields more
    // than its declared size ("File is larger than its declared uncompressed size") before
    // handing the excess on, so the worker reports the damage itself (`Corrupt`) after at most
    // 1 KiB of Data. (Were a reader to send more, the host would cut it at the declaration
    // with `SizeMismatch`, its own check, which the 4 GiB test exercises.)
    assert_refused(&r, "Corrupt");
    assert!(
        r.message().contains("larger than its declared"),
        "{}",
        r.message()
    );
    assert!(
        s.written().is_empty(),
        "partial file left: {:?}",
        s.written()
    );
    s.assert_nothing_outside();
}

/// The unsafe names (D-79), as a ZIP stores them (backslashes are read as separators, so
/// `..\x` is `../x` and `\\server\share` is `//server/share`): each is its own one-member
/// archive, each refused before any write with `UnsafePath`.
const UNSAFE_NAMES: &[&str] = &[
    "../e4-7-parent.txt",
    "a/../../e4-7-grandparent.txt",
    "..\\e4-7-backslash-parent.txt",
    "/e4-7-absolute.txt",
    "C:\\e4-7-drive.txt",
    "C:e4-7-drive-relative.txt",
    "\\\\server\\share\\e4-7-unc.txt",
    "\\\\?\\C:\\e4-7-device-path.txt",
    "\\\\.\\PhysicalDrive0",
    "CON",
    "nul.txt",
    "a/COM1.log",
    "file.txt:e4-7-ads",
    "file.txt::$DATA",
    "trailing-dot.",
    "trailing-space ",
];

/// Nothing named by `UNSAFE_NAMES` exists at a root (an absolute or drive path that escaped).
fn assert_no_escape_at_roots() {
    for n in [
        "/e4-7-absolute.txt",
        "C:\\e4-7-drive.txt",
        "C:/e4-7-drive.txt",
        "C:\\e4-7-device-path.txt",
        // The C: drive's current directory (the test's cwd when on C:).
        "C:e4-7-drive-relative.txt",
    ] {
        assert!(!Path::new(n).exists(), "{n} was written");
    }
}

#[test]
fn unsafe_names_in_a_zip_are_refused() {
    let Some(s) = stage() else { return };
    for (i, name) in UNSAFE_NAMES.iter().enumerate() {
        let a = s.write(
            &format!("name-{i}.zip"),
            &zip(&[
                ZipMember::stored(b"ok.txt", b"fine"),
                ZipMember::stored(name.as_bytes(), b"escaped"),
            ]),
        );
        let r = s.x(&a, &[], &[], SMALL);
        assert_refused(&r, "UnsafePath");
        assert!(s.written().is_empty(), "{name:?}: {:?}", s.written());
        s.assert_nothing_outside();
    }
    assert_no_escape_at_roots();
}

/// The same names in a 7z (names are UTF-16; the reader reads backslashes as separators, as
/// for ZIP), each refused with `UnsafePath`. A well-formed 7z of the same shape extracts, so
/// the refusals are the names', not the builder's.
#[test]
fn unsafe_names_in_a_7z_are_refused() {
    let Some(s) = stage() else { return };
    let a = s.write(
        "control.7z",
        &sevenz(&[("ok.txt", b"fine"), ("dir/b.bin", b"second")]),
    );
    let r = s.x(&a, &[], &[], SMALL);
    assert_eq!(r.code, Some(0), "{}", r.message());
    assert_eq!(std::fs::read(s.out().join("dir/b.bin")).unwrap(), b"second");
    s.clear_out();
    for (i, name) in UNSAFE_NAMES.iter().enumerate() {
        let a = s.write(
            &format!("name-{i}.7z"),
            &sevenz(&[("ok.txt", b"fine"), (name, b"escaped")]),
        );
        let r = s.x(&a, &[], &[], SMALL);
        assert_refused(&r, "UnsafePath");
        assert!(s.written().is_empty(), "{name:?}: {:?}", s.written());
        s.assert_nothing_outside();
    }
    assert_no_escape_at_roots();
}

/// The same names as tar stores them (raw bytes: a backslash stays a backslash, refused as
/// such), each refused with `UnsafePath`.
#[test]
fn unsafe_names_in_a_tar_are_refused() {
    let Some(s) = stage() else { return };
    for (i, name) in UNSAFE_NAMES.iter().enumerate() {
        let mut t = Vec::new();
        tar_member(&mut t, b"ok.txt", b'0', b"", b"fine");
        tar_member(&mut t, name.as_bytes(), b'0', b"", b"escaped");
        let a = s.write(&format!("name-{i}.tar"), &tar_end(t));
        let r = s.x(&a, &[], &[], SMALL);
        assert_refused(&r, "UnsafePath");
        assert!(s.written().is_empty(), "{name:?}: {:?}", s.written());
        s.assert_nothing_outside();
    }
    assert_no_escape_at_roots();
}

/// tar symlinks pointing outside (`../../..`, an absolute path): without `--symlinks` they
/// are skipped (a `SymlinkSkipped` warning each, nothing created) and the rest extracts; an
/// entry below a symlink entry (`up/x.txt` beside the link `up`, the classic write-through)
/// is a conflicting name, refused before any write (`UnsafePath`).
#[test]
fn tar_symlinks_out_are_skipped_and_an_entry_below_one_is_refused() {
    let Some(s) = stage() else { return };
    let mut t = Vec::new();
    tar_member(&mut t, b"up", b'2', b"../../..", b"");
    tar_member(&mut t, b"abs", b'2', b"/etc", b"");
    tar_member(&mut t, b"ok.txt", b'0', b"", b"fine");
    let a = s.write("symlinks.tar", &tar_end(t));
    let r = s.x(&a, &[], &[], SMALL);
    assert_eq!(r.code, Some(0), "{}", r.message());
    assert_eq!(
        r.stderr.matches("SymlinkSkipped").count(),
        2,
        "{}",
        r.stderr
    );
    let want: BTreeSet<PathBuf> = [PathBuf::from("ok.txt")].into();
    assert_eq!(s.written(), want);
    s.assert_nothing_outside();
    s.clear_out();

    let mut t = Vec::new();
    tar_member(&mut t, b"up", b'2', b"../../..", b"");
    tar_member(&mut t, b"up/e4-7-through-link.txt", b'0', b"", b"escaped");
    let a = s.write("symlink-below.tar", &tar_end(t));
    for extra in [&[][..], &["--symlinks"][..]] {
        let r = s.x(&a, extra, &[], SMALL);
        assert_refused(&r, "UnsafePath");
        assert!(r.message().contains("conflicting name"), "{}", r.message());
        assert!(s.written().is_empty(), "{:?}", s.written());
        s.assert_nothing_outside();
    }
}

/// tar hard links pointing outside (`../../../etc/passwd`, `/etc/passwd`) name no member the
/// run extracts: refused before any write (`Unsupported`, "a member that is not extracted");
/// the host never links or copies anything it did not write itself.
#[test]
fn tar_hard_links_out_are_refused() {
    let Some(s) = stage() else { return };
    for (i, target) in [&b"../../../etc/passwd"[..], b"/etc/passwd"]
        .iter()
        .enumerate()
    {
        let mut t = Vec::new();
        tar_member(&mut t, b"ok.txt", b'0', b"", b"fine");
        tar_member(&mut t, b"passwd", b'1', target, b"");
        let a = s.write(&format!("hardlink-{i}.tar"), &tar_end(t));
        let r = s.x(&a, &[], &[], SMALL);
        assert_refused(&r, "Unsupported");
        assert!(r.message().contains("not extracted"), "{}", r.message());
        assert!(s.written().is_empty(), "{:?}", s.written());
        s.assert_nothing_outside();
    }
}

/// Names differing only by case (`Readme.txt`, `README.TXT`): on a case-insensitive target
/// (Windows) the second would overwrite the first, so the pair is refused before any write
/// (`CaseCollision`); on Linux both are distinct files and both are written.
#[test]
fn names_differing_only_by_case_are_a_case_collision() {
    let Some(s) = stage() else { return };
    let a = s.write(
        "case.zip",
        &zip(&[
            ZipMember::stored(b"Readme.txt", b"first"),
            ZipMember::stored(b"README.TXT", b"second"),
        ]),
    );
    let r = s.x(&a, &[], &[], SMALL);
    if cfg!(windows) {
        assert_refused(&r, "CaseCollision");
        assert!(s.written().is_empty(), "{:?}", s.written());
    } else {
        assert_eq!(r.code, Some(0), "{}", r.message());
        assert_eq!(std::fs::read(s.out().join("Readme.txt")).unwrap(), b"first");
        assert_eq!(
            std::fs::read(s.out().join("README.TXT")).unwrap(),
            b"second"
        );
    }
    s.assert_nothing_outside();
}

/// One million members (ZIP64 end records; every member an empty stored file, so the archive
/// is headers only, about 92 MB): `lpk x` with one member selected lists all 1,000,000
/// through the broker in pages of 4,096 (`PAGE`), writes the one file, and the host's peak
/// working set stays under the listing charge it reports (`-v`: "listing charge N bytes",
/// `entry_charge` summed). Bounds: 300 s (debug builds of both processes), and the charge.
const PEAK_PER_MEMBER: u64 = 1024;
const CHARGE_PER_MEMBER_MAX: u64 = 2048;

#[test]
fn a_million_member_central_directory_is_paged_under_the_listing_charge() {
    let Some(s) = stage() else { return };
    const N: u64 = 1_000_000;
    let a = s.path("million.zip");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&a).unwrap());
        let mut cd = Vec::with_capacity(N as usize * 54);
        let mut at = 0u64;
        for i in 0..N {
            let name = format!("f{i:07}");
            let m = ZipMember::stored(name.as_bytes(), b"");
            let local = m.local();
            f.write_all(&local).unwrap();
            cd.extend_from_slice(&m.central(at));
            at += local.len() as u64;
        }
        let cd_len = cd.len() as u64;
        zip_end(&mut cd, at, N, at, cd_len);
        f.write_all(&cd).unwrap();
        f.flush().unwrap();
    }
    // Stated bounds, independent of what `lpk` reports: the host's peak at most 1,024 bytes per
    // member (1 GiB for the million; 367 bytes per member measured on Windows), and 300 s.
    let bounds = Bounds {
        wall: Duration::from_secs(300),
        memory: N * PEAK_PER_MEMBER,
    };
    // `--memory 4G`: the budget is both the listing's ceiling and the worker's commit limit
    // (D-67). The default (half the physical memory, at most the tier's ceiling) varies with
    // the machine and could refuse the listing (about 1 GB of charge) on a small CI runner, or
    // starve the worker, which holds its own copy of the million ZIP headers; 4 GiB is the
    // extraction ceiling and leaves room for both, so the run measures paging, not the budget.
    let r = s.x(&a, &["-v", "--memory", "4G"], &["f0999999"], bounds);
    assert_eq!(r.code, Some(0), "{}", r.message());
    assert!(r.stdout.contains("extracted 1 files"), "{}", r.stdout);
    assert!(r.stderr.contains("1000000 entries"), "{}", r.stderr);
    let charge: u64 = r
        .stderr
        .split("listing charge ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| panic!("no listing charge in: {}", r.stderr));
    eprintln!("listing charge {charge} bytes, host peak {} bytes", r.peak);
    // The reported charge against what the test computes: at least the record and the name's
    // eight copies (`NAME_COPIES`) per member, at most the stated `CHARGE_PER_MEMBER_MAX`
    // (1,009 bytes per member today: `ENTRY_CHARGE` 945 plus 8 x 8).
    let floor = N * (std::mem::size_of::<lpk_proto::Record>() as u64 + 8 * 8);
    assert!(
        (floor..=N * CHARGE_PER_MEMBER_MAX).contains(&charge),
        "listing charge {charge} outside {floor}..={}",
        N * CHARGE_PER_MEMBER_MAX
    );
    assert!(
        r.peak <= charge,
        "the host's peak {} exceeds the listing charge {charge}",
        r.peak
    );
    let want: BTreeSet<PathBuf> = [PathBuf::from("f0999999")].into();
    assert_eq!(s.written(), want);
    s.assert_nothing_outside();
}
